//! Byte source assembled from a stream's extents.

use std::io;

use pm_core::{ReadAt, SharedSource};

use crate::{Extent, ExtentKind};

/// Reads a file whose content is scattered over filesystem extents.
///
/// Regions not covered by any `Allocated` extent (sparse holes, or a chain
/// that ends before the recorded size) read as zeros, so the source always
/// has exactly the file's logical size.
#[derive(Debug, Clone)]
pub struct ExtentSource {
    fs: SharedSource,
    /// Allocated extents sorted by `logical_offset`, non-overlapping.
    extents: Vec<Extent>,
    size: u64,
}

impl ExtentSource {
    /// `fs` is the filesystem's byte source; extent physical offsets are
    /// relative to it. Overlapping extents are trimmed (first wins).
    pub fn new(fs: SharedSource, extents: impl IntoIterator<Item = Extent>, size: u64) -> Self {
        let mut ext: Vec<Extent> = extents
            .into_iter()
            .filter(|e| e.kind == ExtentKind::Allocated && e.len > 0)
            .collect();
        ext.sort_by_key(|e| e.logical_offset);
        let mut out: Vec<Extent> = Vec::with_capacity(ext.len());
        for mut e in ext {
            if let Some(prev) = out.last() {
                let prev_end = prev.logical_offset.saturating_add(prev.len);
                if e.logical_offset < prev_end {
                    let cut = prev_end - e.logical_offset;
                    if cut >= e.len {
                        continue;
                    }
                    e.logical_offset += cut;
                    e.physical_offset = e.physical_offset.saturating_add(cut);
                    e.len -= cut;
                }
            }
            out.push(e);
        }
        Self {
            fs,
            extents: out,
            size,
        }
    }

    pub fn extents(&self) -> &[Extent] {
        &self.extents
    }
}

impl ReadAt for ExtentSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (self.size - offset).min(buf.len() as u64) as usize;
        let mut done = 0usize;
        // First extent that ends after `offset`.
        let mut idx = self
            .extents
            .partition_point(|e| e.logical_offset + e.len <= offset);
        while done < want {
            let pos = offset + done as u64;
            let Some(dst) = buf.get_mut(done..want) else {
                break;
            };
            match self.extents.get(idx) {
                Some(e) if e.logical_offset <= pos => {
                    let within = pos - e.logical_offset;
                    let n = (e.len - within).min(dst.len() as u64) as usize;
                    let Some(dst) = dst.get_mut(..n) else { break };
                    self.fs.read_exact_at(e.physical_offset + within, dst)?;
                    done += n;
                    idx += 1;
                }
                next => {
                    // Hole until the next extent (or end of file): zeros.
                    let hole_end = next.map_or(self.size, |e| e.logical_offset);
                    let n = (hole_end - pos).min(dst.len() as u64) as usize;
                    let Some(dst) = dst.get_mut(..n) else { break };
                    dst.fill(0);
                    done += n;
                }
            }
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;
    use proptest::prelude::*;
    use std::sync::Arc;

    fn ext(l: u64, p: u64, len: u64) -> Extent {
        Extent {
            logical_offset: l,
            physical_offset: p,
            len,
            kind: ExtentKind::Allocated,
        }
    }

    fn fs() -> SharedSource {
        Arc::new(MemSource::new((0..=255u8).collect::<Vec<_>>()))
    }

    #[test]
    fn maps_scattered_extents_and_zero_fills_holes() {
        let s = ExtentSource::new(fs(), [ext(4, 100, 2), ext(0, 10, 3)], 8);
        assert_eq!(
            s.read_vec_at(0, 8).unwrap(),
            vec![10, 11, 12, 0, 100, 101, 0, 0]
        );
        assert_eq!(s.read_vec_at(5, 2).unwrap(), vec![101, 0]);
    }

    #[test]
    fn overlapping_extents_first_wins() {
        let s = ExtentSource::new(fs(), [ext(0, 10, 4), ext(2, 50, 4)], 6);
        assert_eq!(s.read_vec_at(0, 6).unwrap(), vec![10, 11, 12, 13, 52, 53]);
        assert_eq!(s.extents().len(), 2);
    }

    #[test]
    fn ignores_sparse_and_resident_extents() {
        let mut sp = ext(0, 0, 4);
        sp.kind = ExtentKind::Sparse;
        let s = ExtentSource::new(fs(), [sp], 4);
        assert_eq!(s.read_vec_at(0, 4).unwrap(), vec![0; 4]);
    }

    proptest! {
        #[test]
        fn reads_match_reference(
            raw in proptest::collection::vec((0u64..64, 0u64..200, 0u64..20), 0..6),
            size in 0u64..80, off in 0u64..90, n in 0usize..40,
        ) {
            let s = ExtentSource::new(fs(), raw.iter().map(|&(l, p, len)| ext(l, p, len)), size);
            // Reference: byte-by-byte resolution with first-wins semantics.
            let mut sorted: Vec<_> = raw.clone();
            sorted.sort_by_key(|r| r.0);
            let byte_at = |pos: u64| -> u8 {
                for &(l, p, len) in &sorted {
                    if len > 0 && pos >= l && pos < l + len {
                        let phys = p + (pos - l);
                        return if phys < 256 { phys as u8 } else { 0 };
                    }
                }
                0
            };
            let mut buf = vec![0u8; n];
            match s.read_at(off, &mut buf) {
                Ok(got) => {
                    let expect: Vec<u8> = (off..size.max(off)).take(n).map(byte_at).collect();
                    prop_assert_eq!(&buf[..got], &expect[..]);
                }
                // Physical offsets past the 256-byte backing store fail; fine.
                Err(e) => prop_assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            }
        }
    }
}
