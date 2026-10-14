//! Image Gallery (Autopsy's Image Gallery tool): pictures grouped by folder,
//! shown as a thumbnail grid. Thumbnails decode in the background, only for
//! cells that scroll into view, and are cached.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, Sense, Vec2};
use pm_index::{FileCategory, FileIndex};

use crate::format::DisplayPrefs;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource, FileRow};
use crate::task::Task;
use crate::viewers::ViewerPanel;

/// Pictures larger than this are not thumbnailed.
const MAX_THUMB_SOURCE: u64 = 64 * 1024 * 1024;
const THUMB_PX: u32 = 192;

enum Thumb {
    Loading(Task<Option<egui::ColorImage>>),
    Ready(egui::TextureHandle),
    Failed,
}

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
    thumbs: HashMap<usize, Thumb>,
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

fn decode_thumb(src: &pm_core::SharedSource) -> Option<egui::ColorImage> {
    if src.size() > MAX_THUMB_SOURCE {
        return None;
    }
    let bytes = src.read_vec_at(0, src.size() as usize).ok()?;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    reader.limits(limits);
    let thumb = reader
        .decode()
        .ok()?
        .thumbnail(THUMB_PX, THUMB_PX)
        .to_rgba8();
    let size = [thumb.width() as usize, thumb.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        thumb.as_raw(),
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
            thumbs: HashMap::new(),
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
        Some(Content {
            fs: Arc::clone(fs),
            row: FileRow::from_fs(&**fs, f.inode, &f.path, f.name_allocated),
        })
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
        // Kick off decoding for visible cells only.
        if !self.thumbs.contains_key(&fi) {
            let content = self.content(idx, fi);
            let task = Task::spawn(ui.ctx(), move || {
                content
                    .and_then(|c| c.source().ok())
                    .and_then(|s| decode_thumb(&s))
            });
            self.thumbs.insert(fi, Thumb::Loading(task));
        }
        let ctx = ui.ctx().clone();
        if let Some(t) = self.thumbs.get_mut(&fi) {
            if let Thumb::Loading(task) = t
                && let Some(result) = task.poll()
            {
                *t = match result {
                    Some(img) => Thumb::Ready(ctx.load_texture(
                        format!("thumb-{fi}"),
                        img.clone(),
                        egui::TextureOptions::LINEAR,
                    )),
                    None => Thumb::Failed,
                };
            }
            match t {
                Thumb::Ready(tex) => {
                    let size = tex.size_vec2();
                    let scale = (self.thumb_size / size.x).min(self.thumb_size / size.y);
                    let r = egui::Rect::from_center_size(img_rect.center(), size * scale);
                    painter.image(
                        tex.id(),
                        r,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
                Thumb::Loading(_) => {
                    painter.text(
                        img_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "…",
                        egui::FontId::proportional(20.0),
                        visuals.weak_text_color(),
                    );
                }
                Thumb::Failed => {
                    painter.text(
                        img_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "⚠ cannot decode",
                        egui::FontId::proportional(12.0),
                        visuals.warn_fg_color,
                    );
                }
            }
        }
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
mod tests {
    use super::*;

    #[test]
    fn thumbnails_are_bounded() {
        let bmp = pm_testkit::demo::gradient_bmp(640, 320);
        let src: pm_core::SharedSource = Arc::new(pm_core::io::MemSource::new(bmp));
        let t = decode_thumb(&src).unwrap();
        assert_eq!(t.size, [192, 96]);
        let junk: pm_core::SharedSource =
            Arc::new(pm_core::io::MemSource::new(b"not a picture".to_vec()));
        assert!(decode_thumb(&junk).is_none());
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
            let ready = h
                .state()
                .thumbs
                .values()
                .any(|t| matches!(t, Thumb::Ready(_)));
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
