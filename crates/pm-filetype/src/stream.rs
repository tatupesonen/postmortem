//! Sequential, buffered reading over a positional source (what Tika's
//! detectors see as an `InputStream`).

use pm_core::ReadAt;

const CHUNK: usize = 64 * 1024;

/// A forward reader over `src`, optionally bounded.
pub(crate) struct SeqReader<'a> {
    src: &'a dyn ReadAt,
    /// Position of the next byte.
    pos: u64,
    /// End (exclusive) of what may be read.
    limit: u64,
    buf: Vec<u8>,
    buf_start: u64,
}

impl std::fmt::Debug for SeqReader<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SeqReader")
            .field("pos", &self.pos)
            .finish_non_exhaustive()
    }
}

impl<'a> SeqReader<'a> {
    pub(crate) fn new(src: &'a dyn ReadAt) -> Self {
        Self::bounded(src, 0, u64::MAX)
    }

    /// Reads `src` from `start`, at most `limit` bytes.
    pub(crate) fn bounded(src: &'a dyn ReadAt, start: u64, limit: u64) -> Self {
        Self {
            src,
            pos: start,
            limit: start.saturating_add(limit).min(src.size()),
            buf: Vec::new(),
            buf_start: start,
        }
    }

    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    /// Moves to an absolute position (`reset` to a mark).
    pub(crate) fn seek(&mut self, pos: u64) {
        self.pos = pos;
    }

    fn fill(&mut self) -> bool {
        if self.pos >= self.limit {
            return false;
        }
        let in_buf =
            self.pos >= self.buf_start && self.pos < self.buf_start + self.buf.len() as u64;
        if in_buf {
            return true;
        }
        let want = usize::try_from((self.limit - self.pos).min(CHUNK as u64)).unwrap_or(CHUNK);
        self.buf.resize(want, 0);
        let n = self.src.read_at(self.pos, &mut self.buf).unwrap_or(0);
        self.buf.truncate(n);
        self.buf_start = self.pos;
        n > 0
    }

    /// The next byte, or `None` at the end.
    pub(crate) fn byte(&mut self) -> Option<u8> {
        if !self.fill() {
            return None;
        }
        let i = usize::try_from(self.pos - self.buf_start).ok()?;
        let b = self.buf.get(i).copied()?;
        self.pos += 1;
        Some(b)
    }

    /// Reads up to `out.len()` bytes; returns how many were read.
    pub(crate) fn read(&mut self, out: &mut [u8]) -> usize {
        let mut done = 0;
        while done < out.len() {
            if !self.fill() {
                break;
            }
            let i = usize::try_from(self.pos - self.buf_start).unwrap_or(usize::MAX);
            let avail = self.buf.get(i..).unwrap_or_default();
            let n = avail.len().min(out.len() - done);
            if n == 0 {
                break;
            }
            if let (Some(dst), Some(s)) = (out.get_mut(done..done + n), avail.get(..n)) {
                dst.copy_from_slice(s);
            }
            done += n;
            self.pos += n as u64;
        }
        done
    }

    /// Fills `out` completely or returns `false` at the end.
    pub(crate) fn read_exact(&mut self, out: &mut [u8]) -> bool {
        self.read(out) == out.len()
    }

    /// Reads `n` bytes into a vector (fewer at the end).
    pub(crate) fn take(&mut self, n: usize) -> Vec<u8> {
        let mut v = vec![0; n];
        let got = self.read(&mut v);
        v.truncate(got);
        v
    }

    /// Skips `n` bytes; `false` when the end comes first.
    pub(crate) fn skip(&mut self, n: u64) -> bool {
        match self.pos.checked_add(n) {
            Some(p) if p <= self.limit => {
                self.pos = p;
                true
            }
            _ => {
                self.pos = self.limit;
                false
            }
        }
    }
}
