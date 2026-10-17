//! File-name decoding and charset repair.
//!
//! Zip (without the UTF-8 flag), tar and old RAR archives store names as
//! bytes in whatever code page the archiver used. Like 7-Zip, names are
//! decoded as UTF-8 when valid and as CP437 (the DOS code page zip tools
//! default to) otherwise. Autopsy then re-decodes the names of each
//! directory when ICU's charset detector is confident (≥ 90) about their
//! raw bytes, which fixes Shift-JIS, GBK, Big5, Cyrillic, … names;
//! [`repair`] does the same with `chardetng`, accepting only encodings it
//! detects reliably on short strings (UTF-8, East Asian multi-byte and the
//! non-Latin single-byte code pages). Latin guesses are ignored because
//! CP437 is far more likely for Western zip files.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// CP437 code points for bytes 0x80..=0xFF.
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// Decodes CP437 (IBM PC / DOS US).
pub fn decode_cp437(b: &[u8]) -> String {
    b.iter()
        .map(|&c| {
            if c < 0x80 {
                char::from(c)
            } else {
                CP437_HIGH
                    .get(usize::from(c - 0x80))
                    .copied()
                    .unwrap_or('\u{fffd}')
            }
        })
        .collect()
}

/// The default decoding of stored name bytes: UTF-8 when valid, CP437
/// otherwise (7-Zip's behaviour for zip names without the UTF-8 flag).
pub fn decode_default(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => decode_cp437(b),
    }
}

/// Decodes with an `encoding_rs` encoding (malformed sequences replaced).
pub fn decode_with(enc: &'static Encoding, b: &[u8]) -> String {
    enc.decode_without_bom_handling(b).0.into_owned()
}

/// Encodings accepted from the detector (see the module documentation).
fn trusted(enc: &'static Encoding) -> bool {
    use encoding_rs as e;
    [
        e::UTF_8,
        e::SHIFT_JIS,
        e::EUC_JP,
        e::ISO_2022_JP,
        e::GBK,
        e::GB18030,
        e::BIG5,
        e::EUC_KR,
        e::IBM866,
        e::WINDOWS_1251,
        e::KOI8_R,
        e::KOI8_U,
        e::WINDOWS_1253,
        e::WINDOWS_1255,
        e::WINDOWS_1256,
        e::WINDOWS_874,
    ]
    .contains(&enc)
}

/// Autopsy's per-directory charset fix: looks at the raw names of the
/// children of one directory (about the first 1000 bytes of them, as
/// Autopsy does) and returns the encoding to re-decode them with, or
/// `None` to keep [`decode_default`]. Pure-ASCII names never change.
pub fn repair(raw_names: &[&[u8]]) -> Option<&'static Encoding> {
    let mut sample = Vec::new();
    for n in raw_names {
        sample.extend_from_slice(n);
        if sample.len() >= 1000 {
            break;
        }
    }
    if sample.is_ascii() {
        return None;
    }
    if std::str::from_utf8(&sample).is_ok() {
        return Some(encoding_rs::UTF_8);
    }
    // Too little evidence for a reliable guess.
    if sample.iter().filter(|b| !b.is_ascii()).count() < 4 {
        return None;
    }
    let mut det = EncodingDetector::new(Iso2022JpDetection::Allow);
    det.feed(&sample, true);
    let enc = det.guess(None, Utf8Detection::Allow);
    if !trusted(enc) {
        return None;
    }
    // Every name must decode cleanly.
    raw_names
        .iter()
        .all(|n| !enc.decode_without_bom_handling(n).1)
        .then_some(enc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp437() {
        assert_eq!(decode_cp437(b"abc"), "abc");
        assert_eq!(decode_cp437(&[0x84, 0x94, 0x81, 0xE1]), "äöüß");
        assert_eq!(decode_cp437(&[0xFF]), "\u{a0}");
        assert_eq!(decode_default("名前".as_bytes()), "名前");
        assert_eq!(decode_default(&[0x8E]), "Ä");
    }

    #[test]
    fn repairs_shift_jis_names() {
        let names = ["日本語のファイル.txt", "写真フォルダ", "報告書2020.doc"];
        let raw: Vec<Vec<u8>> = names
            .iter()
            .map(|n| encoding_rs::SHIFT_JIS.encode(n).0.into_owned())
            .collect();
        let refs: Vec<&[u8]> = raw.iter().map(Vec::as_slice).collect();
        let enc = repair(&refs).expect("detected");
        assert_eq!(enc, encoding_rs::SHIFT_JIS);
        assert_eq!(decode_with(enc, &raw[0]), names[0]);
    }

    #[test]
    fn ascii_and_latin_names_keep_the_default() {
        assert_eq!(repair(&[b"readme.txt", b"a/b.c"]), None);
        assert_eq!(repair(&[]), None);
        // A lone non-ASCII byte is too little evidence.
        assert_eq!(repair(&[&[0x84, b'r', b'g', b'e', b'r']]), None);
        // CP437 umlauts look Latin to the detector: not trusted.
        let cp437 = [
            &b"Gr\x81\xe1e \x8epfel.txt"[..],
            b"M\x84rchen \x94l.doc",
            b"\x9abersicht.xls",
        ];
        assert_eq!(repair(&cp437), None);
        assert_eq!(repair(&["żółw".as_bytes()]), Some(encoding_rs::UTF_8));
    }
}
