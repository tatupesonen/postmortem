//! Image Gallery (Autopsy's Image Gallery tool): pictures grouped by folder,
//! shown as a thumbnail grid. Thumbnails decode in the background, only for
//! cells that scroll into view, and are cached.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Sense, Vec2};
use pm_index::{FileCategory, FileIndex};

use crate::format::DisplayPrefs;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow};
use crate::thumbs::ThumbCache;
use crate::viewers::ViewerPanel;

/// A folder of pictures.
#[derive(Debug, Clone)]
struct Group {
    folder: String,
    /// Indices into the file index.
    files: Vec<usize>,
    deleted: usize,
}

pub(crate) struct GalleryWindow {
    index: SharedIndex,
    sources: Vec<DataSource>,
    groups: Option<(Arc<FileIndex>, Vec<Group>)>,
    group: usize,
    group_filter: String,
    thumbs: ThumbCache<usize>,
    selected: Option<usize>,
    thumb_size: f32,
    viewer: ViewerPanel,
    prefs: DisplayPrefs,
    /// File chosen for a pop-out window (taken by the main window).
    pub(crate) popout: Option<Content>,
}

impl std::fmt::Debug for GalleryWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GalleryWindow")
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

/// Content for index entry `i`.
pub(crate) fn self_content(sources: &[DataSource], idx: &FileIndex, i: usize) -> Option<Content> {
    let f = idx.files.get(i)?;
    let ds = sources.iter().find(|d| d.id as u32 == f.ds)?;
    let fs = ds.fs(f.fs as usize)?;
    Some(Content::from_fs(
        Arc::clone(fs),
        FileRow::from_fs(&**fs, f.inode, &f.path, f.name_allocated),
    ))
}

impl GalleryWindow {
    pub(crate) fn new(index: SharedIndex, sources: Vec<DataSource>, prefs: DisplayPrefs) -> Self {
        Self {
            index,
            sources,
            groups: None,
            group: 0,
            group_filter: String::new(),
            thumbs: ThumbCache::default(),
            selected: None,
            thumb_size: 160.0,
            viewer: ViewerPanel::default(),
            prefs,
            popout: None,
        }
    }

    fn build_groups(idx: &FileIndex) -> Vec<Group> {
        let mut map: HashMap<String, Group> = HashMap::new();
        for (i, f) in idx.files.iter().enumerate() {
            if f.category != FileCategory::Images || !f.is_regular() || f.size == 0 {
                continue;
            }
            let ds = idx
                .data_sources
                .get(f.ds as usize)
                .map_or("", String::as_str);
            let folder = format!("{ds}{}", f.parent());
            let g = map.entry(folder.clone()).or_insert_with(|| Group {
                folder,
                files: Vec::new(),
                deleted: 0,
            });
            g.files.push(i);
            g.deleted += usize::from(f.deleted());
        }
        let mut groups: Vec<Group> = map.into_values().collect();
        groups.sort_by_key(|a| a.folder.to_lowercase());
        for g in &mut groups {
            g.files
                .sort_by_key(|&i| idx.files.get(i).map(|f| f.name().to_lowercase()));
        }
        groups
    }

    fn content(&self, idx: &FileIndex, i: usize) -> Option<Content> {
        let f = idx.files.get(i)?;
        let ds = self.sources.iter().find(|d| d.id as u32 == f.ds)?;
        let fs = ds.fs(f.fs as usize)?;
        Some(Content::from_fs(
            Arc::clone(fs),
            FileRow::from_fs(&**fs, f.inode, &f.path, f.name_allocated),
        ))
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.index.ready_or_progress(ui) else {
            return;
        };
        if self
            .groups
            .as_ref()
            .is_none_or(|(i, _)| !Arc::ptr_eq(i, &idx))
        {
            self.groups = Some((Arc::clone(&idx), Self::build_groups(&idx)));
            self.thumbs.clear();
            self.group = 0;
        }
        let groups = self
            .groups
            .as_ref()
            .map(|(_, g)| g.clone())
            .unwrap_or_default();
        if groups.is_empty() {
            ui.centered_and_justified(|ui| ui.heading("No pictures found in these data sources."));
            return;
        }
        let total: usize = groups.iter().map(|g| g.files.len()).sum();
        egui::Panel::top("gal-top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong(format!(
                    "{} pictures in {} folders",
                    crate::format::grouped(total as u64),
                    groups.len()
                ));
                ui.separator();
                ui.label("Thumbnail size:");
                ui.add(egui::Slider::new(&mut self.thumb_size, 64.0..=256.0).show_value(false));
                ui.separator();
                let has_next = self.group + 1 < groups.len();
                if ui
                    .add_enabled(has_next, egui::Button::new("Next group ⏵"))
                    .clicked()
                {
                    self.group += 1;
                }
            });
        });
        egui::Panel::left("gal-groups")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.add(
                    egui::TextEdit::singleline(&mut self.group_filter).hint_text("filter folders"),
                );
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let needle = self.group_filter.to_lowercase();
                        for (gi, g) in groups.iter().enumerate() {
                            if !needle.is_empty() && !g.folder.to_lowercase().contains(&needle) {
                                continue;
                            }
                            let label = if g.deleted > 0 {
                                format!("{} ({}, {} deleted)", g.folder, g.files.len(), g.deleted)
                            } else {
                                format!("{} ({})", g.folder, g.files.len())
                            };
                            if ui.selectable_label(self.group == gi, label).clicked() {
                                self.group = gi;
                            }
                        }
                    });
            });
        egui::Panel::bottom("gal-viewer")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let prefs = self.prefs.clone();
                self.viewer.ui(ui, &prefs);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let Some(g) = groups.get(self.group.min(groups.len() - 1)) else {
                return;
            };
            ui.strong(&g.folder);
            self.grid(ui, &idx, g);
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui, idx: &FileIndex, g: &Group) {
        let cell = Vec2::new(self.thumb_size + 12.0, self.thumb_size + 34.0);
        let per_row = ((ui.available_width() / cell.x).floor() as usize).max(1);
        let rows = g.files.len().div_ceil(per_row);
        let mut clicked = None;
        let mut double = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, cell.y, rows, |ui, range| {
                for r in range {
                    ui.horizontal(|ui| {
                        for &fi in g.files.iter().skip(r * per_row).take(per_row) {
                            let (rect, resp) = ui.allocate_exact_size(cell, Sense::click());
                            self.cell(ui, idx, fi, rect, &resp);
                            if resp.double_clicked() {
                                double = Some(fi);
                            } else if resp.clicked() {
                                clicked = Some(fi);
                            }
                        }
                    });
                }
            });
        if let Some(fi) = clicked {
            self.selected = Some(fi);
            let c = self.content(idx, fi);
            self.viewer.set_content(c);
        }
        if let Some(fi) = double {
            self.popout = self.content(idx, fi);
        }
    }

    fn cell(
        &mut self,
        ui: &egui::Ui,
        idx: &FileIndex,
        fi: usize,
        rect: egui::Rect,
        resp: &egui::Response,
    ) {
        let Some(f) = idx.files.get(fi) else { return };
        let visuals = ui.visuals();
        let painter = ui.painter_at(rect);
        let bg = if self.selected == Some(fi) {
            visuals.selection.bg_fill
        } else if resp.hovered() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.extreme_bg_color
        };
        painter.rect_filled(rect.shrink(2.0), 6.0, bg);
        let img_rect =
            egui::Rect::from_min_size(rect.min + Vec2::new(6.0, 6.0), Vec2::splat(self.thumb_size));
        // Decoding starts only for cells that are actually drawn (visible).
        let content = || self_content(&self.sources, idx, fi);
        self.thumbs.paint(ui, img_rect, &fi, content);
        let name_color = if f.deleted() {
            visuals.error_fg_color
        } else {
            visuals.text_color()
        };
        let name = if f.deleted() {
            format!("✖ {}", f.name())
        } else {
            f.name().to_owned()
        };
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 16.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(12.0),
            name_color,
        );
        resp.clone().on_hover_text(&*f.path);
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn gallery_groups_and_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.dd");
        std::fs::write(&path, pm_testkit::demo::demo_disk()).unwrap();
        let evidence = pm_evidence::open(&path, &pm_evidence::OpenOptions::default()).unwrap();
        let sources = vec![DataSource {
            id: 0,
            name: "demo.dd".into(),
            evidence,
        }];
        let ctx = egui::Context::default();
        let index = SharedIndex::build(&ctx, &sources);
        let win = GalleryWindow::new(index, sources, DisplayPrefs::default());
        let mut h = Harness::builder()
            .with_size(egui::vec2(1300.0, 850.0))
            .build_ui_state(|ui, w: &mut GalleryWindow| w.ui(ui), win);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            h.step();
            let ready = h.state().thumbs.any_ready();
            if ready {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "thumbnails did not load"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.run_steps(4);
        h.get_by_label_contains("pictures in");
        // Folders: NTFS Pictures and the FAT DCIM folder (plus the deleted photo at the FAT root).
        assert!(h.query_all_by_label_contains("Pictures").count() >= 1);
        assert!(h.query_all_by_label_contains("DCIM").count() >= 1);
        if let Ok(d) = std::env::var("PM_UI_SCREENSHOT_DIR") {
            h.render()
                .unwrap()
                .save(std::path::PathBuf::from(d).join("gallery.png"))
                .unwrap();
        }
    }
}
