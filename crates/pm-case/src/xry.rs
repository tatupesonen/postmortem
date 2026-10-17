//! "XRY Text Export" data sources (Autopsy `XRYDataSourceProcessor`): a
//! folder exported by MSAB XRY as text. Its entries are added as a logical
//! file set, and every XRY report in it (a UTF-16LE `.txt` starting with a
//! byte-order mark and an `XRY export` header) is parsed into call logs,
//! messages, contacts, device information and web bookmarks.
//!
//! The reports follow Autopsy's reader: a five-line header whose third
//! line names the report type, then entities separated by blank lines;
//! an entity's first line is its title, the others are `key: value` pairs
//! (split at the first colon) or namespace lines (`From`, `To`,
//! `Participant`). Unlike Autopsy, which leaves the reports out of the file
//! set, the reports are added too and carry the artifacts parsed from them,
//! so every result opens its source.

use std::path::{Path, PathBuf};

use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::communications::{
    AccountInstanceSpec, RelationshipKind, is_valid_email_address, normalize_account_id,
};
use pm_casedb::{CaseDb, NewArtifact, Value};

use crate::load::{LoadOptions, LoadProgress, LoadSummary};
use crate::logical::{LogicalOptions, add_logical_files};
use crate::{Case, CaseError, Result, io_err};

/// Attribute source of everything parsed (Autopsy's `PARSER_NAME`).
const MODULE: &str = "XRY DSP";
/// Default display name of the data source.
pub const XRY_SOURCE_NAME: &str = "XRY Text Export";

/// Whether `path` is an XRY report (`isXRYFile`).
pub fn is_xry_file(path: &Path) -> bool {
    use std::io::Read;
    if path.extension().and_then(|e| e.to_str()) != Some("txt") {
        return false;
    }
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    let mut head = [0u8; 4096];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let n = f.read(&mut head).unwrap_or(0);
    let head = head.get(..n).unwrap_or_default();
    head.starts_with(&[0xFF, 0xFE]) && report_type(&decode(head)).is_some()
}

/// The XRY reports directly in `folder` (`XRYFolder.isXRYFolder` looks one
/// level deep).
pub fn xry_files(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(folder)
        .map_err(io_err(folder))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| is_xry_file(p))
        .collect();
    v.sort();
    Ok(v)
}

/// Adds an XRY export folder: its entries as a logical file set, then the
/// artifacts of every report.
pub fn add_xry_folder(
    case: &Case,
    folder: &Path,
    load: &LoadOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    let reports = xry_files(folder)?;
    if reports.is_empty() {
        return Err(CaseError::Invalid(format!(
            "{} does not contain any XRY text files",
            folder.display()
        )));
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)
        .map_err(io_err(folder))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    let mut summary = add_logical_files(
        case,
        &entries,
        &LogicalOptions {
            name: Some(
                load.name
                    .clone()
                    .unwrap_or_else(|| XRY_SOURCE_NAME.to_owned()),
            ),
            modified: false,
            created: false,
            accessed: false,
            host: load.host.clone(),
        },
        progress,
    )?;
    progress.set_stage("Parsing XRY reports");
    let db = case.db();
    let device = db
        .data_source(summary.ds_id)?
        .map_or_else(|| XRY_SOURCE_NAME.to_owned(), |d| d.name);
    for report in &reports {
        if progress.is_cancelled() {
            case.delete_data_source(summary.ds_id)?;
            return Err(CaseError::Cancelled);
        }
        let name = report
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(file) = db.top_level_named(summary.ds_id, &name)?.into_iter().next() else {
            continue;
        };
        let text = decode(&std::fs::read(report).map_err(io_err(report))?);
        let Some(kind) = report_type(&text) else {
            continue;
        };
        let records = parse(kind, &text);
        if let Err(e) = write(db, summary.ds_id, file.id, &device, &records) {
            summary.warnings.push(format!("{name}: {e}"));
        }
    }
    progress.set_stage("Done");
    Ok(summary)
}

/// Decodes UTF-16LE text, dropping the byte-order mark.
fn decode(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| {
            u16::from_le_bytes([
                c.first().copied().unwrap_or(0),
                c.get(1).copied().unwrap_or(0),
            ])
        })
        .collect();
    let s = String::from_utf16_lossy(&units);
    s.strip_prefix('\u{feff}').map_or(s.clone(), str::to_owned)
}

/// Report types Autopsy parses (`XRYFileParserFactory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportKind {
    Calls,
    Contacts,
    DeviceInfo,
    Messages,
    WebBookmarks,
}

/// The header's report type: the third line from the `XRY export` line.
fn report_type(text: &str) -> Option<ReportKind> {
    let mut lines = text.lines();
    lines.find(|l| l.trim().eq_ignore_ascii_case("xry export"))?;
    let ty = lines.nth(1)?.trim().to_lowercase();
    Some(match ty.as_str() {
        "calls" => ReportKind::Calls,
        "contacts/contacts" | "contacts" => ReportKind::Contacts,
        "device/general information" => ReportKind::DeviceInfo,
        "messages/sms" => ReportKind::Messages,
        "web/bookmarks" => ReportKind::WebBookmarks,
        _ => return None,
    })
}

/// The entities after the header, each as its lines (title first).
fn entities(text: &str) -> Vec<Vec<&str>> {
    let mut lines = text.lines();
    if lines
        .find(|l| l.trim().eq_ignore_ascii_case("xry export"))
        .is_none()
    {
        return Vec::new();
    }
    // The header is five lines, the first already read.
    let lines = lines.skip(4);
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for l in lines {
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A `key: value` pair with the namespace it appeared under (all lower
/// case but the value).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pair {
    key: String,
    value: String,
    namespace: String,
}

/// An entity's pairs: lines after the title, namespaces applied; lines
/// that are neither continue the previous value (as Autopsy's messages
/// parser does).
fn pairs(entity: &[&str], namespaces: &[&str]) -> Vec<Pair> {
    let mut ns = String::new();
    let mut out: Vec<Pair> = Vec::new();
    for line in entity.iter().skip(1) {
        let t = line.trim();
        if namespaces.iter().any(|n| t.eq_ignore_ascii_case(n)) {
            ns = t.to_lowercase();
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            out.push(Pair {
                key: k.trim().to_lowercase(),
                value: v.trim().to_owned(),
                namespace: ns.clone(),
            });
        } else if let Some(last) = out.last_mut() {
            if !last.value.is_empty() {
                last.value.push(' ');
            }
            last.value.push_str(t);
        }
    }
    out.retain(|p| !p.value.is_empty());
    out
}

/// Seconds since the epoch of an XRY time such as
/// `1/3/1990 1:23:54 AM (Network)` or `3/20/2012 12:06:30 AM UTC+4`
/// (`XRYUtils.calculateSecondsSinceEpoch`; no offset means UTC).
fn parse_time(value: &str) -> Option<i64> {
    let lower = value.to_lowercase();
    let cut = ["(device)", "(network)"]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(value.len());
    let stripped = value.get(..cut)?.trim();
    let mut parts = stripped.split_whitespace();
    let date = parts.next()?;
    let time = parts.next()?;
    let ampm = parts.next()?.to_ascii_uppercase();
    let offset = parse_offset(parts.next());
    let mut ymd = date.split('/').map(str::parse::<i32>);
    let (month, day, mut year) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    if year < 100 {
        year += 2000;
    }
    let mut hms = time.split(':').map(str::parse::<i32>);
    let (mut h, m, sec) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    match (ampm.as_str(), h) {
        ("AM", 12) => h = 0,
        ("PM", 1..=11) => h += 12,
        ("AM" | "PM", _) => {}
        _ => return None,
    }
    let dt = jiff::civil::DateTime::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
        i8::try_from(h).ok()?,
        i8::try_from(m).ok()?,
        i8::try_from(sec).ok()?,
        0,
    )
    .ok()?;
    let utc = dt
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    Some(utc - i64::from(offset?))
}

/// `UTC+4`, `UTC-03:30`, `GMT+2`, `(+02:00)` → seconds east of UTC; no
/// offset is UTC, an unreadable one `None`.
fn parse_offset(tok: Option<&str>) -> Option<i32> {
    let Some(tok) = tok else { return Some(0) };
    let t = tok.trim_matches(|c| c == '(' || c == ')');
    let t = t
        .strip_prefix("UTC")
        .or_else(|| t.strip_prefix("GMT"))
        .unwrap_or(t);
    if t.is_empty() {
        return Some(0);
    }
    let (sign, rest) = match t.split_at_checked(1)? {
        ("+", r) => (1, r),
        ("-", r) => (-1, r),
        _ => return None,
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h.parse::<i32>().ok()?, m.parse::<i32>().ok()?),
        None => (rest.parse::<i32>().ok()?, 0),
    };
    Some(sign * (h * 3600 + m * 60))
}

fn valid_phone(v: &str) -> bool {
    normalize_account_id("PHONE", v).is_ok()
}

/// One artifact to write, with its phone accounts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub(crate) type_name: &'static str,
    pub(crate) attrs: Vec<(&'static str, Value)>,
    /// Phone numbers related through the artifact (with the device).
    pub(crate) phones: Vec<String>,
    pub(crate) relationship: Option<(RelationshipKind, Option<i64>)>,
}

impl Record {
    fn new(type_name: &'static str) -> Self {
        Self {
            type_name,
            attrs: Vec::new(),
            phones: Vec::new(),
            relationship: None,
        }
    }

    fn text(&mut self, attr: &'static str, v: &str) {
        self.attrs.push((attr, Value::Text(v.to_owned())));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Unknown,
    Incoming,
    Outgoing,
}

impl Direction {
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Incoming => Some("Incoming"),
            Self::Outgoing => Some("Outgoing"),
        }
    }
}

/// Parses a report's entities into records.
pub(crate) fn parse(kind: ReportKind, text: &str) -> Vec<Record> {
    let ents = entities(text);
    match kind {
        ReportKind::Calls => ents.iter().filter_map(|e| call(e)).collect(),
        ReportKind::Messages => messages(&ents),
        ReportKind::Contacts => ents.iter().filter_map(|e| contact(e)).collect(),
        ReportKind::DeviceInfo => ents.iter().filter_map(|e| device_info(e)).collect(),
        ReportKind::WebBookmarks => ents.iter().filter_map(|e| bookmark(e)).collect(),
    }
}

/// `XRYCallsFileParser`.
fn call(entity: &[&str]) -> Option<Record> {
    let mut r = Record::new("KAAMOS_CALLLOG");
    let mut dir = Direction::Unknown;
    let mut caller: Option<String> = None;
    let mut callees: Vec<String> = Vec::new();
    let mut start = None;
    for p in pairs(entity, &["to", "from"]) {
        match p.key.as_str() {
            "name (matched)" | "name" => r.text("KAAMOS_NAME", &p.value),
            "deleted" => r.text("KAAMOS_ISDELETED", &p.value),
            "tel" | "number" => {
                if !valid_phone(&p.value) {
                    continue;
                }
                if p.namespace == "from" || dir == Direction::Incoming {
                    caller = Some(p.value);
                } else if p.namespace == "to" || dir == Direction::Outgoing {
                    callees.push(p.value);
                } else {
                    r.phones.push(p.value.clone());
                    r.text("KAAMOS_PHONE_NUMBER", &p.value);
                }
            }
            "to" if valid_phone(&p.value) => callees.push(p.value),
            "from" if valid_phone(&p.value) => caller = Some(p.value),
            "direction" => {
                dir = if p.value.eq_ignore_ascii_case("incoming") {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                };
            }
            "type" => match p.value.to_lowercase().as_str() {
                "received" => dir = Direction::Incoming,
                "dialed" => dir = Direction::Outgoing,
                _ => {}
            },
            "time" => start = parse_time(&p.value),
            _ => {}
        }
    }
    if let Some(d) = dir.label() {
        r.text("KAAMOS_DIRECTION", d);
    }
    if let Some(c) = &caller {
        r.text("KAAMOS_PHONE_NUMBER_FROM", c);
        r.phones.push(c.clone());
    }
    if !callees.is_empty() {
        r.text("KAAMOS_PHONE_NUMBER_TO", &callees.join(", "));
        r.phones.extend(callees);
    }
    if let Some(t) = start.filter(|&t| t > 0) {
        r.attrs.push(("KAAMOS_DATETIME_START", Value::DateTime(t)));
    }
    r.relationship = Some((RelationshipKind::CallLog, start));
    (!r.attrs.is_empty()).then_some(r)
}

/// `XRYMessagesFileParser`, with segments of one reference number joined.
fn messages(ents: &[Vec<&str>]) -> Vec<Record> {
    const NS: &[&str] = &["from", "to", "participant"];
    let reference = |ps: &[Pair]| {
        ps.iter()
            .find(|p| p.key == "reference number")
            .map(|p| p.value.clone())
    };
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(e) = ents.get(i) {
        i += 1;
        let mut ps = pairs(e, NS);
        // Following entities with the same reference number are further
        // segments of the same message.
        if let Some(r) = reference(&ps) {
            while let Some(next) = ents.get(i) {
                let nps = pairs(next, NS);
                if reference(&nps).as_ref() != Some(&r) {
                    break;
                }
                i += 1;
                let more = nps
                    .iter()
                    .find(|p| p.key == "text" || p.key == "message")
                    .map(|p| p.value.clone());
                if let (Some(more), Some(text)) = (
                    more,
                    ps.iter_mut()
                        .find(|p| p.key == "text" || p.key == "message"),
                ) {
                    text.value.push(' ');
                    text.value.push_str(&more);
                }
            }
        }
        if let Some(r) = message(&ps) {
            out.push(r);
        }
    }
    out
}

fn message(ps: &[Pair]) -> Option<Record> {
    let mut r = Record::new("KAAMOS_MESSAGE");
    let mut dir = Direction::Unknown;
    let mut sender: Option<String> = None;
    let mut recipients: Vec<String> = Vec::new();
    let mut time = None;
    let mut read: Option<i64> = None;
    let mut text: Option<String> = None;
    let mut others: Vec<(&'static str, Value)> = Vec::new();
    for p in ps {
        let v = p.value.as_str();
        match p.key.as_str() {
            "tel" | "number" => {
                if !valid_phone(v) {
                    continue;
                }
                if p.namespace == "from" || dir == Direction::Incoming {
                    sender = Some(v.to_owned());
                } else if p.namespace == "to" || dir == Direction::Outgoing {
                    recipients.push(v.to_owned());
                } else {
                    r.phones.push(v.to_owned());
                    others.push(("KAAMOS_PHONE_NUMBER", Value::Text(v.to_owned())));
                }
            }
            "from" if valid_phone(v) => sender = Some(v.to_owned()),
            "to" if valid_phone(v) => recipients.push(v.to_owned()),
            "time" => time = parse_time(v),
            "type" | "direction" => match v.to_lowercase().as_str() {
                "incoming" => dir = Direction::Incoming,
                "outgoing" => dir = Direction::Outgoing,
                _ => {}
            },
            "status" => match v.to_lowercase().as_str() {
                "read" => read = Some(1),
                "unread" => read = Some(0),
                "deleted" => others.push(("KAAMOS_ISDELETED", Value::Text(v.to_owned()))),
                _ => {}
            },
            "text" | "message" => text = Some(v.to_owned()),
            "service center" if valid_phone(v) => {
                others.push(("KAAMOS_PHONE_NUMBER", Value::Text(v.to_owned())));
            }
            "name (matched)" | "name" => others.push(("KAAMOS_NAME", Value::Text(v.to_owned()))),
            "deleted" => others.push(("KAAMOS_ISDELETED", Value::Text(v.to_owned()))),
            _ => {}
        }
    }
    if sender.is_none() && recipients.is_empty() && text.is_none() && others.is_empty() {
        return None;
    }
    r.text("KAAMOS_MESSAGE_TYPE", MODULE);
    if let Some(d) = dir.label() {
        r.text("KAAMOS_DIRECTION", d);
    }
    if let Some(s) = &sender {
        r.text("KAAMOS_PHONE_NUMBER_FROM", s);
        r.phones.push(s.clone());
    }
    if !recipients.is_empty() {
        r.text("KAAMOS_PHONE_NUMBER_TO", &recipients.join(", "));
        r.phones.extend(recipients);
    }
    if let Some(t) = time.filter(|&t| t > 0) {
        r.attrs.push(("KAAMOS_DATETIME", Value::DateTime(t)));
    }
    if let Some(read) = read {
        r.attrs.push(("KAAMOS_READ_STATUS", Value::Int(read)));
    }
    if let Some(t) = &text {
        r.text("KAAMOS_TEXT", t);
    }
    r.attrs.extend(others);
    r.relationship = Some((RelationshipKind::Message, time));
    Some(r)
}

/// `XRYContactsFileParser`.
fn contact(entity: &[&str]) -> Option<Record> {
    let mut r = Record::new("KAAMOS_CONTACT");
    let (mut name, mut tel, mut mobile, mut home) = (None, None, None, None);
    let mut others: Vec<(&'static str, Value)> = Vec::new();
    let mut email = false;
    for p in pairs(entity, &[]) {
        let v = p.value;
        let first = |slot: &mut Option<String>, attr, others: &mut Vec<_>| {
            if slot.is_none() {
                *slot = Some(v.clone());
            } else {
                others.push((attr, Value::Text(v.clone())));
            }
        };
        match p.key.as_str() {
            "name" => first(&mut name, "KAAMOS_NAME", &mut others),
            "tel" if valid_phone(&v) => first(&mut tel, "KAAMOS_PHONE_NUMBER", &mut others),
            "mobile" if valid_phone(&v) => {
                first(&mut mobile, "KAAMOS_PHONE_NUMBER_MOBILE", &mut others);
            }
            "home" if valid_phone(&v) => first(&mut home, "KAAMOS_PHONE_NUMBER_HOME", &mut others),
            "email home" if is_valid_email_address(&v) => {
                email = true;
                others.push(("KAAMOS_EMAIL_HOME", Value::Text(v)));
            }
            "related application" => others.push(("KAAMOS_PROG_NAME", Value::Text(v))),
            "address home" => others.push(("KAAMOS_LOCATION", Value::Text(v))),
            "deleted" => others.push(("KAAMOS_ISDELETED", Value::Text(v))),
            _ => {}
        }
    }
    let phones: Vec<String> = [&tel, &mobile, &home]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    if let Some(n) = &name {
        r.text("KAAMOS_NAME", n);
    }
    for (attr, v) in [
        ("KAAMOS_PHONE_NUMBER", &tel),
        ("KAAMOS_PHONE_NUMBER_MOBILE", &mobile),
        ("KAAMOS_PHONE_NUMBER_HOME", &home),
    ] {
        if let Some(v) = v {
            r.text(attr, v);
        }
    }
    r.attrs.extend(others);
    if !phones.is_empty() || email {
        r.phones = phones;
        r.relationship = Some((RelationshipKind::Contact, None));
    }
    (!r.attrs.is_empty()).then_some(r)
}

/// `XRYDeviceGenInfoFileParser`: `Attribute` / `Data` pairs.
fn device_info(entity: &[&str]) -> Option<Record> {
    let mut r = Record::new("KAAMOS_DEVICE_INFO");
    let ps: Vec<Pair> = pairs(entity, &[])
        .into_iter()
        .filter(|p| p.key == "attribute" || p.key == "data")
        .collect();
    for chunk in ps.chunks(2) {
        match chunk {
            [a, b] => {
                let (attr, data) = match (a.key.as_str(), b.key.as_str()) {
                    ("attribute", "data") => (a, b),
                    ("data", "attribute") => (b, a),
                    _ => continue,
                };
                let ty = match attr.value.to_lowercase().as_str() {
                    "device name" => "KAAMOS_DEVICE_NAME",
                    "device type" => "KAAMOS_DEVICE_MAKE",
                    "mobile id (imei)" | "imei/meid" => "KAAMOS_IMEI",
                    "security code" | "unlock code" => "KAAMOS_PASSWORD",
                    "model" => "KAAMOS_DEVICE_MODEL",
                    "wifi address" => "KAAMOS_MAC_ADDRESS",
                    "subscriber id (imsi)" => "KAAMOS_IMSI",
                    _ => continue,
                };
                r.text(ty, &data.value);
            }
            [single] if single.key == "data" => r.text("KAAMOS_PATH", &single.value),
            _ => {}
        }
    }
    (!r.attrs.is_empty()).then_some(r)
}

/// `XRYWebBookmarksFileParser`.
fn bookmark(entity: &[&str]) -> Option<Record> {
    let mut r = Record::new("KAAMOS_WEB_BOOKMARK");
    for p in pairs(entity, &[]) {
        match p.key.as_str() {
            "web address" => r.text("KAAMOS_URL", &p.value),
            "domain" => r.text("KAAMOS_DOMAIN", &p.value),
            "application" => r.text("KAAMOS_PROG_NAME", &p.value),
            _ => {}
        }
    }
    (!r.attrs.is_empty()).then_some(r)
}

/// Writes a report's records on its file, with phone accounts related to
/// the device (the data source) as Autopsy's `CommunicationArtifactsHelper`
/// does.
fn write(db: &CaseDb, ds_id: i64, file_id: i64, device: &str, records: &[Record]) -> Result<()> {
    let now = jiff::Timestamp::now().as_second();
    db.bulk(|b| {
        for rec in records {
            let art = NewArtifact {
                type_id: artifact_type_id(rec.type_name).unwrap_or(0),
                ds_id,
                file_id: Some(file_id),
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: now,
                attributes: rec
                    .attrs
                    .iter()
                    .filter_map(|(name, v)| {
                        Some((attribute_type_id(name)?, v.clone(), MODULE.to_owned()))
                    })
                    .collect(),
            };
            let art_id = b.add_artifact(&art)?;
            let Some((kind, time)) = rec.relationship else {
                continue;
            };
            let mut ids = Vec::new();
            let accounts = std::iter::once(("DEVICE", device))
                .chain(rec.phones.iter().map(|p| ("PHONE", p.as_str())));
            for (type_name, id) in accounts {
                if normalize_account_id(type_name, id).is_err() {
                    continue;
                }
                let (account, _) = b.account_instance(AccountInstanceSpec {
                    type_name,
                    id,
                    module: MODULE,
                    file_id,
                    ds_id,
                    extra: Vec::new(),
                    now,
                })?;
                ids.push(account.id);
            }
            ids.sort_unstable();
            ids.dedup();
            if ids.len() > 1 {
                b.add_relationships(&ids, art_id, kind, time, ds_id)?;
            }
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes `text` as an XRY report (UTF-16LE with a byte-order mark).
    pub(crate) fn report(text: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xFE];
        for u in text.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    fn doc(kind: &str, body: &str) -> String {
        format!("XRY export\r\nPhone\r\n{kind}\r\nExported 1/1/2020\r\n\r\n{body}")
    }

    #[test]
    fn times() {
        assert_eq!(
            parse_time("1/3/1990 1:23:54 AM (Network)"),
            Some(631_329_834)
        );
        assert_eq!(
            parse_time("1/3/1990 1:23:54 PM"),
            Some(631_329_834 + 43_200)
        );
        assert_eq!(
            parse_time("1/3/1990 12:23:54 AM"),
            Some(631_329_834 - 3_600)
        );
        assert_eq!(
            parse_time("1/3/1990 1:23:54 AM UTC+1"),
            Some(631_329_834 - 3_600)
        );
        assert_eq!(
            parse_time("1/3/1990 1:23:54 AM (Device) UTC+1"),
            Some(631_329_834)
        );
        assert_eq!(parse_time("bogus"), None);
    }

    #[test]
    fn report_types_and_entities() {
        let text = doc(
            "Calls",
            "Call # 1\r\nCall Type:\tDialed\r\nTime:\t1/3/1990 1:23:54 AM (Network)\r\nTo\r\nTel:\t+358401234567\r\nName (Matched):\tBob\r\n\r\n\r\nCall # 2\r\nDirection:\tIncoming\r\nTel:\t0401111111\r\n",
        );
        assert_eq!(report_type(&text), Some(ReportKind::Calls));
        let calls = parse(ReportKind::Calls, &text);
        assert_eq!(calls.len(), 2);
        let first = calls.first().unwrap();
        assert!(first.attrs.contains(&(
            "KAAMOS_PHONE_NUMBER_TO",
            Value::Text("+358401234567".into())
        )));
        assert!(
            first
                .attrs
                .contains(&("KAAMOS_DATETIME_START", Value::DateTime(631_329_834)))
        );
        let second = calls.get(1).unwrap();
        assert!(
            second
                .attrs
                .contains(&("KAAMOS_DIRECTION", Value::Text("Incoming".into())))
        );
        assert_eq!(second.phones, ["0401111111"]);
        assert_eq!(report_type(&doc("Pictures", "")), None);
    }

    #[test]
    fn message_segments_and_continuations_join() {
        let text = doc(
            "Messages/SMS",
            "Message # 1\r\nType:\tIncoming\r\nStatus:\tRead\r\nFrom\r\nTel:\t0401111111\r\nText:\tmeet me\r\nat the pier\r\nReference Number:\t7\r\nSegment Number:\t1\r\n\r\nMessage # 2\r\nText:\ttonight\r\nReference Number:\t7\r\nSegment Number:\t2\r\n",
        );
        let msgs = parse(ReportKind::Messages, &text);
        assert_eq!(msgs.len(), 1);
        let m = msgs.first().unwrap();
        assert!(m.attrs.contains(&(
            "KAAMOS_TEXT",
            Value::Text("meet me at the pier tonight".into())
        )));
        assert!(
            m.attrs
                .contains(&("KAAMOS_PHONE_NUMBER_FROM", Value::Text("0401111111".into())))
        );
        assert!(m.attrs.contains(&("KAAMOS_READ_STATUS", Value::Int(1))));
    }

    #[test]
    fn contacts_device_info_and_bookmarks() {
        let c = parse(
            ReportKind::Contacts,
            &doc(
                "Contacts/Contacts",
                "Contact # 1\r\nName:\tAlice\r\nMobile:\t0402222222\r\nEmail Home:\talice@example.com\r\n",
            ),
        );
        let c = c.first().unwrap();
        assert!(c.attrs.contains(&(
            "KAAMOS_PHONE_NUMBER_MOBILE",
            Value::Text("0402222222".into())
        )));
        assert_eq!(c.relationship.map(|r| r.0), Some(RelationshipKind::Contact));
        let d = parse(
            ReportKind::DeviceInfo,
            &doc(
                "Device/General Information",
                "Info # 1\r\nAttribute:\tMobile Id (IMEI)\r\nData:\t356938035643809\r\nAttribute:\tModel\r\nData:\tPixel\r\nAttribute:\tDevice Clock\r\nData:\tnow\r\n",
            ),
        );
        assert_eq!(
            d.first().unwrap().attrs,
            [
                ("KAAMOS_IMEI", Value::Text("356938035643809".into())),
                ("KAAMOS_DEVICE_MODEL", Value::Text("Pixel".into()))
            ]
        );
        let w = parse(
            ReportKind::WebBookmarks,
            &doc(
                "Web/Bookmarks",
                "Bookmark # 1\r\nWeb Address:\thttps://example.com/a\r\nDomain:\texample.com\r\n",
            ),
        );
        assert_eq!(w.first().unwrap().attrs.len(), 2);
    }

    #[test]
    fn xry_files_are_recognised() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("calls.txt"), report(&doc("Calls", ""))).unwrap();
        std::fs::write(dir.path().join("plain.txt"), "XRY export\nx\nCalls\n").unwrap();
        std::fs::write(dir.path().join("photo.jpg"), b"\xFF\xD8").unwrap();
        assert_eq!(
            xry_files(dir.path()).unwrap(),
            [dir.path().join("calls.txt")]
        );
    }
}
