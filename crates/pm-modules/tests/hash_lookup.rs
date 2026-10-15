#![allow(
    clippy::unwrap_used,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod common;

use std::sync::Arc;

use md5::{Digest, Md5};
use pm_casedb::catalog::artifact_type_id;
use pm_casedb::{FileKindCode, Known, Origin, Significance, Value};
use pm_hashdb::HashDb;
use pm_ingest::{JobSettings, MessageKind, ModuleSetting};
use pm_modules::hash_lookup::{
    HashLookupFactory, HashLookupSettings, HashSetConfig, SetKind, merge_comments,
};

use common::{DemoCase, read_all};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn settings(cfg: &HashLookupSettings) -> JobSettings {
    JobSettings {
        modules: vec![ModuleSetting {
            id: "hash_lookup".into(),
            enabled: true,
            settings: serde_json::to_value(cfg).unwrap(),
        }],
        ..JobSettings::default()
    }
}

#[test]
fn hashes_and_hash_set_hits() {
    let demo = DemoCase::new();
    let files = demo.files();
    let regular: Vec<_> = files
        .iter()
        .filter(|f| f.kind == FileKindCode::Regular && f.origin == Origin::FileSystem && f.size > 0)
        .collect();
    let by_name = |n: &str| {
        *regular
            .iter()
            .find(|f| f.name == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    let evil = by_name("setup.exe");
    let boring = by_name("readme.txt");
    let noted = by_name("passwords.txt");
    let md5_of = |f: &pm_casedb::FileRecord| -> [u8; 16] {
        Md5::digest(read_all(&*demo.case.content(f).unwrap())).into()
    };

    let sets = tempfile::tempdir().unwrap();
    let bad = sets.path().join("malware.kdb");
    let db = HashDb::create(&bad).unwrap();
    db.add(
        &md5_of(evil),
        None,
        None,
        Some("setup.exe"),
        Some("dropper"),
    )
    .unwrap();
    db.add(&md5_of(evil), None, None, None, Some("seen in case 7"))
        .unwrap();
    drop(db);
    let known = sets.path().join("nsrl.md5");
    std::fs::write(&known, format!("{}  readme.txt\n", hex(&md5_of(boring)))).unwrap();
    let watch = sets.path().join("watch.md5");
    std::fs::write(&watch, format!("{}  passwords.txt\n", hex(&md5_of(noted)))).unwrap();

    let cfg = HashLookupSettings {
        calculate_hashes: true,
        sets: vec![
            HashSetConfig {
                path: bad,
                name: Some("Malware".into()),
                kind: SetKind::Notable,
                search_during_ingest: true,
                send_ingest_messages: None,
            },
            HashSetConfig {
                path: known,
                name: None,
                kind: SetKind::Known,
                search_during_ingest: true,
                send_ingest_messages: None,
            },
            HashSetConfig {
                path: watch,
                name: Some("Watch list".into()),
                kind: SetKind::NoChange,
                search_during_ingest: true,
                send_ingest_messages: Some(false),
            },
        ],
    };
    let (summary, inbox) = demo.run(&settings(&cfg), vec![Arc::new(HashLookupFactory)]);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);

    // Every file except directories and unallocated space has correct
    // MD5, SHA-1 and SHA-256 — virtual files ($MBR, $FAT1…), ADS rows and
    // deleted names included, as in Autopsy (+ SHA-1).
    // (System streams such as `$BadClus:$Bad` are filtered out of ingest.)
    let after = demo.files();
    let mut virtual_hashed = 0;
    for f in after.iter().filter(|f| {
        f.kind != FileKindCode::Directory
            && !matches!(f.origin, Origin::UnallocBlocks | Origin::Slack)
            && pm_ingest::FileFilter::default().accepts(f)
    }) {
        let data = read_all(&*demo.case.content(f).unwrap());
        assert_eq!(
            f.md5.as_deref(),
            Some(Md5::digest(&data).as_slice()),
            "{}",
            f.path()
        );
        assert_eq!(
            f.sha1.as_deref(),
            Some(sha1::Sha1::digest(&data).as_slice()),
            "{}",
            f.path()
        );
        assert_eq!(
            f.sha256.as_deref(),
            Some(sha2::Sha256::digest(&data).as_slice()),
            "{}",
            f.path()
        );
        if f.kind == FileKindCode::Virtual {
            virtual_hashed += 1;
        }
    }
    assert!(
        virtual_hashed > 0,
        "FAT virtual files ($MBR, $FAT1…) must be hashed"
    );
    // Directories are not hashed.
    assert!(
        after
            .iter()
            .filter(|f| f.kind == FileKindCode::Directory)
            .all(|f| f.md5.is_none() && f.sha1.is_none() && f.sha256.is_none())
    );
    // Unallocated space is not hashed.
    assert!(
        after
            .iter()
            .filter(|f| f.origin == Origin::UnallocBlocks)
            .all(|f| f.md5.is_none())
    );

    let get = |id: i64| after.iter().find(|f| f.id == id).unwrap();
    assert_eq!(get(evil.id).known, Known::Notable);
    assert_eq!(get(boring.id).known, Known::Known);
    assert_eq!(get(noted.id).known, Known::Unknown);

    let hits = |id: i64| {
        demo.case
            .db()
            .artifacts_for_file(id)
            .unwrap()
            .into_iter()
            .filter(|a| Some(a.type_id) == artifact_type_id("TSK_HASHSET_HIT"))
            .collect::<Vec<_>>()
    };
    let evil_hits = hits(evil.id);
    assert_eq!(evil_hits.len(), 1);
    assert_eq!(evil_hits[0].significance, Some(Significance::Notable));
    assert_eq!(evil_hits[0].configuration.as_deref(), Some("Malware"));
    let attrs = demo.case.db().attributes(evil_hits[0].id).unwrap();
    let attr = |n: &str| attrs.iter().find(|a| a.name == n).map(|a| a.value.clone());
    assert_eq!(attr("TSK_SET_NAME"), Some(Value::Text("Malware".into())));
    assert_eq!(attr("TSK_HASH_MD5"), Some(Value::Text(hex(&md5_of(evil)))));
    assert_eq!(
        attr("TSK_COMMENT"),
        Some(Value::Text("dropper seen in case 7".into()))
    );
    assert!(hits(boring.id).is_empty(), "known sets create no artifact");
    let watch_hits = hits(noted.id);
    assert_eq!(watch_hits.len(), 1);
    assert_eq!(watch_hits[0].significance, Some(Significance::Unknown));
    assert_eq!(
        demo.case.db().file_score(evil.id).unwrap(),
        Significance::Notable
    );

    let msgs = inbox.since(0);
    assert!(
        msgs.iter()
            .any(|m| m.kind == MessageKind::Data && m.subject == "Notable: setup.exe")
    );
    assert!(
        !msgs.iter().any(|m| m.subject.contains("passwords.txt")),
        "messages off for that set"
    );
    assert!(!msgs.iter().any(|m| m.subject == "No known hash set."));
    let summary_msg = msgs
        .iter()
        .find(|m| m.subject == "Hash Lookup Results")
        .unwrap();
    assert!(
        summary_msg.details.contains("Notables found: 1"),
        "{}",
        summary_msg.details
    );

    // Re-running reuses hashes and adds no duplicate results.
    let (again, _) = demo.run(&settings(&cfg), vec![Arc::new(HashLookupFactory)]);
    assert_eq!(again.artifacts, 0);
    assert_eq!(hits(evil.id).len(), 1);
}

#[test]
fn missing_sets_warn_and_hashing_still_runs() {
    let demo = DemoCase::new();
    let cfg = HashLookupSettings {
        calculate_hashes: true,
        sets: vec![HashSetConfig {
            path: "/nonexistent/set.kdb".into(),
            name: None,
            kind: SetKind::Notable,
            search_during_ingest: true,
            send_ingest_messages: None,
        }],
    };
    let (_, inbox) = demo.run(&settings(&cfg), vec![Arc::new(HashLookupFactory)]);
    let msgs = inbox.since(0);
    assert!(
        msgs.iter()
            .any(|m| m.kind == MessageKind::Error
                && m.subject.starts_with("Could not open hash set"))
    );
    assert!(msgs.iter().any(|m| m.subject == "No notable hash set."));
    assert!(demo.files().iter().any(|f| f.md5.is_some()));
}

#[test]
fn nothing_to_do_without_sets_or_hashing() {
    let demo = DemoCase::new();
    let cfg = HashLookupSettings {
        calculate_hashes: false,
        sets: Vec::new(),
    };
    demo.run(&settings(&cfg), vec![Arc::new(HashLookupFactory)]);
    assert!(demo.files().iter().all(|f| f.md5.is_none()));
}

#[test]
fn comment_merging() {
    assert_eq!(merge_comments(&["a".into(), "b".into()]), "a b");
    let long = merge_comments(&["x".repeat(600)]);
    assert_eq!(long.chars().count(), 503);
    assert!(long.ends_with("..."));
    assert_eq!(merge_comments(&[]), "");
}
