//! Detection and line parsing of the hash-set formats Autopsy supports
//! (Autopsy's hash database formats).

use std::io::Read;
use std::path::Path;

use crate::{HashDbError, Result};

/// A hash-set file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// Autopsy SQLite hash database (`.kdb`), the native, updateable format.
    Kdb,
    /// `md5sum` output (`<md5>  <name>`) or BSD `MD5 (<name>) = <md5>`.
    Md5sum,
    /// NSRL RDS v2 `NSRLFile.txt` (either column layout).
    NsrlV2,
    /// NSRL RDS v3 SQLite (`FILE` table).
    NsrlV3,
    /// HashKeeper `.hsh` CSV.
    HashKeeper,
    /// EnCase binary `.hash`.
    EnCase,
    /// A Autopsy index (`*-md5.idx`) used on its own ("index only").
    HfindIdx,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kdb => "SQLite hash set (.kdb)",
            Self::Md5sum => "md5sum",
            Self::NsrlV2 => "NSRL RDS v2",
            Self::NsrlV3 => "NSRL RDS v3",
            Self::HashKeeper => "HashKeeper",
            Self::EnCase => "EnCase",
            Self::HfindIdx => "hfind index (.idx)",
        }
    }

    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Kdb => 0,
            Self::Md5sum => 1,
            Self::NsrlV2 => 2,
            Self::NsrlV3 => 3,
            Self::HashKeeper => 4,
            Self::EnCase => 5,
            Self::HfindIdx => 6,
        }
    }

    /// Whether lookups go through a sorted `.pmidx` index.
    pub fn needs_index(self) -> bool {
        self != Self::Kdb
    }
}

pub(crate) const ENCASE_MAGIC: &[u8; 8] = b"HASH\x0d\x0a\xff\x00";
/// First EnCase record (Autopsy `encase.c`).
pub(crate) const ENCASE_RECORDS: u64 = 1152;
/// 16-byte MD5 + 2-byte divider.
pub(crate) const ENCASE_RECORD: u64 = 18;
/// Offset of the UTF-16LE set name in an EnCase header.
pub(crate) const ENCASE_NAME: u64 = 1032;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Identifies the format of a hash-set file.
pub fn detect(path: &Path) -> Result<Format> {
    let mut f = std::fs::File::open(path).map_err(|e| HashDbError::io(path, e))?;
    let mut head = vec![0u8; 4096];
    let mut n = 0;
    while n < head.len() {
        match f.read(head.get_mut(n..).unwrap_or_default()) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(HashDbError::io(path, e)),
        }
    }
    head.truncate(n);
    if head.starts_with(ENCASE_MAGIC) {
        return Ok(Format::EnCase);
    }
    if head.starts_with(SQLITE_MAGIC) {
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let has = |t: &str| -> rusqlite::Result<bool> {
            conn.query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [t],
                |r| r.get(0),
            )
        };
        if has("hashes")? {
            return Ok(Format::Kdb);
        }
        if has("FILE")? {
            return Ok(Format::NsrlV3);
        }
        return Err(HashDbError::UnknownFormat(path.to_path_buf()));
    }
    let first = head
        .split(|&b| b == b'\n')
        .next()
        .map(|l| String::from_utf8_lossy(l).trim_end_matches('\r').to_owned())
        .unwrap_or_default();
    if first.starts_with("\"SHA-1\"") {
        return Ok(Format::NsrlV2);
    }
    if first.starts_with("\"file_id\"") {
        return Ok(Format::HashKeeper);
    }
    if let Some((h, _)) = first.split_once('|')
        && (h.len() == 32 || h.len() == 41)
        && h.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(Format::HfindIdx);
    }
    if parse_md5sum(&first).is_some() {
        return Ok(Format::Md5sum);
    }
    Err(HashDbError::UnknownFormat(path.to_path_buf()))
}

/// Parses 32 hex digits.
pub fn parse_md5_hex(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// One parsed entry of a text hash set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LineEntry {
    pub(crate) md5: [u8; 16],
    pub(crate) name: Option<String>,
    pub(crate) comment: Option<String>,
}

/// Parses a data line of a text format (`None` for headers and junk).
pub(crate) fn parse_line(format: Format, layout: NsrlLayout, line: &str) -> Option<LineEntry> {
    let line = line.trim_end_matches(['\r', '\n']);
    match format {
        Format::Md5sum => parse_md5sum(line),
        Format::NsrlV2 => {
            let f = csv_fields(line);
            let (md5_col, name_col) = match layout {
                NsrlLayout::Current => (1, 3),
                NsrlLayout::Legacy => (6, 1),
            };
            Some(LineEntry {
                md5: parse_md5_hex(f.get(md5_col)?)?,
                name: f.get(name_col).filter(|n| !n.is_empty()).cloned(),
                comment: None,
            })
        }
        Format::HashKeeper => {
            // "file_id","hashset_id","file_name","directory","hash","file_size",
            // "date_modified","time_modified","time_zone","comments",...
            let f = csv_fields(line);
            Some(LineEntry {
                md5: parse_md5_hex(f.get(4)?)?,
                name: f.get(2).filter(|n| !n.is_empty()).cloned(),
                comment: f.get(9).filter(|c| !c.is_empty()).cloned(),
            })
        }
        Format::HfindIdx => {
            let (h, _) = line.split_once('|')?;
            let md5 = parse_md5_hex(h)?;
            // The header line is all zeros.
            (md5 != [0; 16]).then_some(LineEntry {
                md5,
                ..LineEntry::default()
            })
        }
        Format::Kdb | Format::NsrlV3 | Format::EnCase => None,
    }
}

/// `md5sum` (`<md5>[ \t]+[*]<name>` or a bare MD5) and BSD
/// (`MD5 (<name>) = <md5>`) lines.
fn parse_md5sum(line: &str) -> Option<LineEntry> {
    if let Some(rest) = line.strip_prefix("MD5 (") {
        let (name, hash) = rest.rsplit_once(") = ")?;
        return Some(LineEntry {
            md5: parse_md5_hex(hash)?,
            name: Some(name.to_owned()),
            comment: None,
        });
    }
    let hash = line.get(..32)?;
    let rest = line.get(32..)?;
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let name = rest.trim_start_matches([' ', '\t']);
    let name = name.strip_prefix('*').unwrap_or(name);
    Some(LineEntry {
        md5: parse_md5_hex(hash)?,
        name: (!name.is_empty()).then(|| name.to_owned()),
        comment: None,
    })
}

/// Column layout of an NSRL RDS v2 file (Autopsy `nsrl.c` accepts both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NsrlLayout {
    /// `"SHA-1","MD5","CRC32","FileName",…`
    #[default]
    Current,
    /// `"SHA-1","FileName","FileSize","ProductCode","OpSystemCode","MD4","MD5",…`
    Legacy,
}

impl NsrlLayout {
    pub(crate) fn from_header(header: &str) -> Self {
        if csv_fields(header).get(1).is_some_and(|f| f == "FileName") {
            Self::Legacy
        } else {
            Self::Current
        }
    }
}

/// Splits a CSV line with `"`-quoted fields (`""` escapes a quote).
pub(crate) fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "d41d8cd98f00b204e9800998ecf8427e";

    #[test]
    fn md5sum_variants() {
        let e = parse_line(
            Format::Md5sum,
            NsrlLayout::Current,
            &format!("{H}  a b.txt"),
        )
        .unwrap();
        assert_eq!(e.name.as_deref(), Some("a b.txt"));
        let e = parse_line(
            Format::Md5sum,
            NsrlLayout::Current,
            &format!("{H} *bin.exe\r\n"),
        )
        .unwrap();
        assert_eq!(e.name.as_deref(), Some("bin.exe"));
        let e = parse_line(Format::Md5sum, NsrlLayout::Current, H).unwrap();
        assert_eq!(e.name, None);
        let e = parse_line(
            Format::Md5sum,
            NsrlLayout::Current,
            &format!("MD5 (x (1).jpg) = {H}"),
        )
        .unwrap();
        assert_eq!(e.name.as_deref(), Some("x (1).jpg"));
        assert!(parse_line(Format::Md5sum, NsrlLayout::Current, &format!("{H}x")).is_none());
        assert!(parse_line(Format::Md5sum, NsrlLayout::Current, "zz").is_none());
    }

    #[test]
    fn nsrl_layouts() {
        let cur = r#""SHA-1","MD5","CRC32","FileName","FileSize","ProductCode","OpSystemCode","SpecialCode""#;
        assert_eq!(NsrlLayout::from_header(cur), NsrlLayout::Current);
        let line = format!(
            r#""0000000F8527DCCAB6642252BBCFA1B8072D33EE","{}","5E0F9D6E","a,""q"".dll",4096,1,"WIN","""#,
            H.to_uppercase()
        );
        let e = parse_line(Format::NsrlV2, NsrlLayout::Current, &line).unwrap();
        assert_eq!(e.name.as_deref(), Some(r#"a,"q".dll"#));
        let old = r#""SHA-1","FileName","FileSize","ProductCode","OpSystemCode","MD4","MD5","CRC32","SpecialCode""#;
        assert_eq!(NsrlLayout::from_header(old), NsrlLayout::Legacy);
        let line = format!(r#""AB","x.sys",10,1,"W","00","{H}","1","""#);
        let e = parse_line(Format::NsrlV2, NsrlLayout::Legacy, &line).unwrap();
        assert_eq!(e.name.as_deref(), Some("x.sys"));
        // The header itself is not an entry.
        assert!(parse_line(Format::NsrlV2, NsrlLayout::Current, cur).is_none());
    }

    #[test]
    fn hashkeeper_and_idx() {
        let line = format!(r#"1,2,"evil.exe","C:\tmp","{H}",10,"","","","known bad""#);
        let e = parse_line(Format::HashKeeper, NsrlLayout::Current, &line).unwrap();
        assert_eq!(e.name.as_deref(), Some("evil.exe"));
        assert_eq!(e.comment.as_deref(), Some("known bad"));
        assert!(
            parse_line(
                Format::HfindIdx,
                NsrlLayout::Current,
                &format!("{}|db", "0".repeat(32))
            )
            .is_none()
        );
        assert!(
            parse_line(
                Format::HfindIdx,
                NsrlLayout::Current,
                &format!("{H}|0000000123")
            )
            .is_some()
        );
    }
}
