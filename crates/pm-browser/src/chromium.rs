//! Chromium-family browser stores (Autopsy `Chromium.java`): `History`
//! (visits, downloads), `Cookies`, `Login Data`, `Web Data` (autofill,
//! addresses), `Favicons`, `Bookmarks` / `Local State` / `Secure
//! Preferences` (JSON).
//!
//! The SQLite readers stream rows to a callback and pick the query by
//! schema version as Autopsy does. Times are Unix seconds.

use std::collections::HashMap;
use std::sync::OnceLock;

use rusqlite::Connection;
use serde_json::Value;

use crate::sqlite::{each, has_column, int, is_blob, text, text_lossy};
use crate::time::webkit_opt;

/// Browser display name → profile location (Autopsy `BROWSERS_MAP`, in
/// its iteration order). UC Browser's files carry suffixes (`%`).
pub const BROWSERS: &[(&str, &str)] = &[
    ("Microsoft Edge", "Microsoft/Edge/User Data"),
    ("Yandex", "YandexBrowser/User Data"),
    ("Opera", "Opera Software/Opera Stable"),
    ("SalamWeb", "SalamWeb/User Data"),
    ("UC Browser", "UCBrowser/User Data"),
    ("Brave", "BraveSoftware/Brave-Browser/User Data"),
    ("Google Chrome", "Chrome/User Data"),
];

/// The browser whose location occurs in `parent_path` (case-insensitive,
/// UC Browser's `User Data%` matches any suffix), with the byte offset
/// just after the location.
pub fn browser_for_path(parent_path: &str) -> Option<(&'static str, usize)> {
    let lower = parent_path.to_lowercase();
    for (name, loc) in BROWSERS {
        if let Some(i) = lower.find(&loc.to_lowercase()) {
            return Some((name, i + loc.len()));
        }
    }
    None
}

/// One visit (`TSK_WEB_HISTORY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub time: Option<i64>,
    pub referrer: String,
}

/// Visits of a `History` database: one row per visit with that visit's
/// own time and the URL of the visit it came from (Autopsy repeats the
/// URL's last visit time and reports the URL itself as referrer).
pub fn history(conn: &Connection, mut f: impl FnMut(Visit) -> bool) -> rusqlite::Result<()> {
    const Q: &str = "SELECT urls.url AS url, urls.title AS title, visits.visit_time AS visit_time, \
         (SELECT u2.url FROM visits v2 JOIN urls u2 ON u2.id = v2.url WHERE v2.id = visits.from_visit) AS referrer \
         FROM urls, visits WHERE urls.id = visits.url ORDER BY visits.id";
    each(conn, Q, |r| {
        f(Visit {
            url: text_lossy(r, "url"),
            title: text_lossy(r, "title"),
            time: webkit_opt(int(r, "visit_time")),
            referrer: text_lossy(r, "referrer"),
        })
    })
}

/// One download (`TSK_WEB_DOWNLOAD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub full_path: String,
    pub url: String,
    pub start_time: Option<i64>,
}

/// Downloads of a `History` database: the pre-v30 table when `downloads`
/// has a `url` column, else one row per URL-chain entry (Autopsy).
pub fn downloads(conn: &Connection, mut f: impl FnMut(Download) -> bool) -> rusqlite::Result<()> {
    let q = if has_column(conn, "downloads", "url") {
        "SELECT full_path, url, start_time, received_bytes FROM downloads"
    } else {
        "SELECT current_path AS full_path, url, start_time, received_bytes FROM downloads, downloads_url_chains WHERE downloads.id=downloads_url_chains.id"
    };
    each(conn, q, |r| {
        f(Download {
            full_path: text_lossy(r, "full_path"),
            url: text_lossy(r, "url"),
            start_time: webkit_opt(int(r, "start_time")),
        })
    })
}

/// One cookie (`TSK_WEB_COOKIE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub host_key: String,
    pub name: String,
    pub value: String,
    pub last_access: Option<i64>,
    pub creation: Option<i64>,
}

/// Cookies of a `Cookies` database (the plain `value`; encrypted values
/// are not decrypted, as in Autopsy).
pub fn cookies(conn: &Connection, mut f: impl FnMut(Cookie) -> bool) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT name, value, host_key, expires_utc,last_access_utc, creation_utc FROM cookies",
        |r| {
            f(Cookie {
                host_key: text_lossy(r, "host_key"),
                name: text_lossy(r, "name"),
                value: text_lossy(r, "value"),
                last_access: webkit_opt(int(r, "last_access_utc")),
                creation: webkit_opt(int(r, "creation_utc")),
            })
        },
    )
}

/// One saved login (`TSK_SERVICE_ACCOUNT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub origin_url: String,
    pub username: String,
    pub date_created: Option<i64>,
    pub signon_realm: String,
}

/// Saved logins of a `Login Data` database (no passwords).
pub fn logins(conn: &Connection, mut f: impl FnMut(Login) -> bool) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT origin_url, username_value, date_created, signon_realm from logins",
        |r| {
            f(Login {
                origin_url: text_lossy(r, "origin_url"),
                username: text_lossy(r, "username_value"),
                date_created: webkit_opt(int(r, "date_created")),
                signon_realm: text_lossy(r, "signon_realm"),
            })
        },
    )
}

/// One autofill entry (`TSK_WEB_FORM_AUTOFILL`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autofill {
    pub name: String,
    pub value: String,
    pub count: i64,
    /// Unix seconds already.
    pub date_created: Option<i64>,
    pub date_last_used: Option<i64>,
    /// The value was a BLOB ("The data was encrypted.").
    pub encrypted: bool,
}

/// Whether `Web Data` has the 8x schema (`autofill.date_created`).
pub fn is_web_data_v8x(conn: &Connection) -> bool {
    has_column(conn, "autofill", "date_created")
}

/// Autofill entries of a `Web Data` database.
pub fn autofill(conn: &Connection, mut f: impl FnMut(Autofill) -> bool) -> rusqlite::Result<()> {
    let v8x = is_web_data_v8x(conn);
    let q = if v8x {
        "SELECT name, value, count, date_created, date_last_used from autofill"
    } else {
        "SELECT name, value, count, date_created  FROM autofill, autofill_dates  WHERE autofill.pair_id = autofill_dates.pair_id"
    };
    each(conn, q, |r| {
        let encrypted = is_blob(r, "value");
        f(Autofill {
            name: text_lossy(r, "name"),
            value: if encrypted {
                String::new()
            } else {
                text_lossy(r, "value")
            },
            count: int(r, "count").unwrap_or(0),
            date_created: int(r, "date_created"),
            date_last_used: if v8x { int(r, "date_last_used") } else { None },
            encrypted,
        })
    })
}

/// One autofill address (`TSK_WEB_FORM_ADDRESS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub name: String,
    pub email: String,
    pub phone: String,
    /// "street, city, state, zip, country".
    pub location: String,
    pub use_date: i64,
    pub use_count: i64,
    pub date_modified: i64,
    /// A field was a BLOB.
    pub encrypted: bool,
}

/// Autofill addresses of a `Web Data` database (both Autopsy schemas).
pub fn addresses(conn: &Connection, mut f: impl FnMut(Address) -> bool) -> rusqlite::Result<()> {
    let v8x = is_web_data_v8x(conn);
    let q = if v8x {
        "SELECT first_name, middle_name, last_name, full_name, street_address, city, state, zipcode, country_code, number, email, date_modified, use_date, use_count \
         FROM autofill_profiles, autofill_profile_names, autofill_profile_emails, autofill_profile_phones \
         WHERE autofill_profiles.guid = autofill_profile_names.guid AND autofill_profiles.guid = autofill_profile_emails.guid AND autofill_profiles.guid = autofill_profile_phones.guid"
    } else {
        "SELECT first_name, middle_name, last_name, address_line_1, address_line_2, city, state, zipcode, country_code, number, email, date_modified \
         FROM autofill_profiles, autofill_profile_names, autofill_profile_emails, autofill_profile_phones \
         WHERE autofill_profiles.guid = autofill_profile_names.guid AND autofill_profiles.guid = autofill_profile_emails.guid AND autofill_profiles.guid = autofill_profile_phones.guid"
    };
    each(conn, q, |r| {
        let mut encrypted = false;
        let mut field = |c: &str| {
            if is_blob(r, c) {
                encrypted = true;
                String::new()
            } else {
                text(r, c).unwrap_or_default()
            }
        };
        let first = field("first_name");
        let middle = field("middle_name");
        let last = field("last_name");
        let email = field("email");
        let phone = field("number");
        let city = field("city");
        let state = field("state");
        let zip = field("zipcode");
        let country = field("country_code");
        let (mut full_name, street) = if v8x {
            (field("full_name"), field("street_address"))
        } else {
            let l1 = field("address_line_1");
            let l2 = field("address_line_2");
            (String::new(), format!("{l1} {l2}"))
        };
        if full_name.is_empty() {
            full_name = format!("{first} {middle} {last}");
        }
        let location = [street.as_str(), &city, &state, &zip, &country].join(", ");
        f(Address {
            name: full_name,
            email,
            phone,
            location,
            use_date: if v8x {
                int(r, "use_date").unwrap_or(0)
            } else {
                0
            },
            use_count: if v8x {
                int(r, "use_count").unwrap_or(0)
            } else {
                0
            },
            date_modified: int(r, "date_modified").unwrap_or(0),
            encrypted,
        })
    })
}

/// One favicon mapping (`TSK_FAVICON`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favicon {
    pub page_url: String,
    pub last_updated: Option<i64>,
    pub last_requested: Option<i64>,
}

/// Favicon mappings of a `Favicons` database.
pub fn favicons(conn: &Connection, mut f: impl FnMut(Favicon) -> bool) -> rusqlite::Result<()> {
    each(
        conn,
        "SELECT page_url, last_updated, last_requested FROM icon_mapping, favicon_bitmaps  WHERE icon_mapping.icon_id = favicon_bitmaps.icon_id",
        |r| {
            f(Favicon {
                page_url: text_lossy(r, "page_url"),
                last_updated: webkit_opt(int(r, "last_updated")),
                last_requested: webkit_opt(int(r, "last_requested")),
            })
        },
    )
}

/// One bookmark (`TSK_WEB_BOOKMARK`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub name: String,
    pub date_added: Option<i64>,
    /// The `roots` key the bookmark is under (`bookmark_bar`, `other`, ...).
    pub root: String,
}

/// JSON string or number as a string (Gson `getAsString`).
fn json_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn json_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// Bookmarks of a `Bookmarks` JSON file, walking folders recursively
/// (Autopsy reads only the first level under each root). `None` when the
/// file is not a bookmarks document.
pub fn bookmarks(data: &[u8]) -> Option<Vec<Bookmark>> {
    let json: Value = serde_json::from_slice(data).ok()?;
    let roots = json.get("roots")?.as_object()?;
    let mut out = Vec::new();
    for (key, root) in roots {
        let mut stack: Vec<(&Value, usize)> = root
            .get("children")
            .and_then(Value::as_array)
            .map(|c| c.iter().rev().map(|v| (v, 0)).collect())
            .unwrap_or_default();
        while let Some((node, depth)) = stack.pop() {
            if !node.is_object() {
                continue;
            }
            if let Some(children) = node.get("children").and_then(Value::as_array) {
                if depth < 64 {
                    stack.extend(children.iter().rev().map(|v| (v, depth + 1)));
                }
                continue;
            }
            out.push(Bookmark {
                url: json_str(node.get("url")),
                name: json_str(node.get("name")),
                date_added: json_i64(node.get("date_added")).and_then(|t| webkit_opt(Some(t))),
                root: key.clone(),
            });
        }
    }
    Some(out)
}

/// One profile of `Local State` (`TSK_LOCAL_STATE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The profile directory name (`Default`, `Profile 1`).
    pub dir: String,
    pub gaia_id: String,
    pub hosted_domain: String,
    pub shortcut_name: String,
    pub name: String,
    pub user_name: String,
}

/// A `Local State` file that is not a JSON object with
/// `profile.info_cache`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a Chromium Local State document")]
pub struct NotLocalState;

/// Profiles of a `Local State` file: `Ok(None)` when it has no `profile`
/// key (the browser location itself is the profile).
pub fn local_state(data: &[u8]) -> Result<Option<Vec<Profile>>, NotLocalState> {
    let json: Value = serde_json::from_slice(data).map_err(|_| NotLocalState)?;
    let obj = json.as_object().ok_or(NotLocalState)?;
    let Some(profile) = obj.get("profile") else {
        return Ok(None);
    };
    let cache = profile
        .get("info_cache")
        .and_then(Value::as_object)
        .ok_or(NotLocalState)?;
    Ok(Some(
        cache
            .iter()
            .filter_map(|(dir, p)| {
                let p = p.as_object()?;
                Some(Profile {
                    dir: dir.clone(),
                    gaia_id: json_str(p.get("gaia_id")),
                    hosted_domain: json_str(p.get("hosted_domain")),
                    shortcut_name: json_str(p.get("shortcut_name")),
                    name: json_str(p.get("name")),
                    user_name: json_str(p.get("user_name")),
                })
            })
            .collect(),
    ))
}

/// One installed extension (`TSK_CHROME_EXTENSIONS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// "Enabled", "Disabled" or "" (no `state`).
    pub flag: String,
    /// Granted API permissions joined with ", ".
    pub permissions: String,
}

/// Extensions of a `Secure Preferences` file (`extensions.settings`;
/// Opera keeps them in `extensions.opsettings`).
pub fn extensions(data: &[u8], opera: bool) -> Option<Vec<Extension>> {
    let json: Value = serde_json::from_slice(data).ok()?;
    let ext = json.get("extensions")?;
    let settings = ext
        .get(if opera { "opsettings" } else { "settings" })?
        .as_object()?;
    Some(
        settings
            .iter()
            .filter_map(|(id, e)| {
                let e = e.as_object()?;
                let flag = match e.get("state") {
                    Some(s) => {
                        if json_i64(Some(s)) == Some(1) {
                            "Enabled".to_owned()
                        } else {
                            "Disabled".to_owned()
                        }
                    }
                    None => String::new(),
                };
                let permissions = e
                    .get("active_permissions")
                    .and_then(|p| p.get("api"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter(|v| !v.is_object() && !v.is_array() && !v.is_null())
                            .map(|v| json_str(Some(v)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let manifest = e.get("manifest");
                let m = |k: &str| json_str(manifest.and_then(|m| m.get(k)));
                Some(Extension {
                    id: id.clone(),
                    name: m("name"),
                    description: m("description"),
                    version: m("version"),
                    flag,
                    permissions,
                })
            })
            .collect(),
    )
}

/// Autopsy's `malicious_chrome_extensions.csv` (randomaccess3/detections,
/// DRL 1.1): extension id → reference URL (the header row is loaded
/// harmlessly, as in Autopsy).
pub fn malicious_extensions() -> &'static HashMap<String, String> {
    static M: OnceLock<HashMap<String, String>> = OnceLock::new();
    M.get_or_init(|| {
        include_str!("../data/malicious_chrome_extensions.csv")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut it = l.split(',');
                let id = it.next()?;
                let reference = it.next()?;
                // Autopsy keeps the CSV quotes; they are dropped here.
                Some((id.to_owned(), reference.trim_matches('"').to_owned()))
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_locations() {
        assert_eq!(
            browser_for_path("/Users/a/AppData/Local/Google/Chrome/User Data/Default/"),
            Some(("Google Chrome", 46))
        );
        assert_eq!(
            browser_for_path("/x/microsoft/edge/user data/Profile 1/").map(|b| b.0),
            Some("Microsoft Edge")
        );
        assert_eq!(
            browser_for_path("/x/UCBrowser/User Data_i18n/Default/").map(|b| b.0),
            Some("UC Browser")
        );
        assert_eq!(browser_for_path("/x/Mozilla/Firefox/"), None);
    }

    #[test]
    fn json_stores() {
        let bm = br#"{"roots":{"bookmark_bar":{"children":[
            {"type":"url","name":"A","url":"http://a.com/","date_added":"13000000000000000"},
            {"type":"folder","name":"F","children":[{"type":"url","name":"B","url":"http://b.com/"}]}]},
            "other":{"children":[]}}}"#;
        let b = bookmarks(bm).unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].date_added, Some(1_355_526_400));
        assert_eq!(b[0].root, "bookmark_bar");
        assert_eq!(b[1].url, "http://b.com/");
        assert_eq!(b[1].date_added, None);
        assert!(bookmarks(b"[]").is_none());

        let ls = br#"{"profile":{"info_cache":{"Default":{"name":"Person 1","user_name":"p@x.com","gaia_id":"123"},"Profile 2":{"name":"Work"}}}}"#;
        let p = local_state(ls).unwrap().unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].dir, "Default");
        assert_eq!(p[0].user_name, "p@x.com");
        assert_eq!(local_state(br#"{"browser":{}}"#), Ok(None));
        assert!(local_state(b"nope").is_err());

        let sp = br#"{"extensions":{"settings":{
            "abc":{"state":1,"active_permissions":{"api":["tabs","storage",{"x":1}]},"manifest":{"name":"N","version":"1.0","description":"D"}},
            "def":{"state":0}}}}"#;
        let e = extensions(sp, false).unwrap();
        assert_eq!(e[0].flag, "Enabled");
        assert_eq!(e[0].permissions, "tabs, storage");
        assert_eq!(e[1].flag, "Disabled");
        assert_eq!(e[1].name, "");
        assert!(extensions(sp, true).is_none());
        let m = malicious_extensions();
        assert!(m.len() > 250, "{}", m.len());
        assert_eq!(
            m.get("cflijgpldfbmdijnkeoadcjpfgokoeck")
                .map(String::as_str),
            Some(
                "https://www.catonetworks.com/blog/threat-intelligence-feeds-and-endpoint-protection-systems-fail-to-detect-24-malicious-chrome-extensions/"
            )
        );
    }
}
