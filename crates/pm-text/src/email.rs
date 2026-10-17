//! RFC 822 / MIME messages (Tika's `RFC822Parser`) through `mail-parser`:
//! body parts as text (HTML parts rendered), attachments extracted one
//! level deep, header fields as metadata.

use mail_parser::{Address, MessageParser, MimeHeaders};
use pm_core::ReadAt;

use crate::util::read_all;
use crate::{Metadata, TextError, TextSink};

/// Messages are parsed in memory; larger ones are not extracted.
const MAX_EMAIL: u64 = 64 * 1024 * 1024;
/// Attachments examined at most.
const MAX_ATTACHMENTS: usize = 256;

pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink, depth: u32) -> Result<Metadata, TextError> {
    let data = read_all(src, MAX_EMAIL)?;
    extract_bytes(&data, sink, depth)
}

pub(crate) fn extract_bytes(data: &[u8], sink: &mut dyn TextSink, depth: u32) -> Result<Metadata, TextError> {
    let msg = MessageParser::default()
        .parse(data)
        .ok_or_else(|| TextError::malformed("e-mail", "no message"))?;
    for part in msg.text_bodies() {
        let Some(text) = part.text_contents() else { continue };
        if part.is_text_html() {
            crate::html::render_str(text, sink)?;
        } else {
            sink.push(text)?;
        }
        sink.push("\n")?;
    }
    for att in msg.attachments().take(MAX_ATTACHMENTS) {
        let name = att.attachment_name().unwrap_or_default().to_owned();
        if !name.is_empty() {
            sink.push(&format!("{name}\n"))?;
        }
        if let Some(inner) = att.message() {
            if depth < crate::MAX_NESTING {
                let raw = inner.raw_message().to_vec();
                crate::nested_result(extract_bytes(&raw, sink, depth + 1))?;
            }
        } else {
            crate::extract_nested(att.contents().to_vec(), &name, sink, depth)?;
        }
        sink.push("\n")?;
    }
    let mut m = Metadata::new();
    let mut put = |k: &str, v: String| {
        if !v.trim().is_empty() {
            m.insert(k.into(), v);
        }
    };
    let from = msg.from().map(addresses).unwrap_or_default();
    put("Message-From", from.clone());
    let author = msg
        .from()
        .and_then(Address::first)
        .and_then(|a| a.name().or(a.address()))
        .unwrap_or_default()
        .to_owned();
    put("Author", author);
    put("Message-To", msg.to().map(addresses).unwrap_or_default());
    put("Message-Cc", msg.cc().map(addresses).unwrap_or_default());
    put("Message-Bcc", msg.bcc().map(addresses).unwrap_or_default());
    put("Title", msg.subject().unwrap_or_default().to_owned());
    if let Some(d) = msg.date()
        && let Some(s) = crate::util::unix_iso(d.to_timestamp()) {
            put("Creation-Date", s);
        }
    if let Some(id) = msg.message_id() {
        put("Message-ID", id.to_owned());
    }
    Ok(m)
}

/// `Name <address>, …`.
fn addresses(a: &Address<'_>) -> String {
    a.iter()
        .map(|x| match (x.name(), x.address()) {
            (Some(n), Some(ad)) if n != ad => format!("{n} <{ad}>"),
            (Some(n), _) => n.to_owned(),
            (None, Some(ad)) => ad.to_owned(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    fn run(raw: &str) -> (String, Metadata) {
        let src = pm_core::io::MemSource::new(raw.as_bytes().to_vec());
        let mut sink = StringSink::default();
        let m = extract(&src, &mut sink, 0).unwrap();
        (sink.text, m)
    }

    #[test]
    fn multipart_with_html_and_attachment() {
        let raw = "From: Alice Smith <alice@example.com>\r\n\
To: bob@example.com, \"Carol\" <carol@example.com>\r\n\
Subject: =?UTF-8?B?SMOkbGzDtg==?=\r\n\
Date: Thu, 02 Jan 2020 03:04:05 +0200\r\n\
Message-ID: <id1@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: text/html; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
<p>Caf=C3=A9 &amp; tea</p>\r\n\
--b1\r\n\
Content-Type: text/plain; name=\"notes.txt\"\r\n\
Content-Disposition: attachment; filename=\"notes.txt\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
c2VjcmV0IHBsYW5z\r\n\
--b1--\r\n";
        let (t, m) = run(raw);
        assert!(t.contains("Café & tea"), "{t:?}");
        assert!(t.contains("notes.txt\nsecret plans"), "{t:?}");
        assert_eq!(m["Title"], "Hällö");
        assert_eq!(m["Message-From"], "Alice Smith <alice@example.com>");
        assert_eq!(m["Author"], "Alice Smith");
        assert_eq!(m["Message-To"], "bob@example.com, Carol <carol@example.com>");
        assert_eq!(m["Creation-Date"], "2020-01-02T01:04:05Z");
    }

    #[test]
    fn plain_message() {
        let (t, m) = run("Subject: hi\nFrom: x@y.z\n\nline one\nline two\n");
        assert!(t.contains("line one\nline two"));
        assert_eq!(m["Title"], "hi");
    }
}
