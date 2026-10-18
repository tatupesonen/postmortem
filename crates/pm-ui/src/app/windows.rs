//! Tool windows (gallery, discovery, timeline, communications, geolocation, reports, file search, keyword search, options) and dialogs.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;

use crate::case_ui::WizardAction;
use crate::discovery::DiscoveryWindow;
use crate::gallery::GalleryWindow;
use crate::task::Task;
use crate::timeline::TimelineWindow;

use super::{PostmortemApp, ResultTab};

impl PostmortemApp {
    pub(super) fn open_gallery(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        if self.gallery.is_none() || self.gallery_closed.swap(false, Ordering::Relaxed) {
            let w = GalleryWindow::new(case, self.prefs.clone());
            self.gallery = Some(Arc::new(Mutex::new(w)));
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("gallery"),
            egui::ViewportCommand::Focus,
        );
    }

    pub(super) fn open_discovery(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        if self.discovery.is_none() || self.discovery_closed.swap(false, Ordering::Relaxed) {
            let w = DiscoveryWindow::new(case, self.prefs.clone());
            self.discovery = Some(Arc::new(Mutex::new(w)));
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("discovery"),
            egui::ViewportCommand::Focus,
        );
    }

    pub(super) fn show_discovery(&mut self, ctx: &egui::Context) {
        if self.discovery_closed.swap(false, Ordering::Relaxed) {
            self.discovery = None;
        }
        let Some(d) = self.discovery.clone() else {
            return;
        };
        if let Some(c) = d.lock().popout.take() {
            self.open_popout(c);
        }
        let closed = Arc::clone(&self.discovery_closed);
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("discovery"),
            egui::ViewportBuilder::default()
                .with_title("Discovery — postmortem")
                .with_inner_size([1350.0, 880.0]),
            move |ui, _| {
                d.lock().ui(ui);
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed.store(true, Ordering::Relaxed);
                }
            },
        );
    }

    pub(super) fn show_gallery(&mut self, ctx: &egui::Context) {
        if self.gallery_closed.swap(false, Ordering::Relaxed) {
            self.gallery = None;
        }
        let Some(g) = self.gallery.clone() else {
            return;
        };
        if let Some(c) = g.lock().popout.take() {
            self.open_popout(c);
        }
        let closed = Arc::clone(&self.gallery_closed);
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("gallery"),
            egui::ViewportBuilder::default()
                .with_title("Image Gallery — postmortem")
                .with_inner_size([1300.0, 850.0]),
            move |ui, _| {
                g.lock().ui(ui);
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed.store(true, Ordering::Relaxed);
                }
            },
        );
    }

    /// Opens (or focuses) the Timeline window, building events if needed.
    pub(super) fn open_timeline(
        &mut self,
        ctx: &egui::Context,
    ) -> Option<Arc<Mutex<TimelineWindow>>> {
        let case = self.analysis.lock().case().cloned()?;
        if self.timeline.is_none() || self.timeline_closed.swap(false, Ordering::Relaxed) {
            self.timeline = Some(Arc::new(Mutex::new(TimelineWindow::new(
                case,
                self.prefs.clone(),
            ))));
        }
        ctx.send_viewport_cmd_to(Self::timeline_viewport_id(), egui::ViewportCommand::Focus);
        self.timeline.clone()
    }

    pub(super) fn timeline_viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("timeline")
    }

    pub(super) fn show_timeline(&mut self, ctx: &egui::Context) {
        if self.timeline_closed.load(Ordering::Relaxed) {
            self.timeline = None;
            self.timeline_closed.store(false, Ordering::Relaxed);
        }
        let Some(tl) = self.timeline.clone() else {
            return;
        };
        let closed = Arc::clone(&self.timeline_closed);
        ctx.show_viewport_deferred(
            Self::timeline_viewport_id(),
            egui::ViewportBuilder::default()
                .with_title("Timeline — postmortem")
                .with_inner_size([1300.0, 850.0]),
            move |ui, _| {
                tl.lock().ui(ui);
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed.store(true, Ordering::Relaxed);
                }
            },
        );
    }

    /// Tools › Communications: loads the filter choices in the background.
    pub(super) fn open_communications(&mut self, ctx: &egui::Context) {
        if let Some(case) = self.analysis.lock().case().cloned() {
            self.cvt_choices = Some(Task::spawn(ctx, move || {
                crate::cvt_ui::Choices::load(&case)
            }));
        }
    }

    pub(super) fn communications(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            self.cvt = None;
            self.cvt_choices = None;
            return;
        };
        if let Some(t) = &mut self.cvt_choices
            && let Some(r) = t.poll()
        {
            match r {
                Ok(c) => self.cvt = Some(crate::cvt_ui::CvtWindow::new(c.clone())),
                Err(e) => self.errors.push(e.clone()),
            }
            self.cvt_choices = None;
        }
        if let Some(w) = &mut self.cvt
            && !w.ui(ctx, &case, &self.prefs)
        {
            self.cvt = None;
        }
    }

    /// Tools › Geolocation.
    pub(super) fn open_geolocation(&mut self, ctx: &egui::Context) {
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ctx, case.as_ref(), false);
        let sources = tree.iter().map(|t| (t.ds.id, t.ds.name.clone())).collect();
        self.geo = Some(crate::geo_ui::GeoWindow::new(sources));
    }

    pub(super) fn geolocation(&mut self, ctx: &egui::Context) {
        let Some(w) = &mut self.geo else {
            return;
        };
        let Some(case) = self.analysis.lock().case().cloned() else {
            self.geo = None;
            return;
        };
        if !w.ui(ctx, &case, &self.prefs) {
            self.geo = None;
        }
    }

    /// Tools › Generate Report: loads the case's choices in the background.
    pub(super) fn open_report_wizard(&mut self, ctx: &egui::Context) {
        if let Some(case) = self.analysis.lock().case().cloned() {
            self.report_choices = Some(Task::spawn(ctx, move || {
                crate::report_ui::Choices::load(&case)
            }));
        }
    }

    pub(super) fn report_wizard(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            self.report_wizard = None;
            self.report_choices = None;
            return;
        };
        if let Some(t) = &mut self.report_choices
            && let Some(r) = t.poll()
        {
            match r {
                Ok(choices) => {
                    self.report_wizard = Some(crate::report_ui::ReportWizard::new(
                        choices.clone(),
                        self.prefs.timezone.clone(),
                    ));
                }
                Err(e) => self.errors.push(e.clone()),
            }
            self.report_choices = None;
        }
        let Some(w) = &mut self.report_wizard else {
            return;
        };
        match w.ui(ctx, &case) {
            // The Reports node follows the change bus.
            crate::report_ui::WizardAction::None | crate::report_ui::WizardAction::Finished => {}
            crate::report_ui::WizardAction::Close => self.report_wizard = None,
        }
    }

    /// The File Search by Attributes window.
    pub(super) fn file_search_window(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            self.file_search = None;
            return;
        };
        if self.file_search.is_none() {
            return;
        }
        let sources: Vec<(i64, String, String)> = self
            .case_sources
            .get(ctx, Some(&case), false)
            .iter()
            .map(|t| (t.ds.id, t.ds.name.clone(), t.ds.timezone.clone()))
            .collect();
        let loader = Arc::clone(&case);
        let mimes = self
            .views
            .mime_types
            .get(ctx, move || {
                loader.db().mime_types().map_err(|e| e.to_string())
            })
            .unwrap_or_default();
        let Some(w) = self.file_search.as_mut() else {
            return;
        };
        match w.ui(ctx, &sources, &mimes) {
            crate::file_search_ui::SearchAction::None => {}
            crate::file_search_ui::SearchAction::Close => self.file_search = None,
            crate::file_search_ui::SearchAction::Search(search) => {
                self.searches.run.0 += 1;
                let number = self.searches.run.0;
                self.searches
                    .file
                    .push(crate::file_search_ui::SearchResults {
                        number,
                        rows: crate::task::Cached::default(),
                        search: Arc::from(search),
                        table: crate::table::TableState::default(),
                    });
                self.searches.tab = ResultTab::FileSearch(number);
            }
        }
    }

    /// The toolbar's Keyword Search panel.
    pub(super) fn keyword_panel(&mut self, ctx: &egui::Context) {
        use crate::keyword_search_ui::PanelAction;
        if self.keyword_panel.is_none() {
            return;
        }
        let (case, ingesting) = {
            let a = self.analysis.lock();
            (a.case().cloned(), a.any_active())
        };
        let Some(case) = case else {
            self.keyword_panel = None;
            return;
        };
        let tree = self.case_sources.get(ctx, Some(&case), false);
        let sources: Vec<(i64, String)> =
            tree.iter().map(|t| (t.ds.id, t.ds.name.clone())).collect();
        let loader = Arc::clone(&case);
        let indexed = self
            .keyword_indexed
            .get(ctx, move || {
                crate::keyword_search_ui::indexed_count(&loader)
            })
            .map(|n| *n);
        let Some(panel) = self.keyword_panel.as_mut() else {
            return;
        };
        match panel.ui(ctx, &sources, indexed, ingesting) {
            PanelAction::None => {}
            PanelAction::Close => self.keyword_panel = None,
            PanelAction::Search(req) => {
                // Autopsy's dropdown closes once the search starts.
                self.keyword_panel = None;
                self.searches.run.1 += 1;
                let number = self.searches.run.1;
                self.searches
                    .keyword
                    .push(crate::keyword_search_ui::KeywordResults {
                        number,
                        request: Arc::new(req),
                        rows: crate::task::Cached::default(),
                        table: crate::table::TableState::default(),
                    });
                self.searches.tab = ResultTab::KeywordSearch(number);
            }
        }
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context) {
        self.command_palette(ctx);
        self.pivot_window(ctx);
        self.file_search_window(ctx);
        self.keyword_panel(ctx);
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ctx, case.as_ref(), false);
        let errors = self.hosts_ui.ui(ctx, case.as_ref(), &tree);
        self.errors.extend(errors);
        if let Some(d) = self.run_ingest.as_mut() {
            match d.ui(ctx) {
                crate::add_source::RunAction::None => {}
                crate::add_source::RunAction::Cancel => self.run_ingest = None,
                crate::add_source::RunAction::Run(req) => {
                    let (ds_id, settings, prefs) = *req;
                    let name = d.ds_name.clone();
                    self.run_ingest = None;
                    self.persisted.ingest = prefs;
                    if let Err(e) = self.analysis.lock().run_ingest(ctx, ds_id, name, settings) {
                        self.errors.push(e);
                    }
                }
            }
        }
        self.remove_data_source_dialog(ctx);
        self.delete_case_dialog(ctx);
        if let Some(w) = self.add_wizard.as_mut() {
            match w.ui(ctx) {
                crate::add_source::AddAction::None => {}
                crate::add_source::AddAction::Cancel => self.add_wizard = None,
                crate::add_source::AddAction::Start(req) => {
                    let crate::add_source::StartRequest {
                        spec,
                        load,
                        settings,
                        prefs,
                    } = *req;
                    self.add_wizard = None;
                    self.persisted.ingest = prefs;
                    self.add_data_source_with(ctx, spec, load, settings);
                }
            }
        }
        // One lock at a time: the guard of an `if let` scrutinee lives
        // through its block.
        let (case, active) = {
            let analysis = self.analysis.lock();
            (analysis.case().cloned(), analysis.any_active())
        };
        if let Some(case) = case {
            let busy = active || self.add_wizard.is_some() || self.case_wizard.is_some();
            self.tz_check.ui(ctx, &case, busy);
        }
        if let Some(w) = self.case_wizard.as_mut() {
            match w.ui(ctx) {
                WizardAction::None => {}
                WizardAction::Cancel => self.case_wizard = None,
                WizardAction::Create { dir, details } => {
                    match self.create_case(ctx, &dir, details) {
                        Ok(()) => self.case_wizard = None,
                        Err(e) => {
                            if let Some(w) = self.case_wizard.as_mut() {
                                w.error = Some(e);
                            }
                        }
                    }
                }
            }
        }
        if self.about_open {
            let wordmark = self.wordmark(ctx);
            let r = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                if let Some(tex) = &wordmark {
                    ui.add(egui::Image::new(tex).max_width(320.0));
                    ui.add_space(8.0);
                }
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A digital forensics platform written in Rust,");
                ui.label("inspired by Autopsy®.");
                ui.add_space(8.0);
                ui.button("Close").clicked()
            });
            if r.inner || r.should_close() {
                self.about_open = false;
            }
        }
        if self.shortcuts_open {
            let r = egui::Modal::new(egui::Id::new("shortcuts")).show(ctx, |ui| {
                ui.heading("Keyboard Shortcuts");
                ui.add_space(4.0);
                let fmt = |m: egui::Modifiers, k: egui::Key| {
                    ctx.format_shortcut(&egui::KeyboardShortcut::new(m, k))
                };
                let cmd = egui::Modifiers::COMMAND;
                let rows: Vec<(String, &str)> = vec![
                    (
                        fmt(cmd, egui::Key::K),
                        "Command palette: tools, nodes, files, file:N / artifact:N",
                    ),
                    (fmt(cmd, egui::Key::O), "Add Data Source"),
                    (fmt(egui::Modifiers::ALT, egui::Key::ArrowLeft), "Back"),
                    (fmt(egui::Modifiers::ALT, egui::Key::ArrowRight), "Forward"),
                    ("↑ ↓  /  J K".into(), "Previous / next row"),
                    ("Shift+↑ ↓".into(), "Extend the selection"),
                    ("Ctrl/⌘+click, Shift+click".into(), "Select several rows"),
                    (fmt(cmd, egui::Key::A), "Select all rows"),
                    (fmt(cmd, egui::Key::F), "Filter the table by name"),
                    (fmt(cmd, egui::Key::B), "Bookmark the selection"),
                    ("1 … 9".into(), "Apply the numbered tag (see Add Tag menus)"),
                    (
                        fmt(cmd, egui::Key::E),
                        "Open the viewed file in the external viewer",
                    ),
                    (format!("{} … 9", fmt(cmd, egui::Key::Num1)), "Result tabs"),
                    ("Enter".into(), "Open the selected row"),
                ];
                egui::Grid::new("shortcut-list")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (keys, what) in &rows {
                            ui.monospace(keys);
                            ui.label(*what);
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
                ui.button("Close").clicked()
            });
            if r.inner || r.should_close() {
                self.shortcuts_open = false;
            }
        }
        if !self.errors.is_empty() {
            let r = egui::Modal::new(egui::Id::new("errors")).show(ctx, |ui| {
                ui.heading("Problem");
                for e in &self.errors {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.button("OK").clicked()
            });
            if r.inner || r.should_close() {
                self.errors.clear();
            }
        }
    }

    /// Tools › Options.
    pub fn open_options(&mut self) {
        let p = &self.persisted;
        let case_root = p
            .case_root
            .clone()
            .unwrap_or_else(crate::analysis::default_case_root);
        self.options = Some(crate::options_ui::OptionsWindow::new(
            crate::options_ui::Settings {
                local_time: p.local_time,
                hide_known_in_views: p.hide_known_in_views,
                custom_tags: p.custom_tags.clone(),
                hash_lookup: p.ingest.hash_lookup.clone(),
            },
            // `~/postmortem/HashSets`, beside the cases folder.
            case_root
                .parent()
                .map_or_else(|| case_root.clone(), std::path::Path::to_path_buf)
                .join("HashSets"),
        ));
    }

    pub(super) fn show_options(&mut self, ctx: &egui::Context) {
        use crate::options_ui::OptionsAction;
        let Some(w) = &mut self.options else {
            return;
        };
        let running = self.analysis.lock().any_active();
        let (settings, close) = match w.ui(ctx, running) {
            OptionsAction::None => return,
            OptionsAction::Close => (None, true),
            OptionsAction::Apply(s) => (Some(s), false),
            OptionsAction::ApplyAndClose(s) => (Some(s), true),
        };
        if close {
            self.options = None;
        }
        let Some(s) = settings else {
            return;
        };
        if s.local_time != self.persisted.local_time {
            self.persisted.local_time = s.local_time;
            self.apply_time_pref();
        }
        if s.hide_known_in_views != self.persisted.hide_known_in_views {
            self.persisted.hide_known_in_views = s.hide_known_in_views;
            self.views.counts.invalidate();
            self.views.rows.clear();
        }
        if s.custom_tags != self.persisted.custom_tags {
            let case = self.analysis.lock().case().cloned();
            if let Some(case) = case {
                self.tagging
                    .apply_definitions(ctx, &case, s.custom_tags.clone());
            }
            self.persisted.custom_tags = s.custom_tags;
        }
        self.persisted.ingest.hash_lookup = s.hash_lookup;
    }

    /// The Pivot window: rows open their file or result.
    fn pivot_window(&mut self, ctx: &egui::Context) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            self.pivot = None;
            return;
        };
        let Some(p) = self.pivot.as_mut() else {
            return;
        };
        let (chosen, keep) = p.ui(ctx, &case);
        if !keep {
            self.pivot = None;
        }
        match chosen {
            Some(crate::pivot::Open::File(id)) => self.reveal_file(id),
            Some(crate::pivot::Open::Artifact(id)) => self.reveal_artifact(id),
            None => {}
        }
    }
}
