//! The timeline over the case database (ADR-017): file-system events are
//! the indexed time columns of `files`, every other event is a row of
//! `events` (written with each artifact). Nothing is loaded up front; each
//! view is a few indexed queries, so the timeline of a case with hundreds
//! of millions of files costs no memory until it is looked at.
//!
//! Calendar bins (months, years in the viewer's time zone) are built from
//! fixed-width SQL histograms fine enough to fall on every bin boundary.

use std::collections::HashMap;
use std::sync::Arc;

use pm_casedb::events::types as tsk;
use pm_casedb::mcp::{EventFilter, EventKeyset, TimeColumn};
use pm_casedb::{CaseDb, FileRecord, Result};

use crate::{Bin, Category, CountsQuery, DeletedFilter, EventType, FileCategory, Filter, ListRow};
use crate::{TimeUnit, TypeZoom, bins};

/// The timeline event type of a TSK event type id.
pub fn event_type_of(tsk_id: i64) -> EventType {
    match tsk_id {
        tsk::FILE_MODIFIED => EventType::FileModified,
        tsk::FILE_ACCESSED => EventType::FileAccessed,
        tsk::FILE_CREATED => EventType::FileCreated,
        tsk::FILE_CHANGED => EventType::FileChanged,
        tsk::WEB_HISTORY | tsk::WEB_HISTORY_CREATED | tsk::WEB_CACHE => EventType::WebHistory,
        tsk::WEB_DOWNLOADS => EventType::WebDownloads,
        tsk::WEB_COOKIE | tsk::WEB_COOKIE_ACCESSED | tsk::WEB_COOKIE_END => EventType::WebCookies,
        tsk::WEB_BOOKMARK => EventType::WebBookmarks,
        tsk::WEB_SEARCH => EventType::WebSearches,
        tsk::WEB_FORM_AUTOFILL
        | tsk::WEB_FORM_AUTOFILL_ACCESSED
        | tsk::WEB_FORM_ADDRESSES
        | tsk::WEB_FORM_ADDRESSES_MODIFIED => EventType::WebFormAutofill,
        tsk::MESSAGE => EventType::Messages,
        tsk::CALL_LOG | tsk::CALL_LOG_END => EventType::Calls,
        tsk::EMAIL | tsk::EMAIL_RCVD => EventType::Email,
        tsk::GPS_ROUTE | tsk::GPS_TRACK => EventType::GpsRoutes,
        tsk::GPS_TRACKPOINT
        | tsk::GPS_BOOKMARK
        | tsk::GPS_LAST_KNOWN_LOCATION
        | tsk::GPS_SEARCH => EventType::GpsTrackpoint,
        tsk::RECENT_DOCUMENTS => EventType::RecentDocuments,
        tsk::INSTALLED_PROGRAM | tsk::PROGRAM_DELETED => EventType::InstalledPrograms,
        tsk::EXIF => EventType::Exif,
        tsk::DEVICES_ATTACHED
        | tsk::BLUETOOTH_PAIRING
        | tsk::BLUETOOTH_PAIRING_ACCESSED
        | tsk::BLUETOOTH_ADAPTER
        | tsk::WIFI_NETWORK => EventType::DevicesAttached,
        tsk::LOG_ENTRY => EventType::LogEntry,
        tsk::REGISTRY => EventType::Registry,
        tsk::PROGRAM_EXECUTION | tsk::PROGRAM_NOTIFICATION => EventType::ProgramExecution,
        // TSK's manual events (TSK_TL_EVENT, "User Created").
        22 => EventType::UserCreated,
        _ => EventType::OtherArtifact,
    }
}

fn type_of_column(c: TimeColumn) -> EventType {
    match c {
        TimeColumn::Modified => EventType::FileModified,
        TimeColumn::Accessed => EventType::FileAccessed,
        TimeColumn::Created => EventType::FileCreated,
        TimeColumn::Changed => EventType::FileChanged,
    }
}

/// What an event is about: a file, or an artifact with its source file.
#[derive(Debug, Clone, PartialEq)]
pub struct DbSubject {
    pub ds_id: i64,
    /// File path, or the event's description.
    pub description: Arc<str>,
    pub deleted: bool,
    /// The file (for artifact events: the artifact's source file).
    pub file: Option<Arc<FileRecord>>,
    pub artifact_id: Option<i64>,
}

/// A page of list rows; [`ListRow::subject`] indexes [`Listing::subjects`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub rows: Vec<ListRow>,
    pub subjects: Vec<DbSubject>,
    /// More events matched than were listed.
    pub truncated: bool,
}

impl Listing {
    pub fn subject(&self, i: u32) -> Option<&DbSubject> {
        self.subjects.get(i as usize)
    }
}

/// Which sources a filter reads.
struct Plan {
    columns: Vec<TimeColumn>,
    /// Stored event types wanted (`None`: all).
    stored: Option<Vec<EventType>>,
    /// One filter per selected data source (one unrestricted when none).
    filters: Vec<EventFilter>,
    /// Stored events are left out (file-only filters are active).
    files_only: bool,
}

/// The case timeline, read on demand.
#[derive(Debug, Clone)]
pub struct DbTimeline {
    db: Arc<CaseDb>,
    data_sources: Vec<(i64, String)>,
    span: Option<(i64, i64)>,
}

impl DbTimeline {
    pub fn open(db: Arc<CaseDb>) -> Result<Self> {
        let data_sources = db
            .data_sources()?
            .into_iter()
            .map(|d| (d.id, d.name))
            .collect();
        let span = db.mcp_time_span()?.map(|(a, b)| (a, b.saturating_add(1)));
        Ok(Self {
            db,
            data_sources,
            span,
        })
    }

    /// `(id, name)` of each data source; [`Filter::data_sources`] holds ids.
    pub fn data_sources(&self) -> &[(i64, String)] {
        &self.data_sources
    }

    /// `[first, last + 1)` over all events.
    pub fn span(&self) -> Option<(i64, i64)> {
        self.span
    }

    pub fn is_empty(&self) -> bool {
        self.span.is_none()
    }

    fn plan(filter: &Filter, start: i64, end: i64) -> Plan {
        let wanted =
            |t: EventType| filter.event_types.is_empty() || filter.event_types.contains(&t);
        let columns = TimeColumn::ALL
            .into_iter()
            .filter(|c| wanted(type_of_column(*c)))
            .collect();
        let stored = (!filter.event_types.is_empty()).then(|| {
            filter
                .event_types
                .iter()
                .copied()
                .filter(|t| t.category() != Category::FileSystem)
                .collect::<Vec<_>>()
        });
        let (extensions, exclude) = category_extensions(filter);
        let deleted = match filter.deleted {
            DeletedFilter::All => None,
            DeletedFilter::OnlyDeleted => Some(true),
            DeletedFilter::OnlyAllocated => Some(false),
        };
        let base = EventFilter {
            start: Some(start),
            end: Some(end),
            text: (!filter.text.is_empty()).then(|| filter.text.clone()),
            deleted,
            extensions,
            exclude_extensions: exclude,
            ..EventFilter::default()
        };
        let mut ds: Vec<i64> = filter.data_sources.iter().map(|&d| i64::from(d)).collect();
        ds.sort_unstable();
        let filters = if ds.is_empty() {
            vec![base]
        } else {
            ds.into_iter()
                .map(|d| EventFilter {
                    ds_id: Some(d),
                    ..base.clone()
                })
                .collect()
        };
        Plan {
            columns,
            stored,
            filters,
            // File categories and deleted state describe files.
            files_only: !filter.file_categories.is_empty() || filter.deleted != DeletedFilter::All,
        }
    }

    /// Histogram rows `(bucket start, event type, count)`.
    fn histogram(
        &self,
        plan: &Plan,
        origin: i64,
        width: i64,
    ) -> Result<Vec<(i64, EventType, u64)>> {
        let mut out = Vec::new();
        for f in &plan.filters {
            for &c in &plan.columns {
                for (b, n) in self.db.mcp_file_event_histogram(c, f, origin, width)? {
                    out.push((origin + b * width, type_of_column(c), n));
                }
            }
            if plan.files_only || plan.stored.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            for (b, ty, n) in self.db.mcp_event_histogram(f, origin, width)? {
                let t = event_type_of(ty);
                if plan.stored.as_ref().is_none_or(|s| s.contains(&t)) {
                    out.push((origin + b * width, t, n));
                }
            }
        }
        Ok(out)
    }

    /// Counts per bin and series (Autopsy's counts view).
    pub fn counts(&self, q: &CountsQuery<'_>) -> Result<Vec<Bin>> {
        let bounds = bins(q.start, q.end, q.unit, q.tz, q.max_bins);
        let width = match q.zoom {
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
            return Ok(out);
        };
        let g = granularity(q.unit, first.0, last.1, q.max_bins);
        let origin = first.0 - first.0.rem_euclid(g);
        let plan = Self::plan(q.filter, first.0, last.1);
        for (t, ty, n) in self.histogram(&plan, origin, g)? {
            let i = bounds.partition_point(|b| b.1 <= t);
            let series = match q.zoom {
                TypeZoom::Category => ty.category() as usize,
                TypeZoom::Event => ty.index(),
            };
            if let Some(c) = out.get_mut(i).and_then(|b| b.counts.get_mut(series)) {
                *c = c.saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
            }
        }
        Ok(out)
    }

    /// Number of matching events in `[start, end)`.
    pub fn count(&self, start: i64, end: i64, filter: &Filter) -> Result<u64> {
        let plan = Self::plan(filter, start, end);
        let width = end.saturating_sub(start).max(1);
        Ok(self
            .histogram(&plan, start, width)?
            .iter()
            .map(|r| r.2)
            .sum())
    }

    /// List rows in `[start, end)` in time order, at most `limit` events;
    /// events of one subject in one second share a row (MACB).
    pub fn list(&self, start: i64, end: i64, filter: &Filter, limit: usize) -> Result<Listing> {
        let plan = Self::plan(filter, start, end);
        let limit = limit.max(1);
        // (time, type, subject key, subject)
        let mut events: Vec<(i64, EventType, SubjectKey, Pending)> = Vec::new();
        let mut truncated = false;
        for f in &plan.filters {
            for &c in &plan.columns {
                let page = self
                    .db
                    .mcp_file_events(c, f, EventKeyset::Start, false, limit)?;
                truncated |= page.len() >= limit;
                for rec in page {
                    let Some(t) = rec.times.get(c.index()).copied().flatten().map(|t| t.0) else {
                        continue;
                    };
                    events.push((
                        t,
                        type_of_column(c),
                        SubjectKey::File(rec.id),
                        Pending::File(Arc::new(rec)),
                    ));
                }
            }
            if plan.files_only || plan.stored.as_ref().is_some_and(Vec::is_empty) {
                continue;
            }
            let ids: Vec<i64> = plan.stored.as_ref().map_or_else(Vec::new, |s| {
                (1..=64)
                    .filter(|&i| s.contains(&event_type_of(i)))
                    .collect()
            });
            let page = self
                .db
                .mcp_events(&ids, f, EventKeyset::Start, false, limit)?;
            truncated |= page.len() >= limit;
            for e in page {
                events.push((
                    e.time,
                    event_type_of(e.type_id),
                    SubjectKey::Description(e.artifact_id, Arc::from(e.full_description.as_str())),
                    Pending::Event {
                        ds_id: e.ds_id,
                        description: Arc::from(e.full_description.as_str()),
                        file_id: e.file_id,
                        artifact_id: e.artifact_id,
                    },
                ));
            }
        }
        events.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.2.cmp(&b.2))
                .then(a.1.cmp(&b.1))
        });
        if events.len() > limit {
            events.truncate(limit);
            truncated = true;
        }
        // Source files of artifact events, in one query.
        let mut wanted: Vec<i64> = events
            .iter()
            .filter_map(|e| match &e.3 {
                Pending::Event { file_id, .. } => *file_id,
                Pending::File(_) => None,
            })
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        let files: HashMap<i64, Arc<FileRecord>> = self
            .db
            .files_by_ids(&wanted)?
            .into_iter()
            .map(|f| (f.id, Arc::new(f)))
            .collect();
        let mut listing = Listing {
            truncated,
            ..Listing::default()
        };
        let mut index: HashMap<SubjectKey, u32> = HashMap::new();
        for (time, kind, key, pending) in events {
            let next = u32::try_from(listing.subjects.len()).unwrap_or(u32::MAX);
            let subject = *index.entry(key).or_insert_with(|| {
                listing.subjects.push(match pending {
                    Pending::File(rec) => DbSubject {
                        ds_id: rec.ds_id,
                        description: Arc::from(rec.path().as_str()),
                        deleted: !rec.name_alloc || !rec.meta_alloc,
                        file: Some(rec),
                        artifact_id: None,
                    },
                    Pending::Event {
                        ds_id,
                        description,
                        file_id,
                        artifact_id,
                    } => {
                        let file = file_id.and_then(|id| files.get(&id).cloned());
                        DbSubject {
                            ds_id,
                            description,
                            deleted: file
                                .as_ref()
                                .is_some_and(|f| !f.name_alloc || !f.meta_alloc),
                            file,
                            artifact_id,
                        }
                    }
                });
                next
            });
            match listing.rows.last_mut() {
                Some(r) if r.time == time && r.subject == subject => {
                    if !r.kinds.contains(&kind) {
                        r.kinds.push(kind);
                    }
                }
                _ => listing.rows.push(ListRow {
                    time,
                    subject,
                    kinds: vec![kind],
                }),
            }
        }
        Ok(listing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SubjectKey {
    File(i64),
    Description(Option<i64>, Arc<str>),
}

enum Pending {
    File(Arc<FileRecord>),
    Event {
        ds_id: i64,
        description: Arc<str>,
        file_id: Option<i64>,
        artifact_id: Option<i64>,
    },
}

/// Extensions for a file-category filter: `(extensions, exclude)`.
fn category_extensions(filter: &Filter) -> (Vec<String>, bool) {
    if filter.file_categories.is_empty() {
        return (Vec::new(), false);
    }
    let named = |cats: &mut dyn Iterator<Item = FileCategory>| -> Vec<String> {
        cats.flat_map(|c| c.extensions().iter().map(|e| (*e).to_owned()))
            .collect()
    };
    if filter.file_categories.contains(&FileCategory::Other) {
        // "Other" plus some named categories = everything except the
        // named categories left out.
        let mut out = FileCategory::ALL
            .into_iter()
            .filter(|c| *c != FileCategory::Other && !filter.file_categories.contains(c));
        (named(&mut out), true)
    } else {
        let mut sel = filter.file_categories.iter().copied();
        (named(&mut sel), false)
    }
}

/// Histogram bucket width (s) that every bin boundary of `unit` falls on
/// (time-zone offsets are multiples of 15 minutes), coarse enough to keep
/// the number of buckets near the number of bins.
fn granularity(unit: TimeUnit, start: i64, end: i64, max_bins: usize) -> i64 {
    let span = end.saturating_sub(start).max(1);
    match unit {
        TimeUnit::Seconds => 1,
        TimeUnit::Minutes => 60,
        TimeUnit::Hours => 900,
        TimeUnit::Days | TimeUnit::Months | TimeUnit::Years => {
            // Fixed-width bins (days of a short range) may use their own
            // width when it is a multiple of 15 minutes.
            let w = span / i64::try_from(max_bins.max(1)).unwrap_or(1);
            if unit == TimeUnit::Days && w > 0 && w % 86_400 == 0 {
                86_400
            } else {
                900
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsk_types_map_to_timeline_types() {
        assert_eq!(event_type_of(tsk::FILE_CHANGED), EventType::FileChanged);
        assert_eq!(
            event_type_of(tsk::WEB_HISTORY_CREATED),
            EventType::WebHistory
        );
        assert_eq!(event_type_of(tsk::LOG_ENTRY), EventType::LogEntry);
        assert_eq!(
            event_type_of(tsk::PROGRAM_EXECUTION),
            EventType::ProgramExecution
        );
        assert_eq!(
            event_type_of(tsk::CUSTOM_ARTIFACT_CATCH_ALL),
            EventType::OtherArtifact
        );
        let file_types: Vec<EventType> = TimeColumn::ALL.into_iter().map(type_of_column).collect();
        assert_eq!(
            file_types.len(),
            EventType::of(Category::FileSystem).count(),
            "one column per file-system type"
        );
    }

    #[test]
    fn other_category_excludes_named_ones() {
        let f = Filter {
            file_categories: [FileCategory::Other, FileCategory::Images].into(),
            ..Filter::default()
        };
        let (ext, exclude) = category_extensions(&f);
        assert!(exclude);
        assert!(ext.contains(&"exe".to_owned()) && !ext.contains(&"jpg".to_owned()));
        let f = Filter {
            file_categories: [FileCategory::Images].into(),
            ..Filter::default()
        };
        let (ext, exclude) = category_extensions(&f);
        assert!(!exclude && ext.contains(&"jpg".to_owned()));
    }
}
