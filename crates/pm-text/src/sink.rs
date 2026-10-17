//! Where extracted text goes.

/// Returned by a sink to end the extraction early (cancellation, limits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop;

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("extraction stopped")
    }
}

impl std::error::Error for Stop {}

/// Receives extracted text piece by piece, in document order.
pub trait TextSink {
    /// Takes the next piece of text; `Err(Stop)` ends the extraction.
    fn push(&mut self, text: &str) -> Result<(), Stop>;
}

impl<F: FnMut(&str) -> Result<(), Stop>> TextSink for F {
    fn push(&mut self, text: &str) -> Result<(), Stop> {
        self(text)
    }
}

/// Collects text into a string, stopping once `limit` bytes are reached
/// (`0` = unlimited).
#[derive(Debug, Default, Clone)]
pub struct StringSink {
    pub text: String,
    pub limit: usize,
}

impl StringSink {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            text: String::new(),
            limit,
        }
    }
}

impl TextSink for StringSink {
    fn push(&mut self, text: &str) -> Result<(), Stop> {
        if self.limit == 0 {
            self.text.push_str(text);
            return Ok(());
        }
        let room = self.limit.saturating_sub(self.text.len());
        if text.len() <= room {
            self.text.push_str(text);
            if self.text.len() >= self.limit {
                return Err(Stop);
            }
            return Ok(());
        }
        let mut cut = room;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        self.text.push_str(text.get(..cut).unwrap_or_default());
        Err(Stop)
    }
}

/// Forwards text to another sink, stopping after `max_bytes` (`0` =
/// unlimited) or when `cancelled` returns true; counts what passed.
pub struct LimitSink<'a> {
    inner: &'a mut dyn TextSink,
    cancelled: &'a dyn Fn() -> bool,
    max_bytes: u64,
    /// Bytes forwarded so far.
    pub bytes: u64,
    /// Non-whitespace characters forwarded so far.
    pub non_space: u64,
}

impl std::fmt::Debug for LimitSink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LimitSink")
            .field("max_bytes", &self.max_bytes)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl<'a> LimitSink<'a> {
    pub fn new(
        inner: &'a mut dyn TextSink,
        cancelled: &'a dyn Fn() -> bool,
        max_bytes: u64,
    ) -> Self {
        Self {
            inner,
            cancelled,
            max_bytes,
            bytes: 0,
            non_space: 0,
        }
    }
}

impl TextSink for LimitSink<'_> {
    fn push(&mut self, text: &str) -> Result<(), Stop> {
        if (self.cancelled)() {
            return Err(Stop);
        }
        let mut text = text;
        let mut stop = false;
        if self.max_bytes > 0 {
            let room = self.max_bytes.saturating_sub(self.bytes);
            if (text.len() as u64) > room {
                let mut cut = usize::try_from(room).unwrap_or(usize::MAX).min(text.len());
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text = text.get(..cut).unwrap_or_default();
                stop = true;
            }
        }
        self.bytes += text.len() as u64;
        self.non_space += text.chars().filter(|c| !c.is_whitespace()).count() as u64;
        self.inner.push(text)?;
        if stop { Err(Stop) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_sink_limit_cuts_on_char_boundary() {
        let mut s = StringSink::with_limit(4);
        assert_eq!(s.push("ab"), Ok(()));
        assert_eq!(s.push("cé"), Err(Stop));
        assert_eq!(s.text, "abc");
    }

    #[test]
    fn limit_sink_counts_and_cancels() {
        let mut out = StringSink::default();
        let cancel = || false;
        let mut l = LimitSink::new(&mut out, &cancel, 5);
        assert!(l.push("a b").is_ok());
        assert_eq!(l.push("cdef"), Err(Stop));
        assert_eq!(l.bytes, 5);
        assert_eq!(l.non_space, 4);
        assert_eq!(out.text, "a bcd");

        let mut out = StringSink::default();
        let cancel = || true;
        let mut l = LimitSink::new(&mut out, &cancel, 0);
        assert_eq!(l.push("x"), Err(Stop));
    }
}
