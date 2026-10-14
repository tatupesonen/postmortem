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

pub(crate) fn read_all(src: &dyn ReadAt) -> Vec<u8> {
    src.read_vec_at(0, usize::try_from(src.size()).unwrap())
        .unwrap()
}
