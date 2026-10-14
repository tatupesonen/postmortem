//! NTFS parser tests on volumes from `pm_testkit::ntfs::NtfsBuilder`,
//! cross-checked against TSK (`fls`, when installed) and the `ntfs` crate.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::ops::ControlFlow;
use std::sync::{Arc, LazyLock};

use pm_core::io::MemSource;
use pm_core::time::format_utc;
use pm_core::{ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{ExtentKind, FileKind, FileSystem, FsKind, MetaFlags, StreamKind};
use pm_fs_ntfs::{NtfsFs, ROOT_INODE, probe};
use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

fn frag_data() -> Vec<u8> {
    (0..50_000u32).map(|i| (i % 251) as u8).collect()
}

fn comp_data() -> Vec<u8> {
    b"compressible text ".repeat(5000)
}

/// One image exercising every builder feature.
static IMAGE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut b = NtfsBuilder::new(16);
    b.file("hello.txt", b"hello, ntfs");
    b.dir("Documents");
    b.file("Documents/big.bin", &vec![0x5Au8; 20_000]);
    b.file_with(
        "Documents/frag.bin",
        &frag_data(),
        FileOpts {
            fragmented: true,
            ..Default::default()
        },
    );
    b.file_with(
        "Documents/comp.txt",
        &comp_data(),
        FileOpts {
            compressed: true,
            ..Default::default()
        },
    );
    b.file_with(
        "ads.txt",
        b"main",
        FileOpts {
            ads: vec![(
                "Zone.Identifier".into(),
                b"[ZoneTransfer]\r\nZoneId=3\r\n".to_vec(),
            )],
            ..Default::default()
        },
    );
    b.file_with(
        "Documents/slackdel.txt",
        b"deleted in slack",
        FileOpts {
            deleted: Deleted::InSlack,
            ..Default::default()
        },
    );
    b.file_with(
        "Documents/unindexed.txt",
        b"unindexed",
        FileOpts {
            deleted: Deleted::Unindexed,
            ..Default::default()
        },
    );
    b.file_with(
        "orphan.txt",
        b"orphan!",
        FileOpts {
            deleted: Deleted::Orphan,
            ..Default::default()
        },
    );
    b.dir("Many");
    for i in 0..150 {
        b.file(
            &format!("Many/file_{i:03}_with_a_longish_name.txt"),
            format!("content {i}").as_bytes(),
        );
    }
    b.build()
});

fn open() -> NtfsFs {
    let src: SharedSource = Arc::new(MemSource::new(IMAGE.clone()));
    assert!(probe(&src));
    NtfsFs::open(src).unwrap()
}

fn listing(fs: &NtfsFs) -> Vec<(String, u64, bool, FileKind)> {
    let mut out = Vec::new();
    walk(fs, &WalkOptions::default(), |i| {
        out.push((i.path(), i.entry.inode, i.entry.allocated, i.entry.kind));
        ControlFlow::Continue(())
    });
    out
}

fn find(fs: &NtfsFs, path: &str) -> (u64, bool) {
    listing(fs)
        .into_iter()
        .find(|(p, ..)| p == path)
        .map_or_else(|| panic!("{path} not found"), |(_, i, a, _)| (i, a))
}

fn read_all(fs: &NtfsFs, inode: u64, stream: Option<u32>) -> Vec<u8> {
    let s = fs.open(inode, stream).unwrap();
    s.read_vec_at(0, s.size() as usize).unwrap()
}

#[test]
fn volume_info() {
    let fs = open();
    let info = fs.info();
    assert_eq!(info.kind, FsKind::Ntfs);
    assert_eq!(info.block_size, 4096);
    assert_eq!(info.label.as_deref(), Some("PMNTFS"));
    assert_eq!(info.serial.as_deref(), Some("123456789ABCDEF0"));
    assert_eq!(info.root_inode, ROOT_INODE);
}

#[test]
fn tree_contains_system_and_user_files() {
    let fs = open();
    let paths: Vec<String> = listing(&fs).into_iter().map(|(p, ..)| p).collect();
    for want in [
        "/$MFT",
        "/$MFTMirr",
        "/$LogFile",
        "/$Volume",
        "/$AttrDef",
        "/$Bitmap",
        "/$Boot",
        "/$BadClus",
        "/$Secure",
        "/$UpCase",
        "/$Extend",
        "/hello.txt",
        "/Documents/frag.bin",
        "/Many/file_149_with_a_longish_name.txt",
    ] {
        assert!(paths.iter().any(|p| p == want), "missing {want}");
    }
    assert!(!paths.iter().any(|p| p == "/."), "root self-entry hidden");
    let many = paths.iter().filter(|p| p.starts_with("/Many/")).count();
    assert_eq!(many, 150, "large directory spans INDX blocks");
}

#[test]
fn contents_resident_nonresident_fragmented_compressed() {
    let fs = open();
    assert_eq!(
        read_all(&fs, find(&fs, "/hello.txt").0, None),
        b"hello, ntfs"
    );
    assert_eq!(
        read_all(&fs, find(&fs, "/Documents/big.bin").0, None),
        vec![0x5A; 20_000]
    );
    let frag = find(&fs, "/Documents/frag.bin").0;
    assert_eq!(read_all(&fs, frag, None), frag_data());
    let ext = fs.extents(frag, None).unwrap();
    assert_eq!(ext.len(), 2, "two runs with a gap");
    assert_eq!(
        ext[1].physical_offset - (ext[0].physical_offset + ext[0].len),
        4096
    );
    let comp = find(&fs, "/Documents/comp.txt").0;
    assert_eq!(read_all(&fs, comp, None), comp_data());
    assert!(
        fs.extents(comp, None)
            .unwrap()
            .iter()
            .any(|e| e.kind == ExtentKind::Sparse)
    );
    let m = fs.metadata(comp).unwrap();
    assert!(m.flags.contains(MetaFlags::COMPRESSED));
    assert_eq!(m.size, 90_000);
    for i in [0, 77, 149] {
        let ino = find(&fs, &format!("/Many/file_{i:03}_with_a_longish_name.txt")).0;
        assert_eq!(read_all(&fs, ino, None), format!("content {i}").as_bytes());
    }
}

#[test]
fn alternate_data_streams() {
    let fs = open();
    let ino = find(&fs, "/ads.txt").0;
    let streams = fs.streams(ino).unwrap();
    assert_eq!(streams.len(), 2);
    let ads = streams
        .iter()
        .find(|s| s.kind == StreamKind::AlternateData)
        .unwrap();
    assert_eq!(ads.name.as_deref(), Some("Zone.Identifier"));
    assert_eq!(
        read_all(&fs, ino, Some(ads.id)),
        b"[ZoneTransfer]\r\nZoneId=3\r\n"
    );
    assert_eq!(read_all(&fs, ino, None), b"main");
    assert!(fs.open(ino, Some(999)).is_err());
}

#[test]
fn metadata_and_timestamps() {
    let fs = open();
    let m = fs.metadata(find(&fs, "/hello.txt").0).unwrap();
    assert_eq!(m.kind, FileKind::Regular);
    assert!(m.is_allocated());
    assert_eq!(m.size, 11);
    assert_eq!(
        format_utc(m.times.created.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(
        format_utc(m.times.modified.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(
        format_utc(m.times.changed.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(
        format_utc(m.times.accessed.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(m.fn_times.unwrap().created, m.times.created);
    assert_eq!(m.seq, Some(1));
    let d = fs.metadata(find(&fs, "/Documents").0).unwrap();
    assert_eq!(d.kind, FileKind::Directory);
}

#[test]
fn deleted_files_recovered() {
    let fs = open();
    for (path, content) in [
        ("/Documents/slackdel.txt", &b"deleted in slack"[..]),
        ("/Documents/unindexed.txt", b"unindexed"),
    ] {
        let (ino, allocated) = find(&fs, path);
        assert!(!allocated, "{path} must be listed as deleted");
        let m = fs.metadata(ino).unwrap();
        assert!(!m.is_allocated());
        assert_eq!(m.seq, Some(2), "sequence bumped on delete");
        assert_eq!(read_all(&fs, ino, None), content);
    }
    let orphans = fs.orphans().unwrap();
    assert_eq!(
        orphans.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(),
        ["orphan.txt"]
    );
    assert_eq!(read_all(&fs, orphans[0].inode, None), b"orphan!");
    assert!(!listing(&fs).iter().any(|(p, ..)| p.ends_with("orphan.txt")));
}

#[test]
fn unallocated_clusters_include_gaps() {
    let fs = open();
    let free = fs.unallocated_blocks().unwrap();
    let frag = find(&fs, "/Documents/frag.bin").0;
    let ext = fs.extents(frag, None).unwrap();
    let gap = (ext[0].physical_offset + ext[0].len) / 4096;
    assert!(
        free.iter().any(|&(s, n)| gap >= s && gap < s + n),
        "fragmentation gap is free"
    );
    for w in free.windows(2) {
        assert!(w[0].0 + w[0].1 < w[1].0);
    }
}

#[test]
fn matches_tsk_fls() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ntfs.dd");
    std::fs::write(&path, &*IMAGE).unwrap();
    let Some(theirs) = pm_testkit::tsk::fls(&path, 0) else {
        eprintln!("fls not available; skipping");
        return;
    };
    let fs = open();
    // Our view in fls terms: each $DATA stream of a file is a row
    // ("name:stream"), plus $OrphanFiles.
    let mut ours: Vec<(String, bool)> = Vec::new();
    walk(&fs, &WalkOptions::default(), |i| {
        let p = i.path().trim_start_matches('/').to_owned();
        ours.push((p.clone(), !i.entry.allocated));
        if let Ok(streams) = fs.streams(i.entry.inode) {
            for s in streams
                .iter()
                .filter(|s| s.kind == StreamKind::AlternateData)
            {
                ours.push((
                    format!("{p}:{}", s.name.as_deref().unwrap_or_default()),
                    !i.entry.allocated,
                ));
            }
        }
        ControlFlow::Continue(())
    });
    ours.push(("$OrphanFiles".into(), false));
    for o in fs.orphans().unwrap() {
        ours.push((format!("$OrphanFiles/{}", o.name), true));
    }
    // TSK lists $Secure's named stream as the file itself; normalise both.
    let normalise = |mut v: Vec<(String, bool)>| {
        v.retain(|(p, _)| !p.starts_with("$Secure"));
        v.sort();
        v
    };
    let theirs = normalise(theirs.into_iter().map(|r| (r.path, r.deleted)).collect());
    assert_eq!(normalise(ours), theirs);
}

#[test]
fn matches_ntfs_crate_oracle() {
    let mut cur = std::io::Cursor::new(IMAGE.clone());
    let ntfs = ntfs::Ntfs::new(&mut cur).unwrap();
    let fs = open();
    for (dir_path, dir_ino) in [
        ("", ROOT_INODE),
        ("/Documents", find(&fs, "/Documents").0),
        ("/Many", find(&fs, "/Many").0),
    ] {
        let file = ntfs.file(&mut cur, dir_ino).unwrap();
        let index = file.directory_index(&mut cur).unwrap();
        let mut entries = index.entries();
        let mut theirs = Vec::new();
        while let Some(e) = entries.next(&mut cur) {
            let e = e.unwrap();
            let key = e.key().unwrap().unwrap();
            if key.namespace() == ntfs::structured_values::NtfsFileNamespace::Dos {
                continue;
            }
            let name = key.name().to_string_lossy();
            if name != "." {
                theirs.push((name, e.file_reference().file_record_number()));
            }
        }
        let mut ours: Vec<(String, u64)> = fs
            .read_dir(dir_ino)
            .unwrap()
            .into_iter()
            .filter(|e| e.allocated)
            .map(|e| (e.name, e.inode))
            .collect();
        theirs.sort();
        ours.sort();
        assert_eq!(ours, theirs, "directory {dir_path}/");
    }
    // Content of every allocated, uncompressed file must agree.
    for (path, ino, allocated, kind) in listing(&fs) {
        if !allocated
            || kind != FileKind::Regular
            || path.contains("comp.txt")
            || path.starts_with("/$")
        {
            continue;
        }
        let file = ntfs.file(&mut cur, ino).unwrap();
        let item = file.data(&mut cur, "").unwrap().unwrap();
        let attr = item.to_attribute().unwrap();
        let mut reader = attr.value(&mut cur).unwrap().attach(&mut cur);
        let mut theirs = Vec::new();
        std::io::Read::read_to_end(&mut reader, &mut theirs).unwrap();
        assert_eq!(read_all(&fs, ino, None), theirs, "{path}");
    }
}

mod robustness {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Random corruption of boot sector, MFT and index blocks never
        /// panics; every reachable stream can be read.
        #[test]
        fn corrupted_volumes_never_panic(flips in proptest::collection::vec((0usize..400 * 1024, any::<u8>()), 1..24)) {
            let mut img = IMAGE.clone();
            for (pos, v) in flips {
                img[pos] = v;
            }
            let src: SharedSource = Arc::new(MemSource::new(img));
            if let Ok(fs) = NtfsFs::open(src) {
                walk(&fs, &WalkOptions::default(), |i| {
                    if let Ok(streams) = fs.streams(i.entry.inode) {
                        for s in streams {
                            if let Ok(src) = fs.open(i.entry.inode, Some(s.id)) {
                                let _ = src.read_vec_at(0, src.size().min(256 * 1024) as usize);
                            }
                        }
                    }
                    ControlFlow::Continue(())
                });
                let _ = fs.orphans();
                let _ = fs.unallocated_blocks();
            }
        }
    }
}

/// Regenerates the fuzz seed: `cargo test -p pm-fs-ntfs -- --ignored write_fuzz_seed`.
#[test]
#[ignore = "writes files into fuzz/seeds"]
fn write_fuzz_seed() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/ntfs_walk");
    std::fs::create_dir_all(&dir).unwrap();
    let mut b = NtfsBuilder::new(1);
    b.file("a.txt", b"resident");
    b.dir("d");
    b.file_with(
        "d/frag.bin",
        &frag_data()[..20_000],
        FileOpts {
            fragmented: true,
            ..Default::default()
        },
    );
    b.file_with(
        "d/c.txt",
        &b"abc".repeat(3000),
        FileOpts {
            compressed: true,
            ..Default::default()
        },
    );
    b.file_with(
        "d/gone.txt",
        b"gone",
        FileOpts {
            deleted: Deleted::InSlack,
            ..Default::default()
        },
    );
    b.file_with(
        "o.txt",
        b"o",
        FileOpts {
            deleted: Deleted::Orphan,
            ads: vec![("s".into(), b"ads".to_vec())],
            ..Default::default()
        },
    );
    std::fs::write(dir.join("small.ntfs"), b.build()).unwrap();
}

/// Replays every input that once crashed or hung the `ntfs_walk` fuzzer.
#[test]
fn fuzz_regressions() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions/ntfs_walk");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let started = std::time::Instant::now();
        let src: SharedSource = Arc::new(MemSource::new(std::fs::read(&path).unwrap()));
        if let Ok(fs) = NtfsFs::open(src) {
            walk(
                &fs,
                &WalkOptions {
                    max_depth: 32,
                    ..Default::default()
                },
                |i| {
                    if let Ok(streams) = fs.streams(i.entry.inode) {
                        for s in streams {
                            if let Ok(c) = fs.open(i.entry.inode, Some(s.id)) {
                                let _ = c.read_vec_at(0, c.size().min(1 << 16) as usize);
                            }
                        }
                    }
                    ControlFlow::Continue(())
                },
            );
            let _ = fs.orphans();
            let _ = fs.unallocated_blocks();
        }
        assert!(
            started.elapsed().as_secs() < 10,
            "{} took {:?}",
            path.display(),
            started.elapsed()
        );
    }
}
