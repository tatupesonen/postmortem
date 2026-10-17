//! Timeline: event types, filters, calendar binning and the case-database
//! timeline ([`db::DbTimeline`]: counts and lists are indexed queries, so
//! nothing is held in memory however large the case).

pub mod db;
pub mod division;
mod types;

use std::collections::HashSet;

use jiff::tz::TimeZone;

pub use division::{TimeUnit, bins};
pub use types::{Category, EventType, hsb};

pub use pm_core::category::FileCategory;

/// Which deleted state to show (a postmortem addition to Autopsy's filters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeletedFilter {
    #[default]
    All,
    OnlyDeleted,
    OnlyAllocated,
}

/// Autopsy's root filter: the AND of all active sub-filters. Empty sets
/// mean "no restriction" (Autopsy's default "unselected" state).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub text: String,
    pub data_sources: HashSet<u32>,
    pub file_categories: HashSet<FileCategory>,
    pub event_types: HashSet<EventType>,
    pub deleted: DeletedFilter,
}

impl Filter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Stacking granularity of the counts view (Autopsy's type zoom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeZoom {
    #[default]
    Category,
    Event,
}

/// Parameters of [`db::DbTimeline::counts`].
#[derive(Debug, Clone, Copy)]
pub struct CountsQuery<'a> {
    pub start: i64,
    pub end: i64,
    pub unit: TimeUnit,
    pub tz: &'a TimeZone,
    pub filter: &'a Filter,
    pub zoom: TypeZoom,
    pub max_bins: usize,
}

/// One bar of the counts view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bin {
    pub start: i64,
    pub end: i64,
    /// Count per series (categories or event types, per [`TypeZoom`]).
    pub counts: Vec<u32>,
}

impl Bin {
    pub fn total(&self) -> u64 {
        self.counts.iter().map(|&c| u64::from(c)).sum()
    }
}

/// A row of the list view: events of one subject in one second, combined
/// into MACB letters when they are file-system events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub time: i64,
    pub subject: u32,
    pub kinds: Vec<EventType>,
}

impl ListRow {
    /// `MA.C`-style string for file-system rows (`_` for absent types).
    pub fn macb(&self) -> Option<String> {
        if !self
            .kinds
            .iter()
            .all(|k| k.category() == Category::FileSystem)
        {
            return None;
        }
        Some(
            EventType::of(Category::FileSystem)
                .map(|t| {
                    if self.kinds.contains(&t) {
                        t.macb_letter().unwrap_or('_')
                    } else {
                        '_'
                    }
                })
                .collect(),
        )
    }
}

/// Series labels for a type zoom, in series index order.
pub fn series(zoom: TypeZoom) -> Vec<(String, [u8; 3])> {
    match zoom {
        TypeZoom::Category => Category::ALL
            .iter()
            .map(|c| (c.name().to_owned(), c.color()))
            .collect(),
        TypeZoom::Event => EventType::ALL
            .iter()
            .map(|t| (t.name().to_owned(), t.color()))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macb_strings() {
        let row = |kinds: Vec<EventType>| ListRow {
            time: 0,
            subject: 0,
            kinds,
        };
        assert_eq!(
            row(vec![EventType::FileModified, EventType::FileCreated])
                .macb()
                .as_deref(),
            Some("M_B_")
        );
        assert_eq!(
            row(vec![EventType::FileAccessed]).macb().as_deref(),
            Some("_A__")
        );
        assert_eq!(row(vec![EventType::WebHistory]).macb(), None);
    }

    #[test]
    fn series_follow_the_zoom() {
        assert_eq!(series(TypeZoom::Category).len(), Category::ALL.len());
        assert_eq!(series(TypeZoom::Event).len(), EventType::COUNT);
    }
}
