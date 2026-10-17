//! Options › Hash Sets (Autopsy `HashLookupSettingsPanel` with its
//! `HashDbCreateDatabaseDialog` and `HashDbImportDatabaseDialog`): the
//! hash sets every new ingest job starts with.
//!
//! Set details (format, size, index state) and indexing run in the
//! background; nothing here reads a set on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pm_hashdb::{HashDb, HashDbError};
use pm_modules::hash_lookup::{HashLookupSettings, HashSetConfig, SetKind};

use crate::task::Task;

/// What the details pane shows for a set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetInfo {
    pub(crate) format: String,
    /// Hashes (records for indexed sets); `None` until indexed.
    pub(crate) hashes: Option<u64>,
    pub(crate) indexed: bool,
    pub(crate) updateable: bool,
    /// Looked up through a sorted index postmortem builds.
    pub(crate) needs_index: bool,
}

fn set_info(path: &Path) -> Result<SetInfo, String> {
    let format = pm_hashdb::detect(path).map_err(|e| e.to_string())?;
    match HashDb::open(path, true) {
        Ok(db) => Ok(SetInfo {
            format: format.name().to_owned(),
            hashes: db.len().ok(),
            indexed: true,
            updateable: format == pm_hashdb::Format::Kdb,
            needs_index: format.needs_index(),
        }),
        Err(HashDbError::NeedsIndex(_)) => Ok(SetInfo {
            format: format.name().to_owned(),
            hashes: None,
            indexed: false,
            updateable: false,
            needs_index: true,
        }),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn kind_name(k: SetKind) -> &'static str {
    match k {
        SetKind::Notable => "Notable",
        SetKind::Known => "Known",
        SetKind::NoChange => "No Change",
    }
}

/// An index build in progress.
#[derive(Debug)]
struct Indexing {
    path: PathBuf,
    progress: Arc<AtomicU64>,
    task: Task<Result<u64, String>>,
}

/// Create Hash Set.
#[derive(Debug)]
struct CreateDialog {
    name: String,
    kind: SetKind,
    dir: PathBuf,
    error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct HashSetsPage {
    /// Where New Hash Set puts sets by default.
    default_dir: PathBuf,
    selected: Option<PathBuf>,
    info: HashMap<PathBuf, Task<Result<SetInfo, String>>>,
    indexing: Option<Indexing>,
    create: Option<CreateDialog>,
}

impl HashSetsPage {
    pub(crate) fn new(default_dir: PathBuf) -> Self {
        Self {
            default_dir,
            ..Self::default()
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, s: &mut HashLookupSettings) {
        ui.label(
            "Hash sets are used to identify known and notable files. New ingest jobs \
             search the sets enabled here; each job can still change them.",
        );
        ui.add_space(6.0);
        self.poll_indexing(ui.ctx());
        let mut remove = None;
        ui.columns(2, |cols| {
            let [left, right] = cols else {
                return;
            };
            left.label("Hash Sets:");
            egui::Frame::group(left.style()).show(left, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("hash-set-list")
                    .min_scrolled_height(260.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if s.sets.is_empty() {
                            ui.weak("None");
                        }
                        for set in &s.sets {
                            let on = self.selected.as_ref() == Some(&set.path);
                            if ui.selectable_label(on, set.display_name()).clicked() {
                                self.selected = Some(set.path.clone());
                            }
                        }
                    });
            });
            left.horizontal(|ui| {
                if ui.button("New Hash Set…").clicked() {
                    self.create = Some(CreateDialog {
                        name: String::new(),
                        kind: SetKind::Notable,
                        dir: self.default_dir.clone(),
                        error: None,
                    });
                }
                if ui
                    .button("Import Hash Set…")
                    .on_hover_text(
                        "TSK .kdb, NSRL (txt or RDS v3), md5sum, HashKeeper, EnCase or a TSK .idx",
                    )
                    .clicked()
                    && let Some(paths) = rfd::FileDialog::new()
                        .set_title("Import Hash Set")
                        .pick_files()
                {
                    for path in paths {
                        self.selected = Some(path.clone());
                        add_set(s, path, SetKind::Notable);
                    }
                }
                if ui
                    .add_enabled(
                        self.selected.is_some(),
                        egui::Button::new("Remove Hash Set"),
                    )
                    .on_hover_text("Removes the set from the list; the file is kept")
                    .clicked()
                {
                    remove = self.selected.take();
                }
            });
            let Some(set) = self
                .selected
                .as_ref()
                .and_then(|p| s.sets.iter_mut().find(|x| &x.path == p))
            else {
                right.weak("Select a hash set to see its details.");
                return;
            };
            self.details(right, set);
        });
        if let Some(p) = remove {
            s.sets.retain(|x| x.path != p);
            self.info.remove(&p);
        }
        self.create_ui(ui.ctx(), s);
    }

    fn details(&mut self, ui: &mut egui::Ui, set: &mut HashSetConfig) {
        let ctx = ui.ctx().clone();
        let path = set.path.clone();
        let info = self.info.entry(path.clone()).or_insert_with(|| {
            let p = path.clone();
            Task::spawn(&ctx, move || set_info(&p))
        });
        let info = info.poll().cloned();
        ui.strong("Hash Set Details");
        egui::Grid::new("hash-set-details")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("Name:");
                ui.label(set.display_name());
                ui.end_row();
                ui.label("Location:");
                ui.label(path.display().to_string());
                ui.end_row();
                match &info {
                    None => {
                        ui.label("Type:");
                        ui.spinner();
                        ui.end_row();
                    }
                    Some(Err(e)) => {
                        ui.label("Status:");
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        ui.end_row();
                    }
                    Some(Ok(i)) => {
                        ui.label("Type:");
                        ui.label(&i.format);
                        ui.end_row();
                        ui.label("Hashes:");
                        ui.label(i.hashes.map_or_else(|| "—".into(), crate::format::grouped));
                        ui.end_row();
                        ui.label("Index status:");
                        ui.label(if i.indexed { "Indexed" } else { "Not indexed" });
                        ui.end_row();
                        ui.label("Updateable:");
                        ui.label(if i.updateable { "Yes" } else { "No" });
                        ui.end_row();
                    }
                }
                ui.label("Set type:");
                egui::ComboBox::from_id_salt("hash-set-kind")
                    .selected_text(kind_name(set.kind))
                    .show_ui(ui, |ui| {
                        for k in [SetKind::Notable, SetKind::Known, SetKind::NoChange] {
                            ui.selectable_value(&mut set.kind, k, kind_name(k));
                        }
                    });
                ui.end_row();
            });
        ui.checkbox(&mut set.search_during_ingest, "Search during ingest");
        let mut messages = set
            .send_ingest_messages
            .unwrap_or(set.kind == SetKind::Notable);
        if ui
            .checkbox(&mut messages, "Send ingest inbox message for each hit")
            .changed()
        {
            set.send_ingest_messages = Some(messages);
        }
        ui.add_space(6.0);
        match &self.indexing {
            Some(ix) if ix.path == path => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Indexing… {} entries read",
                        crate::format::grouped(ix.progress.load(Ordering::Relaxed))
                    ));
                });
            }
            busy => {
                let needs = matches!(&info, Some(Ok(i)) if !i.indexed);
                let label = if needs { "Index" } else { "Re-index" };
                let indexable = matches!(&info, Some(Ok(i)) if i.needs_index);
                if ui
                    .add_enabled(busy.is_none() && indexable, egui::Button::new(label))
                    .on_hover_text("Builds the sorted index postmortem looks hashes up in")
                    .clicked()
                {
                    let progress = Arc::new(AtomicU64::new(0));
                    let (p, pr) = (path.clone(), Arc::clone(&progress));
                    self.indexing = Some(Indexing {
                        path,
                        progress,
                        task: Task::spawn(&ctx, move || {
                            HashDb::build_index(&p, &pr).map_err(|e| e.to_string())
                        }),
                    });
                }
            }
        }
    }

    fn poll_indexing(&mut self, ctx: &egui::Context) {
        let Some(ix) = &mut self.indexing else {
            return;
        };
        if ix.task.poll().is_some() {
            if let Some(Err(e)) = ix.task.poll() {
                tracing::warn!("indexing {}: {e}", ix.path.display());
            }
            self.info.remove(&ix.path);
            self.indexing = None;
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    fn create_ui(&mut self, ctx: &egui::Context, s: &mut HashLookupSettings) {
        let Some(d) = &mut self.create else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::new("create-hash-set")).show(ctx, |ui| {
            ui.heading("Create Hash Set");
            egui::Grid::new("create-hash-set-grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Hash Set Name:");
                    ui.text_edit_singleline(&mut d.name);
                    ui.end_row();
                    ui.label("Type:");
                    ui.horizontal(|ui| {
                        for k in [SetKind::Known, SetKind::Notable, SetKind::NoChange] {
                            ui.radio_value(&mut d.kind, k, kind_name(k));
                        }
                    });
                    ui.end_row();
                    ui.label("Folder:");
                    ui.horizontal(|ui| {
                        ui.label(d.dir.display().to_string());
                        if ui.button("Browse…").clicked()
                            && let Some(dir) =
                                rfd::FileDialog::new().set_directory(&d.dir).pick_folder()
                        {
                            d.dir = dir;
                        }
                    });
                    ui.end_row();
                });
            if let Some(e) = &d.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    match create_set(&d.dir, &d.name) {
                        Ok(path) => {
                            add_set(s, path.clone(), d.kind);
                            self.selected = Some(path);
                            close = true;
                        }
                        Err(e) => d.error = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.create = None;
        }
    }
}

/// Creates an empty `.kdb` set named `name` in `dir`.
pub(crate) fn create_set(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Hash set name cannot be empty.".into());
    }
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
        return Err("Hash set name cannot contain / \\ : * ? \" < > |".into());
    }
    let path = dir.join(format!("{name}.kdb"));
    if path.exists() {
        return Err(format!("{} already exists.", path.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    HashDb::create(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn add_set(s: &mut HashLookupSettings, path: PathBuf, kind: SetKind) {
    if !s.sets.iter().any(|x| x.path == path) {
        s.sets.push(HashSetConfig {
            path,
            name: None,
            kind,
            search_during_ingest: true,
            send_ingest_messages: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_sets_are_updateable_kdbs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create_set(dir.path(), " ").is_err());
        assert!(create_set(dir.path(), "a/b").is_err());
        let p = create_set(dir.path(), "Case hits").unwrap();
        assert_eq!(p, dir.path().join("Case hits.kdb"));
        assert!(create_set(dir.path(), "Case hits").is_err(), "no overwrite");
        let info = set_info(&p).unwrap();
        assert!(info.updateable && info.indexed);
        assert_eq!(info.hashes, Some(0));
    }
}
