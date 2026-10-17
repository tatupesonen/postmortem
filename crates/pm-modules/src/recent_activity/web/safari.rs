//! Safari (Autopsy `ExtractSafari`): `History.db`, `Bookmarks.plist`,
//! `Downloads.plist` under a `Safari` folder and `Cookies.binarycookies`
//! under a `Cookies` folder. Program name "Safari Analyzer".

use pm_browser::binarycookies;
use pm_browser::domain::extract_domain;
use pm_browser::safari as sf;
use pm_ingest::{JobContext, ModuleError};

use super::{Browser, History, Sink, Web, WebArt, done_std, push_rows, read_small, with_db};

const PROG: &str = "Safari Analyzer";

pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    let history = web.named("History.db", "Safari");
    let bookmarks = web.named("Bookmarks.plist", "Safari");
    let downloads = web.named("Downloads.plist", "Safari");
    let cookies = web.named("Cookies.binarycookies", "Cookies");
    if history.is_empty() && bookmarks.is_empty() && downloads.is_empty() && cookies.is_empty() {
        return Ok(());
    }
    web.set_found(Browser::Safari);
    let mut sink = Sink::default();
    web.progress("Safari History");
    for f in &history {
        if ctx.is_cancelled() || done_std(ctx, f, "KAAMOS_WEB_HISTORY") {
            continue;
        }
        with_db(ctx, web, PROG, f, |conn| {
            push_rows(
                ctx,
                f,
                &mut sink,
                |cb| sf::history(conn, cb),
                |v: sf::Visit| {
                    let d = extract_domain(&v.url);
                    Some(WebArt::history(
                        ctx,
                        f.id,
                        &History {
                            url: &v.url,
                            time: v.time,
                            referrer: "",
                            title: &v.title,
                            program: PROG,
                            domain: &d,
                            user: "",
                        },
                    ))
                },
            )
        })?;
    }
    sink.flush(ctx)?;
    web.progress("Safari Bookmarks");
    for f in &bookmarks {
        if ctx.is_cancelled() || done_std(ctx, f, "KAAMOS_WEB_BOOKMARK") {
            continue;
        }
        let Some(list) = read_small(ctx, f).and_then(|d| sf::bookmarks(&d)) else {
            web.error("An error occured while processing Safari Bookmark files".to_owned());
            continue;
        };
        for b in list {
            let url = b.url.unwrap_or_default();
            sink.push(
                ctx,
                WebArt::new(ctx, "KAAMOS_WEB_BOOKMARK", f.id)
                    .text("KAAMOS_URL", &url)
                    .text("KAAMOS_TITLE", b.title.unwrap_or_default())
                    .text("KAAMOS_PROG_NAME", PROG)
                    .text("KAAMOS_DOMAIN", extract_domain(&url)),
            )?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Safari Downloads");
    for f in &downloads {
        if ctx.is_cancelled() || done_std(ctx, f, "KAAMOS_WEB_DOWNLOAD") {
            continue;
        }
        let Some(list) = read_small(ctx, f).and_then(|d| sf::downloads(&d)) else {
            continue;
        };
        for d in list {
            let path = d.path.unwrap_or_default();
            let url = d.url.unwrap_or_default();
            let path_id = if path.is_empty() {
                None
            } else {
                web.find_path(ctx, &path)
            };
            let mut a = WebArt::new(ctx, "KAAMOS_WEB_DOWNLOAD", f.id).text("KAAMOS_PATH", &path);
            if let Some(id) = path_id {
                a = a.int("KAAMOS_PATH_ID", id);
            }
            sink.push(
                ctx,
                a.text("KAAMOS_URL", &url)
                    .time("KAAMOS_DATETIME_ACCESSED", d.time)
                    .text("KAAMOS_DOMAIN", extract_domain(&url))
                    .text("KAAMOS_PROG_NAME", PROG)
                    .associate(path_id),
            )?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Safari Cookies");
    for f in &cookies {
        if ctx.is_cancelled() || done_std(ctx, f, "KAAMOS_WEB_COOKIE") {
            continue;
        }
        let Some(list) = read_small(ctx, f).and_then(|d| binarycookies::parse(&d).ok()) else {
            web.error("An error occured while processing Safari Cookies files".to_owned());
            continue;
        };
        for c in list {
            sink.push(
                ctx,
                WebArt::new(ctx, "KAAMOS_WEB_COOKIE", f.id)
                    .text("KAAMOS_URL", &c.url)
                    .time("KAAMOS_DATETIME_CREATED", c.creation.filter(|t| *t != 0))
                    .time("KAAMOS_DATETIME_END", c.expiration.filter(|t| *t != 0))
                    .text("KAAMOS_NAME", c.name)
                    .text("KAAMOS_VALUE", c.value)
                    .text("KAAMOS_PROG_NAME", PROG)
                    .text("KAAMOS_DOMAIN", extract_domain(&c.url)),
            )?;
        }
    }
    sink.flush(ctx)
}
