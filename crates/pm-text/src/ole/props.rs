//! OLE property sets (`\u{5}SummaryInformation`,
//! `\u{5}DocumentSummaryInformation`) → Tika-style metadata.

use super::{Cfb, codepage, read_stream};
use crate::Metadata;
use crate::util::{filetime_iso, le16, le32, le64, utf16le};

const VT_I2: u32 = 2;
const VT_I4: u32 = 3;
const VT_LPSTR: u32 = 0x1E;
const VT_LPWSTR: u32 = 0x1F;
const VT_FILETIME: u32 = 0x40;
/// Properties read per section at most.
const MAX_PROPS: usize = 1024;

/// Reads both summary streams (missing or broken ones are skipped).
pub(super) fn read(cf: &mut Cfb<'_>) -> Metadata {
    let mut m = Metadata::new();
    if let Ok(b) = read_stream(cf, "/\u{5}SummaryInformation") {
        for (id, v) in section(&b) {
            let key = match id {
                2 => "Title",
                3 => "Subject",
                4 => "Author",
                5 => "Keywords",
                6 => "Comments",
                7 => "Template",
                8 => "Last-Author",
                9 => "Revision-Number",
                11 => "Last-Printed",
                12 => "Creation-Date",
                13 => "Last-Save-Date",
                14 => "Page-Count",
                15 => "Word-Count",
                16 => "Character Count",
                18 => "Application-Name",
                _ => continue,
            };
            m.insert(key.into(), v);
        }
    }
    if let Ok(b) = read_stream(cf, "/\u{5}DocumentSummaryInformation") {
        for (id, v) in section(&b) {
            let key = match id {
                2 => "Category",
                14 => "Manager",
                15 => "Company",
                _ => continue,
            };
            m.insert(key.into(), v);
        }
    }
    m
}

/// Property id → display value of the first section; empty values are
/// dropped.
fn section(b: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if le16(b, 0) != Some(0xFFFE) {
        return out;
    }
    let Some(off) = le32(b, 44).map(|o| o as usize) else {
        return out;
    };
    let Some(sec) = b.get(off..) else {
        return out;
    };
    let n = le32(sec, 4).map_or(0, |n| (n as usize).min(MAX_PROPS));
    // Code page first (PID 1), so strings decode correctly.
    let mut cp = 1252u32;
    let entries: Vec<(u32, usize)> = (0..n)
        .filter_map(|i| {
            let id = le32(sec, 8 + i * 8)?;
            let po = le32(sec, 12 + i * 8)? as usize;
            Some((id, po))
        })
        .collect();
    for &(id, po) in &entries {
        if id == 1 && le32(sec, po) == Some(VT_I2) {
            cp = u32::from(le16(sec, po + 4).unwrap_or(1252));
        }
    }
    for (id, po) in entries {
        let Some(ty) = le32(sec, po) else { continue };
        let v = match ty {
            VT_LPSTR => {
                let len = le32(sec, po + 4).map_or(0, |l| l as usize);
                let Some(raw) = sec.get(po + 8..po + 8 + len.min(1 << 20)) else {
                    continue;
                };
                let raw = raw.split(|&c| c == 0).next().unwrap_or_default();
                let (s, _, _) = codepage(cp).decode(raw);
                s.into_owned()
            }
            VT_LPWSTR => {
                let len = le32(sec, po + 4).map_or(0, |l| l as usize);
                let Some(raw) = sec.get(po + 8..po + 8 + (len * 2).min(1 << 20)) else {
                    continue;
                };
                utf16le(raw).trim_end_matches('\0').to_owned()
            }
            VT_FILETIME => {
                let Some(ft) = le64(sec, po + 4) else {
                    continue;
                };
                // Edit time (PID 10) is a duration, not a date.
                if id == 10 {
                    continue;
                }
                match filetime_iso(ft) {
                    Some(s) => s,
                    None => continue,
                }
            }
            VT_I4 => le32(sec, po + 4)
                .map(|v| (v as i32).to_string())
                .unwrap_or_default(),
            VT_I2 => le16(sec, po + 4)
                .map(|v| (v as i16).to_string())
                .unwrap_or_default(),
            _ => continue,
        };
        let v = v.trim().to_owned();
        if !v.is_empty() {
            out.push((id, v));
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    /// A `SummaryInformation` stream with code page 1252 and the given
    /// `VT_LPSTR` properties.
    pub(crate) fn summary_info(props: &[(u32, &str)]) -> Vec<u8> {
        let mut values: Vec<(u32, Vec<u8>)> = vec![(1, {
            let mut v = 2u32.to_le_bytes().to_vec();
            v.extend(1252u16.to_le_bytes());
            v.extend([0, 0]);
            v
        })];
        for (id, s) in props {
            let mut v = 0x1Eu32.to_le_bytes().to_vec();
            let mut bytes = s.as_bytes().to_vec();
            bytes.push(0);
            while bytes.len() % 4 != 0 {
                bytes.push(0);
            }
            v.extend((bytes.len() as u32).to_le_bytes());
            v.extend(bytes);
            values.push((*id, v));
        }
        let header_len = 8 + values.len() * 8;
        let mut sec = Vec::new();
        let mut body: Vec<u8> = Vec::new();
        let mut index = Vec::new();
        for (id, v) in &values {
            index.extend(id.to_le_bytes());
            index.extend(((header_len + body.len()) as u32).to_le_bytes());
            body.extend(v);
        }
        sec.extend(((header_len + body.len()) as u32).to_le_bytes());
        sec.extend((values.len() as u32).to_le_bytes());
        sec.extend(index);
        sec.extend(body);
        let mut b = vec![0xFE, 0xFF, 0, 0, 5, 1, 2, 0];
        b.extend([0u8; 16]);
        b.extend(1u32.to_le_bytes());
        b.extend([
            0xE0, 0x85, 0x9F, 0xF2, 0xF9, 0x4F, 0x68, 0x10, 0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27,
            0xB3, 0xD9,
        ]);
        b.extend(48u32.to_le_bytes());
        b.extend(sec);
        b
    }

    #[test]
    fn parses_lpstr_and_ignores_garbage() {
        let b = summary_info(&[(2, "T"), (8, "Bob")]);
        let s = super::section(&b);
        assert!(s.contains(&(2, "T".into())));
        assert!(s.contains(&(8, "Bob".into())));
        assert!(super::section(&b[..30]).is_empty());
        assert!(super::section(b"").is_empty());
    }
}
