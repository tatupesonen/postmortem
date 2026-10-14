//! Live job progress for the status bar, progress dialogs and the
//! "ingest progress snapshot" (Autopsy's `IngestProgressSnapshotPanel`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Phase of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Starting,
    /// Analysing files (stage 1, alongside stage-1 data-source modules).
    Files,
    /// A later data-source stage.
    DataSource(u32),
    Finishing,
    Done,
    Cancelled,
}

/// What one worker thread is doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Activity {
    /// Module display name; `None` = idle.
    pub module: Option<String>,
    /// Path of the file being analysed.
    pub file: String,
}

/// Progress of the data-source module that is running.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DsStatus {
    pub module: String,
    pub message: String,
    /// Units done / total (`total == 0`: indeterminate).
    pub done: u64,
    pub total: u64,
}

/// A consistent copy of the progress for display.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub data_source: String,
    pub phase: Phase,
    pub files_done: u64,
    /// Estimate of files to analyse (`0` while unknown).
    pub files_total: u64,
    pub artifacts: u64,
    pub errors: u64,
    pub elapsed: Duration,
    /// Files per second since the job started.
    pub rate: f64,
    pub workers: Vec<Activity>,
    pub ds: Option<DsStatus>,
    /// Accumulated time per module, longest first.
    pub module_times: Vec<(String, Duration)>,
}

impl Snapshot {
    /// Fraction of files analysed (`None` while the total is unknown).
    pub fn fraction(&self) -> Option<f32> {
        (self.files_total > 0)
            .then(|| (self.files_done as f64 / self.files_total as f64).clamp(0.0, 1.0) as f32)
    }

    /// Estimated time left, from the average rate so far.
    pub fn eta(&self) -> Option<Duration> {
        if self.rate <= 0.0 || self.files_total <= self.files_done {
            return None;
        }
        let left = (self.files_total - self.files_done) as f64 / self.rate;
        Some(Duration::from_secs_f64(left))
    }

    /// One-line description of the current work for a status bar.
    pub fn activity_line(&self) -> String {
        match self.phase {
            Phase::Starting => "Starting ingest modules".into(),
            Phase::Finishing => "Finishing".into(),
            Phase::Done => "Analysis complete".into(),
            Phase::Cancelled => "Analysis cancelled".into(),
            Phase::DataSource(_) => self
                .ds
                .as_ref()
                .map_or_else(String::new, |d| format!("{}: {}", d.module, d.message)),
            Phase::Files => self
                .workers
                .iter()
                .find_map(|w| w.module.as_ref().map(|m| format!("{m}: {}", w.file)))
                .unwrap_or_else(|| "Analysing files".into()),
        }
    }
}

/// Shared, lock-light progress state of one job.
#[derive(Debug)]
pub struct IngestProgress {
    started: Instant,
    cancel: AtomicBool,
    pub(crate) files_done: AtomicU64,
    pub(crate) files_total: AtomicU64,
    pub(crate) artifacts: AtomicU64,
    pub(crate) errors: AtomicU64,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    data_source: String,
    phase: Phase,
    workers: Vec<Activity>,
    ds: Option<DsStatus>,
    module_times: HashMap<String, Duration>,
    error_log: Vec<String>,
}

/// Errors kept for display; later ones are only counted.
const MAX_ERROR_LOG: usize = 1000;

impl Default for IngestProgress {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            cancel: AtomicBool::new(false),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            artifacts: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            state: Mutex::new(State::default()),
        }
    }
}

impl IngestProgress {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn phase(&self) -> Phase {
        self.state.lock().phase
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.phase(), Phase::Done | Phase::Cancelled)
    }

    /// Module errors recorded so far (`module: message`).
    pub fn errors(&self) -> Vec<String> {
        self.state.lock().error_log.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        let st = self.state.lock();
        let elapsed = self.started.elapsed();
        let files_done = self.files_done.load(Ordering::Relaxed);
        let mut module_times: Vec<(String, Duration)> = st
            .module_times
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        module_times.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Snapshot {
            data_source: st.data_source.clone(),
            phase: st.phase,
            files_done,
            files_total: self.files_total.load(Ordering::Relaxed),
            artifacts: self.artifacts.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            elapsed,
            rate: files_done as f64 / elapsed.as_secs_f64().max(1e-3),
            workers: st.workers.clone(),
            ds: st.ds.clone(),
            module_times,
        }
    }

    pub(crate) fn set_phase(&self, phase: Phase) {
        self.state.lock().phase = phase;
    }

    pub(crate) fn set_data_source(&self, name: String) {
        self.state.lock().data_source = name;
    }

    pub(crate) fn set_workers(&self, n: usize) {
        self.state.lock().workers = vec![Activity::default(); n];
    }

    pub(crate) fn set_activity(&self, worker: usize, module: Option<&str>, file: &str) {
        let mut st = self.state.lock();
        if let Some(a) = st.workers.get_mut(worker) {
            a.module = module.map(str::to_owned);
            if a.file != file {
                file.clone_into(&mut a.file);
            }
        }
    }

    pub(crate) fn set_ds(&self, ds: Option<DsStatus>) {
        self.state.lock().ds = ds;
    }

    pub(crate) fn update_ds(&self, f: impl FnOnce(&mut DsStatus)) {
        if let Some(d) = self.state.lock().ds.as_mut() {
            f(d);
        }
    }

    pub(crate) fn add_module_times(&self, times: HashMap<&'static str, Duration>) {
        let mut st = self.state.lock();
        for (k, v) in times {
            *st.module_times.entry(k.to_owned()).or_default() += v;
        }
    }

    pub(crate) fn record_error(&self, module: &str, msg: &str) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(module, "{msg}");
        let mut st = self.state.lock();
        if st.error_log.len() < MAX_ERROR_LOG {
            st.error_log.push(format!("{module}: {msg}"));
        }
    }
}

/// Progress handle given to data-source modules
/// (Autopsy's `DataSourceIngestModuleProgress`).
#[derive(Debug)]
pub struct DsProgress<'a> {
    pub(crate) progress: &'a IngestProgress,
}

impl DsProgress<'_> {
    /// Switches to a determinate bar of `total` units.
    pub fn set_total(&self, total: u64) {
        self.progress.update_ds(|d| d.total = total);
    }

    pub fn set_done(&self, done: u64) {
        self.progress.update_ds(|d| d.done = done);
    }

    pub fn message(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.progress.update_ds(|d| d.message = msg);
    }
}
