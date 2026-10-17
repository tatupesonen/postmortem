//! A demo-disk case shared by the module tests.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    reason = "test helpers"
)]

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::{FileQuery, FileRecord};
use pm_core::ReadAt;
use pm_ingest::{Inbox, IngestProgress, JobSettings, JobSummary, ModuleFactory, run_job};

fn demo_image() -> &'static PathBuf {
    static IMG: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &IMG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("demo.dd");
        std::fs::write(&p, pm_testkit::demo::demo_disk()).unwrap();
        (dir, p)
    })
    .1
}

pub(crate) struct DemoCase {
    _dir: tempfile::TempDir,
    pub(crate) case: Arc<Case>,
    pub(crate) ds_id: i64,
}

impl DemoCase {
    pub(crate) fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
        let s = add_image(
            &case,
            demo_image(),
            &LoadOptions::default(),
            &LoadProgress::default(),
        )
        .unwrap();
        Self {
            _dir: dir,
            case: Arc::new(case),
            ds_id: s.ds_id,
        }
    }

    /// A case holding one data source built from raw image bytes.
    pub(crate) fn with_image(image: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("image.dd");
        std::fs::write(&img, image).unwrap();
        let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
        let s = add_image(
            &case,
            &img,
            &LoadOptions::default(),
            &LoadProgress::default(),
        )
        .unwrap();
        Self {
            _dir: dir,
            case: Arc::new(case),
            ds_id: s.ds_id,
        }
    }

    /// The file at `path` (`/dir/name`).
    pub(crate) fn file(&self, path: &str) -> FileRecord {
        self.files()
            .into_iter()
            .find(|f| f.path() == path)
            .unwrap_or_else(|| panic!("no file {path}"))
    }

    pub(crate) fn files(&self) -> Vec<FileRecord> {
        self.case
            .db()
            .query_files(
                &FileQuery {
                    ds_id: Some(self.ds_id),
                    ..Default::default()
                },
                0,
                1_000_000,
            )
            .unwrap()
    }

    pub(crate) fn run(
        &self,
        settings: &JobSettings,
        modules: Vec<Arc<dyn ModuleFactory>>,
    ) -> (JobSummary, Arc<Inbox>) {
        let progress = Arc::new(IngestProgress::default());
        let inbox = Arc::new(Inbox::default());
        let s = run_job(
            &self.case, self.ds_id, settings, &modules, &progress, &inbox,
        )
        .unwrap();
        (s, inbox)
    }
}

/// An artifact with its attributes as `name → display value`.
pub(crate) type ArtifactDump = (
    pm_casedb::ArtifactRecord,
    std::collections::BTreeMap<String, String>,
);

impl DemoCase {
    /// Every artifact of type `ty` in the data source.
    pub(crate) fn artifacts(&self, ty: &str) -> Vec<ArtifactDump> {
        let db = self.case.db();
        let Some(type_id) = pm_casedb::catalog::artifact_type_id(ty) else {
            panic!("unknown artifact type {ty}");
        };
        db.artifacts_of_type(type_id, Some(self.ds_id), 0, 1_000_000)
            .unwrap()
            .into_iter()
            .map(|a| {
                let attrs = db
                    .attributes(a.id)
                    .unwrap()
                    .into_iter()
                    .map(|x| (x.name, x.value.display()))
                    .collect();
                (a, attrs)
            })
            .collect()
    }

    /// Runs one module with the given settings.
    pub(crate) fn run_one(
        &self,
        factory: Arc<dyn ModuleFactory>,
        settings: serde_json::Value,
    ) -> (JobSummary, Arc<Inbox>) {
        let id = factory.info().id.to_owned();
        let job = JobSettings {
            modules: vec![pm_ingest::ModuleSetting {
                id,
                enabled: true,
                settings,
            }],
            ..JobSettings::default()
        };
        self.run(&job, vec![factory])
    }
}

pub(crate) fn read_all(src: &dyn ReadAt) -> Vec<u8> {
    src.read_vec_at(0, usize::try_from(src.size()).unwrap())
        .unwrap()
}
