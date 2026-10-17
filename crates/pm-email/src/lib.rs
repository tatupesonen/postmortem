//! E-mail parsing for Autopsy's Email Parser module: MBOX (streamed one
//! message at a time), EML, JWZ-style threading and vCard contacts.
//!
//! Field formats follow Autopsy's `MimeJ4MessageParser` (address lists
//! as `Name <addr>; ` strings, dates in Unix seconds, the raw header
//! block). Messages larger than [`MAX_MESSAGE`] are skipped, so a hostile
//! MBOX cannot exhaust memory.

use std::collections::HashMap;
use std::io::BufRead;

use mail_parser::{Address, HeaderValue, MessageParser, MimeHeaders};

pub mod vcard;

/// The largest message parsed (bigger ones are skipped).
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// A parsed message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Email {
    /// The raw header block.
    pub headers: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// Unix seconds.
    pub sent: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub text: String,
    pub html: String,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// Mime4j's `Mailbox.toString()` for each address, each followed by `"; "`
/// (Autopsy's `getAddresses`).
fn addresses(a: Option<&Address<'_>>) -> String {
    let mut out = String::new();
    let Some(a) = a else {
        return out;
    };
    for addr in a.iter() {
        match (addr.name(), addr.address()) {
            (Some(n), Some(e)) if !n.is_empty() => {
                out.push_str(n);
                out.push_str(" <");
                out.push_str(e);
                out.push('>');
            }
            (_, Some(e)) => out.push_str(e),
            (Some(n), None) => out.push_str(n),
            (None, None) => continue,
        }
        out.push_str("; ");
    }
    out
}

fn text_list(v: &HeaderValue<'_>) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![format!("<{t}>")],
        HeaderValue::TextList(l) => l.iter().map(|t| format!("<{t}>")).collect(),
        _ => Vec::new(),
    }
}

/// The header block of a raw message (up to the first blank line).
fn header_block(raw: &[u8]) -> String {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n"))
        .unwrap_or(raw.len());
    String::from_utf8_lossy(raw.get(..end).unwrap_or(raw)).into_owned()
}

/// Parses one RFC 822 message (`None` when it is not a message).
pub fn parse_message(raw: &[u8]) -> Option<Email> {
    if raw.len() > MAX_MESSAGE {
        return None;
    }
    let msg = MessageParser::default().parse(raw)?;
    let attachments = msg
        .attachments()
        .map(|p| Attachment {
            name: p
                .attachment_name()
                .map_or_else(|| "attachment".to_owned(), str::to_owned),
            data: p.contents().to_vec(),
        })
        .collect();
    Some(Email {
        headers: header_block(raw),
        from: addresses(msg.from()),
        to: addresses(msg.to()),
        cc: addresses(msg.cc()),
        bcc: addresses(msg.bcc()),
        subject: msg.subject().unwrap_or_default().to_owned(),
        sent: msg.date().map(mail_parser::DateTime::to_timestamp),
        message_id: msg.message_id().map(|m| format!("<{m}>")),
        in_reply_to: text_list(msg.in_reply_to()).into_iter().next(),
        references: text_list(msg.references()),
        text: msg
            .body_text(0)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default(),
        html: msg
            .body_html(0)
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default(),
        attachments,
    })
}

/// Messages of an MBOX file, one at a time: a message starts at a line
/// beginning with `From ` at the start of the file or after a blank line.
#[derive(Debug)]
pub struct Mbox<R: BufRead> {
    reader: R,
    /// The `From ` line that starts the next message.
    pending: Option<Vec<u8>>,
    done: bool,
}

impl<R: BufRead> Mbox<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: None,
            done: false,
        }
    }
}

impl<R: BufRead> Iterator for Mbox<R> {
    /// The message bytes without its `From ` separator line; `None` inside
    /// when the message was larger than [`MAX_MESSAGE`] (skipped).
    type Item = std::io::Result<Option<Vec<u8>>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut msg: Vec<u8> = Vec::new();
        let mut too_big = false;
        let mut started = self.pending.take().is_some();
        let mut prev_blank = true;
        let mut line = Vec::new();
        loop {
            line.clear();
            match self.reader.read_until(b'\n', &mut line) {
                Ok(0) => {
                    self.done = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
            let is_sep = prev_blank && line.starts_with(b"From ");
            if is_sep {
                if started {
                    self.pending = Some(line.clone());
                    return Some(Ok((!too_big).then_some(msg)));
                }
                started = true;
            } else if started && !too_big {
                // mboxrd: ">From " escapes are undone.
                let body = match line.strip_prefix(b">") {
                    Some(rest) if rest.starts_with(b"From ") => rest,
                    _ => &line[..],
                };
                if msg.len() + body.len() > MAX_MESSAGE {
                    too_big = true;
                    msg = Vec::new();
                } else {
                    msg.extend_from_slice(body);
                }
            }
            prev_blank = line == b"\n" || line == b"\r\n";
        }
        if started {
            Some(Ok((!too_big).then_some(msg)))
        } else {
            None
        }
    }
}

/// Whether a file looks like an MBOX (Autopsy: starts with `From `; the
/// MIME check is the caller's).
pub fn looks_like_mbox(head: &[u8]) -> bool {
    head.starts_with(b"From ")
}

/// E-mail addresses in a string (Autopsy's `findEmailAddresess`).
pub fn find_addresses(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in s.split(|c: char| c.is_whitespace() || "<>;,\"'()[]".contains(c)) {
        let Some((local, domain)) = token.split_once('@') else {
            continue;
        };
        let ok_local = !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c));
        let ok_domain = domain.contains('.')
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && domain
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
        if ok_local && ok_domain {
            let a = token.to_lowercase();
            if !out.contains(&a) {
                out.push(a);
            }
        }
    }
    out
}

/// What threading needs from a message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadKey {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// Thread ids for messages (Autopsy's `EmailMessageThreader`, a JWZ
/// threader): messages connected through Message-ID, In-Reply-To and
/// References share a thread. Ids are derived from the thread's root
/// message id (Autopsy uses random UUIDs; deterministic ids keep re-runs
/// from duplicating results).
pub fn thread_ids(keys: &[ThreadKey]) -> Vec<String> {
    let mut sets = UnionFind::default();
    let mut ids: HashMap<&str, usize> = HashMap::new();
    let mut message_nodes = Vec::with_capacity(keys.len());
    for k in keys {
        let me = match k.message_id.as_deref() {
            Some(m) => *ids.entry(m).or_insert_with(|| sets.add()),
            // A message without an id is a thread of its own.
            None => sets.add(),
        };
        message_nodes.push(me);
        for r in k.in_reply_to.iter().chain(&k.references) {
            let other = *ids.entry(r.as_str()).or_insert_with(|| sets.add());
            sets.union(me, other);
        }
    }
    // A thread is named after the id seen first in it.
    let mut first: Vec<(usize, &str)> = ids.iter().map(|(k, v)| (*v, *k)).collect();
    first.sort_unstable();
    let mut root_name: HashMap<usize, String> = HashMap::new();
    for (n, id) in first {
        let r = sets.find(n);
        root_name.entry(r).or_insert_with(|| id.to_owned());
    }
    message_nodes
        .into_iter()
        .enumerate()
        .map(|(i, n)| {
            let r = sets.find(n);
            let seed = root_name
                .get(&r)
                .cloned()
                .unwrap_or_else(|| format!("message {i}"));
            thread_id(&seed)
        })
        .collect()
}

/// Disjoint sets over message ids (lower node wins as the root).
#[derive(Debug, Default)]
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn add(&mut self) -> usize {
        let n = self.parent.len();
        self.parent.push(n);
        n
    }

    fn find(&mut self, mut x: usize) -> usize {
        while let Some(&p) = self.parent.get(x) {
            if p == x {
                break;
            }
            let gp = self.parent.get(p).copied().unwrap_or(p);
            if let Some(slot) = self.parent.get_mut(x) {
                *slot = gp;
            }
            x = p;
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b
            && let Some(slot) = self.parent.get_mut(a.max(b))
        {
            *slot = a.min(b);
        }
    }
}

/// A stable, UUID-formatted id for a thread root.
fn thread_id(seed: &str) -> String {
    // FNV-1a 128 bits, formatted like a UUID.
    let mut h: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    for b in seed.bytes() {
        h ^= u128::from(b);
        h = h.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
    }
    let s = format!("{h:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSG1: &[u8] = b"From: Alice Example <alice@example.com>\r\nTo: bob@example.com, Carol <carol@example.org>\r\nCc: dave@example.net\r\nSubject: Plans\r\nDate: Thu, 4 Mar 2021 09:00:00 +0000\r\nMessage-ID: <m1@example.com>\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"plan.txt\"\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--b--\r\n";

    #[test]
    fn messages_parse_like_autopsy() {
        let e = parse_message(MSG1).unwrap();
        assert_eq!(e.from, "Alice Example <alice@example.com>; ");
        assert_eq!(e.to, "bob@example.com; Carol <carol@example.org>; ");
        assert_eq!(e.cc, "dave@example.net; ");
        assert_eq!(e.subject, "Plans");
        assert_eq!(e.sent, Some(1_614_848_400));
        assert_eq!(e.message_id.as_deref(), Some("<m1@example.com>"));
        assert!(e.headers.starts_with("From: Alice") && e.headers.contains("Message-ID"));
        assert!(!e.headers.contains("See attached"));
        assert_eq!(e.text.trim(), "See attached.");
        assert_eq!(e.attachments.len(), 1);
        assert_eq!(e.attachments[0].name, "plan.txt");
        assert_eq!(e.attachments[0].data, b"hello");
    }

    #[test]
    fn mbox_splits_on_from_lines_and_unescapes() {
        let mbox = b"From alice@example.com Thu Mar  4 09:00:00 2021\nSubject: one\n\n>From the start\nbody\n\nFrom bob@example.com Thu Mar  4 10:00:00 2021\nSubject: two\n\nbody 2\n";
        let msgs: Vec<Vec<u8>> = Mbox::new(&mbox[..]).map(|m| m.unwrap().unwrap()).collect();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], b"Subject: one\n\nFrom the start\nbody\n\n");
        assert_eq!(msgs[1], b"Subject: two\n\nbody 2\n");
        assert!(looks_like_mbox(mbox));
    }

    #[test]
    fn addresses_are_found() {
        assert_eq!(
            find_addresses("Alice <Alice@Example.com>; bob@example.com; nope@local; x"),
            ["alice@example.com", "bob@example.com"]
        );
    }

    #[test]
    fn threads_follow_references() {
        let k = |id: &str, reply: Option<&str>, refs: &[&str]| ThreadKey {
            message_id: Some(id.into()),
            in_reply_to: reply.map(Into::into),
            references: refs.iter().map(|r| (*r).to_owned()).collect(),
        };
        let keys = [
            k("<a>", None, &[]),
            k("<b>", Some("<a>"), &["<a>"]),
            k("<c>", None, &["<a>", "<b>"]),
            k("<x>", None, &[]),
        ];
        let t = thread_ids(&keys);
        assert_eq!(t[0], t[1]);
        assert_eq!(t[1], t[2]);
        assert_ne!(t[0], t[3]);
        assert_eq!(t[0].len(), 36);
        assert_eq!(thread_ids(&keys), t, "stable across runs");
    }
}
