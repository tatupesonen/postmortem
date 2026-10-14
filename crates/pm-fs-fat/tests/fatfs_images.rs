//! End-to-end tests against FAT images produced by the independent `fatfs`
//! writer, covering FAT12/16/32, long names, deleted-file recovery,
//! fragmentation, timestamps and unallocated space.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::io::{Cursor, Write};
use std::ops::ControlFlow;
use std::sync::Arc;

use fatfs::{FatType, FormatVolumeOptions, FsOptions};
use pm_core::io::MemSource;
use pm_core::time::format_utc;
use pm_core::{ReadAt, SharedSource};
use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{FileKind, FileSystem, FsKind, MetaFlags};
use pm_fs_fat::{FatFs, FatOptions, ROOT_INODE, probe};
use rstest::rstest;

#[derive(Debug)]
struct FixedTime;

impl fatfs::TimeProvider for FixedTime {
    fn get_current_date(&self) -> fatfs::Date {
        fatfs::Date {
            year: 2021,
            month: 3,
            day: 4,
        }
    }
    fn get_current_date_time(&self) -> fatfs::DateTime {
        fatfs::DateTime {
            date: self.get_current_date(),
            time: fatfs::Time {
                hour: 10,
                min: 20,
                sec: 30,
                millis: 0,
            },
        }
    }
}

static FIXED: FixedTime = FixedTime;

type Fs<'a> = fatfs::FileSystem<&'a mut Cursor<Vec<u8>>>;

fn build(fat: FatType, mib: usize, populate: impl FnOnce(&Fs<'_>)) -> Vec<u8> {
    let mut cur = Cursor::new(vec![0u8; mib * 1024 * 1024]);
    fatfs::format_volume(
        &mut cur,
        FormatVolumeOptions::new()
            .fat_type(fat)
            .volume_label(*b"PMTEST     ")
            .volume_id(0x1234_ABCD),
    )
    .unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut cur, FsOptions::new().time_provider(&FIXED)).unwrap();
        populate(&fs);
        fs.unmount().unwrap();
    }
    cur.into_inner()
}

fn open(img: Vec<u8>) -> FatFs {
    let src: SharedSource = Arc::new(MemSource::new(img));
    assert!(probe(&src));
    FatFs::open(src, &FatOptions::default()).unwrap()
}

fn write_file(fs: &Fs<'_>, path: &str, data: &[u8]) {
    let mut f = fs.root_dir().create_file(path).unwrap();
    f.truncate().unwrap();
    f.write_all(data).unwrap();
}

/// Lists `path → (inode, allocated)` for every name in the filesystem.
fn listing(fs: &FatFs) -> Vec<(String, u64, bool)> {
    let mut out = Vec::new();
    walk(fs, &WalkOptions::default(), |i| {
        out.push((i.path(), i.entry.inode, i.entry.allocated));
        ControlFlow::Continue(())
    });
    out
}

fn find(fs: &FatFs, path: &str) -> (u64, bool) {
    listing(fs)
        .into_iter()
        .find(|(p, _, _)| p == path)
        .map_or_else(
            || panic!("{path} not found in {:?}", listing(fs)),
            |(_, i, a)| (i, a),
        )
}

fn read_all(fs: &FatFs, inode: u64) -> Vec<u8> {
    let s = fs.open(inode, None).unwrap();
    s.read_vec_at(0, s.size() as usize).unwrap()
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

#[rstest]
#[case(FatType::Fat12, 2, FsKind::Fat12)]
#[case(FatType::Fat16, 20, FsKind::Fat16)]
#[case(FatType::Fat32, 40, FsKind::Fat32)]
fn reads_tree_and_content(#[case] fat: FatType, #[case] mib: usize, #[case] kind: FsKind) {
    let big = pattern(100_000, 7);
    let img = build(fat, mib, |fs| {
        write_file(fs, "hello.txt", b"hello, world");
        fs.root_dir().create_dir("Documents").unwrap();
        write_file(fs, "Documents/A long file name with spaces.docx", &big);
        fs.root_dir().create_dir("Documents/nested").unwrap();
        write_file(fs, "Documents/nested/empty.bin", b"");
    });
    let fs = open(img);
    assert_eq!(fs.info().kind, kind);
    assert_eq!(fs.info().label.as_deref(), Some("PMTEST"));
    assert_eq!(fs.info().serial.as_deref(), Some("1234-ABCD"));

    let paths: Vec<String> = listing(&fs).into_iter().map(|(p, _, _)| p).collect();
    for want in [
        "/$MBR",
        "/$FAT1",
        "/$FAT2",
        "/hello.txt",
        "/Documents",
        "/Documents/A long file name with spaces.docx",
        "/Documents/nested",
        "/Documents/nested/empty.bin",
    ] {
        assert!(
            paths.iter().any(|p| p == want),
            "missing {want} in {paths:?}"
        );
    }

    let (hello, alloc) = find(&fs, "/hello.txt");
    assert!(alloc);
    assert_eq!(read_all(&fs, hello), b"hello, world");
    let (doc, _) = find(&fs, "/Documents/A long file name with spaces.docx");
    assert_eq!(read_all(&fs, doc), big);
    let (empty, _) = find(&fs, "/Documents/nested/empty.bin");
    assert!(read_all(&fs, empty).is_empty());

    let entries = fs.read_dir(find(&fs, "/Documents").0).unwrap();
    let lfn = entries
        .iter()
        .find(|e| e.name.starts_with("A long"))
        .unwrap();
    assert_eq!(lfn.short_name.as_deref(), Some("ALONGF~1.DOC"));

    let m = fs.metadata(hello).unwrap();
    assert_eq!(m.kind, FileKind::Regular);
    assert_eq!(m.size, 12);
    assert!(m.is_allocated());
    assert_eq!(
        format_utc(m.times.modified.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(
        format_utc(m.times.created.unwrap()),
        "2021-03-04 10:20:30 UTC"
    );
    assert_eq!(
        format_utc(m.times.accessed.unwrap()),
        "2021-03-04 00:00:00 UTC"
    );
    assert_eq!(m.times.changed, None);

    let dir = fs.metadata(find(&fs, "/Documents").0).unwrap();
    assert_eq!(dir.kind, FileKind::Directory);
    assert!(dir.size > 0 && dir.size.is_multiple_of(512));
    assert_eq!(fs.metadata(ROOT_INODE).unwrap().kind, FileKind::Directory);
}

#[test]
fn virtual_files_expose_boot_sector_and_fats() {
    let fs = open(build(FatType::Fat16, 20, |_| {}));
    let mbr = read_all(&fs, find(&fs, "/$MBR").0);
    assert_eq!(mbr.len(), 512);
    assert_eq!(&mbr[510..], &[0x55, 0xAA]);
    let fat1 = read_all(&fs, find(&fs, "/$FAT1").0);
    let fat2 = read_all(&fs, find(&fs, "/$FAT2").0);
    assert_eq!(fat1, fat2, "mirrored FATs");
    assert_eq!(fat1[0], 0xF8, "media descriptor in FAT[0]");
}

#[rstest]
#[case(FatType::Fat12, 2)]
#[case(FatType::Fat16, 20)]
#[case(FatType::Fat32, 40)]
fn recovers_deleted_file_with_long_name(#[case] fat: FatType, #[case] mib: usize) {
    let secret = pattern(20_000, 3);
    let img = build(fat, mib, |fs| {
        write_file(fs, "keep.txt", b"keep");
        write_file(fs, "Secret plans for world domination.txt", &secret);
        fs.root_dir()
            .remove("Secret plans for world domination.txt")
            .unwrap();
    });
    let fs = open(img);
    let (ino, allocated) = find(&fs, "/Secret plans for world domination.txt");
    assert!(!allocated, "name must be marked deleted");
    let m = fs.metadata(ino).unwrap();
    assert!(!m.is_allocated());
    assert!(m.flags.contains(MetaFlags::RECOVERED_BEST_EFFORT));
    assert_eq!(m.size, 20_000);
    assert_eq!(
        read_all(&fs, ino),
        secret,
        "contiguous deleted content recovered"
    );

    let e = fs
        .read_dir(ROOT_INODE)
        .unwrap()
        .into_iter()
        .find(|e| e.inode == ino)
        .unwrap();
    assert_eq!(e.short_name.as_deref(), Some("_ECRET~1.TXT"));
}

#[test]
fn deleted_file_whose_clusters_were_reused_is_not_resurrected() {
    let img = build(FatType::Fat16, 20, |fs| {
        fs.root_dir().create_dir("other").unwrap();
        write_file(fs, "old.bin", &pattern(4096, 1));
        fs.root_dir().remove("old.bin").unwrap();
        // Different directory: the deleted name survives, its clusters do not.
        write_file(fs, "other/new.bin", &pattern(4096, 9));
    });
    let fs = open(img);
    let (old, alloc) = find(&fs, "/old.bin");
    assert!(!alloc);
    let data = read_all(&fs, old);
    assert_eq!(data.len(), 4096);
    assert_ne!(
        data,
        pattern(4096, 9),
        "must not return the new file's content"
    );
}

#[test]
fn fragmented_file_follows_chain() {
    let first = pattern(6000, 1);
    let more = pattern(30_000, 5);
    let img = build(FatType::Fat16, 20, |fs| {
        write_file(fs, "a.bin", &first);
        write_file(fs, "b.bin", &pattern(3000, 2));
        // Appending after b.bin was allocated fragments a.bin.
        let mut f = fs.root_dir().open_file("a.bin").unwrap();
        std::io::Seek::seek(&mut f, std::io::SeekFrom::End(0)).unwrap();
        f.write_all(&more).unwrap();
    });
    let fs = open(img);
    let (ai, _) = find(&fs, "/a.bin");
    assert_eq!(read_all(&fs, ai), [first, more].concat());
    let extents = fs.extents(ai, None).unwrap();
    assert!(
        extents.len() >= 2,
        "expected fragmentation, got {extents:?}"
    );
    let total: u64 = extents.iter().map(|e| e.len).sum();
    assert!(total >= 36_000);
    let (bi, _) = find(&fs, "/b.bin");
    assert_eq!(read_all(&fs, bi), pattern(3000, 2));
}

#[test]
fn deleted_directory_contents_are_listed() {
    let img = build(FatType::Fat32, 40, |fs| {
        fs.root_dir().create_dir("gone").unwrap();
        write_file(fs, "gone/inside.txt", b"inside");
        fs.root_dir()
            .open_dir("gone")
            .unwrap()
            .remove("inside.txt")
            .unwrap();
        fs.root_dir().remove("gone").unwrap();
    });
    let fs = open(img);
    let (gone, alloc) = find(&fs, "/gone");
    assert!(!alloc);
    assert_eq!(fs.metadata(gone).unwrap().kind, FileKind::Directory);
    let (inside, alloc) = find(&fs, "/gone/inside.txt");
    assert!(!alloc);
    assert_eq!(read_all(&fs, inside), b"inside");
}

#[test]
fn unallocated_space_covers_free_clusters() {
    let img = build(FatType::Fat16, 20, |fs| {
        write_file(fs, "x.bin", &pattern(50_000, 4));
        fs.root_dir().remove("x.bin").unwrap();
    });
    let fs = open(img);
    let unalloc = fs.unallocated_blocks().unwrap();
    assert!(!unalloc.is_empty());
    for w in unalloc.windows(2) {
        assert!(
            w[0].0 + w[0].1 < w[1].0,
            "runs must be sorted and coalesced"
        );
    }
    let total: u64 = unalloc.iter().map(|r| r.1).sum();
    assert!(
        total * 512 > 18 * 1024 * 1024,
        "almost the whole volume is free"
    );
}

#[test]
fn local_times_converted_with_configured_zone() {
    let img = build(FatType::Fat12, 2, |fs| write_file(fs, "t.txt", b"t"));
    let src: SharedSource = Arc::new(MemSource::new(img));
    let tz = jiff::tz::TimeZone::get("Europe/Helsinki").unwrap();
    let fs = FatFs::open(src, &FatOptions { timezone: tz }).unwrap();
    let (ino, _) = find(&fs, "/t.txt");
    let m = fs.metadata(ino).unwrap();
    assert_eq!(
        format_utc(m.times.modified.unwrap()),
        "2021-03-04 08:20:30 UTC"
    );
}

#[test]
fn rejects_non_fat() {
    let src: SharedSource = Arc::new(MemSource::new(vec![0u8; 1 << 20]));
    assert!(!probe(&src));
    assert!(FatFs::open(src, &FatOptions::default()).is_err());
}

mod robustness {
    use super::*;
    use proptest::prelude::*;
    use std::sync::LazyLock;

    static BASE: LazyLock<Vec<u8>> = LazyLock::new(|| {
        build(FatType::Fat12, 1, |fs| {
            write_file(fs, "file one.txt", &pattern(3000, 1));
            fs.root_dir().create_dir("dir").unwrap();
            write_file(fs, "dir/two.bin", &pattern(9000, 2));
            write_file(fs, "dir/del.bin", &pattern(1000, 3));
            fs.root_dir()
                .open_dir("dir")
                .unwrap()
                .remove("del.bin")
                .unwrap();
        })
    });

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Corrupting the boot sector, FAT or directories never panics or
        /// hangs; every reachable file can be read.
        #[test]
        fn corrupted_images_never_panic(
            flips in proptest::collection::vec((0usize..40 * 1024, any::<u8>()), 1..16)
        ) {
            let mut img = BASE.clone();
            for (pos, val) in flips {
                img[pos] = val;
            }
            let src: SharedSource = Arc::new(MemSource::new(img));
            if let Ok(fs) = FatFs::open(src, &FatOptions::default()) {
                walk(&fs, &WalkOptions::default(), |i| {
                    if let Ok(s) = fs.open(i.entry.inode, None) {
                        let n = s.size().min(64 * 1024) as usize;
                        let _ = s.read_vec_at(0, n);
                    }
                    let _ = fs.extents(i.entry.inode, None);
                    ControlFlow::Continue(())
                });
                let _ = fs.unallocated_blocks();
            }
        }
    }
}

/// Regenerates fuzz seeds: `cargo test -p pm-fs-fat -- --ignored write_fuzz_seeds`.
#[test]
#[ignore = "writes files into fuzz/seeds"]
fn write_fuzz_seeds() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/seeds/fat_walk");
    std::fs::create_dir_all(&dir).unwrap();
    let img = build(FatType::Fat12, 1, |fs| {
        write_file(fs, "a long name.txt", &pattern(3000, 1));
        fs.root_dir().create_dir("d").unwrap();
        write_file(fs, "d/x.bin", &pattern(5000, 2));
        write_file(fs, "d/gone.bin", &pattern(700, 3));
        fs.root_dir()
            .open_dir("d")
            .unwrap()
            .remove("gone.bin")
            .unwrap();
    });
    // Keep the seed small: the metadata lives in the first ~40 KiB.
    std::fs::write(dir.join("fat12.img"), &img[..48 * 1024]).unwrap();
}

/// Our view of the tree in `fls -r -p` terms: (path, short-name path, deleted, `is_dir`),
/// including virtual files and the `$OrphanFiles` directory.
fn fls_like(fs: &FatFs) -> Vec<(String, Option<String>, bool, bool)> {
    let mut out = Vec::new();
    walk(fs, &WalkOptions::default(), |i| {
        let path = i.path().trim_start_matches('/').to_owned();
        let alt = i.entry.short_name.as_ref().map(|s| {
            let parent = i.parent_path.trim_start_matches('/');
            if parent.is_empty() {
                s.clone()
            } else {
                format!("{parent}/{s}")
            }
        });
        out.push((
            path,
            alt,
            !i.entry.allocated,
            i.entry.kind == FileKind::Directory,
        ));
        ControlFlow::Continue(())
    });
    out.push(("$OrphanFiles".into(), None, false, true));
    for o in fs.orphans().unwrap() {
        out.push((
            format!("$OrphanFiles/{}", o.name),
            o.short_name.map(|s| format!("$OrphanFiles/{s}")),
            true,
            o.kind == FileKind::Directory,
        ));
    }
    out.sort();
    out
}

fn assert_matches_fls(name: &str, img: &[u8]) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(format!("{name}.dd"));
    std::fs::write(&path, img).unwrap();
    let Some(theirs) = pm_testkit::tsk::fls(&path, 0) else {
        eprintln!("fls not available; skipping {name}");
        return;
    };
    let ours = fls_like(&open(img.to_vec()));
    let mut unmatched: Vec<_> = ours.clone();
    for t in &theirs {
        let is_dir = t.kind.ends_with("/d") || t.kind == "V/V";
        // TSK truncates some deleted long names; accept a prefix match.
        // TSK truncates some deleted long names and shows only the short
        // name of orphans; we recover more, so accept those forms.
        let pos = unmatched.iter().position(|(p, alt, del, dir)| {
            *del == t.deleted
                && *dir == is_dir
                && (*p == t.path
                    || alt.as_deref() == Some(t.path.as_str())
                    || (t.deleted && p.starts_with(t.path.trim_end())))
        });
        match pos {
            Some(i) => {
                unmatched.remove(i);
            }
            None => panic!("{name}: TSK entry {t:?} missing from ours: {ours:#?}"),
        }
    }
    assert!(
        unmatched.is_empty(),
        "{name}: entries TSK does not report: {unmatched:#?}"
    );
}

#[rstest]
#[case(FatType::Fat12, 2)]
#[case(FatType::Fat16, 20)]
#[case(FatType::Fat32, 40)]
fn tree_matches_tsk_fls(#[case] fat: FatType, #[case] mib: usize) {
    let img = build(fat, mib, |fs| {
        write_file(fs, "keep.txt", b"keep");
        write_file(
            fs,
            "Secret plans for world domination.txt",
            &pattern(20_000, 3),
        );
        fs.root_dir()
            .remove("Secret plans for world domination.txt")
            .unwrap();
        fs.root_dir().create_dir("gone").unwrap();
        write_file(fs, "gone/inside.txt", b"inside");
        fs.root_dir()
            .open_dir("gone")
            .unwrap()
            .remove("inside.txt")
            .unwrap();
        fs.root_dir().remove("gone").unwrap();
        fs.root_dir().create_dir("Documents").unwrap(); // overwrites "gone"'s entry
        write_file(fs, "Documents/A long file name with spaces.docx", b"x");
        fs.root_dir().create_dir("Documents/sub").unwrap();
        write_file(fs, "Documents/sub/deep.bin", &pattern(5000, 1));
    });
    assert_matches_fls(&format!("{fat:?}"), &img);
}

#[test]
fn orphan_found_when_parent_entry_overwritten() {
    let img = build(FatType::Fat16, 20, |fs| {
        fs.root_dir().create_dir("gone").unwrap();
        write_file(fs, "gone/inside.txt", b"inside");
        fs.root_dir()
            .open_dir("gone")
            .unwrap()
            .remove("inside.txt")
            .unwrap();
        fs.root_dir().remove("gone").unwrap();
        fs.root_dir().create_dir("Documents").unwrap();
    });
    let fs = open(img);
    let orphans = fs.orphans().unwrap();
    assert_eq!(orphans.len(), 1, "{orphans:?}");
    assert_eq!(
        orphans[0].name, "inside.txt",
        "long name recovered from deleted LFN"
    );
    assert_eq!(orphans[0].short_name.as_deref(), Some("_NSIDE.TXT"));
    assert!(!orphans[0].allocated);
    // Unlike TSK, we still recover the orphan's content.
    assert_eq!(read_all(&fs, orphans[0].inode), b"inside");
}
