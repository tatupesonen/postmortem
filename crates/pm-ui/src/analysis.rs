//! Background analysis of data sources: add to the case, then run the
//! ingest modules. Progress is shown in the status bar and, in detail, in
//! the "Ingest Progress" window (Autopsy's progress snapshot + inbox).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use parking_lot::Mutex;
use pm_case::{Case, LoadOptions, LoadProgress};
use pm_ingest::{Inbox, IngestMessage, IngestProgress, JobSettings, MessageKind, Phase, Snapshot};

use crate::format::grouped;

/// Where a job is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stage {
    Adding,
    Analysing,
    Done {
        files: u64,
        artifacts: u64,
        errors: usize,
        cancelled: bool,
    },
    Failed(String),
}

/// One data source being added and analysed.
#[derive(Debug)]
pub(crate) struct AnalysisJob {
    pub(crate) name: String,
    pub(crate) load: Arc<LoadProgress>,
    pub(crate) ingest: Arc<IngestProgress>,
    pub(crate) stage: Mutex<Stage>,
}

impl AnalysisJob {
    pub(crate) fn stage(&self) -> Stage {
        self.stage.lock().clone()
    }

    pub(crate) fn is_active(&self) -> bool {
        matches!(self.stage(), Stage::Adding | Stage::Analysing)
    }

    pub(crate) fn cancel(&self) {
        self.load.cancel();
        self.ingest.cancel();
    }
}

/// The open case and its analysis jobs.
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    case: Option<Arc<Case>>,
    pub(crate) jobs: Vec<Arc<AnalysisJob>>,
    pub(crate) inbox: Arc<Inbox>,
}

impl Analysis {
    pub(crate) fn case(&self) -> Option<&Arc<Case>> {
        self.case.as_ref()
    }

    pub(crate) fn any_active(&self) -> bool {
        self.jobs.iter().any(|j| j.is_active())
    }

    /// Whether a data source is still being added to the case.
    pub(crate) fn any_adding(&self) -> bool {
        self.jobs.iter().any(|j| matches!(j.stage(), Stage::Adding))
    }

    /// Makes `case` the open case (any previous jobs are forgotten).
    pub(crate) fn set_case(&mut self, case: Arc<Case>) {
        self.close();
        self.case = Some(case);
    }

    /// Closes the case, cancelling running jobs.
    pub(crate) fn close(&mut self) {
        for j in &self.jobs {
            j.cancel();
        }
        self.jobs.clear();
        self.inbox.clear();
        self.case = None;
    }

    /// Adds `spec` to the case and analyses it with the ingest modules.
    pub(crate) fn start(
        &mut self,
        ctx: &egui::Context,
        spec: pm_case::DataSourceSpec,
        load: LoadOptions,
        settings: JobSettings,
    ) -> Result<(), String> {
        let case = self
            .case
            .clone()
            .ok_or("Create or open a case before adding a data source.")?;
        let name = spec.label();
        let job = Arc::new(AnalysisJob {
            name,
            load: Arc::new(LoadProgress::default()),
            ingest: Arc::new(IngestProgress::default()),
            stage: Mutex::new(Stage::Adding),
        });
        self.jobs.push(Arc::clone(&job));
        let inbox = Arc::clone(&self.inbox);
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("pm-analysis".into())
            .spawn(move || {
                let stage = run(&case, &spec, &load, &settings, &job, &inbox);
                *job.stage.lock() = stage;
                ctx.request_repaint();
            });
        spawned
            .map(|_| ())
            .map_err(|e| format!("cannot start analysis: {e}"))
    }
}

impl Analysis {
    /// Runs the ingest modules again on a data source already in the case
    /// (Autopsy "Run Ingest Modules").
    pub(crate) fn run_ingest(
        &mut self,
        ctx: &egui::Context,
        ds_id: i64,
        name: String,
        settings: JobSettings,
    ) -> Result<(), String> {
        let case = self.case.clone().ok_or("No case is open.")?;
        let job = Arc::new(AnalysisJob {
            name,
            load: Arc::new(LoadProgress::default()),
            ingest: Arc::new(IngestProgress::default()),
            stage: Mutex::new(Stage::Analysing),
        });
        self.jobs.push(Arc::clone(&job));
        let inbox = Arc::clone(&self.inbox);
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("pm-ingest".into())
            .spawn(move || {
                let modules = crate::plugins::modules();
                let stage = match pm_ingest::run_job(
                    &case,
                    ds_id,
                    &settings,
                    &modules,
                    &job.ingest,
                    &inbox,
                ) {
                    Ok(s) => Stage::Done {
                        files: s.files,
                        artifacts: s.artifacts,
                        errors: s.errors.len(),
                        cancelled: s.cancelled,
                    },
                    Err(e) => Stage::Failed(e.to_string()),
                };
                *job.stage.lock() = stage;
                ctx.request_repaint();
            })
            .map(|_| ())
            .map_err(|e| format!("cannot start ingest: {e}"))
    }
}

fn run(
    case: &Arc<Case>,
    spec: &pm_case::DataSourceSpec,
    load: &LoadOptions,
    settings: &JobSettings,
    job: &AnalysisJob,
    inbox: &Arc<Inbox>,
) -> Stage {
    let loaded = match pm_case::add_data_source(case, spec, load, &job.load) {
        Ok(s) => s,
        Err(pm_case::CaseError::Cancelled) => {
            return Stage::Done {
                files: 0,
                artifacts: 0,
                errors: 0,
                cancelled: true,
            };
        }
        Err(e) => return Stage::Failed(e.to_string()),
    };
    *job.stage.lock() = Stage::Analysing;
    let modules = crate::plugins::modules();
    match pm_ingest::run_job(case, loaded.ds_id, settings, &modules, &job.ingest, inbox) {
        Ok(s) => Stage::Done {
            files: s.files,
            artifacts: s.artifacts,
            errors: s.errors.len(),
            cancelled: s.cancelled,
        },
        Err(e) => Stage::Failed(e.to_string()),
    }
}

/// `~/postmortem/Cases` (or the working directory without a home).
pub(crate) fn default_case_root() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_default()
        .join("postmortem")
        .join("Cases")
}

/// `h:mm:ss` / `m:ss`.
pub(crate) fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Compact status-bar line for one job; returns whether it was clicked
/// (to open the details window).
pub(crate) fn status_line(ui: &mut egui::Ui, job: &AnalysisJob) -> bool {
    let mut open = false;
    match job.stage() {
        Stage::Adding => {
            ui.spinner();
            let files = job.load.files.load(Ordering::Relaxed);
            let r = ui.add(
                egui::Label::new(format!(
                    "Adding {}: {} · {} files",
                    job.name,
                    job.load.stage(),
                    grouped(files)
                ))
                .sense(egui::Sense::click()),
            );
            open |= r.clicked();
            if ui.small_button("✖").on_hover_text("Cancel").clicked() {
                job.cancel();
            }
        }
        Stage::Analysing => {
            let snap = job.ingest.snapshot();
            let frac = snap.fraction().unwrap_or(0.0);
            let bar = egui::ProgressBar::new(frac)
                .desired_width(180.0)
                .desired_height(14.0)
                .show_percentage()
                .animate(true);
            open |= ui
                .add(bar)
                .interact(egui::Sense::click())
                .on_hover_text("Show ingest progress")
                .clicked();
            let eta = snap
                .eta()
                .map_or_else(String::new, |d| format!(" · ETA {}", clock(d)));
            let line = format!(
                "{} · {} / {} files{eta} · {}",
                job.name,
                grouped(snap.files_done),
                grouped(snap.files_total),
                snap.activity_line()
            );
            let r = ui.add(
                egui::Label::new(line)
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            open |= r.clicked();
            if ui
                .small_button("✖")
                .on_hover_text("Cancel analysis")
                .clicked()
            {
                job.cancel();
            }
        }
        Stage::Done { .. } | Stage::Failed(_) => {}
    }
    open
}

/// The detailed "Ingest Progress" window contents.
pub(crate) fn details_ui(ui: &mut egui::Ui, analysis: &Analysis) {
    ui.style_mut().interaction.selectable_labels = false;
    if let Some(case) = analysis.case() {
        ui.horizontal(|ui| {
            ui.strong("Case:");
            ui.label(case.details().name);
            ui.weak(case.dir().display().to_string());
        });
        ui.separator();
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for job in analysis.jobs.iter().rev() {
                job_ui(ui, job);
                ui.separator();
            }
            ui.heading("Ingest Messages");
            inbox_ui(ui, &analysis.inbox);
        });
}

fn job_ui(ui: &mut egui::Ui, job: &AnalysisJob) {
    ui.horizontal(|ui| {
        ui.heading(&job.name);
        if job.is_active() && ui.button("Cancel").clicked() {
            job.cancel();
        }
    });
    match job.stage() {
        Stage::Adding => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "{} — {} rows, {} directories",
                    job.load.stage(),
                    grouped(job.load.files.load(Ordering::Relaxed)),
                    grouped(job.load.dirs.load(Ordering::Relaxed))
                ));
            });
        }
        Stage::Analysing => snapshot_ui(ui, &job.ingest.snapshot()),
        Stage::Done {
            files,
            artifacts,
            errors,
            cancelled,
        } => {
            let snap = job.ingest.snapshot();
            ui.label(format!(
                "{} — {} files analysed, {} results, {} module errors, {}",
                if cancelled { "Cancelled" } else { "Complete" },
                grouped(files),
                grouped(artifacts),
                errors,
                clock(snap.elapsed)
            ));
            module_times(ui, &snap);
            let errs = job.ingest.errors();
            if !errs.is_empty() {
                egui::CollapsingHeader::new(format!("Module errors ({})", errs.len()))
                    .id_salt(("errors", &job.name))
                    .show(ui, |ui| {
                        for e in errs.iter().take(200) {
                            ui.label(egui::RichText::new(e).monospace().small());
                        }
                    });
            }
        }
        Stage::Failed(e) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }
}

fn snapshot_ui(ui: &mut egui::Ui, snap: &Snapshot) {
    let phase = match snap.phase {
        Phase::Starting => "Starting modules".to_owned(),
        Phase::Files => "Analysing files".to_owned(),
        Phase::DataSource(n) => format!("Data-source analysis (stage {n})"),
        Phase::Finishing => "Finishing".to_owned(),
        Phase::Done => "Complete".to_owned(),
        Phase::Cancelled => "Cancelled".to_owned(),
    };
    ui.add(
        egui::ProgressBar::new(snap.fraction().unwrap_or(0.0))
            .show_percentage()
            .animate(true),
    );
    ui.label(format!(
        "{phase} — {} / {} files · {:.0} files/s · elapsed {}{} · {} results · {} errors",
        grouped(snap.files_done),
        grouped(snap.files_total),
        snap.rate,
        clock(snap.elapsed),
        snap.eta()
            .map_or_else(String::new, |d| format!(" · ETA {}", clock(d))),
        grouped(snap.artifacts),
        snap.errors
    ));
    if let Some(ds) = &snap.ds {
        ui.label(format!("{}: {}", ds.module, ds.message));
    }
    egui::CollapsingHeader::new("Threads")
        .default_open(true)
        .id_salt(("threads", &snap.data_source))
        .show(ui, |ui| {
            egui::Grid::new(("thread-grid", &snap.data_source))
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    ui.strong("Thread");
                    ui.strong("Activity");
                    ui.strong("File");
                    ui.end_row();
                    for (i, w) in snap.workers.iter().enumerate() {
                        ui.label(format!("{}", i + 1));
                        ui.label(w.module.as_deref().unwrap_or("Idle"));
                        ui.add(
                            egui::Label::new(egui::RichText::new(&w.file).monospace()).truncate(),
                        );
                        ui.end_row();
                    }
                });
        });
    module_times(ui, snap);
}

fn module_times(ui: &mut egui::Ui, snap: &Snapshot) {
    if snap.module_times.is_empty() {
        return;
    }
    let total: f64 = snap.module_times.iter().map(|(_, d)| d.as_secs_f64()).sum();
    egui::CollapsingHeader::new("Module run times")
        .id_salt(("times", &snap.data_source))
        .show(ui, |ui| {
            for (m, d) in &snap.module_times {
                ui.label(format!(
                    "{m}: {} ({:.0} %)",
                    clock(*d),
                    100.0 * d.as_secs_f64() / total.max(1e-9)
                ));
            }
        });
}

fn inbox_ui(ui: &mut egui::Ui, inbox: &Inbox) {
    let msgs: Vec<IngestMessage> = inbox.since(0);
    if msgs.is_empty() {
        ui.weak("No messages.");
        return;
    }
    for m in msgs.iter().rev().take(500) {
        let (icon, color) = match m.kind {
            MessageKind::Error => ("⛔", ui.visuals().error_fg_color),
            MessageKind::Warning => ("⚠", ui.visuals().warn_fg_color),
            MessageKind::Data => ("★", ui.visuals().hyperlink_color),
            MessageKind::Info => ("ℹ", ui.visuals().text_color()),
        };
        let header = format!(
            "{icon} {} — {}",
            m.source.as_deref().unwrap_or("Ingest"),
            m.subject
        );
        if m.details.is_empty() {
            ui.colored_label(color, header);
        } else {
            egui::CollapsingHeader::new(egui::RichText::new(header).color(color))
                .id_salt(("msg", m.id))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(&m.details).monospace());
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_format() {
        assert_eq!(clock(Duration::from_secs(5)), "0:05");
        assert_eq!(clock(Duration::from_secs(125)), "2:05");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    }
}
