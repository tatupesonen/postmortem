//! Timeline events of data artifacts (TSK `TimelineManager.addArtifactEvents`
//! with the `TimelineEventType` definitions): written in the same
//! transaction as the artifact, so the timeline, MCP and reports see web
//! activity, program runs, devices, messages and log entries next to file
//! times.
//!
//! Types TSK maps get their own event types and descriptions; any other
//! data artifact gets one catch-all event per date attribute
//! ("Standard Artifact" / "Custom Artifact" in Autopsy's timeline).
//! Analysis results get no events.

use rusqlite::{Connection, OptionalExtension, params};

use crate::events::{NewEvent, insert_events, types as ev};
use crate::{CaseDb, NewArtifact, Result, Value};

/// How one artifact type becomes events.
struct Mapping {
    artifact: &'static str,
    /// Event type and the time attributes to use (first present wins).
    times: &'static [(i64, &'static [&'static str])],
    /// Attributes joined into the full description.
    full: &'static [&'static str],
    /// Attributes for the short description (first present wins).
    short: &'static [&'static str],
}

const fn m(
    artifact: &'static str,
    times: &'static [(i64, &'static [&'static str])],
    full: &'static [&'static str],
    short: &'static [&'static str],
) -> Mapping {
    Mapping {
        artifact,
        times,
        full,
        short,
    }
}

const ACCESSED: &[&str] = &["TSK_DATETIME_ACCESSED"];
const CREATED: &[&str] = &["TSK_DATETIME_CREATED"];
const DATETIME: &[&str] = &["TSK_DATETIME"];
const START: &[&str] = &["TSK_DATETIME_START"];
const END: &[&str] = &["TSK_DATETIME_END"];
const GEO: &[&str] = &["TSK_NAME", "TSK_GEO_LATITUDE", "TSK_GEO_LONGITUDE"];

/// TSK's artifact event types (`TimelineEventTypes`), plus postmortem's
/// event-log entries as Log Entry events.
const MAPPINGS: &[Mapping] = &[
    m(
        "TSK_WEB_BOOKMARK",
        &[(ev::WEB_BOOKMARK, CREATED)],
        &["TSK_URL", "TSK_TITLE"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_WEB_COOKIE",
        &[
            (ev::WEB_COOKIE, &["TSK_DATETIME_CREATED", "TSK_DATETIME"]),
            (ev::WEB_COOKIE_ACCESSED, ACCESSED),
            (ev::WEB_COOKIE_END, END),
        ],
        &["TSK_URL", "TSK_NAME", "TSK_VALUE"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_WEB_HISTORY",
        &[
            (ev::WEB_HISTORY, ACCESSED),
            (ev::WEB_HISTORY_CREATED, CREATED),
        ],
        &["TSK_URL", "TSK_TITLE"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_WEB_DOWNLOAD",
        &[(ev::WEB_DOWNLOADS, ACCESSED)],
        &["TSK_PATH", "TSK_URL"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_WEB_SEARCH_QUERY",
        &[(ev::WEB_SEARCH, ACCESSED)],
        &["TSK_TEXT", "TSK_DOMAIN", "TSK_PROG_NAME"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_WEB_FORM_AUTOFILL",
        &[
            (ev::WEB_FORM_AUTOFILL, CREATED),
            (ev::WEB_FORM_AUTOFILL_ACCESSED, ACCESSED),
        ],
        &["TSK_NAME", "TSK_VALUE"],
        &["TSK_NAME"],
    ),
    m(
        "TSK_WEB_FORM_ADDRESS",
        &[
            (ev::WEB_FORM_ADDRESSES, ACCESSED),
            (ev::WEB_FORM_ADDRESSES_MODIFIED, &["TSK_DATETIME_MODIFIED"]),
        ],
        &["TSK_NAME", "TSK_LOCATION"],
        &["TSK_NAME"],
    ),
    m(
        "TSK_WEB_CACHE",
        &[(ev::WEB_CACHE, CREATED)],
        &["TSK_PATH", "TSK_URL"],
        &["TSK_DOMAIN"],
    ),
    m(
        "TSK_MESSAGE",
        &[(ev::MESSAGE, DATETIME)],
        &[
            "TSK_MESSAGE_TYPE",
            "TSK_DIRECTION",
            "TSK_PHONE_NUMBER_FROM",
            "TSK_PHONE_NUMBER_TO",
            "TSK_SUBJECT",
            "TSK_TEXT",
        ],
        &["TSK_MESSAGE_TYPE"],
    ),
    m(
        "TSK_EMAIL_MSG",
        &[
            (ev::EMAIL, &["TSK_DATETIME_SENT"]),
            (ev::EMAIL_RCVD, &["TSK_DATETIME_RCVD"]),
        ],
        &["TSK_EMAIL_FROM", "TSK_EMAIL_TO", "TSK_SUBJECT"],
        &["TSK_EMAIL_FROM"],
    ),
    m(
        "TSK_CALLLOG",
        &[(ev::CALL_LOG, START), (ev::CALL_LOG_END, END)],
        &["TSK_NAME", "TSK_PHONE_NUMBER", "TSK_DIRECTION"],
        &["TSK_DIRECTION"],
    ),
    m(
        "TSK_RECENT_OBJECT",
        &[(
            ev::RECENT_DOCUMENTS,
            &["TSK_DATETIME_ACCESSED", "TSK_DATETIME"],
        )],
        &["TSK_PATH", "TSK_PROG_NAME"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_INSTALLED_PROG",
        &[(ev::INSTALLED_PROGRAM, DATETIME)],
        &["TSK_PROG_NAME", "TSK_VERSION", "TSK_PATH"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_DELETED_PROG",
        &[(
            ev::PROGRAM_DELETED,
            &["TSK_DATETIME_DELETED", "TSK_DATETIME"],
        )],
        &["TSK_PROG_NAME", "TSK_PATH"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_PROG_RUN",
        &[(ev::PROGRAM_EXECUTION, DATETIME)],
        &["TSK_PROG_NAME", "TSK_PATH", "TSK_COMMENT", "TSK_COUNT"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_PROG_NOTIFICATIONS",
        &[(ev::PROGRAM_NOTIFICATION, DATETIME)],
        &["TSK_PROG_NAME", "TSK_TITLE", "TSK_TEXT"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_METADATA_EXIF",
        &[(ev::EXIF, CREATED)],
        &["TSK_DEVICE_MAKE", "TSK_DEVICE_MODEL"],
        &["TSK_DEVICE_MAKE"],
    ),
    m(
        "TSK_METADATA",
        &[
            (ev::METADATA_LAST_PRINTED, &["TSK_LAST_PRINTED_DATETIME"]),
            (ev::METADATA_LAST_SAVED, &["TSK_DATETIME_MODIFIED"]),
            (ev::METADATA_CREATED, CREATED),
        ],
        &[
            "TSK_PROG_NAME",
            "TSK_USER_ID",
            "TSK_OWNER",
            "TSK_LAST_SAVED_BY",
        ],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_DEVICE_ATTACHED",
        &[(ev::DEVICES_ATTACHED, DATETIME)],
        &["TSK_DEVICE_MAKE", "TSK_DEVICE_MODEL", "TSK_DEVICE_ID"],
        &["TSK_DEVICE_MODEL", "TSK_DEVICE_ID"],
    ),
    m(
        "TSK_OS_INFO",
        &[(ev::OS_INFO, DATETIME)],
        &["TSK_PROG_NAME", "TSK_VERSION", "TSK_NAME"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_WIFI_NETWORK",
        &[(ev::WIFI_NETWORK, DATETIME)],
        &["TSK_SSID", "TSK_DEVICE_ID"],
        &["TSK_SSID"],
    ),
    m(
        "TSK_BLUETOOTH_PAIRING",
        &[
            (ev::BLUETOOTH_PAIRING, DATETIME),
            (ev::BLUETOOTH_PAIRING_ACCESSED, ACCESSED),
        ],
        &["TSK_DEVICE_NAME", "TSK_MAC_ADDRESS"],
        &["TSK_DEVICE_NAME"],
    ),
    m(
        "TSK_BLUETOOTH_ADAPTER",
        &[(ev::BLUETOOTH_ADAPTER, DATETIME)],
        &["TSK_NAME", "TSK_MAC_ADDRESS"],
        &["TSK_NAME"],
    ),
    m(
        "TSK_SERVICE_ACCOUNT",
        &[(ev::SERVICE_ACCOUNT, CREATED)],
        &["TSK_PROG_NAME", "TSK_USER_ID", "TSK_DOMAIN"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_SCREEN_SHOTS",
        &[(ev::SCREEN_SHOT, DATETIME)],
        &["TSK_PROG_NAME", "TSK_PATH"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_CALENDAR_ENTRY",
        &[
            (ev::CALENDAR_ENTRY_START, START),
            (ev::CALENDAR_ENTRY_END, END),
        ],
        &["TSK_DESCRIPTION", "TSK_LOCATION"],
        &["TSK_DESCRIPTION"],
    ),
    m(
        "TSK_USER_DEVICE_EVENT",
        &[
            (ev::USER_DEVICE_EVENT_START, START),
            (ev::USER_DEVICE_EVENT_END, END),
        ],
        &["TSK_PROG_NAME", "TSK_ACTIVITY_TYPE", "TSK_VALUE"],
        &["TSK_ACTIVITY_TYPE"],
    ),
    m(
        "TSK_BACKUP_EVENT",
        &[(ev::BACKUP_EVENT_START, START), (ev::BACKUP_EVENT_END, END)],
        &["TSK_PROG_NAME", "TSK_PATH"],
        &["TSK_PROG_NAME"],
    ),
    m(
        "TSK_GPS_TRACKPOINT",
        &[(ev::GPS_TRACKPOINT, DATETIME)],
        GEO,
        &["TSK_NAME"],
    ),
    m(
        "TSK_GPS_BOOKMARK",
        &[(ev::GPS_BOOKMARK, DATETIME)],
        GEO,
        &["TSK_NAME"],
    ),
    m(
        "TSK_GPS_LAST_KNOWN_LOCATION",
        &[(ev::GPS_LAST_KNOWN_LOCATION, DATETIME)],
        GEO,
        &["TSK_NAME"],
    ),
    m(
        "TSK_GPS_SEARCH",
        &[(ev::GPS_SEARCH, DATETIME)],
        GEO,
        &["TSK_NAME"],
    ),
    m(
        "TSK_GPS_ROUTE",
        &[(ev::GPS_ROUTE, DATETIME)],
        &["TSK_NAME", "TSK_LOCATION"],
        &["TSK_NAME"],
    ),
    m(
        "PM_EVENT_LOG",
        &[(ev::LOG_ENTRY, DATETIME)],
        &[
            "PM_EVENT_CHANNEL",
            "PM_EVENT_ID",
            "PM_EVENT_DESCRIPTION",
            "TSK_USER_NAME",
            "PM_REMOTE_HOST",
            "PM_EXECUTABLE_INFO",
            "PM_PAYLOAD_DATA_1",
            "PM_PAYLOAD_DATA_2",
        ],
        &["PM_EVENT_DESCRIPTION"],
    ),
];

/// Attribute values joined for descriptions: long texts stay readable.
const MAX_PART: usize = 300;

/// A named attribute of the artifact being described.
struct Attr<'a> {
    name: String,
    display: String,
    value: &'a Value,
}

fn text_of(v: &Value) -> Option<String> {
    let s = match v {
        Value::Text(s) => s.trim().to_owned(),
        Value::Int(i) => i.to_string(),
        Value::Double(d) => d.to_string(),
        Value::DateTime(t) => jiff::Timestamp::from_second(*t).ok()?.to_string(),
        Value::Bytes(_) | Value::Json(_) => return None,
    };
    if s.is_empty() {
        return None;
    }
    Some(if s.chars().count() > MAX_PART {
        let mut t: String = s.chars().take(MAX_PART).collect();
        t.push('…');
        t
    } else {
        s
    })
}

fn time_of(v: &Value) -> Option<i64> {
    match v {
        Value::DateTime(t) | Value::Int(t) if *t > 0 => Some(*t),
        _ => None,
    }
}

/// The events of an artifact just inserted as `id`.
pub(crate) fn add_artifact_events(tx: &Connection, id: i64, a: &NewArtifact) -> Result<usize> {
    let ty: Option<(String, String, i64, bool)> = tx
        .prepare_cached(
            "SELECT name, display_name, category, custom FROM artifact_types WHERE id = ?1",
        )?
        .query_row([a.type_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    // Data artifacts only (category 0), as TSK.
    let Some((type_name, type_display, 0, custom)) = ty else {
        return Ok(0);
    };
    let mut names =
        tx.prepare_cached("SELECT name, display_name FROM attribute_types WHERE id = ?1")?;
    let mut attrs = Vec::with_capacity(a.attributes.len());
    for (type_id, value, _) in &a.attributes {
        if let Some((name, display)) = names
            .query_row([type_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
        {
            attrs.push(Attr {
                name,
                display,
                value,
            });
        }
    }
    let get = |n: &str| attrs.iter().find(|x| x.name == n).map(|x| x.value);
    let event = |type_id: i64, time: i64, full: String, short: Option<String>| NewEvent {
        type_id,
        time,
        full_description: full,
        med_description: None,
        short_description: short,
        ds_id: a.ds_id,
        file_id: a.file_id,
        artifact_id: Some(id),
    };
    let mut events = Vec::new();
    if let Some(m) = MAPPINGS.iter().find(|m| m.artifact == type_name) {
        let parts: Vec<String> = m
            .full
            .iter()
            .filter_map(|n| get(n).and_then(text_of))
            .collect();
        let full = if parts.is_empty() {
            type_display.clone()
        } else {
            parts.join(" : ")
        };
        let short = m.short.iter().find_map(|n| get(n).and_then(text_of));
        for (event_type, time_attrs) in m.times {
            if let Some(t) = time_attrs.iter().find_map(|n| get(n).and_then(time_of)) {
                events.push(event(*event_type, t, full.clone(), short.clone()));
            }
        }
    } else {
        // TSK's catch-all: one event per date attribute.
        let catch_all = if custom {
            ev::CUSTOM_ARTIFACT_CATCH_ALL
        } else {
            ev::STANDARD_ARTIFACT_CATCH_ALL
        };
        let texts: Vec<String> = attrs
            .iter()
            .filter(|x| !matches!(x.value, Value::DateTime(_)))
            .filter_map(|x| text_of(x.value))
            .take(4)
            .collect();
        for x in &attrs {
            if let Value::DateTime(t) = x.value
                && *t > 0
            {
                let mut full = format!("{type_display}: {}", x.display);
                if !texts.is_empty() {
                    full.push_str(" : ");
                    full.push_str(&texts.join(" : "));
                }
                events.push(event(catch_all, *t, full, Some(type_display.clone())));
            }
        }
    }
    insert_events(tx, &events)
}

/// Meta key recording that artifact events were back-filled.
const BACKFILLED: &str = "artifact_events_v1";

impl CaseDb {
    /// Adds the events of every data artifact posted before events were
    /// written with artifacts (cases made by postmortem 0.1.1 and older).
    /// Runs once per case; returns the number of events added.
    pub fn backfill_artifact_events(&self) -> Result<usize> {
        if self.meta(BACKFILLED)?.is_some() {
            return Ok(0);
        }
        let added = self.write(|tx| {
            let ids: Vec<i64> = tx
                .prepare(
                    "SELECT a.id FROM artifacts a JOIN artifact_types t ON t.id = a.type_id \
                     WHERE t.category = 0 AND NOT EXISTS \
                       (SELECT 1 FROM event_descriptions d WHERE d.artifact_id = a.id) \
                     ORDER BY a.id",
                )?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut added = 0;
            for id in ids {
                let a = artifact_for_events(tx, id)?;
                added += add_artifact_events(tx, id, &a)?;
            }
            tx.execute(
                "INSERT INTO meta(key, value) VALUES (?1, '1') ON CONFLICT(key) DO NOTHING",
                [BACKFILLED],
            )?;
            Ok(added)
        })?;
        if added > 0 {
            tracing::info!(added, "timeline events added for existing artifacts");
        }
        Ok(added)
    }
}

/// An artifact as [`add_artifact_events`] needs it.
fn artifact_for_events(tx: &Connection, id: i64) -> Result<NewArtifact> {
    let (type_id, ds_id, file_id): (i64, i64, Option<i64>) = tx.query_row(
        "SELECT type_id, ds_id, file_id FROM artifacts WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut st = tx.prepare_cached(
        "SELECT a.type_id, t.value_type, a.value_text, a.value_int, a.value_double \
         FROM attributes a JOIN attribute_types t ON t.id = a.type_id WHERE a.artifact_id = ?1",
    )?;
    let attributes = st
        .query_map(params![id], |r| {
            let vt: i64 = r.get(1)?;
            let text: Option<String> = r.get(2)?;
            let int: Option<i64> = r.get(3)?;
            let double: Option<f64> = r.get(4)?;
            let value = match (vt, text, int, double) {
                (5, _, Some(t), _) => Value::DateTime(t),
                (_, Some(s), _, _) => Value::Text(s),
                (_, _, Some(i), _) => Value::Int(i),
                (_, _, _, Some(d)) => Value::Double(d),
                _ => Value::Text(String::new()),
            };
            Ok((r.get::<_, i64>(0)?, value, String::new()))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(NewArtifact {
        type_id,
        ds_id,
        file_id,
        source_artifact_id: None,
        score: None,
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 0,
        attributes,
    })
}
