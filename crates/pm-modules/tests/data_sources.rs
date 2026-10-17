//! Data sources other than disk images (Autopsy's data source
//! processors), analysed end to end by the default modules.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::Path;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadProgress, LogicalOptions, add_logical_files};
use pm_casedb::{DataSourceKind, FileKindCode, FileQuery, Origin};
use pm_core::ReadAt;
use pm_ingest::{Inbox, IngestProgress, JobSettings, run_job};

const EML: &[u8] = b"From: Alice <alice@example.com>\r\nTo: Bob <bob@example.com>\r\nSubject: Logical\r\nDate: Fri, 28 May 2021 08:30:00 +0000\r\nMessage-ID: <logical@example.com>\r\n\r\nFiles added from a folder.\r\n";

fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// Autopsy "Logical Files": a folder and a loose file become one logical
/// file set read in place; the default modules analyse it like an image.
#[test]
fn logical_files_are_added_and_analysed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("evidence");
    write(
        &src,
        "notes.txt",
        b"secret plan: migrate the server on friday\n",
    );
    write(&src, "mail/invite.eml", EML);
    write(
        &src,
        "pics/photo.bmp",
        &pm_testkit::demo::gradient_bmp(64, 48),
    );
    write(&src, "pics/deeper/empty.bin", b"");
    let loose = tmp.path().join("loose.txt");
    std::fs::write(&loose, b"a loose file").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&loose, src.join("link.txt")).unwrap();

    let case = Arc::new(Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap());
    let opts = LogicalOptions {
        modified: true,
        ..LogicalOptions::default()
    };
    let s = add_logical_files(
        &case,
        &[src.clone(), loose.clone()],
        &opts,
        &LoadProgress::default(),
    )
    .unwrap();
    let db = case.db();
    let ds = db.data_source(s.ds_id).unwrap().unwrap();
    assert_eq!(ds.name, "LogicalFileSet1");
    assert_eq!(ds.kind, DataSourceKind::LogicalFiles);
    assert!(ds.size > 100, "total of the file sizes");
    #[cfg(unix)]
    assert!(
        s.warnings.iter().any(|w| w.contains("link.txt")),
        "symlinks are skipped"
    );

    let files = db
        .query_files(
            &FileQuery {
                ds_id: Some(s.ds_id),
                ..FileQuery::default()
            },
            0,
            1000,
        )
        .unwrap();
    let by_path = |p: &str| {
        files
            .iter()
            .find(|f| f.path() == p)
            .unwrap_or_else(|| panic!("no {p}"))
    };
    let root = by_path("/evidence");
    assert_eq!(root.kind, FileKindCode::Directory);
    assert_eq!(root.parent_id, None, "selected folders are top level");
    let mail = by_path("/evidence/mail/invite.eml");
    assert_eq!(mail.origin, Origin::Local);
    assert!(mail.times[0].is_some(), "modified time was asked for");
    assert!(mail.times[3].is_none(), "created time was not");
    assert_eq!(by_path("/loose.txt").size, 12);
    assert!(files.iter().all(|f| f.path() != "/evidence/link.txt"));
    let content = case.content(mail).unwrap();
    assert_eq!(content.read_vec_at(0, EML.len()).unwrap(), EML);

    // A second set gets the next default name.
    let s2 = add_logical_files(
        &case,
        &[loose],
        &LogicalOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    assert_eq!(
        db.data_source(s2.ds_id).unwrap().unwrap().name,
        "LogicalFileSet2"
    );

    // The default pipeline runs without image-only assumptions.
    let modules = pm_modules::core_modules();
    let progress = Arc::new(IngestProgress::default());
    let inbox = Arc::new(Inbox::default());
    let summary = run_job(
        &case,
        s.ds_id,
        &JobSettings::default(),
        &modules,
        &progress,
        &inbox,
    )
    .unwrap();
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let mail = db.file(mail.id).unwrap().unwrap();
    assert!(mail.md5.is_some(), "hashed");
    assert_eq!(mail.mime.as_deref(), Some("message/rfc822"));
    let email = pm_casedb::catalog::artifact_type_id("KAAMOS_EMAIL_MSG").unwrap();
    assert_eq!(
        db.artifacts_of_type(email, Some(s.ds_id), 0, 10)
            .unwrap()
            .len(),
        1,
        "the e-mail was parsed"
    );
}

/// Autopsy "Unallocated Space Image File": a raw file becomes layout
/// pieces without file-system analysis; carving finds the planted files.
#[test]
fn unallocated_space_image_is_carved() {
    let tmp = tempfile::tempdir().unwrap();
    // Noise with a picture in the middle.
    let mut raw = vec![0x5Au8; 300_000];
    let pic = pm_testkit::demo::gradient_bmp(40, 30);
    // PhotoRec looks for headers at sector boundaries.
    raw[102_400..102_400 + pic.len()].copy_from_slice(&pic);
    let path = tmp.path().join("unalloc.bin");
    std::fs::write(&path, &raw).unwrap();
    let case = Arc::new(Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap());
    let opts = pm_case::RawOptions {
        chunk: Some(128_000),
        ..pm_case::RawOptions::default()
    };
    let s = pm_case::add_unallocated_image(&case, &path, &opts, &LoadProgress::default()).unwrap();
    assert_eq!(s.files, 3, "128 kB pieces of 300 kB");
    let db = case.db();
    let ds = db.data_source(s.ds_id).unwrap().unwrap();
    assert_eq!(ds.kind, DataSourceKind::UnallocatedImage);
    let pieces = db
        .query_files(
            &FileQuery {
                ds_id: Some(s.ds_id),
                ..FileQuery::default()
            },
            0,
            10,
        )
        .unwrap();
    assert!(pieces.iter().all(|f| f.origin == Origin::UnallocBlocks));
    let second = case.content(&pieces[1]).unwrap();
    assert_eq!(second.size(), 128_000);
    assert_eq!(second.read_vec_at(0, 4).unwrap(), raw[128_000..128_004]);

    let modules = pm_modules::core_modules();
    let summary = run_job(
        &case,
        s.ds_id,
        &JobSettings::default(),
        &modules,
        &Arc::new(IngestProgress::default()),
        &Arc::new(Inbox::default()),
    )
    .unwrap();
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let carved = db
        .query_files(
            &FileQuery {
                ds_id: Some(s.ds_id),
                ..FileQuery::default()
            },
            0,
            100,
        )
        .unwrap()
        .into_iter()
        .filter(|f| f.origin == Origin::Carved)
        .count();
    assert!(carved >= 1, "the planted picture was carved");
}

/// Autopsy "Local Disk": a device read in place (here an image file
/// stands in for the device) is recorded as a local disk.
#[test]
fn local_disk_is_recorded_as_such() {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let s = pm_case::add_local_disk(
        &case,
        pm_testkit::demo::demo_disk_file(),
        &pm_case::LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    let ds = case.db().data_source(s.ds_id).unwrap().unwrap();
    assert_eq!(ds.kind, DataSourceKind::LocalDisk);
    assert!(s.files > 40);
    // Whatever disks this machine lets us read are listed with a size.
    for d in pm_case::local_disks() {
        eprintln!("{} {:?}", d.path.display(), d.size);
    }
}
