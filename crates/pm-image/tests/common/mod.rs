//! Shared helpers for the virtual-disk differential tests.
#![allow(dead_code, reason = "each test binary uses a subset")]

use std::path::{Path, PathBuf};
use std::process::Command;

use pm_core::ReadAt;
use pm_image::Image;

pub(crate) const MIB: usize = 1 << 20;

/// Small deterministic PRNG (xorshift64*).
pub(crate) struct Rng(pub u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Fills `range` of `v` with pseudo-random bytes.
pub(crate) fn fill(v: &mut [u8], range: std::ops::Range<usize>, seed: u64) {
    let mut r = Rng(seed | 1);
    for b in &mut v[range] {
        *b = r.next() as u8;
    }
}

/// A 12 MiB raw disk with data islands separated by zero (sparse) runs:
/// a full first MiB, a single-sector island, a run crossing block/cluster
/// boundaries at 4 MiB, scattered sectors in one block and a tail.
pub(crate) fn pattern() -> Vec<u8> {
    let size = 12 * MIB;
    let mut v = vec![0u8; size];
    fill(&mut v, 0..MIB, 1);
    fill(&mut v, 3 * MIB + 512..3 * MIB + 1024, 2);
    fill(&mut v, 4 * MIB - 70_000..4 * MIB + 70_001, 3);
    for k in 0..20 {
        let at = 6 * MIB + k * 37 * 512;
        fill(&mut v, at..at + 512, 10 + k as u64);
    }
    fill(&mut v, size - 300 * 1024..size - 512, 4);
    v
}

/// Compares every byte of `img` with `raw`: sequential reads in odd-sized
/// chunks, then random reads of random lengths crossing grain, cluster and
/// block boundaries.
pub(crate) fn assert_same(img: &Image, raw: &[u8], what: &str) {
    let src = img.source();
    assert_eq!(src.size(), raw.len() as u64, "{what}: size");
    assert_eq!(img.info.size, raw.len() as u64, "{what}: info size");
    let mut off = 0usize;
    let mut buf = vec![0u8; 100_003];
    while off < raw.len() {
        let n = src.read_at(off as u64, &mut buf).unwrap();
        assert!(n > 0, "{what}: short read at {off}");
        assert!(
            buf[..n] == raw[off..off + n],
            "{what}: mismatch in [{off}, {})",
            off + n
        );
        off += n;
    }
    let mut r = Rng(0xDEAD_BEEF);
    for _ in 0..400 {
        let at = r.below(raw.len() as u64) as usize;
        let len = 1 + r.below(300_000) as usize;
        let mut b = vec![0xAAu8; len];
        let n = src.read_at(at as u64, &mut b).unwrap();
        let end = (at + len).min(raw.len());
        assert_eq!(n, end - at, "{what}: read length at {at}");
        assert!(b[..n] == raw[at..end], "{what}: random read at {at}+{len}");
    }
    let mut b = [0u8; 16];
    assert_eq!(src.read_at(raw.len() as u64, &mut b).unwrap(), 0);
    assert_eq!(src.read_at(u64::MAX, &mut b).unwrap(), 0);
}

/// `qemu-img` if installed.
pub(crate) fn qemu_img() -> Option<PathBuf> {
    let ok = Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        Some(PathBuf::from("qemu-img"))
    } else {
        eprintln!("skipping: qemu-img is not installed");
        None
    }
}

/// Runs `qemu-img` with `args`, panicking with its stderr on failure.
pub(crate) fn qemu(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(
        out.status.success(),
        "qemu-img {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Writes `raw` to `dir/name` and returns the path.
pub(crate) fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

/// Converts the raw file `raw` to `fmt` with options `opts` at `dir/name`.
pub(crate) fn convert(raw: &Path, dir: &Path, name: &str, fmt: &str, opts: &[&str]) -> PathBuf {
    let out = dir.join(name);
    let mut args = vec!["convert", "-f", "raw", "-O", fmt];
    let joined = opts.join(",");
    if !opts.is_empty() {
        args.push("-o");
        args.push(&joined);
    }
    let r = raw.to_str().unwrap();
    let o = out.to_str().unwrap();
    args.push(r);
    args.push(o);
    qemu(&args);
    out
}
