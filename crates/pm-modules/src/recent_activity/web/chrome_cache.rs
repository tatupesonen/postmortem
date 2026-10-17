//! Chrome blockfile cache (Autopsy `ChromeCacheExtractor`): every
//! `index` whose path contains `default/cache` with `data_0`..`data_3`
//! beside it. Each entry with a payload stream becomes a `KAAMOS_WEB_CACHE`
//! artifact on the block file holding the entry; the cached item is the
//! `f_xxxxxx` file, or a derived file `data_N__<address>` extracted from
//! the block file, linked by `KAAMOS_ASSOCIATED_OBJECT`. Brotli-encoded items
//! get the MIME type `application/x-brotli`.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;

use pm_browser::chrome_cache::{Cache, CacheFiles, Entry, Payload};
use pm_browser::domain::extract_domain;
use pm_casedb::{FileAnalysis, FileRecord};
use pm_core::ReadAt;
use pm_ingest::{JobContext, ModuleError};

use super::{Sink, Web, WebArt, done_std};
use crate::derive::{DerivedTree, Existing, Times};

const BROTLI: &str = "application/x-brotli";
/// Derived-file output folder (`ModuleOutput/RecentActivity/<data_N id>/`).
const FOLDER: &str = "RecentActivity";
/// Entries handled per batch of derived files and artifacts.
const CHUNK: usize = 1_000;

/// The files of one cache folder, looked up by name on demand (allocated
/// copies preferred, then the lowest id, as Autopsy does).
struct Folder<'c> {
    ctx: &'c JobContext,
    parent: i64,
    cache: std::cell::RefCell<HashMap<String, Option<FileRecord>>>,
}

impl Folder<'_> {
    fn record(&self, name: &str) -> Option<FileRecord> {
        if let Some(r) = self.cache.borrow().get(name) {
            return r.clone();
        }
        let mut found = self
            .ctx
            .case
            .db()
            .children_named(self.parent, name)
            .unwrap_or_default();
        found.sort_by_key(|f| (!f.meta_alloc, f.id));
        let r = found
            .into_iter()
            .find(|f| f.size > 0 || name.starts_with("f_"));
        let mut c = self.cache.borrow_mut();
        // Only block files are cached (there may be very many f_ files).
        if !name.starts_with("f_") || c.len() < 64 {
            c.insert(name.to_owned(), r.clone());
        }
        r
    }
}

impl CacheFiles for Folder<'_> {
    fn open(&self, name: &str) -> Option<Arc<dyn ReadAt>> {
        let r = self.record(name)?;
        self.ctx.case.content(&r).ok()
    }
}

/// A `Read` over a byte range of a source.
struct RangeReader {
    src: Arc<dyn ReadAt>,
    pos: u64,
    end: u64,
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.end.saturating_sub(self.pos);
        let n = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        if n == 0 {
            return Ok(0);
        }
        let got = self
            .src
            .read_at(self.pos, buf.get_mut(..n).unwrap_or_default())?;
        self.pos += got as u64;
        Ok(got)
    }
}

pub(crate) fn extract(ctx: &JobContext, web: &mut Web<'_>) {
    let indexes: Vec<FileRecord> = web
        .files
        .iter()
        .filter(|f| {
            f.name.eq_ignore_ascii_case("index")
                && f.parent_path.to_lowercase().contains("default/cache")
        })
        .cloned()
        .collect();
    for index in indexes {
        if ctx.is_cancelled() {
            break;
        }
        let Some(parent) = index.parent_id else {
            continue;
        };
        web.progress(format!(
            "Chrome Cache: Loading files from {}.",
            index.parent_path
        ));
        if let Err(e) = folder(ctx, web, parent) {
            tracing::warn!(folder = %index.parent_path, error = %e.0, "Chrome cache not processed");
        }
    }
}

fn folder(ctx: &JobContext, web: &mut Web<'_>, parent: i64) -> Result<(), ModuleError> {
    let files = Folder {
        ctx,
        parent,
        cache: std::cell::RefCell::new(HashMap::new()),
    };
    // Re-run: the folder's block files already carry the artifacts.
    for i in 0..4 {
        if let Some(r) = files.record(&format!("data_{i}"))
            && done_std(ctx, &r, "KAAMOS_WEB_CACHE")
        {
            return Ok(());
        }
    }
    let mut cache = match Cache::open(&files) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(%e, "not a usable Chrome cache folder");
            return Ok(());
        }
    };
    let total = cache.table_len();
    let mut chunk: Vec<Entry> = Vec::new();
    let mut sink = Sink::default();
    let mut err = None;
    let mut n = 0u64;
    cache
        .entries(|e| {
            n += 1;
            if n.is_multiple_of(256) {
                if ctx.is_cancelled() {
                    return false;
                }
                web.progress(format!(
                    "Chromium Analyzer: Extracting cache entry {n} of {total} entries"
                ));
            }
            if e.payload.is_some() {
                chunk.push(e);
            }
            if chunk.len() >= CHUNK
                && let Err(x) = store(ctx, &files, &mut chunk, &mut sink)
            {
                err = Some(x);
                return false;
            }
            true
        })
        .map_err(|e| ModuleError::new(e.to_string()))?;
    if let Some(e) = err {
        return Err(e);
    }
    store(ctx, &files, &mut chunk, &mut sink)?;
    sink.flush(ctx)
}

/// Extracts the in-block payloads of `chunk` as derived files and writes
/// the artifacts of every entry.
fn store(
    ctx: &JobContext,
    files: &Folder<'_>,
    chunk: &mut Vec<Entry>,
    sink: &mut Sink,
) -> Result<(), ModuleError> {
    // Cached item (id, path) per entry index.
    let mut items: Vec<Option<(i64, String)>> = vec![None; chunk.len()];
    // In-block payloads grouped by block file.
    let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, e) in chunk.iter().enumerate() {
        match &e.payload {
            Some(Payload::External { file, .. }) => {
                if let Some(r) = files.record(file) {
                    if e.is_brotli() {
                        let _ = ctx.case.db().set_file_analysis(
                            r.id,
                            &FileAnalysis {
                                mime: Some(BROTLI.to_owned()),
                                ..FileAnalysis::default()
                            },
                        );
                    }
                    if let Some(slot) = items.get_mut(i) {
                        *slot = Some((r.id, r.path()));
                    }
                }
            }
            Some(Payload::Block { file, .. }) => by_file.entry(file.clone()).or_default().push(i),
            None => {}
        }
    }
    for (file, idxs) in by_file {
        if ctx.is_cancelled() {
            break;
        }
        let Some(rec) = files.record(&file) else {
            continue;
        };
        let src = ctx.case.content(&rec)?;
        let mut tree = DerivedTree::new(ctx.case.dir(), FOLDER, &rec);
        let mut nodes = Vec::new();
        for &i in &idxs {
            let Some(e) = chunk.get(i) else { continue };
            let (Some(Payload::Block { offset, len, .. }), Some(name)) = (
                &e.payload,
                e.payload.as_ref().and_then(Payload::derived_name),
            ) else {
                continue;
            };
            let end = offset.saturating_add(*len).min(src.size());
            let mut r = RangeReader {
                src: src.clone(),
                pos: *offset,
                end,
            };
            let node = tree.new_child(tree.root(), &name);
            let t = Some(e.created);
            tree.write_file(
                node,
                &name,
                &mut r,
                Times {
                    modified: t,
                    accessed: t,
                    created: t,
                },
            )?;
            nodes.push((node, i, name, e.is_brotli()));
        }
        let mut ids = HashMap::new();
        let brotli: HashMap<_, _> = nodes.iter().map(|(n, _, _, b)| (*n, *b)).collect();
        tree.commit(ctx, &Existing::default(), &mut |node, id| {
            if brotli.get(&node).copied().unwrap_or(false) {
                let _ = ctx.case.db().set_file_analysis(
                    id,
                    &FileAnalysis {
                        mime: Some(BROTLI.to_owned()),
                        ..FileAnalysis::default()
                    },
                );
            }
            ids.insert(node, id);
        })?;
        for (node, i, name, _) in nodes {
            if let (Some(id), Some(slot)) = (ids.get(&node), items.get_mut(i)) {
                *slot = Some((*id, format!("{}/{name}", rec.path())));
            }
        }
    }
    for (e, item) in chunk.drain(..).zip(items) {
        let Some((item_id, item_path)) = item else {
            continue;
        };
        let Some(entry_file) = files.record(&e.entry_file) else {
            continue;
        };
        sink.push(
            ctx,
            WebArt::new(ctx, "KAAMOS_WEB_CACHE", entry_file.id)
                .text("KAAMOS_URL", &e.key)
                .text("KAAMOS_DOMAIN", extract_domain(&e.key))
                .time("KAAMOS_DATETIME_CREATED", Some(e.created))
                .text("KAAMOS_HEADERS", e.headers_text())
                .text("KAAMOS_PATH", item_path)
                .int("KAAMOS_PATH_ID", item_id)
                .associate(Some(item_id)),
        )?;
    }
    Ok(())
}
