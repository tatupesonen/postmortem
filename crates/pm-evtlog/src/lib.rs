//! Windows event logs in one record model.
//!
//! * **EVTX** (Vista and later) through the `evtx` crate (ADR-009), single
//!   threaded, with the record XML flattened into named fields.
//! * **EVT** (NT 4 – Server 2003) parsed natively: records are found by
//!   their `LfLe` signature, so wrapped circular logs, dirty logs and
//!   records in slack are all recovered.
//!
//! [`MapSet`] applies EvtxECmd-format event maps (468 bundled, plus
//! postmortem's maps for legacy logs, plus examiner-supplied ones).
//!
//! Both produce [`EventRecord`]s whose `data` holds the event's fields:
//! EVTX `EventData`/`UserData` by name (unnamed `Data` items as `"1"`,
//! `"2"`, …), EVT insertion strings as `"1"`, `"2"`, …

mod evt;
mod evtx_log;
pub mod maps;
pub mod sid;

use pm_core::SharedSource;
use thiserror::Error;

pub use evt::{parse_evt, parse_evt_bytes};
pub use evtx_log::{from_xml, parse_evtx};
pub use maps::{EventMap, MapSet, Mapped};

#[derive(Debug, Error)]
pub enum EvtLogError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("EVTX: {0}")]
    Evtx(String),
}

/// Which file format a log came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Evtx,
    Evt,
}

/// One event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub format: LogFormat,
    /// EVTX channel (`Security`, `Microsoft-Windows-…/Operational`); for
    /// EVT the log name is unknown here and left empty.
    pub channel: String,
    /// Provider (EVTX) / source (EVT) name.
    pub provider: String,
    /// Event identifier (EVT: low 16 bits of the event code).
    pub event_id: u32,
    pub record_id: u64,
    /// Unix seconds (UTC).
    pub time: i64,
    pub computer: String,
    /// User SID of the event, if recorded.
    pub user_sid: Option<String>,
    /// Field name → value, in record order.
    pub data: Vec<(String, String)>,
    /// The record as XML (EVTX only), for event maps.
    pub xml: Option<String>,
}

impl EventRecord {
    /// The value of field `name` (case-insensitive), if present and not a
    /// `-` placeholder.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.data
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty() && *v != "-")
    }

    /// The `n`-th (1-based) insertion string / unnamed data item.
    pub fn nth(&self, n: usize) -> Option<&str> {
        self.get(&n.to_string())
    }
}

/// Parses a log, picking the format from its signature. Records are
/// passed to `f`; parsing stops early when `f` returns `false`.
pub fn parse(
    src: &SharedSource,
    f: &mut dyn FnMut(EventRecord) -> bool,
) -> Result<(), EvtLogError> {
    let mut magic = [0u8; 8];
    let n = src.read_at(0, &mut magic)?;
    if magic.get(..n) == Some(b"ElfFile\0".as_slice()) {
        parse_evtx(src, f)
    } else {
        parse_evt(&**src, f)
    }
}
