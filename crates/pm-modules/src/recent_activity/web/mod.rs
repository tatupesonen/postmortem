//! Web browser extractors of Recent Activity (Autopsy `Chromium`,
//! `Firefox`, `ExtractIE`, `ExtractEdge`, `ExtractSafari`,
//! `ChromeCacheExtractor`, `SearchEngineURLQueryAnalyzer`,
//! `ExtractWebAccountType`, `DomainCategoryRunner`). Parsing lives in
//! `pm-browser`; this layer finds the files, copies SQLite stores to the
//! case temp folder and writes artifacts.
//!
//! Instead of Autopsy's dozens of `findFiles` scans, one keyset-paged
//! scan collects every candidate file by name; extractors then filter by
//! location. Artifacts are written in batched transactions straight to
//! the case database (later steps read them back). Re-runs are
//! idempotent: a store that already has artifacts of a type is skipped
//! for that type; the derived analyses deduplicate against what exists.

pub(crate) mod analytics;
pub(crate) mod chrome_cache;
pub(crate) mod chromium;
pub(crate) mod edge;
pub(crate) mod firefox;
pub(crate) mod ie;
pub(crate) mod safari;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use pm_casedb::catalog::{Category, artifact_type_id, attribute_type_id};
use pm_casedb::{FileRecord, NameFilter, NewArtifact, Priority, Significance, Value};
use pm_core::ReadAt;
use pm_ingest::{JobContext, ModuleError};

use super::NAME;
use super::windows::PathLookup;

/// The browser extractors with their Autopsy display names (inbox
/// "Browser Results" order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Browser {
    Chromium,
    Firefox,
    Ie,
    Edge,
    Safari,
}

impl Browser {
    pub(crate) const ALL: [Self; 5] = [
        Self::Chromium,
        Self::Firefox,
        Self::Ie,
        Self::Edge,
        Self::Safari,
    ];

    /// Autopsy's extractor display name (also the `TSK_PROG_NAME` of
    /// Firefox, IE, Edge and Safari artifacts).
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Chromium => "Chromium Analyzer",
            Self::Firefox => "FireFox Analyzer",
            Self::Ie => "Internet Explorer Analyzer",
            Self::Edge => "Microsoft Edge Analyzer",
            Self::Safari => "Safari Analyzer",
        }
    }
}

/// Per-job state shared by the web extractors.
pub(crate) struct Web<'p> {
    /// Every candidate browser file of the data source.
    pub(crate) files: Vec<FileRecord>,
    found: [bool; 5],
    /// Error lines for the "Finished" inbox message.
    pub(crate) errors: Vec<String>,
    pub(crate) paths: PathLookup,
    progress: &'p dyn Fn(String),
    /// User-defined domain categories (module setting).
    pub(crate) custom_categories: pm_browser::categories::SuffixTable,
}

impl std::fmt::Debug for Web<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Web")
            .field("files", &self.files.len())
            .field("found", &self.found)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

/// Names of the files any browser extractor reads.
fn candidate_filter() -> NameFilter {
    let names = [
        // Chromium
        "Local State",
        "Secure Preferences",
        "History",
        "Bookmarks",
        "Cookies",
        "Login Data",
        "Web Data",
        "Favicons",
        "index",
        // Firefox
        "places.sqlite",
        "cookies.sqlite",
        "downloads.sqlite",
        "formhistory.sqlite",
        "autofill-profiles.json",
        // IE / Edge
        "index.dat",
        "WebCacheV01.dat",
        "Spartan.edb",
        // Safari
        "History.db",
        "Bookmarks.plist",
        "Downloads.plist",
        "Cookies.binarycookies",
    ];
    // UC Browser keeps suffixed copies (Autopsy's `History%` patterns).
    let prefixes = [
        "History",
        "Bookmarks",
        "Login Data",
        "Web Data",
        "Local State",
        "Secure Preferences",
    ];
    NameFilter {
        names: names.iter().map(|s| (*s).to_owned()).collect(),
        prefixes: prefixes.iter().map(|s| (*s).to_owned()).collect(),
        contains: vec!["Cookies".to_owned()],
        ext_in_dir: vec![
            ("url".to_owned(), "Favorites".to_owned()),
            ("txt".to_owned(), "Cookies".to_owned()),
        ],
    }
}

impl<'p> Web<'p> {
    /// Collects the candidate files (one pass over the data source).
    pub(crate) fn scan(
        ctx: &JobContext,
        progress: &'p dyn Fn(String),
        settings: &serde_json::Value,
    ) -> Result<Self, ModuleError> {
        let filter = candidate_filter();
        let mut files = Vec::new();
        let mut after = 0;
        loop {
            let page = ctx
                .case
                .db()
                .files_matching_names(ctx.ds_id, &filter, after, 5_000)?;
            let Some(last) = page.last() else { break };
            after = last.id;
            files.extend(page.into_iter().filter(|f| f.size > 0));
            if ctx.is_cancelled() {
                break;
            }
        }
        let mut custom_categories = pm_browser::categories::SuffixTable::default();
        if let Some(list) = settings
            .get("custom_web_categories")
            .and_then(serde_json::Value::as_array)
        {
            for e in list {
                if let (Some(s), Some(c)) = (
                    e.get("suffix").and_then(serde_json::Value::as_str),
                    e.get("category").and_then(serde_json::Value::as_str),
                ) {
                    custom_categories.insert(s, c);
                }
            }
        }
        Ok(Self {
            files,
            found: [false; 5],
            errors: Vec::new(),
            paths: PathLookup::default(),
            progress,
            custom_categories,
        })
    }

    pub(crate) fn set_found(&mut self, b: Browser) {
        if let Some(f) = Browser::ALL
            .iter()
            .position(|x| *x == b)
            .and_then(|i| self.found.get_mut(i))
        {
            *f = true;
        }
    }

    pub(crate) fn found(&self, b: Browser) -> bool {
        Browser::ALL
            .iter()
            .position(|x| *x == b)
            .and_then(|i| self.found.get(i))
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn progress(&self, msg: impl Into<String>) {
        (self.progress)(msg.into());
    }

    /// An error line for the final inbox message (Autopsy
    /// `addErrorMessage`), also logged.
    pub(crate) fn error(&mut self, msg: String) {
        tracing::warn!(target: "recent_activity", "{msg}");
        if self.errors.len() < 1_000 {
            self.errors.push(msg);
        }
    }

    /// Candidate files named `name` (ASCII case-insensitive) whose parent
    /// path contains `dir` (case-insensitive; `""` = anywhere).
    pub(crate) fn named(&self, name: &str, dir: &str) -> Vec<FileRecord> {
        let dir = dir.to_lowercase();
        self.files
            .iter()
            .filter(|f| {
                f.name.eq_ignore_ascii_case(name)
                    && (dir.is_empty() || f.parent_path.to_lowercase().contains(&dir))
            })
            .cloned()
            .collect()
    }

    /// The id of the file at a Windows / Unix path (Autopsy `Util.findID`).
    pub(crate) fn find_path(&mut self, ctx: &JobContext, path: &str) -> Option<i64> {
        self.paths.find(ctx, path)
    }
}

/// Whether the store was already analysed for `type_id` (re-run).
pub(crate) fn done(ctx: &JobContext, f: &FileRecord, type_id: i64) -> bool {
    ctx.case
        .db()
        .file_has_artifact_type(f.id, type_id)
        .unwrap_or(false)
}

/// Whether the store was already analysed for standard type `ty`.
pub(crate) fn done_std(ctx: &JobContext, f: &FileRecord, ty: &str) -> bool {
    artifact_type_id(ty).is_some_and(|t| done(ctx, f, t))
}

/// A custom artifact type id, registered on first use (Autopsy
/// `getOrAddArtifactType`).
pub(crate) fn custom_type(ctx: &JobContext, name: &str, display: &str) -> i64 {
    artifact_type_id(name).unwrap_or_else(|| {
        ctx.case
            .db()
            .artifact_type(name, display, Category::DataArtifact)
            .unwrap_or(0)
    })
}

/// Largest file read whole (JSON, plists, cookie files).
pub(crate) const MAX_SMALL: u64 = 64 << 20;

/// Reads a whole small file (`None` when larger than [`MAX_SMALL`] or
/// unreadable).
pub(crate) fn read_small(ctx: &JobContext, f: &FileRecord) -> Option<Vec<u8>> {
    if f.size > MAX_SMALL {
        tracing::warn!(file = %f.path(), size = f.size, "browser file too large, skipped");
        return None;
    }
    let src = ctx.case.content(f).ok()?;
    let n = usize::try_from(src.size()).ok()?;
    src.read_vec_at(0, n).ok()
}

/// Streams a file's content to `path`, polling for cancellation.
fn copy_to(ctx: &JobContext, f: &FileRecord, path: &Path) -> Result<(), ModuleError> {
    let src = ctx.case.content(f)?;
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(path)
            .map_err(|e| ModuleError::new(format!("{}: {e}", path.display())))?,
    );
    let mut buf = vec![0u8; 1 << 20];
    let mut off = 0u64;
    let size = src.size();
    while off < size {
        if ctx.is_cancelled() {
            return Err(ModuleError::new("cancelled"));
        }
        let n = src.read_at(off, &mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(buf.get(..n).unwrap_or_default())
            .map_err(|e| ModuleError::new(format!("{}: {e}", path.display())))?;
        off += n as u64;
    }
    out.flush()
        .map_err(|e| ModuleError::new(format!("{}: {e}", path.display())))?;
    Ok(())
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A temp copy of an SQLite store with its `-wal` / `-journal`
/// companions (Autopsy copies only the main file and so misses
/// uncommitted rows), opened hardened; deleted on drop.
pub(crate) struct TempDb {
    paths: Vec<PathBuf>,
    pub(crate) conn: rusqlite::Connection,
}

impl std::fmt::Debug for TempDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TempDb")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
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
        let mut paths = vec![
            base.clone(),
            PathBuf::from(format!("{}-shm", base.display())),
        ];
        // Clean up even when the copy fails half-way.
        let guard = |paths: Vec<PathBuf>| {
            for p in &paths {
                let _ = std::fs::remove_file(p);
            }
        };
        if let Err(e) = copy_to(ctx, f, &base) {
            guard(paths);
            return Err(e);
        }
        if let Some(parent) = f.parent_id {
            for suffix in ["-wal", "-journal"] {
                let want = format!("{}{suffix}", f.name);
                if let Ok(Some(s)) = ctx.case.db().child_named_nocase(parent, &want)
                    && s.size > 0
                    && s.stream.is_none()
                {
                    let p = PathBuf::from(format!("{}{suffix}", base.display()));
                    paths.push(p.clone());
                    if let Err(e) = copy_to(ctx, &s, &p) {
                        guard(paths);
                        return Err(e);
                    }
                }
            }
        }
        match pm_browser::sqlite::open(&base) {
            Ok(conn) => Ok(Self { paths, conn }),
            Err(e) => {
                guard(paths);
                Err(ModuleError::new(format!("{}: {e}", f.path())))
            }
        }
    }
}

/// An artifact under construction (attribute source "Recent Activity").
#[derive(Debug)]
pub(crate) struct WebArt {
    a: NewArtifact,
    /// Post a `TSK_ASSOCIATED_OBJECT` on this file pointing here.
    associate: Option<i64>,
}

fn std_attr(name: &str) -> i64 {
    attribute_type_id(name).unwrap_or(0)
}

impl WebArt {
    /// A data artifact of type `type_id` on `file_id`.
    pub(crate) fn of_type(ctx: &JobContext, type_id: i64, file_id: i64) -> Self {
        Self {
            a: NewArtifact {
                type_id,
                ds_id: ctx.ds_id,
                file_id: Some(file_id),
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: ctx.now(),
                attributes: Vec::new(),
            },
            associate: None,
        }
    }

    /// A data artifact of standard type `ty`.
    pub(crate) fn new(ctx: &JobContext, ty: &str, file_id: i64) -> Self {
        Self::of_type(ctx, artifact_type_id(ty).unwrap_or(0), file_id)
    }

    /// An analysis result with a score and justification.
    pub(crate) fn result(
        ctx: &JobContext,
        ty: &str,
        file_id: i64,
        significance: Significance,
        justification: String,
    ) -> Self {
        let mut s = Self::new(ctx, ty, file_id);
        s.a.score = Some((significance, Priority::Normal));
        s.a.justification = Some(justification);
        s
    }

    pub(crate) fn value(mut self, name: &str, v: Value) -> Self {
        self.a.attributes.push((std_attr(name), v, NAME.to_owned()));
        self
    }

    /// A text attribute (added even when empty).
    pub(crate) fn text(self, name: &str, v: impl Into<String>) -> Self {
        self.value(name, Value::Text(v.into()))
    }

    /// A text attribute, left out when empty.
    pub(crate) fn text_nonempty(self, name: &str, v: impl Into<String>) -> Self {
        let v = v.into();
        if v.is_empty() {
            self
        } else {
            self.text(name, v)
        }
    }

    /// A text attribute, left out when blank (Autopsy `isNotBlank`).
    pub(crate) fn text_nonblank(self, name: &str, v: impl Into<String>) -> Self {
        let v = v.into();
        if v.trim().is_empty() {
            self
        } else {
            self.text(name, v)
        }
    }

    /// A date-time attribute (Unix seconds), left out when absent.
    pub(crate) fn time(self, name: &str, v: Option<i64>) -> Self {
        match v {
            Some(t) => self.value(name, Value::DateTime(t)),
            None => self,
        }
    }

    pub(crate) fn int(self, name: &str, v: i64) -> Self {
        self.value(name, Value::Int(v))
    }

    pub(crate) fn associate(mut self, file_id: Option<i64>) -> Self {
        self.associate = file_id;
        self
    }

    /// Autopsy `createHistoryAttributes`.
    pub(crate) fn history(ctx: &JobContext, file_id: i64, h: &History<'_>) -> Self {
        Self::new(ctx, "TSK_WEB_HISTORY", file_id)
            .text("TSK_URL", h.url)
            .time("TSK_DATETIME_ACCESSED", h.time)
            .text_nonblank("TSK_REFERRER", h.referrer)
            .text_nonblank("TSK_TITLE", h.title)
            .text_nonblank("TSK_PROG_NAME", h.program)
            .text_if(!h.url.trim().is_empty(), "TSK_DOMAIN", h.domain)
            .text_nonblank("TSK_USER_NAME", h.user)
    }

    fn text_if(self, cond: bool, name: &str, v: &str) -> Self {
        if cond { self.text(name, v) } else { self }
    }
}

/// Inputs of Autopsy's `createHistoryAttributes`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct History<'a> {
    pub(crate) url: &'a str,
    pub(crate) time: Option<i64>,
    pub(crate) referrer: &'a str,
    pub(crate) title: &'a str,
    pub(crate) program: &'a str,
    pub(crate) domain: &'a str,
    pub(crate) user: &'a str,
}

/// Batched artifact writer: one transaction per [`Sink::FLUSH_EVERY`]
/// artifacts, `TSK_ASSOCIATED_OBJECT` links written with the ids.
#[derive(Debug, Default)]
pub(crate) struct Sink {
    items: Vec<WebArt>,
    pub(crate) written: usize,
}

impl Sink {
    const FLUSH_EVERY: usize = 2_000;

    pub(crate) fn push(&mut self, ctx: &JobContext, a: WebArt) -> Result<(), ModuleError> {
        self.items.push(a);
        if self.items.len() >= Self::FLUSH_EVERY {
            self.flush(ctx)?;
        }
        Ok(())
    }

    /// Writes everything collected so far.
    pub(crate) fn flush(&mut self, ctx: &JobContext) -> Result<(), ModuleError> {
        if self.items.is_empty() {
            return Ok(());
        }
        let items = std::mem::take(&mut self.items);
        let assoc_type = artifact_type_id("TSK_ASSOCIATED_OBJECT").unwrap_or(0);
        let assoc_attr = std_attr("TSK_ASSOCIATED_ARTIFACT");
        let now = ctx.now();
        let ds = ctx.ds_id;
        let n = ctx.case.db().bulk(|b| {
            let mut n = 0;
            for it in &items {
                let id = b.add_artifact(&it.a)?;
                n += 1;
                if let Some(target) = it.associate {
                    b.add_artifact(&NewArtifact {
                        type_id: assoc_type,
                        ds_id: ds,
                        file_id: Some(target),
                        source_artifact_id: None,
                        score: None,
                        conclusion: None,
                        configuration: None,
                        justification: None,
                        created_at: now,
                        attributes: vec![(assoc_attr, Value::Int(id), NAME.to_owned())],
                    })?;
                }
            }
            Ok(n)
        })?;
        self.written += n;
        Ok(())
    }
}

/// Runs `body` on an opened temp copy of a SQLite store, recording
/// Autopsy's "Error while trying to analyze file" line on failure.
pub(crate) fn with_db(
    ctx: &JobContext,
    web: &mut Web<'_>,
    who: &str,
    f: &FileRecord,
    body: impl FnOnce(&rusqlite::Connection) -> Result<(), ModuleError>,
) -> Result<(), ModuleError> {
    match TempDb::open(ctx, f) {
        Ok(db) => body(&db.conn),
        Err(e) => {
            if !ctx.is_cancelled() {
                web.error(format!(
                    "{who}: Error while trying to analyze file:{}",
                    f.name
                ));
                tracing::debug!(file = %f.path(), error = %e.0, "browser database not readable");
            }
            Ok(())
        }
    }
}

/// Logs (Autopsy returns no rows on) an SQL error of a store query.
pub(crate) fn sql_soft(f: &FileRecord, r: rusqlite::Result<()>) {
    if let Err(e) = r {
        tracing::debug!(file = %f.path(), error = %e, "browser query failed (schema mismatch?)");
    }
}

/// Runs a store query and pushes the artifact `make` returns for each
/// row, stopping on the first write error or on cancellation (polled
/// every 1024 rows). SQL errors end the scan quietly, as in Autopsy.
pub(crate) fn push_rows<T>(
    ctx: &JobContext,
    f: &FileRecord,
    sink: &mut Sink,
    query: impl FnOnce(&mut dyn FnMut(T) -> bool) -> rusqlite::Result<()>,
    mut make: impl FnMut(T) -> Option<WebArt>,
) -> Result<(), ModuleError> {
    let mut err = None;
    let mut n = 0u64;
    let r = query(&mut |row: T| {
        n += 1;
        if n.is_multiple_of(1024) && ctx.is_cancelled() {
            return false;
        }
        if let Some(a) = make(row)
            && let Err(e) = sink.push(ctx, a)
        {
            err = Some(e);
            return false;
        }
        true
    });
    sql_soft(f, r);
    err.map_or(Ok(()), Err)
}
