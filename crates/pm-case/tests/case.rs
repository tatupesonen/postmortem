#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pm_case::load::{DEFAULT_UNALLOC_CHUNK, chunk_ranges, should_defer_indexes};
use pm_case::{
    CASE_FILE, Case, CaseDetails, CaseError, Examiner, LoadOptions, LoadProgress, SUBDIRS,
    add_image, parse_layout,
};
use pm_casedb::{FileKindCode, FileRecord, Origin};
use pm_core::ReadAt;
use pm_fs::StreamKind;
use pm_fs::walk::{WalkOptions, walk};
use proptest::prelude::*;

fn details() -> CaseDetails {
    CaseDetails {
        name: "Operation Teapot".into(),
        number: "2026-0042".into(),
        examiner: Examiner {
            name: "J. Doe".into(),
            phone: "+358 40 000 0000".into(),
            email: "jdoe@example.com".into(),
        },
        notes: "seized laptop".into(),
    }
}

/// The demo disk image (NTFS + FAT32 on MBR), written once per test run.
fn demo_image() -> &'static Path {
    static IMG: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, p) = IMG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("demo.dd");
        std::fs::write(&p, pm_testkit::demo::demo_disk()).unwrap();
        (dir, p)
    });
    p
}

fn loaded_case() -> (tempfile::TempDir, Case, pm_case::LoadSummary) {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), details()).unwrap();
    let progress = LoadProgress::default();
    let summary = add_image(&case, demo_image(), &LoadOptions::default(), &progress).unwrap();
    (tmp, case, summary)
}

fn all_files(case: &Case, ds_id: i64) -> Vec<FileRecord> {
    let q = pm_casedb::FileQuery {
        ds_id: Some(ds_id),
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = case.db().query_files(&q, after, 1000).unwrap();
        let Some(last) = page.last() else { break };
        after = last.id;
        out.extend(page);
    }
    out
}

fn read_all(src: &dyn ReadAt) -> Vec<u8> {
    src.read_vec_at(0, usize::try_from(src.size()).unwrap())
        .unwrap()
}

// ------------------------------------------------------------ case folder

#[test]
fn create_writes_layout_and_reopens() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("case");
    let id = {
        let case = Case::create(&dir, details()).unwrap();
        assert_eq!(case.details(), details());
        case.id()
    };
    assert!(dir.join(CASE_FILE).is_file());
    assert!(dir.join(pm_case::DB_FILE).is_file());
    for sub in SUBDIRS {
        assert!(dir.join(sub).is_dir(), "{sub}");
    }
    // Opening by folder and by case file are equivalent.
    for p in [dir.clone(), dir.join(CASE_FILE)] {
        let case = Case::open(&p).unwrap();
        assert_eq!(case.id(), id);
        assert_eq!(case.details(), details());
        assert_eq!(case.db().meta("case_id").unwrap(), Some(id.clone()));
    }
}

#[test]
fn create_refuses_non_empty_folder() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("something"), b"x").unwrap();
    assert!(matches!(
        Case::create(tmp.path(), details()),
        Err(CaseError::FolderNotEmpty(_))
    ));
    // An existing empty folder is fine.
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    Case::create(&empty, details()).unwrap();
}

#[test]
fn details_are_editable_and_persisted() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("case");
    let case = Case::create(&dir, details()).unwrap();
    let mut d = details();
    d.notes = "updated \"notes\"\nwith newline".into();
    case.set_details(d.clone()).unwrap();
    drop(case);
    assert_eq!(Case::open(&dir).unwrap().details(), d);
    assert!(!dir.join(format!("{CASE_FILE}.tmp")).exists());
}

#[test]
fn bad_case_files_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("case");
    drop(Case::create(&dir, details()).unwrap());
    let file = dir.join(CASE_FILE);
    let good = std::fs::read_to_string(&file).unwrap();

    std::fs::write(&file, "not = [valid").unwrap();
    assert!(matches!(
        Case::open(&dir),
        Err(CaseError::BadCaseFile { .. })
    ));

    std::fs::write(&file, good.replace("version = 1", "version = 99")).unwrap();
    let err = Case::open(&dir).unwrap_err().to_string();
    assert!(err.contains("newer"), "{err}");

    std::fs::write(&file, good.replace("postmortem-case", "other")).unwrap();
    assert!(matches!(
        Case::open(&dir),
        Err(CaseError::BadCaseFile { .. })
    ));

    std::fs::write(&file, &good).unwrap();
    std::fs::remove_file(dir.join(pm_case::DB_FILE)).unwrap();
    assert!(matches!(
        Case::open(&dir),
        Err(CaseError::BadCaseFile { .. })
    ));
}

#[test]
fn opening_a_missing_case_is_an_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        Case::open(tmp.path().join("nope")),
        Err(CaseError::Io { .. })
    ));
}

// ---------------------------------------------------------------- loading

#[test]
fn loads_structure_of_the_demo_disk() {
    let (_tmp, case, s) = loaded_case();
    let db = case.db();
    let ds = db.data_source(s.ds_id).unwrap().unwrap();
    assert_eq!(ds.name, "demo.dd");
    assert_eq!(ds.paths, vec![demo_image().display().to_string()]);
    assert_eq!(ds.size, std::fs::metadata(demo_image()).unwrap().len());
    assert_eq!(ds.image_format.as_deref(), Some("raw"));
    assert_eq!(ds.timezone, "UTC");

    let ev = pm_evidence::open(demo_image(), &pm_evidence::OpenOptions::default()).unwrap();
    let vols = db.volumes(s.ds_id).unwrap();
    assert_eq!(
        vols.len(),
        ev.volume_system.as_ref().unwrap().partitions.len()
    );

    let fss = db.file_systems(s.ds_id).unwrap();
    let kinds: Vec<&str> = fss.iter().map(|f| f.kind.as_str()).collect();
    assert_eq!(kinds, ["NTFS", "FAT32"]);
    assert_eq!(s.fs_ids, fss.iter().map(|f| f.id).collect::<Vec<_>>());
    for (rec, found) in fss.iter().zip(&ev.filesystems) {
        assert_eq!(rec.byte_offset, found.offset);
        let vol = vols.iter().find(|v| Some(v.id) == rec.volume_id).unwrap();
        assert_eq!(Some(vol.addr), found.partition.as_ref().map(|p| p.addr));
    }
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
}

/// Every name the reference walker sees becomes exactly one row with the
/// same path, allocation state, size and times.
#[test]
fn rows_match_a_reference_walk() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    assert_eq!(s.files, files.len() as u64);

    let ev = pm_evidence::open(demo_image(), &pm_evidence::OpenOptions::default()).unwrap();
    for (found, &fs_id) in ev.filesystems.iter().zip(&s.fs_ids) {
        let rows: HashMap<String, &FileRecord> = files
            .iter()
            .filter(|f| f.fs_id == Some(fs_id) && f.origin == Origin::FileSystem)
            .filter(|f| f.stream.is_none() && f.parent_id.is_some())
            .filter(|f| !f.parent_path.starts_with("/$OrphanFiles/"))
            .map(|f| (f.path(), f))
            .collect();
        let mut seen = 0usize;
        walk(&*found.fs, &WalkOptions::default(), |item| {
            let path = item.path();
            let row = rows
                .get(&path)
                .unwrap_or_else(|| panic!("{path} missing from case"));
            assert_eq!(row.inode, Some(item.entry.inode), "{path}");
            assert_eq!(row.name_alloc, item.entry.allocated, "{path}");
            let m = item.meta.as_ref().unwrap();
            assert_eq!(row.size, m.size, "{path}");
            assert_eq!(row.meta_alloc, m.is_allocated(), "{path}");
            assert_eq!(
                row.times[0].map(|t| t.0),
                m.times.modified.map(jiff::Timestamp::as_second),
                "{path}"
            );
            seen += 1;
            ControlFlow::Continue(())
        });
        assert_eq!(seen, rows.len(), "fs {fs_id}: extra rows in the case");
    }
}

#[test]
fn tree_links_are_consistent() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    let by_id: HashMap<i64, &FileRecord> = files.iter().map(|f| (f.id, f)).collect();
    let ids: HashSet<i64> = by_id.keys().copied().collect();
    assert_eq!(ids.len(), files.len(), "duplicate ids");
    for f in &files {
        match f.parent_id {
            None => {
                assert_eq!(f.parent_path, "/");
                assert_eq!(f.name, "");
                assert_eq!(f.kind, FileKindCode::Directory);
            }
            Some(p) => {
                let parent = by_id[&p];
                assert_eq!(parent.kind, FileKindCode::Directory, "{}", f.path());
                let expect = if parent.parent_id.is_none() {
                    "/".to_owned()
                } else {
                    format!("{}/", parent.path())
                };
                assert_eq!(f.parent_path, expect, "{}", f.path());
                assert_eq!(f.fs_id, parent.fs_id);
            }
        }
    }
    // One root per file system, and the DB finds it.
    for fs_id in &s.fs_ids {
        let root = case.db().fs_root(*fs_id).unwrap().unwrap();
        assert!(root.parent_id.is_none());
        let names: Vec<String> = case
            .db()
            .children(root.id)
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"$OrphanFiles".to_owned()), "{names:?}");
    }
}

#[test]
fn file_content_matches_the_file_system() {
    let (_tmp, case, s) = loaded_case();
    let ev = pm_evidence::open(demo_image(), &pm_evidence::OpenOptions::default()).unwrap();
    let files = all_files(&case, s.ds_id);
    let mut checked = 0;
    for f in files
        .iter()
        .filter(|f| f.origin == Origin::FileSystem && f.kind == FileKindCode::Regular)
    {
        let idx = s.fs_ids.iter().position(|id| Some(*id) == f.fs_id).unwrap();
        let fs = &ev.filesystems[idx].fs;
        let want = read_all(&*fs.open(f.inode.unwrap(), f.stream).unwrap());
        let got = read_all(&*case.content(f).unwrap());
        assert_eq!(got, want, "{}", f.path());
        checked += 1;
    }
    assert!(checked > 10, "only {checked} files checked");
}

#[test]
fn alternate_data_streams_are_rows() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    let ads = files
        .iter()
        .find(|f| f.name == "setup.exe:Zone.Identifier")
        .expect("ADS row");
    assert!(ads.stream.is_some());
    assert_eq!(ads.parent_path, "/Users/alice/Downloads/");
    assert_eq!(ads.extension, "");
    let exe = files.iter().find(|f| f.name == "setup.exe").unwrap();
    assert_eq!(ads.inode, exe.inode);
    assert_eq!(ads.parent_id, exe.parent_id);
    let body = read_all(&*case.content(ads).unwrap());
    assert!(body.starts_with(b"[ZoneTransfer]"));

    // Every ADS the file system reports has a row.
    let fs = case.filesystem(ads.fs_id.unwrap()).unwrap();
    let streams = fs.streams(exe.inode.unwrap()).unwrap();
    let n = streams
        .iter()
        .filter(|s| s.kind == StreamKind::AlternateData)
        .count();
    let rows = files
        .iter()
        .filter(|f| f.inode == exe.inode && f.stream.is_some() && f.fs_id == exe.fs_id)
        .count();
    assert_eq!(rows, n);
}

#[test]
fn deleted_files_are_flagged() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    let del = files
        .iter()
        .find(|f| f.name == "deleted_photo.bmp")
        .expect("deleted FAT file");
    assert!(!del.name_alloc);
    assert!(del.deleted());
    let q = pm_casedb::FileQuery {
        ds_id: Some(s.ds_id),
        deleted_only: true,
        ..Default::default()
    };
    assert!(case.db().count_files(&q).unwrap() >= 1);
}

#[test]
fn unallocated_space_files_cover_free_blocks() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    let image = std::fs::read(demo_image()).unwrap();
    for &fs_id in &s.fs_ids {
        let fs = case.filesystem(fs_id).unwrap();
        let rec = case.db().file_system(fs_id).unwrap().unwrap();
        let bs = u64::from(fs.info().block_size);
        let free: u64 = fs
            .unallocated_blocks()
            .unwrap()
            .iter()
            .map(|r| r.1 * bs)
            .sum();
        let unalloc: Vec<&FileRecord> = files
            .iter()
            .filter(|f| f.fs_id == Some(fs_id) && f.origin == Origin::UnallocBlocks)
            .collect();
        assert_eq!(unalloc.iter().map(|f| f.size).sum::<u64>(), free);
        for f in unalloc {
            assert_eq!(f.parent_path, "/$Unalloc/");
            assert!(
                f.name.starts_with(&format!("Unalloc_{fs_id}_")),
                "{}",
                f.name
            );
            assert!(f.size <= DEFAULT_UNALLOC_CHUNK);
            let layout = parse_layout(&case.db().file_layout(f.id).unwrap().unwrap()).unwrap();
            let mut want = Vec::new();
            for (off, len) in &layout {
                assert!(*off >= rec.byte_offset);
                want.extend_from_slice(&image[*off as usize..(*off + *len) as usize]);
            }
            assert_eq!(read_all(&*case.content(f).unwrap()), want, "{}", f.name);
        }
    }
}

#[test]
fn reopened_case_reads_content_from_evidence() {
    let (tmp, case, s) = loaded_case();
    let dir = case.dir().to_path_buf();
    let files = all_files(&case, s.ds_id);
    drop(case);
    let case = Case::open(&dir).unwrap();
    let f = files
        .iter()
        .find(|f| f.kind == FileKindCode::Regular && f.size > 0 && f.origin == Origin::FileSystem)
        .unwrap();
    assert_eq!(read_all(&*case.content(f).unwrap()).len() as u64, f.size);
    drop(tmp);
}

#[test]
fn moved_evidence_is_reported_and_can_be_relocated() {
    let tmp = tempfile::tempdir().unwrap();
    let img = tmp.path().join("disk.dd");
    std::fs::copy(demo_image(), &img).unwrap();
    let dir = tmp.path().join("case");
    let ds_id = {
        let case = Case::create(&dir, details()).unwrap();
        add_image(
            &case,
            &img,
            &LoadOptions::default(),
            &LoadProgress::default(),
        )
        .unwrap()
        .ds_id
    };
    let moved = tmp.path().join("moved.dd");
    std::fs::rename(&img, &moved).unwrap();

    let case = Case::open(&dir).unwrap();
    let fs_id = case.db().file_systems(ds_id).unwrap()[0].id;
    assert!(matches!(
        case.filesystem(fs_id),
        Err(CaseError::MissingEvidence { .. })
    ));
    case.relocate_data_source(ds_id, &[moved]).unwrap();
    case.filesystem(fs_id).unwrap();
}

#[test]
fn cancelled_load_leaves_no_trace() {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), details()).unwrap();
    let progress = LoadProgress::default();
    progress.cancel();
    let err = add_image(&case, demo_image(), &LoadOptions::default(), &progress).unwrap_err();
    assert!(matches!(err, CaseError::Cancelled));
    assert!(case.db().data_sources().unwrap().is_empty());
    let rows: i64 = case
        .db()
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn second_data_source_gets_fresh_ids() {
    let (_tmp, case, first) = loaded_case();
    let second = add_image(
        &case,
        demo_image(),
        &LoadOptions {
            name: Some("copy".into()),
            ..LoadOptions::default()
        },
        &LoadProgress::default(),
    )
    .unwrap();
    assert_ne!(first.ds_id, second.ds_id);
    assert_eq!(first.files, second.files);
    let a = all_files(&case, first.ds_id);
    let b = all_files(&case, second.ds_id);
    let max_a = a.iter().map(|f| f.id).max().unwrap();
    assert!(b.iter().all(|f| f.id > max_a));
    assert_eq!(
        case.db().data_source(second.ds_id).unwrap().unwrap().name,
        "copy"
    );

    case.delete_data_source(first.ds_id).unwrap();
    assert!(all_files(&case, first.ds_id).is_empty());
    assert_eq!(all_files(&case, second.ds_id).len(), b.len());
}

#[test]
fn options_disable_orphans_and_unallocated() {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), details()).unwrap();
    let opts = LoadOptions {
        process_unallocated: false,
        fat_orphans: false,
        threads: 1,
        ..LoadOptions::default()
    };
    let s = add_image(&case, demo_image(), &opts, &LoadProgress::default()).unwrap();
    let files = all_files(&case, s.ds_id);
    assert!(!files.iter().any(|f| f.origin == Origin::UnallocBlocks));
    let orphan_dirs: Vec<Option<i64>> = files
        .iter()
        .filter(|f| f.name == "$OrphanFiles")
        .map(|f| f.fs_id)
        .collect();
    // NTFS still gets one; FAT does not.
    assert_eq!(orphan_dirs, vec![Some(s.fs_ids[0])]);
}

#[test]
fn single_threaded_and_parallel_loads_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for threads in [1, 8] {
        let case = Case::create(tmp.path().join(format!("c{threads}")), details()).unwrap();
        let opts = LoadOptions {
            threads,
            ..LoadOptions::default()
        };
        let s = add_image(&case, demo_image(), &opts, &LoadProgress::default()).unwrap();
        let mut p: Vec<(String, Option<u32>, u64)> = all_files(&case, s.ds_id)
            .into_iter()
            .map(|f| (f.path(), f.stream, f.size))
            .collect();
        p.sort();
        paths.push(p);
    }
    assert_eq!(paths[0], paths[1]);
}

#[test]
fn virtual_files_of_fat_are_kept() {
    let (_tmp, case, s) = loaded_case();
    let files = all_files(&case, s.ds_id);
    let fat_virtual: Vec<&str> = files
        .iter()
        .filter(|f| f.fs_id == Some(s.fs_ids[1]) && f.kind == FileKindCode::Virtual)
        .map(|f| f.name.as_str())
        .collect();
    assert!(fat_virtual.contains(&"$MBR"), "{fat_virtual:?}");
}

// ------------------------------------------------------------- pure logic

#[test]
fn index_deferral_heuristic() {
    assert!(!should_defer_indexes(0, 10));
    assert!(should_defer_indexes(0, 1_000_000));
    assert!(should_defer_indexes(1_000_000, 600_000));
    assert!(!should_defer_indexes(100_000_000, 200_000));
}

#[test]
fn chunking_examples() {
    assert_eq!(
        chunk_ranges([(0, 10)], 4),
        vec![vec![(0, 4)], vec![(4, 4)], vec![(8, 2)]]
    );
    assert_eq!(
        chunk_ranges([(0, 3), (10, 3)], 4),
        vec![vec![(0, 3), (10, 1)], vec![(11, 2)]]
    );
    assert_eq!(
        chunk_ranges([(0, 3), (10, 0), (20, 2)], 0),
        vec![vec![(0, 3)], vec![(20, 2)]]
    );
    assert!(chunk_ranges([], 4).is_empty());
}

proptest! {
    #[test]
    fn chunking_preserves_bytes(
        runs in prop::collection::vec((0u64..1_000_000, 0u64..5_000), 0..40),
        chunk in 0u64..20_000,
    ) {
        // Make runs ascending and disjoint, as file systems report them.
        let mut off = 0;
        let ranges: Vec<(u64, u64)> = runs.iter().map(|&(gap, len)| {
            off += gap;
            let r = (off, len);
            off += len;
            r
        }).collect();
        let chunks = chunk_ranges(ranges.clone(), chunk);
        let flat: Vec<u64> = chunks.iter().flatten().flat_map(|&(o, l)| o..o + l).collect();
        let want: Vec<u64> = ranges.iter().flat_map(|&(o, l)| o..o + l).collect();
        prop_assert_eq!(flat, want);
        for c in &chunks {
            prop_assert!(!c.is_empty());
            let total: u64 = c.iter().map(|r| r.1).sum();
            prop_assert!(total > 0);
            if chunk > 0 {
                prop_assert!(total <= chunk);
            }
        }
        if chunk > 0 {
            // All chunks but the last are full.
            for c in chunks.iter().rev().skip(1) {
                prop_assert_eq!(c.iter().map(|r| r.1).sum::<u64>(), chunk);
            }
        }
    }
}
