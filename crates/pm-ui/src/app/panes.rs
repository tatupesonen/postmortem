//! The result pane: listings, tables, result tabs, summaries and row actions.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::case_ui::WelcomeAction;
use crate::format::size_long;
use crate::model::{Content, FileRow, NodeId};
use crate::summary;
use crate::table::RowMenus;
use crate::task::Task;
use crate::viewers::ViewerPanel;
use crate::views::View;

use super::{Listing, Popout, PostmortemApp, ResultTab, add_to_hash_set, copy_out};

/// File and keyword searches, each in its own result tab.
#[derive(Debug)]
pub(super) struct Searches {
    pub(super) file: Vec<crate::file_search_ui::SearchResults>,
    pub(super) keyword: Vec<crate::keyword_search_ui::KeywordResults>,
    /// Searches run in this case, for numbering (Autopsy never reuses one).
    pub(super) run: (usize, usize),
    pub(super) tab: ResultTab,
}

impl Default for Searches {
    fn default() -> Self {
        Self {
            file: Vec::new(),
            keyword: Vec::new(),
            run: (0, 0),
            tab: ResultTab::Listing,
        }
    }
}

impl Searches {
    /// Reacts to committed case changes.
    pub(super) fn on_change(&mut self, watch: crate::changes::Watch, due: crate::changes::Due) {
        use crate::changes::Watch;
        if !due.rows {
            return;
        }
        match watch {
            Watch::Files => {
                for s in &mut self.file {
                    s.rows.refresh();
                }
            }
            Watch::Annotations => {
                for s in &mut self.file {
                    s.rows.refresh();
                }
                for s in &mut self.keyword {
                    s.rows.refresh();
                }
            }
            _ => {}
        }
    }
}

impl PostmortemApp {
    /// Directory listing for a node, loaded in the background from the
    /// case database.
    pub(super) fn listing(&mut self, ctx: &egui::Context, node: &NodeId) -> Option<&Listing> {
        if !self.listings.contains_key(node) {
            let case = self.analysis.lock().case().cloned()?;
            let source = match node {
                NodeId::Dir { file, .. } => Ok(*file),
                NodeId::TopFiles(ds) => Err(*ds),
                _ => return None,
            };
            let task = Task::spawn(ctx, move || {
                let db = case.db();
                let records = match source {
                    Ok(dir) => db.children(dir),
                    // Folders are in the tree; the table lists the files.
                    Err(ds) => db.top_level_files(ds).map(|v| {
                        v.into_iter()
                            .filter(|f| f.kind != pm_casedb::FileKindCode::Directory)
                            .collect()
                    }),
                };
                let mut rows: Vec<FileRow> = records
                    .map_err(|e| e.to_string())?
                    .iter()
                    .map(crate::views::row_of)
                    .collect();
                crate::tags_ui::annotate(&case, &mut rows)?;
                Ok(Arc::new(rows))
            });
            self.listings.insert(node.clone(), task);
        }
        self.listings.get_mut(node).and_then(Task::poll)
    }

    pub(super) fn open_popout(&mut self, content: Content) {
        let mut panel = ViewerPanel::default();
        let title = format!("{} — postmortem", content.row.name);
        panel.set_content(Some(content));
        self.next_popout += 1;
        self.popouts.push(Popout {
            id: egui::ViewportId::from_hash_of(("popout", self.next_popout)),
            title,
            panel: Arc::new(Mutex::new(panel)),
            closed: Arc::new(AtomicBool::new(false)),
        });
    }

    pub(super) fn export(&mut self, ctx: &egui::Context, content: Content) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Extract File")
            .set_file_name(content.row.name.replace(':', "_"))
            .save_file()
        else {
            return;
        };
        self.exports.push(Task::spawn(ctx, move || {
            let src = content.source()?;
            copy_out(src.as_ref(), &dest)?;
            Ok(dest.display().to_string())
        }));
    }

    /// Extract Files: the selected files into a chosen folder (names made
    /// unique with the file's id when they repeat).
    pub(super) fn export_many(&mut self, ctx: &egui::Context, files: Vec<Content>) {
        let Some(dir) = rfd::FileDialog::new()
            .set_title(format!("Extract {} Files to Folder", files.len()))
            .pick_folder()
        else {
            return;
        };
        self.exports.push(Task::spawn(ctx, move || {
            let mut used = std::collections::HashSet::new();
            for c in &files {
                let base = c.row.name.replace([':', '/', '\\'], "_");
                let name = if used.insert(base.clone()) {
                    base
                } else {
                    let id = c.row.file_id().unwrap_or_default();
                    let (stem, ext) = base
                        .rsplit_once('.')
                        .map_or((base.as_str(), String::new()), |(s, e)| {
                            (s, format!(".{e}"))
                        });
                    format!("{stem}_{id}{ext}")
                };
                let src = c.source()?;
                copy_out(src.as_ref(), &dir.join(name))?;
            }
            Ok(format!(
                "{} files extracted to {}",
                files.len(),
                dir.display()
            ))
        }));
    }

    /// Row actions every file table shares: tags, pop-out, extract,
    /// external viewer, Add File to Hash Set and CSV export (`csv`, built
    /// by the caller from its table).
    pub(super) fn file_table_actions(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<pm_case::Case>,
        rows: &[FileRow],
        ev: &crate::table::TableEvents,
        csv: Option<String>,
    ) {
        // The table's selection, for shortcuts (Ctrl+B) that act on it.
        self.marked_files = ev
            .marked
            .iter()
            .filter_map(|&i| rows.get(i).and_then(FileRow::file_id))
            .collect();
        if let Some((sel, req)) = &ev.tag {
            let ids: Vec<i64> = sel
                .iter()
                .filter_map(|&i| rows.get(i).and_then(FileRow::file_id))
                .collect();
            if !ids.is_empty() {
                self.tagging
                    .request(ctx, case, crate::tags_ui::TagTarget::Files(ids), *req);
            }
        }
        let content = |r: &FileRow| Content::from_case(Arc::clone(case), r.clone());
        if let Some(r) = ev.popout.and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
        }
        let to_export: Vec<Content> = ev
            .export
            .iter()
            .filter_map(|&i| rows.get(i))
            .filter(|r| !r.is_dir())
            .map(content)
            .collect();
        match to_export.len() {
            0 => {}
            1 => {
                if let Some(c) = to_export.into_iter().next() {
                    self.export(ctx, c);
                }
            }
            _ => self.export_many(ctx, to_export),
        }
        if let Some(r) = ev.external.and_then(|i| rows.get(i)) {
            self.open_external(ctx, case, content(r));
        }
        if let Some((sel, set)) = &ev.add_hash {
            let files: Vec<Content> = sel
                .iter()
                .filter_map(|&i| rows.get(i))
                .filter(|r| !r.is_dir() && r.file_id().is_some())
                .map(content)
                .collect();
            let set = set.clone();
            self.exports.push(Task::spawn(ctx, move || {
                let mut done = Vec::new();
                for c in &files {
                    done.push(add_to_hash_set(c, &set)?);
                }
                Ok(match done.as_slice() {
                    [one] => one.clone(),
                    _ => format!("{} files added to {}", done.len(), set.display()),
                })
            }));
        }
        if let Some(csv) = csv {
            self.save_csv(ctx, csv);
        }
    }

    /// Save Table as CSV.
    pub(super) fn save_csv(&mut self, ctx: &egui::Context, csv: String) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Export Table to CSV")
            .add_filter("CSV", &["csv"])
            .set_file_name("table.csv")
            .save_file()
        else {
            return;
        };
        self.exports.push(Task::spawn(ctx, move || {
            std::fs::write(&dest, csv).map_err(|e| format!("{}: {e}", dest.display()))?;
            Ok(dest.display().to_string())
        }));
    }

    /// Open in External Viewer (Autopsy `ExternalViewerAction`): the file
    /// is extracted to the case's `Temp/external` folder and handed to the
    /// operating system's default application for its type.
    pub(super) fn open_external(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<pm_case::Case>,
        content: Content,
    ) {
        let dir = case.dir().join("Temp").join("external");
        self.exports.push(Task::spawn(ctx, move || {
            let id = content.row.file_id().unwrap_or_default();
            let name: String = content
                .row
                .name
                .chars()
                .map(|c| {
                    if matches!(c, '/' | '\\' | ':') {
                        '_'
                    } else {
                        c
                    }
                })
                .collect();
            let dest = dir.join(format!("{id}-{name}"));
            std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if !dest.exists() {
                let src = content.source()?;
                copy_out(src.as_ref(), &dest)?;
            }
            crate::platform::open(&dest);
            Ok(dest.display().to_string())
        }));
    }

    pub(super) fn view_table(&mut self, ui: &mut egui::Ui, view: &View) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let hide = self.persisted.hide_known_in_views;
        let task = self.views.rows.entry(view.clone()).or_insert_with(|| {
            let (case, view) = (Arc::clone(&case), view.clone());
            Task::spawn(ui.ctx(), move || {
                crate::views::load_rows(&case, &view, hide).map(Arc::new)
            })
        });
        let data = match task.poll() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading files…");
                });
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(d)) => Arc::clone(d),
        };
        ui.horizontal(|ui| {
            ui.strong(view.title());
            ui.weak(format!(
                "{} files{}",
                crate::format::grouped(data.rows.len() as u64),
                if data.truncated {
                    format!(
                        " (first {} shown)",
                        crate::format::grouped(crate::views::MAX_ROWS as u64)
                    )
                } else {
                    String::new()
                }
            ));
        });
        let case = self.analysis.lock().case().cloned();
        let tags = self.tagging.catalog.get(ui.ctx(), case.as_ref());
        let menus = RowMenus {
            tags: Some(&tags),
            hash_sets: &self.persisted.ingest.hash_lookup.sets,
        };
        let ev = self.table.ui(ui, &data.rows, &self.prefs, menus);
        let csv = (ev.csv || ev.csv_selection).then(|| {
            self.table
                .csv(&data.rows, &self.prefs, None, ev.csv_selection)
        });
        if let Some(case) = &case {
            self.file_table_actions(ui.ctx(), case, &data.rows, &ev, csv);
        }
        if let Some(rec) = ev.selected.and_then(|i| data.records.get(i)) {
            let content = self.content_for(rec);
            self.viewer.set_content(content);
        }
        if let Some(rec) = ev.activated.and_then(|i| data.records.get(i))
            && let Some(c) = self.content_for(rec)
        {
            self.open_popout(c);
        }
    }

    /// The table of an OS Accounts, Tags or Score node.
    pub(super) fn node_view(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let loader = Arc::clone(&case);
        let prefs = self.prefs.clone();
        let target = node.clone();
        let data =
            self.nodes
                .views
                .entry(node.clone())
                .or_default()
                .get(&ctx, move || match target {
                    NodeId::TagFiles(t) => crate::case_nodes::load_tag_files(&loader, t, &prefs),
                    NodeId::TagResults(t) => crate::case_nodes::load_tag_results(&loader, t),
                    NodeId::Score { bad } => crate::case_nodes::load_score(&loader, bad),
                    _ => crate::case_nodes::load_os_accounts(&loader, &prefs),
                });
        let Some(data) = data else {
            if let Some(e) = self.nodes.views.get(node).and_then(|c| c.error()) {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            } else {
                ui.spinner();
            }
            return;
        };
        ui.weak(format!(
            "{} items",
            crate::format::grouped(data.refs.len() as u64)
        ));
        let mut remove: Option<(crate::tags_ui::TagTarget, i64)> = None;
        let refs = &data.refs;
        let details_h = if matches!(node, NodeId::OsAccounts) {
            220.0
        } else {
            0.0
        };
        let ev = ui
            .allocate_ui(
                egui::vec2(ui.available_width(), ui.available_height() - details_h),
                |ui| {
                    self.nodes.grid.ui(ui, "node-view", &data.grid, |ui, i| {
                        let (target, tag) = match refs.get(i) {
                            Some(crate::case_nodes::RowRef::File {
                                file,
                                tag_id: Some(t),
                            }) => (crate::tags_ui::TagTarget::Files(vec![file.id]), *t),
                            Some(crate::case_nodes::RowRef::Artifact {
                                artifact_id,
                                tag_id: Some(t),
                                ..
                            }) => (crate::tags_ui::TagTarget::Artifacts(vec![*artifact_id]), *t),
                            _ => {
                                ui.weak("No actions");
                                return;
                            }
                        };
                        if ui.button("Remove Selected Tag(s)").clicked() {
                            remove = Some((target, tag));
                            ui.close();
                        }
                    })
                },
            )
            .inner;
        if let Some((target, tag)) = remove {
            self.tagging.request(
                &ctx,
                &case,
                target,
                crate::tags_ui::TagRequest::Remove {
                    tag_id: tag,
                    tag_name_id: None,
                },
            );
        }
        let file_of = |r: &crate::case_nodes::RowRef| match r {
            crate::case_nodes::RowRef::File { file, .. } => Some(file.clone()),
            crate::case_nodes::RowRef::Artifact { file, .. } => file.clone(),
            crate::case_nodes::RowRef::OsAccount(_) => None,
        };
        if let Some(r) = ev.selected.and_then(|i| refs.get(i)) {
            if let crate::case_nodes::RowRef::OsAccount(id) = r {
                self.nodes.account_details = Some((*id, crate::task::Cached::default()));
            } else if let Some(f) = file_of(r) {
                self.viewer.set_content(Some(Content::from_case(
                    Arc::clone(&case),
                    crate::views::row_of(&f),
                )));
            }
        }
        if let Some(f) = ev.activated.and_then(|i| refs.get(i)).and_then(file_of) {
            self.open_popout(Content::from_case(
                Arc::clone(&case),
                crate::views::row_of(&f),
            ));
        }
        if let Some((id, details)) = &mut self.nodes.account_details {
            let (id, loader, prefs) = (*id, Arc::clone(&case), self.prefs.clone());
            ui.separator();
            match details.get(&ctx, move || {
                crate::case_nodes::account_details(&loader, id, &prefs)
            }) {
                None => {
                    ui.spinner();
                }
                Some(sections) => {
                    egui::ScrollArea::vertical()
                        .id_salt("account-details")
                        .show(ui, |ui| {
                            for (title, rows) in sections.iter() {
                                ui.strong(*title);
                                egui::Grid::new(*title).num_columns(2).striped(true).show(
                                    ui,
                                    |ui| {
                                        for (k, v) in rows {
                                            ui.label(k);
                                            ui.label(v);
                                            ui.end_row();
                                        }
                                    },
                                );
                            }
                        });
                }
            }
        }
    }

    pub(super) fn results(&mut self, ui: &mut egui::Ui) {
        if self.has_case() {
            self.result_tabs(ui);
        }
        match self.searches.tab {
            ResultTab::Listing => {}
            ResultTab::FileSearch(n) => return self.search_view(ui, n),
            ResultTab::KeywordSearch(n) => return self.keyword_view(ui, n),
        }
        let Some(node) = self.selected.clone() else {
            let wordmark = self.wordmark(ui.ctx());
            if !self.has_case() {
                let recent = self.persisted.recent_cases.clone();
                match crate::case_ui::welcome(ui, wordmark.as_ref(), &recent) {
                    WelcomeAction::None => {}
                    WelcomeAction::NewCase => self.new_case_wizard(),
                    WelcomeAction::OpenCase => self.pick_case(ui.ctx()),
                    WelcomeAction::OpenRecent(p) => {
                        if let Err(e) = self.open_case(ui.ctx(), &p) {
                            self.errors.push(e);
                        }
                    }
                }
                return;
            }
            let mut add = false;
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 150.0).max(0.0));
                if let Some(tex) = &wordmark {
                    ui.add(egui::Image::new(tex).max_width(420.0));
                }
                ui.add_space(18.0);
                add = ui
                    .add(
                        egui::Button::new(egui::RichText::new("➕  Add Data Source").size(16.0))
                            .min_size(egui::vec2(220.0, 36.0)),
                    )
                    .clicked();
                ui.add_space(8.0);
                ui.weak("Add a disk image to the case to begin, or drop one here.");
            });
            if add {
                self.pick_image(ui.ctx());
            }
            return;
        };
        match &node {
            NodeId::DataSource(ds) => self.data_source_summary(ui, *ds),
            NodeId::Host(h) => self.host_view(ui, *h),
            NodeId::Volume { ds, volume } => self.volume_summary(ui, *ds, *volume),
            NodeId::Dir { .. } | NodeId::TopFiles(_) => self.file_table(ui, &node),
            NodeId::Results { type_id } => {
                self.results_view(ui, &crate::results::ResultKey::Type(*type_id));
            }
            NodeId::KeywordHits(f) => {
                self.results_view(ui, &crate::results::ResultKey::Keyword(f.clone()));
            }
            NodeId::TagFiles(_)
            | NodeId::TagResults(_)
            | NodeId::Score { .. }
            | NodeId::OsAccounts => self.node_view(ui, &node),
            NodeId::View(v) => self.view_table(ui, v),
        }
    }

    pub(super) fn results_view(&mut self, ui: &mut egui::Ui, key: &crate::results::ResultKey) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let task = self.results.rows.entry(key.clone()).or_insert_with(|| {
            let (case, key) = (Arc::clone(&case), key.clone());
            Task::spawn(ui.ctx(), move || {
                crate::results::load_rows(&case, &key).map(Arc::new)
            })
        });
        let rows = match task.poll() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading results…");
                });
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(r)) => Arc::clone(r),
        };
        // The title comes from the cached counts (no IO on the UI thread).
        let title = match key {
            crate::results::ResultKey::Type(type_id) => self
                .results
                .counts
                .get(ui.ctx(), Some(&case), false)
                .iter()
                .find(|t| t.type_id == *type_id)
                .map(|t| t.display.clone())
                .unwrap_or_default(),
            crate::results::ResultKey::Keyword(f) => {
                let mut t = format!("Keyword Hits › {}", f.list);
                for part in [&f.term, &f.instance].into_iter().flatten() {
                    t.push_str(" › ");
                    t.push_str(part);
                }
                t
            }
        };
        let tags = self.tagging.catalog.get(ui.ctx(), Some(&case));
        let ev = self.result_table.ui(
            ui,
            &title,
            &rows,
            &self.prefs,
            self.result_selected,
            Some(&tags),
        );
        self.marked_results = ev
            .marked
            .iter()
            .filter_map(|&i| rows.rows.get(i).map(|r| r.artifact.id))
            .collect();
        if let Some((sel, which, req)) = ev.tag {
            let picked = sel.iter().filter_map(|&i| rows.rows.get(i));
            let target = match which {
                crate::results::TagFor::Result => {
                    crate::tags_ui::TagTarget::Artifacts(picked.map(|r| r.artifact.id).collect())
                }
                crate::results::TagFor::SourceFile => {
                    let mut files: Vec<i64> = picked
                        .filter_map(|r| r.file.as_ref().map(|f| f.id))
                        .collect();
                    files.sort_unstable();
                    files.dedup();
                    crate::tags_ui::TagTarget::Files(files)
                }
            };
            self.tagging.request(ui.ctx(), &case, target, req);
        }
        if let Some(i) = ev.clicked {
            self.result_selected = Some(i);
            let row = rows.rows.get(i);
            let content = row
                .and_then(|r| r.file.as_ref())
                .and_then(|f| self.content_for(f));
            self.viewer
                .show(content, row.and_then(crate::results::keyword_query));
        }
    }

    /// The evidence-backed content of a case file (for the viewers).
    pub(super) fn content_for(&self, f: &pm_casedb::FileRecord) -> Option<Content> {
        let case = self.analysis.lock().case().cloned()?;
        Some(Content::from_case(case, crate::views::row_of(f)))
    }

    pub(super) fn file_table(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let ctx = ui.ctx().clone();
        let Some(ds) = node.ds() else {
            return;
        };
        let rows = match self.listing(&ctx, node).cloned() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading directory…");
                });
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
            Some(Ok(r)) => r,
        };
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        if let Some(dir) = rows.first().and_then(|r| r.file.as_ref()) {
            ui.horizontal(|ui| {
                ui.strong("Listing");
                ui.monospace(&dir.parent_path);
            });
        }
        ui.weak(format!("{} items", rows.len()));
        let tags = self.tagging.catalog.get(&ctx, Some(&case));
        let menus = RowMenus {
            tags: Some(&tags),
            hash_sets: &self.persisted.ingest.hash_lookup.sets,
        };
        let ev = self.table.ui(ui, &rows, &self.prefs, menus);
        let csv = (ev.csv || ev.csv_selection)
            .then(|| self.table.csv(&rows, &self.prefs, None, ev.csv_selection));
        self.file_table_actions(&ctx, &case, &rows, &ev, csv);
        let content = |r: &FileRow| Content::from_case(Arc::clone(&case), r.clone());
        if let Some(r) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.set_content(Some(content(r)));
        }
        if let Some(r) = ev.activated.and_then(|i| rows.get(i)) {
            if r.can_open() {
                if let Some(file) = r.file_id() {
                    self.navigate(NodeId::Dir { ds, file });
                }
            } else {
                self.open_popout(content(r));
            }
        }
        if let Some(r) = ev.timeline.and_then(|i| rows.get(i)) {
            let t = r
                .meta
                .as_ref()
                .and_then(|m| m.times.modified.or(m.times.created))
                .map(jiff::Timestamp::as_second);
            if let (Some(t), Some(tl)) = (t, self.open_timeline(&ctx)) {
                tl.lock().focus(t, 60);
            }
        }
    }

    pub(super) fn data_source_summary(&mut self, ui: &mut egui::Ui, ds_id: i64) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let Some(ds) = self.case_sources.find(ds_id).map(|t| t.ds.clone()) else {
            ui.spinner();
            return;
        };
        let live = self.analysis.lock().any_active();
        match self.summary.ui(ui, &case, &ds, live, &self.prefs) {
            Some(summary::Action::ShowFile(c)) => self.viewer.set_content(Some(*c)),
            Some(summary::Action::ViewInTimeline { range }) => {
                if let Some(tl) = self.open_timeline(ui.ctx())
                    && let Ok(ds) = u32::try_from(ds_id)
                {
                    tl.lock().focus_data_source(ds, range);
                }
            }
            None => {}
        }
    }

    pub(super) fn volume_summary(&mut self, ui: &mut egui::Ui, ds_id: i64, volume_id: i64) {
        let Some(t) = self.case_sources.find(ds_id) else {
            return;
        };
        let Some(v) = t.volumes.iter().find(|v| v.volume.id == volume_id) else {
            return;
        };
        let vol = &v.volume;
        ui.heading(format!("vol{} — {}", vol.addr, vol.description));
        let bytes = vol.sector_count.saturating_mul(u64::from(vol.sector_size));
        let fs = if v.roots.is_empty() {
            "none detected".to_owned()
        } else {
            v.roots
                .iter()
                .map(|r| r.fs.kind.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        egui::Grid::new("vol-summary")
            .num_columns(2)
            .striped(true)
            .spacing([24.0, 4.0])
            .show(ui, |ui| {
                let mut kv = |k: &str, v: String| {
                    ui.strong(k);
                    ui.label(v);
                    ui.end_row();
                };
                kv(
                    "Kind",
                    if v.unallocated() {
                        "Unallocated"
                    } else {
                        "Allocated"
                    }
                    .to_owned(),
                );
                kv("Start sector", vol.start_sector.to_string());
                kv(
                    "Length",
                    format!("{} sectors — {}", vol.sector_count, size_long(bytes)),
                );
                if let Some(n) = &vol.name {
                    kv("Name", n.clone());
                }
                kv("File system", fs);
            });
    }

    pub(super) fn popouts(&mut self, ctx: &egui::Context) {
        self.popouts.retain(|p| !p.closed.load(Ordering::Relaxed));
        for p in &self.popouts {
            let panel = Arc::clone(&p.panel);
            let closed = Arc::clone(&p.closed);
            let prefs = self.prefs.clone();
            ctx.show_viewport_deferred(
                p.id,
                egui::ViewportBuilder::default()
                    .with_title(&p.title)
                    .with_inner_size([960.0, 640.0]),
                move |ui, _class| {
                    egui::CentralPanel::default().show(ui, |ui| panel.lock().ui(ui, &prefs));
                    if ui.ctx().input(|i| i.viewport().close_requested()) {
                        closed.store(true, Ordering::Relaxed);
                    }
                },
            );
        }
    }

    /// The result pane's tabs: Listing, then one per search (closable).
    /// The result tabs in order (Ctrl+1..9).
    pub(super) fn result_tab_list(&self) -> Vec<ResultTab> {
        std::iter::once(ResultTab::Listing)
            .chain(
                self.searches
                    .file
                    .iter()
                    .map(|s| ResultTab::FileSearch(s.number)),
            )
            .chain(
                self.searches
                    .keyword
                    .iter()
                    .map(|s| ResultTab::KeywordSearch(s.number)),
            )
            .collect()
    }

    pub(super) fn result_tabs(&mut self, ui: &mut egui::Ui) {
        let mut tabs = vec![(ResultTab::Listing, "Listing".to_owned())];
        tabs.extend(self.searches.file.iter().map(|s| {
            (
                ResultTab::FileSearch(s.number),
                format!("File Search Results {}", s.number),
            )
        }));
        tabs.extend(
            self.searches
                .keyword
                .iter()
                .map(|s| (ResultTab::KeywordSearch(s.number), s.title())),
        );
        if tabs.len() == 1 {
            return;
        }
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (tab, title) in tabs {
                let on = self.searches.tab == tab;
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if ui.selectable_label(on, &title).clicked() {
                        self.searches.tab = tab;
                    }
                    if tab != ResultTab::Listing
                        && ui
                            .small_button("×")
                            .on_hover_text(format!("Close {title}"))
                            .clicked()
                    {
                        close = Some(tab);
                    }
                });
                ui.separator();
            }
        });
        ui.separator();
        if let Some(tab) = close {
            match tab {
                ResultTab::FileSearch(n) => self.searches.file.retain(|s| s.number != n),
                ResultTab::KeywordSearch(n) => self.searches.keyword.retain(|s| s.number != n),
                ResultTab::Listing => {}
            }
            if self.searches.tab == tab {
                self.searches.tab = ResultTab::Listing;
            }
        }
    }

    /// "Keyword search {n} - {term}".
    pub(super) fn keyword_view(&mut self, ui: &mut egui::Ui, number: usize) {
        let ctx = ui.ctx().clone();
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let Some(s) = self
            .searches
            .keyword
            .iter_mut()
            .find(|s| s.number == number)
        else {
            return;
        };
        let (loader, req) = (Arc::clone(&case), Arc::clone(&s.request));
        let Some(data) = s.rows.get(&ctx, move || {
            crate::keyword_search_ui::run(&loader, &req, &|| false)
        }) else {
            if let Some(e) = s.rows.error() {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Searching…");
                });
            }
            return;
        };
        if data.rows.is_empty() {
            ui.weak("No results found.");
            return;
        }
        let mut summary = if data.more {
            format!(
                "{} files shown; there are more. Narrow the search.",
                crate::format::grouped(data.rows.len() as u64)
            )
        } else {
            format!("{} files", crate::format::grouped(data.rows.len() as u64))
        };
        if data.saved > 0 {
            let _ = write!(
                summary,
                " · {} keyword hits saved",
                crate::format::grouped(data.saved as u64)
            );
        }
        ui.weak(summary);
        let query = s.request.query.clone();
        let rows = &data.rows;
        let tags = self.tagging.catalog.get(&ctx, Some(&case));
        let menus = RowMenus {
            tags: Some(&tags),
            hash_sets: &self.persisted.ingest.hash_lookup.sets,
        };
        let ev = s.table.ui_with(
            ui,
            rows,
            &self.prefs,
            menus,
            Some(("Keyword Preview", &data.previews)),
        );
        let csv = (ev.csv || ev.csv_selection).then(|| {
            s.table.csv(
                rows,
                &self.prefs,
                Some(("Keyword Preview", &data.previews)),
                ev.csv_selection,
            )
        });
        self.file_table_actions(&ctx, &case, rows, &ev, csv);
        let content = |r: &FileRow| Content::from_case(Arc::clone(&case), r.clone());
        if let Some(r) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.show(Some(content(r)), Some(query));
        }
        if let Some(r) = ev.activated.and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
        }
    }

    /// "File Search Results {n}".
    pub(super) fn search_view(&mut self, ui: &mut egui::Ui, number: usize) {
        let ctx = ui.ctx().clone();
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let Some(s) = self.searches.file.iter_mut().find(|s| s.number == number) else {
            return;
        };
        let (loader, search) = (Arc::clone(&case), Arc::clone(&s.search));
        let Some(data) = s
            .rows
            .get(&ctx, move || crate::file_search_ui::run(&loader, &search))
        else {
            if let Some(e) = s.rows.error() {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Performing file search by attributes. Please wait.");
                });
            }
            return;
        };
        let (rows, more) = (&data.0, data.1);
        if rows.is_empty() {
            ui.weak("No results found.");
            return;
        }
        ui.weak(if more {
            format!(
                "{} matches shown; there are more. Narrow the search.",
                crate::format::grouped(rows.len() as u64)
            )
        } else {
            format!("{} matches", crate::format::grouped(rows.len() as u64))
        });
        let tags = self.tagging.catalog.get(&ctx, Some(&case));
        let menus = RowMenus {
            tags: Some(&tags),
            hash_sets: &self.persisted.ingest.hash_lookup.sets,
        };
        let ev = s.table.ui(ui, rows, &self.prefs, menus);
        let csv = (ev.csv || ev.csv_selection)
            .then(|| s.table.csv(rows, &self.prefs, None, ev.csv_selection));
        self.file_table_actions(&ctx, &case, rows, &ev, csv);
        let content = |r: &FileRow| Content::from_case(Arc::clone(&case), r.clone());
        if let Some(r) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.set_content(Some(content(r)));
        }
        if let Some(r) = ev.activated.and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
        }
    }
}
