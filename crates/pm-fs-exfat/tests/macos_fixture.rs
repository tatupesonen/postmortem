//! Tests against `fixtures/exfat/basic.raw.gz`, written by macOS's exFAT
//! driver (see `scripts/fixtures/make-exfat.sh`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::io::Read;
use std::ops::ControlFlow;
use std::sync::{Arc, LazyLock};

use pm_core::io::MemSource;
use pm_core::time::format_utc;
use pm_core::{ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{FileKind, FileSystem, FsKind};
use pm_fs_exfat::{ExFatFs, ExFatOptions, probe};

static RAW: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/exfat/basic.raw.gz");
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut out)
        .unwrap();
    out
});

fn open() -> ExFatFs {
    let src: SharedSource = Arc::new(MemSource::new(RAW.clone()));
    assert!(probe(&*src));
    ExFatFs::open(src, &ExFatOptions::default()).unwrap()
}

fn listing(fs: &ExFatFs) -> Vec<(String, u64, bool, FileKind)> {
    let mut v = Vec::new();
    walk(fs, &WalkOptions::default(), |i| {
        v.push((i.path(), i.entry.inode, i.entry.allocated, i.entry.kind));
        ControlFlow::Continue(())
    });
    v
}

fn find(fs: &ExFatFs, path: &str) -> (u64, bool) {
    listing(fs)
        .into_iter()
        .find(|e| e.0 == path)
        .map_or_else(|| panic!("{path} missing"), |e| (e.1, e.2))
}

fn read_all(fs: &ExFatFs, inode: u64) -> Vec<u8> {
    let s = fs.open(inode, None).unwrap();
    s.read_vec_at(0, s.size() as usize).unwrap()
}

#[test]
fn volume() {
    let fs = open();
    assert_eq!(fs.info().kind, FsKind::ExFat);
    assert_eq!(fs.info().label.as_deref(), Some("PMEXFAT"));
}

#[test]
fn tree_and_contents() {
    let fs = open();
    assert_eq!(read_all(&fs, find(&fs, "/hello.txt").0), b"hello exfat\n");
    let long = "/A very long file name with unicode ääkköset and spaces.txt";
    assert_eq!(read_all(&fs, find(&fs, long).0), b"long name\n");
    assert_eq!(
        read_all(&fs, find(&fs, "/docs/report.txt").0),
        "report ".repeat(2000).as_bytes()
    );
    let img: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(
        read_all(&fs, find(&fs, "/DCIM/100CANON/IMG_0001.bin").0),
        img
    );
    for virt in [
        "/$ALLOC_BITMAP",
        "/$UPCASE_TABLE",
        "/PMEXFAT (Volume Label Entry)",
        "/$MBR",
        "/$FAT1",
    ] {
        find(&fs, virt);
    }
}

#[test]
fn deleted_files_recovered() {
    let fs = open();
    let (ino, alloc) = find(&fs, "/deleted.txt");
    assert!(!alloc);
    assert!(!fs.metadata(ino).unwrap().is_allocated());
    assert_eq!(read_all(&fs, ino), b"soon deleted content\n");
    let (ino, alloc) = find(&fs, "/docs/also_deleted.bin");
    assert!(!alloc);
    assert_eq!(read_all(&fs, ino), vec![b'X'; 50_000]);
}

#[test]
fn timestamps_use_utc_offset() {
    // The fixture was written at 18:46:38 local time (UTC+3), so the true
    // UTC time is 15:46:38. TSK 4.15 shows 12:46:38 EEST (it misapplies
    // the offset); we decode the stored offset correctly.
    let fs = open();
    let m = fs.metadata(find(&fs, "/hello.txt").0).unwrap();
    assert_eq!(
        format_utc(m.times.modified.unwrap()),
        "2026-10-14 15:46:38 UTC"
    );
    assert_eq!(
        format_utc(m.times.created.unwrap()),
        "2026-10-14 15:46:38 UTC"
    );
}

#[test]
fn matches_tsk_fls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ex.raw");
    std::fs::write(&path, &*RAW).unwrap();
    let Some(theirs) = pm_testkit::tsk::fls(&path, 0) else {
        eprintln!("fls not available; skipping");
        return;
    };
    let fs = open();
    let mut ours: Vec<(String, bool)> = listing(&fs)
        .into_iter()
        .map(|(p, _, a, _)| (p.trim_start_matches('/').to_owned(), !a))
        .collect();
    ours.push(("$OrphanFiles".into(), false));
    for o in fs.orphans().unwrap() {
        ours.push((format!("$OrphanFiles/{}", o.name), true));
    }
    let mut theirs: Vec<(String, bool)> = theirs.into_iter().map(|r| (r.path, r.deleted)).collect();
    ours.sort();
    theirs.sort();
    assert_eq!(ours, theirs);
}

#[test]
fn unallocated_and_orphans() {
    let fs = open();
    let free: u64 = fs.unallocated_blocks().unwrap().iter().map(|r| r.1).sum();
    assert!(
        free * 512 > 6 * 1024 * 1024,
        "most of the 8 MiB volume is free"
    );
    assert!(fs.orphans().unwrap().is_empty());
}

mod robustness {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn corruption_never_panics(flips in proptest::collection::vec((0usize..2 * 1024 * 1024, any::<u8>()), 1..24)) {
            let mut img = RAW.clone();
            for (p, v) in flips {
                img[p] = v;
            }
            let src: SharedSource = Arc::new(MemSource::new(img));
            if let Ok(fs) = ExFatFs::open(src, &ExFatOptions::default()) {
                walk(&fs, &WalkOptions::default(), |i| {
                    if let Ok(s) = fs.open(i.entry.inode, None) {
                        let _ = s.read_vec_at(0, s.size().min(1 << 20) as usize);
                    }
                    ControlFlow::Continue(())
                });
                let _ = fs.orphans();
            }
        }
    }
}
