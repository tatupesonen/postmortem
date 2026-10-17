//! Timeline events of data artifacts (Autopsy `TimelineManager.addArtifactEvents`
//! with the `TimelineEventType` definitions): written in the same
//! transaction as the artifact, so the timeline, MCP and reports see web
//! activity, program runs, devices, messages and log entries next to file
//! times.
//!
//! Types Autopsy maps get their own event types and descriptions; any other
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

const ACCESSED: &[&str] = &["KAAMOS_DATETIME_ACCESSED"];
const CREATED: &[&str] = &["KAAMOS_DATETIME_CREATED"];
const DATETIME: &[&str] = &["KAAMOS_DATETIME"];
const START: &[&str] = &["KAAMOS_DATETIME_START"];
const END: &[&str] = &["KAAMOS_DATETIME_END"];
const GEO: &[&str] = &["KAAMOS_NAME", "KAAMOS_GEO_LATITUDE", "KAAMOS_GEO_LONGITUDE"];

/// Autopsy's artifact event types (`TimelineEventTypes`), plus postmortem's
/// event-log entries as Log Entry events.
const MAPPINGS: &[Mapping] = &[
    m(
        "KAAMOS_WEB_BOOKMARK",
        &[(ev::WEB_BOOKMARK, CREATED)],
        &["KAAMOS_URL", "KAAMOS_TITLE"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_WEB_COOKIE",
        &[
            (
                ev::WEB_COOKIE,
                &["KAAMOS_DATETIME_CREATED", "KAAMOS_DATETIME"],
            ),
            (ev::WEB_COOKIE_ACCESSED, ACCESSED),
            (ev::WEB_COOKIE_END, END),
        ],
        &["KAAMOS_URL", "KAAMOS_NAME", "KAAMOS_VALUE"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_WEB_HISTORY",
        &[
            (ev::WEB_HISTORY, ACCESSED),
            (ev::WEB_HISTORY_CREATED, CREATED),
        ],
        &["KAAMOS_URL", "KAAMOS_TITLE"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_WEB_DOWNLOAD",
        &[(ev::WEB_DOWNLOADS, ACCESSED)],
        &["KAAMOS_PATH", "KAAMOS_URL"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_WEB_SEARCH_QUERY",
        &[(ev::WEB_SEARCH, ACCESSED)],
        &["KAAMOS_TEXT", "KAAMOS_DOMAIN", "KAAMOS_PROG_NAME"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_WEB_FORM_AUTOFILL",
        &[
            (ev::WEB_FORM_AUTOFILL, CREATED),
            (ev::WEB_FORM_AUTOFILL_ACCESSED, ACCESSED),
        ],
        &["KAAMOS_NAME", "KAAMOS_VALUE"],
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_WEB_FORM_ADDRESS",
        &[
            (ev::WEB_FORM_ADDRESSES, ACCESSED),
            (
                ev::WEB_FORM_ADDRESSES_MODIFIED,
                &["KAAMOS_DATETIME_MODIFIED"],
            ),
        ],
        &["KAAMOS_NAME", "KAAMOS_LOCATION"],
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_WEB_CACHE",
        &[(ev::WEB_CACHE, CREATED)],
        &["KAAMOS_PATH", "KAAMOS_URL"],
        &["KAAMOS_DOMAIN"],
    ),
    m(
        "KAAMOS_MESSAGE",
        &[(ev::MESSAGE, DATETIME)],
        &[
            "KAAMOS_MESSAGE_TYPE",
            "KAAMOS_DIRECTION",
            "KAAMOS_PHONE_NUMBER_FROM",
            "KAAMOS_PHONE_NUMBER_TO",
            "KAAMOS_SUBJECT",
            "KAAMOS_TEXT",
        ],
        &["KAAMOS_MESSAGE_TYPE"],
    ),
    m(
        "KAAMOS_EMAIL_MSG",
        &[
            (ev::EMAIL, &["KAAMOS_DATETIME_SENT"]),
            (ev::EMAIL_RCVD, &["KAAMOS_DATETIME_RCVD"]),
        ],
        &["KAAMOS_EMAIL_FROM", "KAAMOS_EMAIL_TO", "KAAMOS_SUBJECT"],
        &["KAAMOS_EMAIL_FROM"],
    ),
    m(
        "KAAMOS_CALLLOG",
        &[(ev::CALL_LOG, START), (ev::CALL_LOG_END, END)],
        &["KAAMOS_NAME", "KAAMOS_PHONE_NUMBER", "KAAMOS_DIRECTION"],
        &["KAAMOS_DIRECTION"],
    ),
    m(
        "KAAMOS_RECENT_OBJECT",
        &[(
            ev::RECENT_DOCUMENTS,
            &["KAAMOS_DATETIME_ACCESSED", "KAAMOS_DATETIME"],
        )],
        &["KAAMOS_PATH", "KAAMOS_PROG_NAME"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_INSTALLED_PROG",
        &[(ev::INSTALLED_PROGRAM, DATETIME)],
        &["KAAMOS_PROG_NAME", "KAAMOS_VERSION", "KAAMOS_PATH"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_DELETED_PROG",
        &[(
            ev::PROGRAM_DELETED,
            &["KAAMOS_DATETIME_DELETED", "KAAMOS_DATETIME"],
        )],
        &["KAAMOS_PROG_NAME", "KAAMOS_PATH"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_PROG_RUN",
        &[(ev::PROGRAM_EXECUTION, DATETIME)],
        &[
            "KAAMOS_PROG_NAME",
            "KAAMOS_PATH",
            "KAAMOS_COMMENT",
            "KAAMOS_COUNT",
        ],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_PROG_NOTIFICATIONS",
        &[(ev::PROGRAM_NOTIFICATION, DATETIME)],
        &["KAAMOS_PROG_NAME", "KAAMOS_TITLE", "KAAMOS_TEXT"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_METADATA_EXIF",
        &[(ev::EXIF, CREATED)],
        &["KAAMOS_DEVICE_MAKE", "KAAMOS_DEVICE_MODEL"],
        &["KAAMOS_DEVICE_MAKE"],
    ),
    m(
        "KAAMOS_METADATA",
        &[
            (ev::METADATA_LAST_PRINTED, &["KAAMOS_LAST_PRINTED_DATETIME"]),
            (ev::METADATA_LAST_SAVED, &["KAAMOS_DATETIME_MODIFIED"]),
            (ev::METADATA_CREATED, CREATED),
        ],
        &[
            "KAAMOS_PROG_NAME",
            "KAAMOS_USER_ID",
            "KAAMOS_OWNER",
            "KAAMOS_LAST_SAVED_BY",
        ],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_DEVICE_ATTACHED",
        &[(ev::DEVICES_ATTACHED, DATETIME)],
        &[
            "KAAMOS_DEVICE_MAKE",
            "KAAMOS_DEVICE_MODEL",
            "KAAMOS_DEVICE_ID",
        ],
        &["KAAMOS_DEVICE_MODEL", "KAAMOS_DEVICE_ID"],
    ),
    m(
        "KAAMOS_OS_INFO",
        &[(ev::OS_INFO, DATETIME)],
        &["KAAMOS_PROG_NAME", "KAAMOS_VERSION", "KAAMOS_NAME"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_WIFI_NETWORK",
        &[(ev::WIFI_NETWORK, DATETIME)],
        &["KAAMOS_SSID", "KAAMOS_DEVICE_ID"],
        &["KAAMOS_SSID"],
    ),
    m(
        "KAAMOS_BLUETOOTH_PAIRING",
        &[
            (ev::BLUETOOTH_PAIRING, DATETIME),
            (ev::BLUETOOTH_PAIRING_ACCESSED, ACCESSED),
        ],
        &["KAAMOS_DEVICE_NAME", "KAAMOS_MAC_ADDRESS"],
        &["KAAMOS_DEVICE_NAME"],
    ),
    m(
        "KAAMOS_BLUETOOTH_ADAPTER",
        &[(ev::BLUETOOTH_ADAPTER, DATETIME)],
        &["KAAMOS_NAME", "KAAMOS_MAC_ADDRESS"],
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_SERVICE_ACCOUNT",
        &[(ev::SERVICE_ACCOUNT, CREATED)],
        &["KAAMOS_PROG_NAME", "KAAMOS_USER_ID", "KAAMOS_DOMAIN"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_SCREEN_SHOTS",
        &[(ev::SCREEN_SHOT, DATETIME)],
        &["KAAMOS_PROG_NAME", "KAAMOS_PATH"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_CALENDAR_ENTRY",
        &[
            (ev::CALENDAR_ENTRY_START, START),
            (ev::CALENDAR_ENTRY_END, END),
        ],
        &["KAAMOS_DESCRIPTION", "KAAMOS_LOCATION"],
        &["KAAMOS_DESCRIPTION"],
    ),
    m(
        "KAAMOS_USER_DEVICE_EVENT",
        &[
            (ev::USER_DEVICE_EVENT_START, START),
            (ev::USER_DEVICE_EVENT_END, END),
        ],
        &["KAAMOS_PROG_NAME", "KAAMOS_ACTIVITY_TYPE", "KAAMOS_VALUE"],
        &["KAAMOS_ACTIVITY_TYPE"],
    ),
    m(
        "KAAMOS_BACKUP_EVENT",
        &[(ev::BACKUP_EVENT_START, START), (ev::BACKUP_EVENT_END, END)],
        &["KAAMOS_PROG_NAME", "KAAMOS_PATH"],
        &["KAAMOS_PROG_NAME"],
    ),
    m(
        "KAAMOS_GPS_TRACKPOINT",
        &[(ev::GPS_TRACKPOINT, DATETIME)],
        GEO,
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_GPS_BOOKMARK",
        &[(ev::GPS_BOOKMARK, DATETIME)],
        GEO,
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_GPS_LAST_KNOWN_LOCATION",
        &[(ev::GPS_LAST_KNOWN_LOCATION, DATETIME)],
        GEO,
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_GPS_SEARCH",
        &[(ev::GPS_SEARCH, DATETIME)],
        GEO,
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_GPS_ROUTE",
        &[(ev::GPS_ROUTE, DATETIME)],
        &["KAAMOS_NAME", "KAAMOS_LOCATION"],
        &["KAAMOS_NAME"],
    ),
    m(
        "KAAMOS_EVENT_LOG",
        &[(ev::LOG_ENTRY, DATETIME)],
        &[
            "KAAMOS_EVENT_CHANNEL",
            "KAAMOS_EVENT_ID",
            "KAAMOS_EVENT_DESCRIPTION",
            "KAAMOS_USER_NAME",
            "KAAMOS_REMOTE_HOST",
            "KAAMOS_EXECUTABLE_INFO",
            "KAAMOS_PAYLOAD_DATA_1",
            "KAAMOS_PAYLOAD_DATA_2",
        ],
        &["KAAMOS_EVENT_DESCRIPTION"],
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
    // Data artifacts only (category 0), as Autopsy.
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
        // Autopsy's catch-all: one event per date attribute.
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
