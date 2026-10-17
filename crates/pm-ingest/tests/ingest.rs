#![allow(
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{FileKindCode, FileQuery, NewArtifact, Origin, Value};
use pm_ingest::*;

fn demo_image() -> &'static PathBuf {
    static IMG: OnceLock<PathBuf> = OnceLock::new();
    IMG.get_or_init(|| pm_testkit::demo::demo_disk_file().to_path_buf())
}

fn case() -> (tempfile::TempDir, Arc<Case>, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_image(
        &case,
        demo_image(),
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    (tmp, Arc::new(case), s.ds_id)
}

fn total_files(case: &Case, ds_id: i64) -> u64 {
    case.db()
        .count_files(&FileQuery {
            ds_id: Some(ds_id),
            ..Default::default()
        })
        .unwrap()
}

/// Shared observations of the test module.
#[derive(Default)]
struct Seen {
    paths: Mutex<Vec<String>>,
    /// `(file id, non-file-system origin)` in processing order.
    ids: Mutex<Vec<(i64, bool)>>,
    instances: AtomicUsize,
    shut_down: AtomicUsize,
    finished: AtomicUsize,
    ds_order: Mutex<Vec<String>>,
}

/// A configurable test module: records every file, tags `.exe` files with
/// an artifact and sets a MIME type on regular files.
struct Probe {
    id: &'static str,
    seen: Arc<Seen>,
    fail_start: bool,
    cancel_after: Option<usize>,
    fail_on: Option<&'static str>,
    panic_on: Option<&'static str>,
    stages: Vec<u32>,
    default_on: bool,
}

impl Probe {
    fn new(id: &'static str, seen: &Arc<Seen>) -> Self {
        Self {
            id,
            seen: Arc::clone(seen),
            fail_start: false,
            cancel_after: None,
            fail_on: None,
            panic_on: None,
            stages: Vec::new(),
            default_on: true,
        }
    }
}

struct ProbeJob {
    name: &'static str,
    seen: Arc<Seen>,
    cancel_after: Option<usize>,
    fail_on: Option<&'static str>,
    panic_on: Option<&'static str>,
    stages: Mutex<Vec<u32>>,
}

struct ProbeFile {
    job: Arc<ProbeJob>,
}

struct ProbeDs {
    name: String,
    seen: Arc<Seen>,
}

impl ModuleFactory for Probe {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: self.id,
            name: self.id,
            description: "test probe",
            version: "1.0",
            enabled_by_default: self.default_on,
        }
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        if self.fail_start {
            return Err(ModuleError::new("no hash sets configured"));
        }
        assert!(settings.is_null() || settings["custom"] == 1);
        Ok(Arc::new(JobRef(Arc::new(ProbeJob {
            name: self.id,
            seen: Arc::clone(&self.seen),
            cancel_after: self.cancel_after,
            fail_on: self.fail_on,
            panic_on: self.panic_on,
            stages: Mutex::new(self.stages.clone()),
        }))))
    }
}

struct JobRef(Arc<ProbeJob>);

impl ModuleJob for JobRef {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        self.0.seen.instances.fetch_add(1, Ordering::Relaxed);
        Some(Box::new(ProbeFile {
            job: Arc::clone(&self.0),
        }))
    }

    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        let stage = self.0.stages.lock().pop()?;
        Some((
            stage,
            Box::new(ProbeDs {
                name: format!("{}@{stage}", self.0.name),
                seen: Arc::clone(&self.0.seen),
            }),
        ))
    }

    fn finish(&self, ctx: &JobContext) {
        self.0.seen.finished.fetch_add(1, Ordering::Relaxed);
        ctx.post_message(IngestMessage::new(
            MessageKind::Info,
            self.0.name,
            "Probe summary",
        ));
    }
}

impl FileModule for ProbeFile {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let path = file.rec.path();
        self.job
            .seen
            .ids
            .lock()
            .push((file.rec.id, file.rec.origin != Origin::FileSystem));
        let n = {
            let mut p = self.job.seen.paths.lock();
            p.push(path.clone());
            p.len()
        };
        if self.job.cancel_after.is_some_and(|c| n >= c) {
            ctx.cancel_job();
        }
        if self.job.fail_on.is_some_and(|f| path.ends_with(f)) {
            return Err(ModuleError::new("cannot parse"));
        }
        assert!(
            !self.job.panic_on.is_some_and(|f| path.ends_with(f)),
            "bug in module"
        );
        if file.is_file() && !file.is_unallocated_space() {
            file.set_mime("application/x-probe");
            assert!(file.head().len() as u64 <= file.rec.size.max(HEAD_LEN as u64));
        }
        if file.rec.extension == "exe" {
            let set = attribute_type_id("TSK_SET_NAME").unwrap();
            ctx.post_artifact(
                NewArtifact {
                    type_id: artifact_type_id("TSK_INTERESTING_FILE_HIT").unwrap(),
                    ds_id: ctx.ds_id,
                    file_id: Some(file.rec.id),
                    source_artifact_id: None,
                    score: None,
                    conclusion: None,
                    configuration: None,
                    justification: None,
                    created_at: ctx.now(),
                    attributes: vec![(set, Value::Text("Executables".into()), "probe".into())],
                },
                &[set],
            );
        }
        Ok(())
    }

    fn shut_down(&mut self, _ctx: &JobContext) {
        self.job.seen.shut_down.fetch_add(1, Ordering::Relaxed);
    }
}

impl DataSourceModule for ProbeDs {
    fn process(&mut self, _ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        progress.set_total(2);
        progress.message("working");
        progress.set_done(2);
        self.seen.ds_order.lock().push(self.name.clone());
        Ok(())
    }
}

fn run(
    case: &Arc<Case>,
    ds_id: i64,
    settings: &JobSettings,
    factories: Vec<Arc<dyn ModuleFactory>>,
) -> (
    Result<JobSummary, IngestError>,
    Arc<IngestProgress>,
    Arc<Inbox>,
) {
    let progress = Arc::new(IngestProgress::default());
    let inbox = Arc::new(Inbox::default());
    let r = run_job(case, ds_id, settings, &factories, &progress, &inbox);
    (r, progress, inbox)
}

#[test]
fn every_file_is_processed_once_and_results_persist() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let settings = JobSettings {
        threads: 4,
        ..JobSettings::default()
    };
    let (r, progress, inbox) = run(
        &case,
        ds_id,
        &settings,
        vec![Arc::new(Probe::new("probe", &seen))],
    );
    let s = r.unwrap();
    assert!(!s.cancelled);
    assert!(s.errors.is_empty(), "{:?}", s.errors);

    let total = total_files(&case, ds_id);
    let paths = seen.paths.lock().clone();
    // System streams like `$Secure:$SDS` are filtered; everything else once.
    let mut ids: Vec<i64> = seen.ids.lock().iter().map(|x| x.0).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), paths.len(), "a file was processed twice");
    let filtered = total - paths.len() as u64;
    assert!(filtered < 10, "{filtered} files filtered");
    assert_eq!(s.files, total);

    assert_eq!(seen.instances.load(Ordering::Relaxed), 4);
    assert_eq!(seen.shut_down.load(Ordering::Relaxed), 4);
    assert_eq!(seen.finished.load(Ordering::Relaxed), 1);
    assert_eq!(inbox.since(0)[0].subject, "Probe summary");

    // MIME types persisted for regular, non-unallocated files.
    let files = case
        .db()
        .query_files(
            &FileQuery {
                ds_id: Some(ds_id),
                ..Default::default()
            },
            0,
            100_000,
        )
        .unwrap();
    for f in &files {
        let want = f.kind == FileKindCode::Regular
            && f.origin != Origin::UnallocBlocks
            && paths.contains(&f.path());
        assert_eq!(f.mime.is_some(), want, "{}", f.path());
    }
    let exes = files.iter().filter(|f| f.extension == "exe").count() as u64;
    assert!(exes > 0);
    assert_eq!(s.artifacts, exes);

    let snap = progress.snapshot();
    assert_eq!(snap.phase, Phase::Done);
    assert_eq!(snap.files_done, snap.files_total);
    assert_eq!(snap.fraction(), Some(1.0));
    assert_eq!(snap.activity_line(), "Analysis complete");
    assert!(snap.module_times.iter().any(|(m, _)| m == "probe"));
    assert!(snap.workers.iter().all(|w| w.module.is_none()));

    // Re-running does not duplicate artifacts.
    let (r2, ..) = run(
        &case,
        ds_id,
        &settings,
        vec![Arc::new(Probe::new("probe", &Arc::default()))],
    );
    assert_eq!(r2.unwrap().artifacts, 0);
    let jobs: Vec<(i64, i64)> = case
        .db()
        .read(|c| {
            let mut st =
                c.prepare("SELECT status, ended_at IS NOT NULL FROM ingest_jobs ORDER BY id")?;
            Ok(st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    assert_eq!(jobs, vec![(2, 1), (2, 1)]);
}

fn priority(path: &str) -> u8 {
    let top = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap()
        .to_lowercase();
    if top.starts_with("$unalloc") || top.starts_with("pagefile") || top.starts_with("hiberfil") {
        3
    } else if ["users", "documents and settings", "home", "programdata"]
        .iter()
        .any(|p| top.starts_with(p))
    {
        0
    } else if top.starts_with("$orphanfiles") || top.starts_with("windows") {
        2
    } else {
        1
    }
}

#[test]
fn files_arrive_in_priority_order() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let settings = JobSettings {
        threads: 1,
        ..JobSettings::default()
    };
    run(
        &case,
        ds_id,
        &settings,
        vec![Arc::new(Probe::new("p", &seen))],
    )
    .0
    .unwrap();
    let ids = seen.ids.lock();
    let classes: Vec<u8> = seen
        .paths
        .lock()
        .iter()
        .zip(ids.iter())
        .map(|(p, (_, non_fs))| if *non_fs { 3 } else { priority(p) })
        .collect();
    assert!(classes.contains(&0) && classes.contains(&3));
    assert!(classes.is_sorted(), "{classes:?}");
}

#[test]
fn modules_run_in_pipeline_order_and_settings_select_them() {
    let (_t, case, ds_id) = case();
    let a = Arc::new(Seen::default());
    let b = Arc::new(Seen::default());
    let c = Arc::new(Seen::default());
    let mut off_by_default = Probe::new("c", &c);
    off_by_default.default_on = false;
    let settings = JobSettings {
        modules: vec![
            ModuleSetting {
                id: "b".into(),
                enabled: false,
                settings: serde_json::Value::Null,
            },
            ModuleSetting {
                id: "a".into(),
                enabled: true,
                settings: serde_json::json!({"custom": 1}),
            },
        ],
        threads: 2,
        ..JobSettings::default()
    };
    run(
        &case,
        ds_id,
        &settings,
        vec![
            Arc::new(Probe::new("a", &a)),
            Arc::new(Probe::new("b", &b)),
            Arc::new(off_by_default),
        ],
    )
    .0
    .unwrap();
    assert!(!a.paths.lock().is_empty());
    assert!(b.paths.lock().is_empty());
    assert!(c.paths.lock().is_empty());

    let none = JobSettings {
        modules: vec![ModuleSetting {
            id: "a".into(),
            enabled: false,
            settings: serde_json::Value::Null,
        }],
        ..JobSettings::default()
    };
    let (r, ..) = run(&case, ds_id, &none, vec![Arc::new(Probe::new("a", &a))]);
    assert!(matches!(r, Err(IngestError::NoModules)));
}

#[test]
fn startup_failure_aborts_before_any_file() {
    let (_t, case, ds_id) = case();
    let ok = Arc::new(Seen::default());
    let mut bad = Probe::new("Hash Lookup", &Arc::default());
    bad.fail_start = true;
    let (r, progress, _) = run(
        &case,
        ds_id,
        &JobSettings::default(),
        vec![Arc::new(Probe::new("ok", &ok)), Arc::new(bad)],
    );
    let err = r.unwrap_err();
    assert!(
        err.to_string()
            .contains("Hash Lookup: no hash sets configured"),
        "{err}"
    );
    assert!(ok.paths.lock().is_empty());
    assert_eq!(progress.phase(), Phase::Cancelled);
    let status: i64 = case
        .db()
        .read(|c| Ok(c.query_row("SELECT status FROM ingest_jobs", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(status, 1);
}

#[test]
fn module_errors_and_panics_do_not_stop_the_job() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let mut p = Probe::new("p", &seen);
    p.fail_on = Some("/setup.exe");
    p.panic_on = Some("/Users/alice/Downloads");
    let after = Arc::new(Seen::default());
    let (r, ..) = run(
        &case,
        ds_id,
        &JobSettings::default(),
        vec![Arc::new(p), Arc::new(Probe::new("after", &after))],
    );
    let s = r.unwrap();
    assert!(
        s.errors
            .iter()
            .any(|e| e.starts_with("p: ") && e.ends_with("cannot parse")),
        "{:?}",
        s.errors
    );
    assert!(
        s.errors.iter().any(|e| e.contains("panicked")),
        "{:?}",
        s.errors
    );
    // The next module still saw those files.
    let later = after.paths.lock();
    assert!(later.iter().any(|x| x.ends_with("/setup.exe")));
    assert!(later.iter().any(|x| x.ends_with("/Users/alice/Downloads")));
}

#[test]
fn cancellation_stops_promptly() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let mut p = Probe::new("p", &seen);
    p.cancel_after = Some(5);
    let progress = Arc::new(IngestProgress::default());
    let inbox = Arc::new(Inbox::default());
    let settings = JobSettings {
        threads: 1,
        ..JobSettings::default()
    };
    let factories: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(p)];
    let s = run_job(&case, ds_id, &settings, &factories, &progress, &inbox).unwrap();
    assert!(s.cancelled);
    assert_eq!(seen.paths.lock().len(), 5);
    assert_eq!(progress.phase(), Phase::Cancelled);
    assert_eq!(seen.shut_down.load(Ordering::Relaxed), 1);
}

#[test]
fn unallocated_space_can_be_filtered() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let settings = JobSettings {
        filter: FileFilter {
            ignore_unallocated: true,
            ignore_known: false,
        },
        ..JobSettings::default()
    };
    run(
        &case,
        ds_id,
        &settings,
        vec![Arc::new(Probe::new("p", &seen))],
    )
    .0
    .unwrap();
    let paths = seen.paths.lock();
    assert!(!paths.iter().any(|p| p.contains("/$Unalloc/Unalloc_")));
    // The virtual directory itself is still visited (directories are never
    // filtered).
    assert!(paths.iter().any(|p| p == "/$Unalloc"));
}

#[test]
fn data_source_stages_run_in_order() {
    let (_t, case, ds_id) = case();
    let seen = Arc::new(Seen::default());
    let mut late = Probe::new("integrity", &seen);
    late.stages = vec![2];
    let mut early = Probe::new("recent", &seen);
    early.stages = vec![1];
    let (r, ..) = run(
        &case,
        ds_id,
        &JobSettings::default(),
        vec![Arc::new(late), Arc::new(early)],
    );
    r.unwrap();
    assert_eq!(*seen.ds_order.lock(), ["recent@1", "integrity@2"]);
}

#[test]
fn inbox_caps_errors() {
    let inbox = Inbox::default();
    for i in 0..MAX_ERROR_MESSAGES + 50 {
        inbox.post(IngestMessage::new(MessageKind::Error, "m", format!("e{i}")));
    }
    inbox.post(
        IngestMessage::new(MessageKind::Data, "m", "hit")
            .key("k")
            .file(7),
    );
    let all = inbox.since(0);
    assert_eq!(all.len(), MAX_ERROR_MESSAGES + 2);
    assert_eq!(
        all[MAX_ERROR_MESSAGES].subject,
        "Error message limit reached"
    );
    assert_eq!(all.last().unwrap().subject, "hit");
    assert_eq!(inbox.since(all[3].id).len(), all.len() - 4);
    inbox.clear();
    assert!(inbox.is_empty());
}

#[test]
fn filter_rules() {
    let (_t, case, ds_id) = case();
    let files = case
        .db()
        .query_files(
            &FileQuery {
                ds_id: Some(ds_id),
                ..Default::default()
            },
            0,
            100_000,
        )
        .unwrap();
    let f = FileFilter::default();
    let sys_streams: Vec<_> = files
        .iter()
        .filter(|x| !f.accepts(x))
        .map(pm_casedb::FileRecord::path)
        .collect();
    assert!(
        sys_streams
            .iter()
            .all(|p| p.starts_with("/$") && p.contains(':')),
        "{sys_streams:?}"
    );
    let mut known = files
        .iter()
        .find(|x| x.kind == FileKindCode::Regular)
        .unwrap()
        .clone();
    known.known = pm_casedb::Known::Known;
    assert!(f.accepts(&known));
    assert!(
        !FileFilter {
            ignore_unallocated: false,
            ignore_known: true
        }
        .accepts(&known)
    );
}
