//! Firefox (Autopsy `Firefox.java`): history, bookmarks, downloads (both
//! schemas), cookies, form history and autofill addresses from files
//! whose path contains `Firefox`. Program name `FireFox Analyzer`.

use pm_browser::domain::{extract_domain, strip_leading_dots};
use pm_browser::firefox as ff;
use pm_casedb::FileRecord;
use pm_ingest::{JobContext, ModuleError};

use super::chromium::form_address;
use super::{
    Browser, History, Sink, Web, WebArt, done_std, push_rows, read_small, sql_soft, with_db,
};

const PROG: &str = "FireFox Analyzer";

/// Autopsy's Firefox `extractDomain`: `place:` URLs have none.
fn domain(url: &str) -> String {
    if url.to_lowercase().starts_with("place:") {
        String::new()
    } else {
        extract_domain(url)
    }
}

pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    let places = web.named("places.sqlite", "Firefox");
    let cookies = web.named("cookies.sqlite", "Firefox");
    let downloads = web.named("downloads.sqlite", "Firefox");
    let forms = web.named("formhistory.sqlite", "Firefox");
    let profiles = web.named("autofill-profiles.json", "Firefox");
    if places.is_empty()
        && cookies.is_empty()
        && downloads.is_empty()
        && forms.is_empty()
        && profiles.is_empty()
    {
        return Ok(());
    }
    web.set_found(Browser::Firefox);
    let mut sink = Sink::default();
    web.progress("Firefox History");
    for f in &places {
        if ctx.is_cancelled() {
            break;
        }
        if !done_std(ctx, f, "TSK_WEB_HISTORY") {
            history(ctx, web, f, &mut sink)?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Firefox Bookmarks");
    for f in &places {
        if !ctx.is_cancelled() && !done_std(ctx, f, "TSK_WEB_BOOKMARK") {
            bookmarks(ctx, web, f, &mut sink)?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Firefox Downloads");
    for (list, v24) in [(&downloads, false), (&places, true)] {
        for f in list {
            if !ctx.is_cancelled() && !done_std(ctx, f, "TSK_WEB_DOWNLOAD") {
                download(ctx, web, f, v24, &mut sink)?;
            }
        }
    }
    sink.flush(ctx)?;
    web.progress("Firefox Cookies");
    for f in &cookies {
        if !ctx.is_cancelled() && !done_std(ctx, f, "TSK_WEB_COOKIE") {
            cookie(ctx, web, f, &mut sink)?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Firefox Form History");
    for f in &forms {
        if !ctx.is_cancelled() && !done_std(ctx, f, "TSK_WEB_FORM_AUTOFILL") {
            form_history(ctx, web, f, &mut sink)?;
        }
    }
    sink.flush(ctx)?;
    web.progress("Firefox Auto Fill");
    for f in &profiles {
        if ctx.is_cancelled() || done_std(ctx, f, "TSK_WEB_FORM_ADDRESS") {
            continue;
        }
        let Some(data) = read_small(ctx, f) else {
            continue;
        };
        let Some(addresses) = ff::autofill_profiles(&data) else {
            web.error(format!(
                "{PROG}: Error while trying to analyze file:{}",
                f.name
            ));
            continue;
        };
        for a in addresses {
            sink.push(
                ctx,
                form_address(
                    ctx,
                    f,
                    &a.name,
                    &a.email,
                    &a.phone,
                    &a.location,
                    a.created,
                    a.last_used,
                    a.times_used,
                ),
            )?;
        }
    }
    sink.flush(ctx)
}

fn history(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    with_db(ctx, web, PROG, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| ff::history(conn, cb),
            |v: ff::Visit| {
                let d = domain(&v.url);
                Some(WebArt::history(
                    ctx,
                    f.id,
                    &History {
                        url: &v.url,
                        time: v.visit_date,
                        referrer: &v.referrer,
                        title: &v.title,
                        program: PROG,
                        domain: &d,
                        user: "",
                    },
                ))
            },
        )
    })
}

fn bookmarks(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    with_db(ctx, web, PROG, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| ff::bookmarks(conn, cb),
            |b: ff::Bookmark| {
                Some(
                    WebArt::new(ctx, "TSK_WEB_BOOKMARK", f.id)
                        .text("TSK_URL", &b.url)
                        .text("TSK_TITLE", b.title)
                        .time("TSK_DATETIME_CREATED", b.date_added)
                        .text("TSK_PROG_NAME", PROG)
                        .text_nonempty("TSK_DOMAIN", domain(&b.url)),
                )
            },
        )
    })
}

fn cookie(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    with_db(ctx, web, PROG, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| ff::cookies(conn, cb),
            |c: ff::Cookie| {
                let d = domain(&c.host);
                let mut a = WebArt::new(ctx, "TSK_WEB_COOKIE", f.id)
                    .text("TSK_URL", &c.host)
                    .time("TSK_DATETIME_ACCESSED", c.last_accessed)
                    .text("TSK_NAME", c.name)
                    .text("TSK_VALUE", c.value)
                    .text("TSK_PROG_NAME", PROG)
                    .time("TSK_DATETIME_CREATED", c.creation);
                if !d.is_empty() {
                    a = a.text("TSK_DOMAIN", strip_leading_dots(&d));
                }
                Some(a)
            },
        )
    })
}

fn download(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    v24: bool,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    let mut list = Vec::new();
    with_db(ctx, web, PROG, f, |conn| {
        let push = |d| {
            list.push(d);
            list.len() < 1_000_000
        };
        sql_soft(
            f,
            if v24 {
                ff::downloads_v24(conn, push)
            } else {
                ff::downloads_pre24(conn, push)
            },
        );
        Ok(())
    })?;
    for d in list {
        if ctx.is_cancelled() {
            break;
        }
        let path_id = web.find_path(ctx, &d.path);
        let mut a = WebArt::new(ctx, "TSK_WEB_DOWNLOAD", f.id).text("TSK_URL", &d.url);
        if !v24 {
            a = a.time("TSK_DATETIME_ACCESSED", d.time);
        }
        a = a.text("TSK_PATH", &d.path);
        if let Some(id) = path_id {
            a = a.int("TSK_PATH_ID", id);
        }
        if v24 {
            a = a.time("TSK_DATETIME_ACCESSED", d.time);
        }
        sink.push(
            ctx,
            a.text("TSK_PROG_NAME", PROG)
                .text_nonempty("TSK_DOMAIN", domain(&d.url))
                .associate(path_id),
        )?;
    }
    Ok(())
}

fn form_history(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    with_db(ctx, web, PROG, f, |conn| {
        push_rows(
            ctx,
            f,
            sink,
            |cb| ff::form_history(conn, cb),
            |e: ff::FormEntry| {
                let mut a = WebArt::new(ctx, "TSK_WEB_FORM_AUTOFILL", f.id)
                    .text("TSK_NAME", e.field)
                    .text("TSK_VALUE", e.value);
                if let Some((first, last, times)) = e.usage {
                    a = a
                        .time("TSK_DATETIME_CREATED", Some(first))
                        .time("TSK_DATETIME_ACCESSED", Some(last))
                        .int("TSK_COUNT", times);
                }
                Some(a)
            },
        )
    })
}
