//! Discovery window (Autopsy's Discovery tool for files).

use std::sync::Arc;

use egui::{Sense, Vec2};
use pm_discovery::{
    Criteria, FolderTerm, Group, GroupBy, GroupOrder, ItemOrder, ResultType, SizeBucket, search,
};
use pm_index::FileIndex;

use crate::format::{DisplayPrefs, grouped};
use crate::gallery::self_content;
use crate::index::SharedIndex;
use crate::model::{Content, DataSource};
use crate::sortable::{Order, Sort, cmp_text};
use crate::thumbs::ThumbCache;
use crate::viewers::ViewerPanel;

const PAGE_SIZES: [usize; 8] = [25, 50, 75, 100, 125, 150, 175, 200];

pub(crate) struct DiscoveryWindow {
    index: SharedIndex,
    sources: Vec<DataSource>,
    criteria: Criteria,
    group_by: GroupBy,
    group_order: GroupOrder,
    item_order: ItemOrder,
    folder_text: String,
    folder_substring: bool,
    folder_include: bool,
    results: Option<(Arc<FileIndex>, Vec<Group>)>,
    group: usize,
    page: usize,
    page_size: usize,
    selected: Option<usize>,
    /// List view column sort (`None`: the "Order Within Groups By"
    /// choice); applies to the whole group, not just the page.
    list_sort: Option<Sort>,
    list_order: Order,
    thumbs: ThumbCache<usize>,
    viewer: ViewerPanel,
    prefs: DisplayPrefs,
    pub(crate) popout: Option<Content>,
}

impl std::fmt::Debug for DiscoveryWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryWindow")
            .field("criteria", &self.criteria)
            .finish_non_exhaustive()
    }
}

impl DiscoveryWindow {
    pub(crate) fn new(index: SharedIndex, sources: Vec<DataSource>, prefs: DisplayPrefs) -> Self {
        Self {
            index,
            sources,
            criteria: Criteria::new(ResultType::Images),
            group_by: GroupBy::ParentFolder,
            group_order: GroupOrder::GroupSize,
            item_order: ItemOrder::FileName,
            folder_text: String::new(),
            folder_substring: true,
            folder_include: true,
            results: None,
            group: 0,
            page: 0,
            page_size: 100,
            selected: None,
            list_sort: None,
            list_order: Order::default(),
            thumbs: ThumbCache::default(),
            viewer: ViewerPanel::default(),
            prefs,
            popout: None,
        }
    }

    /// Autopsy's live validation messages.
    fn validation_error(&self) -> Option<&'static str> {
        if self.criteria.sizes.is_empty() {
            return Some("At least one size must be selected.");
        }
        None
    }

    fn run(&mut self, idx: Arc<FileIndex>) {
        let groups = search(
            &idx,
            &self.criteria,
            self.group_by,
            self.group_order,
            self.item_order,
        );
        self.results = Some((idx, groups));
        self.group = 0;
        self.page = 0;
        self.selected = None;
        self.list_order.clear();
        self.thumbs.clear();
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.index.ready_or_progress(ui) else {
            return;
        };
        egui::Panel::left("disc-wizard")
            .resizable(true)
            .default_size(330.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.wizard(ui, &idx));
            });
        egui::Panel::bottom("disc-viewer")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let prefs = self.prefs.clone();
                self.viewer.ui(ui, &prefs);
            });
        egui::CentralPanel::default().show(ui, |ui| self.results_ui(ui));
    }

    fn wizard(&mut self, ui: &mut egui::Ui, idx: &Arc<FileIndex>) {
        ui.heading("1. Choose result type");
        ui.horizontal(|ui| {
            for t in ResultType::ALL {
                if ui
                    .selectable_label(self.criteria.result_type == t, t.name())
                    .clicked()
                    && self.criteria.result_type != t
                {
                    let keep = self.criteria.clone();
                    self.criteria = Criteria {
                        result_type: t,
                        ..keep
                    };
                }
            }
        });
        ui.separator();
        ui.heading(format!(
            "2. Filter which {} to show",
            self.criteria.result_type.name().to_lowercase()
        ));
        let video = self.criteria.result_type == ResultType::Videos;
        ui.strong("File Size:");
        for b in SizeBucket::ALL {
            let mut on = self.criteria.sizes.contains(&b);
            if ui.checkbox(&mut on, b.label(video)).changed() {
                if on {
                    self.criteria.sizes.insert(b);
                } else {
                    self.criteria.sizes.remove(&b);
                }
            }
        }
        ui.add_space(6.0);
        ui.strong("Data Source:");
        ui.weak("none selected = all");
        for (i, name) in idx.data_sources.iter().enumerate() {
            let id = i as u32;
            let mut on = self.criteria.data_sources.contains(&id);
            if ui.checkbox(&mut on, format!("{name} (ID: {i})")).changed() {
                if on {
                    self.criteria.data_sources.insert(id);
                } else {
                    self.criteria.data_sources.remove(&id);
                }
            }
        }
        ui.add_space(6.0);
        ui.strong("Parent Folder:");
        let mut remove = None;
        for (i, t) in self.criteria.folders.iter().enumerate() {
            ui.horizontal(|ui| {
                let kind = format!(
                    "{} {}",
                    if t.include { "include" } else { "exclude" },
                    if t.substring {
                        "substring"
                    } else {
                        "full path"
                    }
                );
                ui.label(format!("{}  ({kind})", t.text));
                if ui.small_button("✖").on_hover_text("Delete").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            self.criteria.folders.remove(i);
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.folder_text)
                    .hint_text("/Users/")
                    .desired_width(150.0),
            );
            if ui
                .add_enabled(
                    !self.folder_text.trim().is_empty(),
                    egui::Button::new("Add"),
                )
                .clicked()
            {
                self.criteria.folders.push(FolderTerm {
                    text: self.folder_text.trim().to_owned(),
                    substring: self.folder_substring,
                    include: self.folder_include,
                });
                self.folder_text.clear();
            }
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.folder_substring, false, "Full");
            ui.radio_value(&mut self.folder_substring, true, "Substring");
            ui.separator();
            ui.radio_value(&mut self.folder_include, true, "Include");
            ui.radio_value(&mut self.folder_include, false, "Exclude");
        });
        ui.add_space(6.0);
        ui.checkbox(&mut self.criteria.include_deleted, "Include deleted files");
        ui.weak("Past occurrences, hash set, interesting item and object filters need ingest.");
        ui.separator();
        ui.heading("3. Choose display settings");
        egui::Grid::new("disc-display")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Group By:");
                egui::ComboBox::from_id_salt("disc-group")
                    .selected_text(self.group_by.name())
                    .show_ui(ui, |ui| {
                        for g in GroupBy::ALL {
                            ui.selectable_value(&mut self.group_by, g, g.name());
                        }
                    });
                ui.end_row();
                ui.label("Order Groups By:");
                egui::ComboBox::from_id_salt("disc-gorder")
                    .selected_text(if self.group_order == GroupOrder::GroupSize {
                        "Group Size"
                    } else {
                        "Group Name"
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.group_order,
                            GroupOrder::GroupSize,
                            "Group Size",
                        );
                        ui.selectable_value(
                            &mut self.group_order,
                            GroupOrder::GroupName,
                            "Group Name",
                        );
                    });
                ui.end_row();
                ui.label("Order Within Groups By:");
                egui::ComboBox::from_id_salt("disc-iorder")
                    .selected_text(self.item_order.name())
                    .show_ui(ui, |ui| {
                        for o in ItemOrder::ALL {
                            ui.selectable_value(&mut self.item_order, o, o.name());
                        }
                    });
                ui.end_row();
            });
        let err = self.validation_error();
        if let Some(e) = err {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if ui
            .add_enabled(err.is_none(), egui::Button::new("🔍 Search"))
            .clicked()
        {
            self.run(Arc::clone(idx));
        }
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        let Some((idx, groups)) = self.results.clone() else {
            ui.centered_and_justified(|ui| {
                ui.weak("Choose criteria on the left and press Search.")
            });
            return;
        };
        if groups.is_empty() {
            ui.centered_and_justified(|ui| ui.weak("No files match these criteria."));
            return;
        }
        let total: usize = groups.iter().map(|g| g.files.len()).sum();
        egui::Panel::left("disc-groups")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                ui.strong(format!(
                    "{} results in {} groups",
                    grouped(total as u64),
                    groups.len()
                ));
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for (i, g) in groups.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    self.group == i,
                                    format!("{} ({})", g.label, g.files.len()),
                                )
                                .clicked()
                            {
                                self.group = i;
                                self.page = 0;
                            }
                        }
                    });
            });
        let Some(g) = groups.get(self.group) else {
            return;
        };
        let pages = g.files.len().div_ceil(self.page_size).max(1);
        ui.horizontal(|ui| {
            ui.strong(&g.label);
            ui.separator();
            ui.label(format!("Page {} of {pages}", self.page + 1));
            if ui
                .add_enabled(self.page > 0, egui::Button::new("⏴"))
                .clicked()
            {
                self.page -= 1;
            }
            if ui
                .add_enabled(self.page + 1 < pages, egui::Button::new("⏵"))
                .clicked()
            {
                self.page += 1;
            }
            egui::ComboBox::from_id_salt("disc-pagesize")
                .selected_text(format!("{} per page", self.page_size))
                .show_ui(ui, |ui| {
                    for n in PAGE_SIZES {
                        if ui
                            .selectable_value(&mut self.page_size, n, n.to_string())
                            .clicked()
                        {
                            self.page = 0;
                        }
                    }
                });
        });
        let list = self.criteria.result_type != ResultType::Images;
        let order = self.list_order.get(
            self.group,
            g.files.len(),
            self.list_sort.filter(|_| list),
            |a, b, col| {
                let file = |i: usize| g.files.get(i).and_then(|&fi| idx.files.get(fi));
                let (Some(a), Some(b)) = (file(a), file(b)) else {
                    return std::cmp::Ordering::Equal;
                };
                match col {
                    0 => cmp_text(a.name(), b.name()),
                    1 => a.size.cmp(&b.size),
                    2 => a.times.modified.cmp(&b.times.modified),
                    _ => cmp_text(a.parent(), b.parent()),
                }
            },
        );
        let page: Vec<usize> = order
            .iter()
            .skip(self.page * self.page_size)
            .take(self.page_size)
            .filter_map(|&i| g.files.get(i).copied())
            .collect();
        if list {
            self.list(ui, &idx, &page);
        } else {
            self.thumb_grid(ui, &idx, &page);
        }
    }

    fn select(&mut self, idx: &FileIndex, fi: usize, open: bool) {
        self.selected = Some(fi);
        let c = self_content(&self.sources, idx, fi);
        if open {
            self.popout = c;
        } else {
            self.viewer.set_content(c);
        }
    }

    fn thumb_grid(&mut self, ui: &mut egui::Ui, idx: &Arc<FileIndex>, page: &[usize]) {
        let size = 150.0;
        let cell = Vec2::new(size + 12.0, size + 34.0);
        let per_row = ((ui.available_width() / cell.x).floor() as usize).max(1);
        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            cell.y,
            page.len().div_ceil(per_row),
            |ui, range| {
                for r in range {
                    ui.horizontal(|ui| {
                        for &fi in page.iter().skip(r * per_row).take(per_row) {
                            let (rect, resp) = ui.allocate_exact_size(cell, Sense::click());
                            let selected = self.selected == Some(fi);
                            let bg = if selected {
                                ui.visuals().selection.bg_fill
                            } else {
                                ui.visuals().extreme_bg_color
                            };
                            ui.painter().rect_filled(rect.shrink(2.0), 6.0, bg);
                            let img = egui::Rect::from_min_size(
                                rect.min + Vec2::new(6.0, 6.0),
                                Vec2::splat(size),
                            );
                            let sources = self.sources.clone();
                            self.thumbs
                                .paint(ui, img, &fi, || self_content(&sources, idx, fi));
                            if let Some(f) = idx.files.get(fi) {
                                let col = if f.deleted() {
                                    ui.visuals().error_fg_color
                                } else {
                                    ui.visuals().text_color()
                                };
                                ui.painter().text(
                                    egui::pos2(rect.center().x, rect.bottom() - 16.0),
                                    egui::Align2::CENTER_CENTER,
                                    f.name(),
                                    egui::FontId::proportional(12.0),
                                    col,
                                );
                                resp.clone().on_hover_text(format!(
                                    "{}\n{}",
                                    f.path,
                                    pm_core::units::format_size(f.size)
                                ));
                            }
                            if resp.double_clicked() {
                                action = Some((fi, true));
                            } else if resp.clicked() {
                                action = Some((fi, false));
                            }
                        }
                    });
                }
            },
        );
        if let Some((fi, open)) = action {
            self.select(idx, fi, open);
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, idx: &Arc<FileIndex>, page: &[usize]) {
        use egui_extras::{Column, TableBuilder};
        let mut action = None;
        let mut sort = self.list_sort;
        ui.style_mut().interaction.selectable_labels = false;
        crate::sortable::hscroll(ui, "disc-list-h", |ui| {
            let row_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
            TableBuilder::new(ui)
                .id_salt("disc-list")
                .striped(true)
                .resizable(true)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(260.0).at_least(80.0).clip(true))
                .column(Column::initial(90.0))
                .column(Column::initial(170.0))
                .column(Column::remainder().at_least(240.0).clip(true))
                .header(row_h + 2.0, |mut header| {
                    for (i, t) in ["Name", "Size", "Modified Time", "Path"]
                        .into_iter()
                        .enumerate()
                    {
                        header.col(|ui| {
                            crate::sortable::header_mut(ui, t, &mut sort, i);
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_h, page.len(), |mut row| {
                        let Some(&fi) = page.get(row.index()) else {
                            return;
                        };
                        let Some(f) = idx.files.get(fi) else { return };
                        row.set_selected(self.selected == Some(fi));
                        row.col(|ui| {
                            let text = egui::RichText::new(f.name());
                            ui.label(if f.deleted() {
                                text.color(ui.visuals().error_fg_color)
                            } else {
                                text
                            });
                        });
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(pm_core::units::format_size(f.size));
                                },
                            );
                        });
                        row.col(|ui| {
                            ui.label(self.prefs.time(f.times.modified));
                        });
                        row.col(|ui| {
                            ui.weak(f.parent());
                        });
                        let r = row.response();
                        if r.double_clicked() {
                            action = Some((fi, true));
                        } else if r.clicked() {
                            action = Some((fi, false));
                        }
                    });
                });
        });
        if sort != self.list_sort {
            self.list_sort = sort;
            self.page = 0;
        }
        if let Some((fi, open)) = action {
            self.select(idx, fi, open);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn discovery_search_shows_groups() {
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
        let mut win = DiscoveryWindow::new(index, sources, DisplayPrefs::default());
        // Demo pictures are small: widen the size filter like a user would.
        win.criteria.sizes = SizeBucket::ALL.into_iter().collect();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1300.0, 850.0))
            .build_ui_state(|ui, w: &mut DiscoveryWindow| w.ui(ui), win);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while h.query_by_label_contains("🔍 Search").is_none() {
            assert!(std::time::Instant::now() < deadline, "index did not build");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.get_by_label("🔍 Search").click();
        h.run_steps(3);
        h.get_by_label_contains("results in");
        assert!(h.query_all_by_label_contains("Pictures").count() >= 1);
        let start = std::time::Instant::now();
        while !h.state().thumbs.any_ready() && start.elapsed().as_secs() < 20 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.run_steps(3);
        if let Ok(d) = std::env::var("PM_UI_SCREENSHOT_DIR") {
            h.render()
                .unwrap()
                .save(std::path::PathBuf::from(d).join("discovery.png"))
                .unwrap();
        }
    }
}
