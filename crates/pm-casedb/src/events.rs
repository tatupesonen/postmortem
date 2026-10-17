//! Timeline events (TSK `TimelineManager`, `tsk_events`).
//!
//! Artifact events (web history, messages, registry, …) are stored in
//! `events` with a shared `event_descriptions` row per (description,
//! content, artifact), as TSK does. File-system MACB events are *not*
//! stored (ADR-017): they are read from the indexed time columns of
//! `files`, which avoids duplicating every path four times on cases with
//! hundreds of millions of files.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{CaseDb, Result};

/// TSK event type ids used by postmortem's writers.
pub mod types {
    pub const FILE_MODIFIED: i64 = 4;
    pub const FILE_ACCESSED: i64 = 5;
    pub const FILE_CREATED: i64 = 6;
    pub const FILE_CHANGED: i64 = 7;
    pub const WEB_DOWNLOADS: i64 = 8;
    pub const WEB_COOKIE: i64 = 9;
    pub const WEB_BOOKMARK: i64 = 10;
    pub const WEB_HISTORY: i64 = 11;
    pub const WEB_SEARCH: i64 = 12;
    pub const MESSAGE: i64 = 13;
    pub const GPS_ROUTE: i64 = 14;
    pub const GPS_TRACKPOINT: i64 = 15;
    pub const CALL_LOG: i64 = 16;
    pub const EMAIL: i64 = 17;
    pub const RECENT_DOCUMENTS: i64 = 18;
    pub const INSTALLED_PROGRAM: i64 = 19;
    pub const EXIF: i64 = 20;
    pub const DEVICES_ATTACHED: i64 = 21;
    pub const STANDARD_ARTIFACT_CATCH_ALL: i64 = 23;
    pub const LOG_ENTRY: i64 = 24;
    pub const REGISTRY: i64 = 25;
    pub const CUSTOM_ARTIFACT_CATCH_ALL: i64 = 26;
    pub const WEB_FORM_AUTOFILL: i64 = 27;
    pub const WEB_FORM_ADDRESSES: i64 = 28;
    pub const GPS_BOOKMARK: i64 = 29;
    pub const GPS_LAST_KNOWN_LOCATION: i64 = 30;
    pub const GPS_SEARCH: i64 = 31;
    pub const GPS_TRACK: i64 = 32;
    pub const METADATA_LAST_PRINTED: i64 = 33;
    pub const METADATA_LAST_SAVED: i64 = 34;
    pub const METADATA_CREATED: i64 = 35;
    pub const PROGRAM_EXECUTION: i64 = 36;
    pub const WEB_FORM_AUTOFILL_ACCESSED: i64 = 37;
    pub const CALL_LOG_END: i64 = 38;
    pub const EMAIL_RCVD: i64 = 39;
    pub const WEB_FORM_ADDRESSES_MODIFIED: i64 = 40;
    pub const WEB_COOKIE_ACCESSED: i64 = 41;
    pub const WEB_COOKIE_END: i64 = 42;
    pub const BACKUP_EVENT_START: i64 = 43;
    pub const BACKUP_EVENT_END: i64 = 44;
    pub const BLUETOOTH_PAIRING: i64 = 45;
    pub const CALENDAR_ENTRY_START: i64 = 46;
    pub const CALENDAR_ENTRY_END: i64 = 47;
    pub const PROGRAM_DELETED: i64 = 48;
    pub const OS_INFO: i64 = 49;
    pub const PROGRAM_NOTIFICATION: i64 = 50;
    pub const SCREEN_SHOT: i64 = 51;
    pub const SERVICE_ACCOUNT: i64 = 52;
    pub const USER_DEVICE_EVENT_START: i64 = 53;
    pub const USER_DEVICE_EVENT_END: i64 = 54;
    pub const WEB_CACHE: i64 = 55;
    pub const WIFI_NETWORK: i64 = 56;
    pub const WEB_HISTORY_CREATED: i64 = 57;
    pub const BLUETOOTH_ADAPTER: i64 = 58;
    pub const BLUETOOTH_PAIRING_ACCESSED: i64 = 59;
    pub const USER_CREATED: i64 = 60;
}

/// An event type row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventType {
    pub id: i64,
    pub name: String,
    pub super_type_id: Option<i64>,
}

/// An event to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub type_id: i64,
    /// Unix seconds, UTC.
    pub time: i64,
    pub full_description: String,
    pub med_description: Option<String>,
    pub short_description: Option<String>,
    pub ds_id: i64,
    /// The file the event is about (for artifacts: the artifact's source
    /// file).
    pub file_id: Option<i64>,
    pub artifact_id: Option<i64>,
}

/// A stored event with its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub id: i64,
    pub type_id: i64,
    pub time: i64,
    pub full_description: String,
    pub med_description: Option<String>,
    pub short_description: Option<String>,
    pub ds_id: i64,
    pub file_id: Option<i64>,
    pub artifact_id: Option<i64>,
    pub hash_hit: bool,
    pub tagged: bool,
}

/// Inserts events (idempotent: the same type, description and time is
/// stored once).
pub(crate) fn insert_events(c: &Connection, events: &[NewEvent]) -> Result<usize> {
    // Two lookups so each uses its index (`artifact_id IS ?` cannot use
    // the partial artifact index).
    let mut find_by_artifact = c.prepare_cached(
        "SELECT id FROM event_descriptions WHERE artifact_id = ?3 AND full_description = ?1 AND file_id IS ?2",
    )?;
    let mut find_by_file = c.prepare_cached(
        "SELECT id FROM event_descriptions WHERE file_id IS ?2 AND artifact_id IS NULL AND full_description = ?1",
    )?;
    let mut add_desc = c.prepare_cached(
        "INSERT INTO event_descriptions (full_description, med_description, short_description, ds_id, file_id, artifact_id, hash_hit, tagged) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, \
           COALESCE((SELECT 1 FROM artifacts WHERE file_id = ?5 AND type_id = 10 LIMIT 1), 0), \
           COALESCE((SELECT 1 FROM file_tags WHERE file_id = ?5 LIMIT 1), (SELECT 1 FROM artifact_tags WHERE artifact_id = ?6 LIMIT 1), 0))",
    )?;
    let mut add_event = c.prepare_cached(
        "INSERT OR IGNORE INTO events (type_id, description_id, time) VALUES (?1, ?2, ?3)",
    )?;
    let mut added = 0;
    for e in events {
        let find = if e.artifact_id.is_some() {
            &mut find_by_artifact
        } else {
            &mut find_by_file
        };
        let existing: Option<i64> = find
            .query_row(params![e.full_description, e.file_id, e.artifact_id], |r| {
                r.get(0)
            })
            .optional()?;
        let desc = if let Some(id) = existing {
            id
        } else {
            add_desc.execute(params![
                e.full_description,
                e.med_description,
                e.short_description,
                e.ds_id,
                e.file_id,
                e.artifact_id
            ])?;
            c.last_insert_rowid()
        };
        added += add_event.execute(params![e.type_id, desc, e.time])?;
    }
    Ok(added)
}

const EVENT_COLS: &str = "e.id, e.type_id, e.time, d.full_description, d.med_description, d.short_description, d.ds_id, d.file_id, d.artifact_id, d.hash_hit, d.tagged";

fn event_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<EventRecord> {
    Ok(EventRecord {
        id: r.get(0)?,
        type_id: r.get(1)?,
        time: r.get(2)?,
        full_description: r.get(3)?,
        med_description: r.get(4)?,
        short_description: r.get(5)?,
        ds_id: r.get(6)?,
        file_id: r.get(7)?,
        artifact_id: r.get(8)?,
        hash_hit: r.get(9)?,
        tagged: r.get(10)?,
    })
}

impl CaseDb {
    /// Records events; returns how many were new.
    pub fn add_events(&self, events: &[NewEvent]) -> Result<usize> {
        self.write(|tx| insert_events(tx, events))
    }

    /// The event type hierarchy (TSK ids), by id.
    pub fn event_types(&self) -> Result<Vec<EventType>> {
        self.read(|c| {
            let mut st =
                c.prepare("SELECT id, name, super_type_id FROM event_types ORDER BY id")?;
            let rows = st
                .query_map([], |r| {
                    Ok(EventType {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        super_type_id: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Stored events with `start <= time < end`, keyset paged by
    /// (time, id).
    pub fn events_between(
        &self,
        start: i64,
        end: i64,
        after: (i64, i64),
        limit: usize,
    ) -> Result<Vec<EventRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {EVENT_COLS} FROM events e JOIN event_descriptions d ON d.id = e.description_id \
                 WHERE e.time >= ?1 AND e.time < ?2 AND (e.time, e.id) > (?3, ?4) ORDER BY e.time, e.id LIMIT ?5"
            ))?;
            let rows = st
                .query_map(params![start, end, after.0, after.1, limit as i64], event_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// `(type id, count)` of stored events with `start <= time < end`.
    pub fn event_counts(&self, start: i64, end: i64) -> Result<Vec<(i64, u64)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT type_id, COUNT(*) FROM events WHERE time >= ?1 AND time < ?2 GROUP BY type_id ORDER BY type_id",
            )?;
            let rows = st
                .query_map(params![start, end], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Events of an artifact (for "View in Timeline").
    pub fn events_of_artifact(&self, artifact_id: i64) -> Result<Vec<EventRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {EVENT_COLS} FROM events e JOIN event_descriptions d ON d.id = e.description_id \
                 WHERE d.artifact_id = ?1 ORDER BY e.time, e.id"
            ))?;
            let rows = st
                .query_map([artifact_id], event_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Refreshes the "tagged" flag of a file's (or artifact's) event
    /// descriptions after tags change.
    pub fn refresh_event_tag_flags(
        &self,
        file_id: Option<i64>,
        artifact_id: Option<i64>,
    ) -> Result<()> {
        self.write(|tx| {
            if let Some(f) = file_id {
                tx.execute(
                    "UPDATE event_descriptions SET tagged = EXISTS (SELECT 1 FROM file_tags WHERE file_id = ?1) WHERE file_id = ?1 AND artifact_id IS NULL",
                    [f],
                )?;
            }
            if let Some(a) = artifact_id {
                tx.execute(
                    "UPDATE event_descriptions SET tagged = EXISTS (SELECT 1 FROM artifact_tags WHERE artifact_id = ?1) WHERE artifact_id = ?1",
                    [a],
                )?;
            }
            Ok(())
        })
    }
}
