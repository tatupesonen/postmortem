#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! End-to-end: Data Source Integrity on the E01 fixture (stored MD5 and
//! SHA-1 from `ewfacquire`), on a tampered stored hash, and on a raw
//! image without hashes.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::Significance;
use pm_casedb::integrity::ImageHash;
use pm_ingest::{
    Inbox, IngestMessage, IngestProgress, JobSettings, MessageKind, ModuleFactory, ModuleSetting,
    run_job,
};
use pm_modules::data_source_integrity::{IntegrityFactory, IntegritySettings};

use common::DemoCase;

const E01_MD5: &str = "b7d9562dc88251034f015fcb757c7ec7";
const E01_SHA1: &str = "9c6a5079404c0af11ca2a0cc382541de694bd1dd";

fn e01_case() -> (tempfile::TempDir, Arc<Case>, i64) {
    let dir = tempfile::tempdir().unwrap();
    let img: PathBuf = [env!("CARGO_MANIFEST_DIR"), "../../fixtures/ewf/small.E01"]
        .iter()
        .collect();
    let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_image(
        &case,
        &img,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    (dir, Arc::new(case), s.ds_id)
}

fn run(case: &Arc<Case>, ds_id: i64, s: IntegritySettings) -> Result<Vec<IngestMessage>, String> {
    let job = JobSettings {
        modules: vec![ModuleSetting {
            id: "data_source_integrity".into(),
            enabled: true,
            settings: serde_json::to_value(s).unwrap(),
        }],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(IntegrityFactory)];
    let inbox = Arc::new(Inbox::default());
    run_job(
        case,
        ds_id,
        &job,
        &modules,
        &Arc::new(IngestProgress::default()),
        &inbox,
    )
    .map_err(|e| e.to_string())?;
    Ok(inbox
        .since(0)
        .into_iter()
        .filter(|m| m.source.as_deref() == Some("Data Source Integrity"))
        .collect())
}

fn failures(case: &Case, ds_id: i64) -> Vec<(Option<i64>, Option<Significance>, String)> {
    let ty = pm_casedb::catalog::artifact_type_id("KAAMOS_VERIFICATION_FAILED").unwrap();
    let db = case.db();
    db.artifacts_of_type(ty, Some(ds_id), 0, 100)
        .unwrap()
        .into_iter()
        .map(|a| {
            let comment = db
                .attributes(a.id)
                .unwrap()
                .into_iter()
                .find(|x| x.name == "KAAMOS_COMMENT")
                .unwrap()
                .value
                .display();
            assert_eq!(a.justification.as_deref(), Some(comment.as_str()));
            (a.file_id, a.significance, comment)
        })
        .collect()
}

#[test]
fn verifies_e01_hashes() {
    let (_d, case, ds) = e01_case();
    let stored = case.db().data_source_hashes(ds).unwrap();
    assert_eq!(
        stored,
        vec![
            (ImageHash::Md5, E01_MD5.to_owned()),
            (ImageHash::Sha1, E01_SHA1.to_owned())
        ]
    );
    let msgs = run(&case, ds, IntegritySettings::default()).unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last.kind, MessageKind::Info);
    assert_eq!(last.subject, "Integrity of small.E01 verified");
    assert!(
        last.details.contains("Result: verified"),
        "{}",
        last.details
    );
    assert!(last.details.contains("MD5 hash verified"));
    assert!(
        last.details
            .contains(&format!("Calculated hash: {E01_SHA1}"))
    );
    assert!(msgs.iter().any(|m| m.subject == "Starting small.E01"));
    assert!(failures(&case, ds).is_empty());
}

#[test]
fn reports_a_mismatch() {
    let (_d, case, ds) = e01_case();
    let wrong = "00000000000000000000000000000000";
    case.db()
        .write(|tx| {
            tx.execute(
                "UPDATE data_sources SET md5 = ?2 WHERE id = ?1",
                rusqlite::params![ds, wrong],
            )?;
            Ok(())
        })
        .unwrap();
    let msgs = run(&case, ds, IntegritySettings::default()).unwrap();
    let last = msgs.last().unwrap();
    assert_eq!(last.kind, MessageKind::Warning);
    assert_eq!(last.subject, "small.E01 failed integrity verification");
    assert!(last.details.contains("MD5 hash not verified"));
    assert!(last.details.contains("SHA-1 hash verified"));
    let f = failures(&case, ds);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].0, None);
    assert_eq!(f[0].1, Some(Significance::Notable));
    assert_eq!(
        f[0].2,
        format!(
            "MD5 hash verification failed:\n  Calculated hash: {E01_MD5}\n  Stored hash: {wrong}\n "
        )
    );
    // Re-ingest: still one result.
    run(&case, ds, IntegritySettings::default()).unwrap();
    assert_eq!(failures(&case, ds).len(), 1);
    // Verification switched off.
    let msgs = run(
        &case,
        ds,
        IntegritySettings {
            verify_hashes: false,
            compute_hashes: true,
        },
    )
    .unwrap();
    assert_eq!(
        msgs.last().unwrap().subject,
        "Not verifying existing hashes for small.E01 since the option was disabled"
    );
}

#[test]
fn computes_missing_hashes() {
    let c = DemoCase::new();
    let db = c.case.db();
    assert!(db.data_source_hashes(c.ds_id).unwrap().is_empty());
    // Off: nothing computed.
    let msgs = run(
        &c.case,
        c.ds_id,
        IntegritySettings {
            compute_hashes: false,
            verify_hashes: true,
        },
    )
    .unwrap();
    assert!(
        msgs.last()
            .unwrap()
            .subject
            .starts_with("Not computing new hashes for ")
    );
    assert!(db.data_source_hashes(c.ds_id).unwrap().is_empty());

    let msgs = run(&c.case, c.ds_id, IntegritySettings::default()).unwrap();
    let last = msgs.last().unwrap();
    assert!(
        last.subject.ends_with(" hashes calculated"),
        "{}",
        last.subject
    );
    let image = std::fs::read(common::demo_image_path()).unwrap();
    let md5 = pm_core::decode::to_hex(&<md5::Md5 as md5::Digest>::digest(&image));
    let got = db.data_source_hashes(c.ds_id).unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[0], (ImageHash::Md5, md5.clone()));
    assert!(
        last.details
            .contains(&format!("Calculated MD5 hash: {md5}"))
    );
    // The next run verifies them.
    let msgs = run(&c.case, c.ds_id, IntegritySettings::default()).unwrap();
    assert!(msgs.last().unwrap().subject.starts_with("Integrity of "));

    // Both options off: start-up error.
    let err = run(
        &c.case,
        c.ds_id,
        IntegritySettings {
            compute_hashes: false,
            verify_hashes: false,
        },
    )
    .unwrap_err();
    assert!(
        err.contains("At least one of the checkboxes must be selected"),
        "{err}"
    );
}

#[test]
fn user_entered_hashes() {
    let c = DemoCase::new();
    let db = c.case.db();
    assert!(
        db.set_data_source_hash(c.ds_id, ImageHash::Sha256, "nothex")
            .is_err()
    );
    let h = "AB".repeat(32);
    assert!(
        db.set_data_source_hash(c.ds_id, ImageHash::Sha256, &h)
            .unwrap()
    );
    // Never overwritten.
    assert!(
        !db.set_data_source_hash(c.ds_id, ImageHash::Sha256, &"cd".repeat(32))
            .unwrap()
    );
    assert_eq!(
        db.data_source_hashes(c.ds_id).unwrap(),
        vec![(ImageHash::Sha256, "ab".repeat(32))]
    );
    let msgs = run(&c.case, c.ds_id, IntegritySettings::default()).unwrap();
    assert!(
        msgs.last()
            .unwrap()
            .subject
            .ends_with("failed integrity verification")
    );
}
