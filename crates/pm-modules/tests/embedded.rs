//! Embedded File Extractor end to end: archives and documents inside an
//! NTFS volume are extracted, re-injected and analysed recursively
//! (Autopsy `EmbeddedFileTest`, rebuilt with generated fixtures).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::collections::HashMap;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::{FileKindCode, FileQuery, FileRecord, Origin};
use pm_core::ReadAt;
use pm_ingest::{
    Inbox, IngestProgress, JobSettings, JobSummary, MessageKind, ModuleFactory, ModuleSetting,
    run_job,
};
use pm_modules::embedded::{
    ENCRYPTION_FILE_LEVEL, ENCRYPTION_FULL, EmbeddedFileExtractorFactory, NAME, ZIP_BOMB_SET,
};
use pm_modules::file_type::FileTypeFactory;
use pm_modules::hash_lookup::HashLookupFactory;
use pm_testkit::archives::{self as ak, Method, ZipBuilder, ZipEntry};
use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

/// MD5 of "test" (Autopsy's `HASH_VALUE`).
const TEST_MD5: &str = "098f6bcd4621d373cade4e832627b4f6";

struct Fixture {
    _dir: tempfile::TempDir,
    case: Arc<Case>,
    ds_id: i64,
}

fn fixture(b: &NtfsBuilder) -> Fixture {
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

impl Fixture {
    fn run_with(&self, settings: &JobSettings) -> (JobSummary, Arc<Inbox>) {
        self.run_modules(
            settings,
            &[
                Arc::new(HashLookupFactory),
                Arc::new(EmbeddedFileExtractorFactory),
            ],
        )
    }

    fn run_modules(
        &self,
        settings: &JobSettings,
        modules: &[Arc<dyn ModuleFactory>],
    ) -> (JobSummary, Arc<Inbox>) {
        let inbox = Arc::new(Inbox::default());
        let s = run_job(
            &self.case,
            self.ds_id,
            settings,
            modules,
            &Arc::new(IngestProgress::default()),
            &inbox,
        )
        .unwrap();
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        (s, inbox)
    }

    fn run(&self) -> (JobSummary, Arc<Inbox>) {
        self.run_with(&JobSettings {
            threads: 4,
            ..JobSettings::default()
        })
    }

    fn files(&self) -> Vec<FileRecord> {
        self.case
            .db()
            .query_files(
                &FileQuery {
                    ds_id: Some(self.ds_id),
                    ..FileQuery::default()
                },
                0,
                1_000_000,
            )
            .unwrap()
    }

    fn by_path(&self) -> HashMap<String, FileRecord> {
        self.files().into_iter().map(|f| (f.path(), f)).collect()
    }

    fn file(&self, path: &str) -> FileRecord {
        let all = self.by_path();
        all.get(path)
            .cloned()
            .unwrap_or_else(|| panic!("no {path}; derived: {:?}", derived_paths(&all)))
    }

    fn content(&self, f: &FileRecord) -> Vec<u8> {
        let src = self.case.content(f).unwrap();
        src.read_vec_at(0, usize::try_from(src.size()).unwrap())
            .unwrap()
    }

    /// `(type name, attributes)` of a file's artifacts.
    fn artifacts(&self, f: &FileRecord) -> Vec<(String, HashMap<String, String>)> {
        let db = self.case.db();
        let names: HashMap<i64, String> = db
            .artifact_type_names()
            .unwrap()
            .into_iter()
            .map(|t| (t.0, t.1))
            .collect();
        db.artifacts_for_file(f.id)
            .unwrap()
            .into_iter()
            .map(|a| {
                let attrs = db
                    .attributes(a.id)
                    .unwrap()
                    .into_iter()
                    .map(|x| (x.name.clone(), x.value.display()))
                    .collect();
                (names[&a.type_id].clone(), attrs)
            })
            .collect()
    }
}

fn derived_paths(all: &HashMap<String, FileRecord>) -> Vec<&String> {
    let mut v: Vec<&String> = all
        .iter()
        .filter(|(_, f)| f.origin == Origin::Derived)
        .map(|(p, _)| p)
        .collect();
    v.sort();
    v
}

fn md5_hex(f: &FileRecord) -> String {
    f.md5
        .as_deref()
        .map(pm_core::decode::to_hex)
        .unwrap_or_default()
}

/// The archives of Autopsy's `EmbeddedFileTest` image, generated.
fn archives_volume() -> NtfsBuilder {
    let level1 = ak::zip(&[("level1.txt", b"test")]);
    let level2 = ak::zip(&[("level2.txt", b"test"), ("level1.zip", &level1)]);
    let level3 = ak::zip(&[("level3.txt", b"test"), ("level2.zip", &level2)]);
    let renamed = ak::zip(&[("file.txt", b"test")]);
    let mut big = ZipBuilder::new();
    big.dir("big folder");
    for i in 1..=1000 {
        big.file(&format!("big folder/file{i}.txt"), b"test");
    }
    let mut deep = ZipBuilder::new();
    let mut path = String::from("deep folder");
    deep.dir(&path);
    for i in 1..=25 {
        path = format!("{path}/dir{i}");
        deep.dir(&path);
        deep.file(&format!("{path}/file.txt"), b"test");
    }
    let tar = ak::tar(&[
        ak::TarEntry::Dir("t".into()),
        ak::TarEntry::File("t/in_tar.txt".into(), b"test".to_vec()),
    ]);
    let tgz = ak::gzip(None, &tar);
    let seven = ak::sevenz(&[("in7z.txt", b"test")], &[], true, None, false);
    let rar = ak::rar(rars::ArchiveVersion::Rar50, &[("inrar.txt", b"test")], None);
    let mut b = NtfsBuilder::new(32);
    for d in [
        "ZIP",
        "ZIP/embedded",
        "ZIP/extension",
        "ZIP/big",
        "ZIP/other",
    ] {
        b.dir(d);
    }
    b.file("ZIP/embedded/level3.zip", &level3);
    b.file("ZIP/extension/zipFileWithTxtExtension.txt", &renamed);
    b.file("ZIP/big/big.zip", &big.build());
    b.file("ZIP/big/deep.zip", &deep.build());
    b.file("ZIP/other/archive.tgz", &tgz);
    b.file("ZIP/other/solid.7z", &seven);
    b.file("ZIP/other/v5.rar", &rar);
    b.file("ZIP/other/plain.txt", b"not an archive");
    b
}

#[test]
fn archives_are_extracted_recursively() {
    let f = fixture(&archives_volume());
    let before = f.files().len();
    let (_, inbox) = f.run();
    let all = f.by_path();

    // EmbeddedFile: nested zips at /<parent>/<archive>/<entry>.
    for p in [
        "/ZIP/embedded/level3.zip/level3.txt",
        "/ZIP/embedded/level3.zip/level2.zip/level2.txt",
        "/ZIP/embedded/level3.zip/level2.zip/level1.zip/level1.txt",
    ] {
        let x = &all.get(p).unwrap_or_else(|| panic!("{p}"));
        assert_eq!(x.origin, Origin::Derived);
        assert_eq!(f.content(x), b"test");
        assert_eq!(md5_hex(x), TEST_MD5, "{p} was hashed");
        assert!(
            x.local_path
                .as_deref()
                .unwrap()
                .starts_with("ModuleOutput/EFE/")
        );
    }
    let level2 = f.file("/ZIP/embedded/level3.zip/level2.zip");
    let level3 = f.file("/ZIP/embedded/level3.zip");
    assert_eq!(level2.parent_id, Some(level3.id));
    assert_eq!(level2.derived_from, Some(level3.id));
    assert_eq!(level2.times[0], Some((ak::MTIME, 0)), "zip times kept");

    // Extension: a zip named .txt is found by its signature.
    let inner = f.file("/ZIP/extension/zipFileWithTxtExtension.txt/file.txt");
    assert_eq!(f.content(&inner), b"test");

    // BigFolder: 1000 files (two local sub-folders, two insert batches).
    let big: Vec<&FileRecord> = all
        .values()
        .filter(|x| x.parent_path == "/ZIP/big/big.zip/big folder/")
        .collect();
    assert_eq!(big.len(), 1000);
    for x in &big {
        assert!(x.name.starts_with("file") && x.extension == "txt");
        assert_eq!(md5_hex(x), TEST_MD5);
    }
    let last = f.file("/ZIP/big/big.zip/big folder/file1000.txt");
    assert!(last.local_path.as_deref().unwrap().ends_with("/1/1000"));

    // DeepFolder: 25 levels, one file.txt per level.
    let mut dir = f.file("/ZIP/big/deep.zip/deep folder");
    for i in 1..=25 {
        let next = f.file(&format!("{}/dir{i}", dir.path()));
        assert_eq!(next.kind, FileKindCode::Directory);
        assert_eq!(next.parent_id, Some(dir.id));
        let txt = f.file(&format!("{}/file.txt", next.path()));
        assert_eq!(md5_hex(&txt), TEST_MD5);
        dir = next;
    }

    // tar.gz → tar (Autopsy's default name) → files; 7z; RAR.
    let t = f.file("/ZIP/other/archive.tgz/archive.tar/t/in_tar.txt");
    assert_eq!(f.content(&t), b"test");
    for p in [
        "/ZIP/other/solid.7z/in7z.txt",
        "/ZIP/other/v5.rar/inrar.txt",
    ] {
        assert_eq!(md5_hex(&f.file(p)), TEST_MD5, "{p}");
    }
    assert!(!all.keys().any(|p| p.starts_with("/ZIP/other/plain.txt/")));

    // Raw files are XOR1-encoded in the module's folder.
    let raw = std::fs::read(f.case.dir().join(t.local_path.as_deref().unwrap())).unwrap();
    assert!(raw.starts_with(pm_case::derived::XOR1_HEADER));

    // No artifacts, no warnings; every row is accounted for.
    for x in all.values() {
        assert!(f.artifacts(x).is_empty(), "{}", x.path());
    }
    let msgs = inbox.since(0);
    assert!(
        msgs.iter().all(|m| m.source.as_deref() != Some(NAME)),
        "{msgs:#?}"
    );
    let derived = all.values().filter(|x| x.origin == Origin::Derived).count();
    assert_eq!(all.len(), before + derived);
}

#[test]
fn reingest_neither_duplicates_nor_reprocesses() {
    let f = fixture(&archives_volume());
    f.run();
    let first = f.files();
    let (s, _) = f.run();
    let second = f.files();
    assert_eq!(first.len(), second.len(), "no duplicates");
    // Every file ran exactly once in the second job.
    assert_eq!(s.files, second.len() as u64);

    // A member whose stored copy is smaller than the archive's is
    // re-extracted (Autopsy `updateDerivedFile`) and hashed again.
    let x = f.file("/ZIP/embedded/level3.zip/level3.txt");
    f.case
        .db()
        .update_derived_file(
            x.id,
            &pm_casedb::DerivedUpdate {
                size: 1,
                times: x.times,
                local_path: x.local_path.clone(),
                reset_mime: false,
            },
        )
        .unwrap();
    f.run();
    let y = f.file("/ZIP/embedded/level3.zip/level3.txt");
    assert_eq!(y.id, x.id);
    assert_eq!(y.size, 4);
    assert_eq!(md5_hex(&y), TEST_MD5);
    assert_eq!(f.files().len(), second.len());
}

/// A zip nested `levels` deep: `name` → d1.zip → … → `d<levels>.zip` →
/// leaf.txt.
fn nested(levels: u32) -> Vec<u8> {
    let mut cur = ak::zip(&[("leaf.txt", b"leaf")]);
    for i in (1..=levels).rev() {
        cur = ak::zip(&[(&format!("d{i}.zip"), &cur)]);
    }
    cur
}

fn bombs_volume() -> NtfsBuilder {
    let mut ratio = ZipBuilder::new();
    ratio.file("small.txt", b"fine");
    ratio.add(ZipEntry::new("zeros.bin", vec![0u8; 4096]).declared_size(600_000_000));
    let mut protected = ZipBuilder::new();
    protected.add(ZipEntry::new("secret.txt", b"test".to_vec()).password("pw"));
    protected.add(ZipEntry::new("secret2.txt", b"test".to_vec()).password("pw"));
    let mut partly = ZipBuilder::new();
    partly.file("open.txt", b"test");
    partly.add(ZipEntry::new("aes.txt", b"test".to_vec()).fake_aes());
    let header_encrypted = ak::sevenz(&[("x.txt", b"test")], &[], false, Some("pw"), true);
    let mut b = NtfsBuilder::new(16);
    b.dir("bombs");
    b.dir("enc");
    b.file("bombs/RatioTriggerZipBomb.zip", &ratio.build());
    b.file("bombs/DepthTriggerZipBomb.zip", &nested(5));
    b.file("bombs/ok_depth.zip", &nested(3));
    b.file_with(
        "bombs/deleted_ratio.zip",
        &ratio.build(),
        FileOpts {
            deleted: Deleted::Unindexed,
            ..FileOpts::default()
        },
    );
    b.file("enc/password_protected.zip", &protected.build());
    b.file("enc/partly_protected.zip", &partly.build());
    b.file("enc/header.7z", &header_encrypted);
    b
}

#[test]
fn zip_bombs_and_encryption_are_reported() {
    let f = fixture(&bombs_volume());
    let (_, inbox) = f.run();
    let all = f.by_path();
    let mut flagged = Vec::new();
    for x in all.values() {
        for (ty, attrs) in f.artifacts(x) {
            flagged.push((x.path(), ty, attrs));
        }
    }
    flagged.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<(&str, &str)> = flagged
        .iter()
        .map(|(p, t, _)| (p.as_str(), t.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("/bombs/DepthTriggerZipBomb.zip", "KAAMOS_INTERESTING_ITEM"),
            ("/bombs/RatioTriggerZipBomb.zip", "KAAMOS_INTERESTING_ITEM"),
            ("/enc/header.7z", "KAAMOS_ENCRYPTION_DETECTED"),
            ("/enc/partly_protected.zip", "KAAMOS_ENCRYPTION_DETECTED"),
            ("/enc/password_protected.zip", "KAAMOS_ENCRYPTION_DETECTED"),
        ]
    );
    let attrs = |path: &str| &flagged.iter().find(|x| x.0 == path).unwrap().2;

    let depth = attrs("/bombs/DepthTriggerZipBomb.zip");
    assert_eq!(depth["KAAMOS_SET_NAME"], ZIP_BOMB_SET);
    assert_eq!(depth["KAAMOS_DESCRIPTION"], "Zip Bomb Detected d4.zip");
    assert_eq!(
        depth["KAAMOS_COMMENT"],
        "The archive is 4 levels deep, skipping processing of _bombs_DepthTriggerZipBomb.zip"
    );
    // d1..d4 were extracted, d4 itself was not.
    assert!(all.contains_key("/bombs/DepthTriggerZipBomb.zip/d1.zip/d2.zip/d3.zip/d4.zip"));
    assert!(!all.keys().any(|p| p.ends_with("d4.zip/d5.zip")));
    // Three levels are fine.
    assert!(all.contains_key("/bombs/ok_depth.zip/d1.zip/d2.zip/d3.zip/leaf.txt"));

    let ratio = attrs("/bombs/RatioTriggerZipBomb.zip");
    assert_eq!(ratio["KAAMOS_SET_NAME"], ZIP_BOMB_SET);
    assert_eq!(
        ratio["KAAMOS_DESCRIPTION"],
        "Zip Bomb Detected RatioTriggerZipBomb.zip"
    );
    assert!(
        ratio["KAAMOS_COMMENT"].starts_with("Compression ratio is ")
            && ratio["KAAMOS_COMMENT"]
                .ends_with(", skipping items in _bombs_RatioTriggerZipBomb.zip."),
        "{}",
        ratio["KAAMOS_COMMENT"]
    );
    // Autopsy checks every item before extracting anything.
    assert!(
        !all.keys()
            .any(|p| p.starts_with("/bombs/RatioTriggerZipBomb.zip/"))
    );
    // A deleted archive is not ratio-checked: its members are extracted.
    let deleted_member = all
        .values()
        .find(|x| x.name == "small.txt" && x.parent_path.ends_with("deleted_ratio.zip/"));
    assert!(deleted_member.is_some(), "{:?}", derived_paths(&all));

    assert_eq!(
        attrs("/enc/password_protected.zip")["KAAMOS_COMMENT"],
        ENCRYPTION_FULL
    );
    assert_eq!(
        attrs("/enc/partly_protected.zip")["KAAMOS_COMMENT"],
        ENCRYPTION_FILE_LEVEL
    );
    assert_eq!(attrs("/enc/header.7z")["KAAMOS_COMMENT"], ENCRYPTION_FULL);
    // Encrypted members are listed without content and not hashed; the
    // readable member is extracted.
    let secret = f.file("/enc/password_protected.zip/secret.txt");
    assert!(secret.local_path.is_none() && secret.md5.is_none());
    assert_eq!(secret.size, 0);
    assert_eq!(
        md5_hex(&f.file("/enc/partly_protected.zip/open.txt")),
        TEST_MD5
    );

    let msgs = inbox.since(0);
    let warn = |subject: &str| {
        msgs.iter()
            .filter(|m| m.kind == MessageKind::Warning && m.subject == subject)
            .count()
    };
    assert_eq!(warn("Encrypted files in archive detected."), 3);
    assert_eq!(
        warn(
            "Possible ZIP bomb detected in archive: d4.zip, item: _bombs_DepthTriggerZipBomb.zip_d1.zip_d2.zip_d3.zip_d4.zip"
        ),
        1,
        "{msgs:#?}"
    );
    assert_eq!(
        warn(
            "Possible ZIP bomb detected in archive: RatioTriggerZipBomb.zip, item: _bombs_RatioTriggerZipBomb.zip"
        ),
        1
    );
    let ours: Vec<_> = msgs
        .iter()
        .filter(|m| m.source.as_deref() == Some(NAME))
        .collect();
    assert_eq!(ours.len(), 5, "{msgs:#?}");

    // Re-ingest posts nothing twice.
    f.run();
    let total: usize = f.files().iter().map(|x| f.artifacts(x).len()).sum();
    assert_eq!(total, 5);
}

#[test]
fn documents_yield_their_embedded_content() {
    let png = ak::png(4, 4);
    let jpg = ak::jpeg(8, 8);
    let docx = ak::ooxml(
        "word",
        &[("image1.png", &png), ("image2.jpeg", &jpg)],
        &[("oleObject1.bin", b"ole")],
    );
    let doc = ak::doc(&[ak::blip(ak::BLIP_PNG, &png, 1)]);
    let xls = ak::xls(&[ak::blip(ak::BLIP_JPEG, &jpg, 1)]);
    let ppt = ak::ppt(&[ak::blip(ak::BLIP_PNG, &png, 1)]);
    let pdf = ak::pdf(
        &[("notes.txt", b"test")],
        &[ak::PdfImage::Jpeg(jpg.clone())],
    );
    let mut b = NtfsBuilder::new(16);
    b.dir("docs");
    b.file("docs/report.docx", &docx);
    b.file("docs/old.doc", &doc);
    b.file("docs/sheet.xls", &xls);
    b.file("docs/slides.ppt", &ppt);
    b.file("docs/paper.pdf", &pdf);
    let f = fixture(&b);
    f.run();
    let all = f.by_path();
    let children = |parent: &str| -> Vec<String> {
        let mut v: Vec<String> = all
            .values()
            .filter(|x| x.parent_path == format!("{parent}/"))
            .map(|x| x.name.clone())
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        children("/docs/report.docx"),
        ["image1.png", "image2.jpeg", "oleObject1.bin"]
    );
    assert_eq!(
        f.content(&f.file("/docs/report.docx/image1.png")),
        png,
        "media bytes"
    );
    assert_eq!(children("/docs/old.doc"), ["image_0.png"]);
    assert_eq!(f.content(&f.file("/docs/old.doc/image_0.png")), png);
    assert_eq!(children("/docs/sheet.xls"), ["image_0.jpeg"]);
    assert_eq!(children("/docs/slides.ppt"), ["image_0.png"]);
    let pdf_children = children("/docs/paper.pdf");
    assert!(
        pdf_children.contains(&"notes.txt".to_owned()),
        "{pdf_children:?}"
    );
    let notes = f.file("/docs/paper.pdf/notes.txt");
    assert_eq!(md5_hex(&notes), TEST_MD5);
    assert!(
        notes
            .local_path
            .as_deref()
            .unwrap()
            .ends_with("/extract_0.txt")
    );
    // Documents are not extracted twice.
    let n = f.files().len();
    f.run();
    assert_eq!(f.files().len(), n);
}

#[test]
fn shift_jis_names_are_repaired_per_directory() {
    let names = ["日本語のファイル.txt", "写真フォルダ.txt", "報告書2020.txt"];
    let mut z = ZipBuilder::new();
    for n in names {
        let raw = encoding_rs::SHIFT_JIS.encode(n).0.into_owned();
        z.add(ZipEntry::new(raw, b"test".to_vec()).method(Method::Stored));
    }
    let mut b = NtfsBuilder::new(16);
    b.file("sjis.zip", &z.build());
    let f = fixture(&b);
    f.run();
    for n in names {
        let x = f.file(&format!("/sjis.zip/{n}"));
        assert_eq!(md5_hex(&x), TEST_MD5);
    }
}

#[test]
fn disk_images_are_opt_in() {
    let iso = ak::iso(&[("disc/readme.txt", b"test")], true, true);
    let mut b = NtfsBuilder::new(16);
    b.file("disc.iso", &iso);
    let f = fixture(&b);
    f.run();
    assert!(!f.by_path().keys().any(|p| p.starts_with("/disc.iso/")));
    let settings = JobSettings {
        threads: 2,
        modules: vec![ModuleSetting {
            id: "embedded_file_extractor".into(),
            enabled: true,
            settings: serde_json::json!({ "extract_disk_images": true }),
        }],
        ..JobSettings::default()
    };
    f.run_with(&settings);
    assert_eq!(md5_hex(&f.file("/disc.iso/disc/readme.txt")), TEST_MD5);
}

/// In Autopsy's pipeline File Type Identification runs first and the
/// extractor dispatches on the MIME types it sets.
#[test]
fn mime_types_from_file_type_identification_drive_extraction() {
    let f = fixture(&archives_volume());
    f.run_modules(
        &JobSettings {
            threads: 4,
            ..JobSettings::default()
        },
        &[
            Arc::new(HashLookupFactory),
            Arc::new(FileTypeFactory),
            Arc::new(EmbeddedFileExtractorFactory),
        ],
    );
    let zip = f.file("/ZIP/embedded/level3.zip");
    assert_eq!(zip.mime.as_deref(), Some("application/zip"));
    for p in [
        "/ZIP/embedded/level3.zip/level2.zip/level1.zip/level1.txt",
        "/ZIP/extension/zipFileWithTxtExtension.txt/file.txt",
        "/ZIP/big/big.zip/big folder/file1000.txt",
        "/ZIP/other/archive.tgz/archive.tar/t/in_tar.txt",
        "/ZIP/other/solid.7z/in7z.txt",
        "/ZIP/other/v5.rar/inrar.txt",
    ] {
        assert_eq!(md5_hex(&f.file(p)), TEST_MD5, "{p}");
    }
}
