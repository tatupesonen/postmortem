//! Recycle bin metadata: Vista+ `$I` files (`$Recycle.Bin\<SID>\$I…`,
//! paired with the `$R…` file holding the content) and the XP/2000
//! `INFO2` index (`RECYCLER\<SID>\INFO2`, content in `Dc<n>.<ext>`).
//!
//! Layouts follow Autopsy's `ExtractRecycleBin.parseIFile` and `RBCmd`
//! (`RecycleBin/DollarI.cs`, `RecycleBin/Info2.cs`, MIT).

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

/// Size of one INFO2 record (Unicode format, Windows 2000+).
const INFO2_RECORD: usize = 800;

/// A `$I` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DollarI {
    /// 1 = Vista–8.1, 2 = Windows 10+.
    pub version: u64,
    pub file_size: u64,
    /// Unix seconds.
    pub deleted: Option<i64>,
    /// Original full Windows path (trimmed, NULs dropped).
    pub original_path: String,
}

/// Parses a `$I` file.
pub fn parse_dollar_i(b: &[u8]) -> Option<DollarI> {
    let version = b.u64_le(0).ok()?;
    let name = match version {
        1 => utf16le_lossy(b.get(24..)?),
        2 => {
            let n = b.u32_le(24).ok()? as usize;
            utf16le_lossy(b.get(28..28 + n.checked_mul(2)?).or_else(|| b.get(28..))?)
        }
        _ => return None,
    };
    Some(DollarI {
        version,
        file_size: b.u64_le(8).ok()?,
        deleted: crate::filetime_unix(b.u64_le(16).ok()?),
        original_path: name.trim().to_owned(),
    })
}

/// The `$R` name paired with a `$I` name.
pub fn r_name(i_name: &str) -> Option<String> {
    i_name.strip_prefix("$I").map(|rest| format!("$R{rest}"))
}

/// Autopsy's `getUserIDFromPath`: the SID folder of a `$I` file's parent
/// path (`/$Recycle.Bin/S-1-5-21-…/`), or `None`.
pub fn sid_from_path(parent_path: &str) -> Option<String> {
    parent_path
        .split('/')
        .find(|c| c.starts_with("S-") || c.starts_with("s-"))
        .map(str::to_owned)
}

/// One INFO2 record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info2Record {
    /// Record number; the content file is `Dc<index><ext>`.
    pub index: u32,
    /// Drive number (0 = A:).
    pub drive: u32,
    /// Unix seconds.
    pub deleted: Option<i64>,
    /// Size on disk (cluster-rounded).
    pub size: u32,
    /// Original path (Unicode copy, else ANSI).
    pub original_path: String,
    /// The ANSI path's first byte is zeroed when the entry was restored or
    /// purged from the bin.
    pub removed: bool,
}

impl Info2Record {
    /// Name of the file holding the content (`Dc3.txt`).
    pub fn content_name(&self) -> String {
        let ext = self
            .original_path
            .rsplit('\\')
            .next()
            .and_then(|n| n.rsplit_once('.'))
            .map(|(_, e)| format!(".{e}"))
            .unwrap_or_default();
        let drive = char::from(b'a' + (self.drive.min(25) as u8));
        format!("D{drive}{}{ext}", self.index)
    }
}

/// Parses an INFO2 file (version 5 Unicode records; version 4 ANSI-only
/// records of 280 bytes are read too).
pub fn parse_info2(b: &[u8]) -> Vec<Info2Record> {
    let declared = b.u32_le(12).unwrap_or(0) as usize;
    let rec = if declared == 280 { 280 } else { INFO2_RECORD };
    let mut out = Vec::new();
    let mut at = 20usize;
    while let Some(r) = b.get(at..at + rec) {
        let ansi = r.get(..260).unwrap_or_default();
        let removed = ansi.first() == Some(&0);
        let ansi_path: String = ansi
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| char::from(c))
            .collect();
        let unicode = r.get(280..800).map(utf16le_lossy).unwrap_or_default();
        let path = if unicode.is_empty() {
            ansi_path
        } else {
            unicode
        };
        out.push(Info2Record {
            index: r.u32_le(260).unwrap_or(0),
            drive: r.u32_le(264).unwrap_or(0),
            deleted: r.u64_le(268).ok().and_then(crate::filetime_unix),
            size: r.u32_le(276).unwrap_or(0),
            original_path: path,
            removed,
        });
        at += rec;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_sids() {
        assert_eq!(r_name("$IABC123.txt").as_deref(), Some("$RABC123.txt"));
        assert_eq!(r_name("x"), None);
        assert_eq!(
            sid_from_path("/$Recycle.Bin/S-1-5-21-1-2-3-1001/").as_deref(),
            Some("S-1-5-21-1-2-3-1001")
        );
        assert_eq!(sid_from_path("/$Recycle.Bin/"), None);
        let r = Info2Record {
            index: 3,
            drive: 2,
            deleted: None,
            size: 0,
            original_path: "C:\\a\\b.TXT".into(),
            removed: false,
        };
        assert_eq!(r.content_name(), "Dc3.TXT");
    }

    #[test]
    fn junk_is_rejected() {
        assert!(parse_dollar_i(&[9; 40]).is_none());
        assert!(parse_dollar_i(&[1, 0]).is_none());
        assert!(parse_info2(&[0; 10]).is_empty());
    }
}
