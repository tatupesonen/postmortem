//! Timeline: events, filters, calendar binning and aggregation.
//!
//! The [`Timeline`] keeps events sorted by time in a compact columnar form
//! so range queries are binary searches and aggregations are single passes,
//! fast enough to recompute interactively for millions of events. It is
//! storage-agnostic: today it is built from the in-memory file index
//! ([`Timeline::from_index`]), later from the case database.

pub mod db;
pub mod division;
mod types;

use std::collections::HashSet;
use std::sync::Arc;

use jiff::tz::TimeZone;

pub use division::{TimeUnit, bins};
pub use types::{Category, EventType, hsb};

pub use pm_index::FileCategory;

/// The thing an event is about (a file today; artifacts later).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    /// Data source index.
    pub ds: u32,
    /// Filesystem index within the data source.
    pub fs: u32,
    pub inode: u64,
    /// Full path, used as the event description.
    pub path: Arc<str>,
    pub deleted: bool,
    pub size: u64,
    pub category: FileCategory,
}

/// One timeline event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// Unix seconds.
    pub time: i64,
    pub kind: EventType,
    /// Index into [`Timeline::subjects`].
    pub subject: u32,
}

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

    fn matches(&self, e: &Event, s: &Subject, needle: &str) -> bool {
        (self.event_types.is_empty() || self.event_types.contains(&e.kind))
            && (self.data_sources.is_empty() || self.data_sources.contains(&s.ds))
            && (self.file_categories.is_empty() || self.file_categories.contains(&s.category))
            && match self.deleted {
                DeletedFilter::All => true,
                DeletedFilter::OnlyDeleted => s.deleted,
                DeletedFilter::OnlyAllocated => !s.deleted,
            }
            && (needle.is_empty() || s.path.to_lowercase().contains(needle))
    }
}

/// Stacking granularity of the counts view (Autopsy's type zoom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeZoom {
    #[default]
    Category,
    Event,
}

/// Parameters of [`Timeline::counts`].
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

/// An immutable, time-sorted event store.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    events: Vec<Event>,
    subjects: Vec<Subject>,
    data_sources: Vec<String>,
}

impl Timeline {
    /// File-system events (M, A, B, C) for every indexed file, as Autopsy
    /// creates at data-source add time. Unset (zero) times produce no event.
    pub fn from_index(index: &pm_index::FileIndex) -> Self {
        let mut subjects = Vec::with_capacity(index.files.len());
        let mut events = Vec::with_capacity(index.files.len() * 4);
        for f in &index.files {
            let idx = subjects.len() as u32;
            let t = f.times;
            for (time, kind) in [
                (t.modified, EventType::FileModified),
                (t.accessed, EventType::FileAccessed),
                (t.created, EventType::FileCreated),
                (t.changed, EventType::FileChanged),
            ] {
                if let Some(s) = time.map(jiff::Timestamp::as_second).filter(|&s| s != 0) {
                    events.push(Event {
                        time: s,
                        kind,
                        subject: idx,
                    });
                }
            }
            subjects.push(Subject {
                ds: f.ds,
                fs: f.fs,
                inode: f.inode,
                path: Arc::clone(&f.path),
                deleted: f.deleted(),
                size: f.size,
                category: f.category,
            });
        }
        Self::new(events, subjects, index.data_sources.clone())
    }

    /// Builds from unsorted parts (events are sorted here).
    pub fn new(mut events: Vec<Event>, subjects: Vec<Subject>, data_sources: Vec<String>) -> Self {
        events.retain(|e| (e.subject as usize) < subjects.len());
        events.sort_by_key(|e| (e.time, e.subject, e.kind));
        Self {
            events,
            subjects,
            data_sources,
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Adds events about further subjects (e.g. artifacts from the case
    /// database); `events[].subject` index into `subjects`.
    #[must_use]
    pub fn with(self, events: Vec<Event>, subjects: Vec<Subject>) -> Self {
        let base = self.subjects.len() as u32;
        let mut all_subjects = self.subjects;
        all_subjects.extend(subjects);
        let mut all_events = self.events;
        all_events.extend(events.into_iter().map(|e| Event {
            subject: e.subject + base,
            ..e
        }));
        Self::new(all_events, all_subjects, self.data_sources)
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn subjects(&self) -> &[Subject] {
        &self.subjects
    }

    pub fn subject(&self, i: u32) -> Option<&Subject> {
        self.subjects.get(i as usize)
    }

    pub fn data_sources(&self) -> &[String] {
        &self.data_sources
    }

    /// `[first, last + 1)` over all events.
    pub fn span(&self) -> Option<(i64, i64)> {
        Some((
            self.events.first()?.time,
            self.events.last()?.time.saturating_add(1),
        ))
    }

    /// Events with `start <= time < end`.
    pub fn range(&self, start: i64, end: i64) -> &[Event] {
        let lo = self.events.partition_point(|e| e.time < start);
        let hi = self.events.partition_point(|e| e.time < end);
        self.events.get(lo..hi.max(lo)).unwrap_or_default()
    }

    /// Matching events in `[start, end)`, in time order.
    pub fn query<'a>(
        &'a self,
        start: i64,
        end: i64,
        filter: &'a Filter,
    ) -> impl Iterator<Item = &'a Event> + 'a {
        let needle = filter.text.to_lowercase();
        self.range(start, end).iter().filter(move |e| {
            self.subjects
                .get(e.subject as usize)
                .is_some_and(|s| filter.matches(e, s, &needle))
        })
    }

    pub fn count(&self, start: i64, end: i64, filter: &Filter) -> usize {
        self.query(start, end, filter).count()
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

    /// Counts per bin and series (Autopsy's counts view).
    pub fn counts(&self, q: &CountsQuery<'_>) -> Vec<Bin> {
        let (filter, zoom) = (q.filter, q.zoom);
        let bounds = bins(q.start, q.end, q.unit, q.tz, q.max_bins);
        let width = match zoom {
            TypeZoom::Category => Category::ALL.len(),
            TypeZoom::Event => EventType::COUNT,
        };
        let mut out: Vec<Bin> = bounds
            .iter()
            .map(|&(s, e)| Bin {
                start: s,
                end: e,
                counts: vec![0; width],
            })
            .collect();
        let (Some(first), Some(last)) = (bounds.first(), bounds.last()) else {
            return out;
        };
        let mut bi = 0usize;
        for ev in self.query(first.0, last.1, filter) {
            while bounds.get(bi).is_some_and(|b| ev.time >= b.1) {
                bi += 1;
            }
            let series = match zoom {
                TypeZoom::Category => ev.kind.category() as usize,
                TypeZoom::Event => ev.kind.index(),
            };
            if let Some(c) = out.get_mut(bi).and_then(|b| b.counts.get_mut(series)) {
                *c += 1;
            }
        }
        out
    }

    /// List-view rows: events grouped by (second, subject).
    pub fn list(&self, start: i64, end: i64, filter: &Filter) -> Vec<ListRow> {
        let mut rows: Vec<ListRow> = Vec::new();
        for e in self.query(start, end, filter) {
            match rows.last_mut() {
                Some(r) if r.time == e.time && r.subject == e.subject => r.kinds.push(e.kind),
                _ => rows.push(ListRow {
                    time: e.time,
                    subject: e.subject,
                    kinds: vec![e.kind],
                }),
            }
        }
        // Events of one subject at one second may be interleaved with other
        // subjects' events; merge stragglers.
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn subject(i: u32, path: &str, deleted: bool) -> Subject {
        Subject {
            ds: i % 2,
            fs: 0,
            inode: u64::from(i),
            path: Arc::from(path),
            deleted,
            size: 1,
            category: FileCategory::from_name(path),
        }
    }

    fn sample() -> Timeline {
        let subjects = vec![
            subject(0, "/a/report.docx", false),
            subject(1, "/b/photo.jpg", true),
            subject(2, "/c/tool.exe", false),
        ];
        let events = vec![
            Event {
                time: 100,
                kind: EventType::FileModified,
                subject: 0,
            },
            Event {
                time: 100,
                kind: EventType::FileCreated,
                subject: 0,
            },
            Event {
                time: 50,
                kind: EventType::FileAccessed,
                subject: 1,
            },
            Event {
                time: 3700,
                kind: EventType::FileChanged,
                subject: 2,
            },
            Event {
                time: 90_000,
                kind: EventType::WebHistory,
                subject: 2,
            },
        ];
        Timeline::new(events, subjects, vec!["ds0".into(), "ds1".into()])
    }

    #[test]
    fn sorted_and_ranged() {
        let t = sample();
        assert_eq!(t.span(), Some((50, 90_001)));
        assert_eq!(t.range(0, 101).len(), 3);
        assert_eq!(t.range(101, 3700).len(), 0);
        assert_eq!(t.range(3700, 3701).len(), 1);
        assert_eq!(t.range(10, 5).len(), 0);
    }

    #[test]
    fn filters_and() {
        let t = sample();
        let all = Filter::default();
        assert_eq!(t.count(i64::MIN, i64::MAX, &all), 5);
        let deleted = Filter {
            deleted: DeletedFilter::OnlyDeleted,
            ..Filter::default()
        };
        assert_eq!(t.count(i64::MIN, i64::MAX, &deleted), 1);
        let text = Filter {
            text: "TOOL".into(),
            ..Filter::default()
        };
        assert_eq!(t.count(i64::MIN, i64::MAX, &text), 2);
        let types = Filter {
            event_types: [EventType::FileModified, EventType::WebHistory].into(),
            ..Filter::default()
        };
        assert_eq!(t.count(i64::MIN, i64::MAX, &types), 2);
        let media = Filter {
            file_categories: [FileCategory::Images].into(),
            ..Filter::default()
        };
        assert_eq!(t.count(i64::MIN, i64::MAX, &media), 1);
        let ds1 = Filter {
            data_sources: [1].into(),
            text: "photo".into(),
            ..Filter::default()
        };
        assert_eq!(t.count(i64::MIN, i64::MAX, &ds1), 1);
    }

    #[test]
    fn counts_by_category_and_type() {
        let t = sample();
        let tz = TimeZone::UTC;
        let f = Filter::default();
        let q = CountsQuery {
            start: 0,
            end: 7200,
            unit: TimeUnit::Hours,
            tz: &tz,
            filter: &f,
            zoom: TypeZoom::Category,
            max_bins: 100,
        };
        let bins = t.counts(&q);
        assert_eq!(bins.len(), 2);
        assert_eq!(bins[0].counts[Category::FileSystem as usize], 3);
        assert_eq!(bins[1].counts[Category::FileSystem as usize], 1);
        let by_type = t.counts(&CountsQuery {
            zoom: TypeZoom::Event,
            ..q
        });
        assert_eq!(by_type[0].counts[EventType::FileCreated.index()], 1);
        assert_eq!(by_type.iter().map(Bin::total).sum::<u64>(), 4);
    }

    #[test]
    fn list_groups_macb() {
        let t = sample();
        let rows = t.list(0, 200, &Filter::default());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].macb().as_deref(), Some("M_B_"));
        assert_eq!(rows[0].macb().as_deref(), Some("_A__"));
        let web = t.list(90_000, 90_001, &Filter::default());
        assert_eq!(web[0].macb(), None);
    }

    proptest! {
        #[test]
        fn counts_sum_matches_query(times in proptest::collection::vec(-1_000_000i64..1_000_000, 0..200), start in -1_200_000i64..0, len in 1i64..2_400_000) {
            let subjects = vec![subject(0, "/f", false)];
            let events = times.iter().map(|&time| Event { time, kind: EventType::FileModified, subject: 0 }).collect();
            let t = Timeline::new(events, subjects, vec![]);
            let end = start + len;
            let unit = TimeUnit::for_range(start, end);
            let f = Filter::default();
            let bins = t.counts(&CountsQuery { start, end, unit, tz: &TimeZone::UTC, filter: &f, zoom: TypeZoom::Category, max_bins: 10_000 });
            let first = bins.first().map_or(start, |b| b.start);
            let last = bins.last().map_or(end, |b| b.end);
            let in_bins: u64 = bins.iter().map(Bin::total).sum();
            prop_assert_eq!(in_bins as usize, t.count(first, last, &Filter::default()));
        }
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use pm_core::io::MemSource;
    use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

    #[test]
    fn macb_events_from_index() {
        let mut b = NtfsBuilder::new(4);
        b.file("a.txt", b"a");
        b.file_with(
            "gone.txt",
            b"g",
            FileOpts {
                deleted: Deleted::Unindexed,
                ..Default::default()
            },
        );
        let fs: Arc<dyn pm_fs::FileSystem> =
            Arc::new(pm_fs_ntfs::NtfsFs::open(Arc::new(MemSource::new(b.build()))).unwrap());
        let idx = pm_index::FileIndex::build(
            &[pm_index::Source {
                ds: 0,
                fs_index: 0,
                fs,
            }],
            vec!["img".into()],
            &pm_index::Progress::default(),
        );
        let t = Timeline::from_index(&idx);
        let a = t
            .subjects()
            .iter()
            .position(|s| &*s.path == "/a.txt")
            .unwrap() as u32;
        let kinds: Vec<_> = t
            .range(i64::MIN, i64::MAX)
            .iter()
            .filter(|e| e.subject == a)
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds.len(), 4);
        assert!(
            t.subjects()
                .iter()
                .find(|s| &*s.path == "/gone.txt")
                .unwrap()
                .deleted
        );
        assert!(
            t.range(i64::MIN, i64::MAX)
                .iter()
                .all(|e| e.time == 1_614_853_230)
        );
    }
}
