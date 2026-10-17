//! Tools › Geolocation (Autopsy `GeolocationTopComponent`): the case's
//! waypoints, tracks, routes and areas on an OpenStreetMap map, with
//! Autopsy's filters (data sources, most recent N days, waypoints without
//! time) and a details panel for the clicked point.
//!
//! Points load in a background task; only markers inside the visible map
//! are drawn.

use std::sync::Arc;

use pm_case::Case;
use pm_geo::{GeoData, GeoFilter, PathKind, Waypoint, WaypointKind};
use walkers::{
    HttpOptions, HttpTiles, Map, MapMemory, Plugin, Projector, lon_lat, sources::OpenStreetMap,
};

use crate::format::DisplayPrefs;
use crate::task::Cached;

/// Autopsy's colours per waypoint kind (map markers and legend).
fn colour(kind: WaypointKind) -> egui::Color32 {
    match kind {
        WaypointKind::Exif => egui::Color32::from_rgb(0x99, 0x33, 0xcc),
        WaypointKind::Bookmark => egui::Color32::from_rgb(0x1e, 0x6f, 0xd9),
        WaypointKind::LastKnown => egui::Color32::from_rgb(0xd9, 0x2e, 0x2e),
        WaypointKind::Search => egui::Color32::from_rgb(0x55, 0x55, 0x55),
        WaypointKind::Trackpoint | WaypointKind::TrackPoint => {
            egui::Color32::from_rgb(0x2e, 0xa0, 0x43)
        }
        WaypointKind::RoutePoint | WaypointKind::AreaPoint => {
            egui::Color32::from_rgb(0xe0, 0xa0, 0x00)
        }
    }
}

/// Loaded points, flattened for drawing and picking.
#[derive(Debug, Default)]
pub(crate) struct GeoView {
    data: GeoData,
    points: Vec<Waypoint>,
}

fn load(case: &Case, filter: &GeoFilter) -> Result<GeoView, String> {
    let data = pm_geo::load(case.db(), filter).map_err(|e| e.to_string())?;
    // Path points first, so standalone waypoints draw on top of them.
    let points = data
        .paths
        .iter()
        .flat_map(|p| p.points.iter())
        .chain(&data.waypoints)
        .cloned()
        .collect();
    Ok(GeoView { data, points })
}

pub(crate) struct GeoWindow {
    view: Cached<GeoView>,
    filter: GeoFilter,
    /// The filter the shown data was loaded with.
    loaded: Option<GeoFilter>,
    sources: Vec<(i64, String)>,
    use_days: bool,
    days: i64,
    tiles: Option<HttpTiles>,
    memory: MapMemory,
    fitted: bool,
    selected: Option<usize>,
}

impl std::fmt::Debug for GeoWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoWindow")
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

/// A click on the map: the marker under it, if any.
#[derive(Debug, Default)]
struct Pick {
    clicked: bool,
    point: Option<usize>,
}

/// Draws paths and markers; reports the marker clicked.
struct Overlay<'a> {
    view: &'a GeoView,
    selected: Option<usize>,
    pick: &'a mut Pick,
}

impl Plugin for Overlay<'_> {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &Projector,
        _: &walkers::MapMemory,
    ) {
        let painter = ui.painter_at(response.rect);
        let rect = response.rect.expand(8.0);
        let at = |w: &Waypoint| {
            projector
                .project(lon_lat(w.longitude, w.latitude))
                .to_pos2()
        };
        for p in &self.view.data.paths {
            let line: Vec<egui::Pos2> = p.points.iter().map(at).collect();
            let colour = match p.kind {
                PathKind::Track => colour(WaypointKind::TrackPoint),
                PathKind::Route | PathKind::Area => colour(WaypointKind::RoutePoint),
            };
            for seg in line.windows(2) {
                if let [a, b] = seg {
                    painter.line_segment([*a, *b], egui::Stroke::new(2.5, colour));
                }
            }
            if p.kind == PathKind::Area
                && let (Some(a), Some(b)) = (line.first(), line.last())
            {
                painter.line_segment([*b, *a], egui::Stroke::new(2.5, colour));
            }
        }
        let click = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten();
        let mut best: Option<(f32, usize)> = None;
        for (i, w) in self.view.points.iter().enumerate() {
            let pos = at(w);
            if !rect.contains(pos) {
                continue;
            }
            let selected = self.selected == Some(i);
            painter.circle(
                pos,
                if selected { 7.0 } else { 5.0 },
                colour(w.kind),
                egui::Stroke::new(if selected { 2.5 } else { 1.0 }, egui::Color32::WHITE),
            );
            if let Some(c) = click {
                let d = pos.distance(c);
                if d <= 10.0 && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, i));
                }
            }
        }
        painter.text(
            response.rect.right_bottom() - egui::vec2(6.0, 4.0),
            egui::Align2::RIGHT_BOTTOM,
            "© OpenStreetMap contributors",
            egui::FontId::proportional(11.0),
            egui::Color32::from_gray(60),
        );
        if click.is_some() {
            *self.pick = Pick {
                clicked: true,
                point: best.map(|(_, i)| i),
            };
        }
    }
}

impl GeoWindow {
    pub(crate) fn new(sources: Vec<(i64, String)>) -> Self {
        Self {
            view: Cached::default(),
            filter: GeoFilter {
                data_sources: sources.iter().map(|s| s.0).collect(),
                most_recent_days: None,
                include_without_time: true,
            },
            loaded: None,
            sources,
            use_days: false,
            days: 30,
            tiles: None,
            memory: MapMemory::default(),
            fitted: false,
            selected: None,
        }
    }

    /// Centres and zooms on every loaded point.
    fn fit(&mut self, view: &GeoView) {
        let (mut min_la, mut max_la, mut min_lo, mut max_lo) =
            (90.0f64, -90.0f64, 180.0f64, -180.0f64);
        for w in &view.points {
            min_la = min_la.min(w.latitude);
            max_la = max_la.max(w.latitude);
            min_lo = min_lo.min(w.longitude);
            max_lo = max_lo.max(w.longitude);
        }
        if view.points.is_empty() {
            return;
        }
        self.memory.center_at(lon_lat(
            f64::midpoint(min_lo, max_lo),
            f64::midpoint(min_la, max_la),
        ));
        let span = (max_lo - min_lo).max((max_la - min_la) * 2.0).max(0.005);
        let zoom = (360.0 / span).log2().clamp(2.0, 16.0);
        let _ = self.memory.set_zoom(zoom);
    }

    /// Draws the window; `false` once closed.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        prefs: &DisplayPrefs,
    ) -> bool {
        if self.tiles.is_none() {
            let cache = case.dir().join("Temp").join("map-tiles");
            let options = HttpOptions {
                cache: Some(cache),
                ..HttpOptions::default()
            };
            self.tiles = Some(HttpTiles::with_options(OpenStreetMap, options, ctx.clone()));
        }
        if self.loaded.as_ref() != Some(&self.filter) {
            self.loaded = Some(self.filter.clone());
            self.view.refresh();
            self.selected = None;
            self.fitted = false;
        }
        let (loader, filter) = (Arc::clone(case), self.filter.clone());
        let view = self.view.get(ctx, move || load(&loader, &filter));
        let mut open = true;
        let mut pick = Pick::default();
        egui::Window::new("Geolocation")
            .open(&mut open)
            .default_size([1100.0, 720.0])
            .show(ctx, |ui| {
                egui::Panel::left("geo-filters")
                    .resizable(true)
                    .default_size(230.0)
                    .show(ui, |ui| self.filters(ui, view.as_deref()));
                if let Some(v) = view.as_deref() {
                    egui::Panel::bottom("geo-details")
                        .default_size(120.0)
                        .show(ui, |ui| self.details(ui, v, prefs));
                }
                egui::CentralPanel::default().show(ui, |ui| {
                    let Some(v) = view.as_deref() else {
                        if let Some(e) = self.view.error() {
                            ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
                        } else {
                            ui.spinner();
                        }
                        return;
                    };
                    if !self.fitted {
                        self.fit(v);
                        self.fitted = true;
                    }
                    let tiles = self.tiles.as_mut().map(|t| t as &mut dyn walkers::Tiles);
                    ui.add(
                        Map::new(tiles, &mut self.memory, lon_lat(24.94, 60.17)).with_plugin(
                            Overlay {
                                view: v,
                                selected: self.selected,
                                pick: &mut pick,
                            },
                        ),
                    );
                });
            });
        if pick.clicked {
            self.selected = pick.point;
        }
        open
    }

    fn filters(&mut self, ui: &mut egui::Ui, view: Option<&GeoView>) {
        ui.strong("Data Sources");
        for (id, name) in &self.sources {
            let mut on = self.filter.data_sources.contains(id);
            if ui.checkbox(&mut on, name).changed() {
                if on {
                    self.filter.data_sources.push(*id);
                } else {
                    self.filter.data_sources.retain(|d| d != id);
                }
            }
        }
        ui.separator();
        let mut changed = ui
            .checkbox(&mut self.use_days, "Show waypoints from the most recent")
            .changed();
        ui.horizontal(|ui| {
            changed |= ui
                .add_enabled(
                    self.use_days,
                    egui::DragValue::new(&mut self.days).range(0..=36_500),
                )
                .changed();
            ui.label("days");
        });
        changed |= ui
            .add_enabled(
                self.use_days,
                egui::Checkbox::new(
                    &mut self.filter.include_without_time,
                    "Include waypoints with no time stamps",
                ),
            )
            .changed();
        if changed {
            self.filter.most_recent_days = self.use_days.then_some(self.days);
        }
        ui.separator();
        ui.strong("Legend");
        for k in [
            WaypointKind::Exif,
            WaypointKind::Bookmark,
            WaypointKind::LastKnown,
            WaypointKind::Search,
            WaypointKind::Trackpoint,
            WaypointKind::RoutePoint,
        ] {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, colour(k));
                ui.label(k.display());
            });
        }
        if let Some(v) = view {
            ui.separator();
            ui.weak(format!(
                "{} waypoints, {} tracks/routes/areas",
                crate::format::grouped(v.data.waypoints.len() as u64),
                crate::format::grouped(v.data.paths.len() as u64)
            ));
        }
    }

    fn details(&self, ui: &mut egui::Ui, view: &GeoView, prefs: &DisplayPrefs) {
        let Some(w) = self.selected.and_then(|i| view.points.get(i)) else {
            ui.weak("Click a waypoint to see its details.");
            return;
        };
        egui::Grid::new("geo-detail")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: String| {
                    ui.label(k);
                    ui.label(v);
                    ui.end_row();
                };
                row(ui, "Type", w.kind.display().to_owned());
                row(ui, "Name", w.label.clone());
                row(
                    ui,
                    "Time",
                    prefs.time(w.time.and_then(|t| jiff::Timestamp::from_second(t).ok())),
                );
                row(ui, "Latitude", format!("{:.6}", w.latitude));
                row(ui, "Longitude", format!("{:.6}", w.longitude));
                if let Some(a) = w.altitude {
                    row(ui, "Altitude", format!("{a:.1}"));
                }
            });
    }
}
