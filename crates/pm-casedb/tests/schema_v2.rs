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

    // A user tag definition updates the existing name in place.
    let def = |notable| NewTagName {
        name: "CAT-4: Other".into(),
        description: "edited".into(),
        color: "NONE".into(),
        notable,
    };
    assert_eq!(db.add_or_update_tag_name(&def(false)).unwrap(), cat4);
    assert_ne!(db.file_score(b).unwrap(), Significance::Notable);
    let new = db
        .add_or_update_tag_name(&NewTagName {
            name: "Mine".into(),
            ..def(true)
        })
        .unwrap();
    let mine = db.tag_names().unwrap();
    let mine = mine.iter().find(|t| t.id == new).unwrap();
    assert!(mine.notable && mine.description == "edited");
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

/// Keyword Hits tree grouping follows Autopsy's `KeywordHits` rules.
#[test]
fn keyword_hits_group_like_autopsy() {
    use pm_casedb::keyword_hits::{KeywordHitFilter, SINGLE_LITERAL, SINGLE_REGEX};
    let (_d, db) = new_db();
    let (ds, a, b) = populate(&db);
    let attr = |n: &str| catalog::attribute_type_id(n).unwrap();
    let hit = |file: i64, list: &str, st: i64, re: Option<&str>, kw: &str| {
        let mut art = data_artifact(ds, file, "TSK_KEYWORD_HIT");
        let mut attrs = vec![
            (
                attr("TSK_KEYWORD"),
                Value::Text(kw.into()),
                "Keyword Search".into(),
            ),
            (
                attr("TSK_KEYWORD_SEARCH_TYPE"),
                Value::Int(st),
                "Keyword Search".into(),
            ),
        ];
        if !list.is_empty() {
            attrs.push((
                attr("TSK_SET_NAME"),
                Value::Text(list.into()),
                "Keyword Search".into(),
            ));
        }
        if let Some(re) = re {
            attrs.push((
                attr("TSK_KEYWORD_REGEXP"),
                Value::Text(re.into()),
                "Keyword Search".into(),
            ));
        }
        art.attributes = attrs;
        db.add_artifact(&art).unwrap()
    };
    hit(a, "", 0, None, "budget");
    hit(a, "", 1, Some("hunt"), "hunter2");
    hit(b, "", 1, Some("hunt"), "hunting");
    hit(a, "", 2, Some("\\d{4}"), "2021");
    hit(
        b,
        "Email Addresses",
        2,
        Some("[a-z]+@[a-z.]+"),
        "bob@example.com",
    );
    hit(
        a,
        "Email Addresses",
        2,
        Some("[a-z]+@[a-z.]+"),
        "bob@example.com",
    );
    let groups: Vec<(String, String, Option<String>, u64)> = db
        .keyword_hit_groups()
        .unwrap()
        .into_iter()
        .map(|g| (g.list, g.term, g.instance, g.count))
        .collect();
    let s = |x: &str| x.to_owned();
    assert_eq!(
        groups,
        [
            (s(SINGLE_LITERAL), s("budget"), None, 1),
            (s(SINGLE_LITERAL), s("hunt"), None, 2),
            (s(SINGLE_REGEX), s("\\d{4}"), Some(s("2021")), 1),
            (
                s("Email Addresses"),
                s("[a-z]+@[a-z.]+"),
                Some(s("bob@example.com")),
                2
            ),
        ]
    );
    let in_list = |list: &str, term: Option<&str>| {
        db.keyword_hits_in(
            &KeywordHitFilter {
                list: list.into(),
                term: term.map(Into::into),
                instance: None,
            },
            0,
            10,
        )
        .unwrap()
        .len()
    };
    assert_eq!(in_list(SINGLE_LITERAL, None), 3);
    assert_eq!(in_list(SINGLE_LITERAL, Some("hunt")), 2);
    assert_eq!(in_list("Email Addresses", None), 2);
    assert_eq!(in_list("Nope", None), 0);
}

/// Times the Keyword Hits queries on a real case (`PM_CASE_DB=…/case.db`).
#[test]
#[ignore = "needs a real case database"]
fn keyword_hit_groups_on_a_real_case() {
    let Some(path) = std::env::var_os("PM_CASE_DB") else {
        return;
    };
    let db = CaseDb::open(path).unwrap();
    let t = std::time::Instant::now();
    let g = db.keyword_hit_groups().unwrap();
    let lists: std::collections::BTreeSet<&str> = g.iter().map(|g| g.list.as_str()).collect();
    eprintln!(
        "{} groups in {} lists in {:?}",
        g.len(),
        lists.len(),
        t.elapsed()
    );
    for l in &lists {
        let t = std::time::Instant::now();
        let n = db
            .keyword_hits_in(
                &pm_casedb::keyword_hits::KeywordHitFilter {
                    list: (*l).to_owned(),
                    ..Default::default()
                },
                0,
                20_000,
            )
            .unwrap()
            .len();
        let groups = g.iter().filter(|x| x.list == *l).count();
        eprintln!(
            "  {l}: {n} hits, {groups} groups, listed in {:?}",
            t.elapsed()
        );
    }
}
