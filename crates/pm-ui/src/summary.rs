//! Data Source Summary (Autopsy's per-data-source dashboard), shown in the
//! result pane when a data source is selected. Everything comes from the
//! case database (ADR-023), loaded once per data source in the background.

use std::sync::Arc;

use egui::{Color32, Pos2, Sense, Shape, Stroke, Vec2};
use pm_case::Case;
use pm_casedb::mcp::{EventFilter, EventKeyset, TimeColumn, VolumeSystemRow};
use pm_casedb::summary::DataSourceCounts;
use pm_casedb::{DataSourceRecord, FileRecord, FileSystemRecord};
use pm_timeline::FileCategory;

use crate::format::{DisplayPrefs, grouped, size_long};
use crate::model::Content;
use crate::sortable::{Sort, cmp_text};
use crate::task::Keyed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tab {
    #[default]
    Types,
    RecentFiles,
    Timeline,
    Container,
}

impl Tab {
    const ALL: [Self; 4] = [
        Self::Types,
        Self::RecentFiles,
        Self::Timeline,
        Self::Container,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Types => "Types",
            Self::RecentFiles => "Recent Files",
            Self::Timeline => "Timeline",
            Self::Container => "Container",
        }
    }
}

/// Something the summary wants the main window to do.
#[derive(Debug)]
pub(crate) enum Action {
    /// Open the Timeline on `range` restricted to the summarised data
    /// source.
    ViewInTimeline {
        range: (i64, i64),
    },
    ShowFile(Box<Content>),
}

const DAY: i64 = 86_400;

/// Everything the tabs show, for one data source.
#[derive(Debug, Default)]
pub(crate) struct SummaryData {
    counts: DataSourceCounts,
    /// Regular files per [`FileCategory::ALL`] entry.
    categories: [u64; 6],
    /// Most recently modified / accessed / created regular files.
    recent: [Vec<FileRecord>; 3],
    /// Earliest and latest activity, and counts per day of the last 30.
    activity: Option<(i64, i64, [u64; 31])>,
    file_systems: Vec<FileSystemRecord>,
    volume_systems: Vec<VolumeSystemRow>,
}

impl SummaryData {
    fn load(case: &Case, ds_id: i64) -> Result<Self, String> {
        let db = case.db();
        let e = |e: pm_casedb::DbError| e.to_string();
        let counts = db.data_source_counts(ds_id).map_err(e)?;
        let mut categories = [0u64; 6];
        for (ext, n) in &counts.by_extension {
            let cat = FileCategory::from_name(&format!("x.{ext}"));
            if let Some(i) = FileCategory::ALL.iter().position(|c| *c == cat)
                && let Some(c) = categories.get_mut(i)
            {
                *c += n;
            }
        }
        let recent_of = |col: TimeColumn| {
            let f = EventFilter {
                ds_id: Some(ds_id),
                start: Some(1),
                regular_only: true,
                ..EventFilter::default()
            };
            db.mcp_file_events(col, &f, EventKeyset::Start, true, 10)
        };
        let recent = [
            recent_of(TimeColumn::Modified).map_err(e)?,
            recent_of(TimeColumn::Accessed).map_err(e)?,
            recent_of(TimeColumn::Created).map_err(e)?,
        ];
        let activity = match db.data_source_activity_span(ds_id).map_err(e)? {
            None => None,
            Some((first, last)) => {
                // Autopsy: the 30 days ending at the latest activity, by day.
                let start = last - 30 * DAY;
                let f = EventFilter {
                    ds_id: Some(ds_id),
                    start: Some(start.max(1)),
                    end: Some(last + 1),
                    ..EventFilter::default()
                };
                let mut days = [0u64; 31];
                for col in [
                    TimeColumn::Modified,
                    TimeColumn::Accessed,
                    TimeColumn::Created,
                ] {
                    for (b, n) in db
                        .mcp_file_event_histogram(col, &f, start, DAY)
                        .map_err(e)?
                    {
                        if let Some(d) = usize::try_from(b).ok().and_then(|b| days.get_mut(b)) {
                            *d += n;
                        }
                    }
                }
                Some((first, last, days))
            }
        };
        Ok(Self {
            counts,
            categories,
            recent,
            activity,
            file_systems: db.file_systems(ds_id).map_err(e)?,
            volume_systems: db.mcp_volume_systems(ds_id).map_err(e)?,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct SummaryState {
    tab: Tab,
    /// Keyed by data source and whether analysis was running, so the
    /// summary reloads when ingest finishes (until the case change bus).
    data: Keyed<(i64, bool), SummaryData>,
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

impl SummaryState {
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        case: &Arc<Case>,
        ds: &DataSourceRecord,
        live: bool,
        prefs: &DisplayPrefs,
    ) -> Option<Action> {
        ui.heading(&ds.name);
        ui.horizontal(|ui| {
            for t in Tab::ALL {
                ui.selectable_value(&mut self.tab, t, t.label());
            }
        });
        ui.separator();
        let loader = Arc::clone(case);
        let ds_id = ds.id;
        let Some(data) = self.data.get(ui.ctx(), &(ds_id, live), move || {
            SummaryData::load(&loader, ds_id)
        }) else {
            if let Some(e) = self.data.error() {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            } else {
                ui.spinner();
            }
            return None;
        };
        match self.tab {
            Tab::Types => {
                types(ui, &data, ds);
                None
            }
            Tab::RecentFiles => recent(ui, &data, case, prefs),
            Tab::Timeline => activity(ui, &data, prefs),
            Tab::Container => {
                container(ui, ds, &data);
                None
            }
        }
    }
}

fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<egui::WidgetText>) {
    ui.strong(k);
    ui.add(egui::Label::new(v).selectable(true));
    ui.end_row();
}

fn types(ui: &mut egui::Ui, data: &SummaryData, ds: &DataSourceRecord) {
    let s = &data.counts;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            egui::Grid::new("types-grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    kv(ui, "Size", pm_core::units::format_size(ds.size));
                    let fs: Vec<String> =
                        data.file_systems.iter().map(|f| f.kind.clone()).collect();
                    kv(
                        ui,
                        "File systems",
                        if fs.is_empty() {
                            "none detected".to_owned()
                        } else {
                            fs.join(", ")
                        },
                    );
                    kv(ui, "Allocated files", grouped(s.allocated_files));
                    kv(ui, "Unallocated files", grouped(s.unallocated_files));
                    kv(ui, "Directories", grouped(s.directories));
                    kv(ui, "Bytes in allocated files", size_long(s.allocated_bytes));
                });
        });
        ui.add_space(32.0);
        ui.vertical(|ui| {
            ui.strong("File Types");
            ui.weak("by extension until type detection runs");
            pie(ui, &data.categories);
        });
    });
}

/// A pie chart with an Autopsy-style legend ("Images: 12 (34.5%)").
fn pie(ui: &mut egui::Ui, counts: &[u64; 6]) {
    let total: u64 = counts.iter().sum();
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(180.0), Sense::hover());
        let c = rect.center();
        let r = rect.width() / 2.0 - 4.0;
        let painter = ui.painter_at(rect);
        if total == 0 {
            painter.circle_stroke(c, r, Stroke::new(1.0, ui.visuals().weak_text_color()));
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "No Data",
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        } else {
            let mut angle = -std::f32::consts::FRAC_PI_2;
            for (cat, &n) in FileCategory::ALL.iter().zip(counts) {
                if n == 0 {
                    continue;
                }
                let sweep = n as f32 / total as f32 * std::f32::consts::TAU;
                // Split into ≤ 90° pieces so each polygon is convex.
                let pieces = (sweep / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
                for p in 0..pieces {
                    let a0 = angle + sweep * p as f32 / pieces as f32;
                    let a1 = angle + sweep * (p + 1) as f32 / pieces as f32;
                    let steps = 16;
                    let mut pts = vec![c];
                    for k in 0..=steps {
                        let a = a0 + (a1 - a0) * k as f32 / steps as f32;
                        pts.push(c + Vec2::angled(a) * r);
                    }
                    painter.add(Shape::convex_polygon(pts, rgb(cat.color()), Stroke::NONE));
                }
                angle += sweep;
            }
        }
        ui.vertical(|ui| {
            for (cat, &n) in FileCategory::ALL.iter().zip(counts) {
                if n == 0 {
                    continue;
                }
                ui.horizontal(|ui| {
                    let (sw, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().rect_filled(sw, 2.0, rgb(cat.color()));
                    let pct = n as f64 * 100.0 / total.max(1) as f64;
                    ui.label(format!("{}: {} ({pct:.1}%)", cat.name(), grouped(n)));
                });
            }
        });
    });
}

fn recent(
    ui: &mut egui::Ui,
    data: &SummaryData,
    case: &Arc<Case>,
    prefs: &DisplayPrefs,
) -> Option<Action> {
    let mut action = None;
    ui.weak("Based on file-system times. Recent documents and downloads from Recent Activity appear here after ingest.");
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (k, (title, col)) in [
                ("Recently Modified Files", TimeColumn::Modified),
                ("Recently Accessed Files", TimeColumn::Accessed),
                ("Recently Created Files", TimeColumn::Created),
            ]
            .into_iter()
            .enumerate()
            {
                let pick = |f: &FileRecord| {
                    f.times
                        .get(col.index())
                        .copied()
                        .flatten()
                        .and_then(|(s, _)| jiff::Timestamp::from_second(s).ok())
                };
                ui.add_space(8.0);
                ui.strong(title);
                let mut files: Vec<&FileRecord> = data
                    .recent
                    .get(k)
                    .map(|v| v.iter().collect())
                    .unwrap_or_default();
                // The sort lives in egui memory (these lists are tiny).
                let sort_id = ui.id().with(("recent-sort", title));
                let mut sort: Option<Sort> = ui.data(|d| d.get_temp(sort_id)).flatten();
                if let Some(s) = sort {
                    files.sort_by(|a, b| {
                        let o = if s.col == 0 {
                            pick(a).cmp(&pick(b))
                        } else {
                            cmp_text(&a.path(), &b.path())
                        };
                        if s.ascending { o } else { o.reverse() }
                    });
                }
                let row_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
                ui.push_id(title, |ui| {
                    egui_extras::TableBuilder::new(ui)
                        .striped(true)
                        .vscroll(false)
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .column(egui_extras::Column::initial(190.0))
                        .column(egui_extras::Column::remainder().at_least(200.0).clip(true))
                        .header(row_h + 2.0, |mut header| {
                            for (i, t) in ["Date/Time", "Path"].into_iter().enumerate() {
                                header.col(|ui| {
                                    crate::sortable::header_mut(ui, t, &mut sort, i);
                                });
                            }
                        })
                        .body(|mut body| {
                            for f in &files {
                                body.row(row_h, |mut row| {
                                    row.col(|ui| {
                                        ui.label(prefs.time(pick(f)));
                                    });
                                    row.col(|ui| {
                                        let text = egui::RichText::new(f.path());
                                        let text = if !f.name_alloc || !f.meta_alloc {
                                            text.color(ui.visuals().error_fg_color)
                                        } else {
                                            text
                                        };
                                        if ui
                                            .link(text)
                                            .on_hover_text("Show this file in the content viewer")
                                            .clicked()
                                        {
                                            action = Some(Action::ShowFile(Box::new(
                                                Content::from_case(
                                                    Arc::clone(case),
                                                    crate::views::row_of(f),
                                                ),
                                            )));
                                        }
                                    });
                                });
                            }
                        });
                });
                ui.data_mut(|d| d.insert_temp(sort_id, sort));
            }
        });
    action
}

fn activity(ui: &mut egui::Ui, data: &SummaryData, prefs: &DisplayPrefs) -> Option<Action> {
    let Some((first, last, days)) = data.activity else {
        ui.weak("No data exists.");
        return None;
    };
    let to_ts = |s: i64| jiff::Timestamp::from_second(s).ok();
    egui::Grid::new("activity-grid")
        .num_columns(2)
        .spacing([24.0, 4.0])
        .show(ui, |ui| {
            kv(ui, "Earliest", prefs.time(to_ts(first)));
            kv(ui, "Latest", prefs.time(to_ts(last)));
        });
    let start = last - 30 * DAY;
    ui.add_space(8.0);
    ui.strong("Activity in the last 30 days of this data source");
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().min(900.0), 140.0),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let max = (*days.iter().max().unwrap_or(&1)).max(1) as f32;
    let w = rect.width() / days.len() as f32;
    for (i, &n) in days.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let h = (n as f32 / max) * (rect.height() - 20.0);
        let x = rect.left() + i as f32 * w;
        painter.rect_filled(
            egui::Rect::from_min_max(
                Pos2::new(x + 1.0, rect.bottom() - 16.0 - h),
                Pos2::new(x + w - 1.0, rect.bottom() - 16.0),
            ),
            1.0,
            rgb(pm_timeline::Category::FileSystem.color()),
        );
    }
    let label = |s: i64| {
        to_ts(s)
            .map(|t| t.strftime("%Y-%m-%d").to_string())
            .unwrap_or_default()
    };
    painter.text(
        rect.left_bottom() + Vec2::new(4.0, -2.0),
        egui::Align2::LEFT_BOTTOM,
        label(start),
        egui::FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );
    painter.text(
        rect.right_bottom() + Vec2::new(-4.0, -2.0),
        egui::Align2::RIGHT_BOTTOM,
        label(last),
        egui::FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );
    ui.add_space(8.0);
    ui.button("⏱ View in Timeline")
        .clicked()
        .then_some(Action::ViewInTimeline {
            range: (start, last + DAY),
        })
}

fn container(ui: &mut egui::Ui, ds: &DataSourceRecord, data: &SummaryData) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new("ds-container")
                .num_columns(2)
                .striped(true)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    kv(
                        ui,
                        "Image format",
                        ds.image_format.clone().unwrap_or_else(|| "—".into()),
                    );
                    kv(ui, "Size", size_long(ds.size));
                    kv(ui, "Sector size", ds.sector_size.to_string());
                    kv(ui, "Time zone", ds.timezone.clone());
                    for (i, p) in ds.paths.iter().enumerate() {
                        kv(ui, if i == 0 { "Segments" } else { "" }, p.clone());
                    }
                    for (name, h) in [
                        ("MD5", &ds.md5),
                        ("SHA-1", &ds.sha1),
                        ("SHA-256", &ds.sha256),
                    ] {
                        if let Some(h) = h {
                            kv(ui, name, h.clone());
                        }
                    }
                    for (k, v) in &ds.acquisition {
                        kv(ui, k, v.clone());
                    }
                    if data.volume_systems.is_empty() {
                        kv(ui, "Volume system", "None (single volume)");
                    }
                    for vs in &data.volume_systems {
                        kv(
                            ui,
                            "Volume system",
                            format!(
                                "{} ({} volumes)",
                                if vs.kind == 1 { "GPT" } else { "DOS (MBR)" },
                                vs.volumes.len()
                            ),
                        );
                    }
                    for f in &data.file_systems {
                        kv(
                            ui,
                            "File system",
                            format!(
                                "{} at offset {} — label {:?}, serial {}",
                                f.kind,
                                f.byte_offset,
                                f.label.as_deref().unwrap_or(""),
                                f.serial.as_deref().unwrap_or("")
                            ),
                        );
                    }
                });
        });
}
