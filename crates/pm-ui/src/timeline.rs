//! Timeline window (Autopsy's Timeline tool), shown as its own native window.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Color32, Sense, Vec2};
use egui_extras::{Column, TableBuilder};
use jiff::tz::TimeZone;
use pm_timeline::{
    Bin, Category, CountsQuery, DeletedFilter, EventType, FileCategory, Filter, ListRow, TimeUnit,
    Timeline, TypeZoom,
};

use crate::format::DisplayPrefs;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow};
use crate::sortable::{Order, Sort, cmp_text};
use crate::task::Task;
use crate::viewers::ViewerPanel;

/// Zoom presets (Autopsy's "Zoom in/out to" menu), in seconds.
const PRESETS: [(&str, i64); 16] = [
    ("1 minute", 60),
    ("15 minutes", 900),
    ("1 hour", 3_600),
    ("6 hours", 21_600),
    ("12 hours", 43_200),
    ("1 day", 86_400),
    ("3 days", 259_200),
    ("1 week", 604_800),
    ("2 weeks", 1_209_600),
    ("1 month", 2_629_746),
    ("3 months", 7_889_238),
    ("6 months", 15_778_476),
    ("1 year", 31_556_952),
    ("3 years", 94_670_856),
    ("5 years", 157_784_760),
    ("10 years", 315_569_520),
];

const MAX_BINS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Counts,
    Details,
    List,
}

/// History entry: (range, filter).
type State = ((i64, i64), Filter);

/// Cached view data, keyed by everything that affects it.
#[derive(Debug, Default)]
struct Cache {
    bins_key: Option<(i64, i64, TimeUnit, TypeZoom, String)>,
    bins: Vec<Bin>,
    rows_key: Option<(i64, i64, String)>,
    rows: Vec<ListRow>,
}

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

fn filter_key(f: &Filter) -> String {
    let mut t: Vec<_> = f.event_types.iter().map(|t| t.index()).collect();
    t.sort_unstable();
    let mut d: Vec<_> = f.data_sources.iter().copied().collect();
    d.sort_unstable();
    let mut c: Vec<_> = f.file_categories.iter().map(|c| *c as u8).collect();
    c.sort_unstable();
    format!("{}|{t:?}|{d:?}|{c:?}|{:?}", f.text, f.deleted)
}

/// The timeline window's state.
pub(crate) struct TimelineWindow {
    sources: Vec<DataSource>,
    index: SharedIndex,
    building: Option<Task<Arc<Timeline>>>,
    timeline: Option<Arc<Timeline>>,
    range: (i64, i64),
    filter: Filter,
    back: Vec<State>,
    forward: Vec<State>,
    mode: Mode,
    zoom: TypeZoom,
    log_scale: bool,
    unit_override: Option<TimeUnit>,
    /// Selected bin (start, end) in the counts view.
    selection: Option<(i64, i64)>,
    selected_row: Option<usize>,
    /// Event list sort (`None`: by time) and its cached row order.
    event_sort: Option<Sort>,
    event_order: Order,
    cache: Cache,
    viewer: ViewerPanel,
    prefs: DisplayPrefs,
    /// "View in Timeline" requested before events were ready.
    pending_focus: Option<((i64, i64), Filter)>,
    /// Details view: description detail, clusters cache, selected cluster.
    desc_level: crate::timeline_details::DescLevel,
    details: Option<(String, Vec<crate::timeline_details::Cluster>)>,
    detail_sel: Option<usize>,
    /// Whole-span histogram for the range slider.
    overview: Option<((i64, i64), Vec<u64>)>,
    /// Case whose artifacts (web history, …) become events too.
    case: Option<Arc<pm_case::Case>>,
}

impl std::fmt::Debug for TimelineWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineWindow")
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl TimelineWindow {
    /// Creates the window; events are built once the shared index is ready.
    pub(crate) fn new(
        index: SharedIndex,
        sources: Vec<DataSource>,
        prefs: DisplayPrefs,
        case: Option<Arc<pm_case::Case>>,
    ) -> Self {
        Self {
            index,
            sources,
            building: None,
            timeline: None,
            range: (0, 1),
            filter: Filter::default(),
            back: Vec::new(),
            forward: Vec::new(),
            mode: Mode::Counts,
            zoom: TypeZoom::Category,
            log_scale: true,
            unit_override: None,
            selection: None,
            selected_row: None,
            event_sort: None,
            event_order: Order::default(),
            cache: Cache::default(),
            viewer: ViewerPanel::default(),
            prefs,
            pending_focus: None,
            overview: None,
            case,
            desc_level: crate::timeline_details::DescLevel::default(),
            details: None,
            detail_sel: None,
        }
    }

    /// Rebuilds the events (e.g. after analysis added artifacts).
    pub(crate) fn invalidate(&mut self) {
        self.timeline = None;
        self.building = None;
        self.overview = None;
        self.details = None;
        self.cache = Cache::default();
    }

    /// Focuses the window on `[t - pad, t + pad]` in list mode ("View in Timeline").
    pub(crate) fn focus(&mut self, t: i64, pad: i64) {
        self.mode = Mode::List;
        self.show((t - pad, t + pad + 1), self.filter.clone());
    }

    /// Shows `range` restricted to one data source (Data Source Summary's
    /// "View in Timeline").
    pub(crate) fn focus_data_source(&mut self, ds: u32, range: (i64, i64)) {
        self.mode = Mode::Counts;
        let filter = Filter {
            data_sources: [ds].into(),
            ..Filter::default()
        };
        self.show(range, filter);
    }

    fn show(&mut self, range: (i64, i64), filter: Filter) {
        if self.timeline.is_none() {
            self.pending_focus = Some((range, filter));
        } else {
            self.push(range, filter);
        }
    }

    fn span(&self) -> (i64, i64) {
        self.timeline
            .as_ref()
            .and_then(|t| t.span())
            .unwrap_or((0, 1))
    }

    fn tz(&self) -> &TimeZone {
        &self.prefs.timezone
    }

    /// Autopsy's `pushTimeRange` + history `advance`, with clamping to the span.
    fn push(&mut self, range: (i64, i64), filter: Filter) {
        let (s0, s1) = self.span();
        let mut r = (range.0.max(s0), range.1.min(s1));
        if r.0 >= r.1 {
            r = (s0, s1);
        }
        if r == self.range && filter == self.filter {
            return;
        }
        self.back.push((self.range, self.filter.clone()));
        self.forward.clear();
        self.range = r;
        self.filter = filter;
        self.selection = None;
        self.selected_row = None;
    }

    fn set_filter(&mut self, f: Filter) {
        self.push(self.range, f);
    }

    fn zoom_by(&mut self, factor: f64, anchor: Option<i64>) {
        let (s, e) = self.range;
        let len = (e - s).max(1) as f64;
        let new_len = (len * factor).max(1.0);
        let a = anchor.unwrap_or(s + (e - s) / 2) as f64;
        let frac = (a - s as f64) / len;
        let ns = a - new_len * frac;
        self.push(
            (ns as i64, (ns + new_len).ceil() as i64),
            self.filter.clone(),
        );
    }

    fn unit(&self) -> TimeUnit {
        self.unit_override
            .unwrap_or_else(|| TimeUnit::for_range(self.range.0, self.range.1))
    }

    fn bins(&mut self) -> &[Bin] {
        let unit = self.unit();
        let key = (
            self.range.0,
            self.range.1,
            unit,
            self.zoom,
            filter_key(&self.filter),
        );
        if self.cache.bins_key.as_ref() != Some(&key) {
            if let Some(t) = &self.timeline {
                self.cache.bins = t.counts(&CountsQuery {
                    start: self.range.0,
                    end: self.range.1,
                    unit,
                    tz: &self.prefs.timezone,
                    filter: &self.filter,
                    zoom: self.zoom,
                    max_bins: MAX_BINS,
                });
            }
            self.cache.bins_key = Some(key);
        }
        &self.cache.bins
    }

    fn rows(&mut self, range: (i64, i64)) -> &[ListRow] {
        let key = (range.0, range.1, filter_key(&self.filter));
        if self.cache.rows_key.as_ref() != Some(&key) {
            if let Some(t) = &self.timeline {
                self.cache.rows = t.list(range.0, range.1, &self.filter);
            }
            self.cache.rows_key = Some(key);
            self.selected_row = None;
            self.event_order.clear();
        }
        &self.cache.rows
    }

    fn content_for(&self, row: &ListRow) -> Option<Content> {
        let t = self.timeline.as_ref()?;
        let s = t.subject(row.subject)?;
        let ds = self.sources.iter().find(|d| d.id as u32 == s.ds)?;
        let fs = ds.fs(s.fs as usize)?;
        Some(Content {
            fs: Arc::clone(fs),
            row: FileRow::from_fs(&**fs, s.inode, &s.path, !s.deleted),
        })
    }

    // ------------------------------------------------------------------ UI

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        if self.timeline.is_none() {
            let Some(idx) = self.index.ready_or_progress(ui) else {
                return;
            };
            let (case, sources) = (self.case.clone(), self.sources.clone());
            let task = self.building.get_or_insert_with(|| {
                Task::spawn(ui.ctx(), move || {
                    let t = Timeline::from_index(&idx);
                    let t = match &case {
                        Some(c) => {
                            let (events, subjects) = crate::timeline_artifacts::load(c, &sources);
                            t.with(events, subjects)
                        }
                        None => t,
                    };
                    Arc::new(t)
                })
            });
            let Some(t) = task.poll() else {
                ui.centered_and_justified(egui::Ui::spinner);
                return;
            };
            let t = Arc::clone(t);
            self.range = t.span().unwrap_or((0, 1));
            self.timeline = Some(t);
            self.building = None;
            if let Some((r, f)) = self.pending_focus.take() {
                self.push(r, f);
            }
        }
        let Some(timeline) = self.timeline.clone() else {
            return;
        };
        if timeline.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.heading("No timestamped events in these data sources.")
            });
            return;
        }
        self.shortcuts(ui);
        egui::Panel::top("tl-toolbar").show(ui, |ui| self.toolbar(ui, &timeline));
        egui::Panel::left("tl-filters")
            .resizable(true)
            .default_size(250.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                self.filters(ui, &timeline);
            });
        egui::Panel::bottom("tl-viewer")
            .resizable(true)
            .default_size(260.0)
            .min_size(100.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let prefs = self.prefs.clone();
                self.viewer.ui(ui, &prefs);
            });
        egui::CentralPanel::default().show(ui, |ui| match self.mode {
            Mode::Counts => {
                let h = (ui.available_height() * 0.55).max(160.0);
                self.chart(ui, h);
                ui.separator();
                let range = self.selection.unwrap_or(self.range);
                self.event_table(ui, &timeline, range);
            }
            Mode::Details => {
                let h = (ui.available_height() * 0.6).max(160.0);
                self.details_view(ui, &timeline, h);
                ui.separator();
                let range = self.selection.unwrap_or(self.range);
                self.event_table(ui, &timeline, range);
            }
            Mode::List => {
                let range = self.range;
                self.event_table(ui, &timeline, range);
            }
        });
    }

    fn shortcuts(&mut self, ui: &egui::Ui) {
        let (back, fwd) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight),
            )
        });
        if back {
            self.go_back();
        }
        if fwd {
            self.go_forward();
        }
    }

    fn go_back(&mut self) {
        if let Some((r, f)) = self.back.pop() {
            self.forward
                .push((self.range, std::mem::replace(&mut self.filter, f)));
            self.range = r;
            self.selection = None;
        }
    }

    fn go_forward(&mut self) {
        if let Some((r, f)) = self.forward.pop() {
            self.back
                .push((self.range, std::mem::replace(&mut self.filter, f)));
            self.range = r;
            self.selection = None;
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, timeline: &Timeline) {
        ui.horizontal_wrapped(|ui| {
            ui.label("View:");
            ui.selectable_value(&mut self.mode, Mode::Counts, "Counts");
            ui.selectable_value(&mut self.mode, Mode::Details, "Details");
            ui.selectable_value(&mut self.mode, Mode::List, "List");
            if self.mode == Mode::Details {
                ui.separator();
                ui.label("Description detail:");
                for l in crate::timeline_details::DescLevel::ALL {
                    ui.selectable_value(&mut self.desc_level, l, l.name());
                }
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
                .button("🔍+")
                .on_hover_text("Zoom in (about half as much time)")
                .clicked()
            {
                self.zoom_by(0.5, None);
            }
            let whole = self.range == self.span();
            if ui
                .add_enabled(!whole, egui::Button::new("🔍−"))
                .on_hover_text("Zoom out (about 50 % more time)")
                .clicked()
            {
                self.zoom_by(1.5, None);
            }
            egui::ComboBox::from_id_salt("tl-presets")
                .selected_text("Zoom to…")
                .show_ui(ui, |ui| {
                    for (name, secs) in PRESETS {
                        if ui.selectable_label(false, name).clicked() {
                            let mid = self.range.0 + (self.range.1 - self.range.0) / 2;
                            self.push((mid - secs / 2, mid + secs / 2), self.filter.clone());
                        }
                    }
                    if ui.selectable_label(false, "All").clicked() {
                        let span = self.span();
                        self.push(span, self.filter.clone());
                    }
                });
            if ui
                .add_enabled(!whole, egui::Button::new("Show all"))
                .clicked()
            {
                let span = self.span();
                self.push(span, self.filter.clone());
            }
            ui.separator();
            if self.mode == Mode::Counts {
                egui::ComboBox::from_id_salt("tl-unit")
                    .selected_text(format!(
                        "Bins: {}",
                        self.unit_override.map_or("Auto", TimeUnit::name)
                    ))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.unit_override, None, "Auto");
                        for u in TimeUnit::ALL {
                            ui.selectable_value(&mut self.unit_override, Some(u), u.name());
                        }
                    });
                ui.selectable_value(&mut self.zoom, TypeZoom::Category, "Categories");
                ui.selectable_value(&mut self.zoom, TypeZoom::Event, "Event types");
                ui.checkbox(&mut self.log_scale, "Log scale");
                ui.separator();
            }
            let tz = self.tz().clone();
            ui.label(format!(
                "{} — {}  ·  {} events",
                TimeUnit::Seconds.label(self.range.0, &tz),
                TimeUnit::Seconds.label(self.range.1 - 1, &tz),
                crate::format::grouped(
                    timeline.count(self.range.0, self.range.1, &self.filter) as u64
                )
            ));
        });
    }

    fn filters(&mut self, ui: &mut egui::Ui, timeline: &Timeline) {
        let mut f = self.filter.clone();
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.heading("Filters");
                ui.horizontal(|ui| {
                    ui.label("Text:");
                    ui.add(
                        egui::TextEdit::singleline(&mut f.text).hint_text("enter filter string"),
                    );
                });
                ui.separator();
                ui.strong("Event Types");
                for cat in Category::ALL {
                    let types: Vec<EventType> = EventType::of(cat).collect();
                    let all_on = !f.event_types.is_empty()
                        && types.iter().all(|t| f.event_types.contains(t));
                    egui::CollapsingHeader::new(cat.name())
                        .id_salt(("tl-cat", cat.name()))
                        .default_open(cat == Category::FileSystem)
                        .show(ui, |ui| {
                            let mut toggle_all = all_on;
                            ui.horizontal(|ui| {
                                swatch(ui, color(cat.color()));
                                if ui
                                    .checkbox(&mut toggle_all, "All of this category")
                                    .changed()
                                {
                                    for t in &types {
                                        if toggle_all {
                                            f.event_types.insert(*t);
                                        } else {
                                            f.event_types.remove(t);
                                        }
                                    }
                                }
                            });
                            for t in &types {
                                let mut on = f.event_types.contains(t);
                                ui.horizontal(|ui| {
                                    swatch(ui, color(t.color()));
                                    if ui.checkbox(&mut on, t.name()).changed() {
                                        if on {
                                            f.event_types.insert(*t);
                                        } else {
                                            f.event_types.remove(t);
                                        }
                                    }
                                });
                            }
                        });
                }
                ui.weak("No type selected = all types.");
                ui.separator();
                ui.strong("Data Sources");
                for (i, name) in timeline.data_sources().iter().enumerate() {
                    toggle(ui, &mut f.data_sources, i as u32, name);
                }
                ui.separator();
                ui.strong("File Types");
                for c in FileCategory::ALL {
                    toggle(ui, &mut f.file_categories, c, c.name());
                }
                ui.separator();
                ui.strong("Deleted Files");
                ui.radio_value(&mut f.deleted, DeletedFilter::All, "Show all");
                ui.radio_value(&mut f.deleted, DeletedFilter::OnlyDeleted, "Only deleted");
                ui.radio_value(
                    &mut f.deleted,
                    DeletedFilter::OnlyAllocated,
                    "Only allocated",
                );
                ui.separator();
                if ui
                    .add_enabled(!f.is_default(), egui::Button::new("Reset all filters"))
                    .clicked()
                {
                    f = Filter::default();
                }
            });
        if f != self.filter {
            self.set_filter(f);
        }
    }

    fn chart(&mut self, ui: &mut egui::Ui, height: f32) {
        let tz = self.tz().clone();
        let unit = self.unit();
        let series = Timeline::series(self.zoom);
        let bins = self.bins().to_vec();
        let events = crate::timeline_chart::stacked_bars(
            ui,
            height,
            &crate::timeline_chart::Chart {
                bins: &bins,
                series: &series,
                log: self.log_scale,
                unit,
                tz: &tz,
                selection: self.selection,
                range: self.range,
            },
        );
        for e in events {
            match e {
                crate::timeline_chart::ChartEvent::Select(s) => self.selection = s,
                crate::timeline_chart::ChartEvent::ZoomTo(r) => self.push(r, self.filter.clone()),
                crate::timeline_chart::ChartEvent::Wheel { factor, anchor } => {
                    self.zoom_by(factor, Some(anchor));
                }
            }
        }
        // Autopsy's range slider over the whole span.
        let span = self.span();
        if self.overview.as_ref().is_none_or(|o| o.0 != span)
            && let Some(t) = &self.timeline
        {
            let all = Filter::default();
            let bins = t.counts(&CountsQuery {
                start: span.0,
                end: span.1,
                unit: TimeUnit::for_range(span.0, span.1).finer(),
                tz: &tz,
                filter: &all,
                zoom: TypeZoom::Category,
                max_bins: 240,
            });
            self.overview = Some((span, bins.iter().map(Bin::total).collect()));
        }
        if let Some((_, totals)) = &self.overview
            && let Some(r) = crate::timeline_chart::overview(ui, totals, span, self.range, &tz)
        {
            self.range = r;
            self.selection = None;
        }
    }

    fn details_view(&mut self, ui: &mut egui::Ui, timeline: &Timeline, height: f32) {
        use crate::timeline_details::{DetailsEvent, clusters, view};
        let range = self.range;
        let key = format!("{range:?}{}{:?}", filter_key(&self.filter), self.desc_level);
        if self.details.as_ref().is_none_or(|d| d.0 != key) {
            let rows = timeline.list(range.0, range.1, &self.filter);
            let gap = ((range.1 - range.0) / 80).max(1);
            self.details = Some((key, clusters(&rows, timeline, self.desc_level, gap)));
            self.detail_sel = None;
        }
        let tz = self.tz().clone();
        let cl = self
            .details
            .as_ref()
            .map(|d| d.1.clone())
            .unwrap_or_default();
        for e in view(ui, height, &cl, range, &tz, self.detail_sel) {
            match e {
                DetailsEvent::Select(sel) => {
                    self.detail_sel = sel;
                    match sel.and_then(|i| cl.get(i)) {
                        Some(c) => {
                            self.selection = Some((c.start, c.end + 1));
                            let row = ListRow {
                                time: c.start,
                                subject: c.subject,
                                kinds: vec![c.kind],
                            };
                            let content = self.content_for(&row);
                            self.viewer.set_content(content);
                        }
                        None => self.selection = None,
                    }
                }
                DetailsEvent::ZoomTo(r) => self.push(r, self.filter.clone()),
                DetailsEvent::Wheel { zoom_in, anchor } => {
                    self.zoom_by(if zoom_in { 0.8 } else { 1.25 }, Some(anchor));
                }
            }
        }
    }

    fn event_table(&mut self, ui: &mut egui::Ui, timeline: &Timeline, range: (i64, i64)) {
        let tz = self.tz().clone();
        let rows = self.rows(range).to_vec();
        ui.horizontal(|ui| {
            ui.strong(format!(
                "{} rows",
                crate::format::grouped(rows.len() as u64)
            ));
            if let Some((s, e)) = self.selection {
                ui.weak(format!(
                    "selected bin: {} — {}",
                    TimeUnit::Seconds.label(s, &tz),
                    TimeUnit::Seconds.label(e - 1, &tz)
                ));
            }
        });
        let kinds = |r: &ListRow| {
            r.kinds
                .iter()
                .map(|k| k.name())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let subject = |r: &ListRow| timeline.subject(r.subject);
        let order = self
            .event_order
            .get(0, rows.len(), self.event_sort, |a, b, col| {
                let (Some(a), Some(b)) = (rows.get(a), rows.get(b)) else {
                    return std::cmp::Ordering::Equal;
                };
                let (sa, sb) = (subject(a), subject(b));
                match col {
                    0 => a.time.cmp(&b.time),
                    1 => cmp_text(&kinds(a), &kinds(b)),
                    2 => cmp_text(sa.map_or("", |s| &*s.path), sb.map_or("", |s| &*s.path)),
                    3 => {
                        let ds = |s: Option<&pm_timeline::Subject>| {
                            s.and_then(|s| timeline.data_sources().get(s.ds as usize))
                                .map_or("", String::as_str)
                        };
                        cmp_text(ds(sa), ds(sb))
                    }
                    _ => sa
                        .is_some_and(|s| s.deleted)
                        .cmp(&sb.is_some_and(|s| s.deleted)),
                }
            });
        let mut clicked = None;
        let mut sort = self.event_sort;
        crate::sortable::hscroll(ui, "tl-events-h", |ui| {
            ui.style_mut().interaction.selectable_labels = false;
            let row_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
            TableBuilder::new(ui)
                .id_salt("tl-events")
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(170.0))
                .column(Column::initial(130.0))
                .column(Column::initial(520.0).clip(true))
                .column(Column::initial(140.0).clip(true))
                .column(Column::remainder().at_least(60.0))
                .header(row_h + 2.0, |mut header| {
                    for (i, t) in [
                        "Date/Time",
                        "Event Type",
                        "Description",
                        "Data Source",
                        "Deleted",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        header.col(|ui| {
                            crate::sortable::header_mut(ui, t, &mut sort, i);
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, order.len(), |mut row| {
                        let Some(i) = order.get(row.index()).copied() else {
                            return;
                        };
                        let Some(r) = rows.get(i) else { return };
                        let Some(s) = timeline.subject(r.subject) else {
                            return;
                        };
                        row.set_selected(self.selected_row == Some(i));
                        row.col(|ui| {
                            ui.label(TimeUnit::Seconds.label(r.time, &tz));
                        });
                        row.col(|ui| {
                            match r.macb() {
                                Some(m) => ui.monospace(m).on_hover_text(kinds(r)),
                                None => ui.label(kinds(r)),
                            };
                        });
                        row.col(|ui| {
                            let text = egui::RichText::new(&*s.path);
                            ui.label(if s.deleted {
                                text.color(ui.visuals().error_fg_color)
                            } else {
                                text
                            });
                        });
                        row.col(|ui| {
                            ui.label(
                                timeline
                                    .data_sources()
                                    .get(s.ds as usize)
                                    .map_or("", String::as_str),
                            );
                        });
                        row.col(|ui| {
                            ui.label(if s.deleted { "yes" } else { "" });
                        });
                        if row.response().clicked() {
                            clicked = Some(i);
                        }
                    });
                });
        });
        self.event_sort = sort;
        if let Some(i) = clicked {
            self.selected_row = Some(i);
            let content = rows.get(i).and_then(|r| self.content_for(r));
            self.viewer.set_content(content);
        }
    }
}

fn swatch(ui: &mut egui::Ui, c: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, c);
}

fn toggle<T: std::hash::Hash + Eq + Copy>(
    ui: &mut egui::Ui,
    set: &mut HashSet<T>,
    v: T,
    label: &str,
) {
    let mut on = set.contains(&v);
    if ui.checkbox(&mut on, label).changed() {
        if on {
            set.insert(v);
        } else {
            set.remove(&v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    fn demo_sources() -> (tempfile::TempDir, Vec<DataSource>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.dd");
        std::fs::write(&path, pm_testkit::demo::demo_disk()).unwrap();
        let evidence = pm_evidence::open(&path, &pm_evidence::OpenOptions::default()).unwrap();
        (
            dir,
            vec![DataSource {
                id: 0,
                name: "demo.dd".into(),
                evidence,
            }],
        )
    }

    fn shot(h: &mut Harness<'_, TimelineWindow>, name: &str) {
        if let Ok(dir) = std::env::var("PM_UI_SCREENSHOT_DIR") {
            h.render()
                .unwrap()
                .save(std::path::PathBuf::from(dir).join(format!("{name}.png")))
                .unwrap();
        }
    }

    #[test]
    fn timeline_builds_and_lists_events() {
        let (_dir, sources) = demo_sources();
        let ctx = egui::Context::default();
        let index = SharedIndex::build(&ctx, &sources);
        let win = TimelineWindow::new(index, sources, DisplayPrefs::default(), None);
        let mut h = Harness::builder()
            .with_size(egui::vec2(1300.0, 850.0))
            .build_ui_state(|ui, w: &mut TimelineWindow| w.ui(ui), win);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while h.state().timeline.is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "timeline did not build"
            );
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.run_steps(4);
        let t = h.state().timeline.clone().unwrap();
        assert!(
            t.len() > 40,
            "demo disk yields MACB events, got {}",
            t.len()
        );
        h.get_by_label_contains("events");
        shot(&mut h, "timeline_counts");
        // Zoom to the NTFS builder's fixed timestamp and switch to the list.
        h.state_mut().focus(1_622_505_000, 3600);
        h.run_steps(4);
        assert!(h.query_all_by_label_contains("setup.exe").count() >= 1);
        assert!(h.query_all_by_label_contains("_").count() > 1);
        shot(&mut h, "timeline_list");
        // Details view: clustered stripes above the event table.
        h.state_mut().mode = Mode::Details;
        h.run_steps(4);
        assert!(h.query_all_by_label_contains("Description detail").count() >= 1);
        shot(&mut h, "timeline_details");
    }
}
