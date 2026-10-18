//! Menu bar, toolbar, status bar and keyboard shortcuts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::model::NodeId;
use crate::task::{self, Task};

use super::PostmortemApp;

/// Number keys 1..9.
const DIGITS: [egui::Key; 9] = [
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

impl PostmortemApp {
    /// The wordmark texture for the current theme (decoded on first use and
    /// again when the theme flips).
    pub(super) fn wordmark(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let dark = ctx.theme() == egui::Theme::Dark;
        if self.wordmark.as_ref().is_none_or(|(d, _)| *d != dark) {
            let png = if dark {
                crate::WORDMARK_ON_DARK_PNG
            } else {
                crate::WORDMARK_ON_LIGHT_PNG
            };
            let img = image::load_from_memory(png).ok()?.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            let tex = ctx.load_texture("wordmark", color, egui::TextureOptions::LINEAR);
            self.wordmark = Some((dark, tex));
        }
        self.wordmark.as_ref().map(|(_, t)| t.clone())
    }

    // ------------------------------------------------------------ panels

    pub(super) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Case", |ui| {
                let has_case = self.has_case();
                if ui.button("New Case…").clicked() {
                    ui.close();
                    self.new_case_wizard();
                }
                if ui.button("Open Case…").clicked() {
                    ui.close();
                    self.pick_case(ui.ctx());
                }
                ui.menu_button("Open Recent Case", |ui| {
                    if self.persisted.recent_cases.is_empty() {
                        ui.weak("None");
                    }
                    for p in self.persisted.recent_cases.clone() {
                        if ui.button(p.display().to_string()).clicked() {
                            ui.close();
                            if let Err(e) = self.open_case(ui.ctx(), &p) {
                                self.errors.push(e);
                            }
                        }
                    }
                });
                if ui
                    .add_enabled(has_case, egui::Button::new("Close Case"))
                    .clicked()
                {
                    ui.close();
                    self.close_case(ui.ctx());
                }
                let case = self.analysis.lock().case().cloned();
                if ui
                    .add_enabled(has_case, egui::Button::new("Open Case Folder"))
                    .clicked()
                {
                    if let Some(c) = &case {
                        crate::platform::open(c.dir());
                    }
                    ui.close();
                }
                let busy = self.analysis.lock().any_active();
                if ui
                    .add_enabled(has_case && !busy, egui::Button::new("Delete Case"))
                    .on_disabled_hover_text("Cannot delete a case while ingest is running")
                    .clicked()
                {
                    if let Some(c) = &case {
                        self.confirm_delete_case = Some((c.dir().to_path_buf(), c.details().name));
                    }
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(has_case, egui::Button::new("Add Data Source…"))
                    .clicked()
                {
                    ui.close();
                    self.pick_image(ui.ctx());
                }
                if ui
                    .add_enabled(has_case, egui::Button::new("Manage Hosts"))
                    .clicked()
                {
                    self.hosts_ui.open_manage();
                    ui.close();
                }
                ui.add_enabled_ui(has_case, |ui| {
                    ui.menu_button("Add Recent Image", |ui| {
                        if self.persisted.recent.is_empty() {
                            ui.weak("None");
                        }
                        for p in self.persisted.recent.clone() {
                            if ui.button(p.display().to_string()).clicked() {
                                ui.close();
                                self.add_data_source(ui.ctx(), p);
                            }
                        }
                    });
                });
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                if ui
                    .add_enabled(!self.back.is_empty(), egui::Button::new("Back"))
                    .clicked()
                {
                    self.go_back();
                    ui.close();
                }
                if ui
                    .add_enabled(!self.forward.is_empty(), egui::Button::new("Forward"))
                    .clicked()
                {
                    self.go_forward();
                    ui.close();
                }
                ui.separator();
                ui.label("Theme");
                for (t, n) in [
                    (egui::ThemePreference::System, "System"),
                    (egui::ThemePreference::Light, "Light"),
                    (egui::ThemePreference::Dark, "Dark"),
                ] {
                    if ui.radio(self.persisted.theme == t, n).clicked() {
                        self.persisted.theme = t;
                        ui.ctx().set_theme(t);
                    }
                }
                ui.separator();
                if ui
                    .checkbox(
                        &mut self.persisted.local_time,
                        "Show times in local time zone",
                    )
                    .changed()
                {
                    self.apply_time_pref();
                }
                if ui
                    .checkbox(
                        &mut self.persisted.hide_known_in_views,
                        "Hide known files in Views",
                    )
                    .changed()
                {
                    self.views.counts.invalidate();
                    self.views.rows.clear();
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui
                    .add(egui::Button::new("⌕ Command Palette…").shortcut_text(
                        ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
                            egui::Modifiers::COMMAND,
                            egui::Key::K,
                        )),
                    ))
                    .clicked()
                {
                    self.palette.open();
                    ui.close();
                }
                if ui
                    .add_enabled(self.has_case(), egui::Button::new("📝 Notes"))
                    .clicked()
                {
                    self.notes
                        .get_or_insert_with(crate::notes_ui::NotesWindow::default);
                    ui.close();
                }
                ui.separator();
                if ui.button("⚙ Ingest Progress").clicked() {
                    self.progress_open = true;
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.has_case(),
                        egui::Button::new("🔎 File Search by Attributes"),
                    )
                    .clicked()
                {
                    self.file_search
                        .get_or_insert_with(crate::file_search_ui::FileSearchWindow::default);
                    self.views.mime_types.refresh();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(self.has_data_sources(), egui::Button::new("⏱ Timeline"))
                    .clicked()
                {
                    self.open_timeline(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        self.has_data_sources(),
                        egui::Button::new("🖼 Image Gallery"),
                    )
                    .clicked()
                {
                    self.open_gallery(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(self.has_data_sources(), egui::Button::new("🔍 Discovery"))
                    .clicked()
                {
                    self.open_discovery(ui.ctx());
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(self.has_case(), egui::Button::new("📄 Generate Report"))
                    .clicked()
                {
                    self.open_report_wizard(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(self.has_case(), egui::Button::new("📞 Communications"))
                    .clicked()
                {
                    self.open_communications(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(self.has_case(), egui::Button::new("🌍 Geolocation"))
                    .clicked()
                {
                    self.open_geolocation(ui.ctx());
                    ui.close();
                }
                if ui.button("🧩 Plugins").clicked() {
                    self.plugins_open = true;
                    ui.close();
                }
                if ui.button("⚙ Options").clicked() {
                    self.open_options();
                    ui.close();
                }
            });
            ui.menu_button("Window", |ui| {
                let content = self.viewer.content().cloned();
                if ui
                    .add_enabled(
                        content.is_some(),
                        egui::Button::new("Pop Out Content Viewer"),
                    )
                    .clicked()
                {
                    if let Some(c) = content {
                        self.open_popout(c);
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !self.popouts.is_empty(),
                        egui::Button::new("Close All Pop-out Windows"),
                    )
                    .clicked()
                {
                    self.popouts.clear();
                    ui.close();
                }
            });
            ui.menu_button("Help", |ui| {
                let case = self.analysis.lock().case().cloned();
                if ui
                    .add_enabled(case.is_some(), egui::Button::new("Open Log Folder"))
                    .clicked()
                {
                    if let Some(c) = &case {
                        crate::platform::open(&c.log_dir());
                    }
                    ui.close();
                }
                if ui.button("Keyboard Shortcuts").clicked() {
                    self.shortcuts_open = true;
                    ui.close();
                }
                if ui.button("About postmortem").clicked() {
                    self.about_open = true;
                    ui.close();
                }
            });
        });
    }

    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("🗁 New Case")
                .on_hover_text("Create a case (Case › New Case…)")
                .clicked()
            {
                self.new_case_wizard();
            }
            if ui
                .button("📂 Open Case")
                .on_hover_text("Open an existing case (Case › Open Case…)")
                .clicked()
            {
                self.pick_case(ui.ctx());
            }
            if ui
                .add_enabled(self.has_case(), egui::Button::new("➕ Add Data Source"))
                .on_hover_text("Add a disk image to the case (Ctrl/⌘+O)")
                .on_disabled_hover_text("Create or open a case first")
                .clicked()
            {
                self.pick_image(ui.ctx());
            }
            ui.separator();
            if ui
                .add_enabled(!self.back.is_empty(), egui::Button::new("⏴"))
                .on_hover_text("Back (Alt+←)")
                .clicked()
            {
                self.go_back();
            }
            if ui
                .add_enabled(!self.forward.is_empty(), egui::Button::new("⏵"))
                .on_hover_text("Forward (Alt+→)")
                .clicked()
            {
                self.go_forward();
            }
            ui.separator();
            if ui
                .add_enabled(self.has_data_sources(), egui::Button::new("⏱ Timeline"))
                .on_hover_text("Open the Timeline (Tools › Timeline)")
                .clicked()
            {
                self.open_timeline(ui.ctx());
            }
            if ui
                .add_enabled(self.has_data_sources(), egui::Button::new("🖼 Gallery"))
                .on_hover_text("Open the Image Gallery (Tools › Image Gallery)")
                .clicked()
            {
                self.open_gallery(ui.ctx());
            }
            if ui
                .add_enabled(self.has_data_sources(), egui::Button::new("🔍 Discovery"))
                .on_hover_text("Find files by type, size and location (Tools › Discovery)")
                .clicked()
            {
                self.open_discovery(ui.ctx());
            }
            ui.separator();
            let has = self.viewer.content().is_some();
            if ui
                .add_enabled(has, egui::Button::new("⧉ Pop Out Viewer"))
                .on_hover_text("Show the current file in its own window")
                .clicked()
                && let Some(c) = self.viewer.content().cloned()
            {
                self.open_popout(c);
            }
            if ui
                .add_enabled(self.has_case(), egui::Button::new("📞 Communications"))
                .on_hover_text("Browse accounts and their messages, calls and contacts (Tools › Communications)")
                .clicked()
            {
                self.open_communications(ui.ctx());
            }
            if ui
                .add_enabled(self.has_case(), egui::Button::new("🌍 Geolocation"))
                .on_hover_text("Show located results on a map (Tools › Geolocation)")
                .clicked()
            {
                self.open_geolocation(ui.ctx());
            }
            if ui
                .add_enabled(self.has_case(), egui::Button::new("📄 Generate Report"))
                .on_hover_text("Generate a report about the case (Tools › Generate Report)")
                .clicked()
            {
                self.open_report_wizard(ui.ctx());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let open = self.keyword_panel.is_some();
                if ui
                    .add_enabled(
                        self.has_case(),
                        egui::Button::selectable(open, "🔍 Keyword Search"),
                    )
                    .on_hover_text("Search the text of indexed files for a word, substring or regular expression")
                    .clicked()
                {
                    if open {
                        self.keyword_panel = None;
                    } else {
                        self.keyword_panel = Some(crate::keyword_search_ui::KeywordSearchPanel::default());
                        self.keyword_indexed.refresh();
                    }
                }
            });
        });
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let mut open_progress = false;
        ui.horizontal(|ui| {
            ui.label(format!("{} data source(s)", self.case_sources.count()));
            {
                let analysis = self.analysis.lock();
                for job in analysis.jobs.iter().filter(|j| j.is_active()) {
                    ui.separator();
                    open_progress |= crate::analysis::status_line(ui, job);
                }
                if !analysis.any_active()
                    && let Some(job) = analysis.jobs.last()
                {
                    ui.separator();
                    let text = match job.stage() {
                        crate::analysis::Stage::Done {
                            artifacts,
                            cancelled,
                            ..
                        } => format!(
                            "{} {} — {} results",
                            if cancelled {
                                "⏹ Analysis cancelled:"
                            } else {
                                "✔ Analysed"
                            },
                            job.name,
                            crate::format::grouped(artifacts)
                        ),
                        crate::analysis::Stage::Failed(e) => format!("⛔ {}: {e}", job.name),
                        _ => String::new(),
                    };
                    open_progress |= ui
                        .add(
                            egui::Label::new(text)
                                .truncate()
                                .sense(egui::Sense::click()),
                        )
                        .on_hover_text("Show ingest progress and messages")
                        .clicked();
                }
            }
            let running = task::running();
            if running > 0 {
                ui.separator();
                ui.spinner();
                ui.label(format!("{running} background task(s)"));
            }
            if let Some(c) = self.viewer.content() {
                ui.separator();
                ui.label(egui::RichText::new(&c.row.path).monospace());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(if self.persisted.local_time {
                    "Times: local"
                } else {
                    "Times: UTC"
                });
            });
        });
        if open_progress {
            self.progress_open = true;
        }
        if self.analysis.lock().any_active() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    pub(super) fn show_progress(&mut self, ctx: &egui::Context) {
        if self.progress_closed.swap(false, Ordering::Relaxed) {
            self.progress_open = false;
        }
        if !self.progress_open {
            return;
        }
        let analysis = Arc::clone(&self.analysis);
        let closed = Arc::clone(&self.progress_closed);
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("ingest-progress"),
            egui::ViewportBuilder::default()
                .with_title("Ingest Progress — postmortem")
                .with_inner_size([820.0, 620.0]),
            move |ui, _| {
                egui::CentralPanel::default().show(ui, |ui| {
                    crate::analysis::details_ui(ui, &analysis.lock());
                });
                if analysis.lock().any_active() {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                }
                if ui.ctx().input(|i| i.viewport().close_requested()) {
                    closed.store(true, Ordering::Relaxed);
                }
            },
        );
    }

    /// What Ctrl+B and the tag keys act on: the selected results, else
    /// the selected files, else the viewed file.
    fn shortcut_target(&mut self) -> Option<crate::tags_ui::TagTarget> {
        let result = match (&self.selected, self.result_selected) {
            (Some(NodeId::Results { type_id }), Some(i)) => self
                .results
                .rows
                .get_mut(&crate::results::ResultKey::Type(*type_id))
                .and_then(Task::poll)
                .and_then(|r| r.as_ref().ok())
                .and_then(|rows| rows.rows.get(i))
                .map(|r| r.artifact.id),
            _ => None,
        };
        let viewed = self.viewer.content().and_then(|c| c.row.file_id());
        match result {
            Some(a) if self.marked_results.len() > 1 && self.marked_results.contains(&a) => Some(
                crate::tags_ui::TagTarget::Artifacts(self.marked_results.clone()),
            ),
            Some(a) => Some(crate::tags_ui::TagTarget::Artifacts(vec![a])),
            None if self.marked_files.len() > 1
                && viewed.is_none_or(|v| self.marked_files.contains(&v)) =>
            {
                Some(crate::tags_ui::TagTarget::Files(self.marked_files.clone()))
            }
            None => viewed.map(|f| crate::tags_ui::TagTarget::Files(vec![f])),
        }
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (open, back, fwd, bookmark, palette, external, tab, quick) = ctx.input_mut(|i| {
            let digit = |i: &mut egui::InputState, m: egui::Modifiers| {
                DIGITS.iter().position(|k| i.consume_key(m, *k))
            };
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight),
                i.consume_shortcut(&crate::tags_ui::bookmark_shortcut()),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::K),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::E),
                digit(i, egui::Modifiers::COMMAND),
                if typing {
                    None
                } else {
                    digit(i, egui::Modifiers::NONE)
                },
            )
        });
        if palette {
            self.palette.open();
        }
        // Ctrl+1..9: the result tabs (Listing, then the searches).
        if let Some(n) = tab
            && let Some(t) = self.result_tab_list().get(n)
        {
            self.searches.tab = *t;
        }
        let case = if bookmark || quick.is_some() || external {
            self.analysis.lock().case().cloned()
        } else {
            None
        };
        if let Some(case) = case {
            // Ctrl+E: the viewed file in the external viewer.
            if external && let Some(c) = self.viewer.content().cloned() {
                self.open_external(ctx, &case, c);
            }
            // Ctrl+B bookmarks, 1..9 apply the numbered tags of the Add
            // Tag menu, to the selection (results before files).
            if let Some(target) = self.shortcut_target() {
                if bookmark {
                    self.tagging.bookmark(ctx, &case, target);
                } else if let Some(n) = quick {
                    let cat = self.tagging.catalog.get(ctx, Some(&case));
                    if let Some(t) = crate::tags_ui::quick_tags(&cat).get(n) {
                        self.tagging.request(
                            ctx,
                            &case,
                            target,
                            crate::tags_ui::TagRequest::Apply(t.id),
                        );
                    }
                }
            }
        }
        if open && self.has_case() {
            self.pick_image(ctx);
        }
        if back {
            self.go_back();
        }
        if fwd {
            self.go_forward();
        }
        // Dropping image files onto the window adds them as data sources.
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        // Dropping a case opens it; dropping images adds them to the case.
        for p in dropped {
            if crate::case_ui::is_case_path(&p) {
                if let Err(e) = self.open_case(ctx, &p) {
                    self.errors.push(e);
                }
            } else {
                self.add_data_source(ctx, p);
            }
        }
    }
}
