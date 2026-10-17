//! Differential tests: images written by `qemu-img` from a generated raw
//! disk must read back byte-for-byte identical through pm-image. Skipped
//! (with a message) when `qemu-img` is not installed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

mod common;

use common::{MIB, Rng, assert_same, convert, fill, pattern, qemu, qemu_img, write};
use pm_core::ReadAt;
use pm_image::{ImageError, ImageFormat, open};
use rstest::rstest;

fn roundtrip(fmt: &str, opts: &[&str], want: ImageFormat, ext: &str) {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let raw = pattern();
    let raw_path = write(dir.path(), "src.raw", &raw);
    let img_path = convert(&raw_path, dir.path(), &format!("disk.{ext}"), fmt, opts);
    let img = open(&img_path).unwrap_or_else(|e| panic!("{fmt} {opts:?}: {e}"));
    assert_eq!(img.info.format, want);
    assert_same(&img, &raw, &format!("{fmt} {opts:?}"));
    // Reopening from the stored path list (as a case does) gives the same.
    let again = pm_image::open_paths(&img.info.paths).unwrap();
    assert_eq!(again.info.format, want);
    assert_eq!(again.source().read_vec_at(0, 4096).unwrap(), raw[..4096]);
}

#[rstest]
#[case::dynamic(&["subformat=dynamic", "force_size=on"])]
#[case::fixed(&["subformat=fixed", "force_size=on"])]
fn vhd(#[case] opts: &[&str]) {
    roundtrip("vpc", opts, ImageFormat::Vhd, "vhd");
}

#[rstest]
#[case::dynamic(&["subformat=dynamic"])]
#[case::dynamic_1m_blocks(&["subformat=dynamic", "block_size=1M"])]
#[case::dynamic_no_zero_state(&["subformat=dynamic", "block_state_zero=off", "block_size=1M"])]
#[case::fixed(&["subformat=fixed"])]
fn vhdx(#[case] opts: &[&str]) {
    roundtrip("vhdx", opts, ImageFormat::Vhdx, "vhdx");
}

#[rstest]
#[case::monolithic_sparse(&["subformat=monolithicSparse"])]
#[case::zeroed_grain(&["subformat=monolithicSparse", "zeroed_grain=on"])]
#[case::monolithic_flat(&["subformat=monolithicFlat"])]
#[case::split_sparse(&["subformat=twoGbMaxExtentSparse"])]
#[case::split_flat(&["subformat=twoGbMaxExtentFlat"])]
#[case::stream_optimized(&["subformat=streamOptimized"])]
#[case::compat6(&["subformat=monolithicSparse", "compat6=on"])]
fn vmdk(#[case] opts: &[&str]) {
    roundtrip("vmdk", opts, ImageFormat::Vmdk, "vmdk");
}

#[rstest]
#[case::v2(&["compat=0.10"])]
#[case::v3(&["compat=1.1"])]
#[case::small_clusters(&["cluster_size=4096"])]
#[case::large_clusters(&["cluster_size=2M"])]
#[case::extended_l2(&["extended_l2=on", "cluster_size=128k"])]
#[case::lazy_refcounts(&["lazy_refcounts=on"])]
fn qcow2(#[case] opts: &[&str]) {
    roundtrip("qcow2", opts, ImageFormat::Qcow, "qcow2");
}

#[rstest]
#[case::deflate("zlib", "64k")]
#[case::zstd("zstd", "64k")]
#[case::deflate_small("zlib", "4k")]
#[case::zstd_large("zstd", "1M")]
fn qcow2_compressed(#[case] kind: &str, #[case] cluster: &str) {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut raw = pattern();
    // Compressible text as well as random bytes.
    for (i, b) in raw[8 * MIB..8 * MIB + 200_000].iter_mut().enumerate() {
        *b = b"postmortem "[i % 11];
    }
    let raw_path = write(dir.path(), "src.raw", &raw);
    let out = dir.path().join("c.qcow2");
    let opts = format!("compression_type={kind},cluster_size={cluster}");
    qemu(&[
        "convert",
        "-c",
        "-f",
        "raw",
        "-O",
        "qcow2",
        "-o",
        &opts,
        raw_path.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    let img = open(&out).unwrap();
    assert_same(&img, &raw, &format!("qcow2 compressed {kind} {cluster}"));
}

/// Child images made with `convert -B`: only clusters that differ from the
/// backing file are stored, the rest must come from the parent.
#[rstest]
#[case::qcow2_over_qcow2("qcow2", "qcow2", &[])]
#[case::qcow2_over_raw("qcow2", "raw", &[])]
#[case::qcow2_v2_over_qcow2("qcow2", "qcow2", &["compat=0.10"])]
#[case::vmdk_delta("vmdk", "vmdk", &[])]
fn backing_chain(#[case] fmt: &str, #[case] base_fmt: &str, #[case] opts: &[&str]) {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let base = pattern();
    let base_raw = write(dir.path(), "base.raw", &base);
    let base_name = format!("base.{base_fmt}");
    if base_fmt != "raw" {
        convert(&base_raw, dir.path(), &base_name, base_fmt, &[]);
    }
    let mut child = base.clone();
    fill(&mut child, 512..1536, 77);
    fill(&mut child, 5 * MIB..5 * MIB + 3 * 65_536 + 100, 78);
    child[11 * MIB..11 * MIB + 4096].fill(0); // zeroing over parent data
    fill(&mut child, 4 * MIB - 300..4 * MIB + 300, 79);
    let child_raw = write(dir.path(), "child.raw", &child);
    let out = dir.path().join(format!("child.{fmt}"));
    let mut args = vec![
        "convert",
        "-f",
        "raw",
        "-O",
        fmt,
        "-B",
        base_name.as_str(),
        "-F",
        base_fmt,
    ];
    let joined = opts.join(",");
    if !opts.is_empty() {
        args.push("-o");
        args.push(&joined);
    }
    args.push(child_raw.to_str().unwrap());
    args.push(out.to_str().unwrap());
    // The backing name is relative: pm-image resolves it against the
    // child's directory, not the working directory.
    qemu(&args);
    let img = open(&out).unwrap();
    assert!(img.info.paths.len() >= 2, "{:?}", img.info.paths);
    assert_same(&img, &child, &format!("{fmt} over {base_fmt}"));

    // Moving the parent away gives a clear error naming it.
    let moved = dir.path().join("elsewhere");
    std::fs::create_dir(&moved).unwrap();
    std::fs::rename(dir.path().join(&base_name), moved.join(&base_name)).unwrap();
    if base_fmt == "raw" {
        // The raw base was also the source; nothing else to check.
        return;
    }
    let err = open(&out).unwrap_err();
    assert!(matches!(err, ImageError::MissingParent { .. }), "{err}");
    assert!(err.to_string().contains(&base_name), "{err}");
}

/// Disks larger than 2 GiB split into several extents; check reads across
/// extent boundaries without reading every byte.
#[rstest]
#[case::split_sparse("twoGbMaxExtentSparse")]
#[case::split_flat("twoGbMaxExtentFlat")]
fn vmdk_split_extents(#[case] sub: &str) {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let size: u64 = 4 * 1024 * MIB as u64 + 512 * MIB as u64;
    let raw_path = dir.path().join("big.raw");
    let f = std::fs::File::create(&raw_path).unwrap();
    f.set_len(size).unwrap();
    // Data islands around every 2 GiB boundary (qemu's extent size is
    // 2 GiB minus 64 KiB) and at the end.
    let mut islands = Vec::new();
    for at in [
        0u64,
        2047 * MIB as u64,
        2048 * MIB as u64 - 65_536 - 777,
        4095 * MIB as u64,
        size - 100_000,
    ] {
        let mut d = vec![0u8; 200_000.min((size - at) as usize)];
        let n = d.len();
        fill(&mut d, 0..n, at);
        std::os::unix::fs::FileExt::write_all_at(&f, &d, at).unwrap();
        islands.push((at, d));
    }
    drop(f);
    let img_path = convert(
        &raw_path,
        dir.path(),
        "big.vmdk",
        "vmdk",
        &[&format!("subformat={sub}")],
    );
    let img = open(&img_path).unwrap();
    assert!(img.info.paths.len() >= 4, "{:?}", img.info.paths);
    assert_eq!(img.info.size, size);
    let src = img.source();
    for (at, d) in &islands {
        let lo = at.saturating_sub(5000);
        let hi = (at + d.len() as u64 + 5000).min(size);
        let got = src.read_vec_at(lo, (hi - lo) as usize).unwrap();
        let mut want = vec![0u8; got.len()];
        let s = (at - lo) as usize;
        want[s..s + d.len()].copy_from_slice(d);
        assert!(got == want, "{sub}: island at {at}");
    }
    let mut r = Rng(5);
    for _ in 0..50 {
        let at = r.below(size);
        let mut b = vec![1u8; 4096];
        let n = src.read_at(at, &mut b).unwrap();
        assert_eq!(n as u64, 4096.min(size - at));
    }
}

#[test]
fn encrypted_qcow2_is_refused() {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("enc.qcow2");
    qemu(&[
        "create",
        "--object",
        "secret,id=sec0,data=hunter2",
        "-f",
        "qcow2",
        "-o",
        "encrypt.format=luks,encrypt.key-secret=sec0,encrypt.iter-time=10",
        out.to_str().unwrap(),
        "4M",
    ]);
    let err = open(&out).unwrap_err();
    assert!(matches!(err, ImageError::Encrypted { .. }), "{err}");
    assert!(err.to_string().contains("encrypted"), "{err}");
}

/// The Sleuth Kit's `img_cat` (libvmdk / libvhdi) as a second oracle.
#[rstest]
#[case("vpc", &["subformat=dynamic", "force_size=on"], "vhd")]
#[case("vmdk", &["subformat=monolithicSparse"], "vmdk")]
#[case("vmdk", &["subformat=twoGbMaxExtentSparse"], "vmdk")]
#[case("vmdk", &["subformat=streamOptimized"], "vmdk")]
fn agrees_with_tsk_img_cat(#[case] fmt: &str, #[case] opts: &[&str], #[case] ext: &str) {
    if qemu_img().is_none() {
        return;
    }
    if std::process::Command::new("img_cat")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("skipping: img_cat (The Sleuth Kit) is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let raw = pattern();
    let raw_path = write(dir.path(), "src.raw", &raw);
    let img_path = convert(&raw_path, dir.path(), &format!("d.{ext}"), fmt, opts);
    let out = std::process::Command::new("img_cat")
        .arg(&img_path)
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!(
            "skipping: img_cat cannot read {fmt}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let img = open(&img_path).unwrap();
    let ours = img
        .source()
        .read_vec_at(0, usize::try_from(img.info.size).unwrap())
        .unwrap();
    assert_eq!(ours.len(), out.stdout.len(), "{fmt}: media size vs img_cat");
    assert!(ours == out.stdout, "{fmt}: content differs from img_cat");
}

#[test]
fn sniffs_every_qemu_format() {
    if qemu_img().is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let raw_path = write(dir.path(), "s.raw", &vec![0u8; 2 * MIB]);
    for (fmt, opts, want) in [
        ("vpc", "subformat=fixed,force_size=on", ImageFormat::Vhd),
        ("vpc", "subformat=dynamic", ImageFormat::Vhd),
        ("vhdx", "subformat=dynamic", ImageFormat::Vhdx),
        ("vmdk", "subformat=monolithicSparse", ImageFormat::Vmdk),
        ("vmdk", "subformat=monolithicFlat", ImageFormat::Vmdk),
        ("qcow2", "compat=1.1", ImageFormat::Qcow),
    ] {
        let p = convert(
            &raw_path,
            dir.path(),
            &format!("x-{fmt}-{}", opts.len()),
            fmt,
            &[opts],
        );
        let src = pm_core::io::FileSource::open(&p).unwrap();
        assert_eq!(pm_image::sniff(&src), want, "{fmt} {opts}");
    }
}
