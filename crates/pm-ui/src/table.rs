//! Result table: a virtualised, sortable listing of files.

use std::cmp::Ordering;

use egui_extras::{Column, TableBuilder};

use crate::format::DisplayPrefs;
use crate::icons;
use crate::model::{CommentStatus, FileRow};
use crate::sortable::{Sort, cmp_text};
use crate::tags_ui::{TagCatalog, TagRequest};

/// Sortable columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Col {
    Name,
    /// Autopsy "S": aggregate score.
    Score,
    /// Autopsy "C": tags / comments.
    Comment,
    Modified,
    Changed,
    Accessed,
    Created,
    Size,
    FlagsDir,
    FlagsMeta,
    Extension,
    Inode,
}

impl Col {
    pub const ALL: [Self; 12] = [
        Self::Name,
        Self::Score,
        Self::Comment,
        Self::Modified,
        Self::Changed,
        Self::Accessed,
        Self::Created,
        Self::Size,
        Self::FlagsDir,
        Self::FlagsMeta,
        Self::Extension,
        Self::Inode,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Score => "S",
            Self::Comment => "C",
            Self::Modified => "Modified Time",
            Self::Changed => "Change Time",
            Self::Accessed => "Access Time",
            Self::Created => "Created Time",
            Self::Size => "Size",
            Self::FlagsDir => "Flags(Dir)",
            Self::FlagsMeta => "Flags(Meta)",
            Self::Extension => "Extension",
            Self::Inode => "Meta Addr",
        }
    }
}

fn compare(a: &FileRow, b: &FileRow, col: Col) -> Ordering {
    let t = |r: &FileRow, f: fn(&pm_fs::Times) -> Option<jiff::Timestamp>| {
        r.meta.as_ref().and_then(|m| f(&m.times))
    };
    match col {
        Col::Name => cmp_text(&a.name, &b.name),
        Col::Score => score(a).cmp(&score(b)),
        Col::Comment => comment(a).cmp(&comment(b)),
        Col::Modified => t(a, |t| t.modified).cmp(&t(b, |t| t.modified)),
        Col::Changed => t(a, |t| t.changed).cmp(&t(b, |t| t.changed)),
        Col::Accessed => t(a, |t| t.accessed).cmp(&t(b, |t| t.accessed)),
        Col::Created => t(a, |t| t.created).cmp(&t(b, |t| t.created)),
        Col::Size => a.size.cmp(&b.size),
        Col::FlagsDir => a.name_allocated.cmp(&b.name_allocated),
        Col::FlagsMeta => a.meta_allocated().cmp(&b.meta_allocated()),
        Col::Extension => cmp_text(a.extension(), b.extension()),
        Col::Inode => (a.inode, a.stream).cmp(&(b.inode, b.stream)),
    }
}

fn score(r: &FileRow) -> Option<pm_casedb::Significance> {
    r.annotations.as_ref().map(|a| a.score)
}

fn comment(r: &FileRow) -> Option<CommentStatus> {
    r.annotations.as_ref().map(|a| a.comment_status())
}

/// The S cell: Autopsy shows an icon for notable and likely-notable
/// scores, with "Has an {significance} analysis result score".
fn score_cell(ui: &mut egui::Ui, r: &FileRow) {
    let Some(s) = score(r) else { return };
    let icon = match s {
        pm_casedb::Significance::Notable => icons::SCORE_NOTABLE,
        pm_casedb::Significance::LikelyNotable => icons::SCORE_SUSPICIOUS,
        _ => return,
    };
    ui.label(icon)
        .on_hover_text(format!("Has an {} analysis result score", s.name()));
}

/// The C cell: tags without / with a comment.
fn comment_cell(ui: &mut egui::Ui, r: &FileRow) {
    match comment(r) {
        Some(CommentStatus::TagComment) => {
            ui.label(icons::COMMENT)
                .on_hover_text("Has a tag with a comment");
        }
        Some(CommentStatus::TagNoComment) => {
            ui.label(icons::TAGGED)
                .on_hover_text("Has tags without comments");
        }
        _ => {}
    }
}

/// Sort order for `rows`; directories first, then by `col`.
pub(crate) fn sorted_indices(rows: &[FileRow], col: Col, ascending: bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&i, &j| {
        let (Some(a), Some(b)) = (rows.get(i), rows.get(j)) else {
            return Ordering::Equal;
        };
        b.is_dir().cmp(&a.is_dir()).then_with(|| {
            let o = compare(a, b, col);
            if ascending { o } else { o.reverse() }
        })
    });
    idx
}

/// Table interaction results for the caller. Row actions apply to every
/// selected row (`Vec`s, in display order); viewing actions to the row
/// clicked.
#[derive(Debug, Default)]
pub(crate) struct TableEvents {
    pub(crate) selected: Option<usize>,
    pub(crate) activated: Option<usize>,
    pub(crate) popout: Option<usize>,
    /// Extract File(s).
    pub(crate) export: Vec<usize>,
    pub(crate) timeline: Option<usize>,
    /// Open in External Viewer.
    pub(crate) external: Option<usize>,
    /// Export Table to CSV.
    pub(crate) csv: bool,
    /// Export only the selected rows to CSV.
    pub(crate) csv_selection: bool,
    /// Add File(s) to Hash Set: the rows and the set's path.
    pub(crate) add_hash: Option<(Vec<usize>, std::path::PathBuf)>,
    /// A tag menu choice for the selected rows.
    pub(crate) tag: Option<(Vec<usize>, TagRequest)>,
    /// The selected rows this frame (for shortcuts such as Ctrl+B).
    pub(crate) marked: Vec<usize>,
    /// Pivot on a value of the clicked row (name, MD5).
    pub(crate) pivot: Option<String>,
}

/// A row's identity in a listing: the case file id, then inode and stream
/// (carved, derived and logical files have no inode: keying by inode alone
/// made every such row one selection).
type RowKey = (Option<i64>, u64, Option<u32>);

fn key_of(r: &FileRow) -> RowKey {
    (r.file_id(), r.inode, r.stream)
}

/// Row keys in display order.
fn keys_of(rows: &[FileRow], order: &[usize]) -> Vec<RowKey> {
    order
        .iter()
        .filter_map(|&i| rows.get(i).map(key_of))
        .collect()
}

/// What the row context menu offers beyond the built-in actions.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RowMenus<'a> {
    /// Enables Add/Remove File Tag.
    pub(crate) tags: Option<&'a TagCatalog>,
    /// The configured hash sets; updateable (`.kdb`) ones are offered by
    /// Add File to Hash Set.
    pub(crate) hash_sets: &'a [pm_modules::hash_lookup::HashSetConfig],
}

/// Persistent table state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TableState {
    pub sort: Col,
    pub ascending: bool,
    /// The selected rows and the focused one (shown in the viewer).
    #[serde(skip)]
    sel: crate::selection::Selection<RowKey>,
    /// A file to focus and scroll to once its listing is shown.
    #[serde(skip)]
    reveal: Option<i64>,
    #[serde(skip)]
    /// Display order: (rows, sort column, ascending, filter) it was
    /// computed for, and the row indices.
    order: Option<(usize, Col, bool, String, Vec<usize>)>,
    /// Ctrl+F: only rows whose name contains this (case-insensitive).
    #[serde(skip)]
    filter: String,
    /// The filter row is shown; `.1`: focus it this frame.
    #[serde(skip)]
    filtering: (bool, bool),
}

impl Default for TableState {
    fn default() -> Self {
        Self {
            sort: Col::Name,
            ascending: true,
            sel: crate::selection::Selection::default(),
            reveal: None,
            order: None,
            filter: String::new(),
            filtering: (false, false),
        }
    }
}

impl TableState {
    /// Clears the selection and cached order when the listing changes.
    pub fn reset(&mut self) {
        self.sel.clear();
        self.order = None;
        self.filter.clear();
        self.filtering = (false, false);
    }

    /// Focuses file `file_id` (and scrolls to it) when its row appears.
    pub(crate) fn reveal(&mut self, file_id: i64) {
        self.reveal = Some(file_id);
    }

    /// Ctrl+F opens a name filter above the table; Escape or × closes it.
    fn filter_row(&mut self, ui: &mut egui::Ui, total: usize) {
        if !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F))
        {
            self.filtering = (true, true);
        }
        if !self.filtering.0 {
            return;
        }
        ui.horizontal(|ui| {
            let label = ui.label("Filter:");
            let edit = ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Part of a name")
                        .desired_width(240.0),
                )
                .labelled_by(label.id);
            if std::mem::take(&mut self.filtering.1) {
                edit.request_focus();
            }
            if let Some(o) = &self.order
                && !self.filter.is_empty()
            {
                ui.weak(format!("{} of {total}", o.4.len()));
            }
            let esc = edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape));
            if ui
                .small_button("×")
                .on_hover_text("Close the filter")
                .clicked()
                || esc
            {
                self.filter.clear();
                self.filtering = (false, false);
            }
        });
    }

    /// Whether `r` is selected.
    fn is_marked(&self, r: &FileRow) -> bool {
        self.sel.contains(key_of(r))
    }

    /// Draws the table.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[FileRow],
        prefs: &DisplayPrefs,
        menus: RowMenus<'_>,
    ) -> TableEvents {
        self.ui_with(ui, rows, prefs, menus, None)
    }

    /// [`Self::ui`] with an extra text column after Name (e.g. Autopsy's
    /// "Keyword Preview"); `extra.1[i]` belongs to `rows[i]`.
    pub(crate) fn ui_with(
        &mut self,
        ui: &mut egui::Ui,
        rows: &[FileRow],
        prefs: &DisplayPrefs,
        menus: RowMenus<'_>,
        extra: Option<(&str, &[String])>,
    ) -> TableEvents {
        let tags = menus.tags;
        let mut ev = TableEvents::default();
        let key = rows.as_ptr() as usize;
        self.filter_row(ui, rows.len());
        let stale = !matches!(&self.order, Some((k, c, a, f, _)) if *k == key && *c == self.sort && *a == self.ascending && *f == self.filter);
        if stale {
            let mut order = sorted_indices(rows, self.sort, self.ascending);
            if !self.filter.is_empty() {
                let needle = self.filter.to_lowercase();
                order.retain(|&i| {
                    rows.get(i)
                        .is_some_and(|r| r.name.to_lowercase().contains(&needle))
                });
            }
            self.order = Some((key, self.sort, self.ascending, self.filter.clone(), order));
        }
        let order = self.order.as_ref().map(|o| o.4.clone()).unwrap_or_default();

        // Keyboard navigation: arrows move the selection, Enter opens.
        let sel_pos = self.sel.focus.and_then(|s| {
            order
                .iter()
                .position(|&i| rows.get(i).is_some_and(|r| key_of(r) == s))
        });
        // J/K step like the arrows (without modifiers).
        let (down, up, enter, shift) = ui.input(|i| {
            let plain = |k| i.modifiers.is_none() && i.key_pressed(k);
            (
                i.key_pressed(egui::Key::ArrowDown) || plain(egui::Key::J),
                i.key_pressed(egui::Key::ArrowUp) || plain(egui::Key::K),
                i.key_pressed(egui::Key::Enter),
                i.modifiers.shift,
            )
        });
        let mut scroll_to = None;
        if let Some(id) = self.reveal
            && let Some((p, i, r)) = order.iter().enumerate().find_map(|(p, &i)| {
                rows.get(i)
                    .filter(|r| r.file_id() == Some(id))
                    .map(|r| (p, i, r))
            })
        {
            self.reveal = None;
            self.sel.select_one(key_of(r));
            ev.selected = Some(i);
            scroll_to = Some(p);
        }
        if !ui.ctx().egui_wants_keyboard_input()
            && !order.is_empty()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
        {
            self.sel.select_all(&keys_of(rows, &order));
        }
        if !ui.ctx().egui_wants_keyboard_input() && !order.is_empty() {
            let next = match (down, up, sel_pos) {
                (true, _, Some(p)) => Some((p + 1).min(order.len() - 1)),
                (true, _, None) => Some(0),
                (_, true, Some(p)) => Some(p.saturating_sub(1)),
                _ => None,
            };
            let target =
                next.and_then(|p| order.get(p).and_then(|&i| rows.get(i).map(|r| (p, i, r))));
            if let Some((p, i, r)) = target {
                if shift {
                    self.sel.select_range(&keys_of(rows, &order), p, false);
                } else {
                    self.sel.select_one(key_of(r));
                }
                ev.selected = Some(i);
                scroll_to = Some(p);
            } else if enter {
                ev.activated = sel_pos.and_then(|p| order.get(p).copied());
            }
        }

        let text_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
        // Wide tables scroll horizontally instead of clipping columns.
        crate::sortable::hscroll(ui, "file-table-h", |ui| {
            // Cell text must not swallow clicks meant for the row.
            ui.style_mut().interaction.selectable_labels = false;
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(250.0).at_least(80.0).clip(true));
            if extra.is_some() {
                table = table.column(Column::initial(300.0).at_least(80.0).clip(true));
            }
            table = table
                .columns(Column::initial(26.0).at_least(22.0), 2)
                .columns(Column::initial(150.0).clip(true), 4)
                .column(Column::initial(100.0))
                .columns(Column::initial(95.0), 2)
                .column(Column::initial(70.0))
                .column(Column::remainder().at_least(70.0));
            if let Some(p) = scroll_to {
                table = table.scroll_to_row(p, None);
            }
            table
                .header(text_h + 2.0, |mut header| {
                    // Header index of `Col::ALL[i]` (the extra column is
                    // second and not sortable).
                    let at = |i: usize| if extra.is_some() && i > 0 { i + 1 } else { i };
                    let sort = Col::ALL
                        .iter()
                        .position(|c| *c == self.sort)
                        .map(|col| Sort {
                            col: at(col),
                            ascending: self.ascending,
                        });
                    for (i, col) in Col::ALL.into_iter().enumerate() {
                        if i == 1
                            && let Some((title, _)) = extra
                        {
                            header.col(|ui| {
                                ui.strong(title);
                            });
                        }
                        let i = at(i);
                        header.col(|ui| {
                            if crate::sortable::header(ui, col.title(), sort, i) {
                                if self.sort == col {
                                    self.ascending = !self.ascending;
                                } else {
                                    self.sort = col;
                                    self.ascending = true;
                                }
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(text_h, order.len(), |mut row| {
                        let Some((i, r)) = order
                            .get(row.index())
                            .and_then(|&i| rows.get(i).map(|r| (i, r)))
                        else {
                            return;
                        };
                        row.set_selected(self.is_marked(r));
                        let deleted = r.deleted();
                        let color = |ui: &egui::Ui| {
                            if deleted {
                                ui.visuals().error_fg_color
                            } else {
                                ui.visuals().text_color()
                            }
                        };
                        row.col(|ui| {
                            let icon = if r.is_dir() {
                                "🗀"
                            } else if r.has_children {
                                icons::ARCHIVE
                            } else if r.stream.is_some() {
                                "⧉"
                            } else {
                                "🗋"
                            };
                            let mark = if deleted { " ✖" } else { "" };
                            ui.label(
                                egui::RichText::new(format!("{icon} {}{mark}", r.name))
                                    .color(color(ui)),
                            );
                        });
                        if let Some((_, cells)) = extra {
                            row.col(|ui| {
                                ui.label(cells.get(i).map_or("", String::as_str));
                            });
                        }
                        row.col(|ui| score_cell(ui, r));
                        row.col(|ui| comment_cell(ui, r));
                        let times = r.meta.as_ref().map(|m| m.times).unwrap_or_default();
                        for t in [times.modified, times.changed, times.accessed, times.created] {
                            row.col(|ui| {
                                ui.label(prefs.time(t));
                            });
                        }
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(crate::format::grouped(r.size));
                                },
                            );
                        });
                        row.col(|ui| {
                            ui.label(if r.name_allocated {
                                "Allocated"
                            } else {
                                "Unallocated"
                            });
                        });
                        row.col(|ui| {
                            ui.label(if r.meta_allocated() {
                                "Allocated"
                            } else {
                                "Unallocated"
                            });
                        });
                        row.col(|ui| {
                            ui.label(r.extension());
                        });
                        row.col(|ui| {
                            ui.label(match r.stream {
                                Some(s) => format!("{}:{s}", r.inode),
                                None => r.inode.to_string(),
                            });
                        });
                        let resp = row.response();
                        if resp.clicked() {
                            let m = resp.ctx.input(|i| i.modifiers);
                            self.sel.click(&keys_of(rows, &order), row.index(), m);
                            ev.selected = Some(i);
                        }
                        if resp.double_clicked() {
                            ev.activated = Some(i);
                        }
                        resp.context_menu(|ui| {
                            // Right-clicking outside the selection selects
                            // that row; inside it, actions take them all.
                            if !self.is_marked(r) {
                                self.sel.select_one(key_of(r));
                            }
                            self.sel.focus = Some(key_of(r));
                            ev.selected = Some(i);
                            let marked = self.marked_rows_in(rows, &order);
                            let n = marked.len();
                            let files = |one: &str, many: &str| {
                                if n > 1 {
                                    format!("{many} ({n} files)")
                                } else {
                                    one.to_owned()
                                }
                            };
                            if ui.button("Open in New Window").clicked() {
                                ev.popout = Some(i);
                                ui.close();
                            }
                            let any_file = marked
                                .iter()
                                .any(|&m| rows.get(m).is_some_and(|r| !r.is_dir()));
                            if ui
                                .add_enabled(
                                    any_file,
                                    egui::Button::new(files("Extract File…", "Extract Files…")),
                                )
                                .clicked()
                            {
                                ev.export.clone_from(&marked);
                                ui.close();
                            }
                            let has_time = r
                                .meta
                                .as_ref()
                                .is_some_and(|m| m.times.modified.or(m.times.created).is_some());
                            if ui
                                .add_enabled(
                                    has_time && !r.is_dir(),
                                    egui::Button::new("View File in Timeline…"),
                                )
                                .clicked()
                            {
                                ev.timeline = Some(i);
                                ui.close();
                            }
                            if ui
                                .add_enabled(
                                    !r.is_dir(),
                                    egui::Button::new("Open in External Viewer"),
                                )
                                .clicked()
                            {
                                ev.external = Some(i);
                                ui.close();
                            }
                            if ui.button("Copy Path").clicked() {
                                ui.ctx().copy_text(r.path.clone());
                                ui.close();
                            }
                            ui.menu_button("Pivot on", |ui| {
                                if ui.button(format!("Name: {}", r.name)).clicked() {
                                    ev.pivot = Some(r.name.clone());
                                    ui.close();
                                }
                                if let Some(md5) = r
                                    .file
                                    .as_ref()
                                    .and_then(|f| f.md5.as_ref())
                                    .map(|h| pm_core::decode::to_hex(h))
                                    && ui.button(format!("MD5: {md5}")).clicked()
                                {
                                    ev.pivot = Some(md5);
                                    ui.close();
                                }
                            });
                            if ui.button("Export Table to CSV…").clicked() {
                                ev.csv = true;
                                ui.close();
                            }
                            if n > 1
                                && ui
                                    .button(format!("Export Selected Rows to CSV… ({n} rows)"))
                                    .clicked()
                            {
                                ev.csv_selection = true;
                                ui.close();
                            }
                            if any_file
                                && r.file_id().is_some()
                                && let Some(path) = hash_set_menu(
                                    ui,
                                    &files("Add File to Hash Set", "Add Files to Hash Set"),
                                    menus.hash_sets,
                                )
                            {
                                ev.add_hash = Some((marked.clone(), path));
                            }
                            if let Some(cat) = tags.filter(|_| r.file_id().is_some()) {
                                ui.separator();
                                if let Some(t) = crate::tags_ui::add_menu(
                                    ui,
                                    &files("Add File Tag", "Add File Tag"),
                                    cat,
                                ) {
                                    ev.tag = Some((marked.clone(), t));
                                }
                                let on_row = r
                                    .annotations
                                    .as_ref()
                                    .map(|a| a.tags.as_slice())
                                    .unwrap_or_default();
                                if let Some(t) = crate::tags_ui::remove_menu(
                                    ui,
                                    &files("Remove File Tag", "Remove File Tag"),
                                    on_row,
                                ) {
                                    ev.tag = Some((marked.clone(), t));
                                }
                            }
                        });
                    });
                });
        });
        ev.marked = self.marked_rows_in(rows, &order);
        ev
    }

    /// [`Self::marked_rows`] for a known display order.
    fn marked_rows_in(&self, rows: &[FileRow], order: &[usize]) -> Vec<usize> {
        order
            .iter()
            .copied()
            .filter(|&i| rows.get(i).is_some_and(|r| self.is_marked(r)))
            .collect()
    }
}

/// Autopsy's "Add File to Hash Set" submenu: the updateable sets.
fn hash_set_menu(
    ui: &mut egui::Ui,
    title: &str,
    sets: &[pm_modules::hash_lookup::HashSetConfig],
) -> Option<std::path::PathBuf> {
    let mut chosen = None;
    ui.menu_button(title, |ui| {
        let mut any = false;
        for set in sets.iter().filter(|s| s.updateable()) {
            any = true;
            if ui.button(set.display_name()).clicked() {
                chosen = Some(set.path.clone());
                ui.close();
            }
        }
        if !any {
            ui.weak("No updateable hash sets.\nCreate one in Tools › Options › Hash Sets.");
        }
    });
    chosen
}

impl TableState {
    /// The listing as CSV in the shown order (Autopsy's "Save Table as
    /// CSV"): every column, `extra` after Name.
    pub(crate) fn csv(
        &self,
        rows: &[FileRow],
        prefs: &DisplayPrefs,
        extra: Option<(&str, &[String])>,
        selected_only: bool,
    ) -> String {
        let order = match &self.order {
            Some((k, ..)) if *k == rows.as_ptr() as usize => {
                self.order.as_ref().map(|o| o.4.clone())
            }
            _ => None,
        }
        .unwrap_or_else(|| sorted_indices(rows, self.sort, self.ascending));
        let mut out = String::new();
        let mut line = |cells: &mut dyn Iterator<Item = String>| {
            let quoted: Vec<String> = cells.map(|c| csv_field(&c)).collect();
            out.push_str(&quoted.join(","));
            out.push_str("\r\n");
        };
        let mut head = vec!["Name".to_owned()];
        head.extend(extra.map(|(t, _)| t.to_owned()));
        head.extend(Col::ALL.iter().skip(1).map(|c| c.title().to_owned()));
        line(&mut head.into_iter());
        for i in order {
            let Some(r) = rows.get(i) else { continue };
            if selected_only && !self.is_marked(r) {
                continue;
            }
            let times = r.meta.as_ref().map(|m| m.times).unwrap_or_default();
            let alloc = |a: bool| if a { "Allocated" } else { "Unallocated" }.to_owned();
            let mut cells = vec![r.name.clone()];
            if let Some((_, e)) = extra {
                cells.push(e.get(i).cloned().unwrap_or_default());
            }
            cells.push(score(r).map(|s| s.name().to_owned()).unwrap_or_default());
            cells.push(match comment(r) {
                Some(CommentStatus::TagComment) => "Tag with comment".into(),
                Some(CommentStatus::TagNoComment) => "Tag".into(),
                _ => String::new(),
            });
            for t in [times.modified, times.changed, times.accessed, times.created] {
                cells.push(prefs.time(t));
            }
            cells.push(r.size.to_string());
            cells.push(alloc(r.name_allocated));
            cells.push(alloc(r.meta_allocated()));
            cells.push(r.extension().to_owned());
            cells.push(match r.stream {
                Some(s) => format!("{}:{s}", r.inode),
                None => r.inode.to_string(),
            });
            line(&mut cells.into_iter());
        }
        out
    }
}

/// A CSV field, quoted as RFC 4180 requires.
pub(crate) fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_without_inodes_are_told_apart() {
        // Carved and logical files have no inode; their file ids differ.
        let rec = |id: i64, name: &str| pm_casedb::FileRecord {
            id,
            ds_id: 1,
            fs_id: None,
            parent_id: None,
            origin: pm_casedb::Origin::Carved,
            inode: None,
            stream: None,
            name: name.into(),
            parent_path: "/$CarvedFiles/".into(),
            kind: pm_casedb::FileKindCode::Regular,
            name_alloc: true,
            meta_alloc: true,
            meta_flags: 0,
            size: 10,
            times: [None; 4],
            mode: None,
            uid: None,
            gid: None,
            seq: None,
            attributes: None,
            extension: "jpg".into(),
            mime: None,
            md5: None,
            sha1: None,
            sha256: None,
            known: pm_casedb::Known::Unknown,
            derived_from: None,
            local_path: None,
        };
        let a = crate::views::row_of(&rec(1, "a.jpg"));
        let b = crate::views::row_of(&rec(2, "b.jpg"));
        assert_eq!(a.inode, b.inode, "both without an inode");
        assert_ne!(key_of(&a), key_of(&b));
        let mut t = TableState::default();
        t.sel.select_one(key_of(&a));
        assert!(t.is_marked(&a));
        assert!(!t.is_marked(&b), "clicking one row selects only it");
    }

    use pm_fs::{FileKind, Metadata};

    fn row(name: &str, dir: bool, size: u64) -> FileRow {
        FileRow {
            name: name.into(),
            inode: size,
            stream: None,
            kind: if dir {
                FileKind::Directory
            } else {
                FileKind::Regular
            },
            name_allocated: true,
            meta: Some(Metadata::new(
                size,
                if dir {
                    FileKind::Directory
                } else {
                    FileKind::Regular
                },
                size,
            )),
            size,
            path: format!("/{name}"),
            file: None,
            annotations: None,
            has_children: false,
        }
    }

    #[test]
    fn directories_first_then_column() {
        let rows = vec![
            row("b.txt", false, 5),
            row("Zdir", true, 0),
            row("a.txt", false, 9),
            row("adir", true, 1),
        ];
        let names = |idx: Vec<usize>| {
            idx.into_iter()
                .map(|i| rows[i].name.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(sorted_indices(&rows, Col::Name, true)),
            ["adir", "Zdir", "a.txt", "b.txt"]
        );
        assert_eq!(
            names(sorted_indices(&rows, Col::Size, false)),
            ["adir", "Zdir", "a.txt", "b.txt"]
        );
        assert_eq!(
            names(sorted_indices(&rows, Col::Size, true)),
            ["Zdir", "adir", "b.txt", "a.txt"]
        );
    }

    #[test]
    fn csv_follows_sort_and_quotes() {
        let rows = vec![row("b \"x\".txt", false, 5), row("a,b.txt", false, 9)];
        let t = TableState {
            sort: Col::Size,
            ascending: false,
            ..TableState::default()
        };
        let csv = t.csv(&rows, &DisplayPrefs::default(), None, false);
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("\"Name\",\"S\",\"C\",\"Modified Time\""));
        assert!(lines[1].starts_with("\"a,b.txt\""));
        assert!(lines[2].starts_with("\"b \"\"x\"\".txt\""));
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn extension_column() {
        assert_eq!(row("x.tar.gz", false, 0).extension(), "gz");
        assert_eq!(row(".bashrc", false, 0).extension(), "");
        assert_eq!(row("noext", false, 0).extension(), "");
    }
}
