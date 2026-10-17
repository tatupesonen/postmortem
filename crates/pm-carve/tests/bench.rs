//! Carving throughput (run with `cargo test --release -p pm-carve --test
//! bench -- --ignored --nocapture`; numbers go to `docs/PERF.md`).
#![allow(
    clippy::unwrap_used,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "benchmark"
)]

use std::time::Instant;

use pm_carve::{Options, carve};
use pm_core::io::{FileSource, MemSource};
use pm_testkit::archives as ak;
use pm_testkit::media;

/// `mib` MiB of pseudo-random data with a JPEG, a zip, a PDF and an MP3
/// at the start of every MiB.
fn image(mib: usize) -> (Vec<u8>, usize) {
    let jpg = ak::jpeg(256, 256);
    let zip = ak::zip(&[("a.txt", &b"text ".repeat(2000)[..])]);
    let pdf = ak::pdf(&[], &[ak::PdfImage::Jpeg(jpg.clone())]);
    let mp3 = media::mp3(200, true);
    let mut out = Vec::with_capacity(mib << 20);
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let mut files = 0;
    for _ in 0..mib {
        let start = out.len();
        for f in [&jpg, &zip, &pdf, &mp3] {
            out.extend(f);
            out.resize(out.len().next_multiple_of(512), 0);
            files += 1;
        }
        while out.len() < start + (1 << 20) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend(x.to_le_bytes());
        }
        out.truncate(start + (1 << 20));
    }
    (out, files)
}

#[test]
#[ignore = "benchmark"]
fn carving_throughput() {
    let mib = 512;
    let (img, files) = image(mib);
    let src = MemSource::new(img.clone());
    let t = Instant::now();
    let mut n = 0u64;
    carve(&src, &Options::default(), &|| false, &mut |_| n += 1).unwrap();
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "in memory: {mib} MiB, {n} files carved ({files} planted) in {secs:.2} s = {:.0} MiB/s",
        mib as f64 / secs
    );
    assert_eq!(n, files as u64);

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("unalloc.bin");
    std::fs::write(&p, &img).unwrap();
    drop(img);
    let src = FileSource::open(&p).unwrap();
    let t = Instant::now();
    let mut n = 0u64;
    carve(&src, &Options::default(), &|| false, &mut |_| n += 1).unwrap();
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "file (page cache): {mib} MiB, {n} files in {secs:.2} s = {:.0} MiB/s",
        mib as f64 / secs
    );
}
