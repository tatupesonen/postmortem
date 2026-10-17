//! Internet Explorer 4–9 cache and history files (`index.dat`, the MSIE
//! Cache File format, "MSIECF").
//!
//! Layout (see libmsiecf's format notes): a header with the signature
//! `Client UrlCache MMF Ver 5.2`, the offset of the first `HASH` table,
//! the cache directory names and an allocation bitmap (one bit per
//! 128-byte block, blocks starting at 0x4000); then 128-byte blocks
//! holding `HASH` tables (8-byte entries: hash with flags, record offset;
//! chained by a next-table offset) and records (`URL `, `LEAK`, `REDR`).
//!
//! Records are found through the hash tables (as pasco2, which Autopsy
//! runs, does). Everything is read positionally from a [`ReadAt`] with
//! bounds, loop and size limits; malformed structures are skipped.

use std::collections::HashSet;

use pm_core::ReadAt;

use crate::time::filetime;

/// The 24-byte signature prefix; the format version follows.
pub const SIGNATURE: &[u8] = b"Client UrlCache MMF Ver ";
/// Block size.
pub const BLOCK: u64 = 0x80;
/// Offset of the allocation bitmap.
pub const BITMAP_OFFSET: u64 = 0x250;
/// Offset of the first block covered by the bitmap.
pub const BLOCKS_START: u64 = 0x4000;
const MAX_TABLES: usize = 4096;
const MAX_RECORD_BLOCKS: u64 = 1024;

/// Parse errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not an index.dat file")]
    NotIndexDat,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Kind of a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// `URL `: a cached / visited URL.
    Url,
    /// `LEAK`: an entry whose cached file could not be deleted.
    Leak,
    /// `REDR`: a redirect.
    Redirect,
}

impl RecordKind {
    /// pasco2's type column.
    pub fn label(self) -> &'static str {
        match self {
            Self::Url => "URL",
            Self::Leak => "LEAK",
            Self::Redirect => "REDR",
        }
    }
}

/// One record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: RecordKind,
    /// File offset of the record.
    pub offset: u64,
    /// The location (`Visited: user@url`, `:2004082220040823: user@url`,
    /// `Cookie:user@host/`, a plain URL, ...).
    pub location: String,
    /// Last modification time (Unix seconds).
    pub modified: Option<i64>,
    /// Last access time (Unix seconds).
    pub accessed: Option<i64>,
    /// Name of the cached file (cache index files).
    pub filename: String,
    /// The cache directory holding the file (`0VUUYHWA`).
    pub directory: String,
    /// Hit count.
    pub hits: u32,
    /// The record's data (HTTP response headers for cache entries).
    pub data: Vec<u8>,
}

/// An opened `index.dat`.
#[derive(Debug)]
pub struct IndexDat<'a> {
    src: &'a dyn ReadAt,
    size: u64,
    /// `Ver 5.2` etc.
    pub version: String,
    first_table: u64,
    /// Cache directory names, by index.
    pub directories: Vec<String>,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        b.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn cstr(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    // Locations are ANSI (Windows-1252 on western systems); keep bytes
    // that are not UTF-8 readable.
    let s = b.get(..n).unwrap_or_default();
    match std::str::from_utf8(s) {
        Ok(t) => t.to_owned(),
        Err(_) => s.iter().map(|&c| char::from(c)).collect(),
    }
}

impl<'a> IndexDat<'a> {
    /// Reads the header.
    pub fn open(src: &'a dyn ReadAt) -> Result<Self, Error> {
        let size = src.size();
        let mut head = vec![0u8; usize::try_from(size.min(BITMAP_OFFSET)).unwrap_or(0)];
        src.read_exact_at(0, &mut head)?;
        if !head.starts_with(SIGNATURE) {
            return Err(Error::NotIndexDat);
        }
        let version = cstr(head.get(SIGNATURE.len()..28).unwrap_or_default());
        let first_table = u64::from(u32_at(&head, 0x20).ok_or(Error::NotIndexDat)?);
        let mut directories = Vec::new();
        if let Some(n) = u32_at(&head, 0x48) {
            for i in 0..(n as usize).min(64) {
                let at = 0x4c + i * 12 + 4;
                let Some(name) = head.get(at..at + 8) else {
                    break;
                };
                directories.push(cstr(name));
            }
        }
        Ok(Self {
            src,
            size,
            version,
            first_table,
            directories,
        })
    }

    /// Whether the block holding `offset` is marked allocated in the
    /// bitmap (`None` outside the bitmap's range).
    pub fn is_allocated(&self, offset: u64) -> Option<bool> {
        let block = offset.checked_sub(BLOCKS_START)? / BLOCK;
        let byte = BITMAP_OFFSET + block / 8;
        if byte >= BLOCKS_START {
            return None;
        }
        let mut b = [0u8; 1];
        self.src.read_exact_at(byte, &mut b).ok()?;
        Some(b[0] & (1 << (block % 8)) != 0)
    }

    /// Offsets of the records referenced by the hash tables, in table
    /// order (deduplicated; unused/uninitialised entries skipped).
    pub fn record_offsets(&self) -> Vec<u64> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut tables = HashSet::new();
        let mut table = self.first_table;
        while table != 0 && table < self.size && tables.insert(table) && tables.len() <= MAX_TABLES
        {
            let mut head = [0u8; 16];
            if self.src.read_exact_at(table, &mut head).is_err() || &head[..4] != b"HASH" {
                break;
            }
            let blocks = u64::from(u32_at(&head, 4).unwrap_or(0)).min(MAX_RECORD_BLOCKS * 64);
            let len = (blocks * BLOCK).min(self.size - table);
            let Ok(buf) = self
                .src
                .read_vec_at(table, usize::try_from(len).unwrap_or(0))
            else {
                break;
            };
            let mut e = 16;
            while e + 8 <= buf.len() {
                let hash = u32_at(&buf, e).unwrap_or(0);
                let rec = u32_at(&buf, e + 4).unwrap_or(0);
                e += 8;
                // 0x1 = uninitialised, 0x3 = unused (libmsiecf).
                if hash == 0x1 || hash == 0x3 {
                    continue;
                }
                if rec == 0 || rec == 0x0BAD_F00D || rec == 0xDEAD_BEEF {
                    continue;
                }
                let rec = u64::from(rec);
                if rec % BLOCK != 0 || rec >= self.size || !seen.insert(rec) {
                    continue;
                }
                out.push(rec);
            }
            table = u64::from(u32_at(&head, 8).unwrap_or(0));
        }
        out
    }

    /// The record at `offset` (`None` if it is not a URL/LEAK/REDR record).
    pub fn record(&self, offset: u64) -> Option<Record> {
        let mut head = [0u8; 8];
        self.src.read_exact_at(offset, &mut head).ok()?;
        let kind = match &head[..4] {
            b"URL " => RecordKind::Url,
            b"LEAK" => RecordKind::Leak,
            b"REDR" => RecordKind::Redirect,
            _ => return None,
        };
        let blocks = u64::from(u32_at(&head, 4)?).clamp(1, MAX_RECORD_BLOCKS);
        let len = (blocks * BLOCK).min(self.size - offset);
        let r = self
            .src
            .read_vec_at(offset, usize::try_from(len).ok()?)
            .ok()?;
        if kind == RecordKind::Redirect {
            let location = cstr(r.get(0x10..).unwrap_or_default());
            return Some(Record {
                kind,
                offset,
                location,
                modified: None,
                accessed: None,
                filename: String::new(),
                directory: String::new(),
                hits: 0,
                data: Vec::new(),
            });
        }
        let field = |o: usize| u32_at(&r, o).map(|v| v as usize);
        let string_at = |o: Option<usize>| {
            o.filter(|&o| o > 0)
                .and_then(|o| r.get(o..))
                .map(cstr)
                .unwrap_or_default()
        };
        let location = string_at(field(0x34));
        let filename = string_at(field(0x3c));
        let dir_index = r.get(0x38).copied().unwrap_or(0xFF);
        let directory = if dir_index == 0xFF || dir_index == 0xFE {
            String::new()
        } else {
            self.directories
                .get(usize::from(dir_index))
                .cloned()
                .unwrap_or_default()
        };
        let data = match (field(0x44), field(0x48)) {
            (Some(o), Some(n)) if o > 0 && n > 0 => o
                .checked_add(n)
                .and_then(|end| r.get(o..end))
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Some(Record {
            kind,
            offset,
            location,
            modified: u64_at(&r, 0x08).and_then(filetime),
            accessed: u64_at(&r, 0x10).and_then(filetime),
            filename,
            directory,
            hits: u32_at(&r, 0x54).unwrap_or(0),
            data,
        })
    }

    /// Every record reachable from the hash tables.
    pub fn records(&self) -> impl Iterator<Item = Record> + '_ {
        self.record_offsets()
            .into_iter()
            .filter_map(move |o| self.record(o))
    }
}

/// pasco2's history line interpretation as done by Autopsy's
/// `parsePascoOutput`: `(user, url)` of a location
/// (`Visited: user@url`, `:2004082220040823: user@url`, a plain URL).
pub fn split_location(location: &str, is_domain: impl Fn(&str) -> bool) -> (String, String) {
    let Some((left, right)) = location.split_once('@') else {
        return (String::new(), location.trim().to_owned());
    };
    if is_domain(left) {
        return (String::new(), location.trim().to_owned());
    }
    let user = remove_colon_groups(&left.replace("Visited:", "").replace(":Host:", ""))
        .trim()
        .to_owned();
    let url = remove_colon_groups(&right.replace("Visited:", ""))
        .replace(":Host:", "")
        .trim()
        .to_owned();
    (user, url)
}

/// Java `replaceAll("(:)(.*?)(:)", "")` / `":(.*?):"`.
fn remove_colon_groups(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(':') {
        let Some(len) = rest.get(start + 1..).and_then(|r| r.find(':')) else {
            break;
        };
        out.push_str(rest.get(..start).unwrap_or_default());
        rest = rest.get(start + 1 + len + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Builds `index.dat` files (tests, fuzz seeds).
#[cfg(feature = "testing")]
#[allow(
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    reason = "fixture builder for tests and fuzz seeds"
)]
pub mod testing {
    use super::{BITMAP_OFFSET, BLOCK, BLOCKS_START};

    /// One record to write.
    #[derive(Debug, Clone)]
    pub struct Spec {
        /// `b"URL "`, `b"LEAK"` or `b"REDR"`.
        pub kind: [u8; 4],
        pub location: String,
        pub modified: u64,
        pub accessed: u64,
        pub filename: String,
        pub dir_index: u8,
        pub hits: u32,
        pub data: Vec<u8>,
        /// Referenced from the hash table (else left in a block nobody
        /// points to, with its bitmap bit clear).
        pub referenced: bool,
    }

    impl Spec {
        /// A referenced URL record.
        pub fn url(location: &str, accessed: u64) -> Self {
            Self {
                kind: *b"URL ",
                location: location.to_owned(),
                modified: 0,
                accessed,
                filename: String::new(),
                dir_index: 0xFF,
                hits: 1,
                data: Vec::new(),
                referenced: true,
            }
        }
    }

    fn record(s: &Spec) -> Vec<u8> {
        if &s.kind == b"REDR" {
            let mut r = vec![0u8; 0x10];
            r[..4].copy_from_slice(&s.kind);
            r.extend(s.location.as_bytes());
            r.push(0);
            let blocks = r.len().div_ceil(BLOCK as usize);
            r.resize(blocks * BLOCK as usize, 0);
            r[4..8].copy_from_slice(&(blocks as u32).to_le_bytes());
            return r;
        }
        let mut r = vec![0u8; 0x68];
        r[..4].copy_from_slice(&s.kind);
        r[0x08..0x10].copy_from_slice(&s.modified.to_le_bytes());
        r[0x10..0x18].copy_from_slice(&s.accessed.to_le_bytes());
        r[0x38] = s.dir_index;
        r[0x54..0x58].copy_from_slice(&s.hits.to_le_bytes());
        let loc = r.len();
        r.extend(s.location.as_bytes());
        r.push(0);
        r[0x34..0x38].copy_from_slice(&(loc as u32).to_le_bytes());
        if !s.filename.is_empty() {
            let at = r.len();
            r.extend(s.filename.as_bytes());
            r.push(0);
            r[0x3c..0x40].copy_from_slice(&(at as u32).to_le_bytes());
        }
        if !s.data.is_empty() {
            let at = r.len();
            r.extend(&s.data);
            r[0x44..0x48].copy_from_slice(&(at as u32).to_le_bytes());
            r[0x48..0x4c].copy_from_slice(&(s.data.len() as u32).to_le_bytes());
        }
        let blocks = r.len().div_ceil(BLOCK as usize);
        r.resize(blocks * BLOCK as usize, 0);
        r[4..8].copy_from_slice(&(blocks as u32).to_le_bytes());
        r
    }

    /// A version 5.2 file with cache directories `dirs` and the records
    /// split over hash tables of `per_table` entries.
    pub fn build(dirs: &[&str], records: &[Spec], per_table: usize) -> Vec<u8> {
        let per_table = per_table.max(1);
        let mut f = vec![0u8; BLOCKS_START as usize];
        f[..28].copy_from_slice(b"Client UrlCache MMF Ver 5.2\0");
        f[0x48..0x4c].copy_from_slice(&(dirs.len() as u32).to_le_bytes());
        for (i, d) in dirs.iter().enumerate() {
            let at = 0x4c + i * 12 + 4;
            let n = d.len().min(8);
            f[at..at + n].copy_from_slice(&d.as_bytes()[..n]);
        }
        let referenced: Vec<&Spec> = records.iter().filter(|r| r.referenced).collect();
        let tables = referenced.len().div_ceil(per_table).max(1);
        let table_blocks = (16 + per_table * 8).div_ceil(BLOCK as usize);
        let table_len = table_blocks * BLOCK as usize;
        let mut table_offsets = Vec::new();
        for _ in 0..tables {
            table_offsets.push(f.len());
            f.resize(f.len() + table_len, 0);
        }
        let mut entries: Vec<(u32, u32)> = Vec::new();
        let mut allocated: Vec<(usize, usize)> =
            table_offsets.iter().map(|&o| (o, table_len)).collect();
        for (i, s) in records.iter().enumerate() {
            let at = f.len();
            let r = record(s);
            let len = r.len();
            f.extend(r);
            if s.referenced {
                entries.push((((i as u32 + 1) << 4) | 0x8, at as u32));
                allocated.push((at, len));
            }
        }
        for (t, &off) in table_offsets.iter().enumerate() {
            f[off..off + 4].copy_from_slice(b"HASH");
            f[off + 4..off + 8].copy_from_slice(&(table_blocks as u32).to_le_bytes());
            let next = table_offsets.get(t + 1).copied().unwrap_or(0) as u32;
            f[off + 8..off + 12].copy_from_slice(&next.to_le_bytes());
            for slot in 0..per_table {
                let e = off + 16 + slot * 8;
                let (h, r) = entries
                    .get(t * per_table + slot)
                    .copied()
                    .unwrap_or((0x3, 0x0BAD_F00D));
                f[e..e + 4].copy_from_slice(&h.to_le_bytes());
                f[e + 4..e + 8].copy_from_slice(&r.to_le_bytes());
            }
        }
        f[0x20..0x24].copy_from_slice(&(table_offsets[0] as u32).to_le_bytes());
        let total = f.len();
        f[0x1c..0x20].copy_from_slice(&(total as u32).to_le_bytes());
        let blocks = (total - BLOCKS_START as usize) / BLOCK as usize;
        f[0x24..0x28].copy_from_slice(&(blocks as u32).to_le_bytes());
        for (at, len) in allocated {
            let first = (at - BLOCKS_START as usize) / BLOCK as usize;
            for b in first..first + len / BLOCK as usize {
                let byte = BITMAP_OFFSET as usize + b / 8;
                if byte < BLOCKS_START as usize {
                    f[byte] |= 1 << (b % 8);
                }
            }
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Spec, build};
    use super::*;
    use pm_core::io::MemSource;

    const T: u64 = (1_093_532_400 + 11_644_473_600) * 10_000_000;

    fn sample() -> Vec<u8> {
        let mut cache = Spec::url("http://www.example.com/a.gif", T);
        cache.filename = "a[1].gif".into();
        cache.dir_index = 1;
        cache.data = b"HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\n\r\n".to_vec();
        let mut free = Spec::url("http://free.example.com/", T);
        free.referenced = false;
        let leak = Spec {
            kind: *b"LEAK",
            ..Spec::url("http://leak.example.com/", 0)
        };
        let redr = Spec {
            kind: *b"REDR",
            ..Spec::url("http://redir.example.com/", 0)
        };
        build(
            &["0VUUYHWA", "K9EV0DE3"],
            &[
                Spec::url(
                    "Visited: Mr. Evil@http://www.google.com/search?q=hacking+tools",
                    T,
                ),
                cache,
                free,
                leak,
                redr,
            ],
            2,
        )
    }

    #[test]
    fn reads_records_through_chained_tables() {
        let data = sample();
        let src = MemSource::new(data);
        let idx = IndexDat::open(&src).unwrap();
        assert_eq!(idx.version, "5.2");
        assert_eq!(idx.directories, vec!["0VUUYHWA", "K9EV0DE3"]);
        let recs: Vec<Record> = idx.records().collect();
        assert_eq!(recs.len(), 4);
        assert_eq!(
            recs[0].location,
            "Visited: Mr. Evil@http://www.google.com/search?q=hacking+tools"
        );
        assert_eq!(recs[0].accessed, Some(1_093_532_400));
        assert_eq!(recs[0].modified, None);
        assert_eq!(recs[1].filename, "a[1].gif");
        assert_eq!(recs[1].directory, "K9EV0DE3");
        assert!(recs[1].data.starts_with(b"HTTP/1.1 200"));
        assert_eq!(recs[2].kind, RecordKind::Leak);
        assert_eq!(recs[3].kind, RecordKind::Redirect);
        assert_eq!(recs[3].location, "http://redir.example.com/");
        // pasco2 TestAllocationLayer: referenced records are allocated in
        // the bitmap, the free one is not.
        for r in &recs {
            assert_eq!(idx.is_allocated(r.offset), Some(true));
        }
        let free_at = recs[1].offset + 2 * BLOCK;
        assert_eq!(
            idx.record(free_at).unwrap().location,
            "http://free.example.com/"
        );
        assert_eq!(idx.is_allocated(free_at), Some(false));
        assert_eq!(idx.is_allocated(0x100), None);
    }

    #[test]
    fn empty_and_corrupt_files() {
        // pasco2 TestRecordLayer: an empty history file has no records.
        let empty = build(&[], &[], 4);
        let src = MemSource::new(empty);
        assert_eq!(IndexDat::open(&src).unwrap().records().count(), 0);
        assert!(matches!(
            IndexDat::open(&MemSource::new(b"not an index".to_vec())),
            Err(Error::NotIndexDat | Error::Io(_))
        ));
        // A table pointing at itself terminates.
        let mut d = sample();
        let first = u32::from_le_bytes(d[0x20..0x24].try_into().unwrap()) as usize;
        let first_bytes = (first as u32).to_le_bytes();
        d[first + 8..first + 12].copy_from_slice(&first_bytes);
        let src = MemSource::new(d.clone());
        assert_eq!(IndexDat::open(&src).unwrap().records().count(), 2);
        for n in (0..d.len()).step_by(61) {
            let src = MemSource::new(d[..n].to_vec());
            if let Ok(i) = IndexDat::open(&src) {
                let _ = i.records().count();
            }
        }
    }

    #[test]
    fn locations_split_like_autopsy() {
        let dom = |s: &str| !crate::domain::extract_domain(s).is_empty();
        assert_eq!(
            split_location("Visited: Mr. Evil@http://www.google.com/", dom),
            ("Mr. Evil".into(), "http://www.google.com/".into())
        );
        assert_eq!(
            split_location(":2004082220040823: Mr. Evil@http://www.yahoo.com/", dom),
            ("Mr. Evil".into(), "http://www.yahoo.com/".into())
        );
        assert_eq!(
            split_location("Cookie:mr. evil@google.com/", dom),
            ("Cookie:mr. evil".into(), "google.com/".into())
        );
        assert_eq!(
            split_location("http://a.example.com/x", dom),
            (String::new(), "http://a.example.com/x".into())
        );
        assert_eq!(
            split_location("user@example.com@x", dom),
            ("user".into(), "example.com@x".into())
        );
        assert_eq!(remove_colon_groups(":a: b :c:d"), " b d");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096),
                        first in 0u32..0x6000) {
            let mut d = sample();
            d[0x20..0x24].copy_from_slice(&first.to_le_bytes());
            let at = 0x4000.min(d.len());
            d.truncate(at);
            d.extend(tail);
            let src = MemSource::new(d);
            if let Ok(i) = IndexDat::open(&src) {
                for r in i.records() {
                    let _ = i.is_allocated(r.offset);
                }
            }
        }
    }
}
