#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::catalog::artifact_type_id;
use pm_casedb::{FileQuery, Value};
use pm_ingest::{Inbox, IngestProgress, JobSettings, ModuleFactory, run_job};
use pm_modules::event_logs::EventLogFactory;
use pm_testkit::ntfs::NtfsBuilder;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/evtx")
            .join(name),
    )
    .unwrap()
}

/// An XP-style SecEvent.Evt: a logon, then the security log being cleared.
fn legacy_security_log() -> Vec<u8> {
    use pm_testkit::evt::{build, log};
    log(&[
        build(
            1,
            1_093_532_400,
            528,
            "Security",
            "N-1A9ODN6ZXK4LQ",
            &[
                "Mr. Evil",
                "N-1A9ODN6ZXK4LQ",
                "(0x0,0x2A8E6)",
                "2",
                "User32  ",
                "Negotiate",
                "N-1A9ODN6ZXK4LQ",
                "-",
                "N-1A9ODN6ZXK4LQ$",
                "WORKGROUP",
                "(0x0,0x3E7)",
                "432",
                "-",
                "127.0.0.1",
                "0",
            ],
            None,
        ),
        build(
            2,
            1_093_536_000,
            517,
            "Security",
            "N-1A9ODN6ZXK4LQ",
            &[
                "SYSTEM",
                "NT AUTHORITY",
                "(0x0,0x3E7)",
                "Mr. Evil",
                "N-1A9ODN6ZXK4LQ",
                "(0x0,0x2A8E6)",
            ],
            None,
        ),
    ])
}

struct Fixture {
    _dir: tempfile::TempDir,
    case: Arc<Case>,
    ds_id: i64,
}

fn case_with_logs() -> Fixture {
    let mut b = NtfsBuilder::new(16);
    for d in [
        "Windows",
        "Windows/System32",
        "Windows/System32/winevt",
        "Windows/System32/winevt/Logs",
        "Windows/System32/config",
    ] {
        b.dir(d);
    }
    b.file(
        "Windows/System32/winevt/Logs/Security.evtx",
        &fixture("new-user-security.evtx"),
    );
    b.file(
        "Windows/System32/winevt/Logs/Archive-Security.evtx",
        &fixture("Security_short_selected.evtx"),
    );
    b.file(
        "Windows/System32/config/SecEvent.Evt",
        &legacy_security_log(),
    );
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.dd");
    std::fs::write(&img, b.build()).unwrap();
    let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_image(
        &case,
        &img,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    Fixture {
        _dir: dir,
        case: Arc::new(case),
        ds_id: s.ds_id,
    }
}

fn run(f: &Fixture) -> pm_ingest::JobSummary {
    let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(EventLogFactory)];
    run_job(
        &f.case,
        f.ds_id,
        &JobSettings::default(),
        &modules,
        &Arc::new(IngestProgress::default()),
        &Arc::new(Inbox::default()),
    )
    .unwrap()
}

fn type_id(f: &Fixture, name: &str) -> i64 {
    f.case
        .db()
        .artifact_type_names()
        .unwrap()
        .into_iter()
        .find(|t| t.1 == name)
        .map_or_else(|| panic!("{name} not registered"), |t| t.0)
}

#[test]
fn evtx_and_legacy_logs_become_event_log_entries() {
    let f = case_with_logs();
    let summary = run(&f);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let db = f.case.db();
    let entries = db
        .artifacts_of_type(type_id(&f, "KAAMOS_EVENT_LOG"), None, 0, 1000)
        .unwrap();
    // 4 + 7 mapped EVTX events, 2 mapped legacy events.
    assert_eq!(entries.len(), 13);

    let attrs_of = |id: i64| db.attributes(id).unwrap();
    let text = |attrs: &[pm_casedb::AttributeRecord], name: &str| {
        attrs
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.display())
    };
    let mut created = None;
    let mut xp_logon = None;
    for e in &entries {
        let a = attrs_of(e.id);
        match text(&a, "KAAMOS_EVENT_ID").as_deref() {
            Some("4720") => created = Some(a),
            Some("528") => xp_logon = Some(a),
            _ => {}
        }
    }
    let created = created.expect("4720 entry");
    assert_eq!(
        text(&created, "KAAMOS_EVENT_DESCRIPTION").as_deref(),
        Some("A new account was created")
    );
    assert_eq!(
        text(&created, "KAAMOS_EVENT_CHANNEL").as_deref(),
        Some("Security")
    );
    assert!(
        created
            .iter()
            .any(|a| a.name == "KAAMOS_DATETIME" && a.value == Value::DateTime(1_382_545_359))
    );

    let xp = xp_logon.expect("legacy 528 entry");
    assert_eq!(
        text(&xp, "KAAMOS_USER_NAME").as_deref(),
        Some("N-1A9ODN6ZXK4LQ\\Mr. Evil")
    );
    assert_eq!(
        text(&xp, "KAAMOS_REMOTE_HOST").as_deref(),
        Some("N-1A9ODN6ZXK4LQ (127.0.0.1)")
    );
    assert_eq!(
        text(&xp, "KAAMOS_PAYLOAD_DATA_1").as_deref(),
        Some("LogonType Interactive")
    );

    // The cleared legacy security log is flagged.
    let flagged = db
        .artifacts_of_type(
            artifact_type_id("KAAMOS_INTERESTING_ITEM").unwrap(),
            None,
            0,
            10,
        )
        .unwrap();
    assert_eq!(flagged.len(), 1);
    let fa = attrs_of(flagged[0].id);
    assert_eq!(
        text(&fa, "KAAMOS_SET_NAME").as_deref(),
        Some("Anti-forensics")
    );
    let evt = db
        .query_files(
            &FileQuery {
                name_contains: Some("SecEvent".into()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
    assert_eq!(flagged[0].file_id, Some(evt[0].id));

    // Re-running is idempotent.
    let again = run(&f);
    assert_eq!(again.artifacts, 0);
    assert_eq!(
        db.artifacts_of_type(type_id(&f, "KAAMOS_EVENT_LOG"), None, 0, 1000)
            .unwrap()
            .len(),
        13
    );
}
