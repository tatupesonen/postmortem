//! Recent Activity — web (Autopsy `RecentActivity`: `Chromium`, `Firefox`,
//! `ExtractIE`, `SearchEngineURLQueryAnalyzer`).
//!
//! A data-source module (stage 1) that finds browser databases by name and
//! location, copies them to the case's temp folder, and posts web history,
//! downloads, cookies, bookmarks, accounts, form autofill and search-query
//! artifacts on the database files. Internet Explorer history is read from
//! `index.dat` natively (Autopsy shells out to `pasco`).
//!
//! Re-runs are idempotent: a database file that already has artifacts of a
//! type from this module is skipped for that type.

mod chromium;
mod ie;
mod search;
mod windows;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pm_casedb::catalog::artifact_type_id;
use pm_casedb::{FileKindCode, FileQuery, FileRecord};
use pm_ingest::{
    DataSourceModule, DsProgress, IngestMessage, JobContext, MessageKind, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob,
};

use crate::util::Art;

pub const NAME: &str = "Recent Activity";

#[derive(Debug, Default)]
pub struct RecentActivityFactory;

impl ModuleFactory for RecentActivityFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "recent_activity",
            name: NAME,
            description: "Extracts recent user activity, such as Web browsing, recently used documents and installed programs.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn start(
        &self,
        _ctx: &JobContext,
        _settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        Ok(Arc::new(Job))
    }
}

struct Job;

impl ModuleJob for Job {
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((1, Box::new(Extractor)))
    }
}

struct Extractor;

/// One history visit, kept for the search-query analysis.
#[derive(Debug, Clone)]
pub(crate) struct Visit {
    pub(crate) file_id: i64,
    pub(crate) url: String,
    pub(crate) time: Option<i64>,
}

/// One browser extractor: posts artifacts and collects history visits.
pub(crate) type Step = fn(&JobContext, &mut Vec<Visit>) -> Result<(), ModuleError>;

impl DataSourceModule for Extractor {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let mut steps: Vec<(&str, Step)> = vec![
            ("Chromium browsers", chromium::extract),
            ("Internet Explorer", ie::extract),
        ];
        steps.extend(windows::steps());
        progress.set_total(steps.len() as u64 + 1);
        let mut visits = Vec::new();
        for (i, (label, step)) in steps.iter().enumerate() {
            if ctx.is_cancelled() {
                return Ok(());
            }
            progress.message(format!("Extracting {label}"));
            if let Err(e) = step(ctx, &mut visits) {
                ctx.post_message(
                    IngestMessage::new(
                        MessageKind::Error,
                        NAME,
                        format!("{label}: extraction failed"),
                    )
                    .details(e.0.clone()),
                );
            }
            progress.set_done(i as u64 + 1);
        }
        progress.message("Analysing web search queries");
        search::analyse(ctx, &visits);
        progress.set_done(steps.len() as u64 + 1);
        Ok(())
    }
}

// ------------------------------------------------------------ helpers

/// Files of the data source named `name` (case-insensitive) whose path
/// contains `location` (case-insensitive; empty = anywhere).
pub(crate) fn find_files(
    ctx: &JobContext,
    name: &str,
    location: &str,
) -> Result<Vec<FileRecord>, ModuleError> {
    let q = FileQuery {
        ds_id: Some(ctx.ds_id),
        regular_only: true,
        name_contains: Some(name.to_owned()),
        ..FileQuery::default()
    };
    filtered(ctx, &q, |f| f.name.eq_ignore_ascii_case(name), location)
}

/// Files with extension `ext` whose path contains `location`.
pub(crate) fn find_by_extension(
    ctx: &JobContext,
    ext: &str,
    location: &str,
) -> Result<Vec<FileRecord>, ModuleError> {
    let q = FileQuery {
        ds_id: Some(ctx.ds_id),
        regular_only: true,
        extensions: vec![ext.to_owned()],
        ..FileQuery::default()
    };
    filtered(ctx, &q, |_| true, location)
}

fn filtered(
    ctx: &JobContext,
    q: &FileQuery,
    keep: impl Fn(&FileRecord) -> bool,
    location: &str,
) -> Result<Vec<FileRecord>, ModuleError> {
    let loc = location.to_lowercase();
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = ctx.case.db().query_files(q, after, 5_000)?;
        let Some(last) = page.last() else { break };
        after = last.id;
        out.extend(page.into_iter().filter(|f| {
            f.kind == FileKindCode::Regular
                && f.size > 0
                && f.stream.is_none()
                && keep(f)
                && (loc.is_empty() || f.path().to_lowercase().contains(&loc))
        }));
    }
    Ok(out)
}

/// Whether this module already posted `ty` artifacts on `file` (re-run).
pub(crate) fn already_done(ctx: &JobContext, file: &FileRecord, ty: &str) -> bool {
    let Some(type_id) = artifact_type_id(ty) else {
        return false;
    };
    ctx.case
        .db()
        .artifacts_for_file(file.id)
        .is_ok_and(|a| a.iter().any(|a| a.type_id == type_id))
}

/// Reads a whole (small) file's content.
pub(crate) fn read_all(ctx: &JobContext, f: &FileRecord) -> Result<Vec<u8>, ModuleError> {
    let src = ctx.case.content(f)?;
    let n = usize::try_from(src.size()).map_err(|_| ModuleError::new("file too large"))?;
    Ok(src.read_vec_at(0, n)?)
}

/// A local copy of an SQLite database (plus its `-wal`/`-journal`
/// siblings), deleted when dropped.
pub(crate) struct TempDb {
    paths: Vec<PathBuf>,
    pub(crate) conn: rusqlite::Connection,
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl TempDb {
    pub(crate) fn open(ctx: &JobContext, f: &FileRecord) -> Result<Self, ModuleError> {
        let dir = ctx.case.temp_dir().join("RecentActivity");
        std::fs::create_dir_all(&dir)?;
        let base = dir.join(format!("{}-{}", f.id, sanitize(&f.name)));
        let mut paths = Vec::new();
        write(&base, &read_all(ctx, f)?)?;
        paths.push(base.clone());
        // Uncommitted transactions live in the WAL / rollback journal.
        if let Some(parent) = f.parent_id
            && let Ok(siblings) = ctx.case.db().children(parent)
        {
            for suffix in ["-wal", "-journal"] {
                let want = format!("{}{suffix}", f.name);
                if let Some(s) = siblings.iter().find(|s| s.name == want && s.size > 0) {
                    let p = PathBuf::from(format!("{}{suffix}", base.display()));
                    write(&p, &read_all(ctx, s)?)?;
                    paths.push(p);
                }
            }
        }
        let conn = rusqlite::Connection::open(&base)
            .map_err(|e| ModuleError::new(format!("{}: {e}", f.path())))?;
        paths.push(PathBuf::from(format!("{}-shm", base.display())));
        Ok(Self { paths, conn })
    }
}

fn write(p: &Path, data: &[u8]) -> Result<(), ModuleError> {
    std::fs::write(p, data).map_err(|e| ModuleError::new(format!("{}: {e}", p.display())))
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The Windows / Unix user owning a profile path (`/Users/<u>/…`,
/// `/Documents and Settings/<u>/…`, `/home/<u>/…`).
pub(crate) fn user_from_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (i, p) in parts.iter().enumerate() {
        let l = p.to_lowercase();
        if (l == "users" || l == "documents and settings" || l == "home")
            && let Some(u) = parts.get(i + 1)
        {
            return (*u).to_owned();
        }
    }
    String::new()
}

/// Microseconds since 1601 (WebKit/Chromium) → Unix seconds; 0 = absent.
pub(crate) fn webkit_time(us: Option<i64>) -> Option<i64> {
    us.filter(|&t| t > 0)
        .map(|t| t / 1_000_000 - 11_644_473_600)
}

/// Microseconds since 1970 (Mozilla `PRTime`) → Unix seconds; 0 = absent.
#[allow(dead_code, reason = "for the Firefox extractor (pending)")]
pub(crate) fn prtime(us: Option<i64>) -> Option<i64> {
    us.filter(|&t| t > 0).map(|t| t / 1_000_000)
}

/// Multi-label public suffixes handled when reducing a host to its base
/// domain (Autopsy uses the full public-suffix list via `DomainTokenizer`).
const SECOND_LEVEL: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "ltd.uk",
    "plc.uk",
    "net.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.nz",
    "org.nz",
    "co.jp",
    "ne.jp",
    "or.jp",
    "ac.jp",
    "co.kr",
    "or.kr",
    "com.br",
    "com.cn",
    "net.cn",
    "org.cn",
    "gov.cn",
    "com.hk",
    "com.tw",
    "com.sg",
    "com.mx",
    "com.ar",
    "com.tr",
    "co.in",
    "co.za",
    "co.il",
    "com.ru",
    "com.ua",
    "co.id",
    "com.my",
    "com.ph",
    "com.vn",
    "com.sa",
    "com.eg",
    "com.pl",
    "co.th",
    "github.io",
    "blogspot.com",
];

/// Autopsy `NetworkUtils.extractDomain`: the registrable domain of a URL's
/// host (or an IPv4 address), `""` when there is none.
pub(crate) fn extract_domain(url: &str) -> String {
    let lower = url.trim().to_lowercase();
    if lower.starts_with("res://") {
        return String::new();
    }
    let no_scheme = lower.split_once("://").map_or(lower.as_str(), |(_, r)| r);
    let host_port = no_scheme.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = host_port.rsplit_once('@').map_or(host_port, |(_, h)| h);
    let host = host_port
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_matches('.');
    if host.is_empty() {
        return String::new();
    }
    let is_ipv4 = host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok());
    if is_ipv4 {
        return host.to_owned();
    }
    if !host.contains('.')
        || host
            .chars()
            .any(|c| !(c.is_alphanumeric() || c == '-' || c == '.'))
    {
        return String::new();
    }
    let labels: Vec<&str> = host.split('.').collect();
    let take = if labels.len() >= 3
        && SECOND_LEVEL
            .iter()
            .any(|s| host.ends_with(&format!(".{s}")) || host == *s)
    {
        3
    } else {
        2
    };
    let start = labels.len().saturating_sub(take);
    labels.get(start..).unwrap_or_default().join(".")
}

/// One web history visit (Autopsy `Extract.createHistoryAttributes`).
pub(crate) struct HistoryEntry<'a> {
    pub(crate) url: &'a str,
    pub(crate) time: Option<i64>,
    pub(crate) referrer: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) program: &'a str,
    pub(crate) user: &'a str,
}

/// The `TSK_WEB_HISTORY` artifact for `h` on `file`.
pub(crate) fn history(ctx: &JobContext, file: &FileRecord, h: HistoryEntry<'_>) -> Art {
    Art::data(ctx, NAME, "TSK_WEB_HISTORY", file.id)
        .text("TSK_URL", h.url)
        .time("TSK_DATETIME_ACCESSED", h.time)
        .text_opt("TSK_REFERRER", h.referrer)
        .text_opt("TSK_TITLE", h.title)
        .text("TSK_PROG_NAME", h.program)
        .text("TSK_DOMAIN", extract_domain(h.url))
        .text_opt("TSK_USER_NAME", Some(h.user.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains() {
        assert_eq!(
            extract_domain("https://www.google.com/search?q=x"),
            "google.com"
        );
        assert_eq!(extract_domain("http://news.bbc.co.uk/x"), "bbc.co.uk");
        assert_eq!(extract_domain("http://192.168.1.1:8080/"), "192.168.1.1");
        assert_eq!(extract_domain("res://ieframe.dll/acr.htm"), "");
        assert_eq!(extract_domain("file:///C:/x.html"), "");
        assert_eq!(extract_domain("localhost"), "");
        assert_eq!(
            extract_domain("Mr. Evil@http://www.example.com/"),
            "example.com"
        );
    }

    #[test]
    fn users_and_times() {
        assert_eq!(user_from_path("/Users/alice/AppData/Local/x"), "alice");
        assert_eq!(
            user_from_path("/Documents and Settings/Mr. Evil/Cookies/a.txt"),
            "Mr. Evil"
        );
        assert_eq!(user_from_path("/Windows/x"), "");
        assert_eq!(
            webkit_time(Some(13_000_000_000_000_000)),
            Some(1_355_526_400)
        );
        assert_eq!(webkit_time(Some(0)), None);
        assert_eq!(prtime(Some(1_600_000_000_000_000)), Some(1_600_000_000));
    }
}
