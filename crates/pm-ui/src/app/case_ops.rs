//! Case and data-source lifecycle, and refreshes driven by the case change bus.

use std::path::PathBuf;
use std::sync::Arc;

use crate::case_ui::NewCaseWizard;
use crate::model::Content;
use crate::task::Task;

use super::{PostmortemApp, ResultTab};

impl PostmortemApp {
    /// Adds a disk image to the open case: it is loaded into the case
    /// database and analysed in the background, and opened for browsing.
    /// Uses the remembered ingest settings (see the Add Data Source wizard).
    pub fn add_data_source(&mut self, ctx: &egui::Context, path: PathBuf) {
        let ingest = &self.persisted.ingest;
        let load = pm_case::LoadOptions {
            timezone: Self::default_timezone(),
            fat_orphans: !ingest.ignore_fat_orphans,
            process_unallocated: !ingest.ignore_unallocated,
            ..pm_case::LoadOptions::default()
        };
        let settings = ingest.job_settings();
        self.add_data_source_with(
            ctx,
            pm_case::DataSourceSpec::DiskImage(path),
            load,
            settings,
        );
    }

    pub(super) fn default_timezone() -> String {
        jiff::tz::TimeZone::system()
            .iana_name()
            .unwrap_or("UTC")
            .to_owned()
    }

    pub(super) fn add_data_source_with(
        &mut self,
        ctx: &egui::Context,
        spec: pm_case::DataSourceSpec,
        load: pm_case::LoadOptions,
        settings: pm_ingest::JobSettings,
    ) {
        let recent = match &spec {
            pm_case::DataSourceSpec::DiskImage(p) => Some(p.clone()),
            _ => None,
        };
        if let Err(e) = self.analysis.lock().start(ctx, spec, load, settings) {
            self.errors.push(e);
            return;
        }
        if let Some(path) = recent {
            self.persisted.recent.retain(|r| r != &path);
            self.persisted.recent.insert(0, path);
            self.persisted.recent.truncate(10);
        }
    }

    /// Whether a case is open.
    pub fn has_case(&self) -> bool {
        self.analysis.lock().case().is_some()
    }

    /// Creates a case and makes it the open case.
    pub fn create_case(
        &mut self,
        ctx: &egui::Context,
        dir: &std::path::Path,
        details: pm_case::CaseDetails,
    ) -> Result<(), String> {
        let case = pm_case::Case::create(dir, details)
            .map_err(|e| format!("Cannot create case in {}: {e}", dir.display()))?;
        self.set_case(ctx, case);
        Ok(())
    }

    /// Opens an existing case (its folder or `case.pmcase`) and re-opens
    /// its data sources for browsing; their analysis results are kept.
    pub fn open_case(&mut self, ctx: &egui::Context, path: &std::path::Path) -> Result<(), String> {
        let case = pm_case::Case::open(path)
            .map_err(|e| format!("Cannot open case {}: {e}", path.display()))?;
        let sources = case.db().data_sources().map_err(|e| e.to_string())?;
        self.set_case(ctx, case);
        for ds in sources {
            match ds.paths.first() {
                Some(p) if std::path::Path::new(p).is_file() => {}
                _ => self.errors.push(format!(
                    "Data source {} was not found at {}. It may have been moved.",
                    ds.name,
                    ds.paths.first().map_or("(no path)", String::as_str)
                )),
            }
        }
        Ok(())
    }

    pub(super) fn set_case(&mut self, ctx: &egui::Context, case: pm_case::Case) {
        self.close_case(ctx);
        let dir = case.dir().to_path_buf();
        let title = format!("{} — postmortem", case.details().name);
        let case = Arc::new(case);
        self.tagging
            .apply_definitions(ctx, &case, self.persisted.custom_tags.clone());
        self.analysis.lock().set_case(case);
        self.persisted.recent_cases.retain(|r| r != &dir);
        self.persisted.recent_cases.insert(0, dir);
        self.persisted.recent_cases.truncate(10);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Closes the case and everything shown from it.
    pub fn close_case(&mut self, ctx: &egui::Context) {
        self.analysis.lock().close();
        self.change_watch.reset();
        self.tz_check.clear();
        self.listings.clear();
        self.selected = None;
        self.back.clear();
        self.forward.clear();
        self.viewer.set_content(None);
        self.timeline = None;
        self.gallery = None;
        self.discovery = None;
        self.result_counts.invalidate();
        self.result_rows.clear();
        self.result_selected = None;
        self.view_counts.invalidate();
        self.view_rows.clear();
        self.case_sources.invalidate();
        self.case_summary.clear();
        self.node_views.clear();
        self.account_details = None;
        self.tagging = crate::tags_ui::Tagging::default();
        self.file_search = None;
        self.mime_types.clear();
        self.searches.clear();
        self.keyword_panel = None;
        self.keyword_indexed.clear();
        self.keyword_searches.clear();
        self.result_tab = ResultTab::Listing;
        self.searches_run = (0, 0);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("postmortem".into()));
    }

    pub(super) fn new_case_wizard(&mut self) {
        let base = self
            .persisted
            .case_root
            .clone()
            .unwrap_or_else(crate::analysis::default_case_root);
        self.case_wizard = Some(NewCaseWizard::new(&base));
    }

    pub(super) fn pick_case(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Open Case")
            .add_filter("postmortem case", &["pmcase"])
            .pick_file();
        if let Some(p) = picked
            && let Err(e) = self.open_case(ctx, &p)
        {
            self.errors.push(e);
        }
    }

    pub(super) fn poll_loading(&mut self) {
        self.exports.retain_mut(|t| match t.poll() {
            Some(Ok(_)) => false,
            Some(Err(e)) => {
                self.errors.push(e.clone());
                false
            }
            None => true,
        });
    }

    /// Opens the Add Data Source wizard (requires an open case).
    pub(super) fn pick_image(&mut self, _ctx: &egui::Context) {
        if !self.has_case() {
            self.errors
                .push("Create or open a case before adding a data source.".into());
            return;
        }
        self.add_wizard = Some(crate::add_source::AddSourceWizard::new(
            self.persisted.ingest.clone(),
            Self::default_timezone(),
        ));
    }

    /// Autopsy's "Remove Data Source" confirmation and the removal task.
    pub(super) fn remove_data_source_dialog(&mut self, ctx: &egui::Context) {
        if let Some(t) = &mut self.removing
            && let Some(r) = t.poll()
        {
            if let Err(e) = r {
                self.errors.push(e.clone());
            }
            self.removing = None;
            // The change bus refreshes the views; the selection may be gone.
            self.selected = None;
            self.back.clear();
            self.forward.clear();
            self.viewer.set_content(None);
        }
        let Some((id, name)) = self.confirm_remove.clone() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::new("remove-ds")).show(ctx, |ui| {
            ui.heading("Remove Data Source");
            ui.label(format!(
                "Are you sure you want to remove {name} from the case?\nThe data source and its analysis results will be removed from the case database; the image file is not deleted."
            ));
            ui.horizontal(|ui| {
                if ui.button("Remove").clicked() {
                    if let Some(case) = self.analysis.lock().case().cloned() {
                        self.removing = Some(Task::spawn(ctx, move || {
                            case.delete_data_source(id).map_err(|e| e.to_string())
                        }));
                    }
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.confirm_remove = None;
        }
    }

    /// Autopsy's "Delete Case" confirmation: closes the case, then
    /// removes its folder in the background.
    pub(super) fn delete_case_dialog(&mut self, ctx: &egui::Context) {
        let Some((dir, name)) = self.confirm_delete_case.clone() else {
            return;
        };
        let mut close = false;
        egui::Modal::new(egui::Id::new("delete-case")).show(ctx, |ui| {
            ui.heading("Delete Case");
            ui.label(format!(
                "Are you sure you want to close and delete the case {name}?\nThe case database, index, reports and exported files in\n{}\nwill be deleted. Evidence images are not deleted.",
                dir.display()
            ));
            ui.horizontal(|ui| {
                if ui.button("Delete").clicked() {
                    self.close_case(ctx);
                    self.persisted.recent_cases.retain(|p| p != &dir && p.parent() != Some(dir.as_path()));
                    let dir = dir.clone();
                    self.exports.push(Task::spawn(ctx, move || {
                        pm_case::delete_case_folder(&dir).map_err(|e| e.to_string())?;
                        Ok(dir.display().to_string())
                    }));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.confirm_delete_case = None;
        }
    }

    /// Lets the content viewer show hashes, MIME and known status of case
    /// files, re-read from the database (they change during ingest).
    pub(super) fn update_hash_resolver(&mut self) {
        let case = self.analysis.lock().case().cloned();
        let key = case.as_ref().map(|c| c.dir().to_path_buf());
        if key == self.resolver_key {
            return;
        }
        self.resolver_key = key;
        let Some(case) = case else {
            self.prefs.case = None;
            self.viewer.refresh_hashes();
            return;
        };
        let resolver = move |c: &Content| -> Option<crate::viewers::CaseFileInfo> {
            let db = case.db();
            let f = match &c.origin {
                // Re-read the row: hashes may have been computed since it
                // was listed.
                crate::model::ContentOrigin::Case(_) => db.file(c.row.file_id()?).ok()??,
                crate::model::ContentOrigin::Fs(_) => return None,
            };
            let hex = |v: Option<Vec<u8>>| v.map(|b| pm_core::decode::to_hex(&b));
            Some(crate::viewers::CaseFileInfo {
                case: Arc::clone(&case),
                md5: hex(f.md5.clone()),
                sha1: hex(f.sha1.clone()),
                sha256: hex(f.sha256.clone()),
                known: match f.known {
                    pm_casedb::Known::Known => "Known".into(),
                    pm_casedb::Known::Notable => "Notable".into(),
                    pm_casedb::Known::Unknown => "Unknown".into(),
                },
                mime: f.mime.clone(),
                record: Arc::new(f),
            })
        };
        self.prefs.case = Some(crate::viewers::CaseLookup(Arc::new(resolver)));
        self.viewer.refresh_hashes();
    }

    /// Refreshes what changed in the case database since the last frame
    /// (ADR-023 change bus): counts while writes go on, tables once they
    /// settle.
    pub(super) fn apply_case_changes(&mut self, ctx: &egui::Context, case: &Arc<pm_case::Case>) {
        use crate::changes::Watch;
        let changes = std::sync::Arc::clone(case.db().changes());
        let live = self.analysis.lock().any_active();
        for watch in Watch::ALL {
            let due = self.change_watch.poll(&changes, watch, live);
            if !due.counts {
                continue;
            }
            match watch {
                Watch::Files => {
                    self.view_counts.invalidate();
                    if due.rows {
                        self.listings.clear();
                        self.view_rows.clear();
                        self.node_views.clear();
                        self.viewer.refresh_hashes();
                        for s in &mut self.searches {
                            s.rows.refresh();
                        }
                    }
                }
                Watch::Results => {
                    self.result_counts.invalidate();
                    if due.rows {
                        self.tz_check.refresh();
                    }
                    self.keyword_groups.refresh();
                    self.case_summary.refresh();
                    if due.rows {
                        self.result_rows.clear();
                        self.node_views.clear();
                    }
                }
                Watch::Annotations => {
                    self.case_summary.refresh();
                    if due.rows {
                        self.listings.clear();
                        self.view_rows.clear();
                        self.result_rows.clear();
                        self.node_views.clear();
                        for s in &mut self.searches {
                            s.rows.refresh();
                        }
                        for s in &mut self.keyword_searches {
                            s.rows.refresh();
                        }
                    }
                }
                Watch::Sources => {
                    self.case_sources.refresh();
                    self.tz_check.refresh();
                    if due.rows {
                        self.listings.clear();
                    }
                }
                Watch::Reports => self.case_summary.refresh(),
                Watch::Timeline => {
                    if due.rows
                        && let Some(t) = &self.timeline
                    {
                        t.lock().invalidate();
                    }
                }
            }
        }
        if self.change_watch.pending() {
            ctx.request_repaint_after(crate::changes::LIVE_REFRESH);
        }
    }
}
