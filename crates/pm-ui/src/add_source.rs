//! The Add Data Source wizard (Autopsy's image panel + "Configure Ingest"
//! page), and the ingest preferences it remembers.

use std::path::PathBuf;

use pm_case::LoadOptions;
use pm_ingest::{FileFilter, JobSettings, ModuleSetting};
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

    /// Job settings for the built-in modules.
    pub(crate) fn job_settings(&self) -> JobSettings {
        let modules = pm_modules::core_modules()
            .iter()
            .map(|f| {
                let info = f.info();
                let id = info.id;
                ModuleSetting {
                    id: id.into(),
                    enabled: self.is_enabled(&info),
                    settings: if id == "hash_lookup" {
                        serde_json::to_value(&self.hash_lookup).unwrap_or_default()
                    } else {
                        serde_json::Value::Null
                    },
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
    pub(crate) image: PathBuf,
    pub(crate) load: LoadOptions,
    pub(crate) settings: JobSettings,
    pub(crate) prefs: IngestPrefs,
}

/// The two-step Add Data Source wizard.
#[derive(Debug, Clone)]
pub(crate) struct AddSourceWizard {
    step: u8,
    pub(crate) path: String,
    pub(crate) timezone: String,
    pub(crate) prefs: IngestPrefs,
    pub(crate) error: Option<String>,
    zones: Vec<String>,
}

impl AddSourceWizard {
    pub(crate) fn new(prefs: IngestPrefs, timezone: String) -> Self {
        let mut zones: Vec<String> = jiff::tz::db()
            .available()
            .map(|n| n.as_str().to_owned())
            .collect();
        zones.sort();
        Self {
            step: 1,
            path: String::new(),
            timezone,
            prefs,
            error: None,
            zones,
        }
    }

    fn check_image(&self) -> Result<PathBuf, String> {
        let p = PathBuf::from(self.path.trim());
        if self.path.trim().is_empty() {
            return Err("Choose a disk image.".into());
        }
        if !p.is_file() {
            return Err(format!("{} is not a file.", p.display()));
        }
        Ok(p)
    }

    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> AddAction {
        let mut action = AddAction::None;
        let modal = egui::Modal::new(egui::Id::new("add-data-source")).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.heading("Add Data Source");
            ui.label(match self.step {
                1 => "Step 1 of 2 — Select Disk Image",
                _ => "Step 2 of 2 — Configure Ingest Modules",
            });
            ui.separator();
            if self.step == 1 {
                self.step_one(ui);
            } else {
                self.step_two(ui);
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    action = AddAction::Cancel;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.step == 1 {
                        if ui.button("Next ›").clicked() {
                            match self.check_image() {
                                Ok(_) => {
                                    self.error = None;
                                    self.step = 2;
                                }
                                Err(e) => self.error = Some(e),
                            }
                        }
                    } else {
                        if ui.button("Finish").clicked() {
                            match self.check_image() {
                                Ok(image) => {
                                    action = AddAction::Start(Box::new(StartRequest {
                                        image,
                                        load: LoadOptions {
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
                });
            });
        });
        if modal.should_close() && matches!(action, AddAction::None) {
            action = AddAction::Cancel;
        }
        action
    }

    fn step_one(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("add-image")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Disk image:");
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.path)
                                .hint_text("Raw (dd), split raw or E01 — first segment")
                                .desired_width(400.0),
                        )
                        .changed()
                    {
                        self.error = None;
                    }
                    if ui.button("Browse…").clicked()
                        && let Some(p) = rfd::FileDialog::new()
                            .set_title("Add Data Source — Disk Image")
                            .add_filter(
                                "Disk images",
                                &["dd", "raw", "img", "bin", "001", "000", "aa", "e01", "E01", "s01"],
                            )
                            .add_filter("All files", &["*"])
                            .pick_file()
                    {
                        self.path = p.display().to_string();
                        self.error = None;
                    }
                });
                ui.end_row();
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
            for f in pm_modules::core_modules() {
                let info = f.info();
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

fn kind_name(k: SetKind) -> &'static str {
    match k {
        SetKind::Known => "Known",
        SetKind::Notable => "Notable",
        SetKind::NoChange => "No Change",
    }
}

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
        .on_hover_text("TSK .kdb, NSRL (txt or RDS v3), md5sum, HashKeeper, EnCase or a TSK .idx")
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
    fn image_must_exist() {
        let mut w = AddSourceWizard::new(IngestPrefs::default(), "UTC".into());
        assert!(w.check_image().is_err());
        w.path = "/definitely/not/here.E01".into();
        assert!(w.check_image().is_err());
        let f = tempfile::NamedTempFile::new().unwrap();
        w.path = f.path().display().to_string();
        assert!(w.check_image().is_ok());
        assert!(w.zones.iter().any(|z| z == "Europe/Helsinki"));
    }
}
