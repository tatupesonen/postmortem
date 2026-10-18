//! The postmortem main window.

mod case_ops;
mod chrome;
mod command;
mod panes;
mod tree;
mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use parking_lot::Mutex;

use crate::analysis::Analysis;
use crate::case_ui::NewCaseWizard;
use crate::discovery::DiscoveryWindow;
use crate::format::DisplayPrefs;
use crate::gallery::GalleryWindow;
use crate::model::{Content, FileRow, NodeId};
use crate::summary::SummaryState;
use crate::table::TableState;
use crate::task::Task;
use crate::timeline::TimelineWindow;
use crate::viewers::ViewerPanel;

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
    /// Help › Keyboard Shortcuts.
    shortcuts_open: bool,
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
    summary: SummaryState,
    /// The case, and adding/analysing data sources in the background.
    analysis: Arc<Mutex<Analysis>>,
    progress_open: bool,
    progress_closed: Arc<AtomicBool>,
    case_wizard: Option<NewCaseWizard>,
    result_selected: Option<usize>,
    result_table: crate::results::ResultTable,
    /// Analysis was running last frame (refresh results when it stops).
    /// Case change bus → view refreshes.
    change_watch: crate::changes::ChangeWatch,
    add_wizard: Option<crate::add_source::AddSourceWizard>,
    tz_check: crate::tz_check::TimeZoneCheck,
    /// Files selected in the table shown last frame (Ctrl+B tags them).
    marked_files: Vec<i64>,
    /// Ctrl+K.
    palette: crate::palette::Palette,
    /// Pivot windows' state (one window, value changeable).
    pivot: Option<crate::pivot::PivotWindow>,
    /// Tools › Notes.
    notes: Option<crate::notes_ui::NotesWindow>,
    /// Tools › IOC Search.
    ioc: Option<crate::ioc::IocWindow>,
    /// The Case Overview dashboard's data.
    overview: crate::task::Cached<crate::overview::Overview>,
    /// Results selected in the result table shown last frame.
    marked_results: Vec<i64>,
    /// Data of the tree and result panes, each refreshed from the change
    /// bus by its own component.
    results: crate::results::ResultData,
    views: crate::views::ViewData,
    nodes: crate::case_nodes::NodeData,
    searches: panes::Searches,
    /// Case folder and data-source count the hash resolver was built for.
    resolver_key: Option<PathBuf>,
    /// The Data Sources tree structure from the case database.
    case_sources: crate::case_tree::Sources,
    tagging: crate::tags_ui::Tagging,
    file_search: Option<crate::file_search_ui::FileSearchWindow>,
    /// The toolbar's Keyword Search panel, when open.
    keyword_panel: Option<crate::keyword_search_ui::KeywordSearchPanel>,
    /// "Files Indexed" for the panel.
    keyword_indexed: crate::task::Cached<u64>,
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
    /// Case › Delete Case awaiting confirmation: (folder, display name).
    confirm_delete_case: Option<(PathBuf, String)>,
    removing: Option<Task<Result<(), String>>>,
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
            listings: HashMap::new(),
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            viewer: ViewerPanel::default(),
            popouts: Vec::new(),
            next_popout: 0,
            errors: Vec::new(),
            about_open: false,
            shortcuts_open: false,
            exports: Vec::new(),
            wordmark: None,
            timeline: None,
            timeline_closed: Arc::new(AtomicBool::new(false)),
            gallery: None,
            gallery_closed: Arc::new(AtomicBool::new(false)),
            discovery: None,
            discovery_closed: Arc::new(AtomicBool::new(false)),
            summary: SummaryState::default(),
            analysis: Arc::new(Mutex::new(Analysis::default())),
            progress_open: false,
            progress_closed: Arc::new(AtomicBool::new(false)),
            case_wizard: None,
            result_selected: None,
            result_table: crate::results::ResultTable::default(),
            change_watch: crate::changes::ChangeWatch::default(),
            add_wizard: None,
            tz_check: crate::tz_check::TimeZoneCheck::default(),
            marked_files: Vec::new(),
            palette: crate::palette::Palette::default(),
            pivot: None,
            notes: None,
            ioc: None,
            overview: crate::task::Cached::default(),
            marked_results: Vec::new(),
            results: crate::results::ResultData::default(),
            views: crate::views::ViewData::default(),
            nodes: crate::case_nodes::NodeData::default(),
            searches: panes::Searches::default(),
            resolver_key: None,
            case_sources: crate::case_tree::Sources::default(),
            tagging: crate::tags_ui::Tagging::default(),
            file_search: None,
            keyword_panel: None,
            keyword_indexed: crate::task::Cached::default(),
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
            confirm_delete_case: None,
            removing: None,
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

    /// Background UI tasks still running (test diagnostics).
    pub fn running_tasks(&self) -> usize {
        crate::task::running()
    }

    /// Number of data sources in the open case (as the tree last read
    /// them).
    pub fn data_source_count(&self) -> usize {
        self.case_sources.count()
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

    /// Whether a data source is still being added (before analysis).
    pub fn is_loading(&self) -> bool {
        self.analysis.lock().any_adding()
    }

    /// Whether the open case has data sources (enables the analysis tools).
    fn has_data_sources(&self) -> bool {
        self.case_sources.count() > 0
    }

    fn apply_time_pref(&mut self) {
        self.prefs.timezone = if self.persisted.local_time {
            jiff::tz::TimeZone::system()
        } else {
            jiff::tz::TimeZone::UTC
        };
    }

    /// Draws one frame (shared by eframe and UI tests).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_loading();
        self.update_hash_resolver();
        self.shortcuts(&ctx);
        let case = self.analysis.lock().case().cloned();
        let errors = self.tagging.ui(&ctx, case.as_ref());
        self.errors.extend(errors);
        for d in self.tagging.take_created() {
            crate::tags_ui::upsert_definition(&mut self.persisted.custom_tags, d);
        }
        if let Some(case) = &case {
            self.apply_case_changes(&ctx, case);
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
        if ui.ctx().cumulative_frame_nr() == 0 {
            tracing::info!(
                "first frame {} ms after start-up",
                crate::STARTED.elapsed().as_millis()
            );
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.persisted);
    }
}

/// Copies a file's content to `dest`.
fn copy_out(src: &dyn pm_core::io::ReadAt, dest: &std::path::Path) -> Result<(), String> {
    let mut out = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
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
    Ok(())
}

/// Add File to Hash Set (Autopsy `AddContentToHashDbAction`): the file's
/// MD5 (computed now if ingest has not) and SHA-256, with its name.
fn add_to_hash_set(content: &Content, set: &std::path::Path) -> Result<String, String> {
    use md5::Digest as _;
    let rec = content.row.file.as_deref();
    let known = rec
        .and_then(|f| f.md5.as_deref())
        .and_then(|m| <[u8; 16]>::try_from(m).ok());
    let md5 = if let Some(m) = known {
        m
    } else {
        let src = content.source()?;
        let mut h = md5::Md5::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut off = 0;
        while off < src.size() {
            let n = src.read_at(off, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            h.update(buf.get(..n).unwrap_or_default());
            off += n as u64;
        }
        h.finalize().into()
    };
    let db = pm_hashdb::HashDb::open(set, false).map_err(|e| e.to_string())?;
    db.add(
        &md5,
        None,
        rec.and_then(|f| f.sha256.as_deref()),
        Some(&content.row.name),
        None,
    )
    .map_err(|e| format!("{}: {e}", set.display()))?;
    Ok(set.display().to_string())
}
