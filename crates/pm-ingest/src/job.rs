//! Running an ingest job over one data source.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, bounded};
use pm_case::Case;
use pm_casedb::{
    CaseDb, DbError, FileKindCode, FileQuery, FileRecord, IngestPriority, Known, Origin,
};
use serde::{Deserialize, Serialize};

use crate::context::WriteOp;
use crate::progress::{DsStatus, Phase};
use crate::{
    DataSourceModule, DsProgress, FileModule, Inbox, IngestFile, IngestProgress, JobContext,
    ModuleFactory, ModuleInfo, ModuleJob,
};

/// Files fetched from the database per query.
const PAGE: usize = 2_000;
/// Database operations applied per transaction by the writer.
const WRITE_BATCH: usize = 5_000;

/// Which files are analysed (Autopsy's standard file ingest filters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FileFilter {
    /// Skip unallocated-space and slack files ("All Files and Directories
    /// (Not Unallocated Space)").
    pub ignore_unallocated: bool,
    /// Skip files already marked known (matters on re-ingest).
    pub ignore_known: bool,
}

impl FileFilter {
    /// Whether `f` is scheduled (`IngestTasksScheduler.shouldEnqueueFileTask`).
    pub fn accepts(&self, f: &FileRecord) -> bool {
        // Directories are never filtered.
        if f.kind == FileKindCode::Directory {
            return true;
        }
        // NTFS system-file streams such as `$Secure:$SDS`.
        if f.parent_path == "/"
            && f.inode.is_some_and(|i| i < 32)
            && f.name.starts_with('$')
            && f.name.contains(':')
        {
            return false;
        }
        if self.ignore_unallocated && matches!(f.origin, Origin::UnallocBlocks | Origin::Slack) {
            return false;
        }
        !(self.ignore_known && f.known == Known::Known)
    }
}

/// Enablement and settings of one module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleSetting {
    pub id: String,
    pub enabled: bool,
    #[serde(default)]
    pub settings: serde_json::Value,
}

/// Settings of an ingest job.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JobSettings {
    /// Modules not listed use their defaults.
    pub modules: Vec<ModuleSetting>,
    pub filter: FileFilter,
    /// File worker threads (0 = available parallelism).
    pub threads: usize,
}

impl JobSettings {
    fn module(&self, id: &str) -> Option<&ModuleSetting> {
        self.modules.iter().find(|m| m.id == id)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("data source {0} not found")]
    NoDataSource(i64),
    #[error("no ingest modules are enabled")]
    NoModules,
    #[error("ingest modules failed to start: {}", .0.iter().map(|(m, e)| format!("{m}: {e}")).collect::<Vec<_>>().join("; "))]
    StartUp(Vec<(String, String)>),
}

/// Outcome of a finished job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSummary {
    pub job_id: i64,
    pub files: u64,
    pub artifacts: u64,
    /// Module errors (`module: message`).
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// Runs the enabled modules of `factories` (in pipeline order) over data
/// source `ds_id`. Blocks until the job finishes; run it on a background
/// thread and watch `progress`.
pub fn run_job(
    case: &Arc<Case>,
    ds_id: i64,
    settings: &JobSettings,
    factories: &[Arc<dyn ModuleFactory>],
    progress: &Arc<IngestProgress>,
    inbox: &Arc<Inbox>,
) -> Result<JobSummary, IngestError> {
    let db = Arc::clone(case.db());
    let ds = db
        .data_source(ds_id)?
        .ok_or(IngestError::NoDataSource(ds_id))?;
    progress.set_data_source(ds.name.clone());
    progress.set_phase(Phase::Starting);

    let enabled: Vec<(ModuleInfo, &Arc<dyn ModuleFactory>, serde_json::Value)> = factories
        .iter()
        .filter_map(|f| {
            let info = f.info();
            match settings.module(info.id) {
                Some(m) if !m.enabled => None,
                Some(m) if !m.settings.is_null() => Some((info, f, m.settings.clone())),
                Some(_) => Some((info, f, f.default_settings())),
                None if info.enabled_by_default => Some((info, f, f.default_settings())),
                None => None,
            }
        })
        .collect();
    if enabled.is_empty() {
        progress.set_phase(Phase::Done);
        return Err(IngestError::NoModules);
    }
    let modules: Vec<(&str, &str)> = enabled.iter().map(|(i, ..)| (i.id, i.version)).collect();
    let settings_json = serde_json::to_string(settings).unwrap_or_default();
    let job_id = db.start_ingest_job(ds_id, &settings_json, now(), &modules)?;

    let (sink, ops) = bounded::<WriteOp>(WRITE_BATCH * 2);
    let ctx = JobContext {
        ds_id,
        job_id,
        case: Arc::clone(case),
        process_unallocated: !settings.filter.ignore_unallocated,
        sink,
        inbox: Arc::clone(inbox),
        progress: Arc::clone(progress),
    };

    std::thread::scope(|s| -> Result<JobSummary, IngestError> {
        let writer = {
            let db = Arc::clone(&db);
            let progress = Arc::clone(progress);
            s.spawn(move || run_writer(&db, &ops, &progress))
        };

        let mut jobs: Vec<(ModuleInfo, Arc<dyn ModuleJob>)> = Vec::new();
        let mut failures = Vec::new();
        for (info, factory, module_settings) in &enabled {
            match factory.start(&ctx, module_settings) {
                Ok(j) => jobs.push((info.clone(), j)),
                Err(e) => failures.push((info.name.to_owned(), e.0)),
            }
        }
        if !failures.is_empty() {
            stop_writer(&ctx, writer);
            db.finish_ingest_job(job_id, true, now())?;
            progress.set_phase(Phase::Cancelled);
            return Err(IngestError::StartUp(failures));
        }

        // Data-source modules grouped by stage.
        let mut stages: Vec<(u32, &'static str, Box<dyn DataSourceModule>)> = jobs
            .iter()
            .filter_map(|(info, j)| {
                j.data_source_module()
                    .map(|(st, m)| (st.max(1), info.name, m))
            })
            .collect();
        stages.sort_by_key(|(st, ..)| *st);
        let later = stages.split_off(stages.partition_point(|(st, ..)| *st <= 1));

        progress.set_phase(Phase::Files);
        let total = db.count_files(&FileQuery {
            ds_id: Some(ds_id),
            ..FileQuery::default()
        })?;
        progress.files_total.store(total, Ordering::Relaxed);
        std::thread::scope(|s2| {
            if !stages.is_empty() {
                let ctx = &ctx;
                s2.spawn(move || run_ds_modules(ctx, stages));
            }
            run_files(s2, &ctx, &jobs, settings, &db, progress);
        });

        // Later stages, one after another (sorted by stage above).
        let mut later = later.into_iter().peekable();
        while let Some(&(st, ..)) = later.peek() {
            let mut group = Vec::new();
            while let Some(m) = later.next_if(|(s, ..)| *s == st) {
                group.push(m);
            }
            if progress.is_cancelled() {
                break;
            }
            progress.set_phase(Phase::DataSource(st));
            run_ds_modules(&ctx, group);
        }

        progress.set_phase(Phase::Finishing);
        for (_, j) in &jobs {
            j.finish(&ctx);
        }
        drop(jobs);
        stop_writer(&ctx, writer);

        let cancelled = progress.is_cancelled();
        db.finish_ingest_job(job_id, cancelled, now())?;
        progress.set_phase(if cancelled {
            Phase::Cancelled
        } else {
            Phase::Done
        });
        Ok(JobSummary {
            job_id,
            files: progress.files_done.load(Ordering::Relaxed),
            artifacts: progress.artifacts.load(Ordering::Relaxed),
            errors: progress.errors(),
            cancelled,
        })
    })
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn stop_writer(ctx: &JobContext, writer: std::thread::ScopedJoinHandle<'_, Result<(), DbError>>) {
    let _ = ctx.sink.send(WriteOp::Stop);
    match writer.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => ctx
            .progress
            .record_error("Ingest", &format!("saving results: {e}")),
        Err(_) => ctx
            .progress
            .record_error("Ingest", "result writer panicked"),
    }
}

/// Streams the data source's files in priority order to the workers.
fn run_files<'s>(
    s: &'s std::thread::Scope<'s, '_>,
    ctx: &'s JobContext,
    jobs: &[(ModuleInfo, Arc<dyn ModuleJob>)],
    settings: &JobSettings,
    db: &CaseDb,
    progress: &Arc<IngestProgress>,
) {
    let threads = if settings.threads == 0 {
        std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
    } else {
        settings.threads
    };
    let pipelines: Vec<Vec<(&'static str, Box<dyn FileModule>)>> = (0..threads)
        .map(|_| {
            jobs.iter()
                .filter_map(|(info, j)| j.file_module().map(|m| (info.name, m)))
                .collect::<Vec<_>>()
        })
        .collect();
    if pipelines.first().is_none_or(Vec::is_empty) {
        return;
    }
    progress.set_workers(threads);
    let (tx, rx) = bounded::<FileRecord>(threads * 64);
    let workers: Vec<_> = pipelines
        .into_iter()
        .enumerate()
        .map(|(i, pipeline)| {
            let rx = rx.clone();
            s.spawn(move || run_worker(i, &rx, pipeline, ctx))
        })
        .collect();
    drop(rx);

    'classes: for class in IngestPriority::ALL {
        let mut after = 0;
        loop {
            if progress.is_cancelled() {
                break 'classes;
            }
            let page = match db.ingest_files(ctx.ds_id, class, after, PAGE) {
                Ok(p) => p,
                Err(e) => {
                    progress.record_error("Ingest", &format!("listing files: {e}"));
                    break 'classes;
                }
            };
            let Some(last) = page.last() else { break };
            after = last.id;
            for f in page {
                if !settings.filter.accepts(&f) {
                    progress.files_done.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if tx.send(f).is_err() {
                    break 'classes;
                }
            }
        }
    }
    drop(tx);
    for w in workers {
        if w.join().is_err() {
            progress.record_error("Ingest", "file worker panicked");
        }
    }
}

fn run_worker(
    idx: usize,
    rx: &Receiver<FileRecord>,
    mut pipeline: Vec<(&'static str, Box<dyn FileModule>)>,
    ctx: &JobContext,
) {
    let progress = &ctx.progress;
    let mut times: HashMap<&'static str, Duration> = HashMap::new();
    for rec in rx {
        if progress.is_cancelled() {
            break;
        }
        let path = rec.path();
        let id = rec.id;
        let mut file = IngestFile::new(rec, Arc::clone(&ctx.case));
        for (name, module) in &mut pipeline {
            if progress.is_cancelled() {
                break;
            }
            progress.set_activity(idx, Some(name), &path);
            let t = Instant::now();
            match catch_unwind(AssertUnwindSafe(|| module.process(ctx, &mut file))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => progress.record_error(name, &format!("{path}: {e}")),
                Err(_) => progress.record_error(name, &format!("{path}: module panicked")),
            }
            *times.entry(name).or_default() += t.elapsed();
        }
        if let Some(update) = file.take_update() {
            let _ = ctx.sink.send(WriteOp::Analysis(id, update));
        }
        progress.files_done.fetch_add(1, Ordering::Relaxed);
    }
    progress.set_activity(idx, None, "");
    for (name, module) in &mut pipeline {
        if catch_unwind(AssertUnwindSafe(|| module.shut_down(ctx))).is_err() {
            progress.record_error(name, "shut_down panicked");
        }
    }
    progress.add_module_times(times);
}

fn run_ds_modules(ctx: &JobContext, modules: Vec<(u32, &'static str, Box<dyn DataSourceModule>)>) {
    let progress = &ctx.progress;
    for (_, name, mut module) in modules {
        if progress.is_cancelled() {
            break;
        }
        progress.set_ds(Some(DsStatus {
            module: name.to_owned(),
            ..DsStatus::default()
        }));
        let t = Instant::now();
        let handle = DsProgress { progress };
        match catch_unwind(AssertUnwindSafe(|| module.process(ctx, &handle))) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => progress.record_error(name, &e.0),
            Err(_) => progress.record_error(name, "module panicked"),
        }
        progress.add_module_times(HashMap::from([(name, t.elapsed())]));
        progress.set_ds(None);
    }
}

/// Applies module results in short transactions until told to stop.
fn run_writer(
    db: &CaseDb,
    ops: &Receiver<WriteOp>,
    progress: &IngestProgress,
) -> Result<(), DbError> {
    loop {
        let Ok(first) = ops.recv() else {
            return Ok(());
        };
        let mut stop = false;
        let mut artifacts = 0u64;
        let result = db.bulk(|b| {
            let mut next = Some(first);
            let mut n = 0;
            while let Some(op) = next.take() {
                match op {
                    WriteOp::Stop => {
                        stop = true;
                        break;
                    }
                    WriteOp::Analysis(id, a) => b.set_file_analysis(id, &a)?,
                    WriteOp::Artifact(a, key) => {
                        if b.add_artifact_unless_exists(&a, &key)?.is_some() {
                            artifacts += 1;
                        }
                    }
                }
                n += 1;
                if n < WRITE_BATCH {
                    next = ops.try_recv().ok();
                }
            }
            Ok(())
        });
        if let Err(e) = result {
            // Results can no longer be saved: stop the job.
            progress.cancel();
            return Err(e);
        }
        progress.artifacts.fetch_add(artifacts, Ordering::Relaxed);
        if stop {
            return Ok(());
        }
    }
}
