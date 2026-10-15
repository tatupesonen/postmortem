//! `Amcache.hve` file inventory: executables present on (and usually run
//! on) the system with their SHA-1. New format
//! `Root\InventoryApplicationFile` (Windows 10 1709+) and old format
//! `Root\File\<volume>\<entry>` (Windows 8 – 10 1607); value meanings per
//! `AmcacheParser` (`Amcache/AmcacheNew.cs`, `AmcacheOld.cs`, MIT).

use pm_registry::Hive;

use super::{key, subkeys, text};

/// One inventoried file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AmcacheFile {
    pub path: String,
    /// SHA-1 (hex, the `0000` prefix removed).
    pub sha1: Option<String>,
    pub size: Option<u64>,
    pub product_name: Option<String>,
    pub publisher: Option<String>,
    pub version: Option<String>,
    /// PE link (compile) time, Unix seconds.
    pub link_date: Option<i64>,
    /// Key last written: first execution / install (Unix seconds).
    pub key_last_written: Option<i64>,
    /// File times recorded by the old format.
    pub file_modified: Option<i64>,
    pub file_created: Option<i64>,
}

/// All file entries, both formats.
pub fn files(h: &Hive) -> Vec<AmcacheFile> {
    let mut out = Vec::new();
    if let Some(k) = key(h, "Root\\InventoryApplicationFile") {
        for e in subkeys(&k) {
            let Some(path) = text(&e, "LowerCaseLongPath") else {
                continue;
            };
            let size = e.value("Size").ok().flatten().and_then(|v| {
                v.qword().or_else(|| {
                    let s = v.string()?;
                    match s.strip_prefix("0x") {
                        Some(h) => u64::from_str_radix(h, 16).ok(),
                        None => s.parse().ok(),
                    }
                })
            });
            out.push(AmcacheFile {
                path,
                sha1: text(&e, "FileId").map(|s| strip_sha1(&s)),
                size,
                product_name: text(&e, "ProductName"),
                publisher: text(&e, "Publisher"),
                version: text(&e, "Version").or_else(|| text(&e, "BinFileVersion")),
                link_date: text(&e, "LinkDate").and_then(|d| parse_link_date(&d)),
                key_last_written: e.last_written_unix(),
                ..AmcacheFile::default()
            });
        }
    }
    if let Some(k) = key(h, "Root\\File") {
        for vol in subkeys(&k) {
            for e in subkeys(&vol) {
                let Some(path) = text(&e, "15") else { continue };
                let q = |n: &str| e.value(n).ok().flatten().and_then(|v| v.qword());
                out.push(AmcacheFile {
                    path,
                    sha1: text(&e, "101").map(|s| strip_sha1(&s)),
                    size: q("6"),
                    product_name: text(&e, "0"),
                    publisher: text(&e, "1"),
                    version: text(&e, "5"),
                    link_date: e.dword_value("f").map(i64::from).filter(|&t| t > 0),
                    key_last_written: e.last_written_unix(),
                    file_modified: q("11").and_then(crate::filetime_unix),
                    file_created: q("12").and_then(crate::filetime_unix),
                });
            }
        }
    }
    out
}

fn strip_sha1(s: &str) -> String {
    let s = s.trim();
    let s = if s.len() == 44 {
        s.get(4..).unwrap_or(s)
    } else {
        s
    };
    s.to_lowercase()
}

/// `MM/dd/yyyy HH:mm:ss` → Unix seconds.
fn parse_link_date(s: &str) -> Option<i64> {
    let dt = jiff::civil::DateTime::strptime("%m/%d/%Y %H:%M:%S", s.trim()).ok()?;
    pm_core::time::local_to_timestamp(dt, &jiff::tz::TimeZone::UTC).map(jiff::Timestamp::as_second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            strip_sha1("0000A94A8FE5CCB19BA61C4C0873D391E987982FBBD3"),
            "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3"
        );
        assert_eq!(parse_link_date("03/04/2021 10:20:30"), Some(1_614_853_230));
        assert_eq!(parse_link_date("x"), None);
    }
}
