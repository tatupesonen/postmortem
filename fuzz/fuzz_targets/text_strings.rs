#![no_main]
//! Autopsy's two string scanners and the chunker on arbitrary bytes:
//! never panic, and every chunk's base and overlap stay inside it.

use libfuzzer_sys::fuzz_target;
use pm_text::strings::{Script, StringsConfig, extract_bytes};
use pm_text::{Chunker, sniff_mime};

fuzz_target!(|data: &[u8]| {
    let english = extract_bytes(data, &StringsConfig::default());
    let intl = StringsConfig {
        utf8: true,
        utf16: true,
        scripts: vec![Script::Latin2, Script::Cyrillic, Script::Han, Script::Arabic],
    };
    let international = extract_bytes(data, &intl);
    for text in [english, international, String::from_utf8_lossy(data).into_owned()] {
        for c in Chunker::split(&text) {
            assert!(c.base_bytes <= c.text.len());
            assert!(c.text.is_char_boundary(c.base_bytes));
            assert!(c.base_lower_bytes <= c.lower.len());
        }
    }
    let _ = sniff_mime(data, "fuzz.bin");
});
