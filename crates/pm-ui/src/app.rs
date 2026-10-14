//! The postmortem main window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use pm_evidence::OpenOptions;
use pm_volume::SlotKind;

use crate::format::{DisplayPrefs, size_long};
use crate::model::{Content, DataSource, FileRow, NodeId, list_dir, list_orphans};
use crate::table::TableState;
use crate::task::{self, Task};
use crate::timeline::TimelineWindow;
use crate::viewers::ViewerPanel;

type Listing = Result<Arc<Vec<FileRow>>, String>;

/// Settings persisted between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub theme: egui::ThemePreference,
    pub local_time: bool,
    pub recent: Vec<PathBuf>,
    pub table: TableState,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            theme: egui::ThemePreference::System,
            local_time: false,
            recent: Vec::new(),
            table: TableState::default(),
        }
    }
}

/// A viewer shown in its own native window.
struct Popout {
    id: egui::ViewportId,
    title: String,
    panel: Arc<Mutex<ViewerPanel>>,
    closed: Arc<AtomicBool>,
}

/// Application state.
#[allow(missing_debug_implementations, reason = "holds egui/eframe handles")]
pub struct PostmortemApp {
    persisted: Persisted,
    prefs: DisplayPrefs,
    sources: Vec<DataSource>,
    loading: Vec<(PathBuf, Task<Result<DataSource, String>>)>,
    listings: HashMap<NodeId, Task<Listing>>,
    selected: Option<NodeId>,
    back: Vec<NodeId>,
    forward: Vec<NodeId>,
    table: TableState,
    viewer: ViewerPanel,
    popouts: Vec<Popout>,
    next_popout: u64,
    errors: Vec<String>,
    about_open: bool,
    exports: Vec<Task<Result<String, String>>>,
    wordmark: Option<egui::TextureHandle>,
    timeline: Option<Arc<Mutex<TimelineWindow>>>,
    timeline_closed: Arc<AtomicBool>,
}

impl PostmortemApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Vec<PathBuf>) -> Self {
        let persisted: Persisted = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        Self::with_state(&cc.egui_ctx, persisted, open)
    }

    /// Constructs the app without eframe (used by UI tests).
    pub fn with_state(ctx: &egui::Context, persisted: Persisted, open: Vec<PathBuf>) -> Self {
        ctx.set_theme(persisted.theme);
        let mut app = Self {
            prefs: DisplayPrefs::default(),
            table: persisted.table.clone(),
            persisted,
            sources: Vec::new(),
            loading: Vec::new(),
            listings: HashMap::new(),
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            viewer: ViewerPanel::default(),
            popouts: Vec::new(),
            next_popout: 0,
            errors: Vec::new(),
            about_open: false,
            exports: Vec::new(),
            wordmark: None,
            timeline: None,
            timeline_closed: Arc::new(AtomicBool::new(false)),
        };
        app.apply_time_pref();
        for p in open {
            app.add_data_source(ctx, p);
        }
        app
    }

    pub fn data_sources(&self) -> &[DataSource] {
        &self.sources
    }

    pub fn is_loading(&self) -> bool {
        !self.loading.is_empty()
    }

    fn apply_time_pref(&mut self) {
        self.prefs.timezone = if self.persisted.local_time {
            jiff::tz::TimeZone::system()
        } else {
            jiff::tz::TimeZone::UTC
        };
    }

    /// Opens a disk image in the background.
    pub fn add_data_source(&mut self, ctx: &egui::Context, path: PathBuf) {
        let id = self.sources.len() + self.loading.len();
        let opts = OpenOptions {
            timezone: self.prefs.timezone.clone(),
        };
        let p = path.clone();
        let task = Task::spawn(ctx, move || {
            let evidence = pm_evidence::open(&p, &opts).map_err(|e| e.to_string())?;
            let name = p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            Ok(DataSource { id, name, evidence })
        });
        self.persisted.recent.retain(|r| r != &path);
        self.persisted.recent.insert(0, path.clone());
        self.persisted.recent.truncate(10);
        self.loading.push((path, task));
    }

    fn poll_loading(&mut self) {
        let mut still = Vec::new();
        for (path, mut t) in std::mem::take(&mut self.loading) {
            match t.poll() {
                Some(Ok(ds)) => {
                    let mut ds = ds.clone();
                    ds.id = self.sources.len();
                    let id = ds.id;
                    self.sources.push(ds);
                    if self.selected.is_none() {
                        self.navigate(NodeId::DataSource(id));
                    }
                }
                Some(Err(e)) => self
                    .errors
                    .push(format!("Cannot open {}: {e}", path.display())),
                None => still.push((path, t)),
            }
        }
        self.loading = still;
        self.exports.retain_mut(|t| match t.poll() {
            Some(Ok(_)) => false,
            Some(Err(e)) => {
                self.errors.push(e.clone());
                false
            }
            None => true,
        });
    }

    /// Directory listing for a node, loaded in the background.
    fn listing(&mut self, ctx: &egui::Context, node: &NodeId) -> Option<&Listing> {
        if !self.listings.contains_key(node) {
            let fs_and_target = match node {
                NodeId::Dir {
                    ds,
                    fs,
                    inode,
                    path,
                } => self
                    .sources
                    .get(*ds)
                    .and_then(|d| d.fs(*fs))
                    .map(|f| (Arc::clone(f), Some((*inode, Arc::clone(path))))),
                NodeId::Orphans { ds, fs } => self
                    .sources
                    .get(*ds)
                    .and_then(|d| d.fs(*fs))
                    .map(|f| (Arc::clone(f), None)),
                _ => None,
            };
            let (fs, target) = fs_and_target?;
            let task = Task::spawn(ctx, move || {
                let rows = match target {
                    Some((inode, path)) => list_dir(&*fs, inode, &path)?,
                    None => list_orphans(&*fs)?,
                };
                Ok(Arc::new(rows))
            });
            self.listings.insert(node.clone(), task);
        }
        self.listings.get_mut(node).and_then(Task::poll)
    }

    pub fn navigate(&mut self, node: NodeId) {
        if self.selected.as_ref() == Some(&node) {
            return;
        }
        if let Some(prev) = self.selected.take() {
            self.back.push(prev);
        }
        self.forward.clear();
        self.selected = Some(node);
        self.table.reset();
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            if let Some(cur) = self.selected.replace(prev) {
                self.forward.push(cur);
            }
            self.table.reset();
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            if let Some(cur) = self.selected.replace(next) {
                self.back.push(cur);
            }
            self.table.reset();
        }
    }

    fn fs_of(&self, node: &NodeId) -> Option<Arc<dyn pm_fs::FileSystem>> {
        match node {
            NodeId::Dir { ds, fs, .. } | NodeId::Orphans { ds, fs } => {
                self.sources.get(*ds).and_then(|d| d.fs(*fs)).cloned()
            }
            _ => None,
        }
    }

    fn pick_image(&mut self, ctx: &egui::Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Add Data Source — Disk Image")
            .add_filter(
                "Disk images",
                &[
                    "dd", "raw", "img", "bin", "001", "000", "aa", "e01", "E01", "s01",
                ],
            )
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(p) = picked {
            self.add_data_source(ctx, p);
        }
    }

    fn open_popout(&mut self, content: Content) {
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

    fn export(&mut self, ctx: &egui::Context, content: Content) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title("Extract File")
            .set_file_name(content.row.name.replace(':', "_"))
            .save_file()
        else {
            return;
        };
        self.exports.push(Task::spawn(ctx, move || {
            let src = content.source()?;
            let mut out =
                std::fs::File::create(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            let mut buf = vec![0u8; 1 << 20];
            let mut off = 0;
            while off < src.size() {
                let n = src.read_at(off, &mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut out, buf.get(..n).unwrap_or_default())
                    .map_err(|e| e.to_string())?;
                off += n as u64;
            }
            Ok(dest.display().to_string())
        }));
    }

    /// Opens (or focuses) the Timeline window, building events if needed.
    fn open_timeline(&mut self, ctx: &egui::Context) -> Option<Arc<Mutex<TimelineWindow>>> {
        if self.sources.is_empty() {
            self.errors
                .push("Add a data source before opening the Timeline.".into());
            return None;
        }
        if self.timeline.is_none() || self.timeline_closed.swap(false, Ordering::Relaxed) {
            self.timeline = Some(Arc::new(Mutex::new(TimelineWindow::new(
                ctx,
                self.sources.clone(),
                self.prefs.clone(),
            ))));
        }
        ctx.send_viewport_cmd_to(Self::timeline_viewport_id(), egui::ViewportCommand::Focus);
        self.timeline.clone()
    }

    fn timeline_viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of("timeline")
    }

    fn show_timeline(&mut self, ctx: &egui::Context) {
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

    /// The decoded wordmark texture (created on first use).
    fn wordmark(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.wordmark.is_none() {
            let img = image::load_from_memory(crate::WORDMARK_PNG)
                .ok()?
                .to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            self.wordmark = Some(ctx.load_texture("wordmark", color, egui::TextureOptions::LINEAR));
        }
        self.wordmark.clone()
    }

    // ------------------------------------------------------------ panels

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("Case", |ui| {
                if ui.button("Add Data Source…").clicked() {
                    ui.close();
                    self.pick_image(ui.ctx());
                }
                ui.menu_button("Recent Images", |ui| {
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
                ui.separator();
                if ui.button("Close All Data Sources").clicked() {
                    ui.close();
                    self.sources.clear();
                    self.listings.clear();
                    self.selected = None;
                    self.back.clear();
                    self.forward.clear();
                    self.viewer.set_content(None);
                }
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
            });
            ui.menu_button("Tools", |ui| {
                if ui
                    .add_enabled(!self.sources.is_empty(), egui::Button::new("⏱ Timeline"))
                    .clicked()
                {
                    self.open_timeline(ui.ctx());
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
                if ui.button("About postmortem").clicked() {
                    self.about_open = true;
                    ui.close();
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("➕ Add Data Source")
                .on_hover_text("Open a disk image (Ctrl/⌘+O)")
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
                .add_enabled(!self.sources.is_empty(), egui::Button::new("⏱ Timeline"))
                .on_hover_text("Open the Timeline (Tools › Timeline)")
                .clicked()
            {
                self.open_timeline(ui.ctx());
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
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("{} data source(s)", self.sources.len()));
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
    }

    fn tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.heading("Data Sources");
            if self.sources.is_empty() && self.loading.is_empty() {
                ui.weak("No data sources. Use Case › Add Data Source.");
            }
            for (path, _) in &self.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Opening {}…",
                        path.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    ));
                });
            }
            for ds in self.sources.clone() {
                let node = NodeId::DataSource(ds.id);
                self.tree_node(ui, &node, &format!("🖴 {}", ds.name), true, |app, ui| {
                    app.data_source_children(ui, &ds);
                });
            }
        });
    }

    /// One collapsible tree node with a selectable label; children render lazily.
    fn tree_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        has_children: bool,
        children: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let id = ui.make_persistent_id(node);
        let selected = self.selected.as_ref() == Some(node);
        if has_children {
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                false,
            );
            let (_, header, _) = state
                .show_header(ui, |ui| ui.selectable_label(selected, label))
                .body(|ui| children(self, ui));
            // Click selects and expands; double-click toggles.
            let (clicked, double) = (header.inner.clicked(), header.inner.double_clicked());
            if clicked || double {
                let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                );
                st.set_open(if double { !st.is_open() } else { true });
                st.store(ui.ctx());
            }
            if clicked {
                self.navigate(node.clone());
            }
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                if ui.selectable_label(selected, label).clicked() {
                    self.navigate(node.clone());
                }
            });
        }
    }

    fn data_source_children(&mut self, ui: &mut egui::Ui, ds: &DataSource) {
        if let Some(vs) = &ds.evidence.volume_system {
            for p in &vs.partitions {
                if p.kind == SlotKind::Meta {
                    continue;
                }
                let label = format!(
                    "vol{} ({}: {}-{})",
                    p.addr,
                    p.description,
                    p.start_sector,
                    p.end_sector().saturating_sub(1)
                );
                let fs_idx = ds
                    .evidence
                    .filesystems
                    .iter()
                    .position(|f| f.partition.as_ref().is_some_and(|fp| fp.addr == p.addr));
                let node = NodeId::Volume {
                    ds: ds.id,
                    addr: p.addr,
                };
                self.tree_node(ui, &node, &label, fs_idx.is_some(), |app, ui| {
                    if let Some(i) = fs_idx {
                        app.fs_root_children(ui, ds, i);
                    }
                });
            }
        } else {
            if ds.evidence.filesystems.is_empty() {
                ui.weak("No file system found");
            }
            for i in 0..ds.evidence.filesystems.len() {
                self.fs_root_children(ui, ds, i);
            }
        }
    }

    fn fs_root_children(&mut self, ui: &mut egui::Ui, ds: &DataSource, fs_idx: usize) {
        let Some(fs) = ds.fs(fs_idx) else { return };
        let root = NodeId::Dir {
            ds: ds.id,
            fs: fs_idx,
            inode: fs.root(),
            path: Arc::from("/"),
        };
        let label = format!("🗀 / ({})", fs.info().kind);
        self.tree_node(ui, &root, &label, true, |app, ui| {
            app.dir_children(ui, &root);
            let orphans = NodeId::Orphans {
                ds: ds.id,
                fs: fs_idx,
            };
            app.tree_node(ui, &orphans, "🗀 $OrphanFiles", false, |_, _| {});
        });
    }

    fn dir_children(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let NodeId::Dir { ds, fs, .. } = node else {
            return;
        };
        let (ds, fs) = (*ds, *fs);
        let ctx = ui.ctx().clone();
        match self.listing(&ctx, node).cloned() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Loading…");
                });
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            Some(Ok(rows)) => {
                let mut dirs: Vec<&FileRow> = rows.iter().filter(|r| r.is_dir()).collect();
                dirs.sort_by_key(|r| r.name.to_lowercase());
                for r in dirs {
                    let child = NodeId::Dir {
                        ds,
                        fs,
                        inode: r.inode,
                        path: Arc::from(r.path.as_str()),
                    };
                    let label = if r.deleted() {
                        format!("🗀 {} ✖", r.name)
                    } else {
                        format!("🗀 {}", r.name)
                    };
                    self.tree_node(ui, &child, &label, true, |app, ui| {
                        app.dir_children(ui, &child);
                    });
                }
            }
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        let Some(node) = self.selected.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 150.0).max(0.0));
                if let Some(tex) = self.wordmark(ui.ctx()) {
                    ui.add(egui::Image::new(&tex).max_width(560.0));
                }
                ui.add_space(18.0);
                ui.weak("Add a data source to begin (Case › Add Data Source…), or drop a disk image here.");
            });
            return;
        };
        match &node {
            NodeId::DataSource(ds) => self.data_source_summary(ui, *ds),
            NodeId::Volume { ds, addr } => self.volume_summary(ui, *ds, *addr),
            NodeId::Dir { path, .. } => {
                ui.horizontal(|ui| {
                    ui.strong("Listing");
                    ui.monospace(path.as_ref());
                });
                self.file_table(ui, &node);
            }
            NodeId::Orphans { .. } => {
                ui.strong("$OrphanFiles — deleted files whose parent directory is gone");
                self.file_table(ui, &node);
            }
        }
    }

    fn file_table(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let ctx = ui.ctx().clone();
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
        ui.weak(format!("{} items", rows.len()));
        let ev = self.table.ui(ui, &rows, &self.prefs);
        let Some(fs) = self.fs_of(node) else { return };
        if let Some(i) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.set_content(Some(Content {
                fs: Arc::clone(&fs),
                row: i.clone(),
            }));
        }
        if let Some(r) = ev.activated.and_then(|i| rows.get(i)) {
            if r.is_dir() {
                let (NodeId::Dir { ds, fs: f, .. } | NodeId::Orphans { ds, fs: f }) = node.clone()
                else {
                    return;
                };
                self.navigate(NodeId::Dir {
                    ds,
                    fs: f,
                    inode: r.inode,
                    path: Arc::from(r.path.as_str()),
                });
            } else {
                self.open_popout(Content {
                    fs: Arc::clone(&fs),
                    row: r.clone(),
                });
            }
        }
        if let Some(r) = ev.popout.and_then(|i| rows.get(i)) {
            self.open_popout(Content {
                fs: Arc::clone(&fs),
                row: r.clone(),
            });
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
        if let Some(r) = ev.export.and_then(|i| rows.get(i)) {
            self.export(&ctx, Content { fs, row: r.clone() });
        }
    }

    fn data_source_summary(&mut self, ui: &mut egui::Ui, ds: usize) {
        let Some(d) = self.sources.get(ds) else {
            return;
        };
        let info = &d.evidence.image.info;
        ui.heading(&d.name);
        egui::Grid::new("ds-summary")
            .num_columns(2)
            .striped(true)
            .spacing([24.0, 4.0])
            .show(ui, |ui| {
                let mut kv = |k: &str, v: String| {
                    ui.strong(k);
                    ui.add(egui::Label::new(v).selectable(true));
                    ui.end_row();
                };
                kv("Image format", info.format.to_string());
                kv("Size", size_long(info.size));
                kv("Sector size", info.sector_size.to_string());
                for (i, p) in info.paths.iter().enumerate() {
                    kv(
                        if i == 0 { "Segments" } else { "" },
                        p.display().to_string(),
                    );
                }
                for (algo, h) in &info.stored_hashes {
                    kv(&format!("Stored {algo:?}"), h.clone());
                }
                for (k, v) in &info.metadata {
                    kv(k, v.clone());
                }
                kv(
                    "Volume system",
                    d.evidence.volume_system.as_ref().map_or_else(
                        || "None (single volume)".to_owned(),
                        |v| format!("{} ({} partitions)", v.kind, v.allocated().count()),
                    ),
                );
                for f in &d.evidence.filesystems {
                    let i = f.fs.info();
                    kv(
                        "File system",
                        format!(
                            "{} at offset {} — label {:?}, serial {}",
                            i.kind,
                            f.offset,
                            i.label.as_deref().unwrap_or(""),
                            i.serial.as_deref().unwrap_or("")
                        ),
                    );
                }
            });
        if let Some(vs) = &d.evidence.volume_system {
            for w in &vs.warnings {
                ui.colored_label(ui.visuals().warn_fg_color, w);
            }
        }
    }

    fn volume_summary(&mut self, ui: &mut egui::Ui, ds: usize, addr: u32) {
        let Some(d) = self.sources.get(ds) else {
            return;
        };
        let Some(vs) = &d.evidence.volume_system else {
            return;
        };
        let Some(p) = vs.partitions.iter().find(|p| p.addr == addr) else {
            return;
        };
        ui.heading(format!("vol{} — {}", p.addr, p.description));
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
                kv("Kind", format!("{:?}", p.kind));
                kv("Start sector", p.start_sector.to_string());
                kv(
                    "Length",
                    format!("{} sectors — {}", p.sector_count, size_long(vs.byte_len(p))),
                );
                if let Some(n) = &p.name {
                    kv("Name", n.clone());
                }
                if let Some(u) = &p.unique_id {
                    kv("Partition GUID", u.to_string());
                }
                let fs = d.evidence.fs_for_partition(addr).map_or_else(
                    || "none detected".to_owned(),
                    |f| f.fs.info().kind.to_string(),
                );
                kv("File system", fs);
            });
    }

    fn popouts(&mut self, ctx: &egui::Context) {
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

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let (open, back, fwd) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight),
            )
        });
        if open {
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
        for p in dropped {
            self.add_data_source(ctx, p);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.about_open {
            let wordmark = self.wordmark(ctx);
            let r = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
                if let Some(tex) = &wordmark {
                    ui.add(egui::Image::new(tex).max_width(430.0));
                }
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.label("A digital forensics platform written in Rust,");
                ui.label("inspired by Autopsy® and The Sleuth Kit®.");
                ui.add_space(8.0);
                ui.button("Close").clicked()
            });
            if r.inner || r.should_close() {
                self.about_open = false;
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

    /// Draws one frame (shared by eframe and UI tests).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_loading();
        self.shortcuts(&ctx);
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu_bar(ui);
            self.toolbar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("tree")
            .resizable(true)
            .default_size(300.0)
            .min_size(160.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.tree(ui);
            });
        egui::Panel::bottom("viewer")
            .resizable(true)
            .default_size(330.0)
            .min_size(120.0)
            .show(ui, |ui| {
                // egui 0.36 panels persist the size of their content; claim the
                // whole panel so loading states (a spinner) do not shrink it.
                ui.set_min_size(ui.available_size());
                self.viewer.ui(ui, &self.prefs);
            });
        egui::CentralPanel::default().show(ui, |ui| self.results(ui));
        self.popouts(&ctx);
        self.show_timeline(&ctx);
        self.dialogs(&ctx);
        self.persisted.table = self.table.clone();
    }
}

impl eframe::App for PostmortemApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.persisted);
    }
}
