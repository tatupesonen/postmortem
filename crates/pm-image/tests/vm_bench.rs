//! Sequential read throughput of each virtual disk format (ignored; run
//! in release with `cargo test --release -p pm-image --test vm_bench --
//! --ignored --nocapture`). Numbers go to docs/PERF.md.
//!
//! A 1 GiB raw disk (¾ incompressible data, ¼ repetitive text, a zero gap)
//! is converted with `qemu-img`, then read start to end in 1 MiB requests.
//! Files are freshly written, so this measures reader overhead over a warm
//! page cache, not disk speed. RSS growth during the read is reported to
//! show memory does not scale with image size.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    reason = "benchmark code"
)]

mod common;

use std::time::Instant;

use common::{MIB, Rng, convert, qemu, qemu_img};
use pm_core::ReadAt;

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn time_read(src: &dyn ReadAt) -> (f64, u64) {
    let mut buf = vec![0u8; MIB];
    let before = rss_kib();
    let t = Instant::now();
    let mut off = 0;
    let mut sum = 0u64;
    while off < src.size() {
        let n = src.read_at(off, &mut buf).unwrap();
        assert!(n > 0);
        sum = sum.wrapping_add(u64::from(buf[n - 1]));
        off += n as u64;
    }
    let secs = t.elapsed().as_secs_f64();
    std::hint::black_box(sum);
    (
        src.size() as f64 / MIB as f64 / secs,
        rss_kib().saturating_sub(before),
    )
}

#[test]
#[ignore = "benchmark"]
fn sequential_read_throughput() {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let size = 1024 * MIB;
    let raw_path = dir.path().join("bench.raw");
    {
        let mut data = vec![0u8; size];
        let mut r = Rng(42);
        for chunk in data[..size / 4 * 3].chunks_mut(8) {
            let v = r.next().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        for (i, b) in data[size / 4 * 3..size - 64 * MIB].iter_mut().enumerate() {
            *b = b"postmortem benchmark text "[i % 26];
        }
        std::fs::write(&raw_path, &data).unwrap();
    }
    let raw = pm_image::open(&raw_path).unwrap();
    let (mbps, rss) = time_read(&*raw.source());
    eprintln!("raw file                  {mbps:8.0} MiB/s  rss +{rss} KiB");
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "vhd-dynamic",
            "vpc",
            &["subformat=dynamic", "force_size=on"],
        ),
        ("vhd-fixed", "vpc", &["subformat=fixed", "force_size=on"]),
        ("vhdx-dynamic", "vhdx", &["subformat=dynamic"]),
        ("vmdk-sparse", "vmdk", &["subformat=monolithicSparse"]),
        (
            "vmdk-split-sparse",
            "vmdk",
            &["subformat=twoGbMaxExtentSparse"],
        ),
        ("vmdk-flat", "vmdk", &["subformat=monolithicFlat"]),
        ("vmdk-stream", "vmdk", &["subformat=streamOptimized"]),
        ("qcow2", "qcow2", &[]),
    ];
    for (label, fmt, opts) in cases {
        let p = convert(&raw_path, dir.path(), &format!("{label}.img"), fmt, opts);
        let img = pm_image::open(&p).unwrap();
        let (mbps, rss) = time_read(&*img.source());
        eprintln!("{label:25} {mbps:8.0} MiB/s  rss +{rss} KiB");
        std::fs::remove_file(&p).ok();
    }
    for kind in ["zlib", "zstd"] {
        let p = dir.path().join(format!("c-{kind}.qcow2"));
        qemu(&[
            "convert",
            "-c",
            "-f",
            "raw",
            "-O",
            "qcow2",
            "-o",
            &format!("compression_type={kind}"),
            raw_path.to_str().unwrap(),
            p.to_str().unwrap(),
        ]);
        let img = pm_image::open(&p).unwrap();
        let (mbps, rss) = time_read(&*img.source());
        eprintln!("qcow2-compressed-{kind:8} {mbps:8.0} MiB/s  rss +{rss} KiB");
    }
}
