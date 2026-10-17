//! Extraction throughput (run with `cargo test --release -p pm-archive
//! --test bench -- --ignored --nocapture`; numbers go to `docs/PERF.md`).
#![allow(
    clippy::unwrap_used,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    reason = "benchmark"
)]

use std::time::Instant;

use pm_archive::{Entry, Format, MemberEnd, Options, Sink};
use pm_core::io::MemSource;
use pm_testkit::archives::{self as ak, Method, ZipBuilder, ZipEntry};

#[derive(Default)]
struct Count {
    members: u64,
    bytes: u64,
}

impl Sink for Count {
    fn begin(&mut self, _: &Entry) -> std::io::Result<bool> {
        self.members += 1;
        Ok(true)
    }
    fn write(&mut self, d: &[u8]) -> std::io::Result<()> {
        self.bytes += d.len() as u64;
        Ok(())
    }
    fn end(&mut self, _: &MemberEnd) -> std::io::Result<()> {
        Ok(())
    }
}

fn time(label: &str, format: Format, data: Vec<u8>) {
    let packed = data.len();
    let src = MemSource::new(data);
    let t = Instant::now();
    let mut c = Count::default();
    pm_archive::extract(format, &src, &Options::default(), &mut c).unwrap();
    let s = t.elapsed().as_secs_f64();
    eprintln!(
        "{label}: {} members, {:.0} MiB decoded from {:.1} MiB in {s:.2} s = {:.0} MiB/s, {:.0} members/s",
        c.members,
        c.bytes as f64 / 1_048_576.0,
        packed as f64 / 1_048_576.0,
        c.bytes as f64 / 1_048_576.0 / s,
        c.members as f64 / s
    );
}

/// Compressible but not trivial text.
fn text(len: usize) -> Vec<u8> {
    let mut x = 1u64;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            b"abcdefghij klmnop\n"[(x >> 59) as usize % 18]
        })
        .collect()
}

#[test]
#[ignore = "benchmark"]
fn extraction_throughput() {
    let big = text(256 << 20);
    let mut z = ZipBuilder::new();
    z.add(ZipEntry::new("big.txt", big.clone()));
    time("zip deflate 256 MiB member", Format::Zip, z.build());
    let mut z = ZipBuilder::new();
    z.add(ZipEntry::new("big.bin", big.clone()).method(Method::Stored));
    time("zip stored 256 MiB member", Format::Zip, z.build());
    let mut z = ZipBuilder::new();
    for i in 0..20_000 {
        z.file(&format!("d{}/f{i}.txt", i % 100), b"small file contents");
    }
    time("zip 20 000 small members", Format::Zip, z.build());
    time("gzip 256 MiB", Format::Gzip, ak::gzip(None, &big));
    let small: Vec<(String, Vec<u8>)> = (0..2000)
        .map(|i| (format!("f{i}.txt"), text(4096)))
        .collect();
    let refs: Vec<(&str, &[u8])> = small.iter().map(|(n, d)| (n.as_str(), &d[..])).collect();
    time(
        "7z solid 2000 x 4 KiB",
        Format::SevenZip,
        ak::sevenz(&refs, &[], true, None, false),
    );
}
