//! Character-encoding detection and streaming decoding (Autopsy
//! `EncodingUtils`, which uses ICU's detector and Decodetect).
//!
//! Detection order: byte-order mark → UTF-16 recognised from NUL bytes at
//! every other position → valid UTF-8 → `chardetng` over the first
//! 100 KB. Unknown text decodes as UTF-8 (Autopsy's fallback).

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use pm_core::ReadAt;

use crate::{Stop, TextError, TextSink};

/// Bytes examined for detection (Autopsy's Decodetect sample size).
pub const DETECT_BYTES: usize = 100_000;
const BLOCK: usize = 64 * 1024;

/// Detects the encoding of a text sample (the start of a file).
pub fn detect(sample: &[u8]) -> &'static Encoding {
    if let Some((enc, _)) = Encoding::for_bom(sample) {
        return enc;
    }
    if let Some(enc) = utf16_without_bom(sample) {
        return enc;
    }
    if is_utf8_prefix(sample) {
        return UTF_8;
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(sample, false);
    det.guess(None, chardetng::Utf8Detection::Allow)
}

/// Reads up to [`DETECT_BYTES`] of `src` and detects its encoding.
pub fn detect_source(src: &dyn ReadAt) -> Result<&'static Encoding, TextError> {
    let n = usize::try_from(src.size().min(DETECT_BYTES as u64)).unwrap_or(0);
    let mut buf = vec![0u8; n];
    let got = src.read_at(0, &mut buf)?;
    buf.truncate(got);
    Ok(detect(&buf))
}

/// UTF-16 without a BOM: most code units of Latin text have a zero high
/// byte, so one byte position in every pair is mostly NUL.
fn utf16_without_bom(sample: &[u8]) -> Option<&'static Encoding> {
    let s = sample.get(..sample.len().min(4096) & !1)?;
    let pairs = s.len() / 2;
    if pairs < 4 {
        return None;
    }
    let (mut even0, mut odd0) = (0usize, 0usize);
    for p in s.chunks_exact(2) {
        if p.first() == Some(&0) {
            even0 += 1;
        }
        if p.get(1) == Some(&0) {
            odd0 += 1;
        }
    }
    // At least 40 % of one position NUL, the other almost never.
    if odd0 * 10 >= pairs * 4 && even0 * 20 <= pairs {
        Some(UTF_16LE)
    } else if even0 * 10 >= pairs * 4 && odd0 * 20 <= pairs {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Valid UTF-8, allowing a sequence cut off by the end of the sample;
/// pure ASCII also counts (as Autopsy treats it).
fn is_utf8_prefix(sample: &[u8]) -> bool {
    match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && sample.len() - e.valid_up_to() < 4,
    }
}

/// Decodes `src` with `enc` (a BOM, if present, wins and is removed),
/// pushing text in blocks of about 64 KiB.
pub fn decode_to_sink(
    src: &dyn ReadAt,
    enc: &'static Encoding,
    sink: &mut dyn TextSink,
) -> Result<(), TextError> {
    let mut dec = enc.new_decoder_with_bom_removal();
    let size = src.size();
    let mut buf = vec![0u8; BLOCK];
    let mut out = String::with_capacity(BLOCK * 2);
    let mut off = 0u64;
    loop {
        let want = usize::try_from((size - off.min(size)).min(BLOCK as u64)).unwrap_or(0);
        let n = if want == 0 {
            0
        } else {
            src.read_at(off, buf.get_mut(..want).unwrap_or_default())?
        };
        off += n as u64;
        let last = n == 0;
        let input = buf.get(..n).unwrap_or_default();
        out.clear();
        let need = dec
            .max_utf8_buffer_length(input.len())
            .unwrap_or(input.len() * 3 + 16);
        out.reserve(need);
        let (_, _, _) = dec.decode_to_string(input, &mut out, last);
        if !out.is_empty() {
            sink.push(&out).map_err(|Stop| TextError::Stopped)?;
        }
        if last {
            return Ok(());
        }
    }
}

/// Decodes a byte slice completely (BOM removed).
pub fn decode_bytes(bytes: &[u8], enc: &'static Encoding) -> String {
    let (s, _) = enc.decode_with_bom_removal(bytes);
    s.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    #[test]
    fn detects_boms_and_utf16() {
        assert_eq!(detect(b"\xEF\xBB\xBFabc"), UTF_8);
        assert_eq!(detect(b"\xFF\xFEa\0b\0"), UTF_16LE);
        assert_eq!(detect(b"\xFE\xFF\0a\0b"), UTF_16BE);
        assert_eq!(detect(b"h\0e\0l\0l\0o\0 \0w\0o\0r\0l\0d\0"), UTF_16LE);
        assert_eq!(detect(b"\0h\0e\0l\0l\0o\0 \0w\0o\0r\0l\0d"), UTF_16BE);
    }

    #[test]
    fn detects_utf8_and_legacy() {
        assert_eq!(detect("päivää maailma".as_bytes()), UTF_8);
        assert_eq!(detect(b"plain ascii"), UTF_8);
        // Cut-off multi-byte sequence at the end of the sample.
        assert_eq!(detect(b"abc \xC3"), UTF_8);
        // Latin-1 text with umlauts is not UTF-8.
        let latin = b"Gr\xFC\xDFe aus K\xF6ln, sch\xF6ne Gr\xFC\xDFe und viele W\xFCnsche f\xFCr dich";
        let enc = detect(latin);
        assert_ne!(enc, UTF_8);
        assert_eq!(decode_bytes(latin, enc).chars().nth(2), Some('ü'));
    }

    #[test]
    fn streaming_decode_handles_split_sequences() {
        let text = "ä".repeat(BLOCK); // 2 bytes each: block boundaries split chars
        let src = pm_core::io::MemSource::new(text.as_bytes().to_vec());
        let mut sink = StringSink::default();
        decode_to_sink(&src, UTF_8, &mut sink).unwrap();
        assert_eq!(sink.text, text);

        let src = pm_core::io::MemSource::new(b"\xFF\xFEh\0i\0".to_vec());
        let mut sink = StringSink::default();
        decode_to_sink(&src, UTF_16LE, &mut sink).unwrap();
        assert_eq!(sink.text, "hi");
    }
}
