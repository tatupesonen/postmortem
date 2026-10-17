//! Cached random access for the length finders.

use pm_core::ReadAt;

/// Bytes read per cache refill.
const PAGE: usize = 256 * 1024;

/// A read-through cache of one page over a [`ReadAt`], bounded by `end`.
///
/// Read errors and reads past `end` look like the end of the data: the
/// finders treat a file that runs off the end as truncated.
pub(crate) struct Window<'a> {
    src: &'a dyn ReadAt,
    end: u64,
    start: u64,
    buf: Vec<u8>,
}

impl<'a> Window<'a> {
    pub(crate) fn new(src: &'a dyn ReadAt, end: u64) -> Self {
        Self {
            src,
            end: end.min(src.size()),
            start: 0,
            buf: Vec::new(),
        }
    }

    /// Readable bytes end here.
    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    fn fill(&mut self, pos: u64, len: usize) -> bool {
        let want = len.max(PAGE);
        let avail = self.end.saturating_sub(pos);
        let n = usize::try_from(avail).map_or(want, |a| a.min(want));
        if n < len {
            return false;
        }
        self.buf.resize(n, 0);
        let mut got = 0;
        while got < n {
            let Some(rest) = self.buf.get_mut(got..) else {
                break;
            };
            match self.src.read_at(pos + got as u64, rest) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        self.buf.truncate(got);
        self.start = pos;
        got >= len
    }

    /// Exactly `len` bytes at `pos`, or `None` past the end.
    pub(crate) fn get(&mut self, pos: u64, len: usize) -> Option<&[u8]> {
        let end = pos.checked_add(len as u64)?;
        if end > self.end {
            return None;
        }
        let cached = pos >= self.start && end <= self.start + self.buf.len() as u64;
        if !cached && !self.fill(pos, len) {
            return None;
        }
        let off = usize::try_from(pos - self.start).ok()?;
        self.buf.get(off..off + len)
    }

    /// The bytes from `pos` to the end of the cached page (at least one
    /// byte, unless `pos` is at the end).
    pub(crate) fn chunk(&mut self, pos: u64) -> Option<&[u8]> {
        if pos >= self.end {
            return None;
        }
        let cached = pos >= self.start && pos < self.start + self.buf.len() as u64;
        if !cached && !self.fill(pos, 1) {
            return None;
        }
        let off = usize::try_from(pos - self.start).ok()?;
        self.buf.get(off..)
    }

    /// `N` bytes at `pos` as an array.
    pub(crate) fn array<const N: usize>(&mut self, pos: u64) -> Option<[u8; N]> {
        self.get(pos, N).and_then(|b| b.try_into().ok())
    }

    pub(crate) fn u8(&mut self, pos: u64) -> Option<u8> {
        self.get(pos, 1).and_then(|b| b.first().copied())
    }

    pub(crate) fn u16be(&mut self, pos: u64) -> Option<u16> {
        self.array(pos).map(u16::from_be_bytes)
    }

    pub(crate) fn u16le(&mut self, pos: u64) -> Option<u16> {
        self.array(pos).map(u16::from_le_bytes)
    }

    pub(crate) fn u32be(&mut self, pos: u64) -> Option<u32> {
        self.array(pos).map(u32::from_be_bytes)
    }

    pub(crate) fn u32le(&mut self, pos: u64) -> Option<u32> {
        self.array(pos).map(u32::from_le_bytes)
    }

    pub(crate) fn u64be(&mut self, pos: u64) -> Option<u64> {
        self.array(pos).map(u64::from_be_bytes)
    }

    pub(crate) fn u64le(&mut self, pos: u64) -> Option<u64> {
        self.array(pos).map(u64::from_le_bytes)
    }

    /// The first occurrence of `needle` at or after `from` (before the
    /// end).
    pub(crate) fn find(&mut self, from: u64, needle: &[u8]) -> Option<u64> {
        let finder = memchr::memmem::Finder::new(needle);
        let mut pos = from;
        loop {
            let overlap = needle.len().saturating_sub(1) as u64;
            let chunk = self.chunk(pos)?;
            if let Some(i) = finder.find(chunk) {
                return Some(pos + i as u64);
            }
            if (chunk.len() as u64) <= overlap {
                // Too little left for a match.
                if pos + chunk.len() as u64 >= self.end {
                    return None;
                }
                // Force a refill that holds the straddling bytes.
                let need = needle.len().min(usize::try_from(self.end - pos).ok()?);
                let b = self.get(pos, need)?;
                if b == needle {
                    return Some(pos);
                }
                pos += 1;
                continue;
            }
            pos += chunk.len() as u64 - overlap;
        }
    }

    /// The first byte equal to `b` at or after `from`.
    pub(crate) fn find_byte(&mut self, from: u64, b: u8) -> Option<u64> {
        let mut pos = from;
        loop {
            let chunk = self.chunk(pos)?;
            if let Some(i) = memchr::memchr(b, chunk) {
                return Some(pos + i as u64);
            }
            pos += chunk.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn reads_and_finds_across_pages() {
        let mut data = vec![0u8; PAGE * 2 + 100];
        let at = PAGE as u64 - 2;
        data[at as usize..at as usize + 5].copy_from_slice(b"%%EOF");
        data[PAGE * 2 + 50] = 0xFF;
        let src = MemSource::new(data);
        let mut w = Window::new(&src, u64::MAX);
        assert_eq!(w.find(0, b"%%EOF"), Some(at));
        assert_eq!(w.find(at + 1, b"%%EOF"), None);
        assert_eq!(w.find_byte(0, 0xFF), Some(PAGE as u64 * 2 + 50));
        assert_eq!(w.get(PAGE as u64 * 2 + 99, 2), None);
        assert_eq!(w.u8(PAGE as u64 * 2 + 50), Some(0xFF));
        let mut w = Window::new(&src, 10);
        assert_eq!(w.get(8, 4), None);
        assert_eq!(w.find_byte(0, 0xFF), None);
    }
}
