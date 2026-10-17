//! Property tests: hostile bytes never make the carver panic, and every
//! carved range lies inside the source and does not overlap another.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::OnceLock;

use pm_carve::{Carved, Options, carve};
use pm_core::io::MemSource;
use pm_testkit::archives as ak;
use pm_testkit::media;
use proptest::prelude::*;

/// Headers of every family, each followed by a little valid structure.
fn seed() -> &'static Vec<u8> {
    static SEED: OnceLock<Vec<u8>> = OnceLock::new();
    SEED.get_or_init(|| {
        let png = ak::png(4, 4);
        let parts: Vec<Vec<u8>> = vec![
            ak::jpeg(8, 8),
            png.clone(),
            ak::gif(4, 4),
            pm_testkit::demo::gradient_bmp(4, 4),
            media::tiff(4, 4),
            ak::pdf(&[], &[]),
            ak::zip(&[("a", b"b")]),
            ak::doc(&[ak::blip(ak::BLIP_PNG, &png, 1)]),
            ak::sevenz(&[("a", b"b")], &[], false, None, false),
            ak::gzip(None, b"abc"),
            media::mp3(10, true),
            media::wav(10),
            media::mp4(b"isom", 10),
            media::pe(false, 10),
            media::evtx(1),
        ];
        let mut out = Vec::new();
        for p in parts {
            out.extend(p);
            out.resize(out.len().next_multiple_of(512), 0);
        }
        out
    })
}

fn check(data: Vec<u8>, keep: bool) {
    let size = data.len() as u64;
    let src = MemSource::new(data);
    let mut found: Vec<Carved> = Vec::new();
    let opts = Options {
        keep_corrupted: keep,
        chunk: 8192,
        ..Options::default()
    };
    carve(&src, &opts, &|| false, &mut |c| found.push(c)).unwrap();
    let mut end = 0;
    for c in &found {
        assert!(c.len > 0 && c.offset + c.len <= size, "{c:?} in {size}");
        assert!(c.offset >= end, "overlap: {c:?}");
        assert_eq!(c.offset % 512, 0);
        end = c.offset + c.len;
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn mutated_images_never_panic(
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..40),
        cut in any::<prop::sample::Index>(),
        keep in any::<bool>(),
    ) {
        let mut data = seed().clone();
        for (i, b) in &flips {
            let k = i.index(data.len());
            data[k] = *b;
        }
        let cut = cut.index(data.len());
        data.truncate(cut.max(1));
        check(data, keep);
    }

    #[test]
    fn random_bytes_never_panic(
        data in proptest::collection::vec(any::<u8>(), 0..8192),
        keep in any::<bool>(),
    ) {
        check(data, keep);
    }
}

/// Inputs that crashed the `carve_scan` fuzz target (first byte: flags,
/// as in the target).
#[test]
fn fuzz_regressions() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions/carve_scan");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in entries {
        let data = std::fs::read(e.unwrap().path()).unwrap();
        if let Some((&flags, rest)) = data.split_first() {
            check(rest.to_vec(), flags & 1 != 0);
        }
    }
}
