//! Firefox stores (Autopsy `Firefox.java`): `places.sqlite` (history,
//! bookmarks, v24+ downloads), `downloads.sqlite` (pre-v24),
//! `cookies.sqlite`, `formhistory.sqlite`, `autofill-profiles.json`.
//! Times are Unix seconds (`PRTime / 1000000`, as Autopsy's SQL).

use rusqlite::Connection;
use serde_json::Value;

use crate::sqlite::{each, has_column, int, text_lossy};

/// One visit (`TSK_WEB_HISTORY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub visit_date: Option<i64>,
    /// URL of the visit this one came from.
    pub referrer: String,
}

/// Visits of `places.sqlite` (hidden places skipped).
pub fn history(conn: &Connection, mut f: impl FnMut(Visit) -> bool) -> rusqlite::Result<()> {
    const Q: &str = "SELECT moz_historyvisits.id, url, title, visit_count,(visit_date/1000000) AS visit_date,from_visit,\
         (SELECT url FROM moz_historyvisits history, moz_places places where history.id = moz_historyvisits.from_visit and history.place_id = places.id ) as ref \
         FROM moz_places, moz_historyvisits WHERE moz_places.id = moz_historyvisits.place_id AND hidden = 0";
    each(conn, Q, |r| {
        f(Visit {
            url: text_lossy(r, "url"),
            title: text_lossy(r, "title"),
            visit_date: int(r, "visit_date"),
            referrer: text_lossy(r, "ref"),
        })
    })
}

/// One bookmark (`TSK_WEB_BOOKMARK`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    /// Only when > 0.
    pub date_added: Option<i64>,
}

/// Bookmarks of `places.sqlite`.
pub fn bookmarks(conn: &Connection, mut f: impl FnMut(Bookmark) -> bool) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT fk, moz_bookmarks.title, url, (moz_bookmarks.dateAdded/1000000) AS dateAdded FROM moz_bookmarks INNER JOIN moz_places ON moz_bookmarks.fk=moz_places.id",
        |r| {
            f(Bookmark {
                url: text_lossy(r, "url"),
                title: text_lossy(r, "title"),
                date_added: int(r, "dateAdded").filter(|t| *t > 0),
            })
        },
    )
}

/// One cookie (`TSK_WEB_COOKIE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub host: String,
    pub name: String,
    pub value: String,
    pub last_accessed: Option<i64>,
    /// Only for schemas with `creationTime`.
    pub creation: Option<i64>,
}

/// Cookies of `cookies.sqlite` (with `creationTime` when the schema has it).
pub fn cookies(conn: &Connection, mut f: impl FnMut(Cookie) -> bool) -> rusqlite::Result<()> {
    let with_creation = has_column(conn, "moz_cookies", "creationTime");
    let q = if with_creation {
        "SELECT name,value,host,expiry,(lastAccessed/1000000) AS lastAccessed,(creationTime/1000000) AS creationTime FROM moz_cookies"
    } else {
        "SELECT name,value,host,expiry,(lastAccessed/1000000) AS lastAccessed FROM moz_cookies"
    };
    each(conn, q, |r| {
        f(Cookie {
            host: text_lossy(r, "host"),
            name: text_lossy(r, "name"),
            value: text_lossy(r, "value"),
            last_accessed: int(r, "lastAccessed"),
            creation: if with_creation {
                int(r, "creationTime")
            } else {
                None
            },
        })
    })
}

/// One download (`TSK_WEB_DOWNLOAD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub url: String,
    /// The local path: `file:///` removed and URL-decoded.
    pub path: String,
    pub time: Option<i64>,
}

/// Autopsy: `URLDecoder.decode(target.replaceAll("file:///", ""))`, but
/// `+` is kept (it is literal in `file:` URIs; Java turns it into a
/// space).
pub fn target_path(target: &str) -> String {
    crate::search_engines::decode(&target.replace("file:///", "").replace('+', "%2B"))
}

/// Downloads of `downloads.sqlite` (Firefox < 24).
pub fn downloads_pre24(
    conn: &Connection,
    mut f: impl FnMut(Download) -> bool,
) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT target, source,(startTime/1000000) AS startTime, maxBytes FROM moz_downloads",
        |r| {
            f(Download {
                url: text_lossy(r, "source"),
                path: target_path(&text_lossy(r, "target")),
                time: int(r, "startTime"),
            })
        },
    )
}

/// Downloads recorded as `downloads/destinationFileURI` annotations in
/// `places.sqlite` (Firefox 24+).
pub fn downloads_v24(
    conn: &Connection,
    mut f: impl FnMut(Download) -> bool,
) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT url, content AS target, (lastModified/1000000) AS lastModified  FROM moz_places, moz_annos, moz_anno_attributes  WHERE moz_places.id = moz_annos.place_id AND moz_annos.anno_attribute_id = moz_anno_attributes.id AND moz_anno_attributes.name='downloads/destinationFileURI'",
        |r| {
            f(Download {
                url: text_lossy(r, "url"),
                path: target_path(&text_lossy(r, "target")),
                time: int(r, "lastModified"),
            })
        },
    )
}

/// One form history entry (`TSK_WEB_FORM_AUTOFILL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormEntry {
    pub field: String,
    pub value: String,
    /// v64+: `(firstUsed, lastUsed, timesUsed)`.
    pub usage: Option<(i64, i64, i64)>,
}

/// Form history of `formhistory.sqlite`; the `it`/`ts` timestamp fields
/// are skipped like Autopsy.
pub fn form_history(
    conn: &Connection,
    mut f: impl FnMut(FormEntry) -> bool,
) -> rusqlite::Result<()> {
    let v64 = has_column(conn, "moz_formhistory", "timesUsed");
    let q = if v64 {
        "SELECT fieldname, value, timesUsed, firstUsed, lastUsed FROM moz_formhistory"
    } else {
        "SELECT fieldname, value FROM moz_formhistory"
    };
    each(conn, q, |r| {
        let field = text_lossy(r, "fieldname");
        if matches!(field.to_lowercase().as_str(), "it" | "ts") {
            return true;
        }
        let usage = v64.then(|| {
            (
                int(r, "firstUsed").unwrap_or(0) / 1_000_000,
                int(r, "lastUsed").unwrap_or(0) / 1_000_000,
                int(r, "timesUsed").unwrap_or(0),
            )
        });
        f(FormEntry {
            field,
            value: text_lossy(r, "value"),
            usage,
        })
    })
}

/// One `autofill-profiles.json` address (`TSK_WEB_FORM_ADDRESS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub email: String,
    pub phone: String,
    pub location: String,
    pub created: i64,
    pub last_used: i64,
    pub times_used: i64,
}

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn n(v: &Value, k: &str) -> i64 {
    match v.get(k) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Addresses of `autofill-profiles.json` (`None` if not such a document).
pub fn autofill_profiles(data: &[u8]) -> Option<Vec<Address>> {
    let json: Value = serde_json::from_slice(data).ok()?;
    let arr = json.as_object()?.get("addresses")?.as_array()?;
    Some(
        arr.iter()
            .filter(|a| a.is_object())
            .map(|a| {
                let tel = s(a, "tel");
                let cc = s(a, "tel-country-code");
                let national = s(a, "tel-national");
                let phone = if !tel.is_empty() {
                    tel
                } else if !cc.is_empty() && !national.is_empty() {
                    format!("{cc}{national}")
                } else {
                    String::new()
                };
                let location = [
                    "address-line1",
                    "address-line2",
                    "address-line3",
                    "postal-code",
                    "country",
                ]
                .iter()
                .map(|k| s(a, k))
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
                Address {
                    name: s(a, "name"),
                    email: s(a, "email"),
                    phone,
                    location,
                    created: n(a, "timeCreated") / 1000,
                    last_used: n(a, "timeLastUsed") / 1000,
                    // Gson getAsShort.
                    times_used: i64::from(n(a, "timesUsed") as i16),
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autofill_json() {
        let j = br#"{"version":1,"addresses":[
            {"name":"Jane Doe","email":"j@x.com","tel-country-code":"+1","tel-national":"5551234",
             "address-line1":"1 Main St","address-line2":"","postal-code":"12345","country":"US",
             "timeCreated":1600000000123,"timeLastUsed":1600000100999,"timesUsed":3},
            {"tel":"+35840123"}, 7]}"#;
        let a = autofill_profiles(j).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].phone, "+15551234");
        assert_eq!(a[0].location, "1 Main St, 12345, US");
        assert_eq!(a[0].created, 1_600_000_000);
        assert_eq!(a[0].times_used, 3);
        assert_eq!(a[1].phone, "+35840123");
        assert!(autofill_profiles(b"{}").is_none());
    }

    #[test]
    fn download_paths() {
        assert_eq!(
            target_path("file:///C:/Users/a/Downloads/My%20File.zip"),
            "C:/Users/a/Downloads/My File.zip"
        );
        assert_eq!(target_path("file:///tmp/a+b.txt"), "tmp/a+b.txt");
    }
}
