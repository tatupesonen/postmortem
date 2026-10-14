//! The postmortem main window.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use pm_evidence::OpenOptions;
use pm_volume::SlotKind;

use crate::analysis::Analysis;
use crate::case_ui::{NewCaseWizard, WelcomeAction, WizardAction};
use crate::discovery::DiscoveryWindow;
use crate::format::{DisplayPrefs, size_long};
use crate::gallery::GalleryWindow;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow, NodeId, list_dir, list_orphans};
use crate::summary::{self, SummaryState};
use crate::table::TableState;
use crate::task::{self, Task};
use crate::timeline::TimelineWindow;
use crate::viewers::ViewerPanel;
use crate::views::View;

type Listing = Result<Arc<Vec<FileRow>>, String>;

/// Settings persisted between runs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Persisted {
    pub theme: egui::ThemePreference,
    pub local_time: bool,
    pub recent: Vec<PathBuf>,
    pub table: TableState,
    /// Default base directory for new cases (`~/postmortem/Cases`).
    pub case_root: Option<PathBuf>,
    /// Recently opened case folders, newest first.
    pub recent_cases: Vec<PathBuf>,
    /// Ingest module choices and hash sets (Add Data Source wizard).
    pub ingest: crate::add_source::IngestPrefs,
    /// Leave known files (e.g. NSRL) out of the Views tree (Autopsy default).
    pub hide_known_in_views: bool,
}

impl Default for Persisted {
    fn default() -> Self {
        Self {
            theme: egui::ThemePreference::System,
            local_time: false,
            recent: Vec::new(),
            table: TableState::default(),
            case_root: None,
            recent_cases: Vec::new(),
            ingest: crate::add_source::IngestPrefs::default(),
            hide_known_in_views: true,
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
    /// Decoded wordmark texture and whether it is the dark-theme variant.
    wordmark: Option<(bool, egui::TextureHandle)>,
    timeline: Option<Arc<Mutex<TimelineWindow>>>,
    timeline_closed: Arc<AtomicBool>,
    gallery: Option<Arc<Mutex<GalleryWindow>>>,
    gallery_closed: Arc<AtomicBool>,
    discovery: Option<Arc<Mutex<DiscoveryWindow>>>,
    discovery_closed: Arc<AtomicBool>,
    /// File catalogue of all data sources (rebuilt when sources change).
    index: Option<SharedIndex>,
    summary: SummaryState,
    /// The case, and adding/analysing data sources in the background.
    analysis: Arc<Mutex<Analysis>>,
    progress_open: bool,
    progress_closed: Arc<AtomicBool>,
    case_wizard: Option<NewCaseWizard>,
    result_counts: crate::results::Counts,
    result_rows: HashMap<i64, Task<Result<Arc<crate::results::ResultRows>, String>>>,
    result_selected: Option<usize>,
    /// Analysis was running last frame (refresh results when it stops).
    was_analysing: bool,
    was_analysing_views: bool,
    add_wizard: Option<crate::add_source::AddSourceWizard>,
    view_counts: crate::views::Counts,
    view_rows: HashMap<crate::views::View, Task<Result<Arc<crate::views::ViewRows>, String>>>,
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
            gallery: None,
            gallery_closed: Arc::new(AtomicBool::new(false)),
            discovery: None,
            discovery_closed: Arc::new(AtomicBool::new(false)),
            index: None,
            summary: SummaryState::default(),
            analysis: Arc::new(Mutex::new(Analysis::default())),
            progress_open: false,
            progress_closed: Arc::new(AtomicBool::new(false)),
            case_wizard: None,
            result_counts: crate::results::Counts::default(),
            result_rows: HashMap::new(),
            result_selected: None,
            was_analysing: false,
            was_analysing_views: false,
            add_wizard: None,
            view_counts: crate::views::Counts::default(),
            view_rows: HashMap::new(),
        };
        app.apply_time_pref();
        // Command line: a case (folder or case.pmcase) is opened; images are
        // added to it.
        for p in open {
            if crate::case_ui::is_case_path(&p) {
                if let Err(e) = app.open_case(ctx, &p) {
                    app.errors.push(e);
                }
            } else {
                app.add_data_source(ctx, p);
            }
        }
        app
    }

    pub fn data_sources(&self) -> &[DataSource] {
        &self.sources
    }

    /// Folder of the open case, once one exists.
    pub fn analysis_case_dir(&self) -> Option<PathBuf> {
        self.analysis.lock().case().map(|c| c.dir().to_path_buf())
    }

    /// Files with an MD5 in the case (for tests and diagnostics).
    pub fn analysis_hashed_files(&self) -> u64 {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return 0;
        };
        case.db()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM files WHERE md5 IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
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
        self.add_data_source_with(ctx, path, load, settings);
    }

    fn default_timezone() -> String {
        jiff::tz::TimeZone::system()
            .iana_name()
            .unwrap_or("UTC")
            .to_owned()
    }

    fn add_data_source_with(
        &mut self,
        ctx: &egui::Context,
        path: PathBuf,
        load: pm_case::LoadOptions,
        settings: pm_ingest::JobSettings,
    ) {
        if let Err(e) = self
            .analysis
            .lock()
            .start(ctx, path.clone(), load, settings)
        {
            self.errors.push(e);
            return;
        }
        self.persisted.recent.retain(|r| r != &path);
        self.persisted.recent.insert(0, path.clone());
        self.persisted.recent.truncate(10);
        self.browse_image(ctx, path);
    }

    /// Opens a disk image for browsing (no analysis).
    fn browse_image(&mut self, ctx: &egui::Context, path: PathBuf) {
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
        self.loading.push((path, task));
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
                Some(p) if std::path::Path::new(p).is_file() => {
                    self.browse_image(ctx, PathBuf::from(p));
                }
                _ => self.errors.push(format!(
                    "Data source {} was not found at {}. It may have been moved.",
                    ds.name,
                    ds.paths.first().map_or("(no path)", String::as_str)
                )),
            }
        }
        Ok(())
    }

    fn set_case(&mut self, ctx: &egui::Context, case: pm_case::Case) {
        self.close_case(ctx);
        let dir = case.dir().to_path_buf();
        let title = format!("{} — postmortem", case.details().name);
        self.analysis.lock().set_case(Arc::new(case));
        self.persisted.recent_cases.retain(|r| r != &dir);
        self.persisted.recent_cases.insert(0, dir);
        self.persisted.recent_cases.truncate(10);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// Closes the case and everything shown from it.
    pub fn close_case(&mut self, ctx: &egui::Context) {
        self.analysis.lock().close();
        self.sources.clear();
        self.loading.clear();
        self.listings.clear();
        self.selected = None;
        self.back.clear();
        self.forward.clear();
        self.viewer.set_content(None);
        self.index = None;
        self.timeline = None;
        self.gallery = None;
        self.discovery = None;
        self.result_counts.invalidate();
        self.result_rows.clear();
        self.result_selected = None;
        self.view_counts.invalidate();
        self.view_rows.clear();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("postmortem".into()));
    }

    fn new_case_wizard(&mut self) {
        let base = self
            .persisted
            .case_root
            .clone()
            .unwrap_or_else(crate::analysis::default_case_root);
        self.case_wizard = Some(NewCaseWizard::new(&base));
    }

    fn pick_case(&mut self, ctx: &egui::Context) {
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

    fn poll_loading(&mut self) {
        let mut still = Vec::new();
        for (path, mut t) in std::mem::take(&mut self.loading) {
            match t.poll() {
                Some(Ok(ds)) => {
                    let mut ds = ds.clone();
                    ds.id = self.sources.len();
                    let id = ds.id;
                    self.sources.push(ds);
                    self.index = None;
                    if let Some(tl) = self.timeline.take() {
                        drop(tl); // rebuilt with the new source on next open
                    }
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
        self.result_selected = None;
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

    /// Opens the Add Data Source wizard (requires an open case).
    fn pick_image(&mut self, _ctx: &egui::Context) {
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

    /// The shared file index, (re)built when the set of data sources changes.
    fn shared_index(&mut self, ctx: &egui::Context) -> SharedIndex {
        if let Some(i) = &self.index
            && i.covers() == self.sources.len()
        {
            return i.clone();
        }
        let i = SharedIndex::build(ctx, &self.sources);
        self.index = Some(i.clone());
        i
    }

    fn open_gallery(&mut self, ctx: &egui::Context) {
        if self.sources.is_empty() {
            self.errors
                .push("Add a data source before opening the Image Gallery.".into());
            return;
        }
        if self.gallery.is_none() || self.gallery_closed.swap(false, Ordering::Relaxed) {
            let w = GalleryWindow::new(
                self.shared_index(ctx),
                self.sources.clone(),
                self.prefs.clone(),
            );
            self.gallery = Some(Arc::new(Mutex::new(w)));
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("gallery"),
            egui::ViewportCommand::Focus,
        );
    }

    fn open_discovery(&mut self, ctx: &egui::Context) {
        if self.sources.is_empty() {
            self.errors
                .push("Add a data source before opening Discovery.".into());
            return;
        }
        if self.discovery.is_none() || self.discovery_closed.swap(false, Ordering::Relaxed) {
            let w = DiscoveryWindow::new(
                self.shared_index(ctx),
                self.sources.clone(),
                self.prefs.clone(),
            );
            self.discovery = Some(Arc::new(Mutex::new(w)));
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("discovery"),
            egui::ViewportCommand::Focus,
        );
    }

    fn show_discovery(&mut self, ctx: &egui::Context) {
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

    fn show_gallery(&mut self, ctx: &egui::Context) {
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
    fn open_timeline(&mut self, ctx: &egui::Context) -> Option<Arc<Mutex<TimelineWindow>>> {
        if self.sources.is_empty() {
            self.errors
                .push("Add a data source before opening the Timeline.".into());
            return None;
        }
        if self.timeline.is_none() || self.timeline_closed.swap(false, Ordering::Relaxed) {
            self.timeline = Some(Arc::new(Mutex::new(TimelineWindow::new(
                self.shared_index(ctx),
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

    /// The wordmark texture for the current theme (decoded on first use and
    /// again when the theme flips).
    fn wordmark(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
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

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
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
                ui.separator();
                if ui
                    .add_enabled(has_case, egui::Button::new("Add Data Source…"))
                    .clicked()
                {
                    ui.close();
                    self.pick_image(ui.ctx());
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
                    self.view_counts.invalidate();
                    self.view_rows.clear();
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui.button("⚙ Ingest Progress").clicked() {
                    self.progress_open = true;
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(!self.sources.is_empty(), egui::Button::new("⏱ Timeline"))
                    .clicked()
                {
                    self.open_timeline(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !self.sources.is_empty(),
                        egui::Button::new("🖼 Image Gallery"),
                    )
                    .clicked()
                {
                    self.open_gallery(ui.ctx());
                    ui.close();
                }
                if ui
                    .add_enabled(!self.sources.is_empty(), egui::Button::new("🔍 Discovery"))
                    .clicked()
                {
                    self.open_discovery(ui.ctx());
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
                .add_enabled(!self.sources.is_empty(), egui::Button::new("⏱ Timeline"))
                .on_hover_text("Open the Timeline (Tools › Timeline)")
                .clicked()
            {
                self.open_timeline(ui.ctx());
            }
            if ui
                .add_enabled(!self.sources.is_empty(), egui::Button::new("🖼 Gallery"))
                .on_hover_text("Open the Image Gallery (Tools › Image Gallery)")
                .clicked()
            {
                self.open_gallery(ui.ctx());
            }
            if ui
                .add_enabled(!self.sources.is_empty(), egui::Button::new("🔍 Discovery"))
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
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let mut open_progress = false;
        ui.horizontal(|ui| {
            ui.label(format!("{} data source(s)", self.sources.len()));
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
            if let Some(n) = self.index.as_ref().and_then(SharedIndex::building_count) {
                ui.separator();
                ui.spinner();
                ui.label(format!("Indexing: {} files", crate::format::grouped(n)));
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

    fn show_progress(&mut self, ctx: &egui::Context) {
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

    fn tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.heading("Data Sources");
            if self.sources.is_empty() && self.loading.is_empty() {
                ui.weak(if self.has_case() {
                    "No data sources. Use Case › Add Data Source."
                } else {
                    "No case open. Use Case › New Case or Open Case."
                });
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
            self.views_tree(ui);
            self.results_tree(ui);
        });
    }

    /// Autopsy's Views: file types, deleted files and file sizes.
    fn views_tree(&mut self, ui: &mut egui::Ui) {
        let (case, live) = {
            let a = self.analysis.lock();
            (a.case().cloned(), a.any_active())
        };
        if case.is_none() || (self.sources.is_empty() && self.loading.is_empty()) {
            return;
        }
        let hide = self.persisted.hide_known_in_views;
        if self.was_analysing_views && !live {
            self.view_counts.invalidate();
            self.view_rows.clear();
        }
        self.was_analysing_views = live;
        let c = self.view_counts.get(ui.ctx(), case.as_ref(), live, hide);
        let n = |v: Option<&u64>| crate::format::grouped(v.copied().unwrap_or(0));
        ui.add_space(8.0);
        ui.heading("Views");
        let leaf = |app: &mut Self, ui: &mut egui::Ui, view: View, label: String| {
            app.tree_node(ui, &NodeId::View(view), &label, false, |_, _| {});
        };
        egui::CollapsingHeader::new("File Types")
            .id_salt("views-types")
            .default_open(true)
            .show(ui, |ui| {
                egui::CollapsingHeader::new("By Extension")
                    .id_salt("views-ext")
                    .default_open(true)
                    .show(ui, |ui| {
                        for (i, (name, _)) in crate::views::EXT_GROUPS.iter().enumerate() {
                            leaf(
                                self,
                                ui,
                                View::Ext(i),
                                format!("{name} ({})", n(c.ext.get(i))),
                            );
                        }
                        egui::CollapsingHeader::new(format!(
                            "Documents ({})",
                            crate::format::grouped(c.doc.iter().sum())
                        ))
                        .id_salt("views-docs")
                        .show(ui, |ui| {
                            for (i, (name, _)) in crate::views::DOC_GROUPS.iter().enumerate() {
                                leaf(
                                    self,
                                    ui,
                                    View::Doc(i),
                                    format!("{name} ({})", n(c.doc.get(i))),
                                );
                            }
                        });
                    });
                egui::CollapsingHeader::new("By MIME Type")
                    .id_salt("views-mime")
                    .show(ui, |ui| {
                        if c.mime.is_empty() {
                            ui.weak("Run File Type Identification to populate.");
                        }
                        let mut media = String::new();
                        for (mime, count) in &c.mime {
                            let (top, sub) = mime.split_once('/').unwrap_or((mime.as_str(), ""));
                            if top != media {
                                top.clone_into(&mut media);
                                ui.strong(&media);
                            }
                            leaf(
                                self,
                                ui,
                                View::Mime(mime.clone()),
                                format!("{sub} ({})", crate::format::grouped(*count)),
                            );
                        }
                    });
            });
        egui::CollapsingHeader::new(format!("Deleted Files ({})", n(c.deleted.get(1))))
            .id_salt("views-deleted")
            .show(ui, |ui| {
                leaf(
                    self,
                    ui,
                    View::Deleted(true),
                    format!("File System ({})", n(c.deleted.first())),
                );
                leaf(
                    self,
                    ui,
                    View::Deleted(false),
                    format!("All ({})", n(c.deleted.get(1))),
                );
            });
        egui::CollapsingHeader::new("File Size")
            .id_salt("views-size")
            .show(ui, |ui| {
                for (i, (name, ..)) in crate::views::SIZE_BUCKETS.iter().enumerate() {
                    leaf(
                        self,
                        ui,
                        View::Size(i),
                        format!("{name} ({})", n(c.size.get(i))),
                    );
                }
            });
    }

    fn view_table(&mut self, ui: &mut egui::Ui, view: &View) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let hide = self.persisted.hide_known_in_views;
        let task = self.view_rows.entry(view.clone()).or_insert_with(|| {
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
        let ev = self.table.ui(ui, &data.rows, &self.prefs);
        if let Some(rec) = ev.selected.and_then(|i| data.records.get(i)) {
            let content = self.content_for(&case, rec);
            self.viewer.set_content(content);
        }
        if let Some(rec) = ev.activated.and_then(|i| data.records.get(i))
            && let Some(c) = self.content_for(&case, rec)
        {
            self.open_popout(c);
        }
    }

    /// "Data Artifacts" and "Analysis Results" with per-type counts.
    fn results_tree(&mut self, ui: &mut egui::Ui) {
        let (case, live) = {
            let a = self.analysis.lock();
            (a.case().cloned(), a.any_active())
        };
        if case.is_none() {
            return;
        }
        if self.was_analysing && !live {
            // Final counts and rows once analysis has finished.
            self.result_counts.invalidate();
            self.result_rows.clear();
        }
        self.was_analysing = live;
        let counts = self.result_counts.get(ui.ctx(), case.as_ref(), live);
        // Cached rows go stale while results are still being added.
        for t in counts.iter() {
            let stale = self
                .result_rows
                .get_mut(&t.type_id)
                .and_then(Task::poll)
                .and_then(|r| r.as_ref().ok())
                .is_some_and(|rows| {
                    rows.rows.len() < (t.count as usize).min(crate::results::MAX_ROWS)
                });
            if stale {
                self.result_rows.remove(&t.type_id);
            }
        }
        for (heading, cat) in [
            ("Data Artifacts", pm_casedb::catalog::Category::DataArtifact),
            (
                "Analysis Results",
                pm_casedb::catalog::Category::AnalysisResult,
            ),
        ] {
            ui.add_space(8.0);
            ui.heading(heading);
            let of_cat: Vec<_> = counts
                .iter()
                .filter(|t| t.category == cat)
                .cloned()
                .collect();
            if of_cat.is_empty() {
                ui.weak(if live {
                    "Analysis is running…"
                } else {
                    "None"
                });
            }
            for t in of_cat {
                let node = NodeId::Results { type_id: t.type_id };
                let label = format!("{} ({})", t.display, crate::format::grouped(t.count));
                self.tree_node(ui, &node, &label, false, |_, _| {});
            }
        }
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
            NodeId::Results { type_id } => self.results_view(ui, *type_id),
            NodeId::View(v) => self.view_table(ui, v),
        }
    }

    fn results_view(&mut self, ui: &mut egui::Ui, type_id: i64) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let task = self.result_rows.entry(type_id).or_insert_with(|| {
            let case = Arc::clone(&case);
            Task::spawn(ui.ctx(), move || {
                crate::results::load_rows(&case, type_id).map(Arc::new)
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
        let title = case
            .db()
            .artifact_type_names()
            .ok()
            .and_then(|n| n.into_iter().find(|t| t.0 == type_id).map(|t| t.2))
            .unwrap_or_default();
        if let Some(i) =
            crate::results::table_ui(ui, &title, &rows, &self.prefs, self.result_selected)
        {
            self.result_selected = Some(i);
            let content = rows
                .rows
                .get(i)
                .and_then(|r| r.file.as_ref())
                .and_then(|f| self.content_for(&case, f));
            self.viewer.set_content(content);
        }
    }

    /// The evidence-backed content of a case file (for the viewers).
    fn content_for(&self, case: &pm_case::Case, f: &pm_casedb::FileRecord) -> Option<Content> {
        let ds = case.db().data_source(f.ds_id).ok()??;
        let fs_rec = case.db().file_system(f.fs_id?).ok()??;
        let first = PathBuf::from(ds.paths.first()?);
        let src = self
            .sources
            .iter()
            .find(|s| s.evidence.image.info.paths.first() == Some(&first))?;
        let found = src
            .evidence
            .filesystems
            .iter()
            .find(|x| x.offset == fs_rec.byte_offset)?;
        let mut row = FileRow::from_fs(&*found.fs, f.inode?, &f.path(), f.name_alloc);
        row.name.clone_from(&f.name);
        row.stream = f.stream;
        row.size = f.size;
        Some(Content {
            fs: Arc::clone(&found.fs),
            row,
        })
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
        let Some(d) = self.sources.get(ds).cloned() else {
            return;
        };
        let index = self.shared_index(ui.ctx());
        match self.summary.ui(ui, &d, &index, &self.prefs) {
            Some(summary::Action::ShowFile(c)) => self.viewer.set_content(Some(*c)),
            Some(summary::Action::ViewInTimeline { ds, range }) => {
                if let Some(tl) = self.open_timeline(ui.ctx()) {
                    tl.lock().focus_data_source(ds, range);
                }
            }
            None => {}
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

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(w) = self.add_wizard.as_mut() {
            match w.ui(ctx) {
                crate::add_source::AddAction::None => {}
                crate::add_source::AddAction::Cancel => self.add_wizard = None,
                crate::add_source::AddAction::Start {
                    image,
                    load,
                    settings,
                    prefs,
                } => {
                    self.add_wizard = None;
                    self.persisted.ingest = prefs;
                    self.add_data_source_with(ctx, image, load, settings);
                }
            }
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
        if !self.sources.is_empty() {
            // Index eagerly so analysis tools open instantly.
            self.shared_index(&ctx);
        }
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
        self.show_gallery(&ctx);
        self.show_discovery(&ctx);
        self.show_progress(&ctx);
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
