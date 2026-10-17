//! ext2/3/4 tests on images written by `fstool`, cross-checked with Autopsy.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::ops::ControlFlow;
use std::sync::Arc;

use pm_core::io::MemSource;
use pm_core::time::format_utc;
use pm_core::{ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{FileKind, FileSystem, FsKind};
use pm_fs_ext::{ExtFs, probe};
use pm_testkit::ext::{ExtKind, Op, build};
use rstest::rstest;

fn pattern() -> Vec<u8> {
    (0..200_000u32).map(|i| (i % 251) as u8).collect()
}

fn image(kind: ExtKind, bs: u32) -> Vec<u8> {
    build(
        kind,
        bs,
        8192,
        &[
            Op::Dir("/home"),
            Op::Dir("/home/alice"),
            Op::File("/home/alice/notes.txt", b"hello ext\n".to_vec()),
            Op::File("/home/alice/big.bin", pattern()),
            Op::Symlink("/home/alice/link", "notes.txt"),
            Op::Sparse("/home/alice/sparse.img", 5_000_000),
            Op::File("/home/alice/gone.txt", b"deleted ext file\n".to_vec()),
            Op::Remove("/home/alice/gone.txt"),
        ],
    )
}

fn open(img: Vec<u8>) -> ExtFs {
    let src: SharedSource = Arc::new(MemSource::new(img));
    assert!(probe(&*src));
    ExtFs::open(src).unwrap()
}

fn listing(fs: &ExtFs) -> Vec<(String, u64, bool, FileKind)> {
    let mut v = Vec::new();
    walk(fs, &WalkOptions::default(), |i| {
        v.push((i.path(), i.entry.inode, i.entry.allocated, i.entry.kind));
        ControlFlow::Continue(())
    });
    v
}

fn find(fs: &ExtFs, p: &str) -> (u64, bool) {
    listing(fs)
        .into_iter()
        .find(|e| e.0 == p)
        .map_or_else(|| panic!("{p} missing: {:?}", listing(fs)), |e| (e.1, e.2))
}

fn read_all(fs: &ExtFs, ino: u64) -> Vec<u8> {
    let s = fs.open(ino, None).unwrap();
    s.read_vec_at(0, s.size() as usize).unwrap()
}

#[rstest]
#[case(ExtKind::Ext2, 1024, FsKind::Ext2)]
#[case(ExtKind::Ext3, 1024, FsKind::Ext3)]
#[case(ExtKind::Ext4, 1024, FsKind::Ext4)]
#[case(ExtKind::Ext4, 4096, FsKind::Ext4)]
fn reads_tree_contents_and_metadata(#[case] kind: ExtKind, #[case] bs: u32, #[case] want: FsKind) {
    let fs = open(image(kind, bs));
    assert_eq!(fs.info().kind, want);
    assert_eq!(fs.info().label.as_deref(), Some("PMEXT"));
    assert_eq!(
        read_all(&fs, find(&fs, "/home/alice/notes.txt").0),
        b"hello ext\n"
    );
    assert_eq!(read_all(&fs, find(&fs, "/home/alice/big.bin").0), pattern());
    let sparse = find(&fs, "/home/alice/sparse.img").0;
    let data = read_all(&fs, sparse);
    assert_eq!(data.len(), 5_000_000);
    assert!(data.iter().all(|&b| b == 0));
    let link = find(&fs, "/home/alice/link").0;
    assert_eq!(fs.metadata(link).unwrap().kind, FileKind::Symlink);
    assert_eq!(fs.read_link(link).unwrap().as_deref(), Some("notes.txt"));
    let m = fs.metadata(find(&fs, "/home/alice/notes.txt").0).unwrap();
    assert!(m.is_allocated());
    assert_eq!((m.uid, m.gid), (Some(1000), Some(1000)));
    assert_eq!(m.mode.unwrap() & 0o777, 0o644);
    assert_eq!(
        format_utc(m.times.modified.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    find(&fs, "/lost+found");
}

#[rstest]
#[case(ExtKind::Ext2)]
#[case(ExtKind::Ext4)]
fn deleted_name_from_dirent_slack(#[case] kind: ExtKind) {
    let fs = open(image(kind, 1024));
    let (ino, allocated) = find(&fs, "/home/alice/gone.txt");
    assert!(!allocated, "deleted name recovered from slack");
    assert!(!fs.metadata(ino).unwrap().is_allocated());
}

#[test]
fn unallocated_blocks_are_coalesced() {
    let fs = open(image(ExtKind::Ext4, 1024));
    let free = fs.unallocated_blocks().unwrap();
    assert!(!free.is_empty());
    for w in free.windows(2) {
        assert!(w[0].0 + w[0].1 < w[1].0);
    }
}

#[rstest]
#[case(ExtKind::Ext2)]
#[case(ExtKind::Ext3)]
#[case(ExtKind::Ext4)]
fn matches_reference_fls(#[case] kind: ExtKind) {
    let img = image(kind, 1024);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ext.img");
    std::fs::write(&path, &img).unwrap();
    let Some(theirs) = pm_testkit::reftools::fls(&path, 0) else {
        eprintln!("fls not available; skipping");
        return;
    };
    let fs = open(img);
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

mod robustness {
    use super::*;
    use proptest::prelude::*;
    use std::sync::LazyLock;

    static BASE: LazyLock<Vec<u8>> = LazyLock::new(|| image(ExtKind::Ext4, 1024));

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn corruption_never_panics(flips in proptest::collection::vec((0usize..300 * 1024, any::<u8>()), 1..24)) {
            let mut img = BASE.clone();
            for (p, v) in flips {
                img[p] = v;
            }
            let src: SharedSource = Arc::new(MemSource::new(img));
            if let Ok(fs) = ExtFs::open(src) {
                walk(&fs, &WalkOptions { max_depth: 32, ..Default::default() }, |i| {
                    if let Ok(s) = fs.open(i.entry.inode, None) {
                        let _ = s.read_vec_at(0, s.size().min(1 << 20) as usize);
                    }
                    ControlFlow::Continue(())
                });
                let _ = fs.unallocated_blocks();
            }
        }
    }
}
