//! Chrome/Chromium "blockfile" disk cache (Autopsy
//! `ChromeCacheExtractor`): an `index` file with a hash table of cache
//! addresses, block files `data_0`..`data_3` holding entries, keys and
//! small streams, and `f_xxxxxx` files for large streams.
//!
//! A cache address (u32): bit 31 initialised, bits 28–30 file type
//! (external, rankings, 256 B / 1 KiB / 4 KiB blocks, ...), for block
//! files bits 24–25 the block count minus one, bits 16–23 the `data_N`
//! number and bits 0–15 the first block; for external files bits 0–27
//! the `f_` number. Block data starts after an 8 KiB file header.
//!
//! Fixed against Autopsy: hash collision chains (`next`) are followed,
//! the block count is decoded correctly, keys longer than one block are
//! read whole.

use std::collections::HashSet;
use std::sync::Arc;

use pm_core::ReadAt;

use crate::time::webkit;

/// Index file magic.
pub const INDEX_MAGIC: u32 = 0xC103_CAC3;
/// Size of the index header before the hash table.
pub const INDEX_HEADER: u64 = 368;
/// Size of a block file header.
pub const BLOCK_HEADER: u64 = 8192;
const MAX_TABLE: u32 = 1 << 22;
const MAX_CHAIN: usize = 4096;
const MAX_HEADER_STREAM: u32 = 1 << 20;
const MAX_KEY: u32 = 64 << 10;

/// Opens the cache's files by name (`index`, `data_1`, `f_00002a`).
pub trait CacheFiles {
    fn open(&self, name: &str) -> Option<Arc<dyn ReadAt>>;
}

/// A decoded cache address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address(pub u32);

impl Address {
    pub fn is_initialized(self) -> bool {
        self.0 & 0x8000_0000 != 0
    }

    /// File type (0 external, 1 rankings, 2 256 B, 3 1 KiB, 4 4 KiB,
    /// 5 block files, 6 entries, 7 evicted).
    pub fn file_type(self) -> u32 {
        (self.0 & 0x7000_0000) >> 28
    }

    pub fn is_external(self) -> bool {
        self.file_type() == 0
    }

    /// Size of one block of this file type.
    pub fn block_size(self) -> u64 {
        match self.file_type() {
            1 => 36,
            2 => 256,
            3 => 1024,
            4 => 4096,
            5 => 8,
            6 => 104,
            7 => 48,
            _ => 0,
        }
    }

    /// Number of blocks.
    pub fn num_blocks(self) -> u64 {
        u64::from((self.0 & 0x0300_0000) >> 24) + 1
    }

    pub fn start_block(self) -> u64 {
        u64::from(self.0 & 0xFFFF)
    }

    /// The file holding the data (`f_00002a` / `data_1`).
    pub fn file_name(self) -> String {
        if self.is_external() {
            format!("f_{:06x}", self.0 & 0x0FFF_FFFF)
        } else {
            format!("data_{}", (self.0 & 0x00FF_0000) >> 16)
        }
    }

    /// Byte offset of the data in a block file.
    pub fn offset(self) -> u64 {
        BLOCK_HEADER + self.start_block() * self.block_size()
    }
}

/// Where an entry's payload (stream 1) lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// A whole `f_xxxxxx` file.
    External { file: String, len: u64 },
    /// A byte range of a block file; Autopsy saves it as
    /// `data_N__<address as %08x>`.
    Block {
        file: String,
        offset: u64,
        len: u64,
        address: u32,
    },
}

impl Payload {
    /// The file holding the payload.
    pub fn file(&self) -> &str {
        match self {
            Self::External { file, .. } | Self::Block { file, .. } => file,
        }
    }

    /// Payload length.
    pub fn len(&self) -> u64 {
        match self {
            Self::External { len, .. } | Self::Block { len, .. } => *len,
        }
    }

    /// Whether the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Autopsy's name for an extracted in-block payload.
    pub fn derived_name(&self) -> Option<String> {
        match self {
            Self::Block { file, address, .. } => Some(format!("{file}__{address:08x}")),
            Self::External { .. } => None,
        }
    }
}

/// One cache entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's own address.
    pub address: u32,
    /// The block file holding the entry record (`data_1`).
    pub entry_file: String,
    /// The URL (cache key).
    pub key: String,
    /// Creation time (Unix seconds).
    pub created: i64,
    /// 0 normal, 1 evicted, 2 doomed.
    pub state: u32,
    /// HTTP response line of stream 0.
    pub response: String,
    /// Headers of stream 0, keys lower-cased, in order.
    pub headers: Vec<(String, String)>,
    /// The second non-empty stream (Autopsy's "cached item").
    pub payload: Option<Payload>,
}

impl Entry {
    /// Autopsy's `KAAMOS_HEADERS` text: `"key : value"` joined by `" \n"`.
    pub fn headers_text(&self) -> String {
        self.headers
            .iter()
            .map(|(k, v)| format!("{k} : {v}"))
            .collect::<Vec<_>>()
            .join(" \n")
    }

    /// A header value by lower-case name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// `content-encoding: br` (Autopsy marks the item `application/x-brotli`).
    pub fn is_brotli(&self) -> bool {
        self.header("content-encoding")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("br"))
    }
}

/// Parse errors of the index (entries that fail are skipped).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a Chrome cache index")]
    NotIndex,
    #[error("cache file {0} is missing")]
    Missing(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// A cache folder being read.
pub struct Cache<'a> {
    files: &'a dyn CacheFiles,
    index: Arc<dyn ReadAt>,
    /// `data_0`..`data_3` and others, opened lazily.
    blocks: std::collections::HashMap<String, Option<Arc<dyn ReadAt>>>,
    table_len: u32,
}

impl std::fmt::Debug for Cache<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("table_len", &self.table_len)
            .finish_non_exhaustive()
    }
}

impl<'a> Cache<'a> {
    /// Opens the index; like Autopsy, `data_0`..`data_3` must exist.
    pub fn open(files: &'a dyn CacheFiles) -> Result<Self, Error> {
        let index = files
            .open("index")
            .ok_or_else(|| Error::Missing("index".into()))?;
        let mut head = [0u8; 32];
        index.read_exact_at(0, &mut head)?;
        if u32_at(&head, 0) != Some(INDEX_MAGIC) {
            return Err(Error::NotIndex);
        }
        let table_len = u32_at(&head, 28).unwrap_or(0).min(MAX_TABLE);
        let mut blocks = std::collections::HashMap::new();
        for i in 0..4 {
            let name = format!("data_{i}");
            let f = files
                .open(&name)
                .ok_or_else(|| Error::Missing(name.clone()))?;
            blocks.insert(name, Some(f));
        }
        Ok(Self {
            files,
            index,
            blocks,
            table_len,
        })
    }

    fn block_file(&mut self, name: &str) -> Option<Arc<dyn ReadAt>> {
        if let Some(f) = self.blocks.get(name) {
            return f.clone();
        }
        let f = self.files.open(name);
        if self.blocks.len() < 256 {
            self.blocks.insert(name.to_owned(), f.clone());
        }
        f
    }

    /// Reads `len` bytes of block data at `addr`.
    fn read_block(&mut self, addr: Address, len: u64) -> Option<Vec<u8>> {
        if addr.is_external() || addr.block_size() == 0 {
            return None;
        }
        let f = self.block_file(&addr.file_name())?;
        let off = addr.offset();
        let avail = f.size().checked_sub(off)?;
        let n = len.min(avail);
        f.read_vec_at(off, usize::try_from(n).ok()?).ok()
    }

    /// Hash table length (number of buckets).
    pub fn table_len(&self) -> u32 {
        self.table_len
    }

    /// Calls `f` for every entry reachable from the hash table (buckets
    /// and their collision chains), until it returns `false`.
    pub fn entries(&mut self, mut f: impl FnMut(Entry) -> bool) -> Result<(), Error> {
        let len = u64::from(self.table_len) * 4;
        let avail = self.index.size().saturating_sub(INDEX_HEADER);
        let table = self
            .index
            .read_vec_at(INDEX_HEADER, usize::try_from(len.min(avail)).unwrap_or(0))?;
        let mut seen = HashSet::new();
        for chunk in table.chunks_exact(4) {
            let mut addr = Address(u32_at(chunk, 0).unwrap_or(0));
            let mut chain = 0;
            while addr.is_initialized() && chain < MAX_CHAIN && seen.insert(addr.0) {
                chain += 1;
                let Some((entry, next)) = self.entry(addr) else {
                    break;
                };
                if !f(entry) {
                    return Ok(());
                }
                addr = next;
            }
        }
        Ok(())
    }

    /// The entry at `addr` and its `next` address.
    pub fn entry(&mut self, addr: Address) -> Option<(Entry, Address)> {
        let rec = self.read_block(addr, addr.num_blocks() * addr.block_size())?;
        if rec.len() < 96 {
            return None;
        }
        let next = Address(u32_at(&rec, 4)?);
        let state = u32_at(&rec, 20)?;
        if state > 2 {
            return None;
        }
        let created = i64::from_le_bytes(rec.get(24..32)?.try_into().ok()?);
        let key_len = u32_at(&rec, 32)?;
        let long_key = Address(u32_at(&rec, 36)?);
        let key = if long_key.0 != 0 {
            if long_key.is_external() {
                let file = self.files.open(&long_key.file_name())?;
                let n = u64::from(key_len.min(MAX_KEY)).min(file.size());
                latin1(&file.read_vec_at(0, usize::try_from(n).ok()?).ok()?)
            } else {
                latin1(&self.read_block(long_key, u64::from(key_len.min(MAX_KEY)))?)
            }
        } else {
            let k = rec.get(96..).unwrap_or_default();
            let k = k.get(..k.len().min(key_len as usize)).unwrap_or(k);
            latin1(k)
        };
        let mut response = String::new();
        let mut headers = Vec::new();
        let mut non_empty = 0;
        let mut payload = None;
        for i in 0..4 {
            let size = u32_at(&rec, 40 + i * 4)?;
            let a = Address(u32_at(&rec, 56 + i * 4)?);
            if size == 0 || size > i32::MAX as u32 {
                continue;
            }
            non_empty += 1;
            if non_empty == 1 && !a.is_external() {
                if let Some(data) = self.read_block(a, u64::from(size.min(MAX_HEADER_STREAM))) {
                    (response, headers) = parse_headers(&data);
                }
            } else if non_empty == 2 {
                payload = Some(if a.is_external() {
                    Payload::External {
                        file: a.file_name(),
                        len: u64::from(size),
                    }
                } else {
                    Payload::Block {
                        file: a.file_name(),
                        offset: a.offset(),
                        len: u64::from(size),
                        address: a.0,
                    }
                });
            }
        }
        Some((
            Entry {
                address: addr.0,
                entry_file: addr.file_name(),
                key,
                created: webkit(created),
                state,
                response,
                headers,
                payload,
            },
            next,
        ))
    }
}

/// Bytes as Java `(char) byte` would turn them into a string... but
/// UTF-8 keys are decoded properly.
fn latin1(b: &[u8]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let s = b.get(..n).unwrap_or_default();
    match std::str::from_utf8(s) {
        Ok(t) => t.to_owned(),
        Err(_) => s.iter().map(|&c| char::from(c)).collect(),
    }
}

/// Autopsy's stream-0 header parse: from the first `HTTP`, NUL-separated
/// lines until a double NUL; the first is the response line, the others
/// `key:value` (key lower-cased).
pub fn parse_headers(data: &[u8]) -> (String, Vec<(String, String)>) {
    let Some(start) = data.windows(4).position(|w| w == b"HTTP") else {
        return (String::new(), Vec::new());
    };
    let mut response = String::new();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut i = start;
    let mut first = true;
    loop {
        let begin = i;
        while data.get(i).is_some_and(|&b| b != 0) {
            i += 1;
        }
        let line = String::from_utf8_lossy(data.get(begin..i).unwrap_or_default()).into_owned();
        if first {
            response = line;
            first = false;
        } else if let Some(p) = line.find(':').filter(|&p| p > 0) {
            let key = line.get(..p).unwrap_or_default().to_lowercase();
            let val = line.get(p + 1..).unwrap_or_default().to_owned();
            if let Some(e) = headers.iter_mut().find(|(k, _)| *k == key) {
                e.1 = val;
            } else {
                headers.push((key, val));
            }
        }
        if i >= data.len() || data.get(i + 1).is_none_or(|&b| b == 0) {
            break;
        }
        i += 1;
    }
    (response, headers)
}

/// Builds blockfile caches (tests, fuzz seeds).
#[cfg(feature = "testing")]
#[allow(
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    reason = "fixture builder for tests and fuzz seeds"
)]
pub mod testing {
    use std::collections::BTreeMap;

    use super::{BLOCK_HEADER, INDEX_HEADER, INDEX_MAGIC};

    /// One entry: key, creation (WebKit µs), header stream, payload and
    /// whether the payload goes to an external `f_` file.
    #[derive(Debug, Clone)]
    pub struct Spec {
        pub key: String,
        pub created: i64,
        pub headers: Vec<u8>,
        pub payload: Vec<u8>,
        pub external: bool,
        /// Hash bucket (entries sharing one are chained).
        pub bucket: u32,
    }

    fn addr_block(ftype: u32, file: u32, start: u32, blocks: u32) -> u32 {
        0x8000_0000 | (ftype << 28) | ((blocks - 1) << 24) | (file << 16) | start
    }

    /// The cache files by name for `entries` over `table_len` buckets.
    pub fn build(entries: &[Spec], table_len: u32) -> BTreeMap<String, Vec<u8>> {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut data1 = vec![0u8; BLOCK_HEADER as usize]; // entries (256 B)
        let mut data2 = vec![0u8; BLOCK_HEADER as usize]; // 1 KiB blocks
        let mut ext = 1u32;
        let mut table = vec![0u32; table_len as usize];
        let mut addrs = Vec::new();
        for e in entries {
            let key_bytes = e.key.as_bytes();
            let blocks = (96 + key_bytes.len() + 1).div_ceil(256).clamp(1, 4) as u32;
            let start = ((data1.len() - BLOCK_HEADER as usize) / 256) as u32;
            let me = addr_block(2, 1, start, blocks);
            let mut rec = vec![0u8; blocks as usize * 256];
            rec[24..32].copy_from_slice(&e.created.to_le_bytes());
            rec[32..36].copy_from_slice(&(key_bytes.len() as u32).to_le_bytes());
            let n = key_bytes.len().min(rec.len() - 97);
            rec[96..96 + n].copy_from_slice(&key_bytes[..n]);
            let mut put = |slot: usize, data: &[u8], external: bool| {
                if data.is_empty() {
                    return;
                }
                let a = if external {
                    let name = format!("f_{ext:06x}");
                    files.insert(name, data.to_vec());
                    let a = 0x8000_0000 | ext;
                    ext += 1;
                    a
                } else {
                    let nb = data.len().div_ceil(1024).clamp(1, 4) as u32;
                    let s = ((data2.len() - BLOCK_HEADER as usize) / 1024) as u32;
                    let mut d = data.to_vec();
                    d.resize(nb as usize * 1024, 0);
                    data2.extend(d);
                    addr_block(3, 2, s, nb)
                };
                rec[40 + slot * 4..44 + slot * 4]
                    .copy_from_slice(&(data.len() as u32).to_le_bytes());
                rec[56 + slot * 4..60 + slot * 4].copy_from_slice(&a.to_le_bytes());
            };
            put(0, &e.headers, false);
            put(1, &e.payload, e.external);
            data1.extend(rec);
            addrs.push((me, e.bucket));
        }
        // Chain entries sharing a bucket through `next`.
        for &(me, bucket) in &addrs {
            let b = (bucket % table_len.max(1)) as usize;
            if table[b] == 0 {
                table[b] = me;
            } else {
                // Append to the end of the chain.
                let mut cur = table[b];
                loop {
                    let off = BLOCK_HEADER as usize + ((cur & 0xFFFF) as usize) * 256;
                    let next =
                        u32::from_le_bytes(data1[off + 4..off + 8].try_into().unwrap_or([0; 4]));
                    if next == 0 {
                        data1[off + 4..off + 8].copy_from_slice(&me.to_le_bytes());
                        break;
                    }
                    cur = next;
                }
            }
        }
        let mut index = vec![0u8; INDEX_HEADER as usize];
        index[..4].copy_from_slice(&INDEX_MAGIC.to_le_bytes());
        index[6..8].copy_from_slice(&0x2u16.to_le_bytes());
        index[8..12].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        index[28..32].copy_from_slice(&table_len.to_le_bytes());
        for a in table {
            index.extend(a.to_le_bytes());
        }
        files.insert("index".into(), index);
        files.insert("data_0".into(), vec![0u8; BLOCK_HEADER as usize]);
        files.insert("data_1".into(), data1);
        files.insert("data_2".into(), data2);
        files.insert("data_3".into(), vec![0u8; BLOCK_HEADER as usize]);
        files
    }

    /// An HTTP header stream as Chrome stores it.
    pub fn headers(lines: &[&str]) -> Vec<u8> {
        let mut v = vec![0u8; 12];
        for l in lines {
            v.extend(l.as_bytes());
            v.push(0);
        }
        v.push(0);
        v
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pm_core::io::MemSource;

    use super::testing::{Spec, build, headers};
    use super::*;

    struct Files(BTreeMap<String, Vec<u8>>);

    impl CacheFiles for Files {
        fn open(&self, name: &str) -> Option<Arc<dyn ReadAt>> {
            self.0
                .get(name)
                .map(|d| Arc::new(MemSource::new(d.clone())) as Arc<dyn ReadAt>)
        }
    }

    fn sample() -> Files {
        let long_key = format!("https://example.com/{}", "x".repeat(300));
        Files(build(
            &[
                Spec {
                    key: "https://www.example.com/logo.png".into(),
                    created: 13_000_000_000_000_000,
                    headers: headers(&[
                        "HTTP/1.1 200 OK",
                        "Content-Type: image/png",
                        "Content-Encoding: br",
                    ]),
                    payload: b"\x89PNG small".to_vec(),
                    external: false,
                    bucket: 3,
                },
                Spec {
                    key: "https://cdn.example.com/big.js".into(),
                    created: 13_000_000_001_000_000,
                    headers: headers(&["HTTP/1.1 200", "content-type:text/javascript"]),
                    payload: vec![b'j'; 5000],
                    external: true,
                    bucket: 3,
                },
                Spec {
                    key: long_key,
                    created: 13_000_000_002_000_000,
                    headers: Vec::new(),
                    payload: Vec::new(),
                    external: false,
                    bucket: 5,
                },
            ],
            8,
        ))
    }

    #[test]
    fn reads_entries_and_chains() {
        let files = sample();
        let mut cache = Cache::open(&files).unwrap();
        assert_eq!(cache.table_len(), 8);
        let mut out = Vec::new();
        cache
            .entries(|e| {
                out.push(e);
                true
            })
            .unwrap();
        assert_eq!(out.len(), 3);
        let a = &out[0];
        assert_eq!(a.key, "https://www.example.com/logo.png");
        assert_eq!(a.created, 1_355_526_400);
        assert_eq!(a.response, "HTTP/1.1 200 OK");
        assert_eq!(a.header("content-type"), Some(" image/png"));
        assert!(a.is_brotli());
        assert_eq!(
            a.headers_text(),
            "content-type :  image/png \ncontent-encoding :  br"
        );
        let Some(Payload::Block { file, len, .. }) = &a.payload else {
            panic!("{a:?}")
        };
        assert_eq!((file.as_str(), *len), ("data_2", 10));
        assert!(
            a.payload
                .as_ref()
                .unwrap()
                .derived_name()
                .unwrap()
                .starts_with("data_2__")
        );
        // Second entry reached through the collision chain.
        let b = &out[1];
        assert_eq!(
            b.payload,
            Some(Payload::External {
                file: "f_000001".into(),
                len: 5000
            })
        );
        assert!(!b.is_brotli());
        // A key spanning two blocks is read whole.
        assert_eq!(out[2].key.len(), 320);
        assert_eq!(out[2].payload, None);
    }

    #[test]
    fn missing_files_and_bad_magic() {
        let mut f = sample();
        f.0.remove("data_3");
        assert!(matches!(Cache::open(&f), Err(Error::Missing(n)) if n == "data_3"));
        let mut f = sample();
        f.0.get_mut("index").unwrap()[0] = 0;
        assert!(matches!(Cache::open(&f), Err(Error::NotIndex)));
    }

    #[test]
    fn header_parsing() {
        let (r, h) = parse_headers(b"junkHTTP/1.0 404\0A:1\0bad\0a:2\0\0ignored:3\0");
        assert_eq!(r, "HTTP/1.0 404");
        assert_eq!(h, vec![("a".to_owned(), "2".to_owned())]);
        assert_eq!(parse_headers(b"nothing"), (String::new(), Vec::new()));
        let (r, _) = parse_headers(b"HTTP");
        assert_eq!(r, "HTTP");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(i in 0usize..5, pos in 0usize..20_000, bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..64)) {
            let mut f = sample();
            let name = ["index", "data_0", "data_1", "data_2", "f_000001"][i];
            let d = f.0.get_mut(name).unwrap();
            let n = d.len().max(1);
            for (k, b) in bytes.iter().enumerate() {
                if let Some(x) = d.get_mut((pos + k) % n) {
                    *x = *b;
                }
            }
            if let Ok(mut c) = Cache::open(&f) {
                let _ = c.entries(|_| true);
            }
        }
    }
}
