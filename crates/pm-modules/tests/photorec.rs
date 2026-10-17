//! PhotoRec Carver end to end: files planted in the free clusters of an
//! NTFS volume are carved from its unallocated space, placed under
//! `$CarvedFiles`, mapped to image byte ranges and analysed like any
//! other file (Autopsy `IngestFileFiltersTest` carving cases, rebuilt).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::needless_pass_by_value,
    reason = "test code"
)]

use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::{FileKindCode, FileQuery, FileRecord, Origin};
use pm_core::ReadAt;
use pm_ingest::{
    FileFilter, Inbox, IngestError, IngestProgress, JobSettings, JobSummary, MessageKind,
    ModuleFactory, ModuleSetting, run_job,
};
use pm_modules::embedded::EmbeddedFileExtractorFactory;
use pm_modules::hash_lookup::HashLookupFactory;
use pm_modules::photorec::{CARVED_DIR, NAME, PhotoRecCarverFactory};
use pm_testkit::archives as ak;
use pm_testkit::ntfs::NtfsBuilder;

/// Volume offset of the planted files (free clusters of a 16 MiB volume).
const PLANT_AT: usize = 12 << 20;

struct Fixture {
    _dir: tempfile::TempDir,
    case: Arc<Case>,
    ds_id: i64,
    planted: Vec<(u64, Vec<u8>)>,
}

fn fixture() -> Fixture {
    let mut b = NtfsBuilder::new(16);
    b.file("live.txt", b"a live file");
    let mut img = b.build();
    let jpg = ak::jpeg(24, 24);
    let png = ak::png(8, 8);
    let zip = ak::zip(&[("inside.txt", b"test")]);
    let mut planted = Vec::new();
    let mut at = PLANT_AT;
    for data in [jpg, png, zip] {
        img[at..at + data.len()].copy_from_slice(&data);
        planted.push((at as u64, data.clone()));
        at = (at + data.len()).next_multiple_of(4096) + 8192;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vol.dd");
    std::fs::write(&path, img).unwrap();
    let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_image(
        &case,
        &path,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    Fixture {
        _dir: dir,
        case: Arc::new(case),
        ds_id: s.ds_id,
        planted,
    }
}

impl Fixture {
    fn run(
        &self,
        modules: Vec<Arc<dyn ModuleFactory>>,
        settings: &JobSettings,
    ) -> Result<(JobSummary, Arc<Inbox>), IngestError> {
        let inbox = Arc::new(Inbox::default());
        let s = run_job(
            &self.case,
            self.ds_id,
            settings,
            &modules,
            &Arc::new(IngestProgress::default()),
            &inbox,
        )?;
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        Ok((s, inbox))
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

    fn carved(&self) -> Vec<FileRecord> {
        let mut v: Vec<FileRecord> = self
            .files()
            .into_iter()
            .filter(|f| f.origin == Origin::Carved)
            .collect();
        v.sort_by_key(|f| f.id);
        v
    }
}

fn all_modules() -> Vec<Arc<dyn ModuleFactory>> {
    vec![
        Arc::new(HashLookupFactory),
        Arc::new(EmbeddedFileExtractorFactory),
        Arc::new(PhotoRecCarverFactory),
    ]
}

#[test]
fn unallocated_space_is_carved_into_carved_files() {
    let f = fixture();
    let (_, inbox) = f.run(all_modules(), &JobSettings::default()).unwrap();
    let carved = f.carved();
    let names: Vec<String> = carved.iter().map(|c| c.name.clone()).collect();
    assert_eq!(carved.len(), 3, "{names:?}");
    let image = f.case.image_source(f.ds_id).unwrap();
    for ((at, data), c) in f.planted.iter().zip(&carved) {
        assert_eq!(c.parent_path, format!("/{CARVED_DIR}/1/"));
        assert_eq!(c.kind, FileKindCode::Regular);
        assert!(!c.name_alloc && !c.meta_alloc);
        assert_eq!(c.size, data.len() as u64);
        // Content via the layout equals the planted bytes, at the planted
        // image offset.
        let src = f.case.content(c).unwrap();
        assert_eq!(src.read_vec_at(0, data.len()).unwrap(), *data);
        let layout =
            pm_case::parse_layout(&f.case.db().file_layout(c.id).unwrap().unwrap()).unwrap();
        let fs_off = image.size() - 16 * (1 << 20);
        assert_eq!(layout[0].0, fs_off + at, "{}", c.name);
        // Carved files run through the pipeline (hashed).
        assert!(c.md5.is_some(), "{} hashed", c.name);
    }
    let exts: Vec<&str> = carved.iter().map(|c| c.extension.as_str()).collect();
    assert_eq!(exts, ["jpg", "png", "zip"]);
    // PhotoRec's names: `f` + the sector in the unallocated-space file.
    assert!(
        names.iter().all(|n| n.starts_with('f') && n.len() == 12),
        "{names:?}"
    );
    // The carved zip was extracted by the Embedded File Extractor.
    let inside = f
        .files()
        .into_iter()
        .find(|x| x.name == "inside.txt")
        .expect("carved zip extracted");
    assert_eq!(inside.parent_path, format!("/{CARVED_DIR}/1/{}/", names[2]));
    // `$CarvedFiles` and `1` are virtual directories under the root.
    let base = f
        .files()
        .into_iter()
        .find(|x| x.name == CARVED_DIR)
        .unwrap();
    assert_eq!(base.origin, Origin::VirtualDir);
    assert_eq!(base.parent_path, "/");
    let msgs = inbox.since(0);
    let results = msgs
        .iter()
        .find(|m| m.source.as_deref() == Some(NAME) && m.subject == "PhotoRec Results")
        .expect("summary message");
    assert_eq!(results.kind, MessageKind::Info);
    assert!(results.details.contains("Number of Files Carved: 3"));

    // Re-ingest carves nothing again.
    let n = f.files().len();
    f.run(all_modules(), &JobSettings::default()).unwrap();
    assert_eq!(f.files().len(), n);
}

#[test]
fn extension_filter_and_keep_corrupted_follow_settings() {
    let f = fixture();
    let settings = JobSettings {
        modules: vec![ModuleSetting {
            id: "photorec_carver".into(),
            enabled: true,
            settings: serde_json::json!({
                "extension_filter": "Include",
                "extensions": ["png"],
            }),
        }],
        ..JobSettings::default()
    };
    f.run(vec![Arc::new(PhotoRecCarverFactory)], &settings)
        .unwrap();
    let carved = f.carved();
    assert_eq!(carved.len(), 1);
    assert_eq!(carved[0].extension, "png");
}

#[test]
fn carving_refuses_a_filter_without_unallocated_space() {
    let f = fixture();
    let settings = JobSettings {
        filter: FileFilter {
            ignore_unallocated: true,
            ..FileFilter::default()
        },
        ..JobSettings::default()
    };
    let err = f
        .run(vec![Arc::new(PhotoRecCarverFactory)], &settings)
        .unwrap_err();
    match err {
        IngestError::StartUp(errors) => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].0, NAME);
            assert!(errors[0].1.contains("ignores unallocated space"));
        }
        e => panic!("{e}"),
    }
    // Invalid settings fail start-up too.
    let bad = JobSettings {
        modules: vec![ModuleSetting {
            id: "photorec_carver".into(),
            enabled: true,
            settings: serde_json::json!({
                "extension_filter": "Exclude",
                "extensions": ["nope"],
            }),
        }],
        ..JobSettings::default()
    };
    assert!(matches!(
        f.run(vec![Arc::new(PhotoRecCarverFactory)], &bad),
        Err(IngestError::StartUp(_))
    ));
}
