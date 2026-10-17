//! Text sniffing: Tika's `TextStatistics` / `TextDetector`, and the
//! charset check Autopsy applies to `.txt` files Tika calls binary.

/// Byte histogram (Tika `TextStatistics`).
#[derive(Debug, Clone)]
pub struct TextStatistics {
    counts: [u64; 256],
    total: u64,
}

impl Default for TextStatistics {
    fn default() -> Self {
        Self {
            counts: [0; 256],
            total: 0,
        }
    }
}

impl TextStatistics {
    /// Statistics over `data`.
    pub fn of(data: &[u8]) -> Self {
        let mut s = Self::default();
        s.add(data);
        s
    }

    /// Adds bytes.
    pub fn add(&mut self, data: &[u8]) {
        for &b in data {
            if let Some(c) = self.counts.get_mut(usize::from(b)) {
                *c += 1;
            }
        }
        self.total += data.len() as u64;
    }

    fn count(&self, from: usize, to: usize) -> u64 {
        self.counts.get(from..to).map_or(0, |c| c.iter().sum())
    }

    /// Tab, LF, CR, FF and ESC.
    fn safe_control(&self) -> u64 {
        [0x09u8, 0x0A, 0x0D, 0x0C, 0x1B]
            .iter()
            .map(|&b| self.count_of(b))
            .sum()
    }

    /// Total bytes seen.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Control bytes other than the safe ones.
    pub fn control(&self) -> u64 {
        self.count(0, 0x20) - self.safe_control()
    }

    /// `isMostlyAscii`: < 2 % unsafe control bytes and > 90 % ASCII.
    pub fn is_mostly_ascii(&self) -> bool {
        let control = self.count(0, 0x20);
        let ascii = self.count(0x20, 128);
        let safe = self.safe_control();
        self.total > 0
            && (control - safe) * 100 < self.total * 2
            && (ascii + safe) * 100 > self.total * 90
    }

    /// `looksLikeUTF8`.
    pub fn looks_like_utf8(&self) -> bool {
        let control = self.count(0, 0x20);
        let mut utf8 = self.count(0x20, 0x80);
        let safe = self.safe_control();
        let mut expected = 0u64;
        let leading = [
            self.count(0xC0, 0xE0),
            self.count(0xE0, 0xF0),
            self.count(0xF0, 0xF8),
        ];
        for (i, n) in leading.iter().enumerate() {
            utf8 += n;
            expected += (i as u64 + 1) * n;
        }
        let continuation = self.count(0x80, 0xC0);
        utf8 > 0
            && continuation <= expected
            && continuation + 3 >= expected
            && self.count(0xF8, 0x100) == 0
            && (control - safe) * 100 < utf8 * 2
    }

    /// Fraction of a byte value.
    pub fn count_of(&self, b: u8) -> u64 {
        self.counts.get(usize::from(b)).copied().unwrap_or(0)
    }
}

/// `TextDetector(65536)` on the magic prefix: `true` for text.
pub fn looks_like_text(data: &[u8]) -> bool {
    let s = TextStatistics::of(data.get(..data.len().min(64 * 1024)).unwrap_or(data));
    s.is_mostly_ascii() || s.looks_like_utf8()
}

/// Whether a charset detector would accept `data` as text in some
/// encoding (Autopsy's `EncodingUtils.getEncoding` != unknown).
///
/// Autopsy combines ICU4J's `CharsetDetector` (confidence ≥ 41) with
/// Decodetect (≥ 0.4). This approximation accepts byte-order marks,
/// UTF-16 text without a BOM, and legacy single- or multi-byte text with
/// almost no control bytes.
pub fn has_text_encoding(data: &[u8]) -> bool {
    let data = data.get(..data.len().min(64 * 1024)).unwrap_or(data);
    if data.is_empty() {
        return false;
    }
    if data.starts_with(&[0xEF, 0xBB, 0xBF])
        || data.starts_with(&[0xFF, 0xFE])
        || data.starts_with(&[0xFE, 0xFF])
    {
        return true;
    }
    // UTF-16 without a BOM: one byte of most pairs is NUL, the other
    // printable.
    let pairs = data.len() / 2;
    if pairs >= 4 {
        let (mut le, mut be) = (0usize, 0usize);
        for p in data.chunks_exact(2) {
            let (p0, p1) = (
                p.first().copied().unwrap_or(0),
                p.get(1).copied().unwrap_or(0),
            );
            let printable = |b: u8| (0x20..0x7F).contains(&b) || matches!(b, b'\t' | b'\n' | b'\r');
            if p1 == 0 && printable(p0) {
                le += 1;
            }
            if p0 == 0 && printable(p1) {
                be += 1;
            }
        }
        if le * 10 >= pairs * 7 || be * 10 >= pairs * 7 {
            return true;
        }
    }
    let s = TextStatistics::of(data);
    let bad = s.control() + s.count_of(0x7F);
    // Legacy encodings: printable everywhere, no NULs.
    s.count_of(0) == 0 && bad * 100 <= s.total()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics() {
        assert!(looks_like_text(b"hello world\n"));
        assert!(looks_like_text("päivää maailma, hyvää yötä".as_bytes()));
        assert!(!looks_like_text(&[0, 1, 2, 3, 4, 5, 0xFF, 0xFE]));
        assert!(!looks_like_text(b""));
        // Latin-1 accents: neither ASCII nor UTF-8.
        assert!(!looks_like_text(&[0xE4u8; 40]));
    }

    #[test]
    fn encodings() {
        assert!(has_text_encoding(&[0xE4u8; 40]));
        let utf16: Vec<u8> = "hello there"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(has_text_encoding(&utf16));
        assert!(!has_text_encoding(&[0u8, 1, 2, 3, 0, 0, 7, 8]));
    }
}
