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

fn utf16_report(text: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in text.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Autopsy "XRY Text Export": the folder becomes a file set and the
/// reports' calls and messages become artifacts with related accounts.
#[test]
fn xry_text_export_is_parsed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("xry");
    std::fs::create_dir(&src).unwrap();
    let header = |kind: &str| format!("XRY export\r\nPhone\r\n{kind}\r\nCase 1\r\n\r\n");
    std::fs::write(
        src.join("Calls.txt"),
        utf16_report(&format!(
            "{}Call # 1\r\nType:\tDialed\r\nTime:\t5/28/2021 8:30:00 AM (Network)\r\nTo\r\nTel:\t0401111111\r\n",
            header("Calls")
        )),
    )
    .unwrap();
    std::fs::write(
        src.join("Messages.txt"),
        utf16_report(&format!(
            "{}Message # 1\r\nType:\tIncoming\r\nFrom\r\nTel:\t0401111111\r\nText:\tsee you at the pier\r\n",
            header("Messages/SMS")
        )),
    )
    .unwrap();
    std::fs::write(src.join("attachment.jpg"), b"\xFF\xD8\xFF").unwrap();

    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let s = pm_case::add_data_source(
        &case,
        &pm_case::DataSourceSpec::XryFolder(src.clone()),
        &pm_case::LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap()
    .remove(0);
    let db = case.db();
    assert_eq!(
        db.data_source(s.ds_id).unwrap().unwrap().name,
        "XRY Text Export"
    );
    assert_eq!(s.files, 3, "the reports and the other files");
    let ty = |n| pm_casedb::catalog::artifact_type_id(n).unwrap();
    let calls = db
        .artifacts_of_type(ty("KAAMOS_CALLLOG"), Some(s.ds_id), 0, 100)
        .unwrap();
    assert_eq!(calls.len(), 1);
    let msgs = db
        .artifacts_of_type(ty("KAAMOS_MESSAGE"), Some(s.ds_id), 0, 100)
        .unwrap();
    assert_eq!(msgs.len(), 1);
    let text = db.attributes(msgs[0].id).unwrap();
    assert!(
        text.iter()
            .any(|a| a.value == pm_casedb::Value::Text("see you at the pier".into()))
    );
    let phone = db
        .account("PHONE", "0401111111")
        .unwrap()
        .expect("phone account");
    let related = db
        .related_accounts(phone.id, &pm_casedb::communications::CommsFilter::default())
        .unwrap();
    assert_eq!(related.len(), 1, "related to the device");
    assert_eq!(related[0].1, 2, "a call and a message");
}

const RESULTS_HEADER: &str = "VHD file\tFile system offset\tFile metadata address\tExtraction status\tRule set name\tRule name\tDescription\tFilename\tPath\tExtractFilePath\tcrtime\tmtime\tatime\tctime\n";

/// Autopsy "Logical Imager Results" without drive images: the extracted
/// files become a logical file set and every hit an Interesting Item.
#[test]
fn logical_imager_extracted_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("Logical_Imager_HOST_20210528");
    write(&src, "root/f-1.txt", b"secret plan");
    write(&src, "HOST_users.txt", b"bob\n");
    write(
        &src,
        "SearchResults.txt",
        format!("{RESULTS_HEADER}sparse_image.vhd\t0\t77\t0\tSet A\tPlans\td\tplan.txt\t/Users/bob/Desktop\troot/f-1.txt\t1600000000\t1600000100\t1600000200\t1600000300\n").as_bytes(),
    );
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let added = pm_case::add_data_source(
        &case,
        &pm_case::DataSourceSpec::LogicalImager(src.clone()),
        &pm_case::LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    assert_eq!(added.len(), 1);
    let db = case.db();
    let ds = db.data_source(added[0].ds_id).unwrap().unwrap();
    assert_eq!(ds.name, "Logical_Imager_HOST_20210528");
    assert!(
        case.module_output_dir()
            .join("LogicalImager/Logical_Imager_HOST_20210528/root/f-1.txt")
            .is_file(),
        "copied into the case"
    );
    assert_eq!(db.reports().unwrap().len(), 2, "search results and users");
    let files = db
        .query_files(
            &FileQuery {
                ds_id: Some(ds.id),
                ..FileQuery::default()
            },
            0,
            100,
        )
        .unwrap();
    let plan = files.iter().find(|f| f.name == "plan.txt").unwrap();
    assert_eq!(
        plan.parent_path,
        "/root/sparse_image.vhd/Users/bob/Desktop/"
    );
    assert_eq!(plan.size, 11);
    assert_eq!(plan.times[0], Some((1_600_000_100, 0)), "mtime");
    assert_eq!(plan.times[3], Some((1_600_000_000, 0)), "crtime");
    let content = case.content(plan).unwrap();
    assert_eq!(content.read_vec_at(0, 6).unwrap(), b"secret");
    let hits = db
        .artifacts_of_type(
            pm_casedb::catalog::artifact_type_id("KAAMOS_INTERESTING_ITEM").unwrap(),
            Some(ds.id),
            0,
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].file_id, Some(plan.id));
}

/// Logical imager results with a sparse drive image: the VHD is added as a
/// disk image and hits are found by metadata address and name.
#[test]
fn logical_imager_sparse_vhd() {
    let tmp = tempfile::tempdir().unwrap();
    // Find a file of the demo disk to name in the results.
    let probe = Case::create(tmp.path().join("probe"), CaseDetails::default()).unwrap();
    let p = pm_case::add_image(
        &probe,
        pm_testkit::demo::demo_disk_file(),
        &pm_case::LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    let target = probe
        .db()
        .query_files(
            &FileQuery {
                ds_id: Some(p.ds_id),
                ..FileQuery::default()
            },
            0,
            1000,
        )
        .unwrap()
        .into_iter()
        .find(|f| {
            f.kind == FileKindCode::Regular && f.origin == Origin::FileSystem && f.inode.is_some()
        })
        .unwrap();
    let src = tmp.path().join("imager");
    write(
        &src,
        "sparse_image.vhd",
        &pm_testkit::vdisk::vhd_dynamic(&pm_testkit::demo::demo_disk(), 2 << 20),
    );
    write(
        &src,
        "SearchResults.txt",
        format!(
            "{RESULTS_HEADER}sparse_image.vhd\t0\t{}\t0\tSet A\tAny\td\t{}\t{}\t\t0\t0\t0\t0\n",
            target.inode.unwrap(),
            target.name,
            target.parent_path
        )
        .as_bytes(),
    );
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let added = pm_case::add_data_source(
        &case,
        &pm_case::DataSourceSpec::LogicalImager(src),
        &pm_case::LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    assert_eq!(added.len(), 1);
    let db = case.db();
    let hits = db
        .artifacts_of_type(
            pm_casedb::catalog::artifact_type_id("KAAMOS_INTERESTING_ITEM").unwrap(),
            Some(added[0].ds_id),
            0,
            10,
        )
        .unwrap();
    assert_eq!(hits.len(), 1);
    let f = db.file(hits[0].file_id.unwrap()).unwrap().unwrap();
    assert_eq!(f.name, target.name);
}

/// An L01 among logical files is extracted (as Autopsy does with
/// `ewfexport`) and its file-entry tree added with the recorded times.
#[test]
fn l01_in_logical_files_is_extracted() {
    let l01 = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/ewf/small.L01");
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_logical_files(
        &case,
        &[l01],
        &LogicalOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    assert!(s.warnings.is_empty(), "{:?}", s.warnings);
    let files = case
        .db()
        .query_files(
            &FileQuery {
                ds_id: Some(s.ds_id),
                ..FileQuery::default()
            },
            0,
            100,
        )
        .unwrap();
    let paths: Vec<String> = files
        .iter()
        .map(|f| format!("{}{}", f.parent_path, f.name))
        .collect();
    for p in [
        "/small.L01",
        "/small.L01/docs",
        "/small.L01/docs/a.txt",
        "/small.L01/docs/Ääkköset.txt",
        "/small.L01/b.bin",
    ] {
        assert!(paths.iter().any(|x| x == p), "{p} in {paths:?}");
    }
    let a = files.iter().find(|f| f.name == "a.txt").unwrap();
    assert_eq!(a.size, 17);
    assert_eq!(a.times[0], Some((1_600_000_003, 0)), "written");
    assert_eq!(a.times[3], Some((1_600_000_001, 0)), "created");
    assert_eq!(
        case.content(a).unwrap().read_vec_at(0, 17).unwrap(),
        b"hello from a.txt\n"
    );
    let b = files.iter().find(|f| f.name == "b.bin").unwrap();
    let data = case.content(b).unwrap().read_vec_at(0, 512).unwrap();
    assert_eq!(&data[..3], &[0, 1, 2]);
    assert_eq!(data[511], 0);
    assert_eq!(case.db().data_source(s.ds_id).unwrap().unwrap().size, 529);
}
