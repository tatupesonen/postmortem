//! The postmortem main window.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use pm_evidence::OpenOptions;

use crate::analysis::Analysis;
use crate::case_ui::{NewCaseWizard, WelcomeAction, WizardAction};
use crate::discovery::DiscoveryWindow;
use crate::format::{DisplayPrefs, size_long};
use crate::gallery::GalleryWindow;
use crate::icons;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow, NodeId};
use crate::summary::{self, SummaryState};
use crate::table::TableState;
use crate::task::{self, Task};
use crate::timeline::TimelineWindow;
use crate::viewers::ViewerPanel;
use crate::views::View;

type Listing = Result<Arc<Vec<FileRow>>, String>;

/// A tab of the result pane (Autopsy opens each search in its own result
/// window next to the main "Listing").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    /// What is selected in the tree.
    Listing,
    FileSearch(usize),
    KeywordSearch(usize),
}

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
    /// The user's tag names (Options › Tags), added to every opened case.
    pub custom_tags: Vec<crate::tags_ui::TagDefinition>,
    /// Plugins folder (`None`: `$POSTMORTEM_PLUGINS` or
    /// `~/postmortem/Plugins`).
    pub plugins_dir: Option<PathBuf>,
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
            custom_tags: Vec::new(),
            plugins_dir: None,
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
    result_rows:
        HashMap<crate::results::ResultKey, Task<Result<Arc<crate::results::ResultRows>, String>>>,
    /// Keyword Hits groups, and the hit count they were loaded for.
    keyword_groups: crate::task::Cached<crate::keyword_search_ui::HitTree>,
    keyword_groups_for: u64,
    result_selected: Option<usize>,
    result_table: crate::results::ResultTable,
    /// Analysis was running last frame (refresh results when it stops).
    was_analysing: bool,
    was_analysing_views: bool,
    add_wizard: Option<crate::add_source::AddSourceWizard>,
    /// Case folder and data-source count the hash resolver was built for.
    resolver_key: Option<(PathBuf, usize)>,
    view_counts: crate::views::Counts,
    view_rows: HashMap<crate::views::View, Task<Result<Arc<crate::views::ViewRows>, String>>>,
    /// The Data Sources tree structure from the case database.
    case_sources: crate::case_tree::Sources,
    tagging: crate::tags_ui::Tagging,
    /// Counts of the OS Accounts, Tags and Score nodes.
    case_summary: crate::task::Cached<crate::case_nodes::Summary>,
    /// Rows of the OS Accounts, Tags and Score views.
    node_views: HashMap<NodeId, crate::task::Cached<crate::case_nodes::ViewData>>,
    node_grid: crate::grid::Grid,
    /// Details of the selected OS account.
    account_details: Option<(i64, crate::task::Cached<crate::case_nodes::Details>)>,
    file_search: Option<crate::file_search_ui::FileSearchWindow>,
    /// The toolbar's Keyword Search panel, when open.
    keyword_panel: Option<crate::keyword_search_ui::KeywordSearchPanel>,
    /// "Files Indexed" for the panel.
    keyword_indexed: crate::task::Cached<u64>,
    keyword_searches: Vec<crate::keyword_search_ui::KeywordResults>,
    result_tab: ResultTab,
    /// Searches run in this case, for numbering (Autopsy never reuses one).
    searches_run: (usize, usize),
    run_ingest: Option<crate::add_source::RunIngestDialog>,
    hosts_ui: crate::hosts_ui::HostsUi,
    options: Option<crate::options_ui::OptionsWindow>,
    /// Tools › Plugins is open.
    plugins_open: bool,
    /// Generate Report: the case's choices loading, then the wizard.
    report_choices: Option<Task<Result<crate::report_ui::Choices, String>>>,
    report_wizard: Option<crate::report_ui::ReportWizard>,
    /// Tools › Geolocation.
    geo: Option<crate::geo_ui::GeoWindow>,
    /// Tools › Communications: choices loading, then the window.
    cvt_choices: Option<Task<Result<crate::cvt_ui::Choices, String>>>,
    cvt: Option<crate::cvt_ui::CvtWindow>,
    /// A data source awaiting removal confirmation `(id, name)`.
    confirm_remove: Option<(i64, String)>,
    removing: Option<Task<Result<(), String>>>,
    mime_types: crate::task::Cached<Vec<String>>,
    searches: Vec<crate::file_search_ui::SearchResults>,
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
        crate::plugins::set_dir(persisted.plugins_dir.clone());
        crate::plugins::reload(ctx);
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
            keyword_groups: crate::task::Cached::default(),
            keyword_groups_for: 0,
            result_selected: None,
            result_table: crate::results::ResultTable::default(),
            was_analysing: false,
            was_analysing_views: false,
            add_wizard: None,
            resolver_key: None,
            view_counts: crate::views::Counts::default(),
            view_rows: HashMap::new(),
            case_sources: crate::case_tree::Sources::default(),
            tagging: crate::tags_ui::Tagging::default(),
            case_summary: crate::task::Cached::default(),
            node_views: HashMap::new(),
            node_grid: crate::grid::Grid::default(),
            account_details: None,
            file_search: None,
            keyword_panel: None,
            keyword_indexed: crate::task::Cached::default(),
            keyword_searches: Vec::new(),
            result_tab: ResultTab::Listing,
            searches_run: (0, 0),
            run_ingest: None,
            hosts_ui: crate::hosts_ui::HostsUi::default(),
            options: None,
            plugins_open: false,
            report_choices: None,
            report_wizard: None,
            geo: None,
            cvt_choices: None,
            cvt: None,
            confirm_remove: None,
            removing: None,
            mime_types: crate::task::Cached::default(),
            searches: Vec::new(),
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
                    let _ = id;
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

    /// Directory listing for a node, loaded in the background from the
    /// case database.
    fn listing(&mut self, ctx: &egui::Context, node: &NodeId) -> Option<&Listing> {
        if !self.listings.contains_key(node) {
            let NodeId::Dir { file, .. } = node else {
                return None;
            };
            let case = self.analysis.lock().case().cloned()?;
            let dir = *file;
            let task = Task::spawn(ctx, move || {
                let mut rows: Vec<FileRow> = case
                    .db()
                    .children(dir)
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

    pub fn navigate(&mut self, node: NodeId) {
        self.result_tab = ResultTab::Listing;
        if self.selected.as_ref() == Some(&node) {
            return;
        }
        if let Some(prev) = self.selected.take() {
            self.back.push(prev);
        }
        self.forward.clear();
        self.selected = Some(node);
        self.table.reset();
        self.node_grid.reset();
        self.account_details = None;
        self.result_selected = None;
        self.result_table = crate::results::ResultTable::default();
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
                self.analysis.lock().case().cloned(),
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
                    self.view_counts.invalidate();
                    self.view_rows.clear();
                }
            });
            ui.menu_button("Tools", |ui| {
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
                    self.mime_types.refresh();
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
            ui.heading(format!("{} Data Sources", icons::DATA_SOURCES));
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
            let (case, live) = {
                let a = self.analysis.lock();
                (a.case().cloned(), a.any_active())
            };
            let trees = self.case_sources.get(ui.ctx(), case.as_ref(), live);
            if let Some(e) = self.case_sources.error() {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            }
            if trees.is_empty() && self.case_sources.loading() {
                ui.spinner();
            }
            if self.selected.is_none()
                && let Some(first) = trees.first()
            {
                self.navigate(NodeId::DataSource(first.ds.id));
            }
            // Autopsy's default layout: Data Sources › host › data source.
            let mut host_action = None;
            for (host, sources) in trees.by_host() {
                let node = NodeId::Host(host.map(|h| h.id));
                let label = format!(
                    "{} {}",
                    icons::HOST,
                    host.map_or("Unknown Host", |h| h.name.as_str())
                );
                let resp = self.tree_node_with(ui, &node, &label, true, true, |app, ui| {
                    for t in sources {
                        let node = NodeId::DataSource(t.ds.id);
                        let resp = app.tree_node(
                            ui,
                            &node,
                            &format!("{} {}", icons::DATA_SOURCE, t.ds.name),
                            true,
                            |app, ui| app.data_source_children(ui, t),
                        );
                        resp.context_menu(|ui| app.data_source_menu(ui, &t.ds));
                    }
                });
                if let Some(h) = host {
                    resp.context_menu(|ui| {
                        if let Some(a) = crate::hosts_ui::host_menu(ui, h, &trees) {
                            host_action = Some(a);
                        }
                    });
                }
            }
            if let (Some(a), Some(case)) = (host_action, case.as_ref()) {
                self.hosts_ui.act(ui.ctx(), case, a);
            }
            self.views_tree(ui);
            self.results_tree(ui);
            self.case_nodes_tree(ui);
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
        ui.heading(format!("{} Views", icons::VIEWS));
        let leaf = |app: &mut Self, ui: &mut egui::Ui, view: View, label: String| {
            app.tree_node(ui, &NodeId::View(view), &label, false, |_, _| {});
        };
        egui::CollapsingHeader::new(format!("{} File Types", icons::FILE_TYPES))
            .id_salt("views-types")
            .default_open(true)
            .show(ui, |ui| {
                egui::CollapsingHeader::new(format!("{} By Extension", icons::BY_EXTENSION))
                    .id_salt("views-ext")
                    .default_open(true)
                    .show(ui, |ui| {
                        for (i, (name, _)) in crate::views::EXT_GROUPS.iter().enumerate() {
                            leaf(
                                self,
                                ui,
                                View::Ext(i),
                                format!("{} {name} ({})", icons::file_group(name), n(c.ext.get(i))),
                            );
                        }
                        egui::CollapsingHeader::new(format!(
                            "{} Documents ({})",
                            icons::file_group("Documents"),
                            crate::format::grouped(c.doc.iter().sum())
                        ))
                        .id_salt("views-docs")
                        .show(ui, |ui| {
                            for (i, (name, _)) in crate::views::DOC_GROUPS.iter().enumerate() {
                                leaf(
                                    self,
                                    ui,
                                    View::Doc(i),
                                    format!(
                                        "{} {name} ({})",
                                        icons::file_group(name),
                                        n(c.doc.get(i))
                                    ),
                                );
                            }
                        });
                    });
                egui::CollapsingHeader::new(format!("{} By MIME Type", icons::BY_MIME))
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
                                ui.strong(format!("{} {media}", icons::media_type(&media)));
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
        egui::CollapsingHeader::new(format!(
            "{} Deleted Files ({})",
            icons::DELETED,
            n(c.deleted.get(1))
        ))
        .id_salt("views-deleted")
        .show(ui, |ui| {
            leaf(
                self,
                ui,
                View::Deleted(true),
                format!("{} File System ({})", icons::DELETED, n(c.deleted.first())),
            );
            leaf(
                self,
                ui,
                View::Deleted(false),
                format!("{} All ({})", icons::DELETED, n(c.deleted.get(1))),
            );
        });
        egui::CollapsingHeader::new(format!("{} File Size", icons::FILE_SIZE))
            .id_salt("views-size")
            .show(ui, |ui| {
                for (i, (name, ..)) in crate::views::SIZE_BUCKETS.iter().enumerate() {
                    leaf(
                        self,
                        ui,
                        View::Size(i),
                        format!("{} {name} ({})", icons::FILE_SIZE, n(c.size.get(i))),
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
        let case = self.analysis.lock().case().cloned();
        let tags = self.tagging.catalog.get(ui.ctx(), case.as_ref());
        let ev = self.table.ui(ui, &data.rows, &self.prefs, Some(&tags));
        if let (Some((i, req)), Some(case)) = (ev.tag, case.as_ref())
            && let Some(id) = data.rows.get(i).and_then(FileRow::file_id)
        {
            self.tagging.request(
                ui.ctx(),
                case,
                crate::tags_ui::TagTarget::Files(vec![id]),
                req,
            );
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
            // New artifacts (web history, …) become timeline events.
            if let Some(t) = &self.timeline {
                t.lock().invalidate();
            }
            // Hashes were just computed.
            self.viewer.refresh_hashes();
            // Derived files and data sources may have been added.
            self.case_sources.refresh();
            self.listings.clear();
            self.case_summary.refresh();
            self.node_views.clear();
        }
        self.was_analysing = live;
        let counts = self.result_counts.get(ui.ctx(), case.as_ref(), live);
        // Cached rows go stale while results are still being added.
        for t in counts.iter() {
            let stale = self
                .result_rows
                .get_mut(&crate::results::ResultKey::Type(t.type_id))
                .and_then(Task::poll)
                .and_then(|r| r.as_ref().ok())
                .is_some_and(|rows| {
                    rows.rows.len() < (t.count as usize).min(crate::results::MAX_ROWS)
                });
            if stale {
                self.result_rows
                    .remove(&crate::results::ResultKey::Type(t.type_id));
            }
        }
        for (icon, heading, cat) in [
            (
                icons::DATA_ARTIFACTS,
                "Data Artifacts",
                pm_casedb::catalog::Category::DataArtifact,
            ),
            (
                icons::ANALYSIS_RESULTS,
                "Analysis Results",
                pm_casedb::catalog::Category::AnalysisResult,
            ),
        ] {
            ui.add_space(8.0);
            ui.heading(format!("{icon} {heading}"));
            let of_cat: Vec<_> = counts
                .iter()
                .filter(|t| t.category == cat)
                .cloned()
                .collect();
            let analysis = cat == pm_casedb::catalog::Category::AnalysisResult;
            // OS Accounts sits under Analysis Results, so that section is
            // never empty.
            if of_cat.is_empty() && !analysis {
                ui.weak(if live {
                    "Analysis is running…"
                } else {
                    "None"
                });
            }
            for t in of_cat {
                let node = NodeId::Results { type_id: t.type_id };
                let icon =
                    icons::artifact(&t.name, cat == pm_casedb::catalog::Category::AnalysisResult);
                let label = format!("{icon} {} ({})", t.display, crate::format::grouped(t.count));
                if t.name == "TSK_KEYWORD_HIT" {
                    self.keyword_hits_node(ui, &node, &label, case.as_ref(), t.count);
                } else {
                    self.tree_node(ui, &node, &label, false, |_, _| {});
                }
            }
            if analysis {
                self.os_accounts_node(ui, case.as_ref());
            }
        }
    }

    /// Keyword Hits › list › keyword › match (Autopsy `KeywordHits`).
    fn keyword_hits_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        case: Option<&Arc<pm_case::Case>>,
        total: u64,
    ) {
        use crate::keyword_search_ui::MAX_TREE_CHILDREN;
        use pm_casedb::keyword_hits::KeywordHitFilter;
        let Some(case) = case else {
            return;
        };
        if total != self.keyword_groups_for {
            self.keyword_groups_for = total;
            self.keyword_groups.refresh();
            self.result_rows
                .retain(|k, _| !matches!(k, crate::results::ResultKey::Keyword(_)));
        }
        let loader = Arc::clone(case);
        let tree = self
            .keyword_groups
            .get(ui.ctx(), move || {
                let groups = loader
                    .db()
                    .keyword_hit_groups()
                    .map_err(|e| e.to_string())?;
                Ok(crate::keyword_search_ui::HitTree::build(&groups))
            })
            .unwrap_or_default();
        let filter = |list: &str, term: Option<&str>, inst: Option<&str>| {
            NodeId::KeywordHits(KeywordHitFilter {
                list: list.to_owned(),
                term: term.map(str::to_owned),
                instance: inst.map(str::to_owned),
            })
        };
        let more = |ui: &mut egui::Ui, n: usize| {
            if n > MAX_TREE_CHILDREN {
                ui.weak(format!(
                    "… {} more (listed in the parent's results)",
                    crate::format::grouped((n - MAX_TREE_CHILDREN) as u64)
                ));
            }
        };
        self.tree_node(ui, node, label, true, |app, ui| {
            for l in &tree.lists {
                let node = filter(&l.name, None, None);
                app.tree_node(ui, &node, &l.label, !l.terms.is_empty(), |app, ui| {
                    for t in l.terms.iter().take(MAX_TREE_CHILDREN) {
                        let node = filter(&l.name, Some(&t.term), None);
                        let regex = !t.instances.is_empty();
                        app.tree_node(ui, &node, &t.label, regex, |app, ui| {
                            for (inst, label) in t.instances.iter().take(MAX_TREE_CHILDREN) {
                                let node = filter(&l.name, Some(&t.term), Some(inst));
                                app.tree_node(ui, &node, label, false, |_, _| {});
                            }
                            more(ui, t.instances.len());
                        });
                    }
                    more(ui, l.terms.len());
                });
            }
        });
    }

    /// The OS Accounts node (its count comes with the case summary).
    fn os_accounts_node(&mut self, ui: &mut egui::Ui, case: Option<&Arc<pm_case::Case>>) {
        let Some(case) = case else {
            return;
        };
        let loader = Arc::clone(case);
        let count = self
            .case_summary
            .get(ui.ctx(), move || crate::case_nodes::load_summary(&loader))
            .map(|s| crate::format::grouped(s.os_accounts));
        let label = format!(
            "{} OS Accounts ({})",
            icons::OS_ACCOUNTS,
            count.as_deref().unwrap_or("…")
        );
        self.tree_node(ui, &NodeId::OsAccounts, &label, false, |_, _| {});
    }

    /// A host node: its data sources (Autopsy lists the node's children).
    fn host_view(&mut self, ui: &mut egui::Ui, host: Option<i64>) {
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ui.ctx(), case.as_ref(), false);
        let name = host
            .and_then(|id| tree.hosts.iter().find(|h| h.id == id))
            .map_or("Unknown Host", |h| h.name.as_str());
        ui.heading(format!("{} {name}", icons::HOST));
        let mut open = None;
        for t in tree
            .iter()
            .filter(|t| t.host.as_ref().map(|h| h.id) == host)
        {
            if ui
                .selectable_label(false, format!("{} {}", icons::DATA_SOURCE, t.ds.name))
                .double_clicked()
            {
                open = Some(t.ds.id);
            }
        }
        if let Some(ds) = open {
            self.navigate(NodeId::DataSource(ds));
        }
    }

    /// Autopsy's data source node actions.
    fn data_source_menu(&mut self, ui: &mut egui::Ui, ds: &pm_casedb::DataSourceRecord) {
        let busy = self.analysis.lock().any_active();
        if ui
            .add_enabled(!busy, egui::Button::new("Run Ingest Modules"))
            .on_disabled_hover_text("Ingest is running")
            .clicked()
        {
            self.run_ingest = Some(crate::add_source::RunIngestDialog {
                ds_id: ds.id,
                ds_name: ds.name.clone(),
                prefs: self.persisted.ingest.clone(),
            });
            ui.close();
        }
        if ui.button("Open File Search by Attributes").clicked() {
            let w = self
                .file_search
                .get_or_insert_with(crate::file_search_ui::FileSearchWindow::default);
            w.preselect_data_source(ds.id);
            self.mime_types.refresh();
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(!busy, egui::Button::new("Remove Data Source"))
            .on_disabled_hover_text("Cannot remove a data source while ingest is running")
            .clicked()
        {
            self.confirm_remove = Some((ds.id, ds.name.clone()));
            ui.close();
        }
    }

    /// Tags and Score (Autopsy's tree order after the results).
    fn case_nodes_tree(&mut self, ui: &mut egui::Ui) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let loader = Arc::clone(&case);
        let Some(sum) = self
            .case_summary
            .get(ui.ctx(), move || crate::case_nodes::load_summary(&loader))
        else {
            return;
        };
        ui.add_space(8.0);
        ui.heading(format!("{} Tags", icons::TAGS));
        if sum.tags.is_empty() {
            ui.weak("None");
        }
        for t in &sum.tags {
            egui::CollapsingHeader::new(format!("{} ({})", t.label, t.files + t.results))
                .id_salt(("tag-name", t.id))
                .show(ui, |ui| {
                    self.tree_node(
                        ui,
                        &NodeId::TagFiles(t.id),
                        &format!("File Tags ({})", t.files),
                        false,
                        |_, _| {},
                    );
                    self.tree_node(
                        ui,
                        &NodeId::TagResults(t.id),
                        &format!("Result Tags ({})", t.results),
                        false,
                        |_, _| {},
                    );
                });
        }
        ui.add_space(8.0);
        ui.heading(format!("{} Score", icons::SCORE));
        self.tree_node(
            ui,
            &NodeId::Score { bad: true },
            &format!(
                "{} Bad Items ({})",
                icons::SCORE_NOTABLE,
                crate::format::grouped(sum.bad)
            ),
            false,
            |_, _| {},
        );
        self.tree_node(
            ui,
            &NodeId::Score { bad: false },
            &format!(
                "{} Suspicious Items ({})",
                icons::SCORE_SUSPICIOUS,
                crate::format::grouped(sum.suspicious)
            ),
            false,
            |_, _| {},
        );
        // Autopsy's Reports node: generated reports, opened on click.
        ui.add_space(8.0);
        ui.heading(format!("{} Reports", icons::REPORTS));
        if sum.reports.is_empty() {
            ui.weak("None");
        }
        let mut remove = None;
        for r in &sum.reports {
            let when = self
                .prefs
                .time(jiff::Timestamp::from_second(r.created_at).ok());
            let path = std::path::PathBuf::from(&r.path);
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let resp = ui
                    .selectable_label(false, format!("{} {when}", r.display_name))
                    .on_hover_text(&r.path);
                if resp.clicked() {
                    crate::platform::open(&path);
                }
                resp.context_menu(|ui| {
                    if ui.button("Open Report").clicked() {
                        crate::platform::open(&path);
                        ui.close();
                    }
                    if ui.button("Open Containing Folder").clicked() {
                        if let Some(dir) = path.parent() {
                            crate::platform::open(dir);
                        }
                        ui.close();
                    }
                    if ui.button("Delete Report").clicked() {
                        remove = Some(r.id);
                        ui.close();
                    }
                });
            });
        }
        if let Some(id) = remove
            && let Err(e) = case.db().delete_report(id)
        {
            self.errors.push(e.to_string());
        }
        if remove.is_some() {
            self.case_summary.refresh();
        }
    }

    /// Tools › Communications: loads the filter choices in the background.
    fn open_communications(&mut self, ctx: &egui::Context) {
        if let Some(case) = self.analysis.lock().case().cloned() {
            self.cvt_choices = Some(Task::spawn(ctx, move || {
                crate::cvt_ui::Choices::load(&case)
            }));
        }
    }

    fn communications(&mut self, ctx: &egui::Context) {
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
    fn open_geolocation(&mut self, ctx: &egui::Context) {
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ctx, case.as_ref(), false);
        let sources = tree.iter().map(|t| (t.ds.id, t.ds.name.clone())).collect();
        self.geo = Some(crate::geo_ui::GeoWindow::new(sources));
    }

    fn geolocation(&mut self, ctx: &egui::Context) {
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
    fn open_report_wizard(&mut self, ctx: &egui::Context) {
        if let Some(case) = self.analysis.lock().case().cloned() {
            self.report_choices = Some(Task::spawn(ctx, move || {
                crate::report_ui::Choices::load(&case)
            }));
        }
    }

    fn report_wizard(&mut self, ctx: &egui::Context) {
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
            crate::report_ui::WizardAction::None => {}
            crate::report_ui::WizardAction::Close => self.report_wizard = None,
            crate::report_ui::WizardAction::Finished => self.case_summary.refresh(),
        }
    }

    /// The table of an OS Accounts, Tags or Score node.
    fn node_view(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let loader = Arc::clone(&case);
        let prefs = self.prefs.clone();
        let target = node.clone();
        let data =
            self.node_views
                .entry(node.clone())
                .or_default()
                .get(&ctx, move || match target {
                    NodeId::TagFiles(t) => crate::case_nodes::load_tag_files(&loader, t, &prefs),
                    NodeId::TagResults(t) => crate::case_nodes::load_tag_results(&loader, t),
                    NodeId::Score { bad } => crate::case_nodes::load_score(&loader, bad),
                    _ => crate::case_nodes::load_os_accounts(&loader, &prefs),
                });
        let Some(data) = data else {
            if let Some(e) = self.node_views.get(node).and_then(|c| c.error()) {
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
                    self.node_grid.ui(ui, "node-view", &data.grid, |ui, i| {
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
            self.tagging
                .request(&ctx, &case, target, crate::tags_ui::TagRequest::Remove(tag));
        }
        let file_of = |r: &crate::case_nodes::RowRef| match r {
            crate::case_nodes::RowRef::File { file, .. } => Some(file.clone()),
            crate::case_nodes::RowRef::Artifact { file, .. } => file.clone(),
            crate::case_nodes::RowRef::OsAccount(_) => None,
        };
        if let Some(r) = ev.selected.and_then(|i| refs.get(i)) {
            if let crate::case_nodes::RowRef::OsAccount(id) = r {
                self.account_details = Some((*id, crate::task::Cached::default()));
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
        if let Some((id, details)) = &mut self.account_details {
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

    /// One collapsible tree node with a selectable label; children render lazily.
    fn tree_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        has_children: bool,
        children: impl FnOnce(&mut Self, &mut egui::Ui),
    ) -> egui::Response {
        self.tree_node_with(ui, node, label, has_children, false, children)
    }

    /// [`Self::tree_node`] with a choice of initial expansion.
    fn tree_node_with(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        has_children: bool,
        default_open: bool,
        children: impl FnOnce(&mut Self, &mut egui::Ui),
    ) -> egui::Response {
        let id = ui.make_persistent_id(node);
        let selected = self.selected.as_ref() == Some(node);
        if has_children {
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
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
            header.inner
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let r = ui.selectable_label(selected, label);
                if r.clicked() {
                    self.navigate(node.clone());
                }
                r
            })
            .inner
        }
    }

    fn data_source_children(&mut self, ui: &mut egui::Ui, t: &crate::case_tree::DsTree) {
        for v in &t.volumes {
            let label = format!(
                "{} {}",
                if v.unallocated() {
                    icons::UNALLOCATED
                } else {
                    icons::VOLUME
                },
                v.label()
            );
            let node = NodeId::Volume {
                ds: t.ds.id,
                volume: v.volume.id,
            };
            self.tree_node(ui, &node, &label, !v.roots.is_empty(), |app, ui| {
                for r in &v.roots {
                    app.fs_root_children(ui, r);
                }
            });
        }
        if t.volumes.is_empty() && t.loose.is_empty() {
            ui.weak("No file system found");
        }
        for r in &t.loose {
            self.fs_root_children(ui, r);
        }
    }

    fn fs_root_children(&mut self, ui: &mut egui::Ui, r: &crate::case_tree::FsRoot) {
        let root = NodeId::Dir {
            ds: r.fs.ds_id,
            file: r.root.id,
        };
        let label = format!("{} / ({})", icons::FOLDER, r.fs.kind);
        self.tree_node(ui, &root, &label, true, |app, ui| {
            app.dir_children(ui, &root);
        });
    }

    fn dir_children(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let NodeId::Dir { ds, .. } = node else {
            return;
        };
        let ds = *ds;
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
                let mut dirs: Vec<&FileRow> = rows.iter().filter(|r| r.can_open()).collect();
                dirs.sort_by_key(|r| r.name.to_lowercase());
                for r in dirs {
                    let Some(file) = r.file_id() else { continue };
                    let child = NodeId::Dir { ds, file };
                    let icon = if r.is_dir() {
                        icons::FOLDER
                    } else {
                        icons::ARCHIVE
                    };
                    let label = if r.deleted() {
                        format!("{icon} {} ✖", r.name)
                    } else {
                        format!("{icon} {}", r.name)
                    };
                    self.tree_node(ui, &child, &label, true, |app, ui| {
                        app.dir_children(ui, &child);
                    });
                }
            }
        }
    }

    fn results(&mut self, ui: &mut egui::Ui) {
        if self.has_case() {
            self.result_tabs(ui);
        }
        match self.result_tab {
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
            NodeId::Dir { .. } => self.file_table(ui, &node),
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

    fn results_view(&mut self, ui: &mut egui::Ui, key: &crate::results::ResultKey) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let task = self.result_rows.entry(key.clone()).or_insert_with(|| {
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
                .result_counts
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
        if let Some((i, which, req)) = ev.tag
            && let Some(r) = rows.rows.get(i)
        {
            let target = match which {
                crate::results::TagFor::Result => {
                    Some(crate::tags_ui::TagTarget::Artifacts(vec![r.artifact.id]))
                }
                crate::results::TagFor::SourceFile => r
                    .file
                    .as_ref()
                    .map(|f| crate::tags_ui::TagTarget::Files(vec![f.id])),
            };
            if let Some(target) = target {
                self.tagging.request(ui.ctx(), &case, target, req);
            }
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
    fn content_for(&self, f: &pm_casedb::FileRecord) -> Option<Content> {
        let case = self.analysis.lock().case().cloned()?;
        Some(Content::from_case(case, crate::views::row_of(f)))
    }

    fn file_table(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let ctx = ui.ctx().clone();
        let NodeId::Dir { ds, .. } = *node else {
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
        let ev = self.table.ui(ui, &rows, &self.prefs, Some(&tags));
        if let Some((r, req)) = ev.tag.as_ref().and_then(|(i, q)| Some((rows.get(*i)?, *q)))
            && let Some(id) = r.file_id()
        {
            self.tagging
                .request(&ctx, &case, crate::tags_ui::TagTarget::Files(vec![id]), req);
        }
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
        if let Some(r) = ev.popout.and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
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
            self.export(&ctx, content(r));
        }
    }

    /// The opened evidence of a case data source (matched by image path),
    /// for the tools still on the in-memory index.
    fn evidence_index(&self, ds_id: i64) -> Option<usize> {
        let first = self.case_sources.find(ds_id)?.ds.paths.first()?;
        let first = std::path::Path::new(first);
        self.sources
            .iter()
            .position(|s| s.evidence.image.info.paths.first().map(PathBuf::as_path) == Some(first))
    }

    fn data_source_summary(&mut self, ui: &mut egui::Ui, ds_id: i64) {
        let Some(d) = self
            .evidence_index(ds_id)
            .and_then(|i| self.sources.get(i).cloned())
        else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Opening the data source…");
            });
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

    fn volume_summary(&mut self, ui: &mut egui::Ui, ds_id: i64, volume_id: i64) {
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
        let (open, back, fwd, bookmark) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::O),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight),
                i.consume_shortcut(&crate::tags_ui::bookmark_shortcut()),
            )
        });
        // Ctrl+B: bookmark the selected file (Autopsy tags selected results
        // instead when the result view has a selection).
        if bookmark && let Some(case) = self.analysis.lock().case().cloned() {
            let result = match (&self.selected, self.result_selected) {
                (Some(NodeId::Results { type_id }), Some(i)) => self
                    .result_rows
                    .get_mut(&crate::results::ResultKey::Type(*type_id))
                    .and_then(Task::poll)
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|rows| rows.rows.get(i))
                    .map(|r| r.artifact.id),
                _ => None,
            };
            let target = match result {
                Some(a) => Some(crate::tags_ui::TagTarget::Artifacts(vec![a])),
                None => self
                    .viewer
                    .content()
                    .and_then(|c| c.row.file_id())
                    .map(|f| crate::tags_ui::TagTarget::Files(vec![f])),
            };
            if let Some(t) = target {
                self.tagging.bookmark(ctx, &case, t);
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

    /// The File Search by Attributes window.
    fn file_search_window(&mut self, ctx: &egui::Context) {
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
                self.searches_run.0 += 1;
                let number = self.searches_run.0;
                self.searches.push(crate::file_search_ui::SearchResults {
                    number,
                    rows: crate::task::Cached::default(),
                    search: Arc::from(search),
                    table: crate::table::TableState::default(),
                });
                self.result_tab = ResultTab::FileSearch(number);
            }
        }
    }

    /// Autopsy's "Remove Data Source" confirmation and the removal task.
    fn remove_data_source_dialog(&mut self, ctx: &egui::Context) {
        if let Some(t) = &mut self.removing
            && let Some(r) = t.poll()
        {
            if let Err(e) = r {
                self.errors.push(e.clone());
            }
            self.removing = None;
            // Everything shown from the case may refer to removed rows.
            self.case_sources.invalidate();
            self.listings.clear();
            self.view_counts.invalidate();
            self.view_rows.clear();
            self.result_counts.invalidate();
            self.result_rows.clear();
            self.case_summary.refresh();
            self.node_views.clear();
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

    /// The toolbar's Keyword Search panel.
    fn keyword_panel(&mut self, ctx: &egui::Context) {
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
                self.searches_run.1 += 1;
                let number = self.searches_run.1;
                self.keyword_searches
                    .push(crate::keyword_search_ui::KeywordResults {
                        number,
                        request: Arc::new(req),
                        rows: crate::task::Cached::default(),
                        reported: false,
                        table: crate::table::TableState::default(),
                    });
                self.result_tab = ResultTab::KeywordSearch(number);
            }
        }
    }

    /// The result pane's tabs: Listing, then one per search (closable).
    fn result_tabs(&mut self, ui: &mut egui::Ui) {
        let mut tabs = vec![(ResultTab::Listing, "Listing".to_owned())];
        tabs.extend(self.searches.iter().map(|s| {
            (
                ResultTab::FileSearch(s.number),
                format!("File Search Results {}", s.number),
            )
        }));
        tabs.extend(
            self.keyword_searches
                .iter()
                .map(|s| (ResultTab::KeywordSearch(s.number), s.title())),
        );
        if tabs.len() == 1 {
            return;
        }
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (tab, title) in tabs {
                let on = self.result_tab == tab;
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if ui.selectable_label(on, &title).clicked() {
                        self.result_tab = tab;
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
                ResultTab::FileSearch(n) => self.searches.retain(|s| s.number != n),
                ResultTab::KeywordSearch(n) => self.keyword_searches.retain(|s| s.number != n),
                ResultTab::Listing => {}
            }
            if self.result_tab == tab {
                self.result_tab = ResultTab::Listing;
            }
        }
    }

    /// "Keyword search {n} - {term}".
    fn keyword_view(&mut self, ui: &mut egui::Ui, number: usize) {
        let ctx = ui.ctx().clone();
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let Some(s) = self
            .keyword_searches
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
        if !s.reported && data.saved > 0 {
            // Saved hits appear under Analysis Results › Keyword Hits.
            s.reported = true;
            self.result_counts.invalidate();
            self.result_rows.clear();
            self.case_summary.refresh();
        }
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
        let ev = s.table.ui_with(
            ui,
            rows,
            &self.prefs,
            Some(&tags),
            Some(("Keyword Preview", &data.previews)),
        );
        if let Some((i, req)) = ev.tag
            && let Some(id) = rows.get(i).and_then(FileRow::file_id)
        {
            self.tagging
                .request(&ctx, &case, crate::tags_ui::TagTarget::Files(vec![id]), req);
        }
        let content = |r: &FileRow| Content::from_case(Arc::clone(&case), r.clone());
        if let Some(r) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.show(Some(content(r)), Some(query));
        }
        if let Some(r) = ev.activated.or(ev.popout).and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
        }
        if let Some(r) = ev.export.and_then(|i| rows.get(i)) {
            self.export(&ctx, content(r));
        }
    }

    /// "File Search Results {n}".
    fn search_view(&mut self, ui: &mut egui::Ui, number: usize) {
        let ctx = ui.ctx().clone();
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let Some(s) = self.searches.iter_mut().find(|s| s.number == number) else {
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
        let ev = s.table.ui(ui, rows, &self.prefs, Some(&tags));
        if let Some((i, req)) = ev.tag
            && let Some(id) = rows.get(i).and_then(FileRow::file_id)
        {
            self.tagging
                .request(&ctx, &case, crate::tags_ui::TagTarget::Files(vec![id]), req);
        }
        let content = |r: &FileRow| Content::from_case(Arc::clone(&case), r.clone());
        if let Some(r) = ev.selected.and_then(|i| rows.get(i)) {
            self.viewer.set_content(Some(content(r)));
        }
        if let Some(r) = ev.activated.or(ev.popout).and_then(|i| rows.get(i)) {
            self.open_popout(content(r));
        }
        if let Some(r) = ev.export.and_then(|i| rows.get(i)) {
            self.export(&ctx, content(r));
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        self.file_search_window(ctx);
        self.keyword_panel(ctx);
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ctx, case.as_ref(), false);
        let errors = self.hosts_ui.ui(ctx, case.as_ref(), &tree);
        self.errors.extend(errors);
        if self.hosts_ui.take_changed() {
            self.case_sources.refresh();
        }
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
        if let Some(w) = self.add_wizard.as_mut() {
            match w.ui(ctx) {
                crate::add_source::AddAction::None => {}
                crate::add_source::AddAction::Cancel => self.add_wizard = None,
                crate::add_source::AddAction::Start(req) => {
                    let crate::add_source::StartRequest {
                        image,
                        load,
                        settings,
                        prefs,
                    } = *req;
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

    /// Lets the content viewer show hashes from the case database: maps a
    /// browsed file system to its case file system (same image, same byte
    /// offset) and looks the file up by inode/stream (indexed).
    fn update_hash_resolver(&mut self) {
        let case = self.analysis.lock().case().cloned();
        let key = case
            .as_ref()
            .map(|c| (c.dir().to_path_buf(), self.sources.len()));
        if key == self.resolver_key {
            return;
        }
        self.resolver_key = key;
        let Some(case) = case else {
            self.prefs.case = None;
            self.viewer.refresh_hashes();
            return;
        };
        let fss: Vec<(Arc<dyn pm_fs::FileSystem>, PathBuf, u64)> = self
            .sources
            .iter()
            .flat_map(|s| {
                let path = s
                    .evidence
                    .image
                    .info
                    .paths
                    .first()
                    .cloned()
                    .unwrap_or_default();
                s.evidence
                    .filesystems
                    .iter()
                    .map(move |f| (Arc::clone(&f.fs), path.clone(), f.offset))
            })
            .collect();
        let resolver = move |c: &Content| -> Option<crate::viewers::CaseFileInfo> {
            let db = case.db();
            let f = match &c.origin {
                // Re-read the row: hashes may have been computed since it
                // was listed.
                crate::model::ContentOrigin::Case(_) => db.file(c.row.file_id()?).ok()??,
                crate::model::ContentOrigin::Fs(open) => {
                    let same = |a: &Arc<dyn pm_fs::FileSystem>| {
                        std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(open))
                    };
                    let (_, path, offset) = fss.iter().find(|(fs, ..)| same(fs))?;
                    let ds = db
                        .data_sources()
                        .ok()?
                        .into_iter()
                        .find(|d| d.paths.first().map(PathBuf::from).as_ref() == Some(path))?;
                    let fs = db
                        .file_systems(ds.id)
                        .ok()?
                        .into_iter()
                        .find(|f| f.byte_offset == *offset)?;
                    db.file_by_inode(fs.id, c.row.inode, c.row.stream).ok()??
                }
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

    /// Tools › Options.
    pub fn open_options(&mut self) {
        let p = &self.persisted;
        self.options = Some(crate::options_ui::OptionsWindow::new(
            crate::options_ui::Settings {
                local_time: p.local_time,
                hide_known_in_views: p.hide_known_in_views,
                custom_tags: p.custom_tags.clone(),
            },
        ));
    }

    fn show_options(&mut self, ctx: &egui::Context) {
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
            self.view_counts.invalidate();
            self.view_rows.clear();
        }
        if s.custom_tags != self.persisted.custom_tags {
            let case = self.analysis.lock().case().cloned();
            if let Some(case) = case {
                self.tagging
                    .apply_definitions(ctx, &case, s.custom_tags.clone());
            }
            self.persisted.custom_tags = s.custom_tags;
        }
    }

    /// Draws one frame (shared by eframe and UI tests).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_loading();
        self.update_hash_resolver();
        if !self.sources.is_empty() {
            // Index eagerly so analysis tools open instantly.
            self.shared_index(&ctx);
        }
        self.shortcuts(&ctx);
        let case = self.analysis.lock().case().cloned();
        let errors = self.tagging.ui(&ctx, case.as_ref());
        self.errors.extend(errors);
        for d in self.tagging.take_created() {
            crate::tags_ui::upsert_definition(&mut self.persisted.custom_tags, d);
        }
        if self.tagging.take_changed() {
            // Scores and tag columns changed.
            self.listings.clear();
            self.view_rows.clear();
            self.result_rows.clear();
            self.case_summary.refresh();
            self.node_views.clear();
            for s in &mut self.searches {
                s.rows.refresh();
            }
            for s in &mut self.keyword_searches {
                s.rows.refresh();
            }
        }
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
        self.show_options(&ctx);
        if self.plugins_open {
            self.plugins_open = crate::plugins::window(&ctx);
        }
        self.report_wizard(&ctx);
        self.geolocation(&ctx);
        self.communications(&ctx);
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
