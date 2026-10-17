//! Small helpers shared by the extractors.

use std::io::{self, BufRead, Read};

use pm_core::ReadAt;

use crate::TextError;

/// Reads the whole source if it is at most `limit` bytes.
pub(crate) fn read_all(src: &dyn ReadAt, limit: u64) -> Result<Vec<u8>, TextError> {
    let size = src.size();
    if size > limit {
        return Err(TextError::TooLarge { size, limit });
    }
    let len = usize::try_from(size).map_err(|_| TextError::TooLarge { size, limit })?;
    let mut buf = vec![0u8; len];
    let mut off = 0usize;
    while off < len {
        let n = src.read_at(off as u64, buf.get_mut(off..).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        off += n;
    }
    buf.truncate(off);
    Ok(buf)
}

/// A borrowed source as an owned `ReadAt` (for `pm_core::io::Cursor`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Borrowed<'a>(pub(crate) &'a dyn ReadAt);

impl ReadAt for Borrowed<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read_at(offset, buf)
    }

    fn size(&self) -> u64 {
        self.0.size()
    }
}

/// `io::Read + io::Seek` over a borrowed source.
pub(crate) fn cursor(src: &dyn ReadAt) -> pm_core::io::Cursor<Borrowed<'_>> {
    pm_core::io::Cursor::new(Borrowed(src))
}

/// Sequential reader over a positional source (`io::Read` + `BufRead`).
pub(crate) struct SrcReader<'a> {
    src: &'a dyn ReadAt,
    pos: u64,
    end: u64,
    buf: Vec<u8>,
    start: usize,
    filled: usize,
}

impl<'a> SrcReader<'a> {
    pub(crate) fn new(src: &'a dyn ReadAt) -> Self {
        Self::with_capacity(src, 64 * 1024)
    }

    pub(crate) fn with_capacity(src: &'a dyn ReadAt, cap: usize) -> Self {
        Self {
            src,
            pos: 0,
            end: src.size(),
            buf: vec![0; cap.max(16)],
            start: 0,
            filled: 0,
        }
    }
}

impl Read for SrcReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let avail = self.fill_buf()?;
        let n = avail.len().min(out.len());
        if let (Some(dst), Some(src)) = (out.get_mut(..n), avail.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.consume(n);
        Ok(n)
    }
}

impl BufRead for SrcReader<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.start >= self.filled && self.pos < self.end {
            let want =
                usize::try_from((self.end - self.pos).min(self.buf.len() as u64)).unwrap_or(0);
            let n = self
                .src
                .read_at(self.pos, self.buf.get_mut(..want).unwrap_or_default())?;
            self.pos += n as u64;
            self.start = 0;
            self.filled = n;
            if n == 0 {
                // A short source: treat as end.
                self.end = self.pos;
            }
        }
        Ok(self.buf.get(self.start..self.filled).unwrap_or_default())
    }

    fn consume(&mut self, amt: usize) {
        self.start = (self.start + amt).min(self.filled);
    }
}

/// Little-endian `u16` at `off`.
pub(crate) fn le16(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([*s.first()?, *s.get(1)?]))
}

/// Little-endian `u32` at `off`.
pub(crate) fn le32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes(s.try_into().ok()?))
}

/// Little-endian `u64` at `off`.
pub(crate) fn le64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    Some(u64::from_le_bytes(s.try_into().ok()?))
}

/// Little-endian `f64` at `off`.
pub(crate) fn lef64(b: &[u8], off: usize) -> Option<f64> {
    le64(b, off).map(f64::from_bits)
}

/// Decodes UTF-16LE (lossy).
pub(crate) fn utf16le(b: &[u8]) -> String {
    let units = b.chunks_exact(2).map(|c| {
        u16::from_le_bytes([
            c.first().copied().unwrap_or(0),
            c.get(1).copied().unwrap_or(0),
        ])
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Windows-1252 (lossless for every byte).
pub(crate) fn cp1252(b: &[u8]) -> String {
    let (s, _, _) = encoding_rs::WINDOWS_1252.decode(b);
    s.into_owned()
}

/// Windows FILETIME (100 ns since 1601) → ISO-8601 UTC (`…Z`), as Tika
/// writes dates; `None` for zero or out-of-range values.
pub(crate) fn filetime_iso(ft: u64) -> Option<String> {
    if ft == 0 {
        return None;
    }
    let secs = i64::try_from(ft / 10_000_000).ok()? - 11_644_473_600;
    unix_iso(secs)
}

/// Unix seconds → `yyyy-MM-ddTHH:mm:ssZ`.
pub(crate) fn unix_iso(secs: i64) -> Option<String> {
    let ts = jiff::Timestamp::from_second(secs).ok()?;
    Some(ts.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Java's `Double.toString`: decimal notation with at least one digit
/// after the point for 10⁻³ ≤ |d| < 10⁷, otherwise computerized scientific
/// notation (`1.0E10`), shortest round-trip digits.
pub fn java_double_to_string(d: f64) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if d == 0.0 {
        return if d.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let a = d.abs();
    if (1e-3..1e7).contains(&a) {
        let s = format!("{d}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        // Rust's `{:e}` gives the shortest digits: "1.2345e7" / "1e-5".
        let s = format!("{d:e}");
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let mant = if mant.contains('.') {
            mant.to_owned()
        } else {
            format!("{mant}.0")
        };
        format!("{mant}E{exp}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_doubles() {
        assert_eq!(java_double_to_string(1.0), "1.0");
        assert_eq!(java_double_to_string(-2.5), "-2.5");
        assert_eq!(java_double_to_string(1e7), "1.0E7");
        assert_eq!(java_double_to_string(12_345_678.9), "1.23456789E7");
        assert_eq!(java_double_to_string(0.001), "0.001");
        assert_eq!(java_double_to_string(0.0001), "1.0E-4");
        assert_eq!(java_double_to_string(0.0), "0.0");
        assert_eq!(java_double_to_string(3.25), "3.25");
    }

    #[test]
    fn src_reader_reads_all() {
        let src =
            pm_core::io::MemSource::new((0..=255u8).cycle().take(100_000).collect::<Vec<_>>());
        let mut r = SrcReader::with_capacity(&src, 1000);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap_or_default();
        assert_eq!(out.len(), 100_000);
        assert_eq!(out.get(99_999), Some(&((99_999 % 256) as u8)));
    }

    #[test]
    fn times() {
        assert_eq!(filetime_iso(0), None);
        assert_eq!(
            filetime_iso(132_223_104_000_000_000).as_deref(),
            Some("2020-01-01T00:00:00Z")
        );
        assert_eq!(unix_iso(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }
}
