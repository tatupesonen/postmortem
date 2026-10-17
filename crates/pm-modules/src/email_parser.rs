//! Autopsy's Email Parser (`ThunderbirdMboxFileIngestModule`), written
//! against `pm-plugin-api` (ADR-022) like every new parser: MBOX and EML
//! messages become `KAAMOS_EMAIL_MSG` results with their attachments as
//! derived files, e-mail accounts and MESSAGE relationships; vCards become
//! `KAAMOS_CONTACT` results with phone/e-mail accounts related to the device.
//!
//! Differences from Autopsy: thread ids are derived from the thread's root
//! message (Autopsy draws random UUIDs, so re-runs would duplicate
//! messages); the vCard device account is named after the data source
//! (postmortem does not record Autopsy device ids); PST/OST is not read yet.

use std::fmt::Write as _;
use std::io::BufReader;

use pm_email::{Email, Mbox, ThreadKey, vcard};
use pm_plugin_api::{
    AccountId, Attribute, Communication, DerivedFile, FileFilter, FileInfo, FileIngest, Host,
    HostReader, Level, Message, NewResult, Plugin, PluginInfo, RelationshipKind, Result, Value,
    ValueKind,
};

pub const ID: &str = "email_parser";
pub const NAME: &str = "Email Parser";

/// Autopsy's custom attribute for messages with attachments.
const HAS_ATTACHMENT: &str = "EMAIL_HAS_ATTACHMENT";

/// The largest EML or vCard file read whole.
const MAX_SINGLE: u64 = 64 * 1024 * 1024;

/// The module (enabled by default, as in Autopsy).
#[derive(Debug, Default)]
pub struct EmailParser;

impl Plugin for EmailParser {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: ID.into(),
            name: NAME.into(),
            description: "This module detects and parses mbox and pst/ost files and populates email artifacts in the blackboard.".into(),
            version: "1.0".into(),
            enabled_by_default: true,
            // Mailboxes are recognised by content and extension below.
            file_filter: Some(FileFilter::all_files()),
            data_source_stage: None,
        }
    }

    fn file_ingest(&self, _settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> {
        Ok(Some(Box::new(Worker { defined: false })))
    }
}

struct Worker {
    defined: bool,
}

/// What a file is, from its first bytes, MIME type and extension
/// (Autopsy's `isValidMimeTypeMbox` / `isEMLFile` / `isVcardFile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mbox,
    Eml,
    Vcard,
}

fn kind(file: &FileInfo, head: &[u8]) -> Option<Kind> {
    if file.size <= 64 {
        return None;
    }
    if pm_email::looks_like_mbox(head)
        && file
            .mime
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("application/mbox"))
    {
        return Some(Kind::Mbox);
    }
    if file.extension == "eml" && head.contains(&b':') {
        return Some(Kind::Eml);
    }
    if file.extension == "vcf" && vcard::is_vcard(head) {
        return Some(Kind::Vcard);
    }
    None
}

/// Autopsy's mail folder for `KAAMOS_PATH`: the part of the parent path below
/// Thunderbird's `/Mail/` or `/ImapMail/`, plus the mailbox name, without
/// `.sbd`.
fn mbox_folder(file: &FileInfo) -> String {
    let parent = &file.parent_path;
    let mut folder = if let Some(i) = parent.find("/Mail/") {
        parent.get(i + 5..).unwrap_or_default().to_owned()
    } else if let Some(i) = parent.find("/ImapMail/") {
        parent.get(i + 9..).unwrap_or_default().to_owned()
    } else {
        String::new()
    };
    folder.push_str(&file.name);
    folder.replace(".sbd", "")
}

impl FileIngest for Worker {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> Result<()> {
        let mut head = [0u8; 64];
        let n = host.read(file.id, 0, &mut head)?;
        let Some(kind) = kind(file, head.get(..n).unwrap_or_default()) else {
            return Ok(());
        };
        if !self.defined {
            host.define_attribute_type(HAS_ATTACHMENT, "Has Attachments", ValueKind::String)?;
            self.defined = true;
        }
        match kind {
            Kind::Mbox => mbox(host, file),
            Kind::Eml => eml(host, file),
            Kind::Vcard => vcards(host, file),
        }
    }
}

fn mbox(host: &dyn Host, file: &FileInfo) -> Result<()> {
    let folder = mbox_folder(file);
    // Pass 1: what threading needs; pass 2: the messages.
    let mut keys = Vec::new();
    for m in Mbox::new(BufReader::new(HostReader::new(host, file))) {
        if host.is_cancelled() {
            return Ok(());
        }
        let key = m?
            .and_then(|raw| pm_email::parse_message(&raw))
            .map(|e| ThreadKey {
                message_id: e.message_id,
                in_reply_to: e.in_reply_to,
                references: e.references,
            });
        keys.push(key.unwrap_or_default());
    }
    let threads = pm_email::thread_ids(&keys);
    let mut skipped = 0;
    for (i, m) in Mbox::new(BufReader::new(HostReader::new(host, file))).enumerate() {
        if host.is_cancelled() {
            return Ok(());
        }
        match m?.and_then(|raw| pm_email::parse_message(&raw)) {
            Some(e) => post_email(host, file, &e, &folder, threads.get(i).map(String::as_str))?,
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        host.message(Message {
            level: Level::Warn,
            subject: format!(
                "{NAME}: {skipped} message(s) could not be read in {}",
                file.name
            ),
            details: format!(
                "Messages larger than {} bytes or unparsable are skipped.",
                pm_email::MAX_MESSAGE
            ),
            file_id: Some(file.id),
        });
    }
    Ok(())
}

fn eml(host: &dyn Host, file: &FileInfo) -> Result<()> {
    let raw = HostReader::new(host, file).read_all(MAX_SINGLE)?;
    let Some(e) = pm_email::parse_message(&raw) else {
        return Ok(());
    };
    let key = ThreadKey {
        message_id: e.message_id.clone(),
        in_reply_to: e.in_reply_to.clone(),
        references: e.references.clone(),
    };
    let thread = pm_email::thread_ids(&[key]);
    // Autopsy uses the EML's parent path as the folder.
    post_email(
        host,
        file,
        &e,
        &file.parent_path,
        thread.first().map(String::as_str),
    )
}

/// `addEmailArtifact` + `handleAttachments`.
fn post_email(
    host: &dyn Host,
    file: &FileInfo,
    e: &Email,
    folder: &str,
    thread: Option<&str>,
) -> Result<()> {
    let mut attachment_json = Vec::new();
    if !e.attachments.is_empty() {
        let files: Vec<DerivedFile> = e
            .attachments
            .iter()
            .map(|a| DerivedFile {
                name: a.name.clone(),
                data: a.data.clone(),
                modified: e.sent,
                accessed: None,
                created: None,
            })
            .collect();
        let ids = host.add_derived(file.id, files)?;
        for (a, id) in e.attachments.iter().zip(ids) {
            attachment_json.push(format!(
                "{{\"pathName\":{},\"objectID\":{id}}}",
                json_string(&format!("{}{}/{}", file.parent_path, file.name, a.name))
            ));
        }
    }
    let text = |kind: &str, v: &str| (!v.is_empty()).then(|| Attribute::text(kind, v));
    let mut attrs: Vec<Attribute> = [
        text("KAAMOS_HEADERS", &e.headers),
        text("KAAMOS_EMAIL_FROM", &e.from),
        text("KAAMOS_EMAIL_TO", &e.to),
        text("KAAMOS_SUBJECT", &e.subject),
        e.sent
            .filter(|t| *t > 0)
            .map(|t| Attribute::time("KAAMOS_DATETIME_RCVD", t)),
        e.sent
            .filter(|t| *t > 0)
            .map(|t| Attribute::time("KAAMOS_DATETIME_SENT", t)),
        text("KAAMOS_EMAIL_CONTENT_PLAIN", &e.text),
        Some(Attribute::text("KAAMOS_MSG_ID", file.id.to_string())),
        (!e.attachments.is_empty()).then(|| Attribute::text(HAS_ATTACHMENT, "Yes")),
        text("KAAMOS_PATH", folder),
        text("KAAMOS_EMAIL_CC", &e.cc),
        text("KAAMOS_EMAIL_CONTENT_HTML", &e.html),
        thread.map(|t| Attribute::text("KAAMOS_THREAD_ID", t)),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !attachment_json.is_empty() {
        attrs.push(Attribute::new(
            "KAAMOS_ATTACHMENTS",
            Value::Text(format!(
                "{{\"fileAttachments\":[{}],\"urlAttachments\":[]}}",
                attachment_json.join(",")
            )),
        ));
    }
    // Accounts: the sender when it names exactly one address, and every
    // recipient (to, cc, bcc).
    let mut accounts = Vec::new();
    let mut members = Vec::new();
    let senders = pm_email::find_addresses(&e.from);
    if let [sender] = senders.as_slice() {
        members.push(accounts.len());
        accounts.push(AccountId::new("EMAIL", sender));
    }
    for addr in [&e.to, &e.cc, &e.bcc]
        .into_iter()
        .flat_map(|s| pm_email::find_addresses(s))
    {
        members.push(accounts.len());
        accounts.push(AccountId::new("EMAIL", addr));
    }
    let mut result = NewResult::new("KAAMOS_EMAIL_MSG", Some(file.id));
    result.attributes = attrs;
    host.post_communication(
        result,
        Communication {
            accounts,
            relationship: Some((members, RelationshipKind::Message, e.sent)),
        },
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `VcardParser`: a `KAAMOS_CONTACT` per card, with phone and e-mail accounts
/// related to the device (CONTACT relationship at the file's creation
/// time); embedded photos become derived files.
fn vcards(host: &dyn Host, file: &FileInfo) -> Result<()> {
    let raw = HostReader::new(host, file).read_all(MAX_SINGLE)?;
    let device = host.data_source().name;
    for card in vcard::parse(&String::from_utf8_lossy(&raw)) {
        let mut attrs = Vec::new();
        let name = card.display_name();
        if !name.is_empty() {
            attrs.push(Attribute::text("KAAMOS_NAME", name));
        }
        let mut accounts = vec![AccountId::new("DEVICE", &device)];
        for (ty, number) in &card.phones {
            let kind = match ty.as_deref() {
                Some(t) => {
                    let custom = format!("KAAMOS_PHONE_NUMBER_{t}");
                    host.define_attribute_type(
                        &custom,
                        &format!("Phone Number ({})", capitalize(t)),
                        ValueKind::String,
                    )?;
                    custom
                }
                None => "KAAMOS_PHONE_NUMBER".to_owned(),
            };
            attrs.push(Attribute::text(kind, number));
            accounts.push(AccountId::new("PHONE", number));
        }
        for (ty, addr) in &card.emails {
            let kind = match ty.as_deref() {
                Some(t) => {
                    let custom = format!("KAAMOS_EMAIL_{t}");
                    host.define_attribute_type(
                        &custom,
                        &format!("Email ({})", capitalize(t)),
                        ValueKind::String,
                    )?;
                    custom
                }
                None => "KAAMOS_EMAIL".to_owned(),
            };
            attrs.push(Attribute::text(kind, addr));
            accounts.push(AccountId::new("EMAIL", addr));
        }
        for url in &card.urls {
            attrs.push(Attribute::text("KAAMOS_URL", url));
        }
        if let Some(org) = card.organizations.first() {
            attrs.push(Attribute::text("KAAMOS_ORGANIZATION", org));
        }
        if !card.photos.is_empty() {
            let photos = card
                .photos
                .iter()
                .enumerate()
                .map(|(i, (ty, data))| DerivedFile {
                    name: format!(
                        "{}-photo{}.{}",
                        file.name,
                        i + 1,
                        ty.as_deref().unwrap_or("jpg").to_lowercase()
                    ),
                    data: data.clone(),
                    modified: None,
                    accessed: None,
                    created: None,
                })
                .collect();
            host.add_derived(file.id, photos)?;
        }
        let members = (0..accounts.len()).collect();
        let mut result = NewResult::new("KAAMOS_CONTACT", Some(file.id));
        result.attributes = attrs;
        host.post_communication(
            result,
            Communication {
                accounts,
                relationship: Some((members, RelationshipKind::Contact, file.created)),
            },
        )?;
    }
    Ok(())
}

fn capitalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(parent: &str, name: &str) -> FileInfo {
        FileInfo {
            id: 1,
            ds_id: 1,
            name: name.into(),
            parent_path: parent.into(),
            extension: name.rsplit_once('.').map_or("", |(_, e)| e).to_lowercase(),
            size: 100,
            kind: pm_plugin_api::FileKind::File,
            deleted: false,
            mime: Some("application/mbox".into()),
            md5: None,
            modified: None,
            accessed: None,
            created: None,
            changed: None,
        }
    }

    #[test]
    fn thunderbird_folders_follow_autopsy() {
        assert_eq!(
            mbox_folder(&info(
                "/Users/a/AppData/Roaming/Thunderbird/Profiles/x/Mail/Local Folders/Inbox.sbd/",
                "Work"
            )),
            "/Local Folders/Inbox/Work"
        );
        assert_eq!(mbox_folder(&info("/home/a/", "mbox")), "mbox");
        assert_eq!(capitalize("CELL"), "Cell");
        assert_eq!(json_string("a\"b\\"), "\"a\\\"b\\\\\"");
    }
}
