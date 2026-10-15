//! Result table: a virtualised, sortable listing of files.

use std::cmp::Ordering;

use egui_extras::{Column, TableBuilder};

use crate::format::DisplayPrefs;
use crate::model::FileRow;
use crate::sortable::{Sort, cmp_text};

/// Sortable columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Col {
    Name,
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
    pub const ALL: [Self; 10] = [
        Self::Name,
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

/// Table interaction results for the caller.
#[derive(Debug, Default)]
pub struct TableEvents {
    pub selected: Option<usize>,
    pub activated: Option<usize>,
    pub popout: Option<usize>,
    pub export: Option<usize>,
    pub timeline: Option<usize>,
}

/// Persistent table state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TableState {
    pub sort: Col,
    pub ascending: bool,
    #[serde(skip)]
    pub selected: Option<(u64, Option<u32>)>,
    #[serde(skip)]
    order: Option<(usize, Col, bool, Vec<usize>)>,
}

impl Default for TableState {
    fn default() -> Self {
        Self {
            sort: Col::Name,
            ascending: true,
            selected: None,
            order: None,
        }
    }
}

impl TableState {
    /// Clears the selection and cached order when the listing changes.
    pub fn reset(&mut self) {
        self.selected = None;
        self.order = None;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, rows: &[FileRow], prefs: &DisplayPrefs) -> TableEvents {
        let mut ev = TableEvents::default();
        let key = rows.as_ptr() as usize;
        let stale = !matches!(&self.order, Some((k, c, a, _)) if *k == key && *c == self.sort && *a == self.ascending);
        if stale {
            self.order = Some((
                key,
                self.sort,
                self.ascending,
                sorted_indices(rows, self.sort, self.ascending),
            ));
        }
        let order = self.order.as_ref().map(|o| o.3.clone()).unwrap_or_default();

        // Keyboard navigation: arrows move the selection, Enter opens.
        let sel_pos = self.selected.and_then(|s| {
            order
                .iter()
                .position(|&i| rows.get(i).is_some_and(|r| (r.inode, r.stream) == s))
        });
        let (down, up, enter) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let mut scroll_to = None;
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
                self.selected = Some((r.inode, r.stream));
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
                .column(Column::initial(250.0).at_least(80.0).clip(true))
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
                    let sort = Col::ALL
                        .iter()
                        .position(|c| *c == self.sort)
                        .map(|col| Sort {
                            col,
                            ascending: self.ascending,
                        });
                    for (i, col) in Col::ALL.into_iter().enumerate() {
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
                        let is_sel = self.selected == Some((r.inode, r.stream));
                        row.set_selected(is_sel);
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
                            self.selected = Some((r.inode, r.stream));
                            ev.selected = Some(i);
                        }
                        if resp.double_clicked() {
                            ev.activated = Some(i);
                        }
                        resp.context_menu(|ui| {
                            self.selected = Some((r.inode, r.stream));
                            ev.selected = Some(i);
                            if ui.button("Open in New Window").clicked() {
                                ev.popout = Some(i);
                                ui.close();
                            }
                            if ui
                                .add_enabled(!r.is_dir(), egui::Button::new("Extract File…"))
                                .clicked()
                            {
                                ev.export = Some(i);
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
                            if ui.button("Copy Path").clicked() {
                                ui.ctx().copy_text(r.path.clone());
                                ui.close();
                            }
                        });
                    });
                });
        });
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn extension_column() {
        assert_eq!(row("x.tar.gz", false, 0).extension(), "gz");
        assert_eq!(row(".bashrc", false, 0).extension(), "");
        assert_eq!(row("noext", false, 0).extension(), "");
    }
}
