//! Derived files and the shared file-id allocator.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::HashSet;

use pm_casedb::*;

fn new_db() -> (tempfile::TempDir, CaseDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = CaseDb::create(dir.path().join("case.db")).unwrap();
    (dir, db)
}

fn data_source(db: &CaseDb) -> i64 {
    db.bulk(|b| {
        b.add_data_source(&NewDataSource {
            name: "img".into(),
            kind: DataSourceKind::DiskImage,
            paths: vec![],
            size: 1,
            sector_size: 512,
            timezone: "UTC".into(),
            image_format: None,
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: vec![],
            added_at: 0,
        })
    })
    .unwrap()
}

/// `/a.zip` plus `a.zip/dir/` and `a.zip/dir/x.txt` as derived files.
fn archive_tree(db: &CaseDb, ds: i64) -> (FileRecord, Vec<FileRecord>) {
    let mut zip = NewFile::new(
        ds,
        None,
        "/",
        "a.zip",
        Origin::FileSystem,
        FileKindCode::Regular,
    );
    zip.size = 10;
    let zip_id = db.bulk(|b| b.add_file(&zip)).unwrap();
    let zip = db.file(zip_id).unwrap().unwrap();

    let ids = db.file_ids().reserve(2);
    let mut dir = NewFile::new(
        ds,
        Some(zip_id),
        "/a.zip/",
        "dir",
        Origin::Derived,
        FileKindCode::Directory,
    );
    dir.id = Some(ids.start);
    dir.derived_from = Some(zip_id);
    let mut x = NewFile::new(
        ds,
        Some(ids.start),
        "/a.zip/dir/",
        "x.txt",
        Origin::Derived,
        FileKindCode::Regular,
    );
    x.id = Some(ids.start + 1);
    x.derived_from = Some(zip_id);
    x.size = 3;
    x.times = [Some((1_600_000_000, 0)), None, None, None];
    x.local_path = Some("ModuleOutput/EFE/1/0/1".into());
    let recs = db.add_derived_files(&[dir, x]).unwrap();
    (zip, recs)
}

#[test]
fn derived_files_round_trip() {
    let (_d, db) = new_db();
    let ds = data_source(&db);
    let (zip, recs) = archive_tree(&db, ds);
    assert_eq!(recs.len(), 2);
    // The in-memory records match what the database returns.
    for r in &recs {
        assert_eq!(&db.file(r.id).unwrap().unwrap(), r);
    }
    let x = &recs[1];
    assert_eq!(x.path(), "/a.zip/dir/x.txt");
    assert_eq!(x.extension, "txt");
    assert!(db.has_children(zip.id).unwrap());
    assert!(!db.has_children(x.id).unwrap());
    assert_eq!(
        db.child_named(zip.id, "dir").unwrap().unwrap().id,
        recs[0].id
    );
    assert_eq!(db.child_named(zip.id, "nope").unwrap(), None);
    let of = db.derived_files_of(zip.id, 0, 10).unwrap();
    assert_eq!(
        of.iter().map(|f| f.id).collect::<Vec<_>>(),
        [recs[0].id, recs[1].id]
    );
    assert_eq!(
        db.derived_files_of(zip.id, recs[0].id, 10).unwrap().len(),
        1
    );
}

#[test]
fn update_derived_file_resets_hashes() {
    let (_d, db) = new_db();
    let ds = data_source(&db);
    let (_, recs) = archive_tree(&db, ds);
    let x = &recs[1];
    db.set_file_analysis(
        x.id,
        &FileAnalysis {
            mime: Some("application/octet-stream".into()),
            md5: Some(vec![1; 16]),
            ..FileAnalysis::default()
        },
    )
    .unwrap();
    db.update_derived_file(
        x.id,
        &DerivedUpdate {
            size: 99,
            times: [None, Some((5, 6)), None, None],
            local_path: Some("ModuleOutput/EFE/1/0/9".into()),
            reset_mime: true,
        },
    )
    .unwrap();
    let after = db.file(x.id).unwrap().unwrap();
    assert_eq!(after.size, 99);
    assert_eq!(after.times, [None, Some((5, 6)), None, None]);
    assert_eq!(after.mime, None);
    assert_eq!(after.md5, None);
    assert_eq!(after.local_path.as_deref(), Some("ModuleOutput/EFE/1/0/9"));
}

#[test]
fn deleting_the_data_source_removes_derived_rows() {
    let (_d, db) = new_db();
    let ds = data_source(&db);
    let (_, recs) = archive_tree(&db, ds);
    db.delete_data_source(ds).unwrap();
    assert_eq!(db.file(recs[1].id).unwrap(), None);
}

/// Ids handed out concurrently by many writers never collide, and rows
/// inserted without an explicit id never take a reserved one.
#[test]
fn allocator_is_shared_by_all_writers() {
    let (_d, db) = new_db();
    let ds = data_source(&db);
    let reserved = db.file_ids().reserve(5);
    // A row without an id must not land inside the reserved range.
    let id = db
        .bulk(|b| {
            b.add_file(&NewFile::new(
                ds,
                None,
                "/",
                "plain",
                Origin::FileSystem,
                FileKindCode::Regular,
            ))
        })
        .unwrap();
    assert!(!reserved.contains(&id), "{id} in {reserved:?}");
    // The reserved ids are still insertable.
    for rid in reserved.clone() {
        let mut f = NewFile::new(
            ds,
            None,
            "/",
            format!("r{rid}"),
            Origin::Derived,
            FileKindCode::Regular,
        );
        f.id = Some(rid);
        db.bulk(|b| b.add_file(&f)).unwrap();
    }
    // Concurrent allocation.
    let all: Vec<i64> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    (0..1000)
                        .flat_map(|i| {
                            if i % 2 == 0 {
                                vec![db.file_ids().next()]
                            } else {
                                db.file_ids().reserve(3).collect()
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    let set: HashSet<i64> = all.iter().copied().collect();
    assert_eq!(set.len(), all.len());
    assert!(set.iter().all(|i| *i > reserved.end));
}

#[test]
fn explicit_ids_advance_the_allocator() {
    let (_d, db) = new_db();
    let ds = data_source(&db);
    let mut f = NewFile::new(
        ds,
        None,
        "/",
        "far",
        Origin::FileSystem,
        FileKindCode::Regular,
    );
    let far = db.file_ids().peek() + 1000;
    f.id = Some(far);
    db.bulk(|b| b.add_file(&f)).unwrap();
    assert!(db.file_ids().next() > far);
}

#[test]
fn allocator_resumes_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("case.db");
    let last = {
        let db = CaseDb::create(&path).unwrap();
        let ds = data_source(&db);
        let (_, recs) = archive_tree(&db, ds);
        recs[1].id
    };
    let db = CaseDb::open(&path).unwrap();
    assert_eq!(db.file_ids().peek(), last + 1);
}

#[test]
fn new_file_defaults() {
    let f = NewFile::new(
        3,
        Some(9),
        "/x/",
        "Y.JPG",
        Origin::Carved,
        FileKindCode::Regular,
    );
    let r = f.to_record(77);
    assert_eq!((r.id, r.ds_id, r.parent_id), (77, 3, Some(9)));
    assert_eq!(r.extension, "jpg");
    assert!(r.name_alloc && r.meta_alloc);
    assert_eq!(r.fs_id, None);
    assert_eq!(r.known, Known::Unknown);
}
