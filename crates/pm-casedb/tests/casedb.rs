#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_arguments,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_casedb::catalog::{self, Category, ValueType};
use pm_casedb::*;

fn new_db() -> (tempfile::TempDir, CaseDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = CaseDb::create(dir.path().join("case.db")).unwrap();
    (dir, db)
}

fn ds() -> NewDataSource {
    NewDataSource {
        name: "disk.E01".into(),
        kind: DataSourceKind::DiskImage,
        paths: vec!["/evidence/disk.E01".into()],
        size: 1 << 30,
        sector_size: 512,
        timezone: "UTC".into(),
        image_format: Some("EWF".into()),
        md5: Some("abc".into()),
        sha1: None,
        sha256: None,
        acquisition: vec![("Examiner".into(), "Ä".into())],
        added_at: 1_700_000_000,
    }
}

fn file(
    ds_id: i64,
    fs_id: i64,
    parent: Option<i64>,
    parent_path: &str,
    name: &str,
    kind: FileKindCode,
    size: u64,
    alloc: bool,
) -> NewFile {
    NewFile {
        id: None,
        ds_id,
        fs_id: Some(fs_id),
        parent_id: parent,
        origin: Origin::FileSystem,
        inode: Some(size),
        stream: None,
        name: name.into(),
        parent_path: parent_path.into(),
        kind,
        name_alloc: alloc,
        meta_alloc: alloc,
        meta_flags: 0,
        size,
        times: [
            Some((1_614_853_230, 5)),
            None,
            None,
            Some((1_600_000_000, 0)),
        ],
        mode: Some(0o100_644),
        uid: Some(1000),
        gid: Some(1000),
        seq: Some(1),
        attributes: None,
        derived_from: None,
        local_path: None,
        layout: None,
    }
}

/// ds → fs → root → {docs/ → [a.txt, B.JPG, gone.doc(deleted)], z.exe}
fn populate(db: &CaseDb) -> (i64, i64, i64, i64) {
    db.bulk(|b| {
        let ds_id = b.add_data_source(&ds())?;
        let vs = b.add_volume_system(ds_id, 0, 512, None)?;
        let vol = b.add_volume(
            vs,
            &NewVolume {
                addr: 2,
                kind: 0,
                start_sector: 2048,
                sector_count: 4096,
                description: "NTFS / exFAT (0x07)".into(),
                name: None,
                flags: 0x80,
            },
        )?;
        let fs_id = b.add_file_system(&NewFileSystem {
            ds_id,
            volume_id: Some(vol),
            kind: "NTFS".into(),
            byte_offset: 1_048_576,
            block_size: 4096,
            block_count: 512,
            root_inode: 5,
            label: Some("WIN".into()),
            serial: None,
        })?;
        let root = b.add_file(&file(
            ds_id,
            fs_id,
            None,
            "/",
            "",
            FileKindCode::Directory,
            0,
            true,
        ))?;
        let docs = b.add_file(&file(
            ds_id,
            fs_id,
            Some(root),
            "/",
            "docs",
            FileKindCode::Directory,
            0,
            true,
        ))?;
        b.add_file(&file(
            ds_id,
            fs_id,
            Some(docs),
            "/docs/",
            "a.txt",
            FileKindCode::Regular,
            10,
            true,
        ))?;
        b.add_file(&file(
            ds_id,
            fs_id,
            Some(docs),
            "/docs/",
            "B.JPG",
            FileKindCode::Regular,
            5_000_000,
            true,
        ))?;
        b.add_file(&file(
            ds_id,
            fs_id,
            Some(docs),
            "/docs/",
            "gone.doc",
            FileKindCode::Regular,
            300,
            false,
        ))?;
        b.add_file(&file(
            ds_id,
            fs_id,
            Some(root),
            "/",
            "z.exe",
            FileKindCode::Regular,
            7,
            true,
        ))?;
        Ok((ds_id, fs_id, root, docs))
    })
    .unwrap()
}

#[test]
fn create_open_and_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("case.db");
    {
        let db = CaseDb::create(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        db.set_meta("case_name", "Case 1").unwrap();
        populate(&db);
    }
    assert!(matches!(CaseDb::create(&path), Err(DbError::Exists(_))));
    let db = CaseDb::open(&path).unwrap();
    assert_eq!(db.meta("case_name").unwrap().as_deref(), Some("Case 1"));
    assert_eq!(db.data_sources().unwrap().len(), 1);
    // A database from a newer postmortem is refused, not corrupted.
    db.set_meta("schema_version", "999").unwrap();
    drop(db);
    assert!(matches!(
        CaseDb::open(&path),
        Err(DbError::TooNew { found: 999, .. })
    ));
}

#[test]
fn data_source_volumes_and_file_systems_roundtrip() {
    let (_d, db) = new_db();
    let (ds_id, fs_id, ..) = populate(&db);
    let d = &db.data_sources().unwrap()[0];
    assert_eq!(d.id, ds_id);
    assert_eq!(d.paths, ["/evidence/disk.E01"]);
    assert_eq!(d.acquisition, [("Examiner".to_owned(), "Ä".to_owned())]);
    assert_eq!(
        db.volumes(ds_id).unwrap()[0].description,
        "NTFS / exFAT (0x07)"
    );
    let fs = &db.file_systems(ds_id).unwrap()[0];
    assert_eq!(
        (fs.id, fs.kind.as_str(), fs.byte_offset),
        (fs_id, "NTFS", 1_048_576)
    );
}

#[test]
fn tree_navigation() {
    let (_d, db) = new_db();
    let (_, fs_id, root, docs) = populate(&db);
    assert_eq!(db.fs_root(fs_id).unwrap().unwrap().id, root);
    let names: Vec<_> = db
        .children(root)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    assert_eq!(names, ["docs", "z.exe"], "directories first");
    let kids = db.children(docs).unwrap();
    let names: Vec<_> = kids.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        ["a.txt", "B.JPG", "gone.doc"],
        "case-insensitive name order"
    );
    let jpg = &kids[1];
    assert_eq!(jpg.extension, "jpg");
    assert_eq!(jpg.path(), "/docs/B.JPG");
    assert_eq!(jpg.times[0], Some((1_614_853_230, 5)));
    assert_eq!(jpg.times[1], None);
    assert!(kids[2].deleted());
    assert_eq!(db.file(jpg.id).unwrap().unwrap(), *jpg);
}

#[test]
fn file_queries_and_keyset_paging() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let q = FileQuery {
        ds_id: Some(ds_id),
        regular_only: true,
        ..Default::default()
    };
    assert_eq!(db.count_files(&q).unwrap(), 4);
    let page1 = db.query_files(&q, 0, 2).unwrap();
    let page2 = db.query_files(&q, page1.last().unwrap().id, 2).unwrap();
    assert_eq!(page1.len() + page2.len(), 4);
    assert!(page2.iter().all(|f| f.id > page1[1].id));
    let pics = FileQuery {
        extensions: vec!["JPG".into()],
        ..Default::default()
    };
    assert_eq!(db.count_files(&pics).unwrap(), 1);
    let deleted = FileQuery {
        deleted_only: true,
        ..Default::default()
    };
    assert_eq!(db.query_files(&deleted, 0, 10).unwrap()[0].name, "gone.doc");
    let big = FileQuery {
        min_size: Some(1_000),
        max_size: Some(10_000_000),
        ..Default::default()
    };
    assert_eq!(db.count_files(&big).unwrap(), 1);
    let named = FileQuery {
        name_contains: Some("one".into()),
        ..Default::default()
    };
    assert_eq!(db.count_files(&named).unwrap(), 1);
    let ext = db.extension_counts(Some(ds_id)).unwrap();
    assert!(ext.contains(&("jpg".to_owned(), 1)));
}

#[test]
fn view_conditions() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let all = db.query_files(&FileQuery::default(), 0, 100).unwrap();
    let id = |n: &str| all.iter().find(|f| f.name == n).unwrap().id;
    db.set_file_analysis(
        id("B.JPG"),
        &FileAnalysis {
            mime: Some("image/jpeg".into()),
            known: Some(Known::Known),
            ..Default::default()
        },
    )
    .unwrap();
    db.set_file_analysis(
        id("a.txt"),
        &FileAnalysis {
            mime: Some("text/plain".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let count = |q: FileQuery| db.count_files(&q).unwrap();
    assert_eq!(
        count(FileQuery {
            mime: Some("image/jpeg".into()),
            ..Default::default()
        }),
        1
    );
    assert_eq!(
        count(FileQuery {
            mime: Some("image/jpeg".into()),
            hide_known: true,
            ..Default::default()
        }),
        0
    );
    let regular = count(FileQuery {
        regular_only: true,
        ..Default::default()
    });
    assert_eq!(
        count(FileQuery {
            regular_only: true,
            fs_only: true,
            ..Default::default()
        }),
        regular
    );
    assert_eq!(
        db.mime_counts(Some(ds_id)).unwrap(),
        vec![("image/jpeg".to_owned(), 1), ("text/plain".to_owned(), 1)]
    );
}

#[test]
fn analysis_updates() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let f = db
        .query_files(
            &FileQuery {
                extensions: vec!["txt".into()],
                ..Default::default()
            },
            0,
            1,
        )
        .unwrap()
        .remove(0);
    let md5 = vec![0xAA; 16];
    db.set_file_analysis(
        f.id,
        &FileAnalysis {
            mime: Some("text/plain".into()),
            md5: Some(md5.clone()),
            known: Some(Known::Known),
            ..Default::default()
        },
    )
    .unwrap();
    let g = db.file(f.id).unwrap().unwrap();
    assert_eq!(
        (g.mime.as_deref(), g.md5.as_deref(), g.known),
        (Some("text/plain"), Some(md5.as_slice()), Known::Known)
    );
    // A later partial update keeps earlier values (COALESCE semantics).
    db.set_file_analysis(
        f.id,
        &FileAnalysis {
            sha256: Some(vec![1; 32]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        db.file(f.id).unwrap().unwrap().mime.as_deref(),
        Some("text/plain")
    );
    assert_eq!(
        db.count_files(&FileQuery {
            md5: Some(md5),
            ds_id: Some(ds_id),
            ..Default::default()
        })
        .unwrap(),
        1
    );
    assert_eq!(
        db.count_files(&FileQuery {
            mime_prefix: Some("text/".into()),
            ..Default::default()
        })
        .unwrap(),
        1
    );
}

#[test]
fn artifacts_attributes_and_scores() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let f = db
        .query_files(
            &FileQuery {
                extensions: vec!["exe".into()],
                ..Default::default()
            },
            0,
            1,
        )
        .unwrap()
        .remove(0);
    let set_name = catalog::attribute_type_id("TSK_SET_NAME").unwrap();
    let hit = db
        .add_artifact(&NewArtifact {
            type_id: catalog::artifact_type_id("TSK_HASHSET_HIT").unwrap(),
            ds_id,
            file_id: Some(f.id),
            source_artifact_id: None,
            score: Some((Significance::Notable, Priority::Normal)),
            conclusion: Some("Known malware".into()),
            configuration: Some("Malware DB".into()),
            justification: None,
            created_at: 1,
            attributes: vec![(
                set_name,
                Value::Text("Malware DB".into()),
                "Hash Lookup".into(),
            )],
        })
        .unwrap();
    let url = catalog::attribute_type_id("TSK_URL").unwrap();
    let when = catalog::attribute_type_id("TSK_DATETIME_ACCESSED").unwrap();
    db.add_artifact(&NewArtifact {
        type_id: catalog::artifact_type_id("TSK_WEB_HISTORY").unwrap(),
        ds_id,
        file_id: Some(f.id),
        source_artifact_id: None,
        score: None,
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 2,
        attributes: vec![
            (
                url,
                Value::Text("https://example.com".into()),
                "Recent Activity".into(),
            ),
            (
                when,
                Value::DateTime(1_600_000_000),
                "Recent Activity".into(),
            ),
        ],
    })
    .unwrap();
    let counts = db.artifact_counts(Some(ds_id)).unwrap();
    assert_eq!(counts, [(4, 1), (10, 1)]);
    let a = &db.artifacts_of_type(10, None, 0, 10).unwrap()[0];
    assert_eq!((a.id, a.significance), (hit, Some(Significance::Notable)));
    let attrs = db.attributes(hit).unwrap();
    assert_eq!(
        (attrs[0].name.as_str(), attrs[0].value.clone()),
        ("TSK_SET_NAME", Value::Text("Malware DB".into()))
    );
    let web = db
        .artifacts_for_file(f.id)
        .unwrap()
        .into_iter()
        .find(|a| a.type_id == 4)
        .unwrap();
    let web_attrs = db.attributes(web.id).unwrap();
    assert_eq!(web_attrs[1].value, Value::DateTime(1_600_000_000));
    assert_eq!(db.file_score(f.id).unwrap(), Significance::Notable);
}

#[test]
fn custom_types() {
    let (_d, db) = new_db();
    let shell = db
        .artifact_type("RA_SHELL_BAG", "Shell Bags", Category::DataArtifact)
        .unwrap();
    assert_eq!(shell, 73, "custom types follow the standard ids");
    assert_eq!(
        db.artifact_type("RA_SHELL_BAG", "Shell Bags", Category::DataArtifact)
            .unwrap(),
        shell
    );
    let key = db
        .attribute_type("RA_SHELL_BAG_KEY", "Key", ValueType::String)
        .unwrap();
    assert_eq!(
        db.attribute_type("RA_SHELL_BAG_KEY", "Key", ValueType::String)
            .unwrap(),
        key
    );
    assert!(
        db.artifact_type_names()
            .unwrap()
            .iter()
            .any(|t| t.1 == "RA_SHELL_BAG")
    );
}

#[test]
fn tags_and_notable() {
    let (_d, db) = new_db();
    populate(&db);
    let names = db.tag_names().unwrap();
    assert_eq!(
        names.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["Bookmark", "Follow Up", "Notable Item"]
    );
    let notable = names.iter().find(|t| t.notable).unwrap().id;
    let bookmark = names[0].id;
    let f = db
        .query_files(
            &FileQuery {
                extensions: vec!["txt".into()],
                ..Default::default()
            },
            0,
            1,
        )
        .unwrap()
        .remove(0);
    let t1 = db.tag_file(f.id, bookmark, "look", "alice", 5).unwrap();
    assert_eq!(db.file(f.id).unwrap().unwrap().known, Known::Unknown);
    assert_eq!(db.file_score(f.id).unwrap(), Significance::Unknown);
    let t2 = db.tag_file(f.id, notable, "", "alice", 6).unwrap();
    assert_eq!(
        db.file_score(f.id).unwrap(),
        Significance::Notable,
        "a notable tag scores the file notable (TSK TaggingManager)"
    );
    assert_eq!(
        db.file(f.id).unwrap().unwrap().known,
        Known::Unknown,
        "tags never change the known status"
    );
    assert_eq!(db.file_tags(f.id).unwrap().len(), 2);
    assert_eq!(db.files_with_tag(bookmark).unwrap()[0].1.id, f.id);
    db.untag_file(t1).unwrap();
    assert_eq!(db.file_tags(f.id).unwrap().len(), 1);
    assert_eq!(db.tag_counts().unwrap(), [(notable, 1)]);
    db.untag_file(t2).unwrap();
    assert_eq!(
        db.file_score(f.id).unwrap(),
        Significance::Unknown,
        "removing the notable tag recomputes the score"
    );
    let custom = db.add_tag_name("Evidence", "", false).unwrap();
    assert!(db.tag_names().unwrap().iter().any(|t| t.id == custom));
}

#[test]
fn delete_data_source_removes_everything() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let f = db
        .query_files(&FileQuery::default(), 0, 1)
        .unwrap()
        .remove(0);
    db.tag_file(f.id, 1, "", "a", 0).unwrap();
    db.add_artifact(&NewArtifact {
        type_id: 4,
        ds_id,
        file_id: Some(f.id),
        source_artifact_id: None,
        score: None,
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 0,
        attributes: vec![(1, Value::Text("u".into()), "m".into())],
    })
    .unwrap();
    db.delete_data_source(ds_id).unwrap();
    assert!(db.data_sources().unwrap().is_empty());
    assert_eq!(db.count_files(&FileQuery::default()).unwrap(), 0);
    assert!(db.artifact_counts(None).unwrap().is_empty());
}

#[test]
fn readers_see_committed_data_while_writing() {
    let (_d, db) = new_db();
    let (_, _, _, docs) = populate(&db);
    std::thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..200 {
                assert_eq!(db.children(docs).unwrap().len(), 3);
            }
        });
        for i in 0..50 {
            db.set_meta("k", &i.to_string()).unwrap();
        }
    });
}

#[test]
fn preassigned_ids_and_layout() {
    let (_d, db) = new_db();
    let (ds_id, fs_id, root, _) = populate(&db);
    let next = db.next_file_id().unwrap();
    let mut f = file(
        ds_id,
        fs_id,
        Some(root),
        "/",
        "Unalloc_1_0_4096",
        FileKindCode::Regular,
        4096,
        false,
    );
    f.id = Some(next + 100);
    f.origin = Origin::UnallocBlocks;
    f.layout = Some("[[0,4096]]".into());
    let id = db.bulk(|b| b.add_file(&f)).unwrap();
    assert_eq!(id, next + 100);
    assert_eq!(db.file_layout(id).unwrap().as_deref(), Some("[[0,4096]]"));
    assert_eq!(db.file(id).unwrap().unwrap().origin, Origin::UnallocBlocks);
}

#[test]
fn ingest_priority_classes() {
    let (_d, db) = new_db();
    let ds_id = db
        .bulk(|b| {
            let ds_id = b.add_data_source(&ds())?;
            let fs_id = b.add_file_system(&NewFileSystem {
                ds_id,
                volume_id: None,
                kind: "NTFS".into(),
                byte_offset: 0,
                block_size: 4096,
                block_count: 1,
                root_inode: 5,
                label: None,
                serial: None,
            })?;
            let d = FileKindCode::Directory;
            let r = FileKindCode::Regular;
            let root = b.add_file(&file(ds_id, fs_id, None, "/", "", d, 0, true))?;
            for (pp, name, kind) in [
                ("/", "Users", d),
                ("/Users/", "alice.txt", r),
                ("/", "users_backup", d),
                ("/", "Windows", d),
                ("/Windows/System32/", "cmd.exe", r),
                ("/", "Program Files", d),
                ("/Program Files/x/", "y.dll", r),
                ("/", "pagefile.sys", r),
                ("/", "HIBERFIL.SYS", r),
                ("/", "$OrphanFiles", d),
                ("/$OrphanFiles/", "lost.doc", r),
                ("/", "Documents and Settings", d),
                ("/home/bob/", ".bashrc", r),
                ("/ProgramData/", "cfg", r),
            ] {
                b.add_file(&file(ds_id, fs_id, Some(root), pp, name, kind, 1, true))?;
            }
            let mut u = file(
                ds_id,
                fs_id,
                Some(root),
                "/$Unalloc/",
                "Unalloc_1_0_9",
                r,
                9,
                false,
            );
            u.origin = Origin::UnallocBlocks;
            b.add_file(&u)?;
            Ok(ds_id)
        })
        .unwrap();
    let class = |p: IngestPriority| -> Vec<String> {
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            // Tiny pages exercise the keyset cursor.
            let page = db.ingest_files(ds_id, p, after, 2).unwrap();
            let Some(last) = page.last() else { break };
            after = last.id;
            out.extend(page.into_iter().map(|f| f.path()));
        }
        out
    };
    assert_eq!(
        class(IngestPriority::High),
        [
            "/Users",
            "/Users/alice.txt",
            "/users_backup",
            "/Documents and Settings",
            "/home/bob/.bashrc",
            "/ProgramData/cfg"
        ]
    );
    assert_eq!(
        class(IngestPriority::Medium),
        ["/", "/Program Files", "/Program Files/x/y.dll"]
    );
    assert_eq!(
        class(IngestPriority::Low),
        [
            "/Windows",
            "/Windows/System32/cmd.exe",
            "/$OrphanFiles",
            "/$OrphanFiles/lost.doc"
        ]
    );
    assert_eq!(
        class(IngestPriority::Last),
        ["/pagefile.sys", "/HIBERFIL.SYS", "/$Unalloc/Unalloc_1_0_9"]
    );
}

#[test]
fn artifacts_are_not_duplicated_on_reingest() {
    let (_d, db) = new_db();
    let (ds_id, ..) = populate(&db);
    let f = db
        .query_files(&FileQuery::default(), 0, 100)
        .unwrap()
        .into_iter()
        .find(|f| f.name == "z.exe")
        .unwrap();
    let set = catalog::attribute_type_id("TSK_SET_NAME").unwrap();
    let comment = catalog::attribute_type_id("TSK_COMMENT").unwrap();
    let hit = |set_name: &str, note: &str| NewArtifact {
        type_id: catalog::artifact_type_id("TSK_HASHSET_HIT").unwrap(),
        ds_id,
        file_id: Some(f.id),
        source_artifact_id: None,
        score: Some((Significance::Notable, Priority::Normal)),
        conclusion: None,
        configuration: Some(set_name.into()),
        justification: None,
        created_at: 1,
        attributes: vec![
            (set, Value::Text(set_name.into()), "Hash Lookup".into()),
            (comment, Value::Text(note.into()), "Hash Lookup".into()),
        ],
    };
    let added = db
        .bulk(|b| {
            Ok([
                b.add_artifact_unless_exists(&hit("NSRL bad", "one"), &[set])?,
                // Same set, different comment: keyed on the set name only.
                b.add_artifact_unless_exists(&hit("NSRL bad", "two"), &[set])?,
                b.add_artifact_unless_exists(&hit("Other set", "x"), &[set])?,
                // Key on both attributes: differs in comment, so it is new.
                b.add_artifact_unless_exists(&hit("Other set", "y"), &[set, comment])?,
            ])
        })
        .unwrap();
    assert!(added[0].is_some());
    assert!(added[1].is_none());
    assert!(added[2].is_some());
    assert!(added[3].is_some());
    assert_eq!(db.artifacts_for_file(f.id).unwrap().len(), 3);
}

#[test]
fn extension_rules() {
    assert_eq!(extension_of("a.TXT"), "txt");
    assert_eq!(extension_of(".bashrc"), "");
    assert_eq!(extension_of("file.exe:Zone.Identifier"), "");
    assert_eq!(extension_of("noext"), "");
}

#[test]
fn deferred_indexes_rebuild() {
    let (_d, db) = new_db();
    db.defer_file_indexes().unwrap();
    populate(&db);
    db.rebuild_file_indexes().unwrap();
    assert_eq!(
        db.count_files(&FileQuery {
            extensions: vec!["jpg".into()],
            ..Default::default()
        })
        .unwrap(),
        1
    );
}

/// Load throughput for ADR-014 budgets: `cargo test -p pm-casedb --release -- --ignored bulk_load_rate --nocapture`
#[test]
#[ignore = "benchmark"]
fn bulk_load_rate() {
    let (_d, db) = new_db();
    let n = 1_000_000u64;
    db.defer_file_indexes().unwrap();
    let t = std::time::Instant::now();
    db.bulk(|b| {
        let ds_id = b.add_data_source(&ds())?;
        let fs_id = b.add_file_system(&NewFileSystem {
            ds_id,
            volume_id: None,
            kind: "NTFS".into(),
            byte_offset: 0,
            block_size: 4096,
            block_count: 1,
            root_inode: 5,
            label: None,
            serial: None,
        })?;
        let root = b.add_file(&file(
            ds_id,
            fs_id,
            None,
            "/",
            "",
            FileKindCode::Directory,
            0,
            true,
        ))?;
        let mut dir = root;
        for i in 0..n {
            if i % 1000 == 0 {
                dir = b.add_file(&file(
                    ds_id,
                    fs_id,
                    Some(root),
                    "/",
                    &format!("dir{i}"),
                    FileKindCode::Directory,
                    0,
                    true,
                ))?;
            }
            b.add_file(&file(
                ds_id,
                fs_id,
                Some(dir),
                "/dir/",
                &format!("file_{i}.dat"),
                FileKindCode::Regular,
                i,
                i % 10 != 0,
            ))?;
        }
        Ok(())
    })
    .unwrap();
    let load = t.elapsed();
    let t2 = std::time::Instant::now();
    db.rebuild_file_indexes().unwrap();
    let idx = t2.elapsed();
    eprintln!(
        "inserted {n} files in {load:?} ({:.0}/s); indexes rebuilt in {idx:?}",
        n as f64 / load.as_secs_f64()
    );
    let t3 = std::time::Instant::now();
    let c = db
        .count_files(&FileQuery {
            deleted_only: true,
            ..Default::default()
        })
        .unwrap();
    eprintln!("deleted count {c} in {:?}", t3.elapsed());
}

#[test]
fn file_by_inode_finds_default_and_stream_rows() {
    let (_d, db) = new_db();
    let (ds_id, fs_id, root, _) = populate(&db);
    let mut ads = file(
        ds_id,
        fs_id,
        Some(root),
        "/",
        "z.exe:Zone.Identifier",
        FileKindCode::Regular,
        26,
        true,
    );
    ads.inode = Some(7); // same inode as z.exe (size 7 is used as inode by `file`)
    ads.stream = Some(3);
    db.bulk(|b| b.add_file(&ads)).unwrap();
    let plain = db.file_by_inode(fs_id, 7, None).unwrap().unwrap();
    assert_eq!(plain.name, "z.exe");
    let stream = db.file_by_inode(fs_id, 7, Some(3)).unwrap().unwrap();
    assert_eq!(stream.name, "z.exe:Zone.Identifier");
    assert!(db.file_by_inode(fs_id, 7, Some(9)).unwrap().is_none());
    assert!(db.file_by_inode(fs_id + 1, 7, None).unwrap().is_none());
    assert!(db.file_by_inode(fs_id, u64::MAX, None).unwrap().is_none());
}
