//! PDF password protection as Autopsy learns it: `PDFBox` 3.0.3 (inside
//! Tika's `PDFParser`) loads the document with an empty password and
//! throws `InvalidPasswordException` when neither the owner nor the user
//! password is empty; Tika reports that as `EncryptedDocumentException`.
//!
//! The trailer is found like `PDFBox`'s parser does (`startxref`, classic
//! xref tables and xref streams, the `/Prev` chain, then a brute-force
//! search), the `/Encrypt` dictionary of the standard security handler is
//! read, and the empty password is checked with `PDFBox`'s algorithms for
//! revisions 2–6. Everything else (another security handler, an unknown
//! revision, malformed values) is a parse failure, which Autopsy logs
//! without flagging the file.

use std::collections::HashSet;

use pm_core::ReadAt;

use crate::CheckError;
use crate::crypto::{self, Hash};

/// `PDFBox` reads the last 2048 bytes for `startxref` (`readTrailBytes`).
const TRAIL_BYTES: u64 = 2048;
/// Largest object or xref section parsed.
const MAX_SECTION: usize = 8 << 20;
/// Largest decoded xref stream.
const MAX_XREF_STREAM: usize = 64 << 20;
/// Maximum `/Prev` chain length.
const MAX_SECTIONS: usize = 1024;
/// Nesting limit for arrays and dictionaries.
const MAX_DEPTH: usize = 64;
/// Read size for the brute-force search.
const SCAN_CHUNK: usize = 1 << 20;

const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// Whether `PDFBox` would refuse the document without a password.
pub fn is_password_protected(src: &dyn ReadAt) -> Result<bool, CheckError> {
    let doc = Document { src };
    let trailer = doc.trailer()?;
    let Some(enc) = trailer.encrypt else {
        return Ok(false);
    };
    let enc = match enc {
        Obj::Ref(n, g) => doc.object(n, g, &trailer.sections)?,
        o => o,
    };
    if matches!(enc, Obj::Null) {
        return Ok(false);
    }
    let Obj::Dict(dict) = enc else {
        return Err(CheckError::invalid(
            "encryption dictionary is not a dictionary",
        ));
    };
    let resolve = |o: Option<&Obj>| -> Result<Option<Obj>, CheckError> {
        match o {
            Some(Obj::Ref(n, g)) => doc.object(*n, *g, &trailer.sections).map(Some),
            Some(o) => Ok(Some(o.clone())),
            None => Ok(None),
        }
    };
    let id = match resolve(trailer.id.as_ref())? {
        Some(Obj::Array(a)) => match a.first() {
            Some(Obj::Str(s)) => s.clone(),
            Some(Obj::Ref(n, g)) => match doc.object(*n, *g, &trailer.sections)? {
                Obj::Str(s) => s,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let get = |k: &str| resolve(dict_get(&dict, k));
    let filter = get("Filter")?;
    if !matches!(&filter, Some(Obj::Name(n)) if n == b"Standard") {
        return Err(CheckError::unsupported("not the standard security handler"));
    }
    let int = |k: &str, default: i64| -> Result<i64, CheckError> {
        Ok(match get(k)? {
            Some(Obj::Int(i)) => i,
            Some(Obj::Real(r)) => r as i64,
            _ => default,
        })
    };
    let string = |k: &str| -> Result<Option<Vec<u8>>, CheckError> {
        Ok(match get(k)? {
            Some(Obj::Str(s)) => Some(s),
            _ => None,
        })
    };
    let version = int("V", 0)?;
    let revision = int("R", 0)?;
    let length = int("Length", 40)?;
    // `getInt` reads the low 32 bits as a Java int.
    let permissions = int("P", 0)? as i32;
    let encrypt_metadata = !matches!(get("EncryptMetadata")?, Some(Obj::Bool(false)));
    let mut key_len = if version == 1 { 5 } else { length / 8 };
    if version == 4 || version == 5 {
        let std_cf = match get("CF")? {
            Some(Obj::Dict(cf)) => resolve(dict_get(&cf, "StdCF"))?,
            _ => None,
        };
        if let Some(Obj::Dict(cf)) = std_cf {
            let method = resolve(dict_get(&cf, "CFM"))?;
            let has_length = dict_get(&dict, "Length").is_some();
            let aes = match &method {
                Some(Obj::Name(n)) if n == b"AESV2" => Some(16),
                Some(Obj::Name(n)) if n == b"AESV3" => Some(32),
                _ => None,
            };
            if let Some(n) = aes {
                key_len = n;
                if has_length && length / 8 < key_len {
                    key_len = length / 8;
                }
            }
        }
    }
    let user = string("U")?.ok_or_else(|| CheckError::invalid("no /U"))?;
    let owner = string("O")?.ok_or_else(|| CheckError::invalid("no /O"))?;
    let check = Check {
        revision,
        key_len: usize::try_from(key_len).map_err(|_| CheckError::invalid("key length"))?,
        permissions,
        id,
        user,
        owner,
        encrypt_metadata,
    };
    // `prepareForDecryption`: the owner password first, then the user
    // password; neither → InvalidPasswordException.
    Ok(!(check.is_owner_password(b"")? || check.is_user_password(b"")?))
}

// ------------------------------------------------------------ password check

/// The values of the standard security handler.
#[derive(Debug, Clone)]
struct Check {
    revision: i64,
    key_len: usize,
    permissions: i32,
    id: Vec<u8>,
    user: Vec<u8>,
    owner: Vec<u8>,
    encrypt_metadata: bool,
}

/// `b[r]`, or a parse error.
fn part(b: &[u8], r: std::ops::Range<usize>) -> Result<&[u8], CheckError> {
    b.get(r)
        .ok_or_else(|| CheckError::invalid("value too short"))
}

/// `truncateOrPad`: the password, padded with the standard padding.
fn pad(password: &[u8]) -> Vec<u8> {
    let n = password.len().min(32);
    let mut out = password.get(..n).unwrap_or_default().to_vec();
    out.extend_from_slice(PADDING.get(..32 - n).unwrap_or_default());
    out
}

fn truncate127(p: &[u8]) -> &[u8] {
    p.get(..p.len().min(127)).unwrap_or(p)
}

impl Check {
    fn is_owner_password(&self, password: &[u8]) -> Result<bool, CheckError> {
        match self.revision {
            2..=4 => {
                let user_password = self.user_password_from_owner(password)?;
                self.is_user_password_234(&user_password)
            }
            5 | 6 => {
                if self.owner.len() < 40 {
                    return Err(CheckError::invalid("owner password is too short"));
                }
                let pw = truncate127(password);
                let user_key = adjust_user_key(Some(&self.user))?;
                let (hash, salt) = (part(&self.owner, 0..32)?, part(&self.owner, 32..40)?);
                let calc = if self.revision == 5 {
                    Hash::Sha256.digest(&[pw, salt, user_key])
                } else {
                    hash_2b(&[pw, salt, user_key].concat(), pw, user_key)?
                };
                Ok(calc == hash)
            }
            r => Err(CheckError::unsupported(format!("encryption revision {r}"))),
        }
    }

    fn is_user_password(&self, password: &[u8]) -> Result<bool, CheckError> {
        match self.revision {
            2..=4 => self.is_user_password_234(password),
            5 | 6 => {
                if self.user.len() < 40 {
                    return Err(CheckError::invalid("user password is too short"));
                }
                let pw = truncate127(password);
                let (hash, salt) = (part(&self.user, 0..32)?, part(&self.user, 32..40)?);
                let calc = if self.revision == 5 {
                    Hash::Sha256.digest(&[pw, salt])
                } else {
                    hash_2b(&[pw, salt].concat(), pw, &[])?
                };
                Ok(calc == hash)
            }
            r => Err(CheckError::unsupported(format!("encryption revision {r}"))),
        }
    }

    /// MD5 of `first`, then (revisions 3 and 4) 50 more rounds over the
    /// first `key_len` bytes; the first `key_len` bytes of the result.
    fn md5_rounds(&self, first: &[&[u8]]) -> Result<Vec<u8>, CheckError> {
        let n = self.key_len;
        if n > 16 {
            // Java's `md.update(digest, 0, length)` would throw.
            return Err(CheckError::invalid("key length"));
        }
        let mut digest = Hash::Md5.digest(first);
        if self.revision >= 3 {
            for _ in 0..50 {
                digest = Hash::Md5.digest(&[part(&digest, 0..n)?]);
            }
        }
        Ok(part(&digest, 0..n)?.to_vec())
    }

    /// `getUserPassword234` with `computeRC4key` (algorithm 3, a–d).
    fn user_password_from_owner(&self, owner_password: &[u8]) -> Result<Vec<u8>, CheckError> {
        let key = self.md5_rounds(&[&pad(owner_password)])?;
        if self.revision == 2 {
            return Ok(crypto::rc4(&key, &self.owner));
        }
        let mut data = self.owner.clone();
        for i in (0..20u8).rev() {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            data = crypto::rc4(&k, &data);
        }
        Ok(data)
    }

    /// `isUserPassword234` via `computeUserPassword` and
    /// `computeEncryptedKeyRev234` (algorithm 2).
    fn is_user_password_234(&self, password: &[u8]) -> Result<bool, CheckError> {
        let p = self.permissions.to_le_bytes();
        let no_metadata: &[u8] = if self.revision == 4 && !self.encrypt_metadata {
            &[0xFF; 4]
        } else {
            &[]
        };
        let key = self.md5_rounds(&[&pad(password), &self.owner, &p, &self.id, no_metadata])?;
        if self.revision == 2 {
            return Ok(crypto::rc4(&key, &PADDING) == self.user);
        }
        let mut data = Hash::Md5.digest(&[&PADDING, &self.id]);
        for i in 0..20u8 {
            let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
            data = crypto::rc4(&k, &data);
        }
        // Only the first 16 bytes are compared (zero-padded copies).
        let mut user = self.user.clone();
        user.resize(16, 0);
        Ok(data.get(..16) == Some(user.as_slice()))
    }
}

/// `adjustUserKey`: no key, or the first 48 bytes of /U.
fn adjust_user_key(u: Option<&[u8]>) -> Result<&[u8], CheckError> {
    match u {
        None => Ok(&[]),
        Some(u) => u
            .get(..48)
            .ok_or_else(|| CheckError::invalid("bad /U length")),
    }
}

/// Algorithm 2.B of ISO 32000-2 (revision 6).
fn hash_2b(input: &[u8], password: &[u8], user_key: &[u8]) -> Result<Vec<u8>, CheckError> {
    let key48 = user_key.get(..48);
    let mut k = Hash::Sha256.digest(&[input]);
    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + 48));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            if let Some(u) = key48 {
                k1.extend_from_slice(u);
            }
        }
        let e = crypto::aes128_cbc_encrypt(part(&k, 0..16)?, part(&k, 16..32)?, &k1)
            .ok_or_else(|| CheckError::invalid("AES"))?;
        // The first 16 bytes as a big-endian number, modulo 3.
        let rem = part(&e, 0..16)?
            .iter()
            .fold(0u32, |acc, &b| (acc * 256 + u32::from(b)) % 3);
        k = match rem {
            0 => Hash::Sha256,
            1 => Hash::Sha384,
            _ => Hash::Sha512,
        }
        .digest(&[&e]);
        round += 1;
        let last = u32::from(e.last().copied().unwrap_or(0));
        if !(round < 64 || last > round - 32) {
            break;
        }
    }
    k.truncate(32);
    Ok(k)
}

// --------------------------------------------------------------- PDF objects

/// A dictionary: keys (names without the slash) and values in order.
type Dict = Vec<(Vec<u8>, Obj)>;

/// A parsed PDF object.
#[derive(Debug, Clone, PartialEq)]
enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32, u16),
}

fn dict_get<'d>(d: &'d [(Vec<u8>, Obj)], key: &str) -> Option<&'d Obj> {
    d.iter().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v)
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// A cursor over PDF bytes.
struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn rest(&self) -> &'a [u8] {
        self.b.get(self.pos..).unwrap_or_default()
    }

    /// Skips whitespace and comments.
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if is_ws(c) {
                self.pos += 1;
            } else if c == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// A regular token (keyword or number).
    fn word(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
        }
        self.b.get(start..self.pos).unwrap_or_default()
    }

    fn keyword(&mut self, k: &[u8]) -> bool {
        self.skip_ws();
        let save = self.pos;
        if self.word() == k {
            true
        } else {
            self.pos = save;
            false
        }
    }

    fn uint(&mut self) -> Option<u64> {
        self.skip_ws();
        let save = self.pos;
        let w = self.word();
        let v = std::str::from_utf8(w).ok().and_then(|s| s.parse().ok());
        if v.is_none() {
            self.pos = save;
        }
        v
    }

    fn object(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        let c = self.peek()?;
        match c {
            b'/' => {
                self.pos += 1;
                Some(Obj::Name(self.name()))
            }
            b'(' => {
                self.pos += 1;
                self.literal_string().map(Obj::Str)
            }
            b'<' if self.b.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                self.dict(depth).map(Obj::Dict)
            }
            b'<' => {
                self.pos += 1;
                self.hex_string().map(Obj::Str)
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek()? {
                        b']' => {
                            self.pos += 1;
                            return Some(Obj::Array(items));
                        }
                        _ => items.push(self.object(depth + 1)?),
                    }
                }
            }
            _ => {
                let w = self.word();
                match w {
                    b"true" => Some(Obj::Bool(true)),
                    b"false" => Some(Obj::Bool(false)),
                    b"null" => Some(Obj::Null),
                    _ => {
                        let s = std::str::from_utf8(w).ok()?;
                        if let Ok(i) = s.parse::<i64>() {
                            // `n g R`.
                            let save = self.pos;
                            if let (Some(g), true) = (self.uint(), true)
                                && self.keyword(b"R")
                                && let (Ok(n), Ok(g)) = (u32::try_from(i), u16::try_from(g))
                            {
                                return Some(Obj::Ref(n, g));
                            }
                            self.pos = save;
                            Some(Obj::Int(i))
                        } else {
                            s.parse::<f64>().ok().map(Obj::Real)
                        }
                    }
                }
            }
        }
    }

    fn name(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if is_ws(c) || is_delim(c) {
                break;
            }
            self.pos += 1;
            if c == b'#'
                && let Some(v) = self
                    .b
                    .get(self.pos..self.pos + 2)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                self.pos += 2;
                continue;
            }
            out.push(c);
        }
        out
    }

    fn literal_string(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        let mut nest = 0usize;
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'(' => {
                    nest += 1;
                    out.push(c);
                }
                b')' if nest == 0 => return Some(out),
                b')' => {
                    nest -= 1;
                    out.push(c);
                }
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
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
    }

    fn hex_string(&mut self) -> Option<Vec<u8>> {
        let mut digits = Vec::new();
        loop {
            let c = self.peek()?;
            self.pos += 1;
            match c {
                b'>' => break,
                c if c.is_ascii_hexdigit() => digits.push(c),
                c if is_ws(c) => {}
                // PDFBox skips invalid characters.
                _ => {}
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(b'0');
        }
        Some(
            digits
                .chunks_exact(2)
                .filter_map(|h| {
                    std::str::from_utf8(h)
                        .ok()
                        .and_then(|s| u8::from_str_radix(s, 16).ok())
                })
                .collect(),
        )
    }

    fn dict(&mut self, depth: usize) -> Option<Dict> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            match self.peek()? {
                b'>' if self.b.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    return Some(out);
                }
                b'/' => {
                    self.pos += 1;
                    let k = self.name();
                    let v = self.object(depth + 1)?;
                    out.push((k, v));
                }
                _ => return None,
            }
        }
    }
}

// ------------------------------------------------------------------ xref

/// Where an xref section's entries are.
#[derive(Debug, Clone)]
enum Section {
    /// Classic table: subsections `(first object, count, file offset of
    /// the first entry)`.
    Table(Vec<(u64, u64, u64)>),
    /// Decoded xref stream: widths, `(first, count)` ranges, entries.
    Stream {
        w: [usize; 3],
        index: Vec<(u64, u64)>,
        data: Vec<u8>,
    },
}

/// The merged trailer.
#[derive(Debug, Default)]
struct Trailer {
    encrypt: Option<Obj>,
    id: Option<Obj>,
    /// Newest first.
    sections: Vec<Section>,
}

struct Document<'a> {
    src: &'a dyn ReadAt,
}

impl Document<'_> {
    fn read(&self, off: u64, len: usize) -> Vec<u8> {
        let size = self.src.size();
        if off >= size {
            return Vec::new();
        }
        let n = usize::try_from((size - off).min(len as u64)).unwrap_or(0);
        let mut buf = vec![0u8; n];
        let got = self.src.read_at(off, &mut buf).unwrap_or(0);
        buf.truncate(got);
        buf
    }

    /// `COSParser.getStartxrefOffset`: the number after the last
    /// `startxref` before the last `%%EOF` in the final 2048 bytes.
    fn startxref(&self) -> Option<u64> {
        let size = self.src.size();
        let start = size.saturating_sub(TRAIL_BYTES);
        let tail = self.read(start, TRAIL_BYTES as usize);
        let eof = rfind(&tail, b"%%EOF").unwrap_or(tail.len());
        let at = rfind(tail.get(..eof)?, b"startxref")?;
        let mut lx = Lexer::new(tail.get(at + 9..)?);
        lx.uint()
    }

    fn trailer(&self) -> Result<Trailer, CheckError> {
        if let Some(t) = self.startxref().and_then(|o| self.chain(o)) {
            return Ok(t);
        }
        self.brute_force_trailer()
    }

    /// Follows the `/Prev` chain from `start`.
    fn chain(&self, start: u64) -> Option<Trailer> {
        let mut t = Trailer::default();
        let mut seen = HashSet::new();
        let mut next = Some(start);
        while let Some(off) = next {
            if !seen.insert(off) || seen.len() > MAX_SECTIONS {
                break;
            }
            let (section, dict) = self.section(off)?;
            t.sections.push(section);
            // Newer trailers win.
            if t.encrypt.is_none() {
                t.encrypt = dict_get(&dict, "Encrypt").cloned();
            }
            if t.id.is_none() {
                t.id = dict_get(&dict, "ID").cloned();
            }
            // A hybrid file's /XRefStm.
            if let Some(Obj::Int(x)) = dict_get(&dict, "XRefStm")
                && let Ok(x) = u64::try_from(*x)
                && let Some((s, _)) = self.section(x)
            {
                t.sections.push(s);
            }
            next = match dict_get(&dict, "Prev") {
                Some(Obj::Int(p)) => u64::try_from(*p).ok(),
                _ => None,
            };
        }
        (!t.sections.is_empty()).then_some(t)
    }

    /// An xref section (table + trailer, or xref stream) at `off`.
    fn section(&self, off: u64) -> Option<(Section, Dict)> {
        let buf = self.read(off, MAX_SECTION);
        let mut lx = Lexer::new(&buf);
        lx.skip_ws();
        if lx.rest().starts_with(b"xref") {
            lx.pos += 4;
            let mut subs = Vec::new();
            loop {
                lx.skip_ws();
                if lx.rest().starts_with(b"trailer") {
                    lx.pos += 7;
                    let Obj::Dict(d) = lx.object(0)? else {
                        return None;
                    };
                    return Some((Section::Table(subs), d));
                }
                let first = lx.uint()?;
                let count = lx.uint()?;
                lx.skip_ws();
                subs.push((first, count, off + lx.pos as u64));
                // Entries: "nnnnnnnnnn ggggg n" lines.
                for _ in 0..count {
                    lx.uint()?;
                    lx.uint()?;
                    lx.skip_ws();
                    match lx.word() {
                        b"n" | b"f" => {}
                        _ => return None,
                    }
                }
            }
        }
        // An xref stream object: "n g obj << … >> stream".
        lx.uint()?;
        lx.uint()?;
        if !lx.keyword(b"obj") {
            return None;
        }
        let Obj::Dict(xref) = lx.object(0)? else {
            return None;
        };
        if !matches!(dict_get(&xref, "Type"), Some(Obj::Name(n)) if n == b"XRef") {
            return None;
        }
        let raw = stream_data(&mut lx, &xref)?;
        let data = decode_stream(&xref, raw)?;
        let widths = match dict_get(&xref, "W") {
            Some(Obj::Array(a)) if a.len() == 3 => {
                let mut widths = [0usize; 3];
                for (o, v) in widths.iter_mut().zip(a) {
                    *o = match v {
                        Obj::Int(i) if (0..=8).contains(i) => usize::try_from(*i).ok()?,
                        _ => return None,
                    };
                }
                widths
            }
            _ => return None,
        };
        let index = match dict_get(&xref, "Index") {
            Some(Obj::Array(a)) => a
                .chunks_exact(2)
                .filter_map(|p| match p {
                    [Obj::Int(f), Obj::Int(c)] => {
                        Some((u64::try_from(*f).ok()?, u64::try_from(*c).ok()?))
                    }
                    _ => None,
                })
                .collect(),
            _ => match dict_get(&xref, "Size") {
                Some(Obj::Int(s)) => vec![(0, u64::try_from(*s).ok()?)],
                _ => return None,
            },
        };
        Some((
            Section::Stream {
                w: widths,
                index,
                data,
            },
            xref,
        ))
    }

    /// The file offset of object `n` in the sections (newest first).
    fn offset_of(&self, n: u32, sections: &[Section]) -> Option<u64> {
        let n = u64::from(n);
        for s in sections {
            match s {
                Section::Table(subs) => {
                    for &(first, count, at) in subs {
                        if n >= first && n < first + count {
                            let line = self.read(at + (n - first) * 20, 20);
                            let mut lx = Lexer::new(&line);
                            let off = lx.uint()?;
                            let _gen = lx.uint()?;
                            lx.skip_ws();
                            return (lx.word() == b"n").then_some(off);
                        }
                    }
                }
                Section::Stream { w, index, data } => {
                    let width = w[0] + w[1] + w[2];
                    let mut base = 0u64;
                    for &(first, count) in index {
                        if n >= first && n < first + count {
                            let at = usize::try_from(base + (n - first)).ok()? * width;
                            let e = data.get(at..at + width)?;
                            let field = |from: usize, len: usize, default: u64| {
                                if len == 0 {
                                    default
                                } else {
                                    e.get(from..from + len).map_or(0, |s| {
                                        s.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b))
                                    })
                                }
                            };
                            let ty = field(0, w[0], 1);
                            return (ty == 1).then(|| field(w[0], w[1], 0));
                        }
                        base += count;
                    }
                }
            }
        }
        None
    }

    /// Indirect object `n g`.
    fn object(&self, n: u32, g: u16, sections: &[Section]) -> Result<Obj, CheckError> {
        if let Some(off) = self.offset_of(n, sections)
            && let Some(o) = self.object_at(off, n, g)
        {
            return Ok(o);
        }
        // PDFBox's lenient mode searches the file for "n g obj".
        let pat = format!("{n} {g} obj");
        self.scan_for(pat.as_bytes(), true)
            .into_iter()
            .rev()
            .find_map(|off| self.object_at(off, n, g))
            .ok_or_else(|| CheckError::invalid(format!("object {n} {g} not found")))
    }

    fn object_at(&self, off: u64, n: u32, g: u16) -> Option<Obj> {
        let buf = self.read(off, MAX_SECTION);
        let mut lx = Lexer::new(&buf);
        if lx.uint()? != u64::from(n) || lx.uint()? != u64::from(g) || !lx.keyword(b"obj") {
            return None;
        }
        lx.object(0)
    }

    /// Offsets of every occurrence of `pat` (at a token start when
    /// `token`), read in chunks.
    fn scan_for(&self, pat: &[u8], token: bool) -> Vec<u64> {
        let size = self.src.size();
        let mut out = Vec::new();
        let mut off = 0u64;
        let overlap = pat.len() as u64;
        while off < size {
            let buf = self.read(off, SCAN_CHUNK);
            if buf.is_empty() {
                break;
            }
            let mut i = 0;
            while let Some(p) = find(buf.get(i..).unwrap_or_default(), pat) {
                let at = i + p;
                let ok =
                    !token || at == 0 || buf.get(at - 1).is_some_and(|&c| is_ws(c) || is_delim(c));
                if ok {
                    out.push(off + at as u64);
                    if out.len() > 10_000 {
                        return out;
                    }
                }
                i = at + 1;
            }
            if buf.len() < SCAN_CHUNK {
                break;
            }
            off += buf.len() as u64 - overlap;
        }
        out
    }

    /// `PDFBox`'s brute-force trailer: every `trailer` dictionary and xref
    /// stream dictionary, newest (last in the file) winning.
    fn brute_force_trailer(&self) -> Result<Trailer, CheckError> {
        let mut t = Trailer::default();
        for off in self.scan_for(b"trailer", true).into_iter().rev() {
            let buf = self.read(off + 7, MAX_SECTION);
            if let Some(Obj::Dict(d)) = Lexer::new(&buf).object(0) {
                if t.encrypt.is_none() {
                    t.encrypt = dict_get(&d, "Encrypt").cloned();
                }
                if t.id.is_none() {
                    t.id = dict_get(&d, "ID").cloned();
                }
            }
        }
        for off in self.scan_for(b"/XRef", false).into_iter().rev() {
            // Back up to the dictionary start.
            let from = off.saturating_sub(1024);
            let buf = self.read(from, 4096);
            let rel = usize::try_from(off - from).unwrap_or(0);
            if let Some(start) = rfind(buf.get(..rel).unwrap_or_default(), b"<<") {
                let mut lx = Lexer::new(buf.get(start..).unwrap_or_default());
                if let Some(Obj::Dict(d)) = lx.object(0) {
                    if t.encrypt.is_none() {
                        t.encrypt = dict_get(&d, "Encrypt").cloned();
                    }
                    if t.id.is_none() {
                        t.id = dict_get(&d, "ID").cloned();
                    }
                }
            }
        }
        if t.encrypt.is_none() && t.id.is_none() && self.scan_for(b"obj", true).is_empty() {
            return Err(CheckError::invalid("no PDF objects"));
        }
        Ok(t)
    }
}

/// The raw bytes of the stream following a stream dictionary.
fn stream_data<'b>(lx: &mut Lexer<'b>, d: &[(Vec<u8>, Obj)]) -> Option<&'b [u8]> {
    if !lx.keyword(b"stream") {
        return None;
    }
    if lx.peek() == Some(b'\r') {
        lx.pos += 1;
    }
    if lx.peek() == Some(b'\n') {
        lx.pos += 1;
    }
    let start = lx.pos;
    let len = match dict_get(d, "Length") {
        Some(Obj::Int(l)) => usize::try_from(*l).ok(),
        _ => None,
    };
    let end = match len {
        Some(l) if lx.b.get(start..start + l).is_some() => start + l,
        _ => start + find(lx.b.get(start..)?, b"endstream")?,
    };
    lx.b.get(start..end)
}

/// `FlateDecode` with an optional PNG predictor (all xref streams use
/// at most these).
fn decode_stream(d: &[(Vec<u8>, Obj)], raw: &[u8]) -> Option<Vec<u8>> {
    let filters: Vec<Vec<u8>> = match dict_get(d, "Filter") {
        None => Vec::new(),
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(|o| match o {
                Obj::Name(n) => Some(n.clone()),
                _ => None,
            })
            .collect(),
        _ => return None,
    };
    let mut data = raw.to_vec();
    for f in &filters {
        if f != b"FlateDecode" && f != b"Fl" {
            return None;
        }
        data = inflate(&data)?;
    }
    let parms = match dict_get(d, "DecodeParms") {
        Some(Obj::Dict(p)) => Some(p.clone()),
        Some(Obj::Array(a)) => a.iter().find_map(|o| match o {
            Obj::Dict(p) => Some(p.clone()),
            _ => None,
        }),
        _ => None,
    };
    if let Some(p) = parms {
        let predictor = match dict_get(&p, "Predictor") {
            Some(Obj::Int(i)) => *i,
            _ => 1,
        };
        let columns = match dict_get(&p, "Columns") {
            Some(Obj::Int(i)) if *i > 0 => usize::try_from(*i).ok()?,
            _ => 1,
        };
        if predictor >= 10 {
            data = png_unpredict(&data, columns)?;
        } else if predictor == 2 {
            return None;
        }
    }
    Some(data)
}

/// zlib inflate, bounded; a truncated stream yields what was decoded.
fn inflate(raw: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    let mut d = flate2::read::ZlibDecoder::new(raw).take(MAX_XREF_STREAM as u64);
    match d.read_to_end(&mut out) {
        Ok(_) => Some(out),
        Err(_) if !out.is_empty() => Some(out),
        Err(_) => None,
    }
}

/// Undoes PNG row predictors (one filter byte per row of `columns`
/// bytes, one byte per pixel).
fn png_unpredict(data: &[u8], columns: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; columns];
    for row in data.chunks(columns + 1) {
        let (&filter, bytes) = row.split_first()?;
        let mut cur: Vec<u8> = Vec::with_capacity(columns);
        for (i, &x) in bytes.iter().enumerate().take(columns) {
            let at = |v: &[u8], j: Option<usize>| j.and_then(|j| v.get(j)).copied().unwrap_or(0);
            let left = at(&cur, i.checked_sub(1));
            let up = at(&prev, Some(i));
            let up_left = at(&prev, i.checked_sub(1));
            cur.push(match filter {
                0 => x,
                1 => x.wrapping_add(left),
                2 => x.wrapping_add(up),
                3 => x.wrapping_add(u16::midpoint(u16::from(left), u16::from(up)) as u8),
                4 => x.wrapping_add(paeth(left, up, up_left)),
                _ => return None,
            });
        }
        out.extend_from_slice(&cur);
        cur.resize(columns, 0);
        prev = cur;
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn parse(s: &[u8]) -> Option<Obj> {
        Lexer::new(s).object(0)
    }

    #[test]
    fn objects() {
        assert_eq!(parse(b"/Na#20me"), Some(Obj::Name(b"Na me".to_vec())));
        assert_eq!(
            parse(b"(a\\(b\\)c\\101\\n(x))"),
            Some(Obj::Str(b"a(b)cA\n(x)".to_vec()))
        );
        assert_eq!(parse(b"<41 4 >"), Some(Obj::Str(vec![0x41, 0x40])));
        assert_eq!(parse(b"12 0 R"), Some(Obj::Ref(12, 0)));
        assert_eq!(parse(b"12 0 obj"), Some(Obj::Int(12)));
        assert_eq!(parse(b"-3.5"), Some(Obj::Real(-3.5)));
        assert_eq!(
            parse(b"<</A [1 true null] % c\n/B<</C/D>>>>"),
            Some(Obj::Dict(vec![
                (
                    b"A".to_vec(),
                    Obj::Array(vec![Obj::Int(1), Obj::Bool(true), Obj::Null])
                ),
                (
                    b"B".to_vec(),
                    Obj::Dict(vec![(b"C".to_vec(), Obj::Name(b"D".to_vec()))])
                ),
            ]))
        );
        // Unterminated and too deep.
        assert_eq!(parse(b"<</A (x"), None);
        assert_eq!(parse(&b"[".repeat(1000)), None);
    }

    #[test]
    fn png_predictor() {
        // Rows of 3: "up" filter adds the previous row.
        let data = [2, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(png_unpredict(&data, 3).unwrap(), [1, 2, 3, 2, 3, 4]);
    }

    #[test]
    fn unencrypted_and_garbage() {
        let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\nxref\n0 2\n0000000000 65535 f \n0000000009 00000 n \ntrailer<</Size 2/Root 1 0 R>>\nstartxref\n42\n%%EOF\n";
        assert_eq!(
            is_password_protected(&MemSource::new(pdf.to_vec())),
            Ok(false)
        );
        assert!(is_password_protected(&MemSource::new(b"%PDF-1.4 garbage".to_vec())).is_err());
    }
}
