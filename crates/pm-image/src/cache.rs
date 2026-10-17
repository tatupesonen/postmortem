//! Bounded caches shared by the virtual-disk readers.
//!
//! Virtual disk formats keep their allocation tables (VHD BAT, VMDK grain
//! directory and tables, QCOW2 L1/L2, VHDX BAT and sector bitmaps) on disk.
//! For a multi-terabyte image those tables are many megabytes, so they are
//! never loaded whole: [`MetaPages`] reads fixed-size pages on demand and keeps
//! a bounded, least-recently-used set of them. Memory therefore depends on the
//! cache size, not on the image size (ADR-014).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::io;
use std::sync::Arc;

use parking_lot::Mutex;
use pm_core::{ReadAt, SharedSource};

/// Number of independently locked shards (reduces contention under rayon).
const SHARDS: usize = 8;

#[derive(Debug)]
struct Shard<K, V> {
    map: HashMap<K, (u64, V)>,
    tick: u64,
}

/// A sharded, bounded least-recently-used map.
#[derive(Debug)]
pub(crate) struct Lru<K, V> {
    shards: Vec<Mutex<Shard<K, V>>>,
    per_shard: usize,
    hasher: std::hash::RandomState,
}

impl<K: Eq + Hash + Copy, V: Clone> Lru<K, V> {
    /// A cache holding at most about `capacity` entries (at least one per
    /// shard).
    pub(crate) fn new(capacity: usize) -> Self {
        let per_shard = capacity.div_ceil(SHARDS).max(1);
        Self {
            shards: (0..SHARDS)
                .map(|_| {
                    Mutex::new(Shard {
                        map: HashMap::with_capacity(per_shard),
                        tick: 0,
                    })
                })
                .collect(),
            per_shard,
            hasher: std::hash::RandomState::new(),
        }
    }

    fn shard(&self, k: &K) -> Option<&Mutex<Shard<K, V>>> {
        let h = self.hasher.hash_one(k) as usize;
        self.shards.get(h % SHARDS)
    }

    /// Returns a cached value, marking it recently used.
    pub(crate) fn get(&self, k: &K) -> Option<V> {
        let mut s = self.shard(k)?.lock();
        s.tick += 1;
        let tick = s.tick;
        let entry = s.map.get_mut(k)?;
        entry.0 = tick;
        Some(entry.1.clone())
    }

    /// Inserts a value, evicting the least recently used entry when full.
    pub(crate) fn insert(&self, k: K, v: V) {
        let Some(shard) = self.shard(&k) else { return };
        let mut s = shard.lock();
        s.tick += 1;
        let tick = s.tick;
        if s.map.len() >= self.per_shard && !s.map.contains_key(&k) {
            let oldest = s.map.iter().min_by_key(|(_, (t, _))| *t).map(|(k, _)| *k);
            if let Some(old) = oldest {
                s.map.remove(&old);
            }
        }
        s.map.insert(k, (tick, v));
    }

    /// Number of cached entries (for tests).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().map.len()).sum()
    }
}

/// Page size of cached metadata.
pub(crate) const PAGE: u64 = 4096;
/// Default number of cached metadata pages per image (4 MiB).
pub(crate) const DEFAULT_PAGES: usize = 1024;

/// On-demand, page-cached access to small metadata fields of a set of files
/// (one per extent of a multi-file image).
#[derive(Debug)]
pub(crate) struct MetaPages {
    files: Vec<SharedSource>,
    cache: Lru<(usize, u64), Arc<[u8]>>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl MetaPages {
    pub(crate) fn new(files: Vec<SharedSource>, pages: usize) -> Self {
        Self {
            files,
            cache: Lru::new(pages),
        }
    }

    /// The underlying file `idx`.
    pub(crate) fn file(&self, idx: usize) -> io::Result<&SharedSource> {
        self.files
            .get(idx)
            .ok_or_else(|| invalid(format!("no image file #{idx}")))
    }

    fn page(&self, file: usize, page: u64) -> io::Result<Arc<[u8]>> {
        if let Some(p) = self.cache.get(&(file, page)) {
            return Ok(p);
        }
        let src = self.file(file)?;
        let start = page.saturating_mul(PAGE);
        let len = src.size().saturating_sub(start).min(PAGE) as usize;
        let mut buf = vec![0u8; len];
        src.read_exact_at(start, &mut buf)?;
        let p: Arc<[u8]> = buf.into();
        self.cache.insert((file, page), Arc::clone(&p));
        Ok(p)
    }

    /// Fills `buf` from `offset` of file `file`, through the page cache.
    pub(crate) fn read(&self, file: usize, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset
                .checked_add(done as u64)
                .ok_or_else(|| invalid("metadata offset overflows".into()))?;
            let page = self.page(file, pos / PAGE)?;
            let within = (pos % PAGE) as usize;
            let avail = page.get(within..).unwrap_or_default();
            let dst = buf.get_mut(done..).unwrap_or_default();
            let n = avail.len().min(dst.len());
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("metadata at offset {pos} is beyond the end of the image file"),
                ));
            }
            if let (Some(d), Some(s)) = (dst.get_mut(..n), avail.get(..n)) {
                d.copy_from_slice(s);
            }
            done += n;
        }
        Ok(())
    }

    fn array<const N: usize>(&self, file: usize, offset: u64) -> io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.read(file, offset, &mut b)?;
        Ok(b)
    }

    pub(crate) fn u8(&self, file: usize, offset: u64) -> io::Result<u8> {
        Ok(self.array::<1>(file, offset)?[0])
    }
    pub(crate) fn u32_le(&self, file: usize, offset: u64) -> io::Result<u32> {
        self.array(file, offset).map(u32::from_le_bytes)
    }
    pub(crate) fn u32_be(&self, file: usize, offset: u64) -> io::Result<u32> {
        self.array(file, offset).map(u32::from_be_bytes)
    }
    pub(crate) fn u64_le(&self, file: usize, offset: u64) -> io::Result<u64> {
        self.array(file, offset).map(u64::from_le_bytes)
    }
    pub(crate) fn u64_be(&self, file: usize, offset: u64) -> io::Result<u64> {
        self.array(file, offset).map(u64::from_be_bytes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test code")]
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn lru_is_bounded_and_keeps_recent_entries() {
        let c: Lru<u64, u64> = Lru::new(16);
        for i in 0..1000 {
            c.insert(i, i * 2);
        }
        assert!(c.len() <= 16);
        assert_eq!(c.get(&999), Some(1998));
        assert_eq!(c.get(&0), None);
    }

    #[test]
    fn meta_pages_cross_page_boundaries() {
        let data: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let m = MetaPages::new(vec![Arc::new(MemSource::new(data))], 2);
        assert_eq!(
            m.u32_le(0, PAGE - 2).unwrap(),
            u32::from_le_bytes([0xFE, 0xFF, 0x00, 0x01])
        );
        assert_eq!(m.u8(0, 9_999).unwrap(), (9_999u32 % 256) as u8);
        assert!(m.u32_le(0, 9_998).is_err());
        assert!(m.u8(1, 0).is_err());
        assert!(m.u8(0, u64::MAX).is_err());
    }
}
