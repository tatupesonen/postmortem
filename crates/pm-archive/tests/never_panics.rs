//! Property tests: no input makes a reader panic, loop or exceed its limits.
//!
//! Each format is fed random bytes and mutated (bit-flipped, truncated,
//! spliced) copies of a valid fixture.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod common;

use std::sync::OnceLock;

use common::Collect;
use pm_archive::{Format, Options};
use pm_core::io::MemSource;
use pm_testkit::archives as ak;
use proptest::prelude::*;

const TEXT: &[u8] = b"The quick brown fox jumps over the lazy dog.\n";

/// Tight limits so that hostile inputs finish quickly.
fn opts() -> Options {
    Options {
        ratio_min_size: 1 << 20,
        max_dictionary: 1 << 24,
        max_in_memory: 1 << 22,
        max_members: 10_000,
        ..Options::default()
    }
}

/// One valid sample per format.
fn seeds() -> &'static [(Format, Vec<u8>)] {
    static SEEDS: OnceLock<Vec<(Format, Vec<u8>)>> = OnceLock::new();
    SEEDS.get_or_init(|| {
        let files: &[(&str, &[u8])] = &[("a.txt", TEXT), ("dir/b.txt", b"bbbb")];
        let png = ak::png(4, 4);
        let jpg = ak::jpeg(8, 8);
        vec![
            (Format::Zip, ak::zip(files)),
            (
                Format::Tar,
                ak::tar(&[
                    ak::TarEntry::Pax("a.txt".into(), TEXT.to_vec()),
                    ak::TarEntry::GnuLong("b.txt".into(), b"bbbb".to_vec()),
                ]),
            ),
            (Format::Gzip, ak::gzip(Some("a.txt"), TEXT)),
            (Format::Bzip2, ak::bzip2_bytes(TEXT)),
            (Format::Xz, ak::xz_bytes(TEXT)),
            (Format::Zstd, ak::zstd_bytes(TEXT)),
            (
                Format::SevenZip,
                ak::sevenz(files, &["dir"], false, None, false),
            ),
            (
                Format::Rar,
                ak::rar(rars::ArchiveVersion::Rar50, files, None),
            ),
            (
                Format::Rar,
                ak::rar(rars::ArchiveVersion::Rar29, files, None),
            ),
            (Format::Iso9660, ak::iso(files, true, true)),
            (
                Format::Docx,
                ak::ooxml("word", &[("image1.png", &png)], &[("o.bin", b"ole")]),
            ),
            (
                Format::Doc,
                ak::doc(&[
                    ak::blip(ak::BLIP_PNG, &png, 1),
                    ak::blip(ak::BLIP_JPEG, &jpg, 2),
                ]),
            ),
            (Format::Xls, ak::xls(&[ak::blip(ak::BLIP_PNG, &png, 1)])),
            (
                Format::Ppt,
                ak::ppt(&[ak::blip(ak::BLIP_EMF, b"emf data", 1)]),
            ),
            (
                Format::Pdf,
                ak::pdf(&[("att.txt", TEXT)], &[ak::PdfImage::Jpeg(jpg.clone())]),
            ),
        ]
    })
}

const ALL: [Format; 16] = [
    Format::Zip,
    Format::SevenZip,
    Format::Rar,
    Format::Tar,
    Format::Gzip,
    Format::Bzip2,
    Format::Xz,
    Format::Zstd,
    Format::Iso9660,
    Format::Docx,
    Format::Xlsx,
    Format::Pptx,
    Format::Doc,
    Format::Xls,
    Format::Ppt,
    Format::Pdf,
];

/// Runs detection and extraction; only panics count as failures.
fn exercise(format: Format, data: Vec<u8>) {
    let src = MemSource::new(data);
    let _ = pm_archive::detect(&src);
    let mut sink = Collect::default();
    let o = opts();
    if let Ok(s) = pm_archive::extract(format, &src, &o, &mut sink) {
        assert!(s.members <= o.max_members);
        assert!(s.decoded_bytes < 1 << 30);
    }
}

#[derive(Debug, Clone)]
enum Mutation {
    Flip(usize, u8),
    Truncate(usize),
    Splice(usize, Vec<u8>),
}

fn mutate(mut data: Vec<u8>, ms: &[Mutation]) -> Vec<u8> {
    for m in ms {
        if data.is_empty() {
            break;
        }
        match m {
            Mutation::Flip(at, mask) => {
                let i = at % data.len();
                data[i] ^= mask;
            }
            Mutation::Truncate(at) => data.truncate(at % data.len()),
            Mutation::Splice(at, bytes) => {
                let i = at % data.len();
                let end = (i + bytes.len()).min(data.len());
                data[i..end].copy_from_slice(&bytes[..end - i]);
            }
        }
    }
    data
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        4 => (any::<usize>(), 1u8..).prop_map(|(a, m)| Mutation::Flip(a, m)),
        1 => any::<usize>().prop_map(Mutation::Truncate),
        2 => (any::<usize>(), proptest::collection::vec(
            prop_oneof![Just(0u8), Just(0xFF), Just(0x7F), Just(0x80), any::<u8>()], 1..9))
            .prop_map(|(a, b)| Mutation::Splice(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1024, ..ProptestConfig::default() })]

    #[test]
    fn mutated_fixtures_never_panic(
        seed in any::<prop::sample::Index>(),
        ms in proptest::collection::vec(mutation(), 1..6),
    ) {
        let (format, data) = seed.get(seeds()).clone();
        exercise(format, mutate(data, &ms));
    }

    #[test]
    fn random_bytes_never_panic(
        f in any::<prop::sample::Index>(),
        data in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        exercise(*f.get(&ALL), data);
    }

    #[test]
    fn random_bytes_behind_a_signature_never_panic(
        seed in any::<prop::sample::Index>(),
        keep in 4usize..64,
        data in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let (format, valid) = seed.get(seeds()).clone();
        let mut v = valid[..keep.min(valid.len())].to_vec();
        v.extend(data);
        exercise(format, v);
    }
}

#[test]
fn every_seed_is_valid() {
    for (format, data) in seeds() {
        let src = MemSource::new(data.clone());
        let mut sink = Collect::default();
        let s = pm_archive::extract(*format, &src, &opts(), &mut sink)
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));
        assert!(s.complete > 0, "{format:?}: {s:?}");
    }
}
