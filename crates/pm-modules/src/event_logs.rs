//! Windows Event Logs — a postmortem module beyond Autopsy (which only
//! reaches event logs through Plaso). Every `.evtx` / `.evt` log of the
//! data source is parsed natively (`pm-evtlog`) and every event that has an
//! EvtxECmd-format map becomes an "Event Log Entries" artifact with
//! EvtxECmd's columns (map description, user, remote host, executable,
//! payloads). Cleared logs are flagged as anti-forensics.

use std::sync::Arc;

use pm_casedb::catalog::{Category, ValueType};
use pm_casedb::{FileRecord, Significance, Value};
use pm_evtlog::{EventRecord, MapSet, Mapped};
use pm_ingest::{
    DataSourceModule, DsProgress, IngestMessage, JobContext, MessageKind, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

use crate::recent_activity::find_by_extension;
use crate::util::Art;

pub const NAME: &str = "Windows Event Logs";
const ARTIFACT: (&str, &str) = ("KAAMOS_EVENT_LOG", "Event Log Entries");

/// Per-job settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventLogSettings {
    /// Directory with additional EvtxECmd `.map` files.
    pub extra_maps: Option<std::path::PathBuf>,
}

#[derive(Debug, Default)]
pub struct EventLogFactory;

impl ModuleFactory for EventLogFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "event_logs",
            name: NAME,
            description: "Parses Windows event logs (EVTX and legacy EVT) with EvtxECmd-compatible maps: logons, RDP, account changes, services, PowerShell, USB, Defender, log clearing and more.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(EventLogSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: EventLogSettings = serde_json::from_value(settings.clone())
            .map_err(|e| ModuleError::new(format!("invalid settings: {e}")))?;
        let mut maps = MapSet::builtin().clone();
        if let Some(dir) = &settings.extra_maps {
            let warnings = maps
                .load_dir(dir)
                .map_err(|e| ModuleError::new(format!("{}: {e}", dir.display())))?;
            for w in warnings {
                ctx.post_message(
                    IngestMessage::new(MessageKind::Warning, NAME, "Event map not loaded")
                        .details(w),
                );
            }
        }
        let types = Types::register(ctx)?;
        Ok(Arc::new(Job {
            maps: Arc::new(maps),
            types,
        }))
    }
}

/// Database ids of the custom artifact and attribute types.
#[derive(Debug, Clone, Copy)]
struct Types {
    artifact: i64,
    event_id: i64,
    channel: i64,
    provider: i64,
    description: i64,
    remote_host: i64,
    executable: i64,
    computer: i64,
    record_id: i64,
    payload: [i64; 6],
}

impl Types {
    fn register(ctx: &JobContext) -> Result<Self, ModuleError> {
        let db = ctx.case.db();
        let a = |n: &str, d: &str, v: ValueType| db.attribute_type(n, d, v);
        let mut payload = [0i64; 6];
        for (i, p) in payload.iter_mut().enumerate() {
            *p = a(
                &format!("KAAMOS_PAYLOAD_DATA_{}", i + 1),
                &format!("Payload {}", i + 1),
                ValueType::String,
            )?;
        }
        Ok(Self {
            artifact: db.artifact_type(ARTIFACT.0, ARTIFACT.1, Category::DataArtifact)?,
            event_id: a("KAAMOS_EVENT_ID", "Event ID", ValueType::Integer)?,
            channel: a("KAAMOS_EVENT_CHANNEL", "Channel", ValueType::String)?,
            provider: a("KAAMOS_EVENT_PROVIDER", "Provider", ValueType::String)?,
            description: a("KAAMOS_EVENT_DESCRIPTION", "Event", ValueType::String)?,
            remote_host: a("KAAMOS_REMOTE_HOST", "Remote Host", ValueType::String)?,
            executable: a(
                "KAAMOS_EXECUTABLE_INFO",
                "Executable Info",
                ValueType::String,
            )?,
            computer: a("KAAMOS_COMPUTER_NAME", "Computer", ValueType::String)?,
            record_id: a("KAAMOS_EVENT_RECORD_ID", "Event Record ID", ValueType::Long)?,
            payload,
        })
    }
}

struct Job {
    maps: Arc<MapSet>,
    types: Types,
}

impl ModuleJob for Job {
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((
            1,
            Box::new(Extractor {
                maps: Arc::clone(&self.maps),
                types: self.types,
            }),
        ))
    }
}

struct Extractor {
    maps: Arc<MapSet>,
    types: Types,
}

/// The log name of a legacy `.evt` file (`SecEvent.Evt` → `Security`).
pub fn legacy_channel(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(s, _)| s)
        .to_ascii_lowercase();
    match stem.as_str() {
        "secevent" => "Security".into(),
        "sysevent" => "System".into(),
        "appevent" => "Application".into(),
        "internet" => "Internet Explorer".into(),
        other => other.to_owned(),
    }
}

/// Whether an event documents a cleared log (anti-forensics).
fn log_cleared(rec: &EventRecord) -> bool {
    match (rec.event_id, rec.channel.to_ascii_lowercase().as_str()) {
        (1102, "security") | (104, "system") => true,
        (517, "security") => rec.format == pm_evtlog::LogFormat::Evt,
        _ => false,
    }
}

impl DataSourceModule for Extractor {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let mut logs = find_by_extension(ctx, "evtx", "")?;
        logs.extend(find_by_extension(ctx, "evt", "")?);
        progress.set_total(logs.len() as u64);
        let mut events = 0u64;
        for (i, f) in logs.iter().enumerate() {
            if ctx.is_cancelled() {
                break;
            }
            progress.message(format!("{} ({} mapped events so far)", f.name, events));
            if self.already_done(ctx, f) {
                progress.set_done(i as u64 + 1);
                continue;
            }
            match self.log(ctx, f) {
                Ok(n) => events += n,
                Err(e) => ctx.post_message(
                    IngestMessage::new(
                        MessageKind::Warning,
                        NAME,
                        format!("Could not parse {}", f.name),
                    )
                    .details(e.0)
                    .file(f.id),
                ),
            }
            progress.set_done(i as u64 + 1);
        }
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "Event log analysis finished")
                .details(format!("{} logs, {events} mapped events", logs.len())),
        );
        Ok(())
    }
}

impl Extractor {
    fn already_done(&self, ctx: &JobContext, f: &FileRecord) -> bool {
        ctx.case
            .db()
            .artifacts_for_file(f.id)
            .is_ok_and(|a| a.iter().any(|a| a.type_id == self.types.artifact))
    }

    /// Parses one log; returns the number of mapped events.
    fn log(&self, ctx: &JobContext, f: &FileRecord) -> Result<u64, ModuleError> {
        let src = ctx.case.content(f)?;
        let legacy = legacy_channel(&f.name);
        let mut n = 0u64;
        let mut seen = 0u64;
        let mut cancelled = false;
        pm_evtlog::parse(&src, &mut |mut rec| {
            seen += 1;
            if rec.channel.is_empty() {
                rec.channel.clone_from(&legacy);
            }
            if log_cleared(&rec) {
                Art::result(
                    ctx,
                    NAME,
                    "KAAMOS_INTERESTING_ITEM",
                    f.id,
                    Significance::LikelyNotable,
                )
                .text("KAAMOS_SET_NAME", "Anti-forensics")
                .text(
                    "KAAMOS_COMMENT",
                    format!("Event log cleared ({} event {})", rec.channel, rec.event_id),
                )
                .value("KAAMOS_DATETIME", Value::DateTime(rec.time))
                .text_opt(
                    "KAAMOS_USER_NAME",
                    rec.get("SubjectUserName").map(str::to_owned).or_else(|| {
                        rec.nth(4)
                            .map(str::to_owned)
                            .filter(|_| rec.format == pm_evtlog::LogFormat::Evt)
                    }),
                )
                .post_unchecked(ctx);
            }
            if let Some(m) = self.maps.apply(&rec) {
                self.post(ctx, f, &rec, m);
                n += 1;
            }
            // Poll for cancellation without paying for it on every record.
            if seen.is_multiple_of(1024) && ctx.is_cancelled() {
                cancelled = true;
                return false;
            }
            true
        })
        .map_err(|e| ModuleError::new(e.to_string()))?;
        if cancelled {
            tracing::debug!(file = %f.name, "event log parsing cancelled");
        }
        Ok(n)
    }

    fn post(&self, ctx: &JobContext, f: &FileRecord, rec: &EventRecord, m: Mapped) {
        let t = &self.types;
        let mut art = Art::data_of_type(ctx, NAME, t.artifact, f.id)
            .value("KAAMOS_DATETIME", Value::DateTime(rec.time))
            .custom_int(t.event_id, i64::from(rec.event_id))
            .custom_text(t.channel, Some(rec.channel.clone()))
            .custom_text(t.provider, Some(rec.provider.clone()))
            .custom_text(t.description, Some(m.description))
            .text_opt("KAAMOS_USER_NAME", m.user_name)
            .custom_text(t.remote_host, m.remote_host)
            .custom_text(t.executable, m.executable_info)
            .text_opt("KAAMOS_USER_ID", rec.user_sid.clone())
            .custom_text(t.computer, Some(rec.computer.clone()))
            .custom_int(
                t.record_id,
                i64::try_from(rec.record_id).unwrap_or(i64::MAX),
            );
        for (id, p) in t.payload.iter().zip(m.payload) {
            art = art.custom_text(*id, p);
        }
        art.post_unchecked(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_channels() {
        assert_eq!(legacy_channel("SecEvent.Evt"), "Security");
        assert_eq!(legacy_channel("SYSEVENT.EVT"), "System");
        assert_eq!(legacy_channel("AppEvent.Evt"), "Application");
        assert_eq!(legacy_channel("ODiag.evt"), "odiag");
    }
}
