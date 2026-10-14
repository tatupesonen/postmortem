//! Concatenation of several byte sources into one logical stream.

use std::io;

use pm_core::ReadAt;

/// Presents segments (e.g. `image.001`, `image.002`, …) as one contiguous
/// source. Lookup is `O(log n)` in the number of segments.
#[derive(Debug)]
pub struct ConcatSource<S> {
    segments: Vec<S>,
    /// Start offset of each segment; `starts[i+1] - starts[i]` = segment size.
    starts: Vec<u64>,
    size: u64,
}

impl<S: ReadAt> ConcatSource<S> {
    /// Concatenates `segments` in order. Empty segments are kept (they occupy
    /// zero bytes) so segment indices remain meaningful for error messages.
    pub fn new(segments: Vec<S>) -> io::Result<Self> {
        let mut starts = Vec::with_capacity(segments.len());
        let mut size = 0u64;
        for s in &segments {
            starts.push(size);
            size = size.checked_add(s.size()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "image size overflows u64")
            })?;
        }
        Ok(Self {
            segments,
            starts,
            size,
        })
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Index of the segment holding `offset` (callers ensure `offset < size`).
    fn segment_for(&self, offset: u64) -> usize {
        // Last segment whose start <= offset, skipping empty segments because
        // partition_point finds the first start > offset.
        self.starts
            .partition_point(|&s| s <= offset)
            .saturating_sub(1)
    }
}

impl<S: ReadAt> ReadAt for ConcatSource<S> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let mut idx = self.segment_for(offset);
        let mut done = 0usize;
        let mut pos = offset;
        while done < buf.len() && pos < self.size {
            let (Some(seg), Some(&start)) = (self.segments.get(idx), self.starts.get(idx)) else {
                break;
            };
            let seg_end = start + seg.size();
            if pos >= seg_end {
                idx += 1;
                continue;
            }
            let Some(dst) = buf.get_mut(done..) else {
                break;
            };
            let want = (seg_end - pos).min(dst.len() as u64) as usize;
            let Some(dst) = dst.get_mut(..want) else {
                break;
            };
            let n = seg.read_at(pos - start, dst)?;
            if n == 0 {
                // Segment shorter than when opened (truncated evidence file):
                // report a short read rather than skipping bytes.
                break;
            }
            done += n;
            pos += n as u64;
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

    fn concat(parts: &[&[u8]]) -> ConcatSource<MemSource> {
        ConcatSource::new(parts.iter().map(|p| MemSource::new(p.to_vec())).collect()).unwrap()
    }

    #[test]
    fn reads_across_segment_boundaries() {
        let c = concat(&[b"abc", b"", b"defg", b"h"]);
        assert_eq!(c.size(), 8);
        assert_eq!(c.read_vec_at(0, 8).unwrap(), b"abcdefgh");
        assert_eq!(c.read_vec_at(2, 5).unwrap(), b"cdefg");
        assert_eq!(c.read_vec_at(7, 1).unwrap(), b"h");
        let mut buf = [0u8; 4];
        assert_eq!(c.read_at(8, &mut buf).unwrap(), 0);
    }

    #[test]
    fn empty_concat() {
        let c = concat(&[]);
        assert_eq!(c.size(), 0);
        assert_eq!(c.read_at(0, &mut [0u8; 1]).unwrap(), 0);
    }

    proptest! {
        #[test]
        fn matches_flat_buffer(
            parts in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..40), 0..8),
            off in 0u64..400, n in 0usize..100,
        ) {
            let flat: Vec<u8> = parts.concat();
            let c = ConcatSource::new(parts.into_iter().map(MemSource::new).collect()).unwrap();
            let mut buf = vec![0u8; n];
            let got = c.read_at(off, &mut buf).unwrap();
            let expect = flat.get(off as usize..).map_or(&[][..], |s| &s[..s.len().min(n)]);
            prop_assert_eq!(&buf[..got], expect);
        }
    }
}
