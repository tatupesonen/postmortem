//! String extraction from binary data (Autopsy `StringsTextExtractor`
//! and `StringExtract`), used for unallocated space, unknown binaries and
//! files whose format extractor failed.
//!
//! * When the only enabled script is Latin-Basic ([`Script::Latin1`]) the
//!   fast **English-only** scanner runs: runs of at least four printable
//!   ASCII characters (32–126 and TAB), a single NUL inside a run
//!   tolerated and dropped (so UTF-16LE ASCII comes out whole), each run
//!   followed by a newline.
//! * Otherwise the **international** scanner tries UTF-16LE, UTF-16BE
//!   (even offsets) and UTF-8 at every offset, keeps the decode with the
//!   most characters and accepts it at four or more characters. Every
//!   character must belong to an enabled script or be "common"
//!   (digits, punctuation), and a run may not switch scripts (Autopsy's
//!   per-code-point table). Unlike Autopsy, a string that crosses a 1 MiB
//!   read block is not split.

mod english;
mod international;
mod script_table;

use pm_core::ReadAt;

use crate::{TextError, TextSink};

/// Minimum characters in an extracted string (Autopsy
/// `MIN_CHARS_STRING` / `MIN_PRINTABLE_CHARS`).
pub const MIN_CHARS: usize = 4;

/// Scripts the international scanner can extract (Autopsy
/// `StringExtract.SUPPORTED_SCRIPTS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Latin1,
    Latin2,
    Arabic,
    Cyrillic,
    Han,
    Hiragana,
    Katakana,
    Hangul,
    Armenian,
    Bengali,
    Khmer,
    Ethiopic,
    Georgian,
    Hebrew,
    Lao,
    Mongolian,
    Thai,
    Tibetan,
}

impl Script {
    pub const ALL: [Self; 18] = [
        Self::Latin1,
        Self::Latin2,
        Self::Arabic,
        Self::Cyrillic,
        Self::Han,
        Self::Hiragana,
        Self::Katakana,
        Self::Hangul,
        Self::Armenian,
        Self::Bengali,
        Self::Khmer,
        Self::Ethiopic,
        Self::Georgian,
        Self::Hebrew,
        Self::Lao,
        Self::Mongolian,
        Self::Thai,
        Self::Tibetan,
    ];

    /// Ordinal in Autopsy's `SCRIPT` enum (the table's values).
    fn ordinal(self) -> u8 {
        match self {
            Self::Latin1 => 2,
            Self::Cyrillic => 4,
            Self::Armenian => 5,
            Self::Hebrew => 6,
            Self::Arabic => 7,
            Self::Bengali => 11,
            Self::Thai => 20,
            Self::Lao => 21,
            Self::Tibetan => 22,
            Self::Georgian => 24,
            Self::Hangul => 25,
            Self::Ethiopic => 26,
            Self::Khmer => 31,
            Self::Mongolian => 32,
            Self::Hiragana => 33,
            Self::Katakana => 34,
            Self::Han => 36,
            Self::Latin2 => 68,
        }
    }

    /// Autopsy's enum name, used in settings (`"LATIN_1"`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Latin1 => "LATIN_1",
            Self::Latin2 => "LATIN_2",
            Self::Arabic => "ARABIC",
            Self::Cyrillic => "CYRILLIC",
            Self::Han => "HAN",
            Self::Hiragana => "HIRAGANA",
            Self::Katakana => "KATAKANA",
            Self::Hangul => "HANGUL",
            Self::Armenian => "ARMENIAN",
            Self::Bengali => "BENGALI",
            Self::Khmer => "KHMER",
            Self::Ethiopic => "ETHIOPIC",
            Self::Georgian => "GEORGIAN",
            Self::Hebrew => "HEBREW",
            Self::Lao => "LAO",
            Self::Mongolian => "MONGOLIAN",
            Self::Thai => "THAI",
            Self::Tibetan => "TIBETAN",
        }
    }

    /// Display name (Autopsy `SCRIPT.toString()`).
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Latin1 => "Latin - Basic",
            Self::Latin2 => "Latin - Extended",
            Self::Arabic => "Arabic",
            Self::Cyrillic => "Cyrillic",
            Self::Han => "Han",
            Self::Hiragana => "Hiragana",
            Self::Katakana => "Katakana",
            Self::Hangul => "Hangul",
            Self::Armenian => "Armenian",
            Self::Bengali => "Bengali",
            Self::Khmer => "Khmer",
            Self::Ethiopic => "Ethiopic",
            Self::Georgian => "Georgian",
            Self::Hebrew => "Hebrew",
            Self::Lao => "Laotian",
            Self::Mongolian => "Mongolian",
            Self::Thai => "Thai",
            Self::Tibetan => "Tibetian",
        }
    }

    /// Parses an Autopsy enum name.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// Script ordinals with special meaning.
const NONE: u8 = 0;
const COMMON: u8 = 1;

/// The script ordinal of a BMP code point.
fn script_of(cp: u32) -> u8 {
    let Ok(cp) = u16::try_from(cp) else {
        return NONE;
    };
    let i = script_table::RUNS.partition_point(|&(start, _)| start <= cp);
    i.checked_sub(1)
        .and_then(|i| script_table::RUNS.get(i))
        .map_or(NONE, |&(_, s)| s)
}

/// String-extraction settings (Autopsy `StringsConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringsConfig {
    pub utf8: bool,
    pub utf16: bool,
    pub scripts: Vec<Script>,
}

impl Default for StringsConfig {
    /// Keyword Search's defaults: UTF-8 and UTF-16, Latin-Basic only (so the
    /// English-only scanner runs).
    fn default() -> Self {
        Self {
            utf8: true,
            utf16: true,
            scripts: vec![Script::Latin1],
        }
    }
}

impl StringsConfig {
    /// `StringsTextExtractor` without a configuration: Latin-Extended,
    /// UTF-8 only.
    pub fn extractor_default() -> Self {
        Self {
            utf8: true,
            utf16: false,
            scripts: vec![Script::Latin2],
        }
    }

    /// Whether the English-only scanner is used.
    pub fn is_latin_basic_only(&self) -> bool {
        self.scripts == [Script::Latin1]
    }

    /// Whether characters of the script ordinal may be extracted
    /// (Latin-Basic is implied by Latin-Extended).
    fn enabled(&self, ordinal: u8) -> bool {
        if ordinal == Script::Latin1.ordinal() {
            return self.scripts.contains(&Script::Latin1) || self.scripts.contains(&Script::Latin2);
        }
        self.scripts.iter().any(|s| s.ordinal() == ordinal)
    }
}

/// Extracts strings from `src` into `sink`. Nothing is extracted when both
/// UTF-8 and UTF-16 are off (the extractor is "unsupported" then).
pub fn extract(src: &dyn ReadAt, cfg: &StringsConfig, sink: &mut dyn TextSink) -> Result<(), TextError> {
    if !cfg.utf8 && !cfg.utf16 {
        return Ok(());
    }
    if cfg.is_latin_basic_only() {
        english::extract(src, sink)
    } else {
        international::extract(src, cfg, sink)
    }
}

/// Extracts strings from a byte slice (for tests and small buffers).
pub fn extract_bytes(data: &[u8], cfg: &StringsConfig) -> String {
    let src = pm_core::io::MemSource::new(data.to_vec());
    let mut sink = crate::StringSink::default();
    let _ = extract(&src, cfg, &mut sink);
    sink.text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lookups() {
        assert_eq!(script_of(u32::from('a')), Script::Latin1.ordinal());
        assert_eq!(script_of(u32::from('é')), Script::Latin1.ordinal());
        assert_eq!(script_of(u32::from('ł')), Script::Latin2.ordinal());
        assert_eq!(script_of(u32::from(' ')), COMMON);
        assert_eq!(script_of(u32::from('1')), COMMON);
        assert_eq!(script_of(u32::from('\t')), COMMON);
        assert_eq!(script_of(u32::from('\n')), 67);
        assert_eq!(script_of(u32::from('ж')), Script::Cyrillic.ordinal());
        assert_eq!(script_of(u32::from('中')), Script::Han.ordinal());
        assert_eq!(script_of(u32::from('ك')), Script::Arabic.ordinal());
        assert_eq!(script_of(0x1_F600), NONE);
        assert_eq!(Script::from_name("HANGUL"), Some(Script::Hangul));
        assert_eq!(Script::from_name("x"), None);
    }

    #[test]
    fn disabled_extracts_nothing() {
        let cfg = StringsConfig {
            utf8: false,
            utf16: false,
            scripts: vec![Script::Latin2],
        };
        assert_eq!(extract_bytes(b"hello world", &cfg), "");
    }
}
