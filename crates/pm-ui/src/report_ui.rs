//! Generate Report (Autopsy's report wizard and progress panel): choose a
//! report module, the data sources, then what to include (results and
//! tags for table reports, columns for Files - Text); the report runs on a
//! background thread with progress and cancellation.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use pm_case::Case;
use pm_report::files_text::{FileColumn, FileReportSettings};
use pm_report::tagged_hashes::TaggedHashesSettings;
use pm_report::{ReportKind, ReportSettings};

/// Wizard pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Module,
    DataSources,
    Contents,
    Running,
}

/// Progress shared with the report thread.
#[derive(Debug, Default)]
struct Shared {
    status: Mutex<String>,
    cancelled: AtomicBool,
    result: Mutex<Option<Result<std::path::PathBuf, String>>>,
}

impl pm_report::Progress for Shared {
    fn status(&self, message: &str) {
        message.clone_into(&mut self.status.lock());
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// The data a wizard needs from the case (loaded once when it opens).
#[derive(Debug, Clone, Default)]
pub(crate) struct Choices {
    pub(crate) data_sources: Vec<(i64, String)>,
    /// `(type name, display name)` of the types with results.
    pub(crate) result_types: Vec<(String, String)>,
    pub(crate) tag_names: Vec<String>,
}

impl Choices {
    pub(crate) fn load(case: &Case) -> Result<Self, String> {
        let db = case.db();
        let err = |e: pm_casedb::DbError| e.to_string();
        let names = db.artifact_type_names().map_err(err)?;
        let mut result_types: Vec<(String, String)> = db
            .artifact_counts(None)
            .map_err(err)?
            .into_iter()
            .filter_map(|(id, _)| {
                names
                    .iter()
                    .find(|n| n.0 == id)
                    .filter(|n| !pm_casedb::catalog::HIDDEN_ARTIFACT_TYPES.contains(&n.1.as_str()))
                    .map(|n| (n.1.clone(), n.2.clone()))
            })
            .collect();
        result_types.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(Self {
            data_sources: db
                .data_sources()
                .map_err(err)?
                .into_iter()
                .map(|d| (d.id, d.name))
                .collect(),
            result_types,
            tag_names: db
                .tag_names_in_use(None, None)
                .map_err(err)?
                .into_iter()
                .map(|t| t.name)
                .collect(),
        })
    }
}

/// What the wizard asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WizardAction {
    None,
    Close,
    /// A report was written (refresh the Reports node).
    Finished,
}

#[derive(Debug)]
pub(crate) struct ReportWizard {
    page: Page,
    kind: ReportKind,
    choices: Choices,
    data_sources: BTreeSet<i64>,
    tagged_only: bool,
    tags: BTreeSet<String>,
    types: BTreeSet<String>,
    columns: Vec<FileColumn>,
    delimiter: String,
    /// Save Tagged Hashes: the hash set to add to.
    hash_set: String,
    shared: Arc<Shared>,
    timezone: jiff::tz::TimeZone,
    reported: bool,
}

impl ReportWizard {
    pub(crate) fn new(choices: Choices, timezone: jiff::tz::TimeZone) -> Self {
        Self {
            page: Page::Module,
            kind: ReportKind::Html,
            data_sources: choices.data_sources.iter().map(|d| d.0).collect(),
            types: choices.result_types.iter().map(|t| t.0.clone()).collect(),
            tags: BTreeSet::new(),
            tagged_only: false,
            columns: FileColumn::ALL.to_vec(),
            delimiter: "\t".into(),
            hash_set: String::new(),
            choices,
            shared: Arc::new(Shared::default()),
            timezone,
            reported: false,
        }
    }

    fn settings(&self) -> ReportSettings {
        let all_sources = self.data_sources.len() == self.choices.data_sources.len();
        let all_types = self.types.len() == self.choices.result_types.len();
        ReportSettings {
            data_sources: if all_sources {
                Vec::new()
            } else {
                self.data_sources.iter().copied().collect()
            },
            artifact_types: (!all_types).then(|| self.types.iter().cloned().collect()),
            tag_names: if self.tagged_only {
                self.tags.iter().cloned().collect()
            } else {
                Vec::new()
            },
            tagged_files: true,
            tagged_results: true,
            timezone: self.timezone.clone(),
        }
    }

    fn start(&mut self, case: &Arc<Case>, ctx: &egui::Context) {
        self.page = Page::Running;
        let (case, shared, ctx) = (Arc::clone(case), Arc::clone(&self.shared), ctx.clone());
        let (kind, settings) = (self.kind, self.settings());
        let other = pm_report::OtherSettings {
            files: FileReportSettings {
                columns: self.columns.clone(),
                delimiter: self.delimiter.clone(),
            },
            tagged_hashes: TaggedHashesSettings {
                tag_names: self.tags.iter().cloned().collect(),
                hash_set: self.hash_set.trim().into(),
            },
        };
        let spawned = std::thread::Builder::new()
            .name("pm-report".into())
            .spawn(move || {
                let r = pm_report::generate(&case, kind, &settings, &other, shared.as_ref())
                    .map(|g| g.path)
                    .map_err(|e| e.to_string());
                *shared.result.lock() = Some(r);
                ctx.request_repaint();
            });
        if let Err(e) = spawned {
            *self.shared.result.lock() = Some(Err(format!("cannot start the report: {e}")));
        }
    }

    /// Draws the wizard; `case` is the open case.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: &Arc<Case>) -> WizardAction {
        let mut action = WizardAction::None;
        let mut start = false;
        egui::Modal::new(egui::Id::new("report-wizard")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("Generate Report");
            ui.separator();
            match self.page {
                Page::Module => self.module_page(ui),
                Page::DataSources => self.data_source_page(ui),
                Page::Contents => self.contents_page(ui),
                Page::Running => {
                    if let Some(a) = self.running_page(ui) {
                        action = a;
                    }
                }
            }
            ui.separator();
            if self.page != Page::Running {
                ui.horizontal(|ui| {
                    let back = match self.page {
                        Page::DataSources => Some(Page::Module),
                        Page::Contents => Some(Page::DataSources),
                        _ => None,
                    };
                    if ui
                        .add_enabled(back.is_some(), egui::Button::new("< Back"))
                        .clicked()
                        && let Some(b) = back
                    {
                        self.page = b;
                    }
                    let next = match self.page {
                        Page::Module => Some(Page::DataSources),
                        Page::DataSources => Some(Page::Contents),
                        _ => None,
                    };
                    if ui
                        .add_enabled(next.is_some(), egui::Button::new("Next >"))
                        .clicked()
                        && let Some(n) = next
                    {
                        self.page = n;
                    }
                    let ready = !self.data_sources.is_empty()
                        && match self.kind {
                            ReportKind::Html | ReportKind::Excel => {
                                !self.tagged_only || !self.tags.is_empty()
                            }
                            ReportKind::FilesText => !self.columns.is_empty(),
                            ReportKind::BodyFile | ReportKind::Kml => true,
                            ReportKind::TaggedHashes => {
                                !self.tags.is_empty() && !self.hash_set.trim().is_empty()
                            }
                        };
                    if ui
                        .add_enabled(
                            self.page == Page::Contents && ready,
                            egui::Button::new("Finish"),
                        )
                        .clicked()
                    {
                        start = true;
                    }
                    if ui.button("Cancel").clicked() {
                        action = WizardAction::Close;
                    }
                });
            }
        });
        if start {
            self.start(case, ctx);
        }
        action
    }

    fn module_page(&mut self, ui: &mut egui::Ui) {
        ui.label("Select and configure a report module:");
        for k in ReportKind::ALL {
            ui.radio_value(&mut self.kind, k, k.name());
            ui.indent(k.name(), |ui| ui.weak(k.description()));
        }
    }

    fn data_source_page(&mut self, ui: &mut egui::Ui) {
        ui.label("Select which data source(s) to include:");
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for (id, name) in &self.choices.data_sources {
                    let mut on = self.data_sources.contains(id);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            self.data_sources.insert(*id);
                        } else {
                            self.data_sources.remove(id);
                        }
                    }
                }
            });
    }

    fn tag_checkboxes(&mut self, ui: &mut egui::Ui) {
        if self.choices.tag_names.is_empty() {
            ui.weak("No tags in the case.");
        }
        for t in &self.choices.tag_names {
            let mut on = self.tags.contains(t);
            if ui.checkbox(&mut on, t).changed() {
                if on {
                    self.tags.insert(t.clone());
                } else {
                    self.tags.remove(t);
                }
            }
        }
    }

    fn contents_page(&mut self, ui: &mut egui::Ui) {
        if self.kind == ReportKind::BodyFile {
            ui.label("Every file system file is listed with its MAC times; no options.");
            return;
        }
        if self.kind == ReportKind::Kml {
            ui.label("Every located result (GPS, EXIF) of the chosen data sources; no options.");
            return;
        }
        if self.kind == ReportKind::TaggedHashes {
            ui.label("Add the hashes of files with these tags:");
            self.tag_checkboxes(ui);
            ui.horizontal(|ui| {
                ui.label("Hash set (.kdb):");
                ui.text_edit_singleline(&mut self.hash_set);
                if ui.button("Browse…").clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .add_filter("Hash set", &["kdb"])
                        .set_file_name("tagged.kdb")
                        .save_file()
                {
                    self.hash_set = p.display().to_string();
                }
            });
            ui.weak("A new hash set is created when the file does not exist.");
            return;
        }
        if !self.kind.is_table_report() {
            ui.label("Select items to include in File Report:");
            egui::Grid::new("file-columns")
                .num_columns(2)
                .show(ui, |ui| {
                    for (i, c) in FileColumn::ALL.into_iter().enumerate() {
                        let mut on = self.columns.contains(&c);
                        if ui.checkbox(&mut on, c.title()).changed() {
                            if on {
                                self.columns.push(c);
                                self.columns
                                    .sort_by_key(|x| FileColumn::ALL.iter().position(|y| y == x));
                            } else {
                                self.columns.retain(|x| *x != c);
                            }
                        }
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Delimiter:");
                for (label, d) in [
                    ("Tab", "\t"),
                    ("Comma", ","),
                    ("Semicolon", ";"),
                    ("Pipe", "|"),
                ] {
                    ui.radio_value(&mut self.delimiter, d.to_owned(), label);
                }
            });
            return;
        }
        ui.radio_value(&mut self.tagged_only, false, "All Results");
        ui.radio_value(&mut self.tagged_only, true, "Specific Tagged Results");
        if self.tagged_only {
            ui.indent("tags", |ui| self.tag_checkboxes(ui));
        }
        egui::CollapsingHeader::new(format!(
            "Result types ({} of {})",
            self.types.len(),
            self.choices.result_types.len()
        ))
        .id_salt("report-types")
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.small_button("Select All").clicked() {
                    self.types = self
                        .choices
                        .result_types
                        .iter()
                        .map(|t| t.0.clone())
                        .collect();
                }
                if ui.small_button("Deselect All").clicked() {
                    self.types.clear();
                }
            });
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    for (name, display) in &self.choices.result_types {
                        let mut on = self.types.contains(name);
                        if ui.checkbox(&mut on, display).changed() {
                            if on {
                                self.types.insert(name.clone());
                            } else {
                                self.types.remove(name);
                            }
                        }
                    }
                });
        });
    }

    fn running_page(&mut self, ui: &mut egui::Ui) -> Option<WizardAction> {
        let result = self.shared.result.lock().clone();
        ui.strong(self.kind.name());
        match result {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.shared.status.lock().clone());
                });
                if ui.button("Cancel").clicked() {
                    self.shared.cancelled.store(true, Ordering::Relaxed);
                }
                None
            }
            Some(Ok(path)) => {
                ui.label("Complete");
                if ui.link(path.display().to_string()).clicked() {
                    crate::platform::open(&path);
                }
                let finished = !std::mem::replace(&mut self.reported, true);
                let mut close = false;
                if ui.button("Close").clicked() {
                    close = true;
                }
                if close {
                    Some(WizardAction::Close)
                } else if finished {
                    Some(WizardAction::Finished)
                } else {
                    None
                }
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                ui.button("Close").clicked().then_some(WizardAction::Close)
            }
        }
    }
}
