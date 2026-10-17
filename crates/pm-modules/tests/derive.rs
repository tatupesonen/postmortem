//! `pm_modules::derive::add_children`: byte streams become derived files
//! of a file (the API the e-mail parser uses for attachments).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::Arc;

use parking_lot::Mutex;
use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::{FileQuery, FileRecord, Origin};
use pm_core::ReadAt;
use pm_ingest::{
    FileModule, Inbox, IngestFile, IngestProgress, JobContext, JobSettings, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob, run_job,
};
use pm_modules::derive::{Stream, Times, add_children};
use pm_modules::hash_lookup::HashLookupFactory;
use pm_testkit::ntfs::NtfsBuilder;

/// Turns `mail.eml` into three attachments, two with the same name.
#[derive(Default)]
struct Attach(Mutex<Vec<Option<i64>>>);

struct AttachJob(Arc<Attach>);
struct AttachFile(Arc<Attach>);
struct AttachFactory(Arc<Attach>);

impl ModuleFactory for AttachFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "attach",
            name: "Attach",
            description: "",
            version: "1",
            enabled_by_default: true,
        }
    }
    fn start(
        &self,
        _: &JobContext,
        _: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        Ok(Arc::new(AttachJob(Arc::clone(&self.0))))
    }
}

impl ModuleJob for AttachJob {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(AttachFile(Arc::clone(&self.0))))
    }
}

impl FileModule for AttachFile {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        if file.rec.name != "mail.eml" {
            return Ok(());
        }
        let (mut a, mut b, mut c) = (&b"test"[..], &b"second"[..], &b"third"[..]);
        let ids = add_children(
            ctx,
            &file.rec,
            "EmailParser",
            vec![
                Stream {
                    name: "image.png".into(),
                    times: Times {
                        modified: Some(1_600_000_000),
                        ..Times::default()
                    },
                    data: &mut a,
                },
                Stream {
                    name: "image.png".into(),
                    times: Times::default(),
                    data: &mut b,
                },
                Stream {
                    name: String::new(),
                    times: Times::default(),
                    data: &mut c,
                },
            ],
        )?;
        self.0.0.lock().extend(ids);
        Ok(())
    }
}

#[test]
fn streams_become_analysed_derived_children_once() {
    let mut b = NtfsBuilder::new(16);
    b.file("mail.eml", b"From: a@b");
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("vol.dd");
    std::fs::write(&img, b.build()).unwrap();
    let case = Arc::new(Case::create(dir.path().join("case"), CaseDetails::default()).unwrap());
    let ds = add_image(
        &case,
        &img,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap()
    .ds_id;
    let run = |attach: &Arc<Attach>| {
        let modules: Vec<Arc<dyn ModuleFactory>> = vec![
            Arc::new(AttachFactory(Arc::clone(attach))),
            Arc::new(HashLookupFactory),
        ];
        let s = run_job(
            &case,
            ds,
            &JobSettings::default(),
            &modules,
            &Arc::new(IngestProgress::default()),
            &Arc::new(Inbox::default()),
        )
        .unwrap();
        assert!(s.errors.is_empty(), "{:?}", s.errors);
    };
    let first = Arc::new(Attach::default());
    run(&first);
    let ids = first.0.lock().clone();
    assert_eq!(ids.len(), 3);
    assert!(ids[0].is_some() && ids[1].is_some() && ids[2].is_none());
    let rec = |id: i64| -> FileRecord { case.db().file(id).unwrap().unwrap() };
    let (a, b) = (rec(ids[0].unwrap()), rec(ids[1].unwrap()));
    assert_eq!(
        (a.name.as_str(), b.name.as_str()),
        ("image.png", "image.png")
    );
    assert_eq!(a.parent_path, "/mail.eml/");
    assert_eq!(a.origin, Origin::Derived);
    assert_eq!(a.times[0], Some((1_600_000_000, 0)));
    assert!(
        a.local_path
            .as_deref()
            .unwrap()
            .starts_with("ModuleOutput/EmailParser/")
    );
    let content = |f: &FileRecord| {
        let src = case.content(f).unwrap();
        src.read_vec_at(0, usize::try_from(src.size()).unwrap())
            .unwrap()
    };
    assert_eq!(content(&a), b"test");
    assert_eq!(content(&b), b"second");
    // Scheduled and analysed: MD5("test").
    assert_eq!(
        pm_core::decode::to_hex(a.md5.as_deref().unwrap()),
        "098f6bcd4621d373cade4e832627b4f6"
    );

    // A second run adds nothing.
    let count = || {
        case.db()
            .count_files(&FileQuery {
                ds_id: Some(ds),
                ..FileQuery::default()
            })
            .unwrap()
    };
    let n = count();
    let second = Arc::new(Attach::default());
    run(&second);
    assert!(second.0.lock().iter().all(Option::is_none));
    assert_eq!(count(), n);
}
