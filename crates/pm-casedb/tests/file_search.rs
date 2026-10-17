//! File Search by Attributes filters (Autopsy `filesearch`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_casedb::file_search::{DateFilter, DateKinds, FileSearch, SizeOp};
use pm_casedb::*;

fn ds(name: &str) -> NewDataSource {
    NewDataSource {
        name: name.into(),
        kind: DataSourceKind::DiskImage,
        paths: vec![format!("/{name}")],
        size: 0,
        sector_size: 512,
        timezone: "UTC".into(),
        image_format: None,
        md5: None,
        sha1: None,
        sha256: None,
        acquisition: Vec::new(),
        added_at: 0,
    }
}

/// Two data sources; files `(name, size, mtime, deleted)`.
fn setup() -> (tempfile::TempDir, CaseDb, i64, i64) {
    let dir = tempfile::tempdir().unwrap();
    let db = CaseDb::create(dir.path().join("case.db")).unwrap();
    let (d1, d2) = db
        .bulk(|b| {
            let d1 = b.add_data_source(&ds("one"))?;
            let d2 = b.add_data_source(&ds("two"))?;
            for (d, name, size, mtime, deleted) in [
                (d1, "Report.DOCX", 2_000, 1_600_000_000, false),
                (d1, "notes.txt", 10, 1_600_041_599, true),
                (d1, "100%_o'k.bin", 1 << 20, 1_700_000_000, false),
                (d2, "report-final.pdf", 5_000, 1_600_100_000, false),
            ] {
                let mut f = NewFile::new(d, None, "/", name, Origin::Local, FileKindCode::Regular);
                f.size = size;
                f.times[0] = Some((mtime, 0));
                f.name_alloc = !deleted;
                b.add_file(&f)?;
            }
            Ok((d1, d2))
        })
        .unwrap();
    (dir, db, d1, d2)
}

fn names(db: &CaseDb, s: &FileSearch) -> Vec<String> {
    let mut v: Vec<String> = db
        .search_files(s, 0, 100)
        .unwrap()
        .into_iter()
        .map(|f| f.name)
        .collect();
    v.sort();
    v
}

#[test]
fn each_filter_and_their_combination() {
    let (_d, db, d1, d2) = setup();
    let by_name = FileSearch {
        name: Some("REPORT".into()),
        ..FileSearch::default()
    };
    assert_eq!(
        names(&db, &by_name),
        ["Report.DOCX", "report-final.pdf"],
        "case-insensitive substring"
    );

    let literal = FileSearch {
        name: Some("%_o'".into()),
        ..FileSearch::default()
    };
    assert_eq!(
        names(&db, &literal),
        ["100%_o'k.bin"],
        "quotes and wildcards are literal"
    );

    let big = FileSearch {
        size: Some((SizeOp::Greater, 4_000)),
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &big), ["100%_o'k.bin", "report-final.pdf"]);
    let exact = FileSearch {
        size: Some((SizeOp::Equal, 10)),
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &exact), ["notes.txt"]);

    // 2020-09-13 (one whole day, UTC): 23:59:59 is still inside.
    let day = FileSearch {
        dates: Some(DateFilter {
            from: Some(1_599_955_200),
            to: Some(1_599_955_200 + 86_400),
            kinds: DateKinds {
                modified: true,
                ..DateKinds::default()
            },
        }),
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &day), ["Report.DOCX", "notes.txt"]);
    let no_kind = FileSearch {
        dates: Some(DateFilter {
            from: None,
            to: None,
            kinds: DateKinds::default(),
        }),
        ..FileSearch::default()
    };
    assert!(
        names(&db, &no_kind).is_empty(),
        "no date kind selected matches nothing"
    );

    let deleted = FileSearch {
        deleted: true,
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &deleted), ["notes.txt"]);

    let in_two = FileSearch {
        ds_ids: vec![d2],
        name: Some("report".into()),
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &in_two), ["report-final.pdf"]);
    let in_one_unknown = FileSearch {
        ds_ids: vec![d1],
        known: vec![Known::Unknown],
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &in_one_unknown).len(), 3);
    let notable = FileSearch {
        known: vec![Known::Notable],
        ..FileSearch::default()
    };
    assert!(names(&db, &notable).is_empty());

    // Keyset paging returns every match once.
    let all_d1 = FileSearch {
        ds_ids: vec![d1],
        ..FileSearch::default()
    };
    let first = db.search_files(&all_d1, 0, 2).unwrap();
    let rest = db
        .search_files(&all_d1, first.last().unwrap().id, 2)
        .unwrap();
    assert_eq!(first.len() + rest.len(), 3);
}

#[test]
fn hashes_and_mime_types() {
    let (_d, db, ..) = setup();
    let f = db
        .search_files(
            &FileSearch {
                name: Some("notes".into()),
                ..FileSearch::default()
            },
            0,
            1,
        )
        .unwrap()
        .remove(0);
    db.set_file_analysis(
        f.id,
        &FileAnalysis {
            mime: Some("text/plain".into()),
            md5: Some(vec![0xAB; 16]),
            sha256: Some(vec![0xCD; 32]),
            ..FileAnalysis::default()
        },
    )
    .unwrap();
    let md5 = FileSearch {
        md5: Some(vec![0xAB; 16]),
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &md5), ["notes.txt"]);
    let sha = FileSearch {
        sha256: Some(vec![0xCD; 32]),
        md5: Some(vec![0xAB; 16]),
        ..FileSearch::default()
    };
    assert_eq!(
        names(&db, &sha),
        ["notes.txt"],
        "hash filters combine with AND"
    );
    let mime = FileSearch {
        mime: vec!["text/plain".into(), "image/png".into()],
        ..FileSearch::default()
    };
    assert_eq!(names(&db, &mime), ["notes.txt"]);
    assert_eq!(db.mime_types().unwrap(), ["text/plain"]);
}
