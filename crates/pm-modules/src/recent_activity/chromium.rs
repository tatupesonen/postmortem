//! Chromium-family browsers (Autopsy `Chromium.java`).

use pm_casedb::FileRecord;
use pm_ingest::{JobContext, ModuleError};

use super::{
    NAME, TempDb, Visit, already_done, extract_domain, find_files, history, read_all,
    user_from_path, webkit_time,
};
use crate::util::Art;

/// Browser name → profile location (Autopsy `BROWSERS_MAP`).
const BROWSERS: &[(&str, &str)] = &[
    ("Microsoft Edge", "Microsoft/Edge/User Data"),
    ("Yandex", "YandexBrowser/User Data"),
    ("Opera", "Opera Software/Opera Stable"),
    ("SalamWeb", "SalamWeb/User Data"),
    ("UC Browser", "UCBrowser/User Data"),
    ("Brave", "BraveSoftware/Brave-Browser/User Data"),
    ("Google Chrome", "Chrome/User Data"),
];

const HISTORY: &str = "SELECT urls.url, urls.title, visits.visit_time, \
     (SELECT u2.url FROM visits v2 JOIN urls u2 ON u2.id = v2.url WHERE v2.id = visits.from_visit) AS referrer \
     FROM urls, visits WHERE urls.id = visits.url";
const COOKIES: &str = "SELECT name, value, host_key, last_access_utc, creation_utc FROM cookies";
const DOWNLOADS_V30: &str = "SELECT current_path AS full_path, url, start_time FROM downloads, downloads_url_chains WHERE downloads.id = downloads_url_chains.id";
const DOWNLOADS: &str = "SELECT full_path, url, start_time FROM downloads";
const LOGINS: &str = "SELECT origin_url, username_value, date_created, signon_realm FROM logins";
const AUTOFILL_V8X: &str = "SELECT name, value, count, date_created, date_last_used FROM autofill";
const AUTOFILL: &str = "SELECT name, value, count, date_created, NULL AS date_last_used FROM autofill, autofill_dates WHERE autofill.pair_id = autofill_dates.pair_id";

pub(crate) fn extract(ctx: &JobContext, visits: &mut Vec<Visit>) -> Result<(), ModuleError> {
    for (browser, location) in BROWSERS {
        if ctx.is_cancelled() {
            break;
        }
        for f in find_files(ctx, "History", location)? {
            if !already_done(ctx, &f, "TSK_WEB_HISTORY") {
                history_of(ctx, &f, browser, visits)?;
            }
        }
        for f in find_files(ctx, "Cookies", location)? {
            if !already_done(ctx, &f, "TSK_WEB_COOKIE") {
                cookies(ctx, &f, browser)?;
            }
        }
        for f in find_files(ctx, "History", location)? {
            if !already_done(ctx, &f, "TSK_WEB_DOWNLOAD") {
                downloads(ctx, &f, browser)?;
            }
        }
        for f in find_files(ctx, "Bookmarks", location)? {
            if !already_done(ctx, &f, "TSK_WEB_BOOKMARK") {
                bookmarks(ctx, &f, browser)?;
            }
        }
        for f in find_files(ctx, "Login Data", location)? {
            if !already_done(ctx, &f, "TSK_SERVICE_ACCOUNT") {
                logins(ctx, &f, browser)?;
            }
        }
        for f in find_files(ctx, "Web Data", location)? {
            if !already_done(ctx, &f, "TSK_WEB_FORM_AUTOFILL") {
                autofill(ctx, &f, browser)?;
            }
        }
    }
    Ok(())
}

/// Runs `sql`, calling `row` per result; a missing table is not an error
/// (older/newer schema), the caller may try another query.
fn each(db: &TempDb, sql: &str, mut row: impl FnMut(&rusqlite::Row<'_>)) -> bool {
    let Ok(mut st) = db.conn.prepare(sql) else {
        return false;
    };
    let Ok(mut rows) = st.query([]) else {
        return false;
    };
    while let Ok(Some(r)) = rows.next() {
        row(r);
    }
    true
}

fn s(r: &rusqlite::Row<'_>, c: &str) -> Option<String> {
    r.get::<_, Option<String>>(c).ok().flatten()
}

fn i(r: &rusqlite::Row<'_>, c: &str) -> Option<i64> {
    r.get::<_, Option<i64>>(c).ok().flatten()
}

fn history_of(ctx: &JobContext, f: &FileRecord, browser: &str, visits: &mut Vec<Visit>) -> Result<(), ModuleError> {
    let db = TempDb::open(ctx, f)?;
    let user = user_from_path(&f.parent_path);
    each(&db, HISTORY, |r| {
        let url = s(r, "url").unwrap_or_default();
        let time = webkit_time(i(r, "visit_time"));
        history(ctx, f, &url, time, s(r, "referrer"), s(r, "title"), browser, &user).post_unchecked(ctx);
        visits.push(Visit {
            file_id: f.id,
            url,
            time,
        });
    });
    Ok(())
}

fn cookies(ctx: &JobContext, f: &FileRecord, browser: &str) -> Result<(), ModuleError> {
    let db = TempDb::open(ctx, f)?;
    let user = user_from_path(&f.parent_path);
    each(&db, COOKIES, |r| {
        let host = s(r, "host_key").unwrap_or_default();
        let domain = host.trim_start_matches('.').to_owned();
        Art::data(ctx, NAME, "TSK_WEB_COOKIE", f.id)
            .text("TSK_URL", &host)
            .time("TSK_DATETIME_ACCESSED", webkit_time(i(r, "last_access_utc")))
            .time("TSK_DATETIME_CREATED", webkit_time(i(r, "creation_utc")))
            .text("TSK_NAME", s(r, "name").unwrap_or_default())
            .text("TSK_VALUE", s(r, "value").unwrap_or_default())
            .text("TSK_PROG_NAME", browser)
            .text("TSK_DOMAIN", domain)
            .text_opt("TSK_USER_NAME", Some(user.clone()))
            .post_unchecked(ctx);
    });
    Ok(())
}

fn downloads(ctx: &JobContext, f: &FileRecord, browser: &str) -> Result<(), ModuleError> {
    let db = TempDb::open(ctx, f)?;
    let user = user_from_path(&f.parent_path);
    let mut post = |r: &rusqlite::Row<'_>| {
        let url = s(r, "url").unwrap_or_default();
        Art::data(ctx, NAME, "TSK_WEB_DOWNLOAD", f.id)
            .text("TSK_PATH", s(r, "full_path").unwrap_or_default())
            .text("TSK_URL", &url)
            .time("TSK_DATETIME_ACCESSED", webkit_time(i(r, "start_time")))
            .text("TSK_DOMAIN", extract_domain(&url))
            .text_opt("TSK_USER_NAME", Some(user.clone()))
            .text("TSK_PROG_NAME", browser)
            .post_unchecked(ctx);
    };
    if !each(&db, DOWNLOADS_V30, &mut post) {
        each(&db, DOWNLOADS, &mut post);
    }
    Ok(())
}

fn bookmarks(ctx: &JobContext, f: &FileRecord, browser: &str) -> Result<(), ModuleError> {
    let data = read_all(ctx, f)?;
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&data) else {
        return Ok(()); // not a bookmarks file
    };
    let user = user_from_path(&f.parent_path);
    let mut stack: Vec<&serde_json::Value> = json
        .get("roots")
        .and_then(serde_json::Value::as_object)
        .map(|o| o.values().collect())
        .unwrap_or_default();
    while let Some(node) = stack.pop() {
        if let Some(children) = node.get("children").and_then(serde_json::Value::as_array) {
            stack.extend(children.iter());
        }
        if node.get("type").and_then(serde_json::Value::as_str) != Some("url") {
            continue;
        }
        let url = node.get("url").and_then(serde_json::Value::as_str).unwrap_or_default();
        let created = node
            .get("date_added")
            .and_then(serde_json::Value::as_str)
            .and_then(|d| d.parse::<i64>().ok());
        Art::data(ctx, NAME, "TSK_WEB_BOOKMARK", f.id)
            .text("TSK_URL", url)
            .text_opt(
                "TSK_TITLE",
                node.get("name").and_then(serde_json::Value::as_str).map(str::to_owned),
            )
            .time("TSK_DATETIME_CREATED", webkit_time(created))
            .text("TSK_PROG_NAME", browser)
            .text("TSK_DOMAIN", extract_domain(url))
            .text_opt("TSK_USER_NAME", Some(user.clone()))
            .post_unchecked(ctx);
    }
    Ok(())
}

fn logins(ctx: &JobContext, f: &FileRecord, browser: &str) -> Result<(), ModuleError> {
    let db = TempDb::open(ctx, f)?;
    each(&db, LOGINS, |r| {
        let url = s(r, "origin_url").unwrap_or_default();
        Art::data(ctx, NAME, "TSK_SERVICE_ACCOUNT", f.id)
            .text("TSK_URL", &url)
            .time("TSK_DATETIME_CREATED", webkit_time(i(r, "date_created")))
            .text_opt("TSK_USER_ID", s(r, "username_value"))
            .text_opt("TSK_REALM", s(r, "signon_realm"))
            .text("TSK_DOMAIN", extract_domain(&url))
            .text("TSK_PROG_NAME", browser)
            .post_unchecked(ctx);
    });
    Ok(())
}

fn autofill(ctx: &JobContext, f: &FileRecord, browser: &str) -> Result<(), ModuleError> {
    let db = TempDb::open(ctx, f)?;
    // Autofill dates are Unix seconds (not WebKit time).
    let mut post = |r: &rusqlite::Row<'_>| {
        Art::data(ctx, NAME, "TSK_WEB_FORM_AUTOFILL", f.id)
            .text("TSK_NAME", s(r, "name").unwrap_or_default())
            .text("TSK_VALUE", s(r, "value").unwrap_or_default())
            .value("TSK_COUNT", pm_casedb::Value::Int(i(r, "count").unwrap_or(0)))
            .time("TSK_DATETIME_CREATED", i(r, "date_created").filter(|t| *t > 0))
            .time("TSK_DATETIME_ACCESSED", i(r, "date_last_used").filter(|t| *t > 0))
            .text("TSK_PROG_NAME", browser)
            .post_unchecked(ctx);
    };
    if !each(&db, AUTOFILL_V8X, &mut post) {
        each(&db, AUTOFILL, &mut post);
    }
    Ok(())
}
