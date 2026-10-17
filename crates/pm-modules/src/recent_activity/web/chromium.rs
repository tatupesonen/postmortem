//! Chromium-family browsers (Autopsy `Chromium.java`): profiles, extensions,
//! history, bookmarks, cookies, logins, autofill + addresses, downloads,
//! favicons, then the disk cache.
//!
//! Every candidate file is attributed to the browser whose location
//! (`Chrome/User Data`, ...) its path contains and to the profile
//! directory below it; the profile's user is the `name` of its
//! `Local State` entry (Autopsy's behaviour), or `Default` / `Guest` /
//! `System` for the built-in profiles. Unlike Autopsy, profiles missing
//! from `Local State` are still read (user = directory name), each user's
//! `User Data` folder keeps its own profile names, and files are never
//! processed twice.

use std::collections::HashMap;

use pm_browser::chromium as chr;
use pm_browser::domain::{extract_domain, strip_leading_dots};
use pm_casedb::communications::AccountInstanceSpec;
use pm_casedb::{FileRecord, Significance};
use pm_ingest::{JobContext, ModuleError};

use super::{
    Browser, History, Sink, Web, WebArt, custom_type, done, done_std, push_rows, read_small,
    sql_soft, with_db,
};
use crate::recent_activity::NAME;

const WHO: &str = "Chromium Analyzer";
const ENCRYPTED_FIELD_MESSAGE: &str = "The data was encrypted.";

/// Where a file sits: browser, `User Data` folder and profile directory.
#[derive(Debug, Clone)]
struct Place {
    browser: &'static str,
    /// Lower-cased path of the browser location folder.
    root: String,
    /// Profile directory (`""` for files directly in the location).
    profile: String,
}

fn place(f: &FileRecord) -> Option<Place> {
    let (browser, end) = chr::browser_for_path(&f.parent_path)?;
    let lower = f.parent_path.to_lowercase();
    let root = lower.get(..end)?.to_owned();
    // UC Browser: `User Data%` — skip the rest of the folder name.
    let rest = f
        .parent_path
        .get(end..)
        .filter(|_| lower.len() == f.parent_path.len())
        .unwrap_or_default();
    let rest = if browser == "UC Browser" {
        rest.split_once('/').map_or("", |(_, r)| r)
    } else {
        rest.trim_start_matches('/')
    };
    let profile = rest.split('/').next().unwrap_or_default().to_owned();
    Some(Place {
        browser,
        root,
        profile,
    })
}

/// `(root, lower-cased profile dir)` → user name, from `Local State`.
type Users = HashMap<(String, String), String>;

fn user_of(users: &Users, p: &Place) -> String {
    if let Some(u) = users.get(&(p.root.clone(), p.profile.to_lowercase())) {
        return u.clone();
    }
    match p.profile.as_str() {
        "" | "Default" => "Default".to_owned(),
        "Guest Profile" => "Guest".to_owned(),
        "System Profile" => "System".to_owned(),
        other => other.to_owned(),
    }
}

/// Autopsy's per-function name filters.
fn skipped(f: &FileRecord, words: &[&str]) -> bool {
    let n = f.name.to_lowercase();
    words.iter().any(|w| n.contains(w))
}

/// Whether `f` is the store `base` (UC Browser: any suffix).
fn is_store(f: &FileRecord, p: &Place, base: &str) -> bool {
    if p.browser == "UC Browser" {
        let n = f.name.to_lowercase();
        let b = base.to_lowercase();
        if base == "Cookies" {
            return n.contains(&b);
        }
        // `-journal` / `-wal` companions are not databases of their own.
        n.starts_with(&b) && !n.ends_with("-journal") && !n.ends_with("-wal")
    } else {
        f.name.eq_ignore_ascii_case(base)
    }
}

pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    let files: Vec<(FileRecord, Place)> = web
        .files
        .iter()
        .filter_map(|f| place(f).map(|p| (f.clone(), p)))
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    web.set_found(Browser::Chromium);
    let mut sink = Sink::default();
    let users = profiles(ctx, web, &files, &mut sink)?;
    let of = |base: &str| -> Vec<&(FileRecord, Place)> {
        files.iter().filter(|(f, p)| is_store(f, p, base)).collect()
    };
    let steps: [(&str, &str); 8] = [
        ("Secure Preferences", "Extensions"),
        ("History", "History"),
        ("Bookmarks", "Bookmarks"),
        ("Cookies", "Cookies"),
        ("Login Data", "Logins"),
        ("Web Data", "Auto Fill"),
        ("History", "Downloads"),
        ("Favicons", "Favicons"),
    ];
    for (base, label) in steps {
        for (f, p) in of(base) {
            if ctx.is_cancelled() {
                return sink.flush(ctx);
            }
            let user = user_of(&users, p);
            web.progress(match label {
                "Extensions" => format!("Chrome Extensions {}", p.browser),
                "Favicons" => format!("Chrome Downloads Favicons {}", p.browser),
                l => format!("Chrome {l} Browser {}", p.browser),
            });
            match label {
                "Extensions" => extensions(ctx, f, p, &user, &mut sink)?,
                "History" => history(ctx, web, f, p, &user, &mut sink)?,
                "Bookmarks" => bookmarks(ctx, f, p, &user, &mut sink)?,
                "Cookies" => cookies(ctx, web, f, p, &user, &mut sink)?,
                "Logins" => logins(ctx, web, f, p, &user, &mut sink)?,
                "Auto Fill" => autofill(ctx, web, f, p, &user, &mut sink)?,
                "Downloads" => downloads(ctx, web, f, p, &user, &mut sink)?,
                _ => favicons(ctx, web, f, p, &user, &mut sink)?,
            }
        }
        sink.flush(ctx)?;
    }
    sink.flush(ctx)?;
    web.progress("Chrome Cache");
    super::chrome_cache::extract(ctx, web);
    Ok(())
}

/// `Local State` files → `TSK_LOCAL_STATE` artifacts and the user map.
fn profiles(
    ctx: &JobContext,
    web: &mut Web<'_>,
    files: &[(FileRecord, Place)],
    sink: &mut Sink,
) -> Result<Users, ModuleError> {
    let mut users = Users::new();
    let ty = custom_type(ctx, "TSK_LOCAL_STATE", "Chromium Profiles");
    for (f, p) in files.iter().filter(|(f, p)| is_store(f, p, "Local State")) {
        // Autopsy: allocated files only; name filters.
        if !f.meta_alloc || skipped(f, &["-slack", "cache", "media", "index"]) {
            continue;
        }
        web.progress(format!("Chrome Profiles {}", p.browser));
        let Some(data) = read_small(ctx, f) else {
            continue;
        };
        let profiles = match chr::local_state(&data) {
            Ok(Some(ps)) => ps,
            Ok(None) => continue,
            Err(_) => {
                web.error(format!(
                    "{WHO}: Error while trying to analyze file:{}",
                    f.name
                ));
                continue;
            }
        };
        let write = !done(ctx, f, ty);
        for pr in profiles {
            users.insert(
                (p.root.clone(), pr.dir.to_lowercase()),
                // Autopsy always picks `name` (`userName.contains("")`).
                pr.name.clone(),
            );
            if write {
                sink.push(
                    ctx,
                    WebArt::of_type(ctx, ty, f.id)
                        .text("TSK_PATH", &pr.dir)
                        .text("TSK_USER_ID", pr.gaia_id)
                        .text("TSK_DOMAIN", pr.hosted_domain)
                        .text("TSK_SHORTCUT", pr.shortcut_name)
                        .text("TSK_NAME", pr.name)
                        .text("TSK_USER_NAME", pr.user_name)
                        .text("TSK_PROG_NAME", p.browser),
                )?;
            }
        }
    }
    sink.flush(ctx)?;
    Ok(users)
}

fn extensions(
    ctx: &JobContext,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if !f.meta_alloc || skipped(f, &["-slack", "cache", "media", "index"]) {
        return Ok(());
    }
    let ty = custom_type(ctx, "TSK_CHROME_EXTENSIONS", "Chromium Extensions");
    if done(ctx, f, ty) {
        return Ok(());
    }
    let Some(data) = read_small(ctx, f) else {
        return Ok(());
    };
    let Some(exts) = chr::extensions(&data, p.browser == "Opera") else {
        return Ok(());
    };
    let malicious = chr::malicious_extensions();
    for e in exts {
        let mut a = WebArt::of_type(ctx, ty, f.id).text("TSK_ID", &e.id);
        if let Some(r) = malicious.get(&e.id) {
            a = a.text("TSK_COMMENT", format!("Malicious Extension Found - {r}"));
        }
        sink.push(
            ctx,
            a.text("TSK_NAME", e.name)
                .text("TSK_DESCRIPTION", e.description)
                .text("TSK_VERSION", e.version)
                .text("TSK_FLAG", e.flag)
                .text("TSK_PERMISSIONS", e.permissions)
                .text("TSK_USER_NAME", user)
                .text("TSK_PROG_NAME", p.browser),
        )?;
    }
    Ok(())
}

fn history(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if !f.meta_alloc
        || skipped(f, &["-slack", "cache", "media", "index"])
        || done_std(ctx, f, "TSK_WEB_HISTORY")
    {
        return Ok(());
    }
    with_db(ctx, web, WHO, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| chr::history(conn, cb),
            |v: chr::Visit| {
                let domain = extract_domain(&v.url);
                Some(WebArt::history(
                    ctx,
                    f.id,
                    &History {
                        url: &v.url,
                        time: v.time,
                        referrer: &v.referrer,
                        title: &v.title,
                        program: p.browser,
                        domain: &domain,
                        user,
                    },
                ))
            },
        )
    })
}

fn bookmarks(
    ctx: &JobContext,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    let bad = skipped(
        f,
        &["-slack", "extras", "log", "backup", "visualized", "bak"],
    ) || f.parent_path.to_lowercase().contains("backup");
    if bad || done_std(ctx, f, "TSK_WEB_BOOKMARK") {
        return Ok(());
    }
    let Some(data) = read_small(ctx, f) else {
        return Ok(());
    };
    let Some(list) = chr::bookmarks(&data) else {
        return Ok(());
    };
    for b in list {
        let domain = extract_domain(&b.url);
        let mut a = WebArt::new(ctx, "TSK_WEB_BOOKMARK", f.id)
            .text("TSK_URL", &b.url)
            .text("TSK_TITLE", b.name);
        a = a.time("TSK_DATETIME_CREATED", b.date_added);
        sink.push(
            ctx,
            a.text("TSK_PROG_NAME", p.browser)
                .text("TSK_DOMAIN", domain)
                .text("TSK_USER_NAME", user)
                .text("TSK_COMMENT", b.root),
        )?;
    }
    Ok(())
}

fn cookies(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if skipped(f, &["-slack"]) || done_std(ctx, f, "TSK_WEB_COOKIE") {
        return Ok(());
    }
    with_db(ctx, web, WHO, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| chr::cookies(conn, cb),
            |c: chr::Cookie| {
                Some(
                    WebArt::new(ctx, "TSK_WEB_COOKIE", f.id)
                        .text("TSK_URL", &c.host_key)
                        .time("TSK_DATETIME_ACCESSED", c.last_access)
                        .time("TSK_DATETIME_CREATED", c.creation)
                        .text("TSK_NAME", c.name)
                        .text("TSK_VALUE", c.value)
                        .text("TSK_PROG_NAME", p.browser)
                        .text("TSK_DOMAIN", strip_leading_dots(&c.host_key))
                        .text("TSK_USER_NAME", user),
                )
            },
        )
    })
}

fn logins(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if skipped(f, &["-slack"]) || done_std(ctx, f, "TSK_SERVICE_ACCOUNT") {
        return Ok(());
    }
    with_db(ctx, web, WHO, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| chr::logins(conn, cb),
            |l: chr::Login| {
                Some(
                    WebArt::new(ctx, "TSK_SERVICE_ACCOUNT", f.id)
                        .text("TSK_URL", &l.origin_url)
                        .time("TSK_DATETIME_CREATED", l.date_created)
                        .text("TSK_USER_ID", l.username)
                        .text("TSK_REALM", &l.signon_realm)
                        .text("TSK_DOMAIN", extract_domain(&l.signon_realm))
                        .text("TSK_PROG_NAME", p.browser)
                        .text("TSK_USER_NAME", user),
                )
            },
        )
    })
}

fn autofill(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if skipped(f, &["-slack"]) || done_std(ctx, f, "TSK_WEB_FORM_AUTOFILL") {
        return Ok(());
    }
    let browser = p.browser;
    let mut addresses = Vec::new();
    let mut encrypted = false;
    with_db(ctx, web, WHO, f, |conn| {
        let v8x = chr::is_web_data_v8x(conn);
        push_rows(
            ctx,
            f,
            sink,
            |cb| chr::autofill(conn, cb),
            |a: chr::Autofill| {
                encrypted |= a.encrypted;
                let mut art = WebArt::new(ctx, "TSK_WEB_FORM_AUTOFILL", f.id)
                    .text("TSK_NAME", a.name)
                    .text("TSK_VALUE", a.value)
                    .int("TSK_COUNT", a.count)
                    .time("TSK_DATETIME_CREATED", Some(a.date_created.unwrap_or(0)));
                if v8x {
                    art = art.time("TSK_DATETIME_ACCESSED", Some(a.date_last_used.unwrap_or(0)));
                }
                art = art
                    .text("TSK_USER_NAME", user)
                    .text("TSK_PROG_NAME", browser);
                if a.encrypted {
                    art = art.text("TSK_COMMENT", ENCRYPTED_FIELD_MESSAGE);
                }
                Some(art)
            },
        )?;
        sql_soft(
            f,
            chr::addresses(conn, |a| {
                addresses.push(a);
                addresses.len() < 1_000_000
            }),
        );
        Ok(())
    })?;
    sink.flush(ctx)?;
    for a in addresses {
        encrypted |= a.encrypted;
        let mut art = form_address(
            ctx,
            f,
            &a.name,
            &a.email,
            &a.phone,
            &a.location,
            0,
            a.use_date,
            a.use_count,
        );
        if a.date_modified > 0 {
            art = art.time("TSK_DATETIME_MODIFIED", Some(a.date_modified));
            if a.encrypted {
                art = art.text("TSK_COMMENT", ENCRYPTED_FIELD_MESSAGE);
            }
        }
        sink.push(ctx, art)?;
    }
    if encrypted {
        let comment = format!("{browser} Autofill Database Encryption Detected");
        sink.push(
            ctx,
            WebArt::result(
                ctx,
                "TSK_ENCRYPTION_DETECTED",
                f.id,
                Significance::Notable,
                comment.clone(),
            )
            .text("TSK_COMMENT", comment),
        )?;
    }
    Ok(())
}

/// TSK `WebBrowserArtifactsHelper.addWebFormAddress`: the
/// `TSK_WEB_FORM_ADDRESS` artifact plus EMAIL / PHONE account instances
/// on the store.
#[allow(clippy::too_many_arguments, reason = "mirrors the TSK helper")]
pub(crate) fn form_address(
    ctx: &JobContext,
    f: &FileRecord,
    name: &str,
    email: &str,
    phone: &str,
    location: &str,
    created: i64,
    accessed: i64,
    count: i64,
) -> WebArt {
    for (ty, id) in [("EMAIL", email), ("PHONE", phone)] {
        if id.is_empty() {
            continue;
        }
        if let Err(e) = ctx.case.db().account_instance(AccountInstanceSpec {
            type_name: ty,
            id,
            module: NAME,
            file_id: f.id,
            ds_id: ctx.ds_id,
            extra: Vec::new(),
            now: ctx.now(),
        }) {
            tracing::warn!(%e, id, "invalid account identifier");
        }
    }
    let mut a = WebArt::new(ctx, "TSK_WEB_FORM_ADDRESS", f.id)
        .text("TSK_NAME", name)
        .text_nonempty("TSK_EMAIL", email)
        .text_nonempty("TSK_PHONE_NUMBER", phone)
        .text_nonempty("TSK_LOCATION", location);
    if created > 0 {
        a = a.time("TSK_DATETIME_CREATED", Some(created));
    }
    if accessed > 0 {
        a = a.time("TSK_DATETIME_ACCESSED", Some(accessed));
    }
    if count > 0 {
        a = a.int("TSK_COUNT", count);
    }
    a
}

fn downloads(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if skipped(f, &["-slack", "cache", "index"]) || done_std(ctx, f, "TSK_WEB_DOWNLOAD") {
        return Ok(());
    }
    let mut list = Vec::new();
    with_db(ctx, web, WHO, f, |conn| {
        sql_soft(
            f,
            chr::downloads(conn, |d| {
                list.push(d);
                list.len() < 1_000_000
            }),
        );
        Ok(())
    })?;
    for d in list {
        if ctx.is_cancelled() {
            break;
        }
        let path_id = web.find_path(ctx, &d.full_path);
        let mut a = WebArt::new(ctx, "TSK_WEB_DOWNLOAD", f.id).text("TSK_PATH", &d.full_path);
        if let Some(id) = path_id {
            a = a.int("TSK_PATH_ID", id);
        }
        sink.push(
            ctx,
            a.text("TSK_URL", &d.url)
                .time("TSK_DATETIME_ACCESSED", d.start_time)
                .text("TSK_DOMAIN", extract_domain(&d.url))
                .text("TSK_USER_NAME", user)
                .text("TSK_PROG_NAME", p.browser)
                .associate(path_id),
        )?;
    }
    Ok(())
}

fn favicons(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    p: &Place,
    user: &str,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    if skipped(f, &["-slack", "cache", "index"]) {
        return Ok(());
    }
    let ty = custom_type(ctx, "TSK_FAVICON", "Favicon");
    if done(ctx, f, ty) {
        return Ok(());
    }
    with_db(ctx, web, WHO, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| chr::favicons(conn, cb),
            |i: chr::Favicon| {
                Some(
                    WebArt::of_type(ctx, ty, f.id)
                        .text("TSK_URL", &i.page_url)
                        .time("TSK_DATETIME_MODIFIED", i.last_updated)
                        .time("TSK_DATETIME_ACCESSED", i.last_requested)
                        .text("TSK_DOMAIN", extract_domain(&i.page_url))
                        .text("TSK_USER_NAME", user)
                        .text("TSK_PROG_NAME", p.browser),
                )
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(parent: &str, name: &str) -> FileRecord {
        FileRecord {
            id: 1,
            ds_id: 1,
            fs_id: None,
            parent_id: None,
            origin: pm_casedb::Origin::FileSystem,
            inode: None,
            stream: None,
            name: name.into(),
            parent_path: parent.into(),
            kind: pm_casedb::FileKindCode::Regular,
            name_alloc: true,
            meta_alloc: true,
            meta_flags: 0,
            size: 1,
            times: [None; 4],
            mode: None,
            uid: None,
            gid: None,
            seq: None,
            attributes: None,
            extension: String::new(),
            mime: None,
            md5: None,
            sha1: None,
            sha256: None,
            known: pm_casedb::Known::Unknown,
            derived_from: None,
            local_path: None,
        }
    }

    #[test]
    fn places_and_users() {
        let f = rec(
            "/Users/bob/AppData/Local/Google/Chrome/User Data/Profile 1/",
            "History",
        );
        let p = place(&f).unwrap();
        assert_eq!(p.browser, "Google Chrome");
        assert_eq!(p.profile, "Profile 1");
        assert_eq!(p.root, "/users/bob/appdata/local/google/chrome/user data");
        let mut users = Users::new();
        users.insert((p.root.clone(), "profile 1".into()), "Bob Work".into());
        assert_eq!(user_of(&users, &p), "Bob Work");
        let g = place(&rec("/x/Google/Chrome/User Data/Guest Profile/", "History")).unwrap();
        assert_eq!(user_of(&users, &g), "Guest");
        let o = place(&rec("/x/Opera Software/Opera Stable/", "History")).unwrap();
        assert_eq!((o.browser, o.profile.as_str()), ("Opera", ""));
        assert_eq!(user_of(&users, &o), "Default");
        let uc = place(&rec("/x/UCBrowser/User Data_i18n/Default/", "History_1")).unwrap();
        assert_eq!((uc.browser, uc.profile.as_str()), ("UC Browser", "Default"));
        assert!(is_store(&rec("/", "History_1"), &uc, "History"));
        assert!(!is_store(&rec("/", "History_1-journal"), &uc, "History"));
        assert!(!is_store(&rec("/", "History_1"), &p, "History"));
        assert!(place(&rec("/x/Firefox/", "places.sqlite")).is_none());
    }
}
