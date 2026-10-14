//! Timeline window (Autopsy's Timeline tool), shown as its own native window.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use egui_extras::{Column, TableBuilder};
use jiff::tz::TimeZone;
use pm_timeline::collect::{Progress, Source};
use pm_timeline::{
    Bin, Category, CountsQuery, DeletedFilter, EventType, FileCategory, Filter, ListRow, TimeUnit,
    Timeline, TypeZoom,
};

use crate::format::DisplayPrefs;
use crate::model::{Content, DataSource, FileRow};
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
    building: Option<(Task<Arc<Timeline>>, Arc<Progress>)>,
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
    drag_from: Option<f32>,
    selected_row: Option<usize>,
    cache: Cache,
    viewer: ViewerPanel,
    prefs: DisplayPrefs,
    /// "View in Timeline" requested before events were ready.
    pending_focus: Option<(i64, i64)>,
}

impl std::fmt::Debug for TimelineWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineWindow")
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

impl TimelineWindow {
    /// Starts collecting events from every filesystem of `sources`.
    pub(crate) fn new(ctx: &egui::Context, sources: Vec<DataSource>, prefs: DisplayPrefs) -> Self {
        let progress = Arc::new(Progress::default());
        let p = Arc::clone(&progress);
        let fs_sources: Vec<Source> = sources
            .iter()
            .flat_map(|ds| {
                ds.evidence
                    .filesystems
                    .iter()
                    .enumerate()
                    .map(move |(i, f)| Source {
                        ds: ds.id as u32,
                        fs_index: i as u32,
                        fs: Arc::clone(&f.fs),
                    })
            })
            .collect();
        let names = sources.iter().map(|d| d.name.clone()).collect();
        let task = Task::spawn(ctx, move || {
            Arc::new(pm_timeline::collect::collect(&fs_sources, names, &p))
        });
        Self {
            sources,
            building: Some((task, progress)),
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
            drag_from: None,
            selected_row: None,
            cache: Cache::default(),
            viewer: ViewerPanel::default(),
            prefs,
            pending_focus: None,
        }
    }

    /// Focuses the window on `[t - pad, t + pad]` in list mode ("View in Timeline").
    pub(crate) fn focus(&mut self, t: i64, pad: i64) {
        self.mode = Mode::List;
        if self.timeline.is_none() {
            self.pending_focus = Some((t - pad, t + pad + 1));
        } else {
            self.push((t - pad, t + pad + 1), self.filter.clone());
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
        if let Some((task, progress)) = &mut self.building {
            if let Some(t) = task.poll() {
                let t = Arc::clone(t);
                self.range = t.span().unwrap_or((0, 1));
                self.timeline = Some(t);
                self.building = None;
                if let Some(r) = self.pending_focus.take() {
                    self.push(r, self.filter.clone());
                }
            } else {
                let files = progress.files.load(Ordering::Relaxed);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 3.0);
                    ui.spinner();
                    ui.heading("Building timeline…");
                    ui.label(format!("{} files examined", crate::format::grouped(files)));
                });
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
                return;
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
            ui.selectable_value(&mut self.mode, Mode::List, "List");
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
        let log = self.log_scale;
        let series = Timeline::series(self.zoom);
        let bins = self.bins().to_vec();
        let (rect, resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            Sense::click_and_drag(),
        );
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        let plot = Rect::from_min_max(
            rect.min + Vec2::new(48.0, 8.0),
            rect.max - Vec2::new(8.0, 36.0),
        );
        if bins.is_empty() || plot.width() < 10.0 {
            return;
        }
        let seg = |c: u32| {
            if c == 0 {
                0.0
            } else if log {
                f64::from(c).log10() + 1.0
            } else {
                f64::from(c)
            }
        };
        let max = bins
            .iter()
            .map(|b| b.counts.iter().map(|&c| seg(c)).sum::<f64>())
            .fold(1.0_f64, f64::max);
        let n = bins.len() as f32;
        let slot = plot.width() / n;
        let gap = (slot * 0.15).min(2.0);
        // Y axis.
        let text_color = visuals.text_color();
        let weak = visuals.weak_text_color();
        painter.line_segment(
            [plot.left_bottom(), plot.left_top()],
            Stroke::new(1.0, weak),
        );
        painter.line_segment(
            [plot.left_bottom(), plot.right_bottom()],
            Stroke::new(1.0, weak),
        );
        let max_total = bins.iter().map(Bin::total).max().unwrap_or(0);
        let top_label = if log {
            format!("{max_total}\n(log)")
        } else {
            max_total.to_string()
        };
        painter.text(
            plot.left_top() - Vec2::new(4.0, 0.0),
            Align2::RIGHT_TOP,
            top_label,
            FontId::proportional(11.0),
            weak,
        );
        painter.text(
            plot.left_bottom() - Vec2::new(4.0, 0.0),
            Align2::RIGHT_BOTTOM,
            "0",
            FontId::proportional(11.0),
            weak,
        );
        // Bars.
        for (i, b) in bins.iter().enumerate() {
            let x0 = plot.left() + i as f32 * slot + gap / 2.0;
            let x1 = x0 + slot - gap;
            let mut y = plot.bottom();
            for (s, &c) in b.counts.iter().enumerate() {
                if c == 0 {
                    continue;
                }
                let h = (seg(c) / max) as f32 * plot.height();
                let r = Rect::from_min_max(Pos2::new(x0, y - h), Pos2::new(x1, y));
                let col = series.get(s).map_or(Color32::GRAY, |(_, rgb)| color(*rgb));
                painter.rect_filled(r, 1.0, col);
                y -= h;
            }
            if self.selection == Some((b.start, b.end)) {
                painter.rect_stroke(
                    Rect::from_min_max(
                        Pos2::new(x0 - 1.0, plot.top()),
                        Pos2::new(x1 + 1.0, plot.bottom()),
                    ),
                    2.0,
                    Stroke::new(2.0, visuals.selection.stroke.color),
                    StrokeKind::Outside,
                );
            }
        }
        // X labels: as many as fit.
        let label_w = match unit {
            TimeUnit::Years => 40.0,
            TimeUnit::Months => 110.0,
            TimeUnit::Days => 140.0,
            _ => 150.0,
        };
        let every = ((label_w / slot).ceil() as usize).max(1);
        for (i, b) in bins.iter().enumerate().step_by(every) {
            let x = plot.left() + (i as f32 + 0.5) * slot;
            let col = if b.total() > 0 { text_color } else { weak };
            painter.text(
                Pos2::new(x, plot.bottom() + 4.0),
                Align2::CENTER_TOP,
                unit.label(b.start, &tz),
                FontId::proportional(11.0),
                col,
            );
        }
        painter.text(
            rect.right_bottom() - Vec2::new(6.0, 4.0),
            Align2::RIGHT_BOTTOM,
            format!("bins: {}", unit.name()),
            FontId::proportional(10.0),
            weak,
        );

        let bin_at = |x: f32| -> Option<usize> {
            let i = ((x - plot.left()) / slot).floor();
            (i >= 0.0 && (i as usize) < bins.len()).then_some(i as usize)
        };
        // Interval selection by dragging.
        if resp.drag_started() {
            self.drag_from = resp.interact_pointer_pos().map(|p| p.x);
        }
        if let (Some(x0), Some(p)) = (self.drag_from, resp.interact_pointer_pos()) {
            if resp.dragged() {
                let band = Rect::from_min_max(
                    Pos2::new(x0.min(p.x), plot.top()),
                    Pos2::new(x0.max(p.x), plot.bottom()),
                );
                painter.rect_filled(band, 0.0, visuals.selection.bg_fill.gamma_multiply(0.35));
            }
            if resp.drag_stopped() {
                self.drag_from = None;
                if let (Some(a), Some(b)) = (
                    bin_at(x0.min(p.x).max(plot.left())),
                    bin_at(x0.max(p.x).min(plot.right() - 0.5)),
                ) && let (Some(first), Some(last)) = (bins.get(a), bins.get(b))
                {
                    let r = (first.start, last.end);
                    self.push(r, self.filter.clone());
                }
            }
        }
        // Hover tooltip, click to select, double-click to zoom, wheel to zoom.
        if let Some(i) = resp.hover_pos().and_then(|p| bin_at(p.x)) {
            if let Some(b) = bins.get(i) {
                let b = b.clone();
                let label = unit.label(b.start, &tz);
                let end_label = TimeUnit::Seconds.label(b.end - 1, &tz);
                let resp2 = resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.strong(label.as_str());
                    ui.weak(format!("until {end_label}"));
                    for (s, &c) in b.counts.iter().enumerate() {
                        if c > 0 {
                            ui.horizontal(|ui| {
                                swatch(
                                    ui,
                                    series.get(s).map_or(Color32::GRAY, |(_, rgb)| color(*rgb)),
                                );
                                ui.label(format!(
                                    "{c} {}",
                                    series.get(s).map_or("", |(n, _)| n.as_str())
                                ));
                            });
                        }
                    }
                    if b.total() == 0 {
                        ui.weak("no events");
                    }
                });
                if resp2.double_clicked() {
                    self.push((b.start, b.end), self.filter.clone());
                } else if resp2.clicked() {
                    self.selection = if self.selection == Some((b.start, b.end)) {
                        None
                    } else {
                        Some((b.start, b.end))
                    };
                }
            }
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                let anchor = resp.hover_pos().map(|p| {
                    let frac = f64::from(((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0));
                    self.range.0 + ((self.range.1 - self.range.0) as f64 * frac) as i64
                });
                self.zoom_by(if scroll > 0.0 { 0.8 } else { 1.25 }, anchor);
            }
        }
        if resp.clicked() && resp.hover_pos().and_then(|p| bin_at(p.x)).is_none() {
            self.selection = None;
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
        let mut clicked = None;
        egui::ScrollArea::horizontal()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                let row_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
                TableBuilder::new(ui)
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
                        for t in [
                            "Date/Time",
                            "Event Type",
                            "Description",
                            "Data Source",
                            "Deleted",
                        ] {
                            header.col(|ui| {
                                ui.strong(t);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_h, rows.len(), |mut row| {
                            let i = row.index();
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
                                    Some(m) => ui.monospace(m).on_hover_text(
                                        r.kinds
                                            .iter()
                                            .map(|k| k.name())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    ),
                                    None => ui.label(
                                        r.kinds
                                            .iter()
                                            .map(|k| k.name())
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    ),
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
        let win = TimelineWindow::new(&ctx, sources, DisplayPrefs::default());
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
    }
}
