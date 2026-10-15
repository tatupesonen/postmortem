//! Internet Explorer (Autopsy `ExtractIE.java`): Favorites bookmarks,
//! cookie text files, and history from `index.dat` cache files, which are
//! parsed natively here (Autopsy runs the external `pasco` tool).

use pm_casedb::FileRecord;
use pm_ingest::{JobContext, ModuleError};

use super::{
    HistoryEntry, NAME, Visit, already_done, extract_domain, find_by_extension, find_files,
    history, read_all,
};
use crate::util::Art;

const PROGRAM: &str = "Internet Explorer";

pub(crate) fn extract(ctx: &JobContext, visits: &mut Vec<Visit>) -> Result<(), ModuleError> {
    for f in find_by_extension(ctx, "url", "Favorites")? {
        if !already_done(ctx, &f, "TSK_WEB_BOOKMARK") {
            bookmark(ctx, &f)?;
        }
    }
    for f in find_by_extension(ctx, "txt", "/Cookies/")? {
        if !already_done(ctx, &f, "TSK_WEB_COOKIE") {
            cookie(ctx, &f)?;
        }
    }
    for f in find_files(ctx, "index.dat", "")? {
        if ctx.is_cancelled() {
            break;
        }
        if !already_done(ctx, &f, "TSK_WEB_HISTORY") {
            index_dat(ctx, &f, visits)?;
        }
    }
    Ok(())
}

fn crtime(f: &FileRecord) -> Option<i64> {
    f.times.get(3).copied().flatten().map(|t| t.0)
}

fn bookmark(ctx: &JobContext, f: &FileRecord) -> Result<(), ModuleError> {
    let text = String::from_utf8_lossy(&read_all(ctx, f)?).into_owned();
    let url = text
        .lines()
        .find(|l| l.starts_with("URL"))
        .and_then(|l| l.split_once('='))
        .map_or_else(String::new, |(_, u)| u.trim().to_owned());
    Art::data(ctx, NAME, "TSK_WEB_BOOKMARK", f.id)
        .text("TSK_URL", &url)
        .text("TSK_TITLE", &f.name)
        .time("TSK_DATETIME_CREATED", crtime(f))
        .text("TSK_PROG_NAME", PROGRAM)
        .text("TSK_DOMAIN", extract_domain(&url))
        .post_unchecked(ctx);
    Ok(())
}

fn cookie(ctx: &JobContext, f: &FileRecord) -> Result<(), ModuleError> {
    let text = String::from_utf8_lossy(&read_all(ctx, f)?).into_owned();
    let values: Vec<&str> = text.split('\n').collect();
    let get = |i: usize| values.get(i).map_or("", |v| v.trim_end_matches('\r'));
    let url = get(2);
    Art::data(ctx, NAME, "TSK_WEB_COOKIE", f.id)
        .text("TSK_URL", url)
        .time("TSK_DATETIME_CREATED", crtime(f))
        .text("TSK_NAME", get(0))
        .text("TSK_VALUE", get(1))
        .text("TSK_PROG_NAME", PROGRAM)
        .text("TSK_DOMAIN", extract_domain(url))
        .post_unchecked(ctx);
    Ok(())
}

fn index_dat(ctx: &JobContext, f: &FileRecord, visits: &mut Vec<Visit>) -> Result<(), ModuleError> {
    let data = read_all(ctx, f)?;
    for rec in parse_index_dat(&data) {
        let (user, url) = split_user(&rec.location);
        let entry = HistoryEntry {
            url: &url,
            time: rec.accessed,
            referrer: None,
            title: None,
            program: PROGRAM,
            user: &user,
        };
        history(ctx, f, entry).post_unchecked(ctx);
        visits.push(Visit {
            file_id: f.id,
            url,
            time: rec.accessed,
        });
    }
    Ok(())
}

/// Autopsy's interpretation of a pasco URL column: `Visited: user@url`,
/// `:2004082220040823: user@url`, or a plain URL.
pub(crate) fn split_user(location: &str) -> (String, String) {
    let Some((left, right)) = location.split_once('@') else {
        return (String::new(), location.trim().to_owned());
    };
    if !extract_domain(left).is_empty() {
        return (String::new(), location.trim().to_owned());
    }
    let strip = |s: &str| {
        let s = s.replace("Visited:", "").replace(":Host:", "");
        remove_colon_groups(&s).trim().to_owned()
    };
    (strip(left), strip(right))
}

/// Removes `:…:` groups (Java `replaceAll("(:)(.*?)(:)", "")`).
fn remove_colon_groups(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(':') {
        let Some(len) = rest.get(start + 1..).and_then(|r| r.find(':')) else {
            break;
        };
        out.push_str(rest.get(..start).unwrap_or_default());
        rest = rest.get(start + 1 + len + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// One `URL ` record of an `index.dat` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UrlRecord {
    pub(crate) location: String,
    pub(crate) modified: Option<i64>,
    pub(crate) accessed: Option<i64>,
}

const SIGNATURE: &[u8] = b"Client UrlCache MMF Ver ";
const BLOCK: usize = 0x80;

fn u32_at(d: &[u8], o: usize) -> Option<usize> {
    let b: [u8; 4] = d.get(o..o + 4)?.try_into().ok()?;
    usize::try_from(u32::from_le_bytes(b)).ok()
}

fn filetime_at(d: &[u8], o: usize) -> Option<i64> {
    let b: [u8; 8] = d.get(o..o + 8)?.try_into().ok()?;
    let ft = i64::try_from(u64::from_le_bytes(b)).ok()?;
    (ft > 0).then(|| ft / 10_000_000 - 11_644_473_600)
}

/// Parses the URL records reachable from the hash tables (what `pasco`
/// reports by default; records only found in free blocks are not listed).
pub(crate) fn parse_index_dat(d: &[u8]) -> Vec<UrlRecord> {
    let mut out = Vec::new();
    if !d.starts_with(SIGNATURE) {
        return out;
    }
    let mut table = u32_at(d, 0x20).unwrap_or(0);
    let mut seen_tables = std::collections::HashSet::new();
    let mut seen_records = std::collections::HashSet::new();
    while table != 0 && table < d.len() && seen_tables.insert(table) && seen_tables.len() < 10_000 {
        if d.get(table..table + 4) != Some(b"HASH".as_slice()) {
            break;
        }
        let blocks = u32_at(d, table + 4).unwrap_or(0);
        let end = table
            .saturating_add(blocks.saturating_mul(BLOCK))
            .min(d.len());
        let mut e = table + 16;
        while e + 8 <= end {
            let hash = u32_at(d, e).unwrap_or(0);
            let rec = u32_at(d, e + 4).unwrap_or(0);
            e += 8;
            // Free / deleted slots.
            if hash == 0x3 || hash == 0x1 || rec == 0 || rec == 0x0BAD_F00D || rec == 0xDEAD_BEEF {
                continue;
            }
            if !rec.is_multiple_of(BLOCK) || !seen_records.insert(rec) {
                continue;
            }
            if let Some(r) = url_record(d, rec) {
                out.push(r);
            }
        }
        table = u32_at(d, table + 8).unwrap_or(0);
    }
    out
}

fn url_record(d: &[u8], at: usize) -> Option<UrlRecord> {
    if d.get(at..at + 4)? != b"URL " {
        return None;
    }
    let blocks = u32_at(d, at + 4)?;
    let end = at.checked_add(blocks.checked_mul(BLOCK)?)?.min(d.len());
    let loc_off = at.checked_add(u32_at(d, at + 0x34)?)?;
    if loc_off >= end {
        return None;
    }
    let raw = d.get(loc_off..end)?;
    let n = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let location = String::from_utf8_lossy(raw.get(..n)?).into_owned();
    if location.is_empty() {
        return None;
    }
    Some(UrlRecord {
        location,
        modified: filetime_at(d, at + 0x08),
        accessed: filetime_at(d, at + 0x10),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal index.dat with one hash table and two URL records (one
    /// referenced, one free).
    fn sample() -> Vec<u8> {
        let mut d = vec![0u8; 0x8000];
        d[..28].copy_from_slice(b"Client UrlCache MMF Ver 5.2\0");
        d[0x20..0x24].copy_from_slice(&0x5000u32.to_le_bytes());
        // HASH table at 0x5000, 1 block, no next.
        d[0x5000..0x5004].copy_from_slice(b"HASH");
        d[0x5004..0x5008].copy_from_slice(&1u32.to_le_bytes());
        // Entry 0 → URL record at 0x6000; entry 1 → free.
        d[0x5010..0x5014].copy_from_slice(&0x1234_5680u32.to_le_bytes());
        d[0x5014..0x5018].copy_from_slice(&0x6000u32.to_le_bytes());
        d[0x5018..0x501C].copy_from_slice(&3u32.to_le_bytes());
        d[0x501C..0x5020].copy_from_slice(&0x6100u32.to_le_bytes());
        for (at, url) in [
            (
                0x6000usize,
                "Visited: Mr. Evil@http://www.google.com/search?q=hacking+tools",
            ),
            (0x6100, "http://free.example.com/"),
        ] {
            d[at..at + 4].copy_from_slice(b"URL ");
            d[at + 4..at + 8].copy_from_slice(&2u32.to_le_bytes());
            // 2004-08-26 15:00:00 UTC as FILETIME.
            let ft: u64 = (1_093_532_400 + 11_644_473_600) * 10_000_000;
            d[at + 0x10..at + 0x18].copy_from_slice(&ft.to_le_bytes());
            d[at + 0x34..at + 0x38].copy_from_slice(&0x68u32.to_le_bytes());
            d[at + 0x68..at + 0x68 + url.len()].copy_from_slice(url.as_bytes());
        }
        d
    }

    #[test]
    fn parses_referenced_url_records() {
        let recs = parse_index_dat(&sample());
        assert_eq!(recs.len(), 1);
        assert_eq!(
            recs[0].location,
            "Visited: Mr. Evil@http://www.google.com/search?q=hacking+tools"
        );
        assert_eq!(recs[0].accessed, Some(1_093_532_400));
        assert_eq!(recs[0].modified, None);
        assert!(parse_index_dat(b"not an index").is_empty());
    }

    #[test]
    fn user_and_url_split_like_autopsy() {
        assert_eq!(
            split_user("Visited: Mr. Evil@http://www.google.com/"),
            ("Mr. Evil".into(), "http://www.google.com/".into())
        );
        assert_eq!(
            split_user(":2004082220040823: Mr. Evil@http://www.yahoo.com/"),
            ("Mr. Evil".into(), "http://www.yahoo.com/".into())
        );
        assert_eq!(
            split_user("Cookie:mr. evil@google.com/"),
            ("Cookie:mr. evil".into(), "google.com/".into())
        );
        assert_eq!(
            split_user("http://a.example.com/x"),
            (String::new(), "http://a.example.com/x".into())
        );
        assert_eq!(remove_colon_groups(":a: b :c:d"), " b d");
    }

    #[test]
    fn corrupt_tables_terminate() {
        let mut d = sample();
        // Hash table points at itself as "next".
        d[0x5008..0x500C].copy_from_slice(&0x5000u32.to_le_bytes());
        assert_eq!(parse_index_dat(&d).len(), 1);
        // Truncated file.
        assert!(parse_index_dat(&d[..0x5010]).is_empty());
    }
}
