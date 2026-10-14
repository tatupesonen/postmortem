//! EWF reader tests: a committed fixture plus a live matrix against
//! libewf's `ewfacquire` (skipped when it is not installed).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use md5::{Digest, Md5};
use pm_core::ReadAt;
use pm_image::{HashAlgo, ImageFormat, open};
use rstest::rstest;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn md5_of(src: &dyn ReadAt) -> String {
    let mut h = Md5::new();
    let mut off = 0u64;
    let mut buf = vec![0u8; 100_003]; // deliberately not chunk-aligned
    while off < src.size() {
        let n = src.read_at(off, &mut buf).unwrap();
        assert!(n > 0, "short read at {off}");
        h.update(&buf[..n]);
        off += n as u64;
    }
    pm_core::decode::to_hex(&h.finalize())
}

#[test]
fn small_fixture_reads_and_verifies() {
    let img = open(fixture("ewf/small.E01")).unwrap();
    assert_eq!(img.info.format, ImageFormat::Ewf);
    assert_eq!(img.info.size, 262_144);
    assert_eq!(img.info.sector_size, 512);
    let md5 = img
        .info
        .stored_hashes
        .iter()
        .find(|h| h.0 == HashAlgo::Md5)
        .unwrap();
    assert_eq!(md5.1, "b7d9562dc88251034f015fcb757c7ec7");
    let sha1 = img
        .info
        .stored_hashes
        .iter()
        .find(|h| h.0 == HashAlgo::Sha1)
        .unwrap();
    assert_eq!(sha1.1, "9c6a5079404c0af11ca2a0cc382541de694bd1dd");
    assert_eq!(
        md5_of(&*img.source()),
        md5.1,
        "content must hash to the acquisition MD5"
    );

    // Block 1 (64 KiB..128 KiB) is ASCII text in the generator.
    let text = img.source().read_vec_at(65_536, 31).unwrap();
    assert_eq!(text, b"postmortem fixture text block. ");

    let meta = |k: &str| {
        img.info
            .metadata
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(meta("Case number"), Some("CASE-42"));
    assert_eq!(
        meta("Examiner"),
        Some("Examiner Ä"),
        "UTF-16 header2 preferred over ASCII header"
    );
    assert_eq!(meta("Evidence number"), Some("EV-7"));
    assert_eq!(meta("Description"), Some("pm fixture"));
    assert_eq!(meta("Notes"), Some("notes here"));
    assert!(meta("Acquisition date").unwrap().ends_with("UTC"));
}

#[test]
fn unaligned_reads_across_chunk_boundaries() {
    let img = open(fixture("ewf/small.E01")).unwrap();
    let src = img.source();
    let whole = src.read_vec_at(0, src.size() as usize).unwrap();
    for (off, len) in [
        (0, 1),
        (32_767, 2),
        (32_000, 70_000),
        (262_143, 1),
        (131_000, 131_144),
    ] {
        assert_eq!(
            src.read_vec_at(off, len).unwrap(),
            whole[off as usize..off as usize + len]
        );
    }
    assert!(src.read_vec_at(262_144, 1).is_err());
}

#[test]
fn truncated_segment_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let data = std::fs::read(fixture("ewf/small.E01")).unwrap();
    for cut in [13, 100, 900, 2000, 40_000, 69_100, data.len() - 10] {
        let p = dir.path().join(format!("cut{cut}.E01"));
        std::fs::write(&p, &data[..cut]).unwrap();
        if let Ok(img) = open(&p) {
            let src = img.source();
            let _ = src.read_vec_at(0, src.size() as usize);
        }
    }
}

/// Deterministic pseudo-random + compressible content.
fn source_bytes(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|i| {
            if (i / 40_000) % 3 == 0 {
                (i % 251) as u8
            } else {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            }
        })
        .collect()
}

#[rstest]
#[case("encase6", "deflate:best", 64)]
#[case("encase6", "none", 64)]
#[case("encase6", "deflate:fast", 16)]
#[case("encase5", "deflate:best", 64)]
#[case("encase7", "deflate:best", 1024)]
#[case("ftk", "deflate:best", 64)]
#[case("linen6", "deflate:fast", 64)]
#[case("smart", "deflate:best", 64)]
#[case("ewf", "deflate:best", 64)]
fn matches_ewfacquire_output(#[case] format: &str, #[case] compression: &str, #[case] spc: u32) {
    if pm_testkit::tsk::tool("ewfacquire").is_none() {
        eprintln!("ewfacquire not available; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("src.raw");
    let data = source_bytes(2_600_000 + 1234); // not a multiple of the chunk size
    std::fs::write(&raw, &data).unwrap();
    let target = dir.path().join("img");
    let status = Command::new("ewfacquire")
        .args([
            "-u",
            "-q",
            "-f",
            format,
            "-c",
            compression,
            "-b",
            &spc.to_string(),
            "-S",
            "1MiB",
            "-t",
        ])
        .arg(&target)
        .arg(&raw)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let first = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("e01") || e.eq_ignore_ascii_case("s01"))
        })
        .expect("first segment");
    let img = open(&first).unwrap();
    // ewfacquire drops a trailing partial sector.
    let size = img.info.size as usize;
    assert_eq!(size, data.len() / 512 * 512);
    let got = img.source().read_vec_at(0, size).unwrap();
    assert_eq!(
        &got[..],
        &data[..size],
        "{format}/{compression}/{spc}: content differs"
    );
    if format != "smart" && format != "ewf" {
        assert!(
            img.info.paths.len() >= 2,
            "expected multiple segments: {:?}",
            img.info.paths
        );
    }
    // ewfacquire 20140817 hashes the whole source, including the partial
    // sector it does not store.
    if let Some(md5) = img.info.stored_hashes.iter().find(|h| h.0 == HashAlgo::Md5) {
        let src = pm_core::io::MemSource::new(data);
        assert_eq!(md5_of(&src), md5.1);
    }
}
