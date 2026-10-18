//! Multi-row selection shared by the tables: click selects one row,
//! Ctrl+click toggles, Shift+click (and Shift+arrows) selects the range
//! from the anchor, Ctrl+Shift+click adds that range, Ctrl+A selects all.
//! Rows are identified by a key that survives re-sorting and reloads.

use std::collections::BTreeSet;

/// The selected rows of a table and its focused row (the one the viewer
/// shows and keyboard navigation moves from).
#[derive(Debug, Clone)]
pub(crate) struct Selection<K> {
    pub(crate) focus: Option<K>,
    marked: BTreeSet<K>,
    /// Where a Shift selection starts.
    anchor: Option<K>,
}

impl<K> Default for Selection<K> {
    fn default() -> Self {
        Self {
            focus: None,
            marked: BTreeSet::new(),
            anchor: None,
        }
    }
}

impl<K: Ord + Copy> Selection<K> {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Selects only `key`.
    pub(crate) fn select_one(&mut self, key: K) {
        self.focus = Some(key);
        self.marked = BTreeSet::from([key]);
        self.anchor = Some(key);
    }

    /// Ctrl+click: adds or removes `key`, which gets the focus and becomes
    /// the Shift anchor.
    pub(crate) fn toggle(&mut self, key: K) {
        if self.marked.is_empty()
            && let Some(f) = self.focus
        {
            self.marked.insert(f);
        }
        if !self.marked.remove(&key) {
            self.marked.insert(key);
        }
        self.focus = Some(key);
        self.anchor = Some(key);
    }

    /// Shift+click: the rows from the anchor to display position `to` of
    /// `keys` (the rows in display order), added to the selection when
    /// `add`.
    pub(crate) fn select_range(&mut self, keys: &[K], to: usize, add: bool) {
        let from = self
            .anchor
            .and_then(|a| keys.iter().position(|k| *k == a))
            .unwrap_or(to);
        let (lo, hi) = (from.min(to), from.max(to));
        if !add {
            self.marked.clear();
        }
        self.marked
            .extend(keys.get(lo..=hi).unwrap_or_default().iter().copied());
        self.focus = keys.get(to).copied();
        if self.anchor.is_none() {
            self.anchor = self.focus;
        }
    }

    /// Ctrl+A: every row; the focus stays (or goes to the first row).
    pub(crate) fn select_all(&mut self, keys: &[K]) {
        self.marked = keys.iter().copied().collect();
        if self.focus.is_none() {
            self.focus = keys.first().copied();
        }
    }

    /// A row click with `modifiers`: Shift extends (Ctrl+Shift adds),
    /// Ctrl toggles, otherwise the row alone. `pos` is the row's display
    /// position in `keys`.
    pub(crate) fn click(&mut self, keys: &[K], pos: usize, modifiers: egui::Modifiers) {
        let Some(&key) = keys.get(pos) else { return };
        if modifiers.shift {
            self.select_range(keys, pos, modifiers.command);
        } else if modifiers.command {
            self.toggle(key);
        } else {
            self.select_one(key);
        }
    }

    /// Whether `key` is selected (a focused row alone counts).
    pub(crate) fn contains(&self, key: K) -> bool {
        self.marked.contains(&key) || (self.marked.is_empty() && self.focus == Some(key))
    }

    /// Positions in `keys` (display order) of the selected rows.
    pub(crate) fn positions(&self, keys: &[K]) -> Vec<usize> {
        keys.iter()
            .enumerate()
            .filter(|(_, k)| self.contains(**k))
            .map(|(p, _)| p)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: egui::Modifiers = egui::Modifiers::NONE;

    #[test]
    fn click_toggle_and_range() {
        let keys = [10, 11, 12, 13, 14];
        let mut s = Selection::default();
        s.click(&keys, 1, NONE);
        assert_eq!(s.positions(&keys), [1]);
        // Ctrl+click adds; the clicked row becomes the Shift anchor.
        s.click(&keys, 3, egui::Modifiers::COMMAND);
        assert_eq!(s.positions(&keys), [1, 3]);
        // Shift+click: anchor (3) to 0.
        s.click(&keys, 0, egui::Modifiers::SHIFT);
        assert_eq!(s.positions(&keys), [0, 1, 2, 3]);
        assert_eq!(s.focus, Some(10));
        // Ctrl+click again removes.
        s.click(&keys, 1, egui::Modifiers::COMMAND);
        assert_eq!(s.positions(&keys), [0, 2, 3]);
        // Ctrl+Shift adds a range: anchor (1) to 4.
        s.click(&keys, 4, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
        assert_eq!(s.positions(&keys), [0, 1, 2, 3, 4]);
        s.click(&keys, 2, NONE);
        assert_eq!(s.positions(&keys), [2]);
        s.select_all(&keys);
        assert_eq!(s.positions(&keys).len(), 5);
        s.clear();
        assert!(s.positions(&keys).is_empty());
        // A focused row alone counts as selected.
        s.focus = Some(12);
        assert_eq!(s.positions(&keys), [2]);
    }
}
