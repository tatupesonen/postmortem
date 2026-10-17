//! Recent Activity (Autopsy `RecentActivity`, `RAImageIngestModule`).
//!
//! A data-source module (stage 1) running Autopsy's extractors in its
//! order — Windows artefacts first (registry, recycle bin, jump lists,
//! LNK files, OS info, data source usage), then the web browsers
//! (Chromium family + cache, Firefox, Internet Explorer, Edge legacy,
//! Safari), the analyses over their artifacts (search queries, web
//! account roles), Zone.Identifier downloads, prefetch and execution
//! caches, and domain categories last.
//!
//! Inbox messages as Autopsy: `Started <ds>`, `Finished <ds> - <n errors
//! / No errors reported>` (error lines in the details) and `<ds> -
//! Browser Results` (Found / Not Found per browser extractor).
//!
//! Settings (optional): `custom_web_categories`: a list of
//! `{"suffix": "...", "category": "..."}` checked before the built-in
//! domain categories (Autopsy's Custom Web Categories options).

mod web;
mod windows;

use std::fmt::Write as _;
use std::sync::Arc;

use pm_casedb::{FileKindCode, FileQuery, FileRecord};
use pm_ingest::{
    DataSourceModule, DsProgress, IngestMessage, JobContext, MessageKind, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob,
};

pub(crate) use pm_browser::domain::extract_domain;

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
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        Ok(Arc::new(Job {
            settings: settings.clone(),
        }))
    }
}

struct Job {
    settings: serde_json::Value,
}

impl ModuleJob for Job {
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((
            1,
            Box::new(Extractor {
                settings: self.settings.clone(),
            }),
        ))
    }
}

struct Extractor {
    settings: serde_json::Value,
}

/// One history visit (kept for the Windows extractors' signature).
#[derive(Debug, Clone)]
pub(crate) struct Visit;

/// One Windows extractor.
pub(crate) type Step = fn(&JobContext, &mut Vec<Visit>) -> Result<(), ModuleError>;

/// One web extractor.
type WebStep = fn(&JobContext, &mut web::Web<'_>) -> Result<(), ModuleError>;

enum Task {
    Windows(&'static str, Step),
    Web(&'static str, WebStep),
}

/// Autopsy's extractor order (`RAImageIngestModule.startUp`).
fn tasks() -> Vec<Task> {
    let [
        registry,
        recycle,
        jump,
        lnk,
        os,
        usage,
        zone,
        prefetch,
        execution,
    ] = windows::steps();
    let w = |(n, s): (&'static str, Step)| Task::Windows(n, s);
    vec![
        w(registry),
        w(recycle),
        w(jump),
        w(lnk),
        w(os),
        w(usage),
        Task::Web("Chromium Analyzer", web::chromium::extract),
        Task::Web("FireFox Analyzer", web::firefox::extract),
        Task::Web("Internet Explorer Analyzer", web::ie::extract),
        Task::Web("Microsoft Edge Analyzer", web::edge::extract),
        Task::Web("Safari Analyzer", web::safari::extract),
        Task::Web(
            "Search Engine Query Analyzer",
            web::analytics::search_queries_step,
        ),
        Task::Web(
            "Web Account Type Analyzer",
            web::analytics::account_types_step,
        ),
        w(zone),
        w(prefetch),
        w(execution),
        Task::Web("Domain Category Analyzer", web::analytics::categories_step),
    ]
}

impl DataSourceModule for Extractor {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let ds_name = ctx
            .case
            .db()
            .data_source(ctx.ds_id)
            .ok()
            .flatten()
            .map_or_else(|| ctx.ds_id.to_string(), |d| d.name);
        ctx.post_message(IngestMessage::new(
            MessageKind::Info,
            NAME,
            format!("Started {ds_name}"),
        ));
        let tasks = tasks();
        progress.set_total(tasks.len() as u64);
        let report = |m: String| progress.message(m);
        let mut web = web::Web::scan(ctx, &report, &self.settings)?;
        let mut visits = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        for (i, task) in tasks.iter().enumerate() {
            if ctx.is_cancelled() {
                tracing::info!("Recent Activity has been canceled");
                break;
            }
            let (label, result) = match task {
                Task::Windows(label, step) => {
                    progress.message(format!("Extracting {label}"));
                    (*label, step(ctx, &mut visits))
                }
                Task::Web(label, step) => {
                    progress.message((*label).to_owned());
                    (*label, step(ctx, &mut web))
                }
            };
            if let Err(e) = result {
                tracing::error!(extractor = label, error = %e.0, "Recent Activity extractor failed");
                errors.push(format!("{NAME} had errors -- see log"));
            }
            progress.set_done(i as u64 + 1);
        }
        errors.append(&mut web.errors);
        // Autopsy's texts; the details are plain text (the inbox does not
        // render Autopsy's HTML).
        let (kind, subject, details) = if errors.is_empty() {
            (
                MessageKind::Info,
                "No errors reported".to_owned(),
                "No errors encountered.".to_owned(),
            )
        } else {
            let mut d = "Errors encountered during analysis:".to_owned();
            for e in &errors {
                let _ = write!(d, "\n- {e}");
            }
            let s = if errors.len() == 1 {
                "1 error found".to_owned()
            } else {
                format!("{} errors found", errors.len())
            };
            (MessageKind::Error, s, d)
        };
        ctx.post_message(
            IngestMessage::new(kind, NAME, format!("Finished {ds_name} - {subject}"))
                .details(details),
        );
        let mut hist = format!("Browser Data on {ds_name}:");
        for b in web::Browser::ALL {
            let _ = write!(
                hist,
                "\n- {}: {}",
                b.display_name(),
                if web.found(b) { "Found." } else { "Not Found." }
            );
        }
        ctx.post_message(
            IngestMessage::new(
                MessageKind::Info,
                NAME,
                format!("{ds_name} - Browser Results"),
            )
            .details(hist),
        );
        Ok(())
    }
}

/// Regular files (no streams, size > 0) of the data source with
/// extension `ext` whose path contains `location` (case-insensitive;
/// empty = anywhere), keyset-paged on the extension index.
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
    let loc = location.to_lowercase();
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = ctx.case.db().query_files(&q, after, 5_000)?;
        let Some(last) = page.last() else { break };
        after = last.id;
        out.extend(page.into_iter().filter(|f| {
            f.kind == FileKindCode::Regular
                && f.size > 0
                && f.stream.is_none()
                && (loc.is_empty() || f.path().to_lowercase().contains(&loc))
        }));
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn users() {
        assert_eq!(user_from_path("/Users/alice/AppData/Local/x"), "alice");
        assert_eq!(
            user_from_path("/Documents and Settings/Mr. Evil/Cookies/a.txt"),
            "Mr. Evil"
        );
        assert_eq!(user_from_path("/Windows/x"), "");
    }

    #[test]
    fn autopsy_order() {
        let names: Vec<&str> = tasks()
            .iter()
            .map(|t| match t {
                Task::Windows(n, _) | Task::Web(n, _) => *n,
            })
            .collect();
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("Data Source Usage") < pos("Chromium Analyzer"));
        assert!(pos("Safari Analyzer") < pos("Search Engine Query Analyzer"));
        assert!(pos("Web Account Type Analyzer") < pos("Zone.Identifier"));
        assert_eq!(names.last(), Some(&"Domain Category Analyzer"));
    }
}
