//! Schema v2: aggregate scores (Autopsy `ScoringManager`), tags with tag sets,
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
        type_id: catalog::artifact_type_id("KAAMOS_INTERESTING_FILE_HIT").unwrap(),
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
fn aggregate_score_follows_autopsy_ordering() {
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
        .add_artifact(&data_artifact(ds, b, "KAAMOS_WEB_HISTORY"))
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
        .add_artifact(&data_artifact(ds, b, "KAAMOS_WEB_HISTORY"))
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
    let hit = data_artifact(ds, a, "KAAMOS_HASHSET_HIT");
    db.add_artifact(&hit).unwrap();
    let art = db
        .add_artifact(&data_artifact(ds, a, "KAAMOS_WEB_HISTORY"))
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
        .add_artifact(&data_artifact(ds, a, "KAAMOS_EMAIL_MSG"))
        .unwrap();
    let msg2 = db
        .add_artifact(&data_artifact(ds, b, "KAAMOS_EMAIL_MSG"))
        .unwrap();
    let call = db
        .add_artifact(&data_artifact(ds, b, "KAAMOS_CALLLOG"))
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
            "INSERT INTO meta (key, value) VALUES ('schema_version', '1'), ('type_namespace', 'kaamos');
             INSERT INTO artifact_types (id, name, display_name, category) VALUES (12, 'KAAMOS_INTERESTING_FILE_HIT', 'Interesting Files', 1);
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
        let mut art = data_artifact(ds, file, "KAAMOS_KEYWORD_HIT");
        let mut attrs = vec![
            (
                attr("KAAMOS_KEYWORD"),
                Value::Text(kw.into()),
                "Keyword Search".into(),
            ),
            (
                attr("KAAMOS_KEYWORD_SEARCH_TYPE"),
                Value::Int(st),
                "Keyword Search".into(),
            ),
        ];
        if !list.is_empty() {
            attrs.push((
                attr("KAAMOS_SET_NAME"),
                Value::Text(list.into()),
                "Keyword Search".into(),
            ));
        }
        if let Some(re) = re {
            attrs.push((
                attr("KAAMOS_KEYWORD_REGEXP"),
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

/// Data artifacts become timeline events as they are posted (Autopsy
/// `TimelineManager`); analysis results do not; a case without them is
/// back-filled once.
#[test]
fn artifacts_post_their_timeline_events() {
    let (dir, db) = new_db();
    let (ds, a, _) = populate(&db);
    let attr = |n: &str| catalog::attribute_type_id(n).unwrap();
    let mut run = data_artifact(ds, a, "KAAMOS_PROG_RUN");
    run.attributes = vec![
        (
            attr("KAAMOS_PROG_NAME"),
            Value::Text("COREUPDATER.EXE".into()),
            "t".into(),
        ),
        (
            attr("KAAMOS_DATETIME"),
            Value::DateTime(1_600_486_859),
            "t".into(),
        ),
    ];
    let run_id = db.add_artifact(&run).unwrap();
    let evs = db.events_of_artifact(run_id).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].type_id, types::PROGRAM_EXECUTION);
    assert_eq!(evs[0].time, 1_600_486_859);
    assert_eq!(evs[0].full_description, "COREUPDATER.EXE");
    assert_eq!(evs[0].file_id, Some(a));

    // Web history: one event per mapped time (accessed and created).
    let mut web = data_artifact(ds, a, "KAAMOS_WEB_HISTORY");
    web.attributes = vec![
        (
            attr("KAAMOS_URL"),
            Value::Text("https://example.com/".into()),
            "t".into(),
        ),
        (
            attr("KAAMOS_DOMAIN"),
            Value::Text("example.com".into()),
            "t".into(),
        ),
        (
            attr("KAAMOS_DATETIME_ACCESSED"),
            Value::DateTime(200),
            "t".into(),
        ),
        (
            attr("KAAMOS_DATETIME_CREATED"),
            Value::DateTime(100),
            "t".into(),
        ),
    ];
    let web_id = db.add_artifact(&web).unwrap();
    let mut kinds: Vec<i64> = db
        .events_of_artifact(web_id)
        .unwrap()
        .iter()
        .map(|e| e.type_id)
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, [types::WEB_HISTORY, types::WEB_HISTORY_CREATED]);

    // Custom types: the catch-all, one event per date attribute.
    let custom = db
        .artifact_type("KAAMOS_THING", "Things", catalog::Category::DataArtifact)
        .unwrap();
    let when = db
        .attribute_type("KAAMOS_WHEN", "When", catalog::ValueType::DateTime)
        .unwrap();
    let mut thing = data_artifact(ds, a, "KAAMOS_PROG_RUN");
    thing.type_id = custom;
    thing.attributes = vec![(when, Value::DateTime(300), "t".into())];
    let thing_id = db.add_artifact(&thing).unwrap();
    let evs = db.events_of_artifact(thing_id).unwrap();
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].type_id, types::CUSTOM_ARTIFACT_CATCH_ALL);
    assert_eq!(evs[0].full_description, "Things: When");

    // Analysis results get none.
    let hit = db
        .add_artifact(&result(
            ds,
            Some(a),
            None,
            Significance::Notable,
            Priority::Normal,
        ))
        .unwrap();
    assert!(db.events_of_artifact(hit).unwrap().is_empty());

    // Back-fill: events removed (as in an old case) come back, once.
    rusqlite::Connection::open(dir.path().join("case.db"))
        .unwrap()
        .execute_batch("DELETE FROM events; DELETE FROM event_descriptions;")
        .unwrap();
    assert_eq!(db.backfill_artifact_events().unwrap(), 4);
    assert_eq!(db.events_of_artifact(run_id).unwrap().len(), 1);
    assert_eq!(db.backfill_artifact_events().unwrap(), 0, "runs once");
}

/// The change bus moves only the generations of the domains a committed
/// write touched.
#[test]
fn committed_writes_bump_their_domains() {
    use pm_casedb::changes::Domain;
    let (_dir, db) = new_db();
    let ch = std::sync::Arc::clone(db.changes());
    let before = |d| ch.generation(d);
    let (files0, arts0, tags0) = (
        before(Domain::Files),
        before(Domain::Artifacts),
        before(Domain::Tags),
    );
    let (ds, a, _) = populate(&db);
    assert!(ch.generation(Domain::Files) > files0);
    assert!(ch.generation(Domain::DataSources) > 0);
    assert_eq!(ch.generation(Domain::Artifacts), arts0, "no artifact yet");
    db.add_artifact(&data_artifact(ds, a, "KAAMOS_WEB_HISTORY"))
        .unwrap();
    assert!(ch.generation(Domain::Artifacts) > arts0);
    assert_eq!(ch.generation(Domain::Tags), tags0);
    // A failed transaction publishes nothing.
    let files = ch.generation(Domain::Files);
    let r: pm_casedb::Result<()> = db.write(|tx| {
        tx.execute("UPDATE files SET size = 1 WHERE id = ?1", [a])?;
        Err(pm_casedb::DbError::Invalid("abort".into()))
    });
    assert!(r.is_err());
    assert_eq!(ch.generation(Domain::Files), files, "rolled back");
}

/// A generation moves only once its commit is visible to readers: a view
/// that reloads as soon as it sees the change must read the new state.
#[test]
fn published_changes_are_visible_to_readers() {
    use pm_casedb::changes::Domain;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_dir, db) = new_db();
    let (ds, _, _) = populate(&db);
    let db = std::sync::Arc::new(db);
    let size_of = move |db: &CaseDb| -> i64 {
        db.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(size), 0) FROM files WHERE ds_id = ?1",
                [ds],
                |r| r.get(0),
            )?)
        })
        .unwrap()
    };
    let done = std::sync::Arc::new(AtomicBool::new(false));
    let watcher = {
        let (db, done) = (std::sync::Arc::clone(&db), std::sync::Arc::clone(&done));
        std::thread::spawn(move || {
            let ch = std::sync::Arc::clone(db.changes());
            let mut seen = ch.generation(Domain::Files);
            let mut stale = 0;
            while !done.load(Ordering::Acquire) {
                let g = ch.generation(Domain::Files);
                if g != seen {
                    seen = g;
                    // The writer stores the generation's number as size.
                    if size_of(&db) < i64::try_from(g).unwrap() {
                        stale += 1;
                    }
                }
            }
            stale
        })
    };
    for _ in 0..300 {
        let g = db.changes().generation(Domain::Files);
        db.write(|tx| {
            tx.execute(
                "UPDATE files SET size = ?2 WHERE id = (SELECT MIN(id) FROM files WHERE ds_id = ?1)",
                rusqlite::params![ds, i64::try_from(g).unwrap() + 1],
            )?;
            Ok(())
        })
        .unwrap();
    }
    done.store(true, Ordering::Release);
    assert_eq!(
        watcher.join().unwrap(),
        0,
        "a reader saw a generation before its data"
    );
}

/// Cases made before the KAAMOS_ type namespace are refused.
#[test]
fn cases_without_the_kaamos_namespace_are_refused() {
    let (dir, db) = new_db();
    drop(db);
    let path = dir.path().join("case.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM meta WHERE key = 'type_namespace'", [])
        .unwrap();
    let e = CaseDb::open(&path).expect_err("refused");
    assert!(e.to_string().contains("create the case again"), "{e}");
    assert!(CaseDb::open_read_only(&path).is_err());
}

/// Pivot: results whose text attribute equals a value, through the
/// `(type_id, value_text)` index; case variants as typed and lower case.
#[test]
fn artifacts_with_a_value_are_found() {
    let (_dir, db) = new_db();
    let (ds, a, _) = populate(&db);
    let text = |n: &str, v: &str| {
        (
            catalog::attribute_type_id(n).unwrap(),
            Value::Text(v.into()),
            "test".to_owned(),
        )
    };
    let mut web = data_artifact(ds, a, "KAAMOS_WEB_HISTORY");
    web.attributes = vec![
        text("KAAMOS_URL", "https://evil.example/"),
        text("KAAMOS_DOMAIN", "evil.example"),
    ];
    let w = db.add_artifact(&web).unwrap();
    let mut mail = data_artifact(ds, a, "KAAMOS_EMAIL_MSG");
    mail.attributes = vec![text("KAAMOS_EMAIL_FROM", "boss@evil.example")];
    db.add_artifact(&mail).unwrap();
    let hits = db.artifacts_with_value("evil.example", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].artifact.id, w);
    assert_eq!(hits[0].attribute, "Domain");
    assert_eq!(hits[0].type_display, "Web History");
    // As typed in upper case, the lower-case variant matches.
    assert_eq!(
        db.artifacts_with_value("Boss@Evil.Example", 10)
            .unwrap()
            .len(),
        1
    );
    // The plan seeks the index.
    let plan: Vec<String> = db
        .read(|c| {
            let mut st = c.prepare(
                "EXPLAIN QUERY PLAN SELECT artifact_id FROM attributes WHERE type_id IN (SELECT id FROM attribute_types WHERE value_type = 0) AND value_text IN ('a', 'b')",
            )?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(3))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    assert!(
        plan.iter().any(|p| p.contains("attributes_type_text")),
        "{plan:?}"
    );
}

/// The notebook: notes about files or results, findings, and the change
/// bus's Notes domain.
#[test]
fn notes_are_kept_and_marked_as_findings() {
    use pm_casedb::changes::Domain;
    let (_dir, db) = new_db();
    let (_, a, _) = populate(&db);
    let before = db.changes().generation(Domain::Notes);
    let n1 = db
        .add_note("Look at the VPN logs", None, "jdoe", 100)
        .unwrap();
    let n2 = db
        .add_note("Exfiltration tool", Some((ObjKind::File, a)), "jdoe", 200)
        .unwrap();
    assert!(db.changes().generation(Domain::Notes) > before);
    let all = db.notes(false).unwrap();
    assert_eq!(
        all.iter().map(|n| n.id).collect::<Vec<_>>(),
        [n2, n1],
        "newest first"
    );
    assert_eq!(all[0].about, Some((ObjKind::File, a)));
    assert!(db.notes(true).unwrap().is_empty());
    db.update_note(n2, "Exfiltration tool (rclone)", true, 300)
        .unwrap();
    let findings = db.notes(true).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].text, "Exfiltration tool (rclone)");
    assert_eq!(findings[0].updated_at, 300);
    db.delete_note(n1).unwrap();
    assert_eq!(db.notes(false).unwrap().len(), 1);
    assert!(db.note(n1).unwrap().is_none());
}
