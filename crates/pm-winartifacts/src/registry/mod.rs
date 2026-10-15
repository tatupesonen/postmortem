//! Registry analyses, by hive.
//!
//! Each function reads the keys a RegRipper plugin used by Autopsy reads
//! (cited per item) and returns plain data. Missing keys yield empty
//! results; damaged values are skipped.

pub mod amcache;
pub mod ntuser;
pub mod sam;
pub mod shellbags;
pub mod shimcache;
pub mod software;
pub mod system;

use pm_core::Bytes;
use pm_registry::{Hive, Key};

/// A key by path, swallowing errors (damaged hives are tolerated).
pub(crate) fn key<'h>(h: &'h Hive, path: &str) -> Option<Key<'h>> {
    h.key(path).ok().flatten()
}

/// A subkey by relative path.
pub(crate) fn sub<'h>(k: &Key<'h>, path: &str) -> Option<Key<'h>> {
    k.subkey_path(path).ok().flatten()
}

/// Subkeys, or none.
pub(crate) fn subkeys<'h>(k: &Key<'h>) -> Vec<Key<'h>> {
    k.subkeys().unwrap_or_default()
}

/// A non-empty, trimmed string value.
pub(crate) fn text(k: &Key<'_>, name: &str) -> Option<String> {
    k.string_value(name)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// A string stored either as text or as UTF-16 / 8-bit binary data.
pub(crate) fn text_any(k: &Key<'_>, name: &str) -> Option<String> {
    if let Some(s) = text(k, name) {
        return Some(s);
    }
    let raw = k.binary_value(name)?;
    let s = if raw.len() >= 2 && raw.get(1) == Some(&0) {
        pm_core::decode::utf16le_lossy(&raw)
    } else {
        pm_core::decode::cstr_lossy(&raw)
    };
    Some(s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// A FILETIME stored as `REG_QWORD` or 8+ bytes of binary data.
pub(crate) fn filetime_value(k: &Key<'_>, name: &str) -> Option<i64> {
    let raw = k.binary_value(name)?;
    crate::filetime_unix(raw.u64_le(0).ok()?)
}

/// A Windows `SYSTEMTIME` (16 bytes) as Unix seconds, read as UTC.
#[allow(clippy::many_single_char_names, reason = "date fields")]
pub(crate) fn systemtime(b: &[u8]) -> Option<i64> {
    let y = b.u16_le(0).ok()?;
    let mo = b.u16_le(2).ok()?;
    let d = b.u16_le(6).ok()?;
    let h = b.u16_le(8).ok()?;
    let mi = b.u16_le(10).ok()?;
    let s = b.u16_le(12).ok()?;
    let dt = jiff::civil::DateTime::new(
        i16::try_from(y).ok()?,
        i8::try_from(mo).ok()?,
        i8::try_from(d).ok()?,
        i8::try_from(h).ok()?,
        i8::try_from(mi).ok()?,
        i8::try_from(s).ok()?,
        0,
    )
    .ok()?;
    pm_core::time::local_to_timestamp(dt, &jiff::tz::TimeZone::UTC).map(jiff::Timestamp::as_second)
}

/// Values named by a number with an optional prefix (`File1`, `Item 3`,
/// `0`), sorted numerically, as RegRipper's MRU plugins list them.
pub(crate) fn numbered_values(k: &Key<'_>, prefix: &str) -> Vec<(String, String)> {
    let mut v: Vec<(u64, String, String)> = k
        .values()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| {
            let name = v.name();
            let rest = name
                .get(..prefix.len())
                .filter(|p| p.eq_ignore_ascii_case(prefix))
                .map(|_| name.get(prefix.len()..).unwrap_or_default())?;
            let n: u64 = rest.trim().parse().ok()?;
            let data = v
                .string()
                .or_else(|| v.data().ok().map(|d| pm_core::decode::utf16le_lossy(&d)))?;
            Some((n, name, data))
        })
        .collect();
    v.sort_by_key(|(n, ..)| *n);
    v.into_iter().map(|(_, n, d)| (n, d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemtimes() {
        let mut b = Vec::new();
        for v in [2021u16, 3, 4, 4, 10, 20, 30, 0] {
            b.extend(v.to_le_bytes());
        }
        assert_eq!(systemtime(&b), Some(1_614_853_230));
        assert_eq!(systemtime(&[0; 16]), None);
        assert_eq!(systemtime(&[1, 2]), None);
    }
}
