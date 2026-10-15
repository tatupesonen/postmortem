//! The hive's bytes: the primary file plus sectors patched in from
//! transaction logs.

use std::collections::BTreeMap;
use std::io;

use pm_core::SharedSource;

use crate::base_block::BASE_BLOCK_SIZE;

/// Granularity of log patches (old-format logs dirty 512-byte sectors;
/// new-format pages are multiples of it).
pub(crate) const SECTOR: usize = 512;

/// Positional reads over the primary file with log patches applied.
#[derive(Debug)]
pub(crate) struct HiveData {
    src: SharedSource,
    /// The (possibly replaced) base block.
    base: Vec<u8>,
    /// Sector index (of the hive file) → patched bytes.
    overlay: BTreeMap<u64, Box<[u8]>>,
    /// Logical file length (grows when a log extends the hive).
    len: u64,
}

impl HiveData {
    pub(crate) fn new(src: SharedSource, base: Vec<u8>) -> Self {
        let len = src.size();
        Self {
            src,
            base,
            overlay: BTreeMap::new(),
            len,
        }
    }

    pub(crate) fn base(&self) -> &[u8] {
        &self.base
    }

    pub(crate) fn set_base(&mut self, base: Vec<u8>) {
        self.base = base;
    }

    /// Logical length in bytes.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Ensures the logical length covers `len` bytes.
    pub(crate) fn grow_to(&mut self, len: u64) {
        self.len = self.len.max(len);
    }

    /// Number of patched sectors.
    #[cfg(test)]
    pub(crate) fn patched_sectors(&self) -> usize {
        self.overlay.len()
    }

    /// Writes `data` at hive-file offset `offset` (sector aligned, a
    /// multiple of the sector size) into the overlay.
    pub(crate) fn patch(&mut self, offset: u64, data: &[u8]) {
        let first = offset / SECTOR as u64;
        for (i, chunk) in data.chunks(SECTOR).enumerate() {
            let mut sector = vec![0u8; SECTOR];
            if let Some(dst) = sector.get_mut(..chunk.len()) {
                dst.copy_from_slice(chunk);
            }
            self.overlay
                .insert(first + i as u64, sector.into_boxed_slice());
        }
        self.grow_to(offset + data.len() as u64);
    }

    /// Reads `len` bytes at hive-file offset `offset`.
    pub(crate) fn read(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let end = offset
            .checked_add(len as u64)
            .filter(|&e| e <= self.len)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("hive read of {len} bytes at {offset:#x} past end"),
                )
            })?;
        let mut buf = vec![0u8; len];
        // Primary file (short reads past its end leave zeros).
        let mut done = 0usize;
        while done < len {
            let Some(dst) = buf.get_mut(done..) else {
                break;
            };
            match self.src.read_at(offset + done as u64, dst) {
                Ok(0) => break,
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        // Log patches.
        let first = offset / SECTOR as u64;
        let last = (end - 1) / SECTOR as u64;
        for (&idx, sector) in self.overlay.range(first..=last) {
            copy_overlap(&mut buf, offset, idx * SECTOR as u64, sector);
        }
        // Base block copy.
        if offset < BASE_BLOCK_SIZE as u64 {
            copy_overlap(&mut buf, offset, 0, &self.base);
        }
        Ok(buf)
    }
}

/// Copies the part of `src` (located at `src_off`) that overlaps the
/// buffer located at `buf_off`.
fn copy_overlap(buf: &mut [u8], buf_off: u64, src_off: u64, src: &[u8]) {
    let start = buf_off.max(src_off);
    let end = (buf_off + buf.len() as u64).min(src_off + src.len() as u64);
    if start >= end {
        return;
    }
    let n = (end - start) as usize;
    let d = (start - buf_off) as usize;
    let s = (start - src_off) as usize;
    if let (Some(dst), Some(from)) = (buf.get_mut(d..d + n), src.get(s..s + n)) {
        dst.copy_from_slice(from);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pm_core::io::MemSource;

    use super::*;

    #[test]
    fn overlay_and_base_win_over_file() {
        let file: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
        let mut d = HiveData::new(Arc::new(MemSource::new(file.clone())), vec![0xAA; 4096]);
        assert_eq!(d.read(10, 4).unwrap(), vec![0xAA; 4]);
        assert_eq!(d.read(5000, 2).unwrap(), file[5000..5002].to_vec());
        d.patch(4608, &[7u8; 512]);
        let r = d.read(4600, 16).unwrap();
        assert_eq!(&r[..8], &file[4600..4608]);
        assert_eq!(&r[8..], &[7u8; 8]);
        // Growing past the file end.
        d.patch(8192, &[9u8; 1024]);
        assert_eq!(d.len(), 9216);
        assert_eq!(d.read(9000, 8).unwrap(), vec![9u8; 8]);
        assert!(d.read(9210, 8).is_err());
        assert_eq!(d.patched_sectors(), 3);
    }
}
