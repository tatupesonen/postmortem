//! Internet Explorer (Autopsy `ExtractIE`): Favorites, cookie text files
//! and history from every `index.dat` — parsed natively (Autopsy runs
//! the external pasco2 tool and reads its text output). Program name
//! "Internet Explorer Analyzer".

use pm_browser::domain::extract_domain;
use pm_browser::ie as iex;
use pm_browser::index_dat::{IndexDat, RecordKind, split_location};
use pm_casedb::FileRecord;
use pm_ingest::{JobContext, ModuleError};

use super::{Browser, History, Sink, Web, WebArt, done_std, read_small};

const PROG: &str = "Internet Explorer Analyzer";

/// Autopsy's IE `extractDomain`: `res://` URLs have none.
fn domain(url: &str) -> String {
    if url.to_lowercase().starts_with("res://") {
        String::new()
    } else {
        extract_domain(url)
    }
}

/// The file's creation time (Autopsy `getCrtime`, 0 when unknown).
fn crtime(f: &FileRecord) -> i64 {
    f.times.get(3).copied().flatten().map_or(0, |t| t.0)
}

pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    let favorites: Vec<FileRecord> = web
        .files
        .iter()
        .filter(|f| {
            f.extension.eq_ignore_ascii_case("url")
                && f.parent_path.to_lowercase().contains("favorites")
        })
        .cloned()
        .collect();
    let cookies: Vec<FileRecord> = web
        .files
        .iter()
        .filter(|f| {
            f.extension.eq_ignore_ascii_case("txt")
                && f.parent_path.to_lowercase().contains("cookies")
        })
        .cloned()
        .collect();
    let index_files = web.named("index.dat", "");
    if favorites.is_empty() && cookies.is_empty() && index_files.is_empty() {
        return Ok(());
    }
    web.set_found(Browser::Ie);
    let mut sink = Sink::default();
    web.progress("IE Bookmarks");
    for f in &favorites {
        if ctx.is_cancelled() {
            break;
        }
        if done_std(ctx, f, "TSK_WEB_BOOKMARK") {
            continue;
        }
        let Some(data) = read_small(ctx, f) else {
            web.error(format!("{PROG}: Error parsing IE bookmark File {}", f.name));
            continue;
        };
        let url = iex::favorite_url(&data);
        sink.push(
            ctx,
            WebArt::new(ctx, "TSK_WEB_BOOKMARK", f.id)
                .text("TSK_URL", &url)
                .text("TSK_TITLE", &f.name)
                .time("TSK_DATETIME_CREATED", Some(crtime(f)))
                .text("TSK_PROG_NAME", PROG)
                .text("TSK_DOMAIN", domain(&url)),
        )?;
    }
    sink.flush(ctx)?;
    web.progress("IE Cookies");
    for f in &cookies {
        if ctx.is_cancelled() {
            break;
        }
        if done_std(ctx, f, "TSK_WEB_COOKIE") {
            continue;
        }
        let Some(data) = read_small(ctx, f) else {
            web.error(format!(
                "{PROG}: Error reading Internet Explorer cookie {}",
                f.name
            ));
            continue;
        };
        let c = iex::cookie(&data);
        let d = domain(&c.url);
        sink.push(
            ctx,
            WebArt::new(ctx, "TSK_WEB_COOKIE", f.id)
                .text("TSK_URL", &c.url)
                .time("TSK_DATETIME_CREATED", Some(crtime(f)))
                .text("TSK_NAME", c.name)
                .text("TSK_VALUE", c.value)
                .text("TSK_PROG_NAME", PROG)
                .text_nonempty("TSK_DOMAIN", d),
        )?;
    }
    sink.flush(ctx)?;
    web.progress("IE History");
    for f in &index_files {
        if ctx.is_cancelled() {
            break;
        }
        if done_std(ctx, f, "TSK_WEB_HISTORY") {
            continue;
        }
        history(ctx, web, f, &mut sink)?;
        sink.flush(ctx)?;
    }
    sink.flush(ctx)
}

/// URL records of one `index.dat` → `TSK_WEB_HISTORY` (Autopsy reads
/// pasco2's `URL` lines: location, last-access time).
fn history(
    ctx: &JobContext,
    web: &mut Web<'_>,
    f: &FileRecord,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    let src = ctx.case.content(f)?;
    let idx = match IndexDat::open(src.as_ref()) {
        Ok(i) => i,
        Err(e) => {
            tracing::debug!(file = %f.path(), %e, "not an index.dat");
            web.error(format!(
                "{PROG}: Error processing Internet Explorer history."
            ));
            return Ok(());
        }
    };
    for (n, off) in idx.record_offsets().into_iter().enumerate() {
        if n % 1024 == 0 && ctx.is_cancelled() {
            break;
        }
        let Some(rec) = idx.record(off) else { continue };
        if rec.kind != RecordKind::Url {
            continue;
        }
        let (user, url) = split_location(&rec.location, |left| !domain(left).is_empty());
        let d = domain(&url);
        sink.push(
            ctx,
            WebArt::history(
                ctx,
                f.id,
                &History {
                    url: &url,
                    time: Some(rec.accessed.unwrap_or(0)),
                    referrer: "",
                    title: "",
                    program: PROG,
                    domain: &d,
                    user: &user,
                },
            ),
        )?;
    }
    Ok(())
}
