//! A sortable, virtualised table of text cells, for the simpler result
//! views (tags, scores, OS accounts) that do not need typed columns.

use egui_extras::{Column, TableBuilder};

use crate::sortable::{Order, Sort, cmp_text, header_mut};

/// Rows of text cells; `keys` optionally gives a numeric sort key per cell
/// (times, sizes) so they do not sort as text.
#[derive(Debug, Clone, Default)]
pub(crate) struct GridData {
    pub(crate) cols: Vec<&'static str>,
    pub(crate) rows: Vec<Vec<String>>,
    /// `keys[row][col]`, when present, sorts that cell numerically.
    pub(crate) keys: Vec<Vec<Option<i64>>>,
}

impl GridData {
    pub(crate) fn new(cols: Vec<&'static str>) -> Self {
        Self {
            cols,
            ..Self::default()
        }
    }

    /// Adds a row of cells, each optionally with a numeric sort key.
    pub(crate) fn push(&mut self, cells: Vec<(String, Option<i64>)>) {
        let (text, keys) = cells.into_iter().unzip();
        self.rows.push(text);
        self.keys.push(keys);
    }

    fn cmp(&self, a: usize, b: usize, col: usize) -> std::cmp::Ordering {
        let key = |r: usize| self.keys.get(r).and_then(|k| k.get(col).copied().flatten());
        if let (Some(x), Some(y)) = (key(a), key(b)) {
            return x.cmp(&y);
        }
        let text = |r: usize| {
            self.rows
                .get(r)
                .and_then(|c| c.get(col))
                .map_or("", String::as_str)
        };
        cmp_text(text(a), text(b))
    }
}

/// What happened in the grid this frame (data row indices).
#[derive(Debug, Default)]
pub(crate) struct GridEvents {
    pub(crate) selected: Option<usize>,
    pub(crate) activated: Option<usize>,
}

/// Sort, order cache and selection of one grid.
#[derive(Debug, Default)]
pub(crate) struct Grid {
    sort: Option<Sort>,
    order: Order,
    pub(crate) selected: Option<usize>,
}

impl Grid {
    /// Forgets the selection (the data changed).
    pub(crate) fn reset(&mut self) {
        self.selected = None;
        self.order.clear();
    }

    /// Draws `data`; `menu(ui, row)` fills a row's context menu.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        data: &GridData,
        mut menu: impl FnMut(&mut egui::Ui, usize),
    ) -> GridEvents {
        let mut ev = GridEvents::default();
        let key = std::ptr::from_ref(data) as usize;
        let order = self
            .order
            .get(key, data.rows.len(), self.sort, |a, b, c| data.cmp(a, b, c));
        let text_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
        crate::sortable::hscroll(ui, (id, "h"), |ui| {
            ui.push_id(id, |ui| {
                ui.style_mut().interaction.selectable_labels = false;
                let mut table = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for (i, _) in data.cols.iter().enumerate() {
                    table = if i + 1 == data.cols.len() {
                        table.column(Column::remainder().at_least(80.0))
                    } else {
                        table.column(Column::initial(160.0).at_least(40.0).clip(true))
                    };
                }
                table
                    .header(text_h + 2.0, |mut h| {
                        for (i, title) in data.cols.iter().enumerate() {
                            h.col(|ui| header_mut(ui, title, &mut self.sort, i));
                        }
                    })
                    .body(|body| {
                        body.rows(text_h, order.len(), |mut row| {
                            let Some(&i) = order.get(row.index()) else {
                                return;
                            };
                            let Some(cells) = data.rows.get(i) else {
                                return;
                            };
                            row.set_selected(self.selected == Some(i));
                            for c in cells {
                                row.col(|ui| {
                                    ui.label(c);
                                });
                            }
                            let resp = row.response();
                            if resp.clicked() {
                                self.selected = Some(i);
                                ev.selected = Some(i);
                            }
                            if resp.double_clicked() {
                                ev.activated = Some(i);
                            }
                            resp.context_menu(|ui| {
                                self.selected = Some(i);
                                ev.selected = Some(i);
                                menu(ui, i);
                            });
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

    #[test]
    fn numeric_keys_sort_numerically_and_text_case_insensitively() {
        let mut d = GridData::new(vec!["Name", "Size"]);
        d.push(vec![("b".into(), None), ("10".into(), Some(10))]);
        d.push(vec![("A".into(), None), ("9".into(), Some(9))]);
        assert_eq!(d.cmp(0, 1, 1), std::cmp::Ordering::Greater, "10 > 9");
        assert_eq!(d.cmp(0, 1, 0), std::cmp::Ordering::Greater, "b > A");
    }
}
