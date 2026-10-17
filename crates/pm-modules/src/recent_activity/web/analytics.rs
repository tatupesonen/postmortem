//! Analyses over the web artifacts of the data source (whichever module
//! posted them): search queries (Autopsy `SearchEngineURLQueryAnalyzer`),
//! web account roles (`ExtractWebAccountType`) and domain categories
//! (`DomainCategoryRunner`). Artifacts are streamed from the case
//! database in keyset pages; memory is bounded by distinct hosts /
//! domains, not by artifacts.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use pm_browser::account_type::RoleProcessor;
use pm_browser::categories::{Categorizer, host_and_domain};
use pm_browser::search_engines::search_queries;
use pm_casedb::ArtifactRow;
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_ingest::{JobContext, ModuleError};

use super::{Sink, Web, WebArt};

const PAGE: usize = 2_000;

fn ty(name: &str) -> i64 {
    artifact_type_id(name).unwrap_or(0)
}

fn at(name: &str) -> i64 {
    attribute_type_id(name).unwrap_or(0)
}

/// Streams the data source's artifacts of `types` with attributes `attrs`.
fn each_artifact(
    ctx: &JobContext,
    types: &[&str],
    attrs: &[&str],
    mut f: impl FnMut(ArtifactRow) -> Result<(), ModuleError>,
) -> Result<(), ModuleError> {
    let types: Vec<i64> = types.iter().map(|t| ty(t)).collect();
    let attrs: Vec<i64> = attrs.iter().map(|a| at(a)).collect();
    let mut after = 0;
    loop {
        if ctx.is_cancelled() {
            return Ok(());
        }
        let page = ctx
            .case
            .db()
            .artifact_rows(ctx.ds_id, &types, &attrs, after, PAGE)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = last.id;
        for r in page {
            f(r)?;
        }
    }
}

/// `KAAMOS_WEB_SEARCH_QUERY` for every search-engine query in the URLs of
/// bookmarks and history (one per distinct query and source artifact).
pub(crate) fn search_queries_step(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    web.progress("Find Search Queries");
    let (url, prog, domain, time, text) = (
        at("KAAMOS_URL"),
        at("KAAMOS_PROG_NAME"),
        at("KAAMOS_DOMAIN"),
        at("KAAMOS_DATETIME_ACCESSED"),
        at("KAAMOS_TEXT"),
    );
    // What earlier runs produced: (file, text, domain, program, time).
    let mut existing: HashSet<(i64, String, String, String, i64)> = HashSet::new();
    each_artifact(
        ctx,
        &["KAAMOS_WEB_SEARCH_QUERY"],
        &[
            "KAAMOS_TEXT",
            "KAAMOS_DOMAIN",
            "KAAMOS_PROG_NAME",
            "KAAMOS_DATETIME_ACCESSED",
        ],
        |r| {
            if let Some(file) = r.file_id {
                existing.insert((
                    file,
                    r.text(text).unwrap_or_default().to_owned(),
                    r.text(domain).unwrap_or_default().to_owned(),
                    r.text(prog).unwrap_or_default().to_owned(),
                    r.int(time).unwrap_or(0),
                ));
            }
            Ok(())
        },
    )?;
    let mut sink = Sink::default();
    each_artifact(
        ctx,
        &["KAAMOS_WEB_BOOKMARK", "KAAMOS_WEB_HISTORY"],
        &[
            "KAAMOS_URL",
            "KAAMOS_PROG_NAME",
            "KAAMOS_DOMAIN",
            "KAAMOS_DATETIME_ACCESSED",
        ],
        |r| {
            let (Some(file), Some(u)) = (r.file_id, r.text(url)) else {
                return Ok(());
            };
            let queries = search_queries(u);
            if queries.is_empty() {
                return Ok(());
            }
            let browser = r.text(prog).unwrap_or_default().to_owned();
            let d = r.text(domain).unwrap_or_default().to_owned();
            let t = r.int(time).unwrap_or(0);
            for q in queries {
                let key = (file, q, d.clone(), browser.clone(), t);
                if existing.contains(&key) {
                    continue;
                }
                let q = key.1.clone();
                existing.insert(key);
                sink.push(
                    ctx,
                    WebArt::new(ctx, "KAAMOS_WEB_SEARCH_QUERY", file)
                        .text("KAAMOS_DOMAIN", &d)
                        .text("KAAMOS_TEXT", q)
                        .text("KAAMOS_PROG_NAME", &browser)
                        .time("KAAMOS_DATETIME_ACCESSED", Some(t)),
                )?;
            }
            Ok(())
        },
    )?;
    sink.flush(ctx)
}

/// `KAAMOS_WEB_ACCOUNT_TYPE`: the highest role per (domain, platform) seen
/// in history and saved-login URLs.
pub(crate) fn account_types_step(ctx: &JobContext, _web: &mut Web<'_>) -> Result<(), ModuleError> {
    let (url, domain, text) = (at("KAAMOS_URL"), at("KAAMOS_DOMAIN"), at("KAAMOS_TEXT"));
    let service = ty("KAAMOS_SERVICE_ACCOUNT");
    let mut roles: RoleProcessor<i64> = RoleProcessor::default();
    each_artifact(
        ctx,
        &["KAAMOS_WEB_HISTORY", "KAAMOS_SERVICE_ACCOUNT"],
        &["KAAMOS_URL", "KAAMOS_DOMAIN"],
        |r| {
            if let (Some(file), Some(u), Some(d)) = (r.file_id, r.text(url), r.text(domain)) {
                roles.add(u, d, r.type_id == service, &file);
            }
            Ok(())
        },
    )?;
    let found = roles.finish();
    if found.is_empty() {
        return Ok(());
    }
    let mut existing: HashSet<(i64, String, String)> = HashSet::new();
    each_artifact(
        ctx,
        &["KAAMOS_WEB_ACCOUNT_TYPE"],
        &["KAAMOS_DOMAIN", "KAAMOS_TEXT"],
        |r| {
            if let Some(f) = r.file_id {
                existing.insert((
                    f,
                    r.text(domain).unwrap_or_default().to_owned(),
                    r.text(text).unwrap_or_default().to_owned(),
                ));
            }
            Ok(())
        },
    )?;
    let mut sink = Sink::default();
    for role in found {
        if ctx.is_cancelled() {
            break;
        }
        let t = role.text();
        if !existing.insert((role.source, role.domain.clone(), t.clone())) {
            continue;
        }
        sink.push(
            ctx,
            WebArt::new(ctx, "KAAMOS_WEB_ACCOUNT_TYPE", role.source)
                .text("KAAMOS_DOMAIN", &role.domain)
                .text("KAAMOS_TEXT", t)
                .text("KAAMOS_URL", &role.url),
        )?;
    }
    sink.flush(ctx)
}

/// The first artifact of a host in Autopsy's order: accessed time
/// descending, then URL case-insensitively, then id.
#[derive(Debug, Clone)]
struct HostFirst {
    time: i64,
    url_lower: String,
    id: i64,
    file_id: i64,
    domain: String,
}

fn autopsy_order(a: &HostFirst, b: &HostFirst) -> Ordering {
    b.time
        .cmp(&a.time)
        .then_with(|| a.url_lower.cmp(&b.url_lower))
        .then_with(|| a.id.cmp(&b.id))
}

/// `KAAMOS_WEB_CATEGORIZATION`: the category of each host suffix (search
/// engine, web mail, disposable mail, messaging, user categories), once
/// per suffix, on the file of the most recent artifact of the first host
/// that matched it.
pub(crate) fn categories_step(ctx: &JobContext, web: &mut Web<'_>) -> Result<(), ModuleError> {
    web.progress("Finding Domain Types");
    let (url, domain, time, host_attr) = (
        at("KAAMOS_URL"),
        at("KAAMOS_DOMAIN"),
        at("KAAMOS_DATETIME_ACCESSED"),
        at("KAAMOS_HOST"),
    );
    let categorizer = Categorizer {
        custom: web.custom_categories.clone(),
    };
    let mut hosts: HashMap<String, HostFirst> = HashMap::new();
    each_artifact(
        ctx,
        &[
            "KAAMOS_WEB_BOOKMARK",
            "KAAMOS_WEB_CACHE",
            "KAAMOS_WEB_COOKIE",
            "KAAMOS_WEB_DOWNLOAD",
            "KAAMOS_WEB_HISTORY",
            "KAAMOS_WEB_SEARCH_QUERY",
        ],
        &["KAAMOS_URL", "KAAMOS_DOMAIN", "KAAMOS_DATETIME_ACCESSED"],
        |r| {
            let Some(file_id) = r.file_id else {
                return Ok(());
            };
            let Some((host, dom)) = host_and_domain(r.text(url), r.text(domain)) else {
                return Ok(());
            };
            if host.trim().is_empty() {
                return Ok(());
            }
            let cand = HostFirst {
                time: r.int(time).unwrap_or(0),
                url_lower: r.text(url).unwrap_or_default().to_lowercase(),
                id: r.id,
                file_id,
                domain: dom,
            };
            match hosts.get_mut(&host) {
                Some(cur) if autopsy_order(&cand, cur) == Ordering::Less => *cur = cand,
                Some(_) => {}
                None => {
                    if hosts.len() < 10_000_000 {
                        hosts.insert(host, cand);
                    }
                }
            }
            Ok(())
        },
    )?;
    // Suffixes already reported by an earlier run.
    let mut suffixes: HashSet<String> = HashSet::new();
    each_artifact(
        ctx,
        &["KAAMOS_WEB_CATEGORIZATION"],
        &["KAAMOS_HOST", "KAAMOS_DOMAIN"],
        |r| {
            let h = r.text(host_attr).unwrap_or_default();
            let d = r.text(domain).unwrap_or_default();
            if let Some(c) = categorizer.category(d, h) {
                suffixes.insert(c.host_suffix);
            }
            Ok(())
        },
    )?;
    let mut ordered: Vec<(String, HostFirst)> = hosts.into_iter().collect();
    ordered.sort_by(|a, b| autopsy_order(&a.1, &b.1));
    let mut sink = Sink::default();
    for (host, first) in ordered {
        if ctx.is_cancelled() {
            break;
        }
        let Some(c) = categorizer.category(&first.domain, &host) else {
            continue;
        };
        if c.host_suffix.trim().is_empty() || c.category.trim().is_empty() {
            continue;
        }
        if !suffixes.insert(c.host_suffix) {
            continue;
        }
        sink.push(
            ctx,
            WebArt::new(ctx, "KAAMOS_WEB_CATEGORIZATION", first.file_id)
                .text("KAAMOS_DOMAIN", &first.domain)
                .text("KAAMOS_HOST", &host)
                .text("KAAMOS_NAME", c.category),
        )?;
    }
    sink.flush(ctx)
}
