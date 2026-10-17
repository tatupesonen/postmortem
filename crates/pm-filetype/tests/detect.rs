#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

//! Cross-module behaviour of `pm-filetype`: robustness on hostile input
//! (every container route), the detectable-type lists, and a throughput
//! benchmark (`cargo test -p pm-filetype --release -- --ignored bench`).

use pm_core::io::MemSource;
use pm_filetype::cfb::builder::{Stream, build as build_cfb};
use pm_filetype::{
    FileClass, FileType, FileTypeDetector, Signature, ZipBuilder, autopsy_defined_types,
    detected_types, tika_detect, tika_detectable_types,
};
use proptest::prelude::*;

/// Prefixes that route input into each detector (containers, magic with
/// offsets/regexes, XML roots, text statistics).
const PREFIXES: &[&[u8]] = &[
    b"",
    b"PK\x03\x04",
    b"PK\x03\x04\x14\x00\x08\x00\x08\x00",
    b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
    b"OggS\x00\x02",
    b"bplist00",
    b"\x1F\x8B\x08\x00",
    b"<?xml version=\"1.0\"?>",
    b"<?xml version=\"1.0\" encoding=\"UTF-16\"?>",
    b"\xFF\xFE<\x00?\x00x\x00",
    b"<!DOCTYPE html [<!ENTITY a '&b;'>]><html xmlns='&a;'>",
    b"%PDF-1.7\n",
    b"7z\xBC\xAF\x27\x1C",
    b"Rar!\x1A\x07\x00",
    b"MZ\x90\x00",
    b"{\\rtf1",
    b"\xEF\xBB\xBF",
    b"BZh91AY&SY",
];

fn detect(d: &FileTypeDetector, data: &[u8], name: &str) -> String {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
    d.detect(FileClass::File, name, &ext, &MemSource::new(data.to_vec()))
}

/// A small but structurally valid sample of each container kind.
fn containers() -> Vec<(&'static str, Vec<u8>)> {
    let content_types = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    vec![
        (
            "a.docx",
            ZipBuilder::new()
                .deflated("[Content_Types].xml", content_types)
                .entry("word/document.xml", &[b'x'; 3000], true, true, false)
                .build(),
        ),
        (
            "a.odt",
            ZipBuilder::new()
                .stored("mimetype", b"application/vnd.oasis.opendocument.text")
                .deflated("content.xml", b"<office:document-content/>")
                .build(),
        ),
        (
            "a.jar",
            ZipBuilder::new()
                .deflated("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\r\n")
                .build(),
        ),
        (
            "a.doc",
            build_cfb(&[
                Stream::root("WordDocument", &[0u8; 600]),
                Stream::root("\u{1}CompObj", &[1u8; 100]),
            ]),
        ),
        (
            "a.xls",
            build_cfb(&[Stream::root("Workbook", &[7u8; 5000])]),
        ),
    ]
}

#[test]
fn container_samples() {
    let d = FileTypeDetector::default();
    let want = [
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "application/vnd.oasis.opendocument.text",
        "application/java-archive",
        "application/msword",
        "application/vnd.ms-excel",
    ];
    for ((name, data), want) in containers().iter().zip(want) {
        assert_eq!(detect(&d, data, name), want, "{name}");
        // Without a name, the container alone decides.
        assert_eq!(
            tika_detect(&MemSource::new(data.clone()), None).unwrap(),
            want,
            "{name}"
        );
    }
}

#[test]
fn detectable_type_lists() {
    let tika = tika_detectable_types();
    assert!(tika.len() > 1500, "{}", tika.len());
    assert!(
        tika.iter()
            .all(|t| !t.contains("tika-") && !t.contains(';'))
    );
    for t in [
        "application/pdf",
        "image/png",
        "application/x-msoffice",
        "text/plain",
    ] {
        assert!(tika.iter().any(|x| x == t), "{t}");
    }
    let user = FileType::new("application/x-mine", vec![Signature::ascii("MINE", 0)]);
    let all = detected_types(std::slice::from_ref(&user));
    assert!(all.iter().any(|t| t == "application/x-mine"));
    for a in autopsy_defined_types() {
        assert!(all.contains(&a.mime_type), "{}", a.mime_type);
    }
    // Sorted case-insensitively, without duplicates.
    assert!(all.windows(2).all(|w| {
        let (a, b) = (w[0].to_lowercase(), w[1].to_lowercase());
        a < b || (a == b && w[0] < w[1])
    }));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arbitrary bytes behind every routing prefix never panic, with or
    /// without a name.
    #[test]
    fn never_panics(
        prefix in prop::sample::select(PREFIXES),
        tail in prop::collection::vec(any::<u8>(), 0..4096),
        named in any::<bool>(),
    ) {
        let mut data = prefix.to_vec();
        data.extend_from_slice(&tail);
        let d = FileTypeDetector::default();
        let name = if named { "x.docx" } else { "x" };
        let t = detect(&d, &data, name);
        prop_assert!(t.contains('/'));
        let _ = tika_detect(&MemSource::new(data), named.then_some(name));
    }

    /// Valid containers with random corruption never panic.
    #[test]
    fn corrupted_containers_never_panic(
        which in 0usize..5,
        flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..32),
        cut in any::<prop::sample::Index>(),
    ) {
        let (name, mut data) = containers().swap_remove(which);
        for (i, b) in flips {
            let i = i.index(data.len());
            data[i] = b;
        }
        let keep = cut.index(data.len() + 1).max(data.len() / 2);
        data.truncate(keep);
        let d = FileTypeDetector::default();
        let t = detect(&d, &data, name);
        prop_assert!(t.contains('/'));
    }
}

/// Throughput over a mixed synthetic corpus.
#[test]
#[ignore = "benchmark; run with --release -- --ignored"]
fn bench_detect_throughput() {
    let mut corpus: Vec<(String, Vec<u8>)> = Vec::new();
    for (n, d) in containers() {
        corpus.push((n.to_owned(), d));
    }
    let text = "The quick brown fox jumps over the lazy dog.\n".repeat(200);
    corpus.push(("notes.txt".into(), text.clone().into_bytes()));
    corpus.push(("readme".into(), text.into_bytes()));
    corpus.push((
        "page.html".into(),
        b"<!DOCTYPE html><html><head><title>x</title></head><body>hi</body></html>".to_vec(),
    ));
    corpus.push((
        "data.xml".into(),
        b"<?xml version=\"1.0\"?><root><a/></root>".to_vec(),
    ));
    corpus.push((
        "doc.pdf".into(),
        b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n1 0 obj".to_vec(),
    ));
    corpus.push(("img.png".into(), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec()));
    corpus.push(("img.jpg".into(), b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".to_vec()));
    corpus.push(("prog.exe".into(), b"MZ\x90\0".repeat(1000)));
    let mut x = 0x9E37_79B9u32;
    let noise: Vec<u8> = (0..65_536)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    corpus.push(("blob.bin".into(), noise));
    let srcs: Vec<(String, String, MemSource)> = corpus
        .into_iter()
        .map(|(n, d)| {
            let ext = n.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase();
            (n, ext, MemSource::new(d))
        })
        .collect();
    let d = FileTypeDetector::default();
    // Warm up (database parse).
    let _ = d.detect(FileClass::File, "a", "", &srcs[0].2);
    let rounds = 300;
    let start = std::time::Instant::now();
    let mut bytes = 0u64;
    for _ in 0..rounds {
        for (n, e, s) in &srcs {
            let _ = d.detect(FileClass::File, n, e, s);
            bytes += pm_core::ReadAt::size(s);
        }
    }
    let secs = start.elapsed().as_secs_f64();
    let files = (rounds * srcs.len()) as f64;
    eprintln!(
        "pm-filetype detect: {:.0} files/s/thread, {:.1} MB/s ({} files)",
        files / secs,
        bytes as f64 / secs / 1e6,
        files
    );
}
