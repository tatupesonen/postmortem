#![allow(clippy::unwrap_used, clippy::print_stderr, reason = "benchmark")]

//! `cargo test -p pm-encryption --release --test bench -- --ignored --nocapture`

use pm_core::io::MemSource;

#[test]
#[ignore = "benchmark"]
fn bench_entropy() {
    let mut x = 0x9E37_79B9u32;
    let data: Vec<u8> = (0..256 << 20)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect();
    let src = MemSource::new(data);
    let start = std::time::Instant::now();
    let e = pm_encryption::entropy::entropy(&src, &|| false).unwrap();
    let secs = start.elapsed().as_secs_f64();
    eprintln!("entropy {e:.6}: {:.0} MB/s", 256.0 * 1.048_576 / secs);
}
