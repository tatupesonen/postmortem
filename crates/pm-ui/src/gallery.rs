//! Image Gallery (Autopsy's Image Gallery tool): the case's pictures grouped
//! by folder, shown as a thumbnail grid.
//!
//! The list comes from the case database (ADR-014), so it costs a few
//! dozen bytes per picture and no evidence walk. Identical pictures (same
//! MD5) are shown once, in the first folder that holds them, with a count
//! of their copies; "Show duplicates" lists every copy. Pictures not yet
//! hashed are always shown. Thumbnails decode in the background, only for
//! cells that scroll into view, and the cache is bounded.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use egui::{Sense, Vec2};
use pm_case::Case;
use pm_casedb::gallery::GalleryFile;

use crate::format::DisplayPrefs;
use crate::model::Content;
use crate::task::Cached;
use crate::thumbs::ThumbCache;
use crate::viewers::ViewerPanel;

/// A folder of pictures.
#[derive(Debug, Clone, Default)]
struct Group {
    folder: String,
    /// Indices into [`Gallery::files`].
    files: Vec<usize>,
    deleted: usize,
    /// Copies left out because an earlier folder shows the same picture.
    duplicates: usize,
}

/// The loaded picture list with both groupings.
#[derive(Debug, Default)]
pub(crate) struct Gallery {
    files: Vec<GalleryFile>,
    /// Every picture.
    all: Vec<Group>,
    /// Each distinct picture once.
    unique: Vec<Group>,
    /// Copies in the case per shown file index (only when > 1).
    copies: HashMap<usize, usize>,
    duplicates: usize,
}

impl Gallery {
    fn load(case: &Case) -> Result<Self, String> {
        let db = case.db();
        let files = db.gallery_files().map_err(|e| e.to_string())?;
        let names: HashMap<i64, String> = db
            .data_sources()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|d| (d.id, d.name))
            .collect();
        Ok(Self::build(files, &names))
    }

    fn build(files: Vec<GalleryFile>, ds_names: &HashMap<i64, String>) -> Self {
        let mut map: HashMap<(i64, Option<i64>, &str), Group> = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            let g = map
                .entry((f.ds_id, f.volume, f.parent_path.as_str()))
                .or_insert_with(|| Group {
                    // Autopsy's unique path: image/vol_N/folder.
                    folder: format!(
                        "{}{}{}",
                        ds_names.get(&f.ds_id).map_or("", String::as_str),
                        f.volume.map(|v| format!("/vol{v}")).unwrap_or_default(),
                        f.parent_path
                    ),
                    ..Group::default()
                });
            g.files.push(i);
            g.deleted += usize::from(f.deleted);
        }
        let mut all: Vec<Group> = map.into_values().collect();
        all.sort_by_cached_key(|g| g.folder.to_lowercase());
        for g in &mut all {
            g.files
                .sort_by_cached_key(|&i| files.get(i).map(|f| f.name.to_lowercase()));
        }
        // A picture is represented by its first allocated copy in display
        // order (else its first copy): a carved or deleted copy never hides
        // the live file.
        let mut first: HashMap<[u8; 16], usize> = HashMap::new();
        for &i in all.iter().flat_map(|g| &g.files) {
            let Some(m) = files.get(i).and_then(|f| f.md5) else {
                continue;
            };
            let better = |cur: usize| {
                files.get(cur).is_some_and(|c| c.deleted)
                    && files.get(i).is_some_and(|f| !f.deleted)
            };
            match first.get(&m) {
                Some(&cur) if !better(cur) => {}
                _ => {
                    first.insert(m, i);
                }
            }
        }
        let mut copies: HashMap<usize, usize> = HashMap::new();
        let mut unique = Vec::with_capacity(all.len());
        let mut duplicates = 0;
        for g in &all {
            let mut u = Group {
                folder: g.folder.clone(),
                ..Group::default()
            };
            for &i in &g.files {
                let Some(f) = files.get(i) else { continue };
                if let Some(&shown) = f.md5.as_ref().and_then(|m| first.get(m))
                    && shown != i
                {
                    *copies.entry(shown).or_insert(1) += 1;
                    u.duplicates += 1;
                    duplicates += 1;
                    continue;
                }
                u.files.push(i);
                u.deleted += usize::from(f.deleted);
            }
            if !u.files.is_empty() {
                unique.push(u);
            }
        }
        Self {
            files,
            all,
            unique,
            copies,
            duplicates,
        }
    }

    fn groups(&self, show_duplicates: bool) -> &[Group] {
        if show_duplicates {
            &self.all
        } else {
            &self.unique
        }
    }
}

pub(crate) struct GalleryWindow {
    case: Arc<Case>,
    gallery: Cached<Gallery>,
    show_duplicates: bool,
    group: usize,
    group_filter: String,
    thumbs: ThumbCache<i64>,
    /// Selected file id.
    selected: Option<i64>,
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

/// Content of a case file, read from the database.
fn content_of(case: &Arc<Case>, id: i64) -> Option<Content> {
    let rec = case.db().file(id).ok()??;
    Some(Content::from_case(
        Arc::clone(case),
        crate::views::row_of(&rec),
    ))
}

impl GalleryWindow {
    pub(crate) fn new(case: Arc<Case>, prefs: DisplayPrefs) -> Self {
        Self {
            case,
            gallery: Cached::default(),
            show_duplicates: false,
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

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        let case = Arc::clone(&self.case);
        let Some(gallery) = self.gallery.get(ui.ctx(), move || Gallery::load(&case)) else {
            ui.centered_and_justified(|ui| {
                if let Some(e) = self.gallery.error() {
                    ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
                } else {
                    ui.spinner();
                }
            });
            return;
        };
        let groups = gallery.groups(self.show_duplicates);
        egui::Panel::top("gal-top").show(ui, |ui| {
            ui.horizontal(|ui| {
                let total: usize = groups.iter().map(|g| g.files.len()).sum();
                ui.strong(format!(
                    "{} pictures in {} folders",
                    crate::format::grouped(total as u64),
                    crate::format::grouped(groups.len() as u64)
                ));
                if gallery.duplicates > 0 {
                    let mut show = self.show_duplicates;
                    if ui
                        .checkbox(
                            &mut show,
                            format!(
                                "Show duplicates ({})",
                                crate::format::grouped(gallery.duplicates as u64)
                            ),
                        )
                        .on_hover_text("Pictures with the same MD5 are otherwise shown once")
                        .changed()
                    {
                        self.show_duplicates = show;
                        self.group = 0;
                    }
                }
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
                if ui
                    .button("⟳ Refresh")
                    .on_hover_text("Reload the list (hashes added by a running ingest)")
                    .clicked()
                {
                    self.gallery.refresh();
                }
            });
        });
        let groups = gallery.groups(self.show_duplicates);
        if groups.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.heading("No pictures found in this case.");
                });
            });
            return;
        }
        egui::Panel::left("gal-groups")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.add(
                    egui::TextEdit::singleline(&mut self.group_filter).hint_text("filter folders"),
                );
                let needle = self.group_filter.to_lowercase();
                let shown: Vec<usize> = (0..groups.len())
                    .filter(|&gi| {
                        needle.is_empty()
                            || groups
                                .get(gi)
                                .is_some_and(|g| g.folder.to_lowercase().contains(&needle))
                    })
                    .collect();
                let row_h = ui.spacing().interact_size.y;
                egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                    ui,
                    row_h,
                    shown.len(),
                    |ui, range| {
                        for &gi in shown.get(range).unwrap_or_default() {
                            let Some(g) = groups.get(gi) else { continue };
                            let mut label = format!("{} ({}", g.folder, g.files.len());
                            if g.deleted > 0 {
                                let _ = write!(label, ", {} deleted", g.deleted);
                            }
                            if g.duplicates > 0 {
                                let _ = write!(label, ", {} duplicates", g.duplicates);
                            }
                            label.push(')');
                            if ui.selectable_label(self.group == gi, label).clicked() {
                                self.group = gi;
                            }
                        }
                    },
                );
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
            self.grid(ui, &gallery, g);
        });
        self.thumbs.trim();
    }

    fn grid(&mut self, ui: &mut egui::Ui, gallery: &Gallery, g: &Group) {
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
                            let Some(f) = gallery.files.get(fi) else {
                                continue;
                            };
                            let (rect, resp) = ui.allocate_exact_size(cell, Sense::click());
                            let copies = if self.show_duplicates {
                                0
                            } else {
                                gallery.copies.get(&fi).copied().unwrap_or(0)
                            };
                            self.cell(ui, f, copies, rect, &resp);
                            if resp.double_clicked() {
                                double = Some(f.id);
                            } else if resp.clicked() {
                                clicked = Some(f.id);
                            }
                        }
                    });
                }
            });
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.viewer.set_content(content_of(&self.case, id));
        }
        if let Some(id) = double {
            self.popout = content_of(&self.case, id);
        }
    }

    fn cell(
        &mut self,
        ui: &egui::Ui,
        f: &GalleryFile,
        copies: usize,
        rect: egui::Rect,
        resp: &egui::Response,
    ) {
        let visuals = ui.visuals();
        let painter = ui.painter_at(rect);
        let bg = if self.selected == Some(f.id) {
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
        let (case, id) = (Arc::clone(&self.case), f.id);
        self.thumbs
            .paint_with(ui, img_rect, &f.id, move || content_of(&case, id));
        if copies > 1 {
            let badge = egui::Rect::from_min_size(
                img_rect.right_top() + Vec2::new(-34.0, 2.0),
                Vec2::new(32.0, 18.0),
            );
            painter.rect_filled(badge, 4.0, visuals.selection.bg_fill);
            painter.text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                format!("×{copies}"),
                egui::FontId::proportional(12.0),
                visuals.strong_text_color(),
            );
        }
        let name_color = if f.deleted {
            visuals.error_fg_color
        } else {
            visuals.text_color()
        };
        let name = if f.deleted {
            format!("✖ {}", f.name)
        } else {
            f.name.clone()
        };
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 16.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(12.0),
            name_color,
        );
        let path = format!("{}{}", f.parent_path, f.name);
        let hover = if copies > 1 {
            format!("{path}\n{copies} identical copies in the case (same MD5)")
        } else {
            path
        };
        resp.clone().on_hover_text(hover);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: i64, dir: &str, name: &str, md5: Option<u8>) -> GalleryFile {
        GalleryFile {
            id,
            ds_id: 1,
            volume: None,
            parent_path: dir.into(),
            name: name.into(),
            size: 10,
            deleted: false,
            md5: md5.map(|b| [b; 16]),
        }
    }

    #[test]
    fn identical_pictures_are_shown_once() {
        let files = vec![
            file(1, "/b/", "x.jpg", Some(1)),
            file(2, "/a/", "y.jpg", Some(1)),
            file(3, "/a/", "z.jpg", None),
            file(4, "/c/", "w.jpg", Some(1)),
            file(5, "/c/", "v.jpg", None),
            GalleryFile {
                deleted: true,
                ..file(6, "/a/", "a-deleted.jpg", Some(2))
            },
            file(7, "/c/", "u.jpg", Some(2)),
        ];
        let names = HashMap::from([(1, "img".to_owned())]);
        let g = Gallery::build(files, &names);
        let folders = |gs: &[Group]| gs.iter().map(|g| g.folder.clone()).collect::<Vec<_>>();
        assert_eq!(folders(&g.all), ["img/a/", "img/b/", "img/c/"]);
        // /a/y.jpg is first; /b/ only had a copy and disappears.
        assert_eq!(folders(&g.unique), ["img/a/", "img/c/"]);
        assert_eq!(g.duplicates, 3);
        let shown: Vec<i64> = g
            .unique
            .iter()
            .flat_map(|gr| gr.files.iter().map(|&i| g.files[i].id))
            .collect();
        assert_eq!(
            shown,
            [2, 3, 7, 5],
            "unhashed pictures always show; the allocated copy wins"
        );
        let y = g.files.iter().position(|f| f.id == 2).unwrap();
        assert_eq!(g.copies.get(&y), Some(&3));
        assert_eq!(g.unique[1].duplicates, 1);
    }
}
