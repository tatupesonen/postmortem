//! Schema v2: aggregate scores (TSK `ScoringManager`), tags with tag sets,
//! timeline events, communication accounts and the v1 → v2 migration.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use pm_casedb::communications::{AccountInstanceSpec, CommsFilter, RelationshipKind};
use pm_casedb::events::{NewEvent, types};
use pm_casedb::*;

fn new_db() -> (tempfile::TempDir, CaseDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = CaseDb::create(dir.path().join("case.db")).unwrap();
    (dir, db)
}

/// A data source with two files; returns `(ds, file a, file b)`.
fn populate(db: &CaseDb) -> (i64, i64, i64) {
    db.bulk(|b| {
        let ds = b.add_data_source(&NewDataSource {
            name: "img".into(),
            kind: DataSourceKind::DiskImage,
            paths: vec!["/img.dd".into()],
            size: 0,
            sector_size: 512,
            timezone: "UTC".into(),
            image_format: None,
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: Vec::new(),
            added_at: 0,
        })?;
        let mut f = NewFile::new(ds, None, "/", "a.txt", Origin::Local, FileKindCode::Regular);
        let a = b.add_file(&f)?;
        f.name = "b.txt".into();
        let bb = b.add_file(&f)?;
        Ok((ds, a, bb))
    })
    .unwrap()
}

fn result(
    ds: i64,
    file: Option<i64>,
    source: Option<i64>,
    sig: Significance,
    prio: Priority,
) -> NewArtifact {
    NewArtifact {
        type_id: catalog::artifact_type_id("TSK_INTERESTING_FILE_HIT").unwrap(),
        ds_id: ds,
        file_id: file,
        source_artifact_id: source,
        score: Some((sig, prio)),
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 0,
        attributes: Vec::new(),
    }
}

fn data_artifact(ds: i64, file: i64, ty: &str) -> NewArtifact {
    NewArtifact {
        type_id: catalog::artifact_type_id(ty).unwrap(),
        ds_id: ds,
        file_id: Some(file),
        source_artifact_id: None,
        score: None,
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 0,
        attributes: Vec::new(),
    }
}

#[test]
fn aggregate_score_follows_tsk_ordering() {
    let (_d, db) = new_db();
    let (ds, a, b) = populate(&db);
    assert_eq!(db.score(ObjKind::File, a).unwrap(), Score::UNKNOWN);

    db.add_artifact(&result(
        ds,
        Some(a),
        None,
        Significance::LikelyNotable,
        Priority::Normal,
    ))
    .unwrap();
    assert_eq!(db.file_score(a).unwrap(), Significance::LikelyNotable);
    // NONE (a definitive "not notable") outranks LIKELY_NOTABLE.
    db.add_artifact(&result(
        ds,
        Some(a),
        None,
        Significance::None,
        Priority::Normal,
    ))
    .unwrap();
    assert_eq!(db.file_score(a).unwrap(), Significance::None);
    db.add_artifact(&result(
        ds,
        Some(a),
        None,
        Significance::Notable,
        Priority::Normal,
    ))
    .unwrap();
    assert_eq!(db.file_score(a).unwrap(), Significance::Notable);
    // An override result wins whatever its significance.
    db.add_artifact(&result(
        ds,
        Some(a),
        None,
        Significance::LikelyNone,
        Priority::Override,
    ))
    .unwrap();
    assert_eq!(
        db.score(ObjKind::File, a).unwrap(),
        Score {
            significance: Significance::LikelyNone,
            priority: Priority::Override
        }
    );

    // A result about an artifact scores the artifact, not its file.
    let art = db
        .add_artifact(&data_artifact(ds, b, "TSK_WEB_HISTORY"))
        .unwrap();
    db.add_artifact(&result(
        ds,
        Some(b),
        Some(art),
        Significance::Notable,
        Priority::Normal,
    ))
    .unwrap();
    assert_eq!(db.score(ObjKind::Artifact, art).unwrap(), Score::NOTABLE);
    assert_eq!(db.file_score(b).unwrap(), Significance::Unknown);

    // Score tree: Bad items are files and *data* artifacts.
    assert_eq!(
        db.score_count(true, None).unwrap(),
        1,
        "the artifact (file a is override likely-none)"
    );
    assert_eq!(
        db.scored_objects(true, Some(ds), (ObjKind::File, 0), 10)
            .unwrap(),
        [(ObjKind::Artifact, art)]
    );
    assert_eq!(
        db.scores(ObjKind::File, &[a, b]).unwrap().len(),
        1,
        "unscored ids are omitted"
    );
}

#[test]
fn notable_tags_score_and_tag_sets_are_exclusive() {
    let (_d, db) = new_db();
    let (ds, a, b) = populate(&db);
    let set = db
        .add_tag_set(
            "Project VIC",
            &[
                NewTagName {
                    name: "CAT-1: Child Exploitation (Illegal)".into(),
                    description: String::new(),
                    color: "RED".into(),
                    notable: true,
                },
                NewTagName {
                    name: "CAT-4: Other".into(),
                    description: String::new(),
                    color: "NONE".into(),
                    notable: false,
                },
            ],
        )
        .unwrap();
    let (cat1, cat4) = (set.names[0].id, set.names[1].id);
    assert_eq!(set.names[1].rank, Some(1));

    db.tag_file(a, cat1, "", "alice", 1).unwrap();
    assert_eq!(db.file_score(a).unwrap(), Significance::Notable);
    // Swapping to the other category of the same set removes CAT-1 and
    // with it the notable score.
    db.tag_file(a, cat4, "", "alice", 2).unwrap();
    let tags = db.file_tags(a).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].tag_name_id, cat4);
    assert_eq!(db.file_score(a).unwrap(), Significance::Unknown);

    // Result tags.
    let art = db
        .add_artifact(&data_artifact(ds, b, "TSK_WEB_HISTORY"))
        .unwrap();
    let notable = db
        .tag_names()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "Notable Item")
        .unwrap()
        .id;
    let t = db.tag_artifact(art, notable, "why", "bob", 3).unwrap();
    assert_eq!(db.score(ObjKind::Artifact, art).unwrap(), Score::NOTABLE);
    assert_eq!(db.artifact_tags(art).unwrap()[0].comment, "why");
    let in_use: Vec<String> = db
        .tag_names_in_use(None, None)
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(in_use, ["CAT-4: Other", "Notable Item"]);
    let bobs: Vec<String> = db
        .tag_names_in_use(Some("bob"), Some(ds))
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(bobs, ["Notable Item"]);
    assert_eq!(db.tag_usage(notable, None, None).unwrap(), (0, 1));
    assert_eq!(
        db.artifact_tags_named(notable, Some("bob"), 0, 10)
            .unwrap()
            .len(),
        1
    );
    db.untag_artifact(t).unwrap();
    assert_eq!(db.score(ObjKind::Artifact, art).unwrap(), Score::UNKNOWN);

    // Making a tag name notable rescans its items.
    db.tag_file(b, cat4, "", "alice", 4).unwrap();
    db.update_tag_name(cat4, "", "NONE", true).unwrap();
    assert_eq!(db.file_score(b).unwrap(), Significance::Notable);
}

#[test]
fn events_are_idempotent_and_flag_hash_hits() {
    let (_d, db) = new_db();
    let (ds, a, _) = populate(&db);
    let hit = data_artifact(ds, a, "TSK_HASHSET_HIT");
    db.add_artifact(&hit).unwrap();
    let art = db
        .add_artifact(&data_artifact(ds, a, "TSK_WEB_HISTORY"))
        .unwrap();
    let ev = |t: i64| NewEvent {
        type_id: types::WEB_HISTORY,
        time: t,
        full_description: "https://example.com/x".into(),
        med_description: Some("example.com".into()),
        short_description: Some("example.com".into()),
        ds_id: ds,
        file_id: Some(a),
        artifact_id: Some(art),
    };
    assert_eq!(db.add_events(&[ev(100), ev(200)]).unwrap(), 2);
    assert_eq!(
        db.add_events(&[ev(100)]).unwrap(),
        0,
        "same type, description and time"
    );
    assert_eq!(db.event_counts(0, 1000).unwrap(), [(types::WEB_HISTORY, 2)]);
    let page = db.events_between(0, 1000, (i64::MIN, 0), 1).unwrap();
    assert_eq!(page.len(), 1);
    assert!(page[0].hash_hit, "source file has a hash set hit");
    let next = db
        .events_between(0, 1000, (page[0].time, page[0].id), 10)
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].time, 200);
    assert_eq!(db.events_of_artifact(art).unwrap().len(), 2);
    let types = db.event_types().unwrap();
    assert_eq!(
        types.iter().find(|t| t.id == 11).unwrap().name,
        "Web History Accessed"
    );
    assert_eq!(
        types.iter().find(|t| t.id == 11).unwrap().super_type_id,
        Some(2)
    );
}

#[test]
fn communications_accounts_and_relationships() {
    let (_d, db) = new_db();
    let (ds, a, b) = populate(&db);
    let spec = |ty: &'static str, id: &'static str, file: i64| AccountInstanceSpec {
        type_name: ty,
        id,
        module: "test",
        file_id: file,
        ds_id: ds,
        extra: Vec::new(),
        now: 0,
    };
    let (alice, inst1) = db
        .account_instance(spec("EMAIL", "Alice@Example.com", a))
        .unwrap();
    let (alice2, inst2) = db
        .account_instance(spec("EMAIL", "alice@example.com;", a))
        .unwrap();
    assert_eq!(alice.id, alice2.id, "normalised to one account");
    assert_eq!(inst1, inst2, "one instance artifact per file");
    let (_, inst3) = db
        .account_instance(spec("EMAIL", "alice@example.com", b))
        .unwrap();
    assert_ne!(inst1, inst3);
    assert_eq!(db.account_instances_of(&alice).unwrap(), [inst1, inst3]);
    let (bob, _) = db
        .account_instance(spec("PHONE", "+1 (555) 000-1111", b))
        .unwrap();
    assert_eq!(bob.identifier, "+15550001111");
    let (carol, _) = db
        .account_instance(spec("EMAIL", "carol@example.org", b))
        .unwrap();
    assert!(
        db.account_instance(spec("PHONE", "not a number", b))
            .is_err()
    );

    let msg1 = db
        .add_artifact(&data_artifact(ds, a, "TSK_EMAIL_MSG"))
        .unwrap();
    let msg2 = db
        .add_artifact(&data_artifact(ds, b, "TSK_EMAIL_MSG"))
        .unwrap();
    let call = db
        .add_artifact(&data_artifact(ds, b, "TSK_CALLLOG"))
        .unwrap();
    db.add_relationships(
        &[alice.id, bob.id, carol.id],
        msg1,
        RelationshipKind::Message,
        Some(1_000),
        ds,
    )
    .unwrap();
    db.add_relationships(
        &[alice.id, bob.id, carol.id],
        msg1,
        RelationshipKind::Message,
        Some(1_000),
        ds,
    )
    .unwrap();
    db.add_relationships(
        &[alice.id, carol.id],
        msg2,
        RelationshipKind::Message,
        Some(2_000),
        ds,
    )
    .unwrap();
    db.add_relationships(
        &[alice.id, bob.id],
        call,
        RelationshipKind::CallLog,
        Some(0),
        ds,
    )
    .unwrap();

    let all = CommsFilter::default();
    let devices = db.account_device_instances(&all).unwrap();
    let count = |id: i64| {
        devices
            .iter()
            .find(|d| d.account.id == id)
            .unwrap()
            .relationships
    };
    assert_eq!(
        count(alice.id),
        4,
        "alice-bob msg1, alice-carol msg1, alice-carol msg2, alice-bob call"
    );
    assert_eq!(count(bob.id), 3);
    assert_eq!(count(carol.id), 3);

    let messages_only = CommsFilter {
        kinds: vec![RelationshipKind::Message],
        ..CommsFilter::default()
    };
    let related = db.related_accounts(alice.id, &messages_only).unwrap();
    assert_eq!(related[0].0.id, carol.id);
    assert_eq!(related[0].1, 2);
    assert_eq!(
        db.relationship_sources(alice.id, Some(carol.id), &all)
            .unwrap(),
        [msg2, msg1]
    );
    assert_eq!(
        db.relationship_sources(alice.id, Some(bob.id), &all)
            .unwrap()
            .len(),
        2
    );
    let after_1500 = CommsFilter {
        start: Some(1_500),
        ..CommsFilter::default()
    };
    assert_eq!(
        db.relationship_sources(alice.id, None, &after_1500)
            .unwrap(),
        [msg2]
    );
    let phones_only = CommsFilter {
        account_type_ids: vec![db.account_type("PHONE").unwrap().unwrap().id],
        ..CommsFilter::default()
    };
    assert_eq!(db.account_device_instances(&phones_only).unwrap().len(), 1);
    assert_eq!(db.accounts_of_artifact(msg1).unwrap().len(), 3);
    let in_use: Vec<String> = db
        .account_types_in_use()
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(in_use, ["EMAIL", "PHONE"]);
    let custom = db.add_account_type("WWF", "Words with Friends").unwrap();
    assert_eq!(db.add_account_type("WWF", "x").unwrap().id, custom.id);
}

/// A case written by schema v1 (before scores and with the old
/// significance codes) migrates in place.
#[test]
fn v1_cases_migrate_to_v2() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("case.db");
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute_batch(include_str!("../migrations/0001_initial.sql"))
            .unwrap();
        c.execute_batch(
            "INSERT INTO meta (key, value) VALUES ('schema_version', '1');
             INSERT INTO artifact_types (id, name, display_name, category) VALUES (12, 'TSK_INTERESTING_FILE_HIT', 'Interesting Files', 1);
             INSERT INTO data_sources (id, name, kind, paths, size, sector_size, timezone, added_at) VALUES (1, 'img', 0, '[]', 0, 512, 'UTC', 0);
             INSERT INTO files (id, ds_id, origin, name, parent_path, kind, name_alloc, meta_alloc, meta_flags, size, extension)
                 VALUES (1, 1, 3, 'a', '/', 1, 1, 1, 0, 0, ''), (2, 1, 3, 'b', '/', 1, 1, 1, 0, 0, ''), (3, 1, 3, 'c', '/', 1, 1, 1, 0, 0, '');
             -- v1 codes: 2 = none, 3 = likely notable
             INSERT INTO artifacts (type_id, ds_id, file_id, significance, priority, created_at) VALUES
                 (12, 1, 1, 3, 0, 0), (12, 1, 1, 2, 0, 0), (12, 1, 2, 3, 0, 0);
             INSERT INTO file_tags (file_id, tag_name_id, examiner, created_at) VALUES (3, 3, 'x', 0);",
        )
        .unwrap();
    }
    let db = CaseDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(
        db.file_score(1).unwrap(),
        Significance::None,
        "NONE outranks LIKELY_NOTABLE"
    );
    assert_eq!(db.file_score(2).unwrap(), Significance::LikelyNotable);
    assert_eq!(
        db.file_score(3).unwrap(),
        Significance::Notable,
        "notable tag"
    );
    let arts = db.artifacts_for_file(2).unwrap();
    assert_eq!(
        arts[0].significance,
        Some(Significance::LikelyNotable),
        "codes remapped"
    );
    assert_eq!(db.event_types().unwrap().len(), 60);
}
