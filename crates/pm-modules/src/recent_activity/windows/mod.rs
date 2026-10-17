//! Recent Activity — Windows OS artefacts (Autopsy `ExtractRecycleBin`,
//! `ExtractJumpLists`, `RecentDocumentsByLnk`, `ExtractRegistry`,
//! `ExtractOs`, `DataSourceUsageAnalyzer`, `ExtractZoneIdentifier`,
//! `ExtractPrefetch`), parsed natively (no RegRipper, no external tools),
//! plus execution artefacts Autopsy only shows in the RegRipper text report
//! (`ShimCache`, Amcache, `UserAssist`, `RecentFileCache`).
//!
//! Artifacts are written in one batch per extractor with duplicate checks
//! on identifying attributes, so re-running ingest adds nothing new.

mod accounts;
mod art;
mod execution;
mod lnk;
mod os;
mod prefetch;
mod recycle;
mod registry;
mod zone;

use std::collections::HashMap;
use std::sync::Arc;

use pm_casedb::FileRecord;
use pm_core::SharedSource;
use pm_core::io::MemSource;
use pm_ingest::{JobContext, ModuleError};
use pm_registry::Hive;

use super::{Step, Visit};

/// Largest small artefact file read whole (LNK, `$I`, INFO2, prefetch,
/// jump lists, Zone.Identifier, `RecentFileCache`).
pub(crate) const MAX_SMALL_FILE: u64 = 64 << 20;
/// Most files of one kind processed per data source.
const PAGE: usize = 5_000;

/// The Windows extractors, in Autopsy's order (registry first so later
/// steps can map SIDs to user names).
pub(crate) fn steps() -> [(&'static str, Step); 9] {
    [
        ("Windows Registry", registry::extract as Step),
        ("Recycle Bin", recycle::extract),
        ("Jump Lists", lnk::extract_jump_lists),
        ("Recent documents (LNK)", lnk::extract_recent_documents),
        ("OS Info", os::extract_os),
        ("Data Source Usage", os::extract_usage),
        ("Zone.Identifier", zone::extract),
        ("Prefetch", prefetch::extract),
        ("Program execution caches", execution::extract),
    ]
}

/// Unused visit list parameter (the windows steps do not produce web
/// history).
pub(crate) type Visits = Vec<Visit>;

/// All files of the data source with extension `ext` whose name matches
/// `name_like` (SQL LIKE), optionally only those whose path contains
/// `location` (case-insensitive), streamed in id order.
pub(crate) fn files_with_ext(
    ctx: &JobContext,
    ext: &str,
    name_like: Option<&str>,
    location: &str,
) -> Result<Vec<FileRecord>, ModuleError> {
    let loc = location.to_lowercase();
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = ctx
            .case
            .db()
            .files_by_extension(ctx.ds_id, ext, name_like, after, PAGE)?;
        let Some(last) = page.last() else { break };
        after = last.id;
        out.extend(
            page.into_iter()
                .filter(|f| loc.is_empty() || f.path().to_lowercase().contains(&loc)),
        );
        if ctx.is_cancelled() {
            break;
        }
    }
    Ok(out)
}

/// Reads a small file whole (`None` when empty, too large or unreadable).
pub(crate) fn read_small(ctx: &JobContext, f: &FileRecord) -> Option<Vec<u8>> {
    if f.size == 0 || f.size > MAX_SMALL_FILE {
        return None;
    }
    let src = ctx.case.content(f).ok()?;
    let n = usize::try_from(src.size()).ok()?;
    src.read_vec_at(0, n).ok()
}

/// Opens a hive, replaying its `.LOG`/`.LOG1`/`.LOG2` siblings when dirty.
pub(crate) fn open_hive(ctx: &JobContext, f: &FileRecord) -> Option<Hive> {
    let src: SharedSource = ctx.case.content(f).ok()?;
    let mut logs: Vec<SharedSource> = Vec::new();
    if let Some(parent) = f.parent_id {
        for suffix in [".LOG", ".LOG1", ".LOG2"] {
            if let Ok(Some(l)) = ctx
                .case
                .db()
                .child_named_nocase(parent, &format!("{}{suffix}", f.name))
                && let Some(bytes) = read_small(ctx, &l)
            {
                logs.push(Arc::new(MemSource::new(bytes)));
            }
        }
    }
    match Hive::open_with_logs(src, &logs) {
        Ok(h) => {
            if h.replay_report().applied() {
                tracing::info!(hive = %f.path(), report = ?h.replay_report(), "transaction logs replayed");
            }
            Some(h)
        }
        Err(e) => {
            tracing::warn!(hive = %f.path(), %e, "not a readable registry hive");
            None
        }
    }
}

/// Resolves Windows paths (`C:\Windows\x.exe`, `/WINDOWS/x.exe`) to files
/// of the data source, caching results.
#[derive(Debug, Default)]
pub(crate) struct PathLookup {
    cache: HashMap<String, Option<i64>>,
}

impl PathLookup {
    /// The file id for `path`, if it exists on any file system.
    pub(crate) fn find(&mut self, ctx: &JobContext, path: &str) -> Option<i64> {
        let norm = path.trim().replace('\\', "/").to_lowercase();
        if norm.is_empty() || norm.starts_with("//") || norm.contains("://") {
            return None;
        }
        if let Some(hit) = self.cache.get(&norm) {
            return *hit;
        }
        let id = ctx
            .case
            .db()
            .resolve_path(ctx.ds_id, &norm)
            .ok()
            .and_then(|v| v.first().map(|f| f.id));
        if self.cache.len() < 100_000 {
            self.cache.insert(norm, id);
        }
        id
    }
}

/// SID → user name, from the SAM and `ProfileList` of the data source
/// (Autopsy's OS-account map).
pub(crate) fn user_map(ctx: &JobContext) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for f in registry::hive_files(ctx, "sam").unwrap_or_default() {
        if let Some(h) = open_hive(ctx, &f) {
            for u in pm_winartifacts::registry::sam::sam(&h).users {
                if let Some(sid) = u.sid {
                    map.insert(sid, u.name);
                }
            }
        }
    }
    for f in registry::hive_files(ctx, "software").unwrap_or_default() {
        if let Some(h) = open_hive(ctx, &f) {
            for p in pm_winartifacts::registry::software::profile_list(&h) {
                if !p.user.is_empty() {
                    map.entry(p.sid).or_insert(p.user);
                }
            }
        }
    }
    map
}

/// The user owning a profile path (`/Documents and Settings/<u>/…`).
pub(crate) fn user_of(path: &str) -> String {
    super::user_from_path(path)
}
