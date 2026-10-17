//! PDF attachments and images (Autopsy's `PDFAttachmentExtractor`, Tika
//! with `extractInlineImages` and unique images only).
//!
//! The file is scanned for indirect objects instead of being parsed
//! through its cross-reference table, so damaged files, files with broken
//! xrefs and objects left behind by incremental updates are still found,
//! and nothing but the dictionaries of interest is held in memory:
//!
//! * `/Type /EmbeddedFile` streams are attachments, named from the file
//!   specification that points at them (`/UF`, else `/F`), also when the
//!   specification sits in an object stream; unnamed ones become
//!   `extract_<n>`;
//! * `/Subtype /Image` streams are images: DCT → `image<n>.jpg`, JPX →
//!   `image<n>.jp2`, and Flate/uncompressed `DeviceGray` or `DeviceRGB` pixel
//!   data → `image<n>.png` (re-encoded without loading the image).
//!
//! Encrypted documents (`/Encrypt`) are reported as
//! [`ArchiveError::Encrypted`]; their streams cannot be read.

use std::collections::HashMap;
use std::io::{self, Read, Write};

use pm_core::{Bytes, ReadAt};

use crate::driver::{CopyError, Driver};
use crate::io::{RangeReader, read_to_end_bounded, read_upto};
use crate::{ArchiveError, Entry, MemberEnd};

/// Bytes read to parse one object's dictionary.
const DICT_WINDOW: usize = 64 * 1024;
/// Scan window.
const WIN: usize = 4 << 20;
/// Most objects remembered (lengths, file specifications, streams).
const MAX_TRACKED: usize = 1 << 20;
/// Nesting limit of the object parser.
const MAX_DEPTH: usize = 32;

/// A PDF object (only what the extractor needs).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Vec<(Vec<u8>, Obj)>),
    Ref(u32, u16),
}

impl Obj {
    fn get(&self, key: &[u8]) -> Option<&Obj> {
        match self {
            Self::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn name(&self) -> Option<&[u8]> {
        match self {
            Self::Name(n) => Some(n),
            _ => None,
        }
    }

    fn int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            #[allow(clippy::cast_possible_truncation, reason = "PDF reals used as sizes")]
            Self::Real(r) => Some(*r as i64),
            _ => None,
        }
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | 0)
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// A small recursive-descent parser over a byte slice.
pub(crate) struct Parser<'b> {
    b: &'b [u8],
    pub(crate) pos: usize,
}

impl<'b> Parser<'b> {
    pub(crate) fn new(b: &'b [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if is_ws(c) {
                self.pos += 1;
            } else if c == b'%' {
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.b.get(self.pos..).is_some_and(|r| r.starts_with(s))
    }

    fn token(&mut self) -> &'b [u8] {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
        }
        self.b.get(start..self.pos).unwrap_or_default()
    }

    pub(crate) fn object(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        let c = self.peek()?;
        match c {
            b'/' => {
                self.pos += 1;
                let raw = self.token();
                Some(Obj::Name(unescape_name(raw)))
            }
            b'(' => self.literal().map(Obj::Str),
            b'<' if self.starts_with(b"<<") => {
                self.pos += 2;
                let mut d = Vec::new();
                loop {
                    self.skip_ws();
                    if self.starts_with(b">>") {
                        self.pos += 2;
                        return Some(Obj::Dict(d));
                    }
                    let Obj::Name(key) = self.object(depth + 1)? else {
                        return None;
                    };
                    let v = self.object(depth + 1)?;
                    if d.len() < 4096 {
                        d.push((key, v));
                    }
                }
            }
            b'<' => {
                self.pos += 1;
                let mut hex = Vec::new();
                loop {
                    let c = self.peek()?;
                    self.pos += 1;
                    if c == b'>' {
                        break;
                    }
                    if c.is_ascii_hexdigit() {
                        hex.push(c);
                    }
                }
                if hex.len() % 2 == 1 {
                    hex.push(b'0');
                }
                Some(Obj::Str(
                    hex.chunks_exact(2)
                        .filter_map(|p| {
                            std::str::from_utf8(p)
                                .ok()
                                .and_then(|s| u8::from_str_radix(s, 16).ok())
                        })
                        .collect(),
                ))
            }
            b'[' => {
                self.pos += 1;
                let mut a = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek()? == b']' {
                        self.pos += 1;
                        return Some(Obj::Array(a));
                    }
                    let v = self.object(depth + 1)?;
                    if a.len() < 65_536 {
                        a.push(v);
                    }
                }
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let t = self.token();
                let s = std::str::from_utf8(t).ok()?;
                if let Ok(i) = s.parse::<i64>() {
                    // `n g R`?
                    let save = self.pos;
                    self.skip_ws();
                    let g = self.token();
                    self.skip_ws();
                    if !g.is_empty()
                        && g.iter().all(u8::is_ascii_digit)
                        && self.peek() == Some(b'R')
                        && self
                            .b
                            .get(self.pos + 1)
                            .is_none_or(|&c| is_ws(c) || is_delim(c))
                        && let (Ok(n), Ok(g)) = (
                            u32::try_from(i),
                            std::str::from_utf8(g).unwrap_or("x").parse::<u16>(),
                        )
                    {
                        self.pos += 1;
                        return Some(Obj::Ref(n, g));
                    }
                    self.pos = save;
                    Some(Obj::Int(i))
                } else {
                    s.parse::<f64>().ok().map(Obj::Real)
                }
            }
            _ => {
                let t = self.token();
                match t {
                    b"true" => Some(Obj::Bool(true)),
                    b"false" => Some(Obj::Bool(false)),
                    b"null" => Some(Obj::Null),
                    _ => None,
                }
            }
        }
    }

    fn literal(&mut self) -> Option<Vec<u8>> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1usize;
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(c);
                }
                _ => out.push(c),
            }
            if out.len() > 1 << 20 {
                return None;
            }
        }
    }
}

fn unescape_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while let Some(&c) = raw.get(i) {
        if c == b'#'
            && let Some(h) = raw.get(i + 1..i + 3)
            && let Some(v) = std::str::from_utf8(h)
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Decodes a PDF text string (UTF-16BE or UTF-8 with BOM, else
/// `PDFDocEncoding`, treated as Latin-1).
fn text(s: &[u8]) -> String {
    if let Some(u) = s.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = u
            .chunks_exact(2)
            .map(|c| c.u16_be(0).unwrap_or(0))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(u) = s.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(u).into_owned()
    } else {
        s.iter().map(|&b| char::from(b)).collect()
    }
}

/// A stream of interest.
#[derive(Debug, Clone)]
struct StreamObj {
    num: u32,
    dict: Obj,
    data: u64,
    len: Option<u64>,
    len_ref: Option<u32>,
}

/// What the scan found.
#[derive(Debug, Default)]
struct Found {
    files: Vec<StreamObj>,
    images: Vec<StreamObj>,
    objstms: Vec<StreamObj>,
    /// Embedded-file object number → name from its file specification.
    names: HashMap<u32, String>,
    /// Integer objects (stream lengths).
    ints: HashMap<u32, u64>,
    encrypted: bool,
}

impl Found {
    fn note_filespec(&mut self, o: &Obj) {
        let Some(ef) = o.get(b"EF") else { return };
        let name = o.get(b"UF").or_else(|| o.get(b"F")).and_then(|n| match n {
            Obj::Str(s) => Some(text(s)),
            _ => None,
        });
        for key in [&b"UF"[..], b"F"] {
            if let Some(Obj::Ref(n, _)) = ef.get(key)
                && let Some(name) = &name
                && self.names.len() < MAX_TRACKED
            {
                self.names.entry(*n).or_insert_with(|| name.clone());
            }
        }
    }
}

/// Parses `n g obj` backwards from the `obj` keyword at `at` in `buf`.
fn header_before(buf: &[u8], at: usize) -> Option<(u32, usize)> {
    let mut i = at;
    let back_ws = |i: &mut usize| {
        let s = *i;
        while *i > 0 && buf.get(*i - 1).copied().is_some_and(is_ws) {
            *i -= 1;
        }
        *i < s
    };
    let back_digits = |i: &mut usize| {
        let s = *i;
        while *i > 0 && buf.get(*i - 1).is_some_and(u8::is_ascii_digit) {
            *i -= 1;
        }
        (*i < s).then_some(*i..s)
    };
    if !back_ws(&mut i) {
        return None;
    }
    back_digits(&mut i)?;
    if !back_ws(&mut i) {
        return None;
    }
    let num = back_digits(&mut i)?;
    if i > 0
        && !buf
            .get(i - 1)
            .copied()
            .is_some_and(|c| is_ws(c) || is_delim(c))
    {
        return None;
    }
    let n = std::str::from_utf8(buf.get(num)?).ok()?.parse().ok()?;
    Some((n, i))
}

fn scan(src: &dyn ReadAt, d: &Driver<'_>) -> Result<Found, ArchiveError> {
    let size = src.size();
    let mut f = Found::default();
    let mut pos = 0u64;
    while pos < size {
        d.check_cancel()?;
        let buf = read_upto(src, pos, WIN)?;
        if buf.is_empty() {
            break;
        }
        let mut next = pos + buf.len().saturating_sub(64).max(1) as u64;
        if buf.len() < WIN {
            next = size;
        }
        let finder = memchr::memmem::Finder::new(b"obj");
        let mut skip_to = 0u64;
        for at in finder.find_iter(&buf) {
            let abs = pos + at as u64;
            if abs < skip_to {
                continue;
            }
            // `endobj` is not a header.
            if at >= 3 && buf.get(at - 3..at) == Some(&b"end"[..]) {
                continue;
            }
            let Some((num, _start)) = header_before(&buf, at) else {
                continue;
            };
            let body = abs + 3;
            let window = read_upto(src, body, DICT_WINDOW)?;
            let mut p = Parser::new(&window);
            let Some(obj) = p.object(0) else {
                continue;
            };
            match &obj {
                Obj::Int(v) => {
                    if let Ok(v) = u64::try_from(*v)
                        && f.ints.len() < MAX_TRACKED
                    {
                        f.ints.insert(num, v);
                    }
                    continue;
                }
                Obj::Dict(_) => {}
                _ => continue,
            }
            if obj.get(b"Encrypt").is_some() && obj.get(b"Root").is_some()
                || obj.get(b"Type").and_then(Obj::name) == Some(b"XRef")
                    && obj.get(b"Encrypt").is_some()
            {
                f.encrypted = true;
            }
            f.note_filespec(&obj);
            p.skip_ws();
            if !p.starts_with(b"stream") {
                continue;
            }
            p.pos += 6;
            if window.get(p.pos) == Some(&b'\r') {
                p.pos += 1;
            }
            if window.get(p.pos) == Some(&b'\n') {
                p.pos += 1;
            }
            let data = body + p.pos as u64;
            let (len, len_ref) = match obj.get(b"Length") {
                Some(Obj::Ref(n, _)) => (None, Some(*n)),
                Some(o) => (o.int().and_then(|v| u64::try_from(v).ok()), None),
                None => (None, None),
            };
            let s = StreamObj {
                num,
                dict: obj.clone(),
                data,
                len,
                len_ref,
            };
            let ty = obj.get(b"Type").and_then(Obj::name);
            let sub = obj.get(b"Subtype").and_then(Obj::name);
            let tracked = f.files.len() + f.images.len() + f.objstms.len();
            if tracked < MAX_TRACKED {
                if ty == Some(b"EmbeddedFile") {
                    f.files.push(s);
                } else if sub == Some(b"Image") {
                    f.images.push(s);
                } else if ty == Some(b"ObjStm") {
                    f.objstms.push(s);
                }
            }
            if let Some(l) = len {
                skip_to = data.saturating_add(l);
            }
        }
        // Trailer dictionaries of classic xref tables.
        for at in memchr::memmem::find_iter(&buf, b"trailer") {
            let window = read_upto(src, pos + at as u64 + 7, DICT_WINDOW)?;
            if let Some(t) = Parser::new(&window).object(0)
                && t.get(b"Encrypt").is_some()
            {
                f.encrypted = true;
            }
        }
        pos = next.max(skip_to.min(size)).max(pos + 1);
    }
    Ok(f)
}

/// Resolves a stream's length (direct, indirect, or up to `endstream`).
fn stream_len(
    src: &dyn ReadAt,
    s: &StreamObj,
    ints: &HashMap<u32, u64>,
) -> Result<u64, ArchiveError> {
    let avail = src.size().saturating_sub(s.data);
    if let Some(l) = s
        .len
        .or_else(|| s.len_ref.and_then(|n| ints.get(&n).copied()))
        && l <= avail
    {
        return Ok(l);
    }
    // Search for `endstream`.
    let mut pos = s.data;
    while pos < src.size() {
        let buf = read_upto(src, pos, WIN)?;
        if buf.is_empty() {
            break;
        }
        if let Some(i) = memchr::memmem::find(&buf, b"endstream") {
            let mut end = pos + i as u64;
            // Drop the EOL before the keyword.
            let tail = read_upto(src, end.saturating_sub(2), 2)?;
            if tail.ends_with(b"\r\n") {
                end -= 2;
            } else if tail.ends_with(b"\n") || tail.ends_with(b"\r") {
                end -= 1;
            }
            return Ok(end.saturating_sub(s.data));
        }
        if buf.len() < 9 {
            break;
        }
        pos += (buf.len() - 8) as u64;
    }
    Ok(avail)
}

/// The filter names of a stream.
fn filters(dict: &Obj) -> Vec<Vec<u8>> {
    match dict.get(b"Filter") {
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(|o| o.name().map(<[u8]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

/// Wraps `r` in the decoders for `names` (`None`: unsupported filter).
fn decode_chain<'r>(r: Box<dyn Read + 'r>, names: &[Vec<u8>]) -> Option<Box<dyn Read + 'r>> {
    let mut r = r;
    for n in names {
        r = match n.as_slice() {
            b"FlateDecode" | b"Fl" => Box::new(flate2::read::ZlibDecoder::new(r)),
            b"ASCIIHexDecode" | b"AHx" => Box::new(AsciiHex {
                inner: r,
                done: false,
            }),
            b"ASCII85Decode" | b"A85" => Box::new(Ascii85::new(r)),
            _ => return None,
        };
    }
    Some(r)
}

pub(crate) fn extract(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let mut f = scan(src, d)?;
    if f.encrypted {
        return Err(ArchiveError::Encrypted);
    }
    // File specifications inside object streams.
    for os in std::mem::take(&mut f.objstms) {
        d.check_cancel()?;
        let len = stream_len(src, &os, &f.ints)?;
        let raw: Box<dyn Read> = Box::new(RangeReader::new(src, os.data, len));
        let Some(mut dec) = decode_chain(raw, &filters(&os.dict)) else {
            continue;
        };
        let Ok(Some(body)) = read_to_end_bounded(&mut dec, d.opts.max_in_memory) else {
            continue;
        };
        let n = os
            .dict
            .get(b"N")
            .and_then(Obj::int)
            .unwrap_or(0)
            .clamp(0, 1 << 20) as usize;
        let first = os.dict.get(b"First").and_then(Obj::int).unwrap_or(0).max(0) as usize;
        let mut hp = Parser::new(&body);
        let mut offsets = Vec::new();
        for _ in 0..n {
            match (hp.object(0), hp.object(0)) {
                (Some(Obj::Int(num)), Some(Obj::Int(off))) => offsets.push((num, off)),
                _ => break,
            }
        }
        for (num, off) in offsets {
            let Some(at) = usize::try_from(off).ok().and_then(|o| o.checked_add(first)) else {
                continue;
            };
            let Some(slice) = body.get(at..) else {
                continue;
            };
            let mut p = Parser::new(slice);
            match p.object(0) {
                Some(o @ Obj::Dict(_)) => f.note_filespec(&o),
                Some(Obj::Int(v)) => {
                    if let (Ok(num), Ok(v)) = (u32::try_from(num), u64::try_from(v))
                        && f.ints.len() < MAX_TRACKED
                    {
                        f.ints.insert(num, v);
                    }
                }
                _ => {}
            }
        }
    }
    let mut index = 0u64;
    for (k, s) in f.files.iter().enumerate() {
        let name = f
            .names
            .get(&s.num)
            .cloned()
            .filter(|n| !n.is_empty())
            .map_or_else(
                || format!("extract_{k}"),
                |n| n.rsplit(['/', '\\']).next().unwrap_or_default().to_owned(),
            );
        let len = stream_len(src, s, &f.ints)?;
        let declared = s
            .dict
            .get(b"Params")
            .and_then(|p| p.get(b"Size"))
            .and_then(Obj::int)
            .and_then(|v| u64::try_from(v).ok());
        let e = Entry {
            index,
            path: name,
            size: declared,
            packed_size: Some(len),
            ..Entry::default()
        };
        index += 1;
        let raw: Box<dyn Read> = Box::new(RangeReader::new(src, s.data, len));
        match decode_chain(raw, &filters(&s.dict)) {
            Some(mut dec) => {
                d.member(&e, Some(&mut dec), None)?;
            }
            None => {
                d.begin(&e)?;
                d.end(&MemberEnd::Skipped("unsupported PDF filter".into()))?;
            }
        }
    }
    let mut img_no = 0u64;
    for s in &f.images {
        d.check_cancel()?;
        let len = stream_len(src, s, &f.ints)?;
        let names = filters(&s.dict);
        let Some(plan) = image_plan(&s.dict, &names) else {
            continue;
        };
        let e = Entry {
            index,
            path: format!("image{img_no}.{}", plan.ext()),
            packed_size: Some(len),
            ..Entry::default()
        };
        index += 1;
        img_no += 1;
        let raw: Box<dyn Read> = Box::new(RangeReader::new(src, s.data, len));
        match plan {
            ImagePlan::Passthrough { decode, .. } => match decode_chain(raw, &decode) {
                Some(mut dec) => {
                    d.member(&e, Some(&mut dec), None)?;
                }
                None => {
                    d.begin(&e)?;
                    d.end(&MemberEnd::Skipped("unsupported PDF filter".into()))?;
                }
            },
            ImagePlan::Png(p) => {
                let Some(mut dec) = decode_chain(raw, &names) else {
                    continue;
                };
                if !d.begin(&e)? {
                    d.end(&MemberEnd::Complete)?;
                    continue;
                }
                let end = match write_png(&mut dec, &p, d) {
                    Ok(()) => MemberEnd::Complete,
                    Err(CopyError::Read(err)) => MemberEnd::Failed(err.to_string()),
                    Err(CopyError::Stop(err)) => return Err(err),
                };
                d.end(&end)?;
            }
        }
    }
    Ok(())
}

/// Raw pixel layout of an image converted to PNG.
#[derive(Debug, Clone, Copy)]
struct PngPlan {
    width: u32,
    height: u32,
    bits: u8,
    /// 0 gray, 2 RGB (PNG colour types).
    color: u8,
    /// Data already carries PNG row filters (`/Predictor` ≥ 10).
    predicted: bool,
}

#[derive(Debug)]
enum ImagePlan {
    Passthrough {
        decode: Vec<Vec<u8>>,
        ext: &'static str,
    },
    Png(PngPlan),
}

impl ImagePlan {
    fn ext(&self) -> &'static str {
        match self {
            Self::Passthrough { ext, .. } => ext,
            Self::Png(_) => "png",
        }
    }
}

fn image_plan(dict: &Obj, names: &[Vec<u8>]) -> Option<ImagePlan> {
    if let Some((last, rest)) = names.split_last() {
        let ext = match last.as_slice() {
            b"DCTDecode" | b"DCT" => Some("jpg"),
            b"JPXDecode" => Some("jp2"),
            _ => None,
        };
        if let Some(ext) = ext {
            return Some(ImagePlan::Passthrough {
                decode: rest.to_vec(),
                ext,
            });
        }
    }
    if dict
        .get(b"ImageMask")
        .is_some_and(|m| *m == Obj::Bool(true))
    {
        return None;
    }
    let color = match dict.get(b"ColorSpace") {
        Some(Obj::Name(n)) if n == b"DeviceGray" || n == b"G" || n == b"CalGray" => 0,
        Some(Obj::Name(n)) if n == b"DeviceRGB" || n == b"RGB" || n == b"CalRGB" => 2,
        Some(Obj::Array(a)) if a.first().and_then(Obj::name) == Some(b"CalRGB") => 2,
        Some(Obj::Array(a)) if a.first().and_then(Obj::name) == Some(b"CalGray") => 0,
        _ => return None,
    };
    let bits = u8::try_from(dict.get(b"BitsPerComponent").and_then(Obj::int)?).ok()?;
    let ok_bits = if color == 0 {
        matches!(bits, 1 | 2 | 4 | 8 | 16)
    } else {
        matches!(bits, 8 | 16)
    };
    if !ok_bits {
        return None;
    }
    let width = u32::try_from(dict.get(b"Width").and_then(Obj::int)?).ok()?;
    let height = u32::try_from(dict.get(b"Height").and_then(Obj::int)?).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if !names.iter().all(|n| {
        matches!(
            n.as_slice(),
            b"FlateDecode" | b"Fl" | b"ASCIIHexDecode" | b"AHx" | b"ASCII85Decode" | b"A85"
        )
    }) {
        return None;
    }
    let parms = dict.get(b"DecodeParms");
    let parms = match parms {
        Some(Obj::Array(a)) => a.last(),
        other => other,
    };
    let predictor = parms
        .and_then(|p| p.get(b"Predictor"))
        .and_then(Obj::int)
        .unwrap_or(1);
    if predictor == 2 || (3..10).contains(&predictor) {
        return None;
    }
    Some(ImagePlan::Png(PngPlan {
        width,
        height,
        bits,
        color,
        predicted: predictor >= 10,
    }))
}

fn png_chunk(ty: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(data.len() + 12);
    c.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
    c.extend_from_slice(&ty);
    c.extend_from_slice(data);
    let mut h = crc32fast::Hasher::new();
    h.update(&ty);
    h.update(data);
    c.extend_from_slice(&h.finalize().to_be_bytes());
    c
}

/// Streams raw rows into a PNG (several `IDAT` chunks).
fn write_png(r: &mut dyn Read, p: &PngPlan, d: &mut Driver<'_>) -> Result<(), CopyError> {
    let stop = CopyError::Stop;
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&p.width.to_be_bytes());
    ihdr.extend_from_slice(&p.height.to_be_bytes());
    ihdr.extend_from_slice(&[p.bits, p.color, 0, 0, 0]);
    d.write(b"\x89PNG\r\n\x1a\n").map_err(stop)?;
    d.write(&png_chunk(*b"IHDR", &ihdr)).map_err(stop)?;
    let channels: u64 = if p.color == 2 { 3 } else { 1 };
    let row = (u64::from(p.width) * channels * u64::from(p.bits)).div_ceil(8);
    let row_len = usize::try_from(row + u64::from(p.predicted))
        .map_err(|e| CopyError::Read(io::Error::other(e)))?;
    if row_len > 64 << 20 {
        return Err(CopyError::Read(io::Error::other("image row too large")));
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut buf = vec![0u8; row_len];
    for _ in 0..p.height {
        if let Err(e) = r.read_exact(&mut buf) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                break;
            }
            return Err(CopyError::Read(e));
        }
        if !p.predicted {
            enc.write_all(&[0]).map_err(CopyError::Read)?;
        }
        enc.write_all(&buf).map_err(CopyError::Read)?;
        if enc.get_ref().len() >= 256 * 1024 {
            let data = std::mem::take(enc.get_mut());
            d.write(&png_chunk(*b"IDAT", &data)).map_err(stop)?;
        }
    }
    let data = enc.finish().map_err(CopyError::Read)?;
    d.write(&png_chunk(*b"IDAT", &data)).map_err(stop)?;
    d.write(&png_chunk(*b"IEND", &[])).map_err(stop)?;
    Ok(())
}

/// `ASCIIHexDecode`.
struct AsciiHex<R> {
    inner: R,
    done: bool,
}

impl<R: Read> Read for AsciiHex<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut n = 0;
        let mut pair: Option<u8> = None;
        let mut one = [0u8; 1];
        while n < out.len() && !self.done {
            if self.inner.read(&mut one)? == 0 {
                self.done = true;
                break;
            }
            let c = one[0];
            if c == b'>' {
                self.done = true;
                break;
            }
            if let Some(v) = (c as char).to_digit(16) {
                let v = v as u8;
                match pair.take() {
                    Some(hi) => {
                        if let Some(slot) = out.get_mut(n) {
                            *slot = hi << 4 | v;
                        }
                        n += 1;
                    }
                    None => pair = Some(v),
                }
            }
        }
        if let Some(hi) = pair
            && let Some(slot) = out.get_mut(n)
        {
            // An odd final digit is followed by an implied 0.
            *slot = hi << 4;
            n += 1;
        }
        Ok(n)
    }
}

/// `ASCII85Decode`.
struct Ascii85<R> {
    inner: R,
    group: Vec<u8>,
    pending: Vec<u8>,
    done: bool,
}

impl<R: Read> Ascii85<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            group: Vec::with_capacity(5),
            pending: Vec::new(),
            done: false,
        }
    }

    fn flush_group(&mut self, last: bool) {
        let len = self.group.len();
        if len == 0 {
            return;
        }
        while self.group.len() < 5 {
            self.group.push(b'u');
        }
        let v = self
            .group
            .iter()
            .fold(0u64, |acc, &c| acc * 85 + u64::from(c.saturating_sub(b'!')));
        let bytes = (v as u32).to_be_bytes();
        let keep = if last { len.saturating_sub(1) } else { 4 };
        self.pending
            .extend_from_slice(bytes.get(..keep).unwrap_or_default());
        self.group.clear();
    }
}

impl<R: Read> Read for Ascii85<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut one = [0u8; 1];
        while self.pending.len() < out.len() && !self.done {
            if self.inner.read(&mut one)? == 0 {
                self.done = true;
                self.flush_group(true);
                break;
            }
            match one[0] {
                b'~' => {
                    self.done = true;
                    self.flush_group(true);
                }
                b'z' if self.group.is_empty() => self.pending.extend_from_slice(&[0; 4]),
                c @ b'!'..=b'u' => {
                    self.group.push(c);
                    if self.group.len() == 5 {
                        self.flush_group(false);
                    }
                }
                _ => {}
            }
        }
        let n = self.pending.len().min(out.len());
        if let (Some(dst), Some(src)) = (out.get_mut(..n), self.pending.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.pending.drain(..n);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_objects() {
        let mut p = Parser::new(b"<< /Type /Filespec /F (a\\(b\\).pdf) /UF <FEFF00E4> /EF << /F 12 0 R >> /N [1 2.5 true null] /K#20x 3 >>");
        let o = p.object(0).unwrap();
        assert_eq!(o.get(b"Type").and_then(Obj::name), Some(&b"Filespec"[..]));
        assert_eq!(o.get(b"F"), Some(&Obj::Str(b"a(b).pdf".to_vec())));
        assert_eq!(
            o.get(b"UF").map(|s| match s {
                Obj::Str(s) => text(s),
                _ => String::new(),
            }),
            Some("ä".to_owned())
        );
        assert_eq!(
            o.get(b"EF").and_then(|e| e.get(b"F")),
            Some(&Obj::Ref(12, 0))
        );
        assert_eq!(o.get(b"K x"), Some(&Obj::Int(3)));
        let mut f = Found::default();
        f.note_filespec(&o);
        assert_eq!(f.names.get(&12).map(String::as_str), Some("ä"));
        // Deep nesting and junk do not panic.
        let deep = "[".repeat(10_000);
        assert!(Parser::new(deep.as_bytes()).object(0).is_none());
        assert!(Parser::new(b"<< /A").object(0).is_none());
        assert!(Parser::new(b"(unterminated").object(0).is_none());
    }

    #[test]
    fn headers_before_obj() {
        let b = b"xx\n12 0 obj<<>>";
        assert_eq!(header_before(b, 8), Some((12, 3)));
        assert_eq!(header_before(b"a12 0 obj", 6), None);
        assert_eq!(header_before(b"obj", 0), None);
    }

    #[test]
    fn ascii_filters() {
        let mut out = Vec::new();
        AsciiHex {
            inner: &b"48 65 6c6C6f>"[..],
            done: false,
        }
        .read_to_end(&mut out)
        .unwrap();
        assert_eq!(out, b"Hello");
        let mut out = Vec::new();
        Ascii85::new(&b"87cURD]j7BEbo7~>"[..])
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"Hello world");
        let mut out = Vec::new();
        Ascii85::new(&b"z~>"[..]).read_to_end(&mut out).unwrap();
        assert_eq!(out, [0; 4]);
    }
}
