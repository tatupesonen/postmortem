#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! End-to-end: Interesting Files Identifier with user sets (every
//! condition kind, exclusive rules, disabled sets) and the standard sets.

mod common;

use std::sync::Arc;

use pm_casedb::Significance;
use pm_ingest::{JobSettings, MessageKind, ModuleFactory, ModuleSetting};
use pm_modules::file_type::FileTypeFactory;
use pm_modules::interesting_files::{InterestingFilesFactory, InterestingFilesSettings};
use pm_rules::{FilesSet, MetaType, NameCondition, Rule, TextMatch, xml};
use pm_testkit::ntfs::NtfsBuilder;

use common::DemoCase;

fn image() -> Vec<u8> {
    let mut b = NtfsBuilder::new(16);
    for d in [
        "/Users",
        "/Users/bob",
        "/Users/bob/Documents",
        "/Program Files",
        "/Program Files/KeePass Password Safe 2",
        "/tmp",
    ] {
        b.dir(d);
    }
    b.file("/Users/bob/Documents/plan.docx", b"PK\x03\x04 not really")
        .file("/Users/bob/Documents/notes.txt", b"hello")
        .file("/Users/bob/photo.jpg", b"\xFF\xD8\xFF\xE0\0\x10JFIF\0")
        .file("/tmp/scratch.txt", b"tmp")
        .file(
            "/Program Files/KeePass Password Safe 2/KeePass.exe",
            b"MZ\x90\0",
        );
    b.build()
}

fn run(c: &DemoCase, s: &InterestingFilesSettings) -> Arc<pm_ingest::Inbox> {
    let job = JobSettings {
        modules: vec![
            ModuleSetting {
                id: "file_type".into(),
                enabled: true,
                settings: serde_json::Value::Null,
            },
            ModuleSetting {
                id: "interesting_files".into(),
                enabled: true,
                settings: serde_json::to_value(s).unwrap(),
            },
        ],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> =
        vec![Arc::new(FileTypeFactory), Arc::new(InterestingFilesFactory)];
    let (summary, inbox) = c.run(&job, modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    inbox
}

/// `(path, set, rule)` of every hit, sorted.
fn hits(c: &DemoCase) -> Vec<(String, String, String)> {
    let mut v: Vec<_> = c
        .artifacts("TSK_INTERESTING_ITEM")
        .into_iter()
        .map(|(a, attrs)| {
            assert_eq!(a.significance, Some(Significance::LikelyNotable));
            assert_eq!(
                a.configuration.as_deref(),
                Some(attrs["TSK_SET_NAME"].as_str())
            );
            let path = c
                .case
                .db()
                .file(a.file_id.unwrap())
                .unwrap()
                .unwrap()
                .path();
            (
                path,
                attrs["TSK_SET_NAME"].clone(),
                attrs["TSK_CATEGORY"].clone(),
            )
        })
        .collect();
    v.sort();
    v
}

fn user_sets() -> Vec<FilesSet> {
    let docs = FilesSet {
        name: "Bob's documents".into(),
        description: "Office files and text outside /tmp".into(),
        ignore_known_files: false,
        ignore_unallocated_space: true,
        standard_set: false,
        version: 0,
        rules: vec![
            Rule {
                name_condition: Some(NameCondition::Extension(TextMatch::AnyOf(vec![
                    "docx".into(),
                    "txt".into(),
                ]))),
                ..Rule::all("documents", MetaType::Files)
            },
            Rule {
                path: Some(TextMatch::Partial("/tmp/".into())),
                exclusive: true,
                ..Rule::all("not tmp", MetaType::Files)
            },
        ],
    };
    let pictures = FilesSet {
        name: "Pictures".into(),
        rules: vec![Rule {
            mime_type: Some("image/jpeg".into()),
            ..Rule::all("jpeg", MetaType::Files)
        }],
        ..docs.clone()
    };
    let dirs = FilesSet {
        name: "Profiles".into(),
        rules: vec![Rule {
            name_condition: Some(NameCondition::FullName(TextMatch::Exact("BOB".into()))),
            ..Rule::all("bob's folder", MetaType::Directories)
        }],
        ..docs.clone()
    };
    vec![docs, pictures, dirs]
}

#[test]
fn user_sets_and_messages() {
    let c = DemoCase::with_image(&image());
    let inbox = run(
        &c,
        &InterestingFilesSettings {
            sets: Some(user_sets()),
            disabled_sets: Vec::new(),
        },
    );
    let s = |p: &str, set: &str, r: &str| (p.to_owned(), set.to_owned(), r.to_owned());
    assert_eq!(
        hits(&c),
        vec![
            s("/Users/bob", "Profiles", "bob's folder"),
            s(
                "/Users/bob/Documents/notes.txt",
                "Bob's documents",
                "documents"
            ),
            s(
                "/Users/bob/Documents/plan.docx",
                "Bob's documents",
                "documents"
            ),
            s("/Users/bob/photo.jpg", "Pictures", "jpeg"),
        ]
    );
    let msg = inbox
        .since(0)
        .into_iter()
        .find(|m| m.subject == "Interesting File Match: Pictures(photo.jpg)")
        .unwrap();
    assert_eq!(msg.kind, MessageKind::Data);
    assert_eq!(
        msg.details,
        "File: /Users/bob/photo.jpg\nRule Set: Pictures"
    );

    // Re-ingest: no duplicates and no new messages.
    let inbox = run(
        &c,
        &InterestingFilesSettings {
            sets: Some(user_sets()),
            disabled_sets: Vec::new(),
        },
    );
    assert_eq!(hits(&c).len(), 4);
    assert!(inbox.since(0).iter().all(|m| m.kind != MessageKind::Data));
}

#[test]
fn disabled_sets_and_xml_round_trip() {
    let c = DemoCase::with_image(&image());
    // Sets imported from Autopsy's XML behave the same.
    let sets = xml::read(&xml::write(&user_sets())).unwrap();
    run(
        &c,
        &InterestingFilesSettings {
            sets: Some(sets),
            disabled_sets: vec!["Pictures".into(), "Profiles".into()],
        },
    );
    let h = hits(&c);
    assert_eq!(h.len(), 2, "{h:?}");
    assert!(h.iter().all(|(_, set, _)| set == "Bob's documents"));
}

#[test]
fn standard_sets_by_default() {
    let c = DemoCase::with_image(&image());
    run(&c, &InterestingFilesSettings::default());
    let h = hits(&c);
    assert!(
        h.contains(&(
            "/Program Files/KeePass Password Safe 2/KeePass.exe".into(),
            "Encryption Programs".into(),
            "KeePass".into()
        )),
        "{h:?}"
    );
}

#[test]
fn invalid_sets_fail_start_up() {
    let c = DemoCase::with_image(&image());
    let bad = FilesSet {
        rules: vec![Rule {
            path: Some(TextMatch::Regex("(".into())),
            ..Rule::all("bad", MetaType::Files)
        }],
        ..user_sets().remove(0)
    };
    let job = JobSettings {
        modules: vec![ModuleSetting {
            id: "interesting_files".into(),
            enabled: true,
            settings: serde_json::to_value(InterestingFilesSettings {
                sets: Some(vec![bad]),
                disabled_sets: Vec::new(),
            })
            .unwrap(),
        }],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(InterestingFilesFactory)];
    let err = pm_ingest::run_job(
        &c.case,
        c.ds_id,
        &job,
        &modules,
        &Arc::new(pm_ingest::IngestProgress::default()),
        &Arc::new(pm_ingest::Inbox::default()),
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("Error getting interesting files sets"),
        "{err}"
    );
}
