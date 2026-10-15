#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pm_core::SharedSource;
use pm_core::io::FileSource;
use pm_evtlog::{EventRecord, MapSet};

fn load(name: &str) -> Vec<EventRecord> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/evtx")
        .join(name);
    let src: SharedSource = Arc::new(FileSource::open(&p).unwrap());
    let mut out = Vec::new();
    pm_evtlog::parse(&src, &mut |r| {
        out.push(r);
        true
    })
    .unwrap();
    out
}

#[test]
fn account_creation_log_is_parsed_and_mapped() {
    let recs = load("new-user-security.evtx");
    assert_eq!(recs.len(), 4);
    let created = recs.iter().find(|r| r.event_id == 4720).unwrap();
    assert_eq!(created.channel, "Security");
    assert_eq!(created.provider, "Microsoft-Windows-Security-Auditing");
    assert_eq!(created.time, 1_382_545_359);
    assert_eq!(created.get("TargetUserName"), Some("IEUser"));
    let m = MapSet::builtin().apply(created).unwrap();
    assert_eq!(m.description, "A new account was created");
    assert_eq!(
        m.payload[0].as_deref(),
        Some("Target: IE8Win7\\IEUser (S-1-5-21-3463664321-2923530833-3546627382-1000)")
    );
    let groups: Vec<String> = recs
        .iter()
        .filter(|r| r.event_id == 4732)
        .filter_map(|r| MapSet::builtin().apply(r)?.payload[0].clone())
        .collect();
    assert_eq!(
        groups,
        [
            "Target: Builtin\\Users (S-1-5-32-545)",
            "Target: Builtin\\Administrators (S-1-5-32-544)"
        ]
    );
}

#[test]
fn failed_logon_carries_the_attacker_address() {
    let recs = load("Security_short_selected.evtx");
    assert_eq!(recs.len(), 7);
    let failed = recs.iter().find(|r| r.event_id == 4625).unwrap();
    let m = MapSet::builtin().apply(failed).unwrap();
    assert_eq!(m.description, "Failed logon");
    assert_eq!(m.remote_host.as_deref(), Some("TEMPORAL (23.94.153.202)"));
    assert_eq!(
        m.payload[0].as_deref(),
        Some("Target: TEMPORAL\\Administrator")
    );
    // Every record of these samples has a map.
    assert!(recs.iter().all(|r| MapSet::builtin().apply(r).is_some()));
}
