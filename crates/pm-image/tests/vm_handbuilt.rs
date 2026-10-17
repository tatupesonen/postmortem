//! Virtual disks built in Rust (pm-testkit `vdisk`): differencing VHD and
//! VHDX (which `qemu-img` cannot write), parent resolution, corrupt headers
//! and "never panics" properties.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

mod common;

use std::sync::Arc;

use common::{MIB, assert_same, fill, pattern, write};
use pm_core::io::MemSource;
use pm_core::{ReadAt, SharedSource};
use pm_image::{ImageError, ImageFormat, open, qcow2, vhd, vhdx, vmdk};
use pm_testkit::vdisk::{
    VhdParent, VhdxOptions, vhd_differencing, vhd_dynamic, vhd_fixed, vhdx, vmdk_sparse,
};
use proptest::prelude::*;

/// Child content: the parent with some sectors replaced, and which sectors
/// the child stores.
fn child_of(base: &[u8]) -> (Vec<u8>, impl Fn(u64) -> bool + Clone) {
    let present = |s: u64| {
        (1..3).contains(&s)               // sectors 1-2
            || (4096..4096 + 4096).contains(&s) // a full 2 MiB block
            || (8192 + 100..8192 + 140).contains(&s) // inside a block
            || s % 1000 == 7 // scattered
    };
    let mut child = base.to_vec();
    for s in 0..(base.len() / 512) as u64 {
        if present(s) {
            let a = s as usize * 512;
            fill(&mut child, a..a + 512, 1000 + s);
        }
    }
    // Every 50th stored sector is zero in the child over parent data.
    for s in (0..(base.len() / 512) as u64)
        .filter(|s| present(*s))
        .step_by(50)
    {
        let a = s as usize * 512;
        child[a..a + 512].fill(0);
    }
    (child, present)
}

/// The bytes the builder stores: child content where present, garbage
/// elsewhere (which a correct reader must never return).
fn stored(child: &[u8], present: &dyn Fn(u64) -> bool) -> Vec<u8> {
    let mut v = child.to_vec();
    for s in 0..(v.len() / 512) as u64 {
        if !present(s) {
            let a = s as usize * 512;
            v[a..a + 512].fill(0xEE);
        }
    }
    v
}

#[test]
fn vhd_fixed_and_dynamic() {
    let dir = tempfile::tempdir().unwrap();
    let raw = pattern();
    let p = write(dir.path(), "f.vhd", &vhd_fixed(&raw));
    let img = open(&p).unwrap();
    assert_eq!(img.info.format, ImageFormat::Vhd);
    assert_same(&img, &raw, "fixed");
    for bs in [512 * 1024u32, 2 * MIB as u32] {
        let p = write(dir.path(), "d.vhd", &vhd_dynamic(&raw, bs));
        let img = open(&p).unwrap();
        assert_same(&img, &raw, &format!("dynamic {bs}"));
        assert!(
            img.info
                .metadata
                .iter()
                .any(|(k, v)| k == "Disk type" && v == "dynamic")
        );
    }
}

#[test]
fn vhd_differencing_reads_parent_for_absent_sectors() {
    let dir = tempfile::tempdir().unwrap();
    let base = pattern();
    std::fs::create_dir(dir.path().join("parents")).unwrap();
    write(
        &dir.path().join("parents"),
        "base.vhd",
        &vhd_dynamic(&base, 2 * MIB as u32),
    );
    let (child, present) = child_of(&base);
    let disk = vhd_differencing(
        &stored(&child, &present),
        2 * MIB as u32,
        &present,
        &VhdParent {
            relative: r".\parents\base.vhd",
            absolute: r"D:\Hyper-V\parents\base.vhd",
            parent_id: [0x22; 16],
        },
    );
    let p = write(dir.path(), "child.vhd", &disk);
    let img = open(&p).unwrap();
    assert_eq!(img.info.paths.len(), 2);
    assert!(img.info.paths[1].ends_with("parents/base.vhd"));
    assert_same(&img, &child, "differencing");

    // Only the absolute locator's file name matches next to the child.
    std::fs::rename(
        dir.path().join("parents/base.vhd"),
        dir.path().join("base.vhd"),
    )
    .unwrap();
    assert_same(&open(&p).unwrap(), &child, "moved parent");

    // No parent at all: a clear error naming the reference.
    std::fs::remove_file(dir.path().join("base.vhd")).unwrap();
    let err = open(&p).unwrap_err();
    assert!(matches!(err, ImageError::MissingParent { .. }), "{err}");
    assert!(err.to_string().contains("base.vhd"), "{err}");

    // In-memory: absent sectors are zeros without a parent.
    let (src, h) = vhd::VhdSource::from_source(Arc::new(MemSource::new(disk)), None).unwrap();
    assert_eq!(h.disk_type, vhd::DiskType::Differencing);
    let b = src.read_vec_at(0, 1536).unwrap();
    assert!(b[..512].iter().all(|&x| x == 0));
    assert_eq!(b[512..1536], child[512..1536]);
}

#[test]
fn circular_parents_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let raw = vec![1u8; MIB];
    let all = |_| true;
    for (me, other) in [("a.vhd", "b.vhd"), ("b.vhd", "a.vhd")] {
        let d = vhd_differencing(
            &raw,
            2 * MIB as u32,
            &all,
            &VhdParent {
                relative: other,
                absolute: other,
                parent_id: [0; 16],
            },
        );
        write(dir.path(), me, &d);
    }
    let err = open(dir.path().join("a.vhd")).unwrap_err();
    assert!(matches!(err, ImageError::ParentChainTooDeep(_)), "{err}");
}

#[test]
fn vhdx_dynamic_and_sector_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let raw = pattern();
    for (bs, lss) in [
        (MIB as u32, 512u32),
        (4 * MIB as u32, 4096),
        (32 * MIB as u32, 512),
    ] {
        let o = VhdxOptions {
            block_size: bs,
            logical_sector: lss,
            ..VhdxOptions::default()
        };
        let p = write(dir.path(), "d.vhdx", &vhdx(&raw, &o));
        let img = open(&p).unwrap();
        assert_eq!(img.info.format, ImageFormat::Vhdx);
        assert_eq!(img.info.sector_size, lss);
        assert_same(&img, &raw, &format!("vhdx {bs} {lss}"));
        assert!(!img.info.metadata.iter().any(|(k, _)| k == "Warning"));
    }
}

#[test]
fn vhdx_dirty_log_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let raw = pattern();
    let o = VhdxOptions {
        dirty_log: true,
        ..VhdxOptions::default()
    };
    let p = write(dir.path(), "dirty.vhdx", &vhdx(&raw, &o));
    let img = open(&p).unwrap();
    assert_same(&img, &raw, "dirty");
    let (_, w) = img
        .info
        .metadata
        .iter()
        .find(|(k, _)| k == "Warning")
        .unwrap();
    assert!(w.contains("log not replayed"), "{w}");
}

#[test]
fn vhdx_differencing_uses_sector_bitmaps() {
    let dir = tempfile::tempdir().unwrap();
    let base = pattern();
    write(
        dir.path(),
        "base.vhdx",
        &vhdx(&base, &VhdxOptions::default()),
    );
    let (child, present) = child_of(&base);
    let o = VhdxOptions {
        parent: Some((&present, r".\base.vhdx")),
        ..VhdxOptions::default()
    };
    let p = write(
        dir.path(),
        "child.vhdx",
        &vhdx(&stored(&child, &present), &o),
    );
    let img = open(&p).unwrap();
    assert_eq!(img.info.paths.len(), 2);
    assert_same(&img, &child, "vhdx differencing");
    std::fs::remove_file(dir.path().join("base.vhdx")).unwrap();
    let err = open(&p).unwrap_err();
    assert!(matches!(err, ImageError::MissingParent { .. }), "{err}");
}

#[test]
fn vmdk_sparse_and_delta_link() {
    let dir = tempfile::tempdir().unwrap();
    let base = pattern();
    let p = write(
        dir.path(),
        "base.vmdk",
        &vmdk_sparse(&base, "base.vmdk", None),
    );
    assert_same(&open(&p).unwrap(), &base, "vmdk sparse");
    // A renamed monolithic file still finds its own (embedded) extent.
    std::fs::copy(&p, dir.path().join("evidence-copy.vmdk")).unwrap();
    assert_same(
        &open(dir.path().join("evidence-copy.vmdk")).unwrap(),
        &base,
        "renamed",
    );
    let mut child = base.clone();
    fill(&mut child, 2 * MIB..2 * MIB + 65_536, 9);
    // Grain-granular delta: copy only changed grains into the child image.
    let mut delta = vec![0u8; base.len()];
    delta[2 * MIB..2 * MIB + 65_536].copy_from_slice(&child[2 * MIB..2 * MIB + 65_536]);
    let c = write(
        dir.path(),
        "child.vmdk",
        &vmdk_sparse(&delta, "child.vmdk", Some("base.vmdk")),
    );
    let img = open(&c).unwrap();
    assert_eq!(img.info.paths.len(), 2);
    assert_same(&img, &child, "vmdk delta");
}

#[test]
fn vmdk_descriptor_with_missing_extent_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(
        dir.path(),
        "d.vmdk",
        b"# Disk DescriptorFile\nversion=1\ncreateType=\"monolithicFlat\"\nRW 2048 FLAT \"d-flat.vmdk\" 0\n",
    );
    let err = open(&p).unwrap_err();
    assert!(err.to_string().contains("d-flat.vmdk"), "{err}");
    // With the extent present it opens; a ZERO extent follows it.
    write(dir.path(), "d-flat.vmdk", &vec![5u8; 2048 * 512]);
    std::fs::write(
        &p,
        b"# Disk DescriptorFile\nversion=1\ncreateType=\"custom\"\nRW 2048 FLAT \"d-flat.vmdk\" 0\nRW 16 ZERO\n",
    )
    .unwrap();
    let img = open(&p).unwrap();
    assert_eq!(img.info.size, 2064 * 512);
    let mut want = vec![5u8; 2048 * 512];
    want.extend(vec![0u8; 16 * 512]);
    assert_same(&img, &want, "flat + zero");
}

// ------------------------------------------------------------ corrupt headers

fn mem(v: Vec<u8>) -> SharedSource {
    Arc::new(MemSource::new(v))
}

#[test]
fn corrupt_vhd_headers() {
    let raw = vec![3u8; 4 * MIB];
    let good = vhd_dynamic(&raw, 2 * MIB as u32);
    // Bad block size in the dynamic header.
    let mut bad = good.clone();
    bad[512 + 32..512 + 36].copy_from_slice(&3u32.to_be_bytes());
    let len = bad.len();
    bad[len - 512 + 32..len - 512 + 36].copy_from_slice(&3u32.to_be_bytes());
    let e = vhd::VhdSource::from_source(mem(bad), None).unwrap_err();
    assert!(e.to_string().contains("block size"), "{e}");
    // Unknown disk type.
    let mut bad = vhd_fixed(&raw);
    let len = bad.len();
    bad[len - 512 + 60..len - 512 + 64].copy_from_slice(&9u32.to_be_bytes());
    assert!(vhd::VhdSource::from_source(mem(bad), None).is_err());
    // BAT pointing past the end: reads fail, never panic.
    let mut bad = good.clone();
    bad[1536..1540].copy_from_slice(&0x7FFF_FFF0u32.to_be_bytes());
    let (src, _) = vhd::VhdSource::from_source(mem(bad), None).unwrap();
    assert!(src.read_vec_at(0, 4096).is_err());
}

#[test]
fn corrupt_vhdx_headers() {
    let raw = vec![3u8; 2 * MIB];
    let mut bad = vhdx(&raw, &VhdxOptions::default());
    // Break both header checksums.
    bad[64 * 1024 + 100] ^= 1;
    bad[128 * 1024 + 100] ^= 1;
    let e = vhdx::from_source(mem(bad), None).unwrap_err();
    assert!(e.to_string().contains("no valid header"), "{e}");
    assert!(vhdx::from_source(mem(b"vhdxfile".to_vec()), None).is_err());
}

#[test]
fn corrupt_qcow2_headers() {
    let mut h = vec![0u8; 4096];
    h[..4].copy_from_slice(b"QFI\xfb");
    h[4..8].copy_from_slice(&3u32.to_be_bytes());
    h[20..24].copy_from_slice(&40u32.to_be_bytes()); // cluster bits
    h[100..104].copy_from_slice(&104u32.to_be_bytes());
    let e = qcow2::from_source(mem(h.clone()), None).unwrap_err();
    assert!(e.to_string().contains("cluster bits"), "{e}");
    h[20..24].copy_from_slice(&16u32.to_be_bytes());
    h[32..36].copy_from_slice(&1u32.to_be_bytes()); // AES
    let e = qcow2::from_source(mem(h.clone()), None).unwrap_err();
    assert!(matches!(e, ImageError::Encrypted { .. }), "{e}");
    assert!(e.to_string().contains("AES"), "{e}");
    h[32..36].copy_from_slice(&0u32.to_be_bytes());
    h[24..32].copy_from_slice(&(1u64 << 40).to_be_bytes()); // size, L1 too small
    let e = qcow2::from_source(mem(h.clone()), None).unwrap_err();
    assert!(e.to_string().contains("L1 table"), "{e}");
    h[4..8].copy_from_slice(&1u32.to_be_bytes());
    assert!(matches!(
        qcow2::from_source(mem(h), None).unwrap_err(),
        ImageError::Unsupported { .. }
    ));
}

#[test]
fn corrupt_vmdk_headers() {
    let raw = vec![3u8; MIB];
    let mut bad = vmdk_sparse(&raw, "x.vmdk", None);
    bad[20..28].copy_from_slice(&0u64.to_le_bytes()); // grain size 0
    assert!(vmdk::from_source(mem(bad), None).is_err());
    let mut bad = vmdk_sparse(&raw, "x.vmdk", None);
    bad[44..48].copy_from_slice(&0u32.to_le_bytes()); // GTEs per GT
    assert!(vmdk::from_source(mem(bad), None).is_err());
    let mut bad = vmdk_sparse(&raw, "x.vmdk", None);
    bad[56..64].copy_from_slice(&u64::MAX.to_le_bytes()); // GD at end, no footer
    assert!(vmdk::from_source(mem(bad), None).is_err());
}

// --------------------------------------------------------------- never panic

fn read_all_bounded(src: &dyn ReadAt) {
    let n = src.size().min(4 * MIB as u64);
    let mut buf = vec![0u8; 65_536];
    let mut off = 0;
    while off < n {
        match src.read_at(off, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(k) => off += k as u64,
        }
    }
    let _ = src.read_at(src.size().saturating_sub(10), &mut buf);
}

fn mutate(mut v: Vec<u8>, edits: &[(usize, u8)]) -> Vec<u8> {
    let len = v.len();
    for &(i, b) in edits {
        v[i % len] = b;
    }
    v
}

fn seeds() -> Vec<Vec<u8>> {
    let mut raw = vec![0u8; 3 * MIB];
    fill(&mut raw, 0..100_000, 1);
    fill(&mut raw, 2 * MIB + 7..2 * MIB + 9000, 2);
    let all = |s: u64| s.is_multiple_of(3);
    vec![
        vhd_dynamic(&raw, 512 * 1024),
        vhd_fixed(&raw[..MIB]),
        vhd_differencing(
            &raw,
            512 * 1024,
            &all,
            &VhdParent {
                relative: "p.vhd",
                absolute: "p.vhd",
                parent_id: [0; 16],
            },
        ),
        vhdx(&raw, &VhdxOptions::default()),
        vhdx(
            &raw,
            &VhdxOptions {
                parent: Some((&all, "p.vhdx")),
                ..VhdxOptions::default()
            },
        ),
        vmdk_sparse(&raw, "x.vmdk", None),
    ]
}

fn open_any(data: Vec<u8>) {
    let src = mem(data);
    let parent: SharedSource = mem(vec![0x5A; 64 * 1024]);
    if let Ok((s, _)) = vhd::VhdSource::from_source(Arc::clone(&src), Some(Arc::clone(&parent))) {
        read_all_bounded(&s);
    }
    if let Ok((s, _)) = vhdx::from_source(Arc::clone(&src), Some(Arc::clone(&parent))) {
        read_all_bounded(&s);
    }
    if let Ok((s, _)) = vmdk::from_source(Arc::clone(&src), Some(Arc::clone(&parent))) {
        read_all_bounded(&s);
    }
    if let Ok((s, _)) = qcow2::from_source(Arc::clone(&src), Some(parent)) {
        read_all_bounded(&s);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn mutated_images_never_panic(
        which in 0usize..6,
        edits in proptest::collection::vec((0usize..1 << 22, any::<u8>()), 1..40),
    ) {
        let s = seeds().swap_remove(which);
        // Bias edits towards the metadata at the start and end of the file.
        let len = s.len();
        let edits: Vec<(usize, u8)> = edits
            .into_iter()
            .map(|(i, b)| match i % 3 {
                0 => (i % 4096, b),
                1 => (len - 1 - i % 2048, b),
                _ => (i, b),
            })
            .collect();
        open_any(mutate(s, &edits));
    }

    #[test]
    fn arbitrary_bytes_never_panic(
        magic in prop::sample::select(vec![&b"conectix"[..], b"vhdxfile", b"KDMV", b"QFI\xfb", b"# Disk DescriptorFile\n"]),
        body in proptest::collection::vec(any::<u8>(), 0..300_000),
    ) {
        let mut v = magic.to_vec();
        v.extend(body);
        open_any(v);
    }
}

#[test]
fn qcow2_hand_patched_backing_reads() {
    // A minimal QCOW2 with no L2 tables reads entirely from its backing.
    let cluster = 65_536usize;
    let mut v = vec![0u8; 3 * cluster];
    v[..4].copy_from_slice(b"QFI\xfb");
    v[4..8].copy_from_slice(&2u32.to_be_bytes());
    v[20..24].copy_from_slice(&16u32.to_be_bytes());
    v[24..32].copy_from_slice(&(2 * MIB as u64).to_be_bytes());
    v[36..40].copy_from_slice(&1u32.to_be_bytes());
    v[40..48].copy_from_slice(&(cluster as u64).to_be_bytes());
    let mut backing = vec![0u8; MIB];
    fill(&mut backing, 0..MIB, 3);
    let (s, h) = qcow2::from_source(mem(v), Some(mem(backing.clone()))).unwrap();
    assert_eq!(h.version, 2);
    let got = s.read_vec_at(0, 2 * MIB).unwrap();
    assert_eq!(got[..MIB], backing[..]);
    assert!(
        got[MIB..].iter().all(|&b| b == 0),
        "beyond a shorter backing file"
    );
}

/// Inputs that crashed the `{vhd,vhdx,vmdk,qcow2}_read` fuzz targets.
#[test]
fn fuzz_regressions() {
    for target in ["vhd_read", "vhdx_read", "vmdk_read", "qcow2_read"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/regressions")
            .join(target);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries {
            open_any(std::fs::read(e.unwrap().path()).unwrap());
        }
    }
}
