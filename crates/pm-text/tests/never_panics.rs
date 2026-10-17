#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
//! "Never panics" properties for every extractor on hostile input:
//! random bytes, and valid-looking headers followed by random bytes.

use pm_text::strings::{Script, StringsConfig, extract_bytes};
use pm_text::{Chunker, ExtractOptions, Kind, StringSink, extract, kind_for_mime, sniff_mime};
use proptest::prelude::*;

const KINDS: [Kind; 11] = [
    Kind::Plain,
    Kind::Html,
    Kind::Xml,
    Kind::Sqlite,
    Kind::Pdf,
    Kind::Ooxml,
    Kind::Odf,
    Kind::Epub,
    Kind::Ole,
    Kind::Rtf,
    Kind::Email,
];

/// Magic numbers that steer the parsers past their first checks.
const HEADERS: [&[u8]; 8] = [
    b"%PDF-1.7\n",
    b"{\\rtf1\\ansi ",
    b"PK\x03\x04",
    b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
    b"From: a@b.c\nTo: d@e.f\nSubject: x\nContent-Type: multipart/mixed; boundary=b\n\n--b\n",
    b"<html><body>",
    b"<?xml version=\"1.0\"?><a>",
    b"SQLite format 3\0",
];

fn run_all(data: &[u8]) {
    let src = pm_core::io::MemSource::new(data.to_vec());
    let tmp = tempfile::tempdir().unwrap();
    let opts = ExtractOptions {
        temp_dir: Some(tmp.path().to_path_buf()),
        sqlite_wal: None,
    };
    for kind in KINDS {
        let mut sink = StringSink::with_limit(1 << 20);
        let _ = extract(kind, &src, &mut sink, &opts);
        assert!(sink.text.len() <= 1 << 20);
    }
    let mime = sniff_mime(data, "x.bin");
    let _ = kind_for_mime(mime, data.len() as u64);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn extractors_never_panic(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        run_all(&data);
    }

    #[test]
    fn extractors_never_panic_after_magic(
        header in 0..HEADERS.len(),
        tail in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        let mut data = HEADERS[header].to_vec();
        data.extend(tail);
        run_all(&data);
    }

    #[test]
    fn strings_and_chunks_never_panic(data in proptest::collection::vec(any::<u8>(), 0..20_000)) {
        let intl = StringsConfig {
            utf8: true,
            utf16: true,
            scripts: vec![Script::Latin2, Script::Cyrillic, Script::Han],
        };
        for cfg in [StringsConfig::default(), intl, StringsConfig::extractor_default()] {
            let text = extract_bytes(&data, &cfg);
            for c in Chunker::split(&text) {
                prop_assert!(c.base_bytes <= c.text.len());
                prop_assert!(c.base_lower_bytes <= c.lower.len());
            }
        }
    }
}
