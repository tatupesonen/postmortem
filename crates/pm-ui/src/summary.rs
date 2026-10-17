//! Data Source Summary (Autopsy's per-data-source dashboard), shown in the
//! result pane when a data source is selected.

use std::sync::Arc;

use egui::{Color32, Pos2, Sense, Shape, Stroke, Vec2};
use pm_index::{FileCategory, FileIndex, IndexedFile};

use crate::format::{DisplayPrefs, grouped, size_long};
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow};
use crate::sortable::{Sort, cmp_text};

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

#[derive(Debug, Default)]
pub(crate) struct SummaryState {
    tab: Tab,
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

impl SummaryState {
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        ds: &DataSource,
        index: &SharedIndex,
        prefs: &DisplayPrefs,
    ) -> Option<Action> {
        ui.heading(&ds.name);
        ui.horizontal(|ui| {
            for t in Tab::ALL {
                ui.selectable_value(&mut self.tab, t, t.label());
            }
        });
        ui.separator();
        if self.tab == Tab::Container {
            container(ui, ds);
            return None;
        }
        let idx = index.ready_or_progress(ui)?;
        let ds_id = ds.id as u32;
        match self.tab {
            Tab::Types => {
                types(ui, &idx, ds);
                None
            }
            Tab::RecentFiles => recent(ui, &idx, ds, prefs),
            Tab::Timeline => activity(ui, &idx, ds_id, prefs),
            Tab::Container => None,
        }
    }
}

fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<egui::WidgetText>) {
    ui.strong(k);
    ui.add(egui::Label::new(v).selectable(true));
    ui.end_row();
}

fn types(ui: &mut egui::Ui, idx: &FileIndex, ds: &DataSource) {
    let s = idx.type_summary(ds.id as u32);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            egui::Grid::new("types-grid")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    kv(
                        ui,
                        "Size",
                        pm_core::units::format_size(ds.evidence.image.info.size),
                    );
                    let fs: Vec<String> = ds
                        .evidence
                        .filesystems
                        .iter()
                        .map(|f| f.fs.info().kind.to_string())
                        .collect();
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
                    kv(ui, "Bytes in allocated files", size_long(s.total_bytes));
                });
        });
        ui.add_space(32.0);
        ui.vertical(|ui| {
            ui.strong("File Types");
            ui.weak("by extension until type detection runs");
            pie(ui, &s.categories);
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

fn content(ds: &DataSource, f: &IndexedFile) -> Option<Content> {
    let fs = ds.fs(f.fs as usize)?;
    Some(Content::from_fs(
        Arc::clone(fs),
        FileRow::from_fs(&**fs, f.inode, &f.path, f.name_allocated),
    ))
}

fn recent(
    ui: &mut egui::Ui,
    idx: &FileIndex,
    ds: &DataSource,
    prefs: &DisplayPrefs,
) -> Option<Action> {
    let mut action = None;
    ui.weak("Based on file-system times. Recent documents and downloads from Recent Activity appear here after ingest.");
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            for (title, pick) in [
                (
                    "Recently Modified Files",
                    (|t: &pm_fs::Times| t.modified) as fn(&pm_fs::Times) -> Option<jiff::Timestamp>,
                ),
                ("Recently Accessed Files", |t: &pm_fs::Times| t.accessed),
                ("Recently Created Files", |t: &pm_fs::Times| t.created),
            ] {
                ui.add_space(8.0);
                ui.strong(title);
                let mut files = idx.most_recent(ds.id as u32, 10, pick);
                // The sort lives in egui memory (these lists are tiny).
                let sort_id = ui.id().with(("recent-sort", title));
                let mut sort: Option<Sort> = ui.data(|d| d.get_temp(sort_id)).flatten();
                if let Some(s) = sort {
                    files.sort_by(|a, b| {
                        let o = if s.col == 0 {
                            pick(&a.times).cmp(&pick(&b.times))
                        } else {
                            cmp_text(&a.path, &b.path)
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
                                        ui.label(prefs.time(pick(&f.times)));
                                    });
                                    row.col(|ui| {
                                        let text = egui::RichText::new(&*f.path);
                                        let text = if f.deleted() {
                                            text.color(ui.visuals().error_fg_color)
                                        } else {
                                            text
                                        };
                                        if ui
                                            .link(text)
                                            .on_hover_text("Show this file in the content viewer")
                                            .clicked()
                                        {
                                            action = content(ds, f)
                                                .map(|c| Action::ShowFile(Box::new(c)));
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

fn activity(ui: &mut egui::Ui, idx: &FileIndex, ds: u32, prefs: &DisplayPrefs) -> Option<Action> {
    const DAY: i64 = 86_400;
    let times: Vec<i64> = idx
        .of_data_source(ds)
        .flat_map(|f| [f.times.modified, f.times.accessed, f.times.created])
        .flatten()
        .map(jiff::Timestamp::as_second)
        .filter(|&s| s > 0)
        .collect();
    let (Some(&first), Some(&last)) = (times.iter().min(), times.iter().max()) else {
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
    // Autopsy: the 30 days ending at the latest activity, by day.
    let start = last - 30 * DAY;
    let mut days = [0u32; 31];
    for t in times.iter().filter(|&&t| t >= start) {
        if let Some(d) = days.get_mut(((t - start) / DAY) as usize) {
            *d += 1;
        }
    }
    ui.add_space(8.0);
    ui.strong("Activity in the last 30 days of this data source");
    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width().min(900.0), 140.0),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    let max =
        f32::from(u16::try_from(*days.iter().max().unwrap_or(&1)).unwrap_or(u16::MAX)).max(1.0);
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

fn container(ui: &mut egui::Ui, d: &DataSource) {
    let info = &d.evidence.image.info;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new("ds-container")
                .num_columns(2)
                .striped(true)
                .spacing([24.0, 4.0])
                .show(ui, |ui| {
                    kv(ui, "Image format", info.format.to_string());
                    kv(ui, "Size", size_long(info.size));
                    kv(ui, "Sector size", info.sector_size.to_string());
                    for (i, p) in info.paths.iter().enumerate() {
                        kv(
                            ui,
                            if i == 0 { "Segments" } else { "" },
                            p.display().to_string(),
                        );
                    }
                    for (algo, h) in &info.stored_hashes {
                        kv(ui, &format!("Stored {algo:?}"), h.clone());
                    }
                    for (k, v) in &info.metadata {
                        kv(ui, k, v.clone());
                    }
                    kv(
                        ui,
                        "Volume system",
                        d.evidence.volume_system.as_ref().map_or_else(
                            || "None (single volume)".to_owned(),
                            |v| format!("{} ({} partitions)", v.kind, v.allocated().count()),
                        ),
                    );
                    for f in &d.evidence.filesystems {
                        let i = f.fs.info();
                        kv(
                            ui,
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
        });
}
