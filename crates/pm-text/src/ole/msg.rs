//! Outlook `.msg` items ([MS-OXMSG]): header fields, the body (plain,
//! HTML or compressed RTF) and the text of attachments that postmortem
//! can extract (one level deep, as Tika recurses into embedded
//! documents).

use super::{Cfb, read_stream};
use crate::util::{cp1252, filetime_iso, le32, le64, utf16le};
use crate::{Metadata, TextError, TextSink};

/// Attachments examined at most.
const MAX_ATTACHMENTS: usize = 256;
/// Decompressed RTF body size at most.
const MAX_RTF: usize = 64 * 1024 * 1024;

pub(super) fn is_msg(cf: &Cfb<'_>) -> bool {
    cf.read_root_storage().any(|e| {
        let n = e.name();
        n.starts_with("__substg1.0_") || n == "__properties_version1.0"
    })
}

/// A string property of the storage at `dir` ("" = the message).
fn string_prop(cf: &mut Cfb<'_>, dir: &str, id: u16) -> Option<String> {
    let path = format!("{dir}/__substg1.0_{id:04X}001F");
    if let Ok(b) = read_stream(cf, &path) {
        return Some(utf16le(&b).trim_end_matches('\0').to_owned());
    }
    let path = format!("{dir}/__substg1.0_{id:04X}001E");
    read_stream(cf, &path)
        .ok()
        .map(|b| cp1252(&b).trim_end_matches('\0').to_owned())
}

fn binary_prop(cf: &mut Cfb<'_>, dir: &str, id: u16) -> Option<Vec<u8>> {
    read_stream(cf, &format!("{dir}/__substg1.0_{id:04X}0102")).ok()
}

/// A `PT_SYSTIME` property from the fixed-size property stream.
fn time_prop(props: &[u8], header: usize, tag: u32) -> Option<String> {
    let mut off = header;
    while off + 16 <= props.len() {
        if le32(props, off) == Some(tag) {
            return filetime_iso(le64(props, off + 8)?);
        }
        off += 16;
    }
    None
}

pub(super) fn extract(
    cf: &mut Cfb<'_>,
    sink: &mut dyn TextSink,
    meta: &mut Metadata,
    depth: u32,
) -> Result<(), TextError> {
    let subject = string_prop(cf, "", 0x0037).unwrap_or_default();
    let from_name = string_prop(cf, "", 0x0C1A).unwrap_or_default();
    let from_addr = string_prop(cf, "", 0x0C1F)
        .or_else(|| string_prop(cf, "", 0x5D01))
        .unwrap_or_default();
    let to = string_prop(cf, "", 0x0E04).unwrap_or_default();
    let cc = string_prop(cf, "", 0x0E03).unwrap_or_default();
    let bcc = string_prop(cf, "", 0x0E02).unwrap_or_default();
    let from = match (from_name.is_empty(), from_addr.is_empty()) {
        (false, false) if from_name != from_addr => format!("{from_name} <{from_addr}>"),
        (false, _) => from_name.clone(),
        (true, _) => from_addr.clone(),
    };
    let mut head = String::new();
    if !subject.is_empty() {
        head.push_str(&subject);
        head.push('\n');
    }
    for (label, v) in [
        ("From", &from),
        ("To", &to),
        ("Cc", &cc),
        ("Bcc", &bcc),
        ("Subject", &subject),
    ] {
        if !v.is_empty() {
            head.push_str(label);
            head.push_str(": ");
            head.push_str(v);
            head.push('\n');
        }
    }
    head.push('\n');
    sink.push(&head)?;

    // Body: plain text, else HTML, else compressed RTF.
    if let Some(body) = string_prop(cf, "", 0x1000).filter(|b| !b.trim().is_empty()) {
        sink.push(&body)?;
    } else if let Some(html) = binary_prop(cf, "", 0x1013)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .or_else(|| string_prop(cf, "", 0x1013))
    {
        crate::html::render_str(&html, sink)?;
    } else if let Some(rtf) = binary_prop(cf, "", 0x1009).and_then(|c| decompress_rtf(&c)) {
        let mut rtf_meta = Metadata::new();
        crate::rtf::extract_bytes(&rtf, sink, &mut rtf_meta)?;
    }
    sink.push("\n")?;

    // Attachments.
    let attach_dirs: Vec<String> = cf
        .read_root_storage()
        .filter(|e| e.is_storage() && e.name().starts_with("__attach_version1.0_"))
        .map(|e| format!("/{}", e.name()))
        .take(MAX_ATTACHMENTS)
        .collect();
    for dir in attach_dirs {
        let name = string_prop(cf, &dir, 0x3707)
            .or_else(|| string_prop(cf, &dir, 0x3704))
            .unwrap_or_default();
        if !name.is_empty() {
            sink.push(&format!("Attachment: {name}\n"))?;
        }
        if let Some(data) = binary_prop(cf, &dir, 0x3701) {
            crate::extract_nested(data, &name, sink, depth)?;
            sink.push("\n")?;
        }
    }

    if let Ok(props) = read_stream(cf, "/__properties_version1.0")
        && let Some(t) =
            time_prop(&props, 32, 0x0039_0040).or_else(|| time_prop(&props, 32, 0x0E06_0040))
    {
        meta.insert("Creation-Date".into(), t);
    }
    let mut put = |k: &str, v: &str| {
        if !v.is_empty() {
            meta.insert(k.into(), v.into());
        }
    };
    put("Message-From", &from);
    put("Author", &from_name);
    put("Message-To", &to);
    put("Message-Cc", &cc);
    put("Message-Bcc", &bcc);
    put("Title", &subject);
    Ok(())
}

/// [MS-OXRTFCP] decompression (`LZFu`; `MELA` is stored uncompressed).
pub(crate) fn decompress_rtf(data: &[u8]) -> Option<Vec<u8>> {
    const PREBUF: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
    let raw_size = (le32(data, 4)? as usize).min(MAX_RTF);
    let kind = le32(data, 8)?;
    let body = data.get(16..)?;
    if kind == 0x414C_454D {
        return Some(body.get(..raw_size.min(body.len()))?.to_vec());
    }
    if kind != 0x7546_5A4C {
        return None;
    }
    let mut dict = [0u8; 4096];
    dict.get_mut(..PREBUF.len())?.copy_from_slice(PREBUF);
    let mut wp = PREBUF.len();
    let mut out = Vec::with_capacity(raw_size.min(1 << 20));
    let mut i = 0usize;
    'outer: while i < body.len() && out.len() < raw_size {
        let control = *body.get(i)?;
        i += 1;
        for bit in 0..8 {
            if out.len() >= raw_size || i >= body.len() {
                break 'outer;
            }
            if control & (1 << bit) == 0 {
                let b = *body.get(i)?;
                i += 1;
                out.push(b);
                *dict.get_mut(wp)? = b;
                wp = (wp + 1) % 4096;
            } else {
                let hi = usize::from(*body.get(i)?);
                let lo = usize::from(*body.get(i + 1)?);
                i += 2;
                let offset = (hi << 4) | (lo >> 4);
                let len = (lo & 0x0F) + 2;
                if offset == wp {
                    break 'outer;
                }
                for k in 0..len {
                    let b = *dict.get((offset + k) % 4096)?;
                    out.push(b);
                    *dict.get_mut(wp)? = b;
                    wp = (wp + 1) % 4096;
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;
    use crate::ole::tests::cfb_file;

    fn u16s(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn message_fields_body_and_attachment() {
        let att = b"attached plain text\n".to_vec();
        let data = cfb_file(&[
            ("/__substg1.0_0037001F", &u16s("Quarterly numbers")),
            ("/__substg1.0_0C1A001F", &u16s("Alice")),
            ("/__substg1.0_0C1F001F", &u16s("alice@example.com")),
            ("/__substg1.0_0E04001F", &u16s("Bob")),
            ("/__substg1.0_1000001F", &u16s("See the attachment.")),
            (
                "/__attach_version1.0_#00000000/__substg1.0_3707001F",
                &u16s("notes.txt"),
            ),
            ("/__attach_version1.0_#00000000/__substg1.0_37010102", &att),
        ]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        let m = crate::ole::extract(&src, &mut sink).unwrap();
        let t = sink.text;
        assert!(
            t.starts_with("Quarterly numbers\nFrom: Alice <alice@example.com>\nTo: Bob\n"),
            "{t:?}"
        );
        assert!(t.contains("See the attachment."));
        assert!(
            t.contains("Attachment: notes.txt\nattached plain text"),
            "{t:?}"
        );
        assert_eq!(m["Title"], "Quarterly numbers");
        assert_eq!(m["Message-From"], "Alice <alice@example.com>");
    }

    #[test]
    fn lzfu_spec_example() {
        // [MS-OXRTFCP] 3.1.1: "{\rtf1\ansi\ansicpg1252\pard hello world}\r\n".
        let comp: [u8; 61] = [
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5,
            0xc7, 0xa7, 0x03, 0x00, 0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42,
            0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0,
            0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let out = decompress_rtf(&comp).unwrap();
        assert_eq!(out, b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    }
}
