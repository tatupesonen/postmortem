#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! End-to-end: Extension Mismatch Detector after File Type Identification
//! on a generated NTFS volume.

mod common;

use std::sync::Arc;

use pm_casedb::Significance;
use pm_ingest::{JobSettings, MessageKind, ModuleFactory, ModuleSetting};
use pm_modules::ext_mismatch::{CheckType, ExtMismatchFactory, ExtMismatchSettings, MismatchMap};
use pm_modules::file_type::FileTypeFactory;
use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

use common::DemoCase;

const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0\x01\x01\0\0\x01\0\x01\0\0\xFF\xD9";
const PDF: &[u8] = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n1 0 obj\n";
const TEXT: &[u8] = b"plain words, nothing else\n";

fn image() -> Vec<u8> {
    let mut b = NtfsBuilder::new(16);
    b.dir("/f");
    b.file("/f/photo.jpg", JPEG)
        .file("/f/photo.txt", JPEG)
        .file("/f/report.pdf", PDF)
        .file("/f/report.doc", PDF)
        .file("/f/report", PDF)
        .file("/f/notes.txt", TEXT)
        .file("/f/notes.jpg", TEXT)
        .file("/f/known.txt", PDF)
        .file_with(
            "/f/gone.txt",
            PDF,
            FileOpts {
                deleted: Deleted::InSlack,
                ..FileOpts::default()
            },
        );
    b.build()
}

fn run(c: &DemoCase, settings: &ExtMismatchSettings) -> Arc<pm_ingest::Inbox> {
    let job = JobSettings {
        modules: vec![
            ModuleSetting {
                id: "file_type".into(),
                enabled: true,
                settings: serde_json::Value::Null,
            },
            ModuleSetting {
                id: "ext_mismatch".into(),
                enabled: true,
                settings: serde_json::to_value(settings).unwrap(),
            },
        ],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> =
        vec![Arc::new(FileTypeFactory), Arc::new(ExtMismatchFactory)];
    let (summary, inbox) = c.run(&job, modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    inbox
}

/// Paths with a mismatch result, and the result for one of them.
fn flagged(c: &DemoCase) -> Vec<String> {
    let mut v: Vec<String> = c
        .artifacts("TSK_EXT_MISMATCH_DETECTED")
        .into_iter()
        .map(|(a, _)| {
            c.case
                .db()
                .file(a.file_id.unwrap())
                .unwrap()
                .unwrap()
                .path()
        })
        .collect();
    v.sort();
    v
}

fn mark_known(c: &DemoCase, path: &str) {
    let id = c.file(path).id;
    c.case
        .db()
        .write(|tx| {
            tx.execute("UPDATE files SET known = 1 WHERE id = ?1", [id])?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn default_settings_flag_media_and_documents() {
    let c = DemoCase::with_image(&image());
    mark_known(&c, "/f/known.txt");
    let gone = c.file("/f/gone.txt");
    assert!(gone.deleted() && gone.kind == pm_casedb::FileKindCode::Regular);
    let inbox = run(&c, &ExtMismatchSettings::default());
    // The deleted PDF named ".txt" was typed but not compared.
    assert_eq!(
        c.file("/f/gone.txt").mime.as_deref(),
        Some("application/pdf")
    );
    // Not: matching extensions, no extension, text (not a media type),
    // known files, deleted names.
    assert_eq!(flagged(&c), ["/f/photo.txt", "/f/report.doc"]);

    let hits = c.artifacts("TSK_EXT_MISMATCH_DETECTED");
    let (a, attrs) = hits
        .iter()
        .find(|(a, _)| a.file_id == Some(c.file("/f/report.doc").id))
        .unwrap();
    assert_eq!(a.significance, Some(Significance::LikelyNotable));
    assert_eq!(
        a.justification.as_deref(),
        Some("File has MIME type of application/pdf")
    );
    assert_eq!(a.conclusion, None);
    assert_eq!(a.configuration, None);
    assert!(attrs.is_empty(), "{attrs:?}");

    let msg = inbox
        .since(0)
        .into_iter()
        .find(|m| m.subject == "File Extension Mismatch Results")
        .expect("summary");
    assert_eq!(msg.kind, MessageKind::Info);
    assert_eq!(msg.source.as_deref(), Some("Extension Mismatch Detector"));
    assert!(
        msg.details.contains("Total Files Processed: "),
        "{}",
        msg.details
    );

    // Re-ingest adds nothing.
    run(&c, &ExtMismatchSettings::default());
    assert_eq!(c.artifacts("TSK_EXT_MISMATCH_DETECTED").len(), 2);
}

#[test]
fn check_modes_and_skip_options() {
    let all = ExtMismatchSettings {
        check_type: CheckType::All,
        skip_files_without_extension: false,
        skip_known_files: false,
        mismatch_map: None,
    };
    let c = DemoCase::with_image(&image());
    mark_known(&c, "/f/known.txt");
    run(&c, &all);
    assert_eq!(
        flagged(&c),
        [
            "/f/known.txt",
            "/f/notes.jpg",
            "/f/photo.txt",
            "/f/report",
            "/f/report.doc"
        ]
    );

    let no_text = ExtMismatchSettings {
        check_type: CheckType::NoTextFiles,
        ..all
    };
    let c = DemoCase::with_image(&image());
    run(&c, &no_text);
    assert_eq!(
        flagged(&c),
        ["/f/known.txt", "/f/photo.txt", "/f/report", "/f/report.doc"]
    );
}

#[test]
fn custom_map() {
    let mut map = MismatchMap::new();
    map.insert(
        "application/pdf".into(),
        Some(["pdf", "doc"].iter().map(|s| (*s).to_owned()).collect()),
    );
    let s = ExtMismatchSettings {
        mismatch_map: Some(map),
        ..ExtMismatchSettings::default()
    };
    let c = DemoCase::with_image(&image());
    run(&c, &s);
    // JPEG is no longer listed and ".doc" is allowed for PDFs; only the
    // PDF named ".txt" (not marked known here) remains.
    assert_eq!(flagged(&c), ["/f/known.txt"]);
}
