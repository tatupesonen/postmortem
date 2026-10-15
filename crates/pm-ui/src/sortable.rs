//! Shared pieces of the data tables: clickable sort headers, cached row
//! orders and horizontal scrolling for tables wider than their panel.

use std::cmp::Ordering;
use std::sync::Arc;

/// The sorted column and direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sort {
    pub(crate) col: usize,
    pub(crate) ascending: bool,
}

impl Sort {
    /// The sort after clicking `col`'s header: the sorted column flips
    /// direction, another column sorts ascending.
    pub(crate) fn toggled(current: Option<Self>, col: usize) -> Self {
        match current {
            Some(s) if s.col == col => Self {
                col,
                ascending: !s.ascending,
            },
            _ => Self {
                col,
                ascending: true,
            },
        }
    }
}

/// A column header that sorts on click; shows ⏶/⏷ on the sorted column.
/// Returns whether it was clicked.
pub(crate) fn header(ui: &mut egui::Ui, title: &str, sort: Option<Sort>, col: usize) -> bool {
    let dir = sort.filter(|s| s.col == col).map(|s| s.ascending);
    let arrow = match dir {
        Some(true) => " ⏶",
        Some(false) => " ⏷",
        None => "",
    };
    ui.add(egui::Button::selectable(dir.is_some(), format!("{title}{arrow}")).frame(false))
        .on_hover_text("Sort by this column")
        .clicked()
}

/// [`header`] that also applies the click to `sort`.
pub(crate) fn header_mut(ui: &mut egui::Ui, title: &str, sort: &mut Option<Sort>, col: usize) {
    if header(ui, title, *sort, col) {
        *sort = Some(Sort::toggled(*sort, col));
    }
}

/// Wraps a table that may be wider than the panel: it scrolls sideways
/// (touchpad swipe, Shift + wheel or the scroll bar, which is always shown
/// so the hidden columns are discoverable).
pub(crate) fn hscroll<R>(
    ui: &mut egui::Ui,
    id_salt: impl egui::AsIdSalt,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::horizontal()
        .id_salt(id_salt)
        .auto_shrink(false)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, add)
        .inner
}

/// Case-insensitive text order without allocating.
pub(crate) fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}

/// A cached display order: data row indices in sorted order. Recomputed
/// only when the data (identified by `key`, e.g. its address), its length
/// or the sort changes, so large tables are not re-sorted every frame.
#[derive(Debug, Default)]
pub(crate) struct Order {
    key: Option<(usize, usize, Option<Sort>)>,
    order: Arc<[usize]>,
}

impl Order {
    /// The order of `len` rows under `sort`; `cmp(a, b, col)` compares data
    /// rows `a` and `b` by column `col` (ascending). Ties keep data order.
    pub(crate) fn get(
        &mut self,
        key: usize,
        len: usize,
        sort: Option<Sort>,
        cmp: impl Fn(usize, usize, usize) -> Ordering,
    ) -> Arc<[usize]> {
        if self.key != Some((key, len, sort)) {
            let mut idx: Vec<usize> = (0..len).collect();
            if let Some(s) = sort {
                idx.sort_by(|&a, &b| {
                    let o = cmp(a, b, s.col);
                    if s.ascending { o } else { o.reverse() }
                });
            }
            self.order = idx.into();
            self.key = Some((key, len, sort));
        }
        Arc::clone(&self.order)
    }

    /// Forgets the cached order.
    pub(crate) fn clear(&mut self) {
        self.key = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_flips_the_sorted_column_only() {
        let s = Sort::toggled(None, 2);
        assert_eq!(
            s,
            Sort {
                col: 2,
                ascending: true
            }
        );
        assert!(!Sort::toggled(Some(s), 2).ascending);
        assert_eq!(
            Sort::toggled(Some(Sort::toggled(Some(s), 2)), 0),
            Sort {
                col: 0,
                ascending: true
            }
        );
    }

    #[test]
    fn order_is_stable_cached_and_reversible() {
        let data = ["b", "A", "c", "a"];
        let cmp = |a: usize, b: usize, _: usize| cmp_text(data[a], data[b]);
        let mut o = Order::default();
        assert_eq!(&*o.get(1, 4, None, cmp), [0, 1, 2, 3]);
        let asc = Some(Sort {
            col: 0,
            ascending: true,
        });
        // "A" and "a" tie and keep data order.
        assert_eq!(&*o.get(1, 4, asc, cmp), [1, 3, 0, 2]);
        let desc = Some(Sort {
            col: 0,
            ascending: false,
        });
        assert_eq!(&*o.get(1, 4, desc, cmp), [2, 0, 1, 3]);
        // Cached: a comparator that would panic is not called again.
        assert_eq!(&*o.get(1, 4, desc, |_, _, _| unreachable!()), [2, 0, 1, 3]);
    }
}
