//! Zip reader.
//!
//! The central directory (with ZIP64 and prepended-data/SFX offsets) is
//! the index; when it is missing or unreadable (truncated, carved or
//! damaged archives) the local headers are walked from the start instead,
//! resynchronising on the next signature after damage. Methods: stored,
//! deflate, bzip2, LZMA, zstd and xz. Encrypted entries (`ZipCrypto` or
//! `WinZip` AES) are reported as encrypted; `ZipCrypto` entries are decrypted
//! when a password is supplied.

use std::io::{BufReader, Read};

use pm_core::{Bytes, ReadAt};

use crate::driver::{CopyError, Driver};
use crate::io::{RangeReader, read_upto};
use crate::names::decode_default;
use crate::{ArchiveError, Entry, MemberEnd};

const LOCAL_SIG: &[u8; 4] = b"PK\x03\x04";
const CENTRAL_SIG: &[u8; 4] = b"PK\x01\x02";
const EOCD_SIG: &[u8; 4] = b"PK\x05\x06";
const EOCD64_SIG: &[u8; 4] = b"PK\x06\x06";
const LOCATOR_SIG: &[u8; 4] = b"PK\x06\x07";
const DESCRIPTOR_SIG: &[u8; 4] = b"PK\x07\x08";

/// Flag bits.
const FLAG_ENCRYPTED: u16 = 1;
const FLAG_DESCRIPTOR: u16 = 1 << 3;
const FLAG_STRONG: u16 = 1 << 6;
const FLAG_UTF8: u16 = 1 << 11;

/// Method of AES-encrypted entries (the real method is in extra 0x9901).
const METHOD_AES: u16 = 99;

/// A member as described by the central directory (or a local header).
#[derive(Debug, Clone, Default)]
pub(crate) struct ZipEntry {
    pub(crate) name: Vec<u8>,
    flags: u16,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    csize: u64,
    usize: u64,
    /// Local header offset as stored (before the prepended-data delta).
    local_offset: u64,
    made_by: u16,
    external: u32,
    unicode_name: Option<String>,
    modified: Option<i64>,
    accessed: Option<i64>,
    created: Option<i64>,
}

impl ZipEntry {
    fn encrypted(&self) -> bool {
        self.flags & (FLAG_ENCRYPTED | FLAG_STRONG) != 0 || self.method == METHOD_AES
    }

    fn decoded_name(&self) -> (String, Option<Vec<u8>>) {
        if self.flags & FLAG_UTF8 != 0 {
            (String::from_utf8_lossy(&self.name).into_owned(), None)
        } else if let Some(u) = &self.unicode_name {
            (u.clone(), None)
        } else {
            (decode_default(&self.name), Some(self.name.clone()))
        }
    }

    fn is_dir(&self) -> bool {
        if self.name.ends_with(b"/") || self.name.ends_with(b"\\") {
            return true;
        }
        // DOS directory attribute from DOS/Windows hosts.
        let host = self.made_by >> 8;
        matches!(host, 0 | 10 | 11 | 14) && self.external & 0x10 != 0 && self.usize == 0
    }

    fn to_entry(&self, index: u64, encrypted: bool) -> Entry {
        let (path, raw_path) = self.decoded_name();
        let dos = pm_core::time::dos_datetime(self.dos_date, self.dos_time, 0)
            .and_then(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).ok())
            .map(|z| z.timestamp().as_second());
        let is_dir = self.is_dir();
        Entry {
            index,
            path,
            raw_path,
            is_dir,
            size: Some(self.usize),
            packed_size: Some(self.csize),
            encrypted: encrypted && !is_dir,
            modified: self.modified.or(dos),
            accessed: self.accessed,
            created: self.created,
        }
    }

    /// Applies an extra field block (central or local).
    fn apply_extra(&mut self, extra: &[u8], central: bool) {
        let mut pos = 0usize;
        while let (Ok(id), Ok(len)) = (extra.u16_le(pos), extra.u16_le(pos + 2)) {
            let Ok(data) = extra.sub(pos + 4, usize::from(len)) else {
                break;
            };
            match id {
                0x0001 => self.apply_zip64(data, central),
                0x000A => self.apply_ntfs(data),
                0x5455 => self.apply_ut(data),
                0x7075 => self.apply_unicode_path(data),
                _ => {}
            }
            pos += 4 + usize::from(len);
        }
    }

    fn apply_zip64(&mut self, d: &[u8], central: bool) {
        let mut p = 0;
        let mut next = |cond: bool| -> Option<u64> {
            if !cond {
                return None;
            }
            let v = d.u64_le(p).ok();
            p += 8;
            v
        };
        if let Some(v) = next(self.usize == 0xFFFF_FFFF) {
            self.usize = v;
        }
        if let Some(v) = next(self.csize == 0xFFFF_FFFF) {
            self.csize = v;
        }
        if central && let Some(v) = next(self.local_offset == 0xFFFF_FFFF) {
            self.local_offset = v;
        }
    }

    fn apply_ntfs(&mut self, d: &[u8]) {
        // 4 reserved bytes, then tag/size attributes; tag 1 holds the times.
        let mut p = 4;
        while let (Ok(tag), Ok(size)) = (d.u16_le(p), d.u16_le(p + 2)) {
            if tag == 1 && size >= 24 {
                let ft = |o: usize| {
                    d.u64_le(p + 4 + o)
                        .ok()
                        .filter(|&v| v != 0)
                        .and_then(pm_core::time::from_filetime)
                        .map(jiff::Timestamp::as_second)
                };
                self.modified = ft(0).or(self.modified);
                self.accessed = ft(8).or(self.accessed);
                self.created = ft(16).or(self.created);
                return;
            }
            p += 4 + usize::from(size);
        }
    }

    fn apply_ut(&mut self, d: &[u8]) {
        let Ok(flags) = d.u8_at(0) else {
            return;
        };
        let mut p = 1;
        let mut take = |bit: u8| -> Option<i64> {
            if flags & bit == 0 {
                return None;
            }
            let v = d.i32_le(p).ok().map(i64::from);
            p += 4;
            v
        };
        let m = take(1);
        let a = take(2);
        let c = take(4);
        if self.modified.is_none() {
            self.modified = m;
        }
        if self.accessed.is_none() {
            self.accessed = a;
        }
        if self.created.is_none() {
            self.created = c;
        }
    }

    /// Info-ZIP Unicode Path: used when its CRC matches the stored name.
    fn apply_unicode_path(&mut self, d: &[u8]) {
        if d.u8_at(0) != Ok(1) {
            return;
        }
        let Ok(crc) = d.u32_le(1) else { return };
        if crc != crc32fast::hash(&self.name) {
            return;
        }
        if let Some(Ok(s)) = d.get(5..).map(std::str::from_utf8) {
            self.unicode_name = Some(s.to_owned());
        }
    }
}

/// The end-of-central-directory information.
#[derive(Debug, Clone, Copy)]
struct Directory {
    /// Absolute start of the central directory.
    start: u64,
    size: u64,
    /// Added to every local-header offset (prepended data / SFX stub).
    delta: i64,
}

/// Locates the central directory.
fn find_directory(src: &dyn ReadAt) -> Option<Directory> {
    let size = src.size();
    let tail_len = size.min(65_557 + 22);
    let tail_start = size - tail_len;
    let tail = read_upto(src, tail_start, usize::try_from(tail_len).ok()?).ok()?;
    for pos in memchr::memmem::rfind_iter(&tail, EOCD_SIG) {
        let Ok(eocd) = tail.sub(pos, 22) else {
            continue;
        };
        let comment_len = u64::from(eocd.u16_le(20).ok()?);
        let abs = tail_start + pos as u64;
        if abs + 22 + comment_len > size {
            continue;
        }
        let mut cd_size = u64::from(eocd.u32_le(12).ok()?);
        let mut cd_offset = u64::from(eocd.u32_le(16).ok()?);
        let mut record_pos = abs;
        // ZIP64 locator right before the EOCD.
        if abs >= 20
            && let Ok(loc) = read_upto(src, abs - 20, 20)
            && loc.starts_with(LOCATOR_SIG)
            && let Ok(off64) = loc.u64_le(8)
        {
            let candidates = [Some(off64), (abs - 20).checked_sub(56)];
            for c in candidates.into_iter().flatten() {
                if let Ok(rec) = read_upto(src, c, 56)
                    && rec.starts_with(EOCD64_SIG)
                    && let (Ok(s), Ok(o)) = (rec.u64_le(40), rec.u64_le(48))
                {
                    cd_size = s;
                    cd_offset = o;
                    record_pos = c;
                    break;
                }
            }
        }
        let Some(actual) = record_pos.checked_sub(cd_size) else {
            continue;
        };
        let at = |o: u64| read_upto(src, o, 4).is_ok_and(|b| b == CENTRAL_SIG);
        // An empty archive has an empty directory.
        if cd_size == 0 {
            return Some(Directory {
                start: actual,
                size: 0,
                delta: 0,
            });
        }
        if at(cd_offset) {
            return Some(Directory {
                start: cd_offset,
                size: cd_size,
                delta: 0,
            });
        }
        if at(actual) {
            let delta = i64::try_from(actual).ok()? - i64::try_from(cd_offset).ok()?;
            return Some(Directory {
                start: actual,
                size: cd_size,
                delta,
            });
        }
    }
    None
}

/// Reads the central directory entries (at most `max + 1`).
fn read_directory(
    src: &dyn ReadAt,
    dir: Directory,
    max: u64,
    d: &mut Driver<'_>,
) -> Result<Vec<ZipEntry>, ArchiveError> {
    let mut r = BufReader::with_capacity(256 * 1024, RangeReader::new(src, dir.start, dir.size));
    let mut out = Vec::new();
    let mut fixed = [0u8; 46];
    loop {
        if out.len() as u64 > max {
            break;
        }
        if out.len() % 4096 == 0 {
            d.check_cancel()?;
        }
        match read_full(&mut r, &mut fixed) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                d.warn(format!("central directory: {e}"));
                break;
            }
        }
        if !fixed.starts_with(CENTRAL_SIG) {
            d.warn("central directory: bad entry signature");
            break;
        }
        let f = &fixed[..];
        let name_len = usize::from(f.u16_le(28).unwrap_or(0));
        let extra_len = usize::from(f.u16_le(30).unwrap_or(0));
        let comment_len = usize::from(f.u16_le(32).unwrap_or(0));
        let mut var = vec![0u8; name_len + extra_len + comment_len];
        if !read_full(&mut r, &mut var).unwrap_or(false) {
            d.warn("central directory: truncated entry");
            break;
        }
        let mut e = ZipEntry {
            made_by: f.u16_le(4).unwrap_or(0),
            flags: f.u16_le(8).unwrap_or(0),
            method: f.u16_le(10).unwrap_or(0),
            dos_time: f.u16_le(12).unwrap_or(0),
            dos_date: f.u16_le(14).unwrap_or(0),
            crc: f.u32_le(16).unwrap_or(0),
            csize: u64::from(f.u32_le(20).unwrap_or(0)),
            usize: u64::from(f.u32_le(24).unwrap_or(0)),
            external: f.u32_le(38).unwrap_or(0),
            local_offset: u64::from(f.u32_le(42).unwrap_or(0)),
            name: var.get(..name_len).unwrap_or_default().to_vec(),
            ..ZipEntry::default()
        };
        e.apply_extra(
            var.get(name_len..name_len + extra_len).unwrap_or_default(),
            true,
        );
        out.push(e);
    }
    Ok(out)
}

/// Fills `buf`; `Ok(false)` on a clean end of input before any byte.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(buf.get_mut(got..).unwrap_or_default()) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated",
                ));
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Selects which members are delivered and how they are named (Office
/// Open XML extraction keeps only media and embeddings).
pub(crate) type Filter<'f> = &'f mut dyn FnMut(&Entry) -> Option<Entry>;

/// A [`Filter`] reborrowed for one member.
type MemberFilter<'r, 'f> = Option<&'r mut (dyn FnMut(&Entry) -> Option<Entry> + 'f)>;

/// Extracts a zip archive (`filter`: optional member selection/renaming).
pub(crate) fn extract(
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
    mut filter: Option<Filter<'_>>,
) -> Result<(), ArchiveError> {
    let max = d.opts.max_members;
    if let Some(dir) = find_directory(src) {
        let entries = read_directory(src, dir, max, d)?;
        if !entries.is_empty() || dir.size == 0 {
            // Autopsy checks every item's declared ratio before extracting
            // anything.
            for (i, z) in entries.iter().enumerate() {
                d.check_declared(&z.to_entry(i as u64, z.encrypted()))?;
            }
            for (i, z) in entries.iter().enumerate() {
                let local = z
                    .local_offset
                    .checked_add_signed(dir.delta)
                    .unwrap_or(u64::MAX);
                member(src, d, z.clone(), local, i as u64, filter.as_deref_mut())?;
            }
            return Ok(());
        }
    }
    d.warn("no usable central directory; walking local headers");
    walk_local(src, d, filter)
}

/// Reads the local header at `offset`; returns the data start and the
/// local header's extra-field times merged into `z`.
fn local_header(src: &dyn ReadAt, offset: u64, z: &mut ZipEntry) -> Result<u64, String> {
    let h = read_upto(src, offset, 30).map_err(|e| e.to_string())?;
    if h.len() < 30 || !h.starts_with(LOCAL_SIG) {
        return Err("bad local header".into());
    }
    let n = u64::from(h.u16_le(26).unwrap_or(0));
    let m = h.u16_le(28).unwrap_or(0);
    if m > 0 {
        let extra = read_upto(src, offset + 30 + n, usize::from(m)).unwrap_or_default();
        let mut l = ZipEntry::default();
        l.apply_extra(&extra, false);
        z.accessed = z.accessed.or(l.accessed);
        z.created = z.created.or(l.created);
        z.modified = z.modified.or(l.modified);
    }
    Ok(offset + 30 + n + u64::from(m))
}

/// Delivers one member whose local header is at `local`.
fn member(
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
    mut z: ZipEntry,
    local: u64,
    index: u64,
    filter: MemberFilter<'_, '_>,
) -> Result<(), ArchiveError> {
    let data_start = local_header(src, local, &mut z);
    let mut encrypted = z.encrypted();
    let mut keys = None;
    if encrypted
        && z.method != METHOD_AES
        && z.flags & FLAG_STRONG == 0
        && let (Some(pw), Ok(start)) = (d.opts.password.as_deref(), &data_start)
    {
        keys = zipcrypto_check(src, *start, &z, pw);
        encrypted = keys.is_none();
    }
    let mut entry = z.to_entry(index, encrypted);
    if let Some(f) = filter {
        match f(&entry) {
            Some(e) => entry = e,
            None => return Ok(()),
        }
    }
    let wanted = d.begin(&entry)?;
    if !wanted {
        let end = if entry.encrypted {
            MemberEnd::Skipped("encrypted".into())
        } else {
            MemberEnd::Complete
        };
        return d.end(&end);
    }
    let start = match data_start {
        Ok(s) => s,
        Err(why) => return d.end(&MemberEnd::Failed(why)),
    };
    let (start, csize) = match keys {
        Some(_) => (start + 12, z.csize.saturating_sub(12)),
        None => (start, z.csize),
    };
    let mut raw = RangeReader::new(src, start, csize);
    let input: Box<dyn Read + '_> = match keys {
        Some(k) => Box::new(ZipCryptoReader {
            inner: &mut raw,
            keys: k,
        }),
        None => Box::new(&mut raw),
    };
    let result = match decoder(z.method, input, &z, d.opts.max_dictionary) {
        Ok(mut dec) => {
            let mut crc = crc32fast::Hasher::new();
            let mut verify = |b: &[u8]| crc.update(b);
            match d.copy(&mut dec, Some(&mut verify)) {
                Ok(()) => {
                    if crc.finalize() == z.crc {
                        MemberEnd::Complete
                    } else {
                        MemberEnd::ChecksumMismatch
                    }
                }
                Err(CopyError::Read(e)) => MemberEnd::Failed(e.to_string()),
                Err(CopyError::Stop(e)) => return Err(e),
            }
        }
        Err(why) => MemberEnd::Skipped(why),
    };
    d.end(&result)
}

/// A decoder for a zip compression method.
fn decoder<'r>(
    method: u16,
    input: Box<dyn Read + 'r>,
    z: &ZipEntry,
    max_dict: u64,
) -> Result<Box<dyn Read + 'r>, String> {
    let buffered = BufReader::with_capacity(64 * 1024, input);
    Ok(match method {
        0 => Box::new(buffered),
        8 => Box::new(flate2::bufread::DeflateDecoder::new(buffered)),
        12 => Box::new(bzip2::bufread::BzDecoder::new(buffered)),
        14 => lzma_decoder(buffered, z, max_dict)?,
        93 => Box::new(crate::single::ZstdFrames::new(buffered, max_dict)),
        95 => Box::new(lzma_rust2::XzReader::new_mem_limit(
            buffered,
            true,
            mem_limit_kb(max_dict),
        )),
        METHOD_AES => return Err("WinZip AES encryption".into()),
        9 => return Err("unsupported compression method deflate64".into()),
        m => return Err(format!("unsupported compression method {m}")),
    })
}

/// Memory limit for the xz/LZMA decoders, in KiB.
pub(crate) fn mem_limit_kb(max_dict: u64) -> u32 {
    u32::try_from(max_dict / 1024 + 1024).unwrap_or(u32::MAX - 1)
}

/// Zip's LZMA: a 4-byte version/size header and 5 property bytes, then a
/// raw stream (end marker when flag bit 1 is set).
fn lzma_decoder<'r>(
    mut r: BufReader<Box<dyn Read + 'r>>,
    z: &ZipEntry,
    max_dict: u64,
) -> Result<Box<dyn Read + 'r>, String> {
    let mut hdr = [0u8; 9];
    r.read_exact(&mut hdr).map_err(|e| e.to_string())?;
    let props = hdr[4];
    let dict = u32::from_le_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]);
    if u64::from(dict) > max_dict {
        return Err(format!("LZMA dictionary of {dict} bytes exceeds the limit"));
    }
    let size = if z.flags & 2 != 0 { u64::MAX } else { z.usize };
    lzma_rust2::LzmaReader::new_with_props(r, size, props, dict, None)
        .map(|l| Box::new(l) as Box<dyn Read + 'r>)
        .map_err(|e| e.to_string())
}

/// Walks local headers from the start (no central directory).
fn walk_local(
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
    mut filter: Option<Filter<'_>>,
) -> Result<(), ArchiveError> {
    let size = src.size();
    let mut pos = 0u64;
    let mut index = 0u64;
    while pos < size {
        d.check_cancel()?;
        let Some(at) = next_signature(src, pos, LOCAL_SIG)? else {
            break;
        };
        pos = at;
        let h = read_upto(src, pos, 30)?;
        if h.len() < 30 {
            break;
        }
        let mut z = ZipEntry {
            flags: h.u16_le(6).unwrap_or(0),
            method: h.u16_le(8).unwrap_or(0),
            dos_time: h.u16_le(10).unwrap_or(0),
            dos_date: h.u16_le(12).unwrap_or(0),
            crc: h.u32_le(14).unwrap_or(0),
            csize: u64::from(h.u32_le(18).unwrap_or(0)),
            usize: u64::from(h.u32_le(22).unwrap_or(0)),
            ..ZipEntry::default()
        };
        let n = usize::from(h.u16_le(26).unwrap_or(0));
        let m = usize::from(h.u16_le(28).unwrap_or(0));
        let var = read_upto(src, pos + 30, n + m)?;
        z.name = var.get(..n).unwrap_or_default().to_vec();
        z.apply_extra(var.get(n..).unwrap_or_default(), true);
        let data_start = pos + 30 + (n + m) as u64;
        if z.flags & FLAG_DESCRIPTOR != 0 && z.csize == 0 {
            // The sizes follow the data, in the data descriptor.
            if let Some((csize, usize, crc)) = find_descriptor(src, data_start)? {
                z.csize = csize;
                z.usize = usize;
                z.crc = crc;
            } else {
                let end = next_signature(src, data_start, LOCAL_SIG)?.unwrap_or(size);
                z.csize = end.saturating_sub(data_start);
            }
        }
        member(src, d, z.clone(), pos, index, filter.as_deref_mut())?;
        index += 1;
        let mut next = data_start.saturating_add(z.csize);
        if z.flags & FLAG_DESCRIPTOR != 0 {
            next = skip_descriptor(src, next)?;
        }
        pos = next.max(pos + 1);
    }
    Ok(())
}

/// The next offset ≥ `from` holding `sig`.
fn next_signature(src: &dyn ReadAt, from: u64, sig: &[u8]) -> Result<Option<u64>, ArchiveError> {
    const WIN: usize = 1 << 20;
    let mut pos = from;
    while pos < src.size() {
        let buf = read_upto(src, pos, WIN)?;
        if buf.is_empty() {
            break;
        }
        if let Some(i) = memchr::memmem::find(&buf, sig) {
            return Ok(Some(pos + i as u64));
        }
        if buf.len() < sig.len() {
            break;
        }
        pos += (buf.len() - (sig.len() - 1)) as u64;
    }
    Ok(None)
}

/// Finds a data descriptor after `data_start` whose compressed size
/// matches its distance: `(csize, usize, crc)`.
fn find_descriptor(
    src: &dyn ReadAt,
    data_start: u64,
) -> Result<Option<(u64, u64, u32)>, ArchiveError> {
    let mut from = data_start;
    while let Some(at) = next_signature(src, from, DESCRIPTOR_SIG)? {
        let b = read_upto(src, at, 24)?;
        let dist = at - data_start;
        if b.u32_le(8).is_ok_and(|c| u64::from(c) == dist) {
            return Ok(Some((
                dist,
                u64::from(b.u32_le(12).unwrap_or(0)),
                b.u32_le(4).unwrap_or(0),
            )));
        }
        if b.u64_le(8).is_ok_and(|c| c == dist) {
            return Ok(Some((
                dist,
                b.u64_le(16).unwrap_or(0),
                b.u32_le(4).unwrap_or(0),
            )));
        }
        from = at + 1;
    }
    Ok(None)
}

/// Skips a data descriptor (with or without its signature).
fn skip_descriptor(src: &dyn ReadAt, at: u64) -> Result<u64, ArchiveError> {
    let b = read_upto(src, at, 4)?;
    Ok(if b == DESCRIPTOR_SIG {
        at + 16
    } else {
        at + 12
    })
}

// --- ZipCrypto (traditional PKWARE encryption) ---

#[derive(Debug, Clone, Copy)]
struct Keys([u32; 3]);

fn crc_byte(crc: u32, b: u8) -> u32 {
    let mut c = (crc ^ u32::from(b)) & 0xFF;
    for _ in 0..8 {
        c = if c & 1 != 0 {
            0xEDB8_8320 ^ (c >> 1)
        } else {
            c >> 1
        };
    }
    c ^ (crc >> 8)
}

impl Keys {
    fn new(password: &[u8]) -> Self {
        let mut k = Self([0x1234_5678, 0x2345_6789, 0x3456_7890]);
        for &b in password {
            k.update(b);
        }
        k
    }

    fn update(&mut self, b: u8) {
        self.0[0] = crc_byte(self.0[0], b);
        self.0[1] = self.0[1]
            .wrapping_add(self.0[0] & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        self.0[2] = crc_byte(self.0[2], (self.0[1] >> 24) as u8);
    }

    fn stream_byte(&self) -> u8 {
        let t = (self.0[2] | 2) & 0xFFFF;
        (t.wrapping_mul(t ^ 1) >> 8) as u8
    }

    fn decrypt(&mut self, c: u8) -> u8 {
        let p = c ^ self.stream_byte();
        self.update(p);
        p
    }
}

/// Checks the password against the 12-byte encryption header; returns the
/// keys positioned after it.
fn zipcrypto_check(src: &dyn ReadAt, start: u64, z: &ZipEntry, pw: &[u8]) -> Option<Keys> {
    let hdr = read_upto(src, start, 12).ok()?;
    if hdr.len() < 12 {
        return None;
    }
    let mut k = Keys::new(pw);
    let mut last = 0;
    for &c in &hdr {
        last = k.decrypt(c);
    }
    let check = if z.flags & FLAG_DESCRIPTOR != 0 {
        (z.dos_time >> 8) as u8
    } else {
        (z.crc >> 24) as u8
    };
    (last == check).then_some(k)
}

struct ZipCryptoReader<'r, R> {
    inner: &'r mut R,
    keys: Keys,
}

impl<R: Read> Read for ZipCryptoReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        for b in buf.iter_mut().take(n) {
            *b = self.keys.decrypt(*b);
        }
        Ok(n)
    }
}

/// Encrypts with `ZipCrypto` (used by tests and fixture builders).
#[cfg(test)]
pub(crate) fn zipcrypto_encrypt(password: &[u8], header: [u8; 12], data: &[u8]) -> Vec<u8> {
    let mut k = Keys::new(password);
    header
        .iter()
        .chain(data)
        .map(|&p| {
            let c = p ^ k.stream_byte();
            k.update(p);
            c
        })
        .collect()
}

/// Entry names of a zip's central directory (for format refinement).
pub(crate) fn names(src: &dyn ReadAt, limit: usize) -> Vec<Vec<u8>> {
    let Some(dir) = find_directory(src) else {
        return Vec::new();
    };
    let opts = crate::Options::default();
    let mut nul = NullSink;
    let mut d = Driver::new(src.size(), &opts, &mut nul);
    read_directory(src, dir, limit as u64, &mut d)
        .map(|v| v.into_iter().take(limit).map(|z| z.name).collect())
        .unwrap_or_default()
}

struct NullSink;

impl crate::Sink for NullSink {
    fn begin(&mut self, _: &Entry) -> std::io::Result<bool> {
        Ok(false)
    }
    fn write(&mut self, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn end(&mut self, _: &MemberEnd) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zipcrypto_round_trip() {
        let mut hdr = [7u8; 12];
        hdr[11] = 0xAB;
        let enc = zipcrypto_encrypt(b"secret", hdr, b"hello");
        let mut k = Keys::new(b"secret");
        let dec: Vec<u8> = enc.iter().map(|&c| k.decrypt(c)).collect();
        assert_eq!(&dec[..12], &hdr);
        assert_eq!(&dec[12..], b"hello");
    }

    #[test]
    fn extra_fields() {
        let mut z = ZipEntry {
            usize: 0xFFFF_FFFF,
            csize: 0xFFFF_FFFF,
            local_offset: 0xFFFF_FFFF,
            ..ZipEntry::default()
        };
        let mut extra = vec![1, 0, 24, 0];
        extra.extend(5_000_000_000u64.to_le_bytes());
        extra.extend(7u64.to_le_bytes());
        extra.extend(9u64.to_le_bytes());
        // UT: mtime only.
        extra.extend([0x55, 0x54, 5, 0, 1]);
        extra.extend(1_600_000_000i32.to_le_bytes());
        // Truncated trailing field is ignored.
        extra.extend([0x0A, 0x00, 0xFF]);
        z.apply_extra(&extra, true);
        assert_eq!(z.usize, 5_000_000_000);
        assert_eq!(z.csize, 7);
        assert_eq!(z.local_offset, 9);
        assert_eq!(z.modified, Some(1_600_000_000));
    }
}
