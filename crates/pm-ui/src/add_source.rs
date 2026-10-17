//! The Add Data Source wizard (Autopsy's image panel + "Configure Ingest"
//! page), and the ingest preferences it remembers.

use std::path::PathBuf;
use std::sync::Arc;

use pm_case::LoadOptions;
use pm_ingest::{FileFilter, JobSettings, ModuleFactory, ModuleSetting};
use pm_modules::hash_lookup::{HashLookupSettings, HashSetConfig, SetKind};
use serde::{Deserialize, Serialize};

/// Ingest choices remembered between runs (Autopsy stores them per
/// settings context).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestPrefs {
    /// Ids of modules switched off.
    pub disabled: Vec<String>,
    /// Ids of modules switched on that are off by default (Keyword
    /// Search). Modules in neither list use their default.
    pub enabled: Vec<String>,
    pub hash_lookup: HashLookupSettings,
    /// "All Files and Directories (Not Unallocated Space)".
    pub ignore_unallocated: bool,
    /// "Ignore orphan files in FAT file systems".
    pub ignore_fat_orphans: bool,
    /// Plugin settings as JSON text, by module id (the module's defaults
    /// when absent).
    pub module_settings: std::collections::BTreeMap<String, String>,
}

impl IngestPrefs {
    /// Whether a module runs (an explicit choice, else its default).
    pub(crate) fn is_enabled(&self, info: &pm_ingest::ModuleInfo) -> bool {
        if self.disabled.iter().any(|d| d == info.id) {
            false
        } else {
            self.enabled.iter().any(|d| d == info.id) || info.enabled_by_default
        }
    }

    /// Records the user's choice for a module.
    pub(crate) fn set_enabled(&mut self, id: &str, on: bool) {
        self.disabled.retain(|d| d != id);
        self.enabled.retain(|d| d != id);
        if on {
            self.enabled.push(id.to_owned());
        } else {
            self.disabled.push(id.to_owned());
        }
    }

    /// Job settings for the built-in modules and loaded plugins.
    pub(crate) fn job_settings(&self) -> JobSettings {
        self.job_settings_for(&crate::plugins::modules())
    }

    /// Job settings for `modules`.
    pub(crate) fn job_settings_for(&self, modules: &[Arc<dyn ModuleFactory>]) -> JobSettings {
        let modules = modules
            .iter()
            .map(|f| {
                let info = f.info();
                let id = info.id;
                let settings = if id == "hash_lookup" {
                    serde_json::to_value(&self.hash_lookup).unwrap_or_default()
                } else {
                    self.module_settings
                        .get(id)
                        .and_then(|t| serde_json::from_str(t).ok())
                        .unwrap_or_else(|| f.default_settings())
                };
                ModuleSetting {
                    id: id.into(),
                    enabled: self.is_enabled(&info),
                    settings,
                }
            })
            .collect();
        JobSettings {
            modules,
            filter: FileFilter {
                ignore_unallocated: self.ignore_unallocated,
                ignore_known: false,
            },
            threads: 0,
        }
    }
}

/// What the wizard asks the app to do.
#[derive(Debug, Clone)]
pub(crate) enum AddAction {
    None,
    Cancel,
    Start(Box<StartRequest>),
}

/// What the wizard collected for adding a data source.
#[derive(Debug, Clone)]
pub(crate) struct StartRequest {
    pub(crate) spec: pm_case::DataSourceSpec,
    pub(crate) load: LoadOptions,
    pub(crate) settings: JobSettings,
    pub(crate) prefs: IngestPrefs,
}

/// Autopsy's data source types (processors), in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    DiskImage,
    LocalDisk,
    LogicalFiles,
    Unallocated,
    LogicalImager,
    Xry,
}

impl SourceKind {
    const ALL: [Self; 6] = [
        Self::DiskImage,
        Self::LocalDisk,
        Self::LogicalFiles,
        Self::Unallocated,
        Self::LogicalImager,
        Self::Xry,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::DiskImage => "Disk Image or VM File",
            Self::LocalDisk => "Local Disk",
            Self::LogicalFiles => "Logical Files",
            Self::Unallocated => "Unallocated Space Image File",
            Self::LogicalImager => "Autopsy Logical Imager Results",
            Self::Xry => "XRY Text Export",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::DiskImage => "Raw (dd), split raw, E01, VMDK, VHD, VHDX or QCOW2 image.",
            Self::LocalDisk => {
                "A disk or partition of this computer, read in place (needs administrator rights)."
            }
            Self::LogicalFiles => "Files and folders of this computer, read in place.",
            Self::Unallocated => {
                "A raw file of unallocated space, carved and searched without file-system analysis."
            }
            Self::LogicalImager => {
                "The output folder of the logical imager: sparse drive images or extracted files, with search hits."
            }
            Self::Xry => {
                "A folder of XRY text reports: calls, messages, contacts, device information and bookmarks."
            }
        }
    }
}

/// The Add Data Source wizard: type, its panel, then ingest modules.
#[derive(Debug)]
pub(crate) struct AddSourceWizard {
    step: u8,
    pub(crate) kind: SourceKind,
    /// Disk image or unallocated space image file.
    pub(crate) path: String,
    pub(crate) timezone: String,
    pub(crate) prefs: IngestPrefs,
    pub(crate) error: Option<String>,
    zones: Vec<String>,
    /// Local disks found (loaded in the background).
    disks: Option<crate::task::Task<Vec<pm_case::LocalDisk>>>,
    disk: Option<PathBuf>,
    /// Logical files: the selection, set name and timestamps.
    pub(crate) logical: Vec<PathBuf>,
    set_name: String,
    /// A path typed instead of browsed for.
    typed: String,
    times: [bool; 3],
    /// Unallocated space image: split into 2 GB chunks.
    chunked: bool,
}

impl AddSourceWizard {
    pub(crate) fn new(prefs: IngestPrefs, timezone: String) -> Self {
        let mut zones: Vec<String> = jiff::tz::db()
            .available()
            .map(|n| n.as_str().to_owned())
            .collect();
        zones.sort();
        Self {
            step: 0,
            kind: SourceKind::DiskImage,
            path: String::new(),
            timezone,
            prefs,
            error: None,
            zones,
            disks: None,
            disk: None,
            logical: Vec::new(),
            set_name: String::new(),
            typed: String::new(),
            times: [false; 3],
            chunked: true,
        }
    }

    fn check_file(&self, what: &str) -> Result<PathBuf, String> {
        let p = PathBuf::from(self.path.trim());
        if self.path.trim().is_empty() {
            return Err(format!("Choose {what}."));
        }
        if !p.is_file() {
            return Err(format!("{} is not a file.", p.display()));
        }
        Ok(p)
    }

    /// The data source the panel describes, or why it is incomplete.
    fn spec(&self) -> Result<pm_case::DataSourceSpec, String> {
        use pm_case::DataSourceSpec as S;
        Ok(match self.kind {
            SourceKind::DiskImage => S::DiskImage(self.check_file("a disk image")?),
            SourceKind::LocalDisk => S::LocalDisk(self.disk.clone().ok_or("Select a local disk.")?),
            SourceKind::LogicalFiles => {
                if self.logical.is_empty() {
                    return Err("Add at least one file or folder.".into());
                }
                if let Some(p) = self.logical.iter().find(|p| !p.exists()) {
                    return Err(format!("{} does not exist or cannot be read.", p.display()));
                }
                S::LogicalFiles {
                    paths: self.logical.clone(),
                    modified: self.times[0],
                    created: self.times[1],
                    accessed: self.times[2],
                }
            }
            SourceKind::Unallocated => S::UnallocatedImage {
                path: self.check_file("an unallocated space image file")?,
                chunk: self.chunked.then_some(pm_case::RAW_CHUNK),
            },
            SourceKind::LogicalImager => {
                let p = PathBuf::from(self.path.trim());
                if self.path.trim().is_empty() {
                    return Err("Choose the logical imager results folder.".into());
                }
                if !pm_case::is_logical_imager_folder(&p) {
                    return Err(format!("Cannot find SearchResults.txt in {}.", p.display()));
                }
                S::LogicalImager(p)
            }
            SourceKind::Xry => {
                let p = PathBuf::from(self.path.trim());
                if self.path.trim().is_empty() {
                    return Err("Choose the XRY export folder.".into());
                }
                match pm_case::xry_files(&p) {
                    Ok(v) if !v.is_empty() => S::XryFolder(p),
                    Ok(_) => {
                        return Err("Selected folder did not contain any XRY text files.".into());
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
        })
    }

    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> AddAction {
        let mut action = AddAction::None;
        let modal = egui::Modal::new(egui::Id::new("add-data-source")).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading("Add Data Source");
            ui.label(match self.step {
                0 => "Step 1 of 3 — Select Data Source Type",
                1 => "Step 2 of 3 — Select Data Source",
                _ => "Step 3 of 3 — Configure Ingest Modules",
            });
            ui.separator();
            match self.step {
                0 => self.type_page(ui),
                1 => self.source_page(ui),
                _ => self.step_two(ui),
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    action = AddAction::Cancel;
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match self.step {
                        0 => {
                            if ui.button("Next ›").clicked() {
                                self.error = None;
                                self.step = 1;
                            }
                        }
                        1 => {
                            if ui.button("Next ›").clicked() {
                                match self.spec() {
                                    Ok(_) => {
                                        self.error = None;
                                        self.step = 2;
                                    }
                                    Err(e) => self.error = Some(e),
                                }
                            }
                            if ui.button("‹ Back").clicked() {
                                self.step = 0;
                            }
                        }
                        _ => {
                            if ui.button("Finish").clicked() {
                                match self.spec() {
                                    Ok(spec) => {
                                        let name = (self.kind == SourceKind::LogicalFiles
                                            && !self.set_name.trim().is_empty())
                                        .then(|| self.set_name.trim().to_owned());
                                        action = AddAction::Start(Box::new(StartRequest {
                                            spec,
                                            load: LoadOptions {
                                                name,
                                                timezone: self.timezone.clone(),
                                                fat_orphans: !self.prefs.ignore_fat_orphans,
                                                process_unallocated: !self.prefs.ignore_unallocated,
                                                ..LoadOptions::default()
                                            },
                                            settings: self.prefs.job_settings(),
                                            prefs: self.prefs.clone(),
                                        }));
                                    }
                                    Err(e) => {
                                        self.error = Some(e);
                                        self.step = 1;
                                    }
                                }
                            }
                            if ui.button("‹ Back").clicked() {
                                self.step = 1;
                            }
                        }
                    },
                );
            });
        });
        if modal.should_close() && matches!(action, AddAction::None) {
            action = AddAction::Cancel;
        }
        action
    }

    fn type_page(&mut self, ui: &mut egui::Ui) {
        for k in SourceKind::ALL {
            ui.radio_value(&mut self.kind, k, egui::RichText::new(k.name()).strong());
            ui.indent(k.name(), |ui| {
                ui.weak(k.description());
            });
            ui.add_space(4.0);
        }
    }

    fn source_page(&mut self, ui: &mut egui::Ui) {
        match self.kind {
            SourceKind::DiskImage => self.image_page(ui),
            SourceKind::LocalDisk => self.disk_page(ui),
            SourceKind::LogicalFiles => self.logical_page(ui),
            SourceKind::Unallocated => self.unalloc_page(ui),
            SourceKind::Xry => {
                self.folder_page(ui, "Select an XRY folder to process:", "Select XRY Folder");
            }
            SourceKind::LogicalImager => {
                self.folder_page(
                    ui,
                    "Select the logical imager results folder:",
                    "Select Folder",
                );
                egui::Grid::new("imager-tz")
                    .num_columns(2)
                    .show(ui, |ui| self.timezone_row(ui));
                ui.weak("The folder is copied into the case before it is added.");
            }
        }
    }

    fn file_row(&mut self, ui: &mut egui::Ui, label: &str, hint: &str, title: &str) {
        ui.label(label);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.path)
                        .hint_text(hint)
                        .desired_width(400.0),
                )
                .changed()
            {
                self.error = None;
            }
            if ui.button("Browse…").clicked()
                && let Some(p) = rfd::FileDialog::new().set_title(title).pick_file()
            {
                self.path = p.display().to_string();
                self.error = None;
            }
        });
        ui.end_row();
    }

    fn timezone_row(&mut self, ui: &mut egui::Ui) {
        ui.label("Time zone:");
        egui::ComboBox::from_id_salt("tz")
            .selected_text(&self.timezone)
            .width(300.0)
            .show_ui(ui, |ui| {
                for z in &self.zones {
                    ui.selectable_value(&mut self.timezone, z.clone(), z);
                }
            });
        ui.end_row();
    }

    fn image_page(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("add-image")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                self.file_row(
                    ui,
                    "Disk image:",
                    "Raw, split raw, E01, VMDK, VHD(X) or QCOW2 — first segment",
                    "Add Data Source — Disk Image",
                );
                self.timezone_row(ui);
                ui.label("");
                ui.checkbox(
                    &mut self.prefs.ignore_fat_orphans,
                    "Ignore orphan files in FAT file systems",
                )
                .on_hover_text("Finding FAT orphan files scans all free space, which is slow on large volumes.");
                ui.end_row();
            });
        ui.weak("The time zone is used for file systems that store local times (FAT).");
    }

    fn disk_page(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.disks.is_none() {
            self.disks = Some(crate::task::Task::spawn(&ctx, pm_case::local_disks));
        }
        ui.horizontal(|ui| {
            ui.strong("Local Disk:");
            if ui.button("Refresh Local Disks").clicked() {
                self.disks = None;
                self.disk = None;
            }
        });
        match self.disks.as_mut().and_then(crate::task::Task::poll) {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading local disks...");
                });
            }
            Some(disks) if disks.is_empty() => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Disks were not detected. On some systems it requires admin privileges (or root).",
                );
            }
            Some(disks) => {
                let disks = disks.clone();
                egui::Grid::new("local-disks").striped(true).show(ui, |ui| {
                    ui.strong("Disk Name");
                    ui.strong("Disk Size");
                    ui.end_row();
                    for d in &disks {
                        let on = self.disk.as_ref() == Some(&d.path);
                        if ui
                            .selectable_label(on, d.path.display().to_string())
                            .clicked()
                        {
                            self.disk = Some(d.path.clone());
                            self.error = None;
                        }
                        ui.label(
                            d.size
                                .map_or_else(|| "—".into(), pm_core::units::format_size),
                        );
                        ui.end_row();
                    }
                });
            }
        }
        egui::Grid::new("disk-opts").num_columns(2).show(ui, |ui| {
            self.timezone_row(ui);
            ui.label("");
            ui.checkbox(
                &mut self.prefs.ignore_fat_orphans,
                "Ignore orphan files in FAT file systems",
            );
            ui.end_row();
        });
        ui.weak("The disk is only read. Nothing is written to it.");
    }

    fn logical_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Add Files…").clicked()
                && let Some(ps) = rfd::FileDialog::new()
                    .set_title("Select Local Files or Folders")
                    .pick_files()
            {
                self.logical.extend(ps);
                self.error = None;
            }
            if ui.button("Add Folder…").clicked()
                && let Some(ps) = rfd::FileDialog::new()
                    .set_title("Select Local Files or Folders")
                    .pick_folders()
            {
                self.logical.extend(ps);
                self.error = None;
            }
            if ui
                .add_enabled(!self.logical.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                self.logical.clear();
            }
        });
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.typed)
                    .hint_text("Or type a path")
                    .desired_width(400.0),
            );
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui
                .add_enabled(!self.typed.trim().is_empty(), egui::Button::new("Add Path"))
                .clicked()
                || enter)
                && !self.typed.trim().is_empty()
            {
                self.logical.push(PathBuf::from(self.typed.trim()));
                self.typed.clear();
                self.error = None;
            }
        });
        let mut remove = None;
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                if self.logical.is_empty() {
                    ui.weak("No files or folders selected.");
                }
                for (i, p) in self.logical.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").on_hover_text("Delete").clicked() {
                            remove = Some(i);
                        }
                        ui.label(p.display().to_string());
                    });
                }
            });
        if let Some(i) = remove {
            self.logical.remove(i);
        }
        ui.horizontal(|ui| {
            ui.label("Logical File Set Display Name:");
            ui.add(
                egui::TextEdit::singleline(&mut self.set_name)
                    .hint_text("Default (LogicalFileSet<N>)")
                    .desired_width(260.0),
            );
        });
        ui.label("Timestamps To Include:");
        ui.indent("times", |ui| {
            ui.checkbox(
                &mut self.times[0],
                "Modified Time - Often not changed when a file is copied",
            );
            ui.checkbox(
                &mut self.times[1],
                "Creation Time - Often changed when a file is copied",
            );
            ui.checkbox(
                &mut self.times[2],
                "Access Time - Can be changed when the file is opened",
            );
        });
        ui.weak("NOTE: Time stamps may have changed when the files were copied to the current location.");
    }

    fn folder_page(&mut self, ui: &mut egui::Ui, label: &str, title: &str) {
        ui.label(label);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.path)
                        .hint_text("Folder")
                        .desired_width(400.0),
                )
                .changed()
            {
                self.error = None;
            }
            if ui.button("Browse…").clicked()
                && let Some(p) = rfd::FileDialog::new().set_title(title).pick_folder()
            {
                self.path = p.display().to_string();
                self.error = None;
            }
        });
    }

    fn unalloc_page(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("add-unalloc")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                self.file_row(
                    ui,
                    "Unallocated space image file:",
                    "A raw file",
                    "Browse for an unallocated space image file",
                );
                self.timezone_row(ui);
                ui.label("Break image up into:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.chunked, false, "Do not break up");
                    ui.radio_value(&mut self.chunked, true, "2GB chunks");
                });
                ui.end_row();
            });
    }

    fn step_two(&mut self, ui: &mut egui::Ui) {
        modules_page(ui, &mut self.prefs);
    }
}

/// Autopsy's "Configure Ingest" page: modules with their settings and the
/// file filter (shared by the Add Data Source and Run Ingest Modules
/// wizards).
pub(crate) fn modules_page(ui: &mut egui::Ui, prefs: &mut IngestPrefs) {
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .min_scrolled_height(260.0)
        .show(ui, |ui| {
            let plugins = crate::plugins::current();
            for f in plugins.modules() {
                let info = f.info();
                let is_plugin = plugins.plugins.iter().any(|p| p.info.id == info.id);
                let mut on = prefs.is_enabled(&info);
                if ui
                    .checkbox(&mut on, egui::RichText::new(info.name).strong())
                    .changed()
                {
                    prefs.set_enabled(info.id, on);
                }
                ui.indent(info.id, |ui| {
                    ui.weak(info.description);
                    if info.id == "hash_lookup" && on {
                        hash_sets_ui(ui, &mut prefs.hash_lookup);
                    }
                    if is_plugin && on {
                        plugin_settings_ui(ui, prefs, info.id, &f.default_settings());
                    }
                });
                ui.add_space(4.0);
            }
        });
    ui.separator();
    ui.label("Run ingest modules on:");
    ui.radio_value(
        &mut prefs.ignore_unallocated,
        false,
        "All Files, Directories, and Unallocated Space",
    );
    ui.radio_value(
        &mut prefs.ignore_unallocated,
        true,
        "All Files and Directories (Not Unallocated Space)",
    );
}

/// A plugin's settings as JSON text (plugins have no settings panels of
/// their own yet).
fn plugin_settings_ui(
    ui: &mut egui::Ui,
    prefs: &mut IngestPrefs,
    id: &str,
    defaults: &serde_json::Value,
) {
    let text = prefs
        .module_settings
        .entry(id.to_owned())
        .or_insert_with(|| serde_json::to_string_pretty(defaults).unwrap_or_default());
    ui.label("Settings (JSON):");
    ui.add(
        egui::TextEdit::multiline(text)
            .code_editor()
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    if let Err(e) = serde_json::from_str::<serde_json::Value>(text) {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("Not valid JSON ({e}); the plugin's defaults will be used."),
        );
    }
}

/// What the Run Ingest Modules dialog asks for.
#[derive(Debug)]
pub(crate) enum RunAction {
    None,
    Cancel,
    Run(Box<(i64, JobSettings, IngestPrefs)>),
}

/// Autopsy's "Run Ingest Modules" wizard on an existing data source.
#[derive(Debug)]
pub(crate) struct RunIngestDialog {
    pub(crate) ds_id: i64,
    pub(crate) ds_name: String,
    pub(crate) prefs: IngestPrefs,
}

impl RunIngestDialog {
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> RunAction {
        let mut action = RunAction::None;
        egui::Modal::new(egui::Id::new("run-ingest")).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading("Run Ingest Modules");
            ui.label(format!("Configure ingest for {}", self.ds_name));
            ui.separator();
            modules_page(ui, &mut self.prefs);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    action = RunAction::Cancel;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Finish").clicked() {
                        action = RunAction::Run(Box::new((
                            self.ds_id,
                            self.prefs.job_settings(),
                            self.prefs.clone(),
                        )));
                    }
                });
            });
        });
        action
    }
}

use crate::options_hashsets::kind_name;

/// Hash Lookup's per-run settings: the hash sets and "calculate MD5".
fn hash_sets_ui(ui: &mut egui::Ui, s: &mut HashLookupSettings) {
    ui.checkbox(
        &mut s.calculate_hashes,
        "Calculate MD5 even if no hash set is selected",
    );
    let mut remove = None;
    egui::Grid::new("hash-sets")
        .num_columns(4)
        .striped(true)
        .show(ui, |ui| {
            for (i, set) in s.sets.iter_mut().enumerate() {
                ui.checkbox(&mut set.search_during_ingest, "");
                ui.label(set.name.clone().unwrap_or_else(|| {
                    set.path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                }))
                .on_hover_text(set.path.display().to_string());
                egui::ComboBox::from_id_salt(("hash-kind", i))
                    .selected_text(kind_name(set.kind))
                    .show_ui(ui, |ui| {
                        for k in [SetKind::Notable, SetKind::Known, SetKind::NoChange] {
                            ui.selectable_value(&mut set.kind, k, kind_name(k));
                        }
                    });
                if ui.small_button("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        s.sets.remove(i);
    }
    if s.sets.is_empty() {
        ui.weak("No hash sets: files are only hashed.");
    }
    if ui
        .button("Add Hash Set…")
        .on_hover_text(".kdb, NSRL (txt or RDS v3), md5sum, HashKeeper, EnCase or an hfind .idx")
        .clicked()
        && let Some(paths) = rfd::FileDialog::new()
            .set_title("Add Hash Set")
            .pick_files()
    {
        for path in paths {
            if !s.sets.iter().any(|x| x.path == path) {
                s.sets.push(HashSetConfig {
                    path,
                    name: None,
                    kind: SetKind::Notable,
                    search_during_ingest: true,
                    send_ingest_messages: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_map_to_job_settings() {
        let mut p = IngestPrefs::default();
        let s = p.job_settings();
        // Every module runs by default unless it is off by default.
        for f in pm_modules::core_modules() {
            let info = f.info();
            let m = s.modules.iter().find(|m| m.id == info.id).unwrap();
            assert_eq!(m.enabled, info.enabled_by_default, "{}", info.id);
        }
        assert!(!s.filter.ignore_unallocated);
        p.disabled.push("hash_lookup".into());
        p.ignore_unallocated = true;
        let s = p.job_settings();
        let h = s.modules.iter().find(|m| m.id == "hash_lookup").unwrap();
        assert!(!h.enabled);
        assert!(s.filter.ignore_unallocated);
        assert_eq!(h.settings["calculate_hashes"], serde_json::json!(true));
    }

    #[test]
    fn off_by_default_modules_need_an_explicit_choice() {
        let info = pm_ingest::ModuleInfo {
            id: "keyword_search",
            name: "Keyword Search",
            description: "",
            version: "1",
            enabled_by_default: false,
        };
        let mut p = IngestPrefs::default();
        assert!(!p.is_enabled(&info), "default off");
        p.set_enabled("keyword_search", true);
        assert!(p.is_enabled(&info));
        p.set_enabled("keyword_search", false);
        assert!(!p.is_enabled(&info));
        assert_eq!(p.enabled, Vec::<String>::new());
    }

    #[test]
    fn sources_must_exist() {
        let mut w = AddSourceWizard::new(IngestPrefs::default(), "UTC".into());
        assert!(w.spec().is_err());
        w.path = "/definitely/not/here.E01".into();
        assert!(w.spec().is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        w.path = f.path().display().to_string();
        assert!(matches!(
            w.spec(),
            Ok(pm_case::DataSourceSpec::DiskImage(_))
        ));
        assert!(w.zones.iter().any(|z| z == "Europe/Helsinki"));
        w.kind = SourceKind::LogicalFiles;
        assert!(w.spec().is_err(), "nothing selected");
        w.logical.push(f.path().to_path_buf());
        assert!(matches!(
            w.spec(),
            Ok(pm_case::DataSourceSpec::LogicalFiles { .. })
        ));
        w.kind = SourceKind::LocalDisk;
        assert!(w.spec().is_err(), "no disk selected");
        w.kind = SourceKind::Unallocated;
        assert!(matches!(
            w.spec(),
            Ok(pm_case::DataSourceSpec::UnallocatedImage { chunk: Some(_), .. })
        ));
    }
}
