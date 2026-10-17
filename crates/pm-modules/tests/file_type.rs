#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

//! End-to-end: File Type Identification on generated NTFS volumes and the
//! demo disk.

mod common;

use std::sync::Arc;

use pm_casedb::{FileKindCode, Origin, Significance};
use pm_filetype::cfb::builder::{Stream, build as build_cfb};
use pm_filetype::{FileType, Signature, ZipBuilder};
use pm_ingest::{
    Inbox, IngestFile, IngestProgress, JobSettings, MessageKind, ModuleFactory, ModuleSetting,
    run_job,
};
use pm_modules::file_type::{FileTypeFactory, FileTypeSettings, default_detector, mime_or_detect};
use pm_modules::picture_analyzer::PictureAnalyzerFactory;
use pm_testkit::ntfs::{FileOpts, NtfsBuilder};

use common::DemoCase;

const OOXML_TYPES: &[u8] = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

fn noise(n: usize) -> Vec<u8> {
    let mut x = 0x2545_F491u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn image() -> Vec<u8> {
    let mut b = NtfsBuilder::new(16);
    b.dir("/docs").dir("/WINDOWS").dir("/WINDOWS/system32");
    b.file(
        "/docs/report.pdf",
        b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n1 0 obj\n",
    )
    .file(
        "/docs/pic.png",
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0",
    )
    .file(
        "/docs/letter.docx",
        &ZipBuilder::new()
            .deflated("[Content_Types].xml", OOXML_TYPES)
            .deflated("word/document.xml", b"<w:document/>")
            .build(),
    )
    .file(
        "/docs/old.doc",
        &build_cfb(&[Stream::root("WordDocument", &[0u8; 700])]),
    )
    .file("/docs/notes.txt", b"Meeting notes: bring the keys.\n")
    .file("/docs/notes", b"Meeting notes without an extension.\n")
    .file(
        "/docs/page.html",
        b"<!DOCTYPE html><html><body>hello</body></html>",
    )
    .file(
        "/docs/data.xml",
        b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .file("/docs/empty.dat", b"")
    .file("/docs/random.bin", &noise(5000))
    .file("/docs/legacy.txt", &[0xE4u8; 300])
    .file("/docs/legacy.bin", &[0xE4u8; 300])
    .file(
        "/docs/song.mp3",
        &[0xFF, 0xFF, 0xFF, 0xFB, 0x90, 0x00, 0x11],
    )
    .file("/docs/a.mine", b"MINE some payload \xAA")
    .file("/docs/b.mine", b"MINE some payload \xAB")
    .file("/docs/c.other", b"OTHR payload")
    .file(
        "/WINDOWS/system32/SYSTEM",
        &[b"regf".as_slice(), &[0u8; 600]].concat(),
    )
    .file_with(
        "/docs/setup.exe",
        &[b"MZ\x90\x00".as_slice(), &[0u8; 60]].concat(),
        FileOpts {
            ads: vec![(
                "Zone.Identifier".into(),
                b"[ZoneTransfer]\r\nZoneId=3\r\n".to_vec(),
            )],
            ..FileOpts::default()
        },
    );
    b.build()
}

fn user_types() -> Vec<FileType> {
    let mut mine = FileType::new(
        "application/x-mine",
        vec![
            Signature::ascii("MINE", 0),
            Signature::raw(&[0xAA], 0).from_end(),
        ],
    );
    mine.create_interesting_file_hit = true;
    mine.interesting_files_set_name = "Mine files".into();
    // A type without hits.
    let other = FileType::new("application/x-other", vec![Signature::ascii("OTHR", 0)]);
    vec![mine, other]
}

fn settings(types: Vec<FileType>) -> serde_json::Value {
    serde_json::to_value(FileTypeSettings { user_types: types }).unwrap()
}

#[test]
fn types_files_like_autopsy() {
    let c = DemoCase::with_image(&image());
    let (summary, inbox) = c.run_one(Arc::new(FileTypeFactory), settings(user_types()));
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let mime = |p: &str| c.file(p).mime;
    let want = [
        ("/docs/report.pdf", "application/pdf"),
        ("/docs/pic.png", "image/png"),
        (
            "/docs/letter.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        ),
        ("/docs/old.doc", "application/msword"),
        ("/docs/notes.txt", "text/plain"),
        ("/docs/notes", "text/plain"),
        ("/docs/page.html", "text/html"),
        // Autopsy's own "<?xml" signature wins over Tika's SVG root.
        ("/docs/data.xml", "text/xml"),
        ("/docs/empty.dat", "application/octet-stream"),
        ("/docs/random.bin", "application/octet-stream"),
        // `.txt` with a legacy 8-bit encoding is text; other names are not.
        ("/docs/legacy.txt", "text/plain"),
        ("/docs/legacy.bin", "application/octet-stream"),
        // "audio/mpeg" starting with FF FF is rejected.
        ("/docs/song.mp3", "application/octet-stream"),
        ("/docs/a.mine", "application/x-mine"),
        ("/docs/b.mine", "text/plain"),
        ("/docs/c.other", "application/x-other"),
        ("/WINDOWS/system32/SYSTEM", "application/x.windows-registry"),
        // A DOS stub without a PE header.
        ("/docs/setup.exe", "application/x-dosexec"),
        ("/docs/setup.exe:Zone.Identifier", "text/plain"),
    ];
    for (p, m) in want {
        assert_eq!(mime(p).as_deref(), Some(m), "{p}");
    }
    // Directories are not typed.
    for d in c
        .files()
        .iter()
        .filter(|f| f.kind == FileKindCode::Directory)
    {
        assert_eq!(d.mime, None, "{}", d.path());
    }

    // One interesting item, for the user type that asks for it.
    let hits = c.artifacts("KAAMOS_INTERESTING_ITEM");
    assert_eq!(hits.len(), 1, "{hits:?}");
    let (a, attrs) = &hits[0];
    assert_eq!(a.file_id, Some(c.file("/docs/a.mine").id));
    assert_eq!(a.significance, Some(Significance::LikelyNotable));
    assert_eq!(a.configuration.as_deref(), Some("Mine files"));
    assert_eq!(attrs["KAAMOS_SET_NAME"], "Mine files");
    assert_eq!(attrs["KAAMOS_CATEGORY"], "application/x-mine");

    // The summary message.
    let msgs = inbox.since(0);
    let m = msgs
        .iter()
        .find(|m| m.subject == "File Type Id Results")
        .expect("summary");
    assert_eq!(m.kind, MessageKind::Info);
    assert_eq!(m.source.as_deref(), Some("File Type Identification"));
    // Every scheduled file, directories included, counts.
    let filter = JobSettings::default().filter;
    let scheduled = c.files().iter().filter(|f| filter.accepts(f)).count();
    assert!(
        m.details
            .contains(&format!("Total Files Processed: {scheduled}")),
        "{} / {scheduled}",
        m.details
    );

    // Re-ingest: no duplicate result, types unchanged.
    c.run_one(Arc::new(FileTypeFactory), settings(user_types()));
    assert_eq!(c.artifacts("KAAMOS_INTERESTING_ITEM").len(), 1);
    assert_eq!(mime("/docs/report.pdf").as_deref(), Some("application/pdf"));
}

#[test]
fn invalid_user_types_fail_start_up() {
    let c = DemoCase::with_image(&image());
    for bad in [
        FileType::new("application/x-a", vec![]),
        FileType::new("", vec![Signature::ascii("A", 0)]),
        FileType {
            create_interesting_file_hit: true,
            ..FileType::new("application/x-a", vec![Signature::ascii("A", 0)])
        },
    ] {
        let job = JobSettings {
            modules: vec![ModuleSetting {
                id: "file_type".into(),
                enabled: true,
                settings: settings(vec![bad]),
            }],
            ..JobSettings::default()
        };
        let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(FileTypeFactory)];
        let err = run_job(
            &c.case,
            c.ds_id,
            &job,
            &modules,
            &Arc::new(IngestProgress::default()),
            &Arc::new(Inbox::default()),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Unable to run file type detection."),
            "{err}"
        );
    }
    // Nothing was typed.
    assert!(c.files().iter().all(|f| f.mime.is_none()));
}

/// Unallocated space, FAT virtual files and the demo disk's contents;
/// the Picture Analyzer reads the types File Type Identification set.
#[test]
fn demo_disk_rules_and_pipeline() {
    let c = DemoCase::new();
    let modules: Vec<Arc<dyn ModuleFactory>> =
        vec![Arc::new(FileTypeFactory), Arc::new(PictureAnalyzerFactory)];
    let (summary, _) = c.run(&JobSettings::default(), modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let files = c.files();
    let unalloc: Vec<_> = files
        .iter()
        .filter(|f| f.origin == Origin::UnallocBlocks)
        .collect();
    assert!(!unalloc.is_empty());
    for f in unalloc {
        assert_eq!(
            f.mime.as_deref(),
            Some("application/octet-stream"),
            "{}",
            f.path()
        );
    }
    let virt: Vec<_> = files
        .iter()
        .filter(|f| f.kind == FileKindCode::Virtual)
        .collect();
    assert!(!virt.is_empty());
    for f in virt {
        assert_eq!(
            f.mime.as_deref(),
            Some("application/octet-stream"),
            "{}",
            f.path()
        );
    }
    for f in files
        .iter()
        .filter(|f| f.kind == FileKindCode::Directory || f.origin == Origin::VirtualDir)
    {
        assert_eq!(f.mime, None, "{}", f.path());
    }
    let by_name = |n: &str| files.iter().find(|f| f.name == n).unwrap();
    assert_eq!(by_name("readme.txt").mime.as_deref(), Some("text/plain"));
    assert_eq!(
        by_name("setup.exe").mime.as_deref(),
        Some("application/x-dosexec")
    );
    // Every regular file the job schedules, allocated or not, has a type.
    let filter = JobSettings::default().filter;
    let untyped: Vec<String> = files
        .iter()
        .filter(|f| f.kind == FileKindCode::Regular && filter.accepts(f) && f.mime.is_none())
        .map(pm_casedb::FileRecord::path)
        .collect();
    assert!(untyped.is_empty(), "{untyped:?}");
}

#[test]
fn mime_or_detect_uses_stored_or_detects() {
    let c = DemoCase::with_image(&image());
    let rec = c.file("/docs/report.pdf");
    let mut f = IngestFile::new(rec.clone(), Arc::clone(&c.case));
    assert_eq!(f.mime(), None);
    assert_eq!(
        mime_or_detect(&mut f, default_detector()).as_deref(),
        Some("application/pdf")
    );
    // Recorded on the file for the framework to save.
    assert_eq!(f.mime(), Some("application/pdf"));

    // A stored type wins; legacy parameters are removed.
    let mut stored = rec;
    stored.mime = Some("text/plain; charset=UTF-8".into());
    let mut f = IngestFile::new(stored, Arc::clone(&c.case));
    assert_eq!(
        mime_or_detect(&mut f, default_detector()).as_deref(),
        Some("text/plain")
    );

    // Directories have none.
    let mut d = IngestFile::new(c.file("/docs"), Arc::clone(&c.case));
    assert_eq!(mime_or_detect(&mut d, default_detector()), None);
}
