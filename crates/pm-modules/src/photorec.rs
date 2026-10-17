//! PhotoRec Carver (Autopsy `modules/photoreccarver`, inventory 03).
//!
//! Carves the unallocated-space files of a data source with the native
//! carver in `pm-carve` instead of running the PhotoRec executable. As in
//! Autopsy, carved files are virtual: `files` rows of origin "carved"
//! whose content is a list of disk-image byte ranges (no copy is made),
//! named the way PhotoRec names them (`f<sector>.<ext>`), placed in
//! `$CarvedFiles/<n>/` (at most 2000 per folder) under the root of the
//! file system the space belongs to, and scheduled into the running job.
//!
//! Settings and start-up checks are Autopsy's: "keep corrupted files",
//! and an include/exclude list of PhotoRec file-type names validated
//! against PhotoRec's list; carving needs a file filter that processes
//! unallocated space.
//!
//! Differences from Autopsy: no copy of the unallocated space is written
//! to the case's temporary folder, and re-running ingest does not carve an
//! unallocated-space file again (Autopsy adds the same files twice).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use pm_carve::{Carved, Filter, KNOWN_EXTENSIONS};
use pm_casedb::{FileKindCode, FileRecord, NewFile, Origin};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

pub const NAME: &str = "PhotoRec Carver";
/// Virtual folder of carved files (TSK `VirtualDirectory.NAME_CARVED`).
pub const CARVED_DIR: &str = "$CarvedFiles";
/// Carved files per numbered sub-folder (TSK
/// `MAX_CARVED_FILES_PER_FOLDER`).
pub const FILES_PER_FOLDER: u64 = 2000;
/// Rows inserted per transaction.
const BATCH: usize = 1000;

/// Autopsy's extension filter option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExtensionFilter {
    #[default]
    NoFilter,
    Include,
    Exclude,
}

/// Per-job settings (`PhotoRecCarverIngestJobSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub keep_corrupted_files: bool,
    pub extension_filter: ExtensionFilter,
    /// PhotoRec file-type names (`jpg`, `zip`, …).
    pub extensions: Vec<String>,
}

#[derive(Debug, Default)]
pub struct PhotoRecCarverFactory;

impl ModuleFactory for PhotoRecCarverFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "photorec_carver",
            name: NAME,
            description: "Runs PhotoRec carver against unallocated space in the data source.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(Settings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let s: Settings = if settings.is_null() {
            Settings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("{NAME} settings: {e}")))?
        };
        let opts = carve_options(&s)?;
        if !ctx.process_unallocated {
            return Err(ModuleError::new(
                "The selected file ingest filter ignores unallocated space. This module carves unallocated space. Please choose a filter which does not ignore unallocated space or disable this module.",
            ));
        }
        Ok(Arc::new(JobHandle(Arc::new(Job {
            opts,
            folders: Mutex::new(HashMap::new()),
            carved: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
        }))))
    }
}

/// Validates the settings (Autopsy `startUp`) and builds carving options.
pub fn carve_options(s: &Settings) -> Result<pm_carve::Options, ModuleError> {
    let list: Vec<String> = s
        .extensions
        .iter()
        .map(|e| e.trim().to_owned())
        .filter(|e| !e.is_empty())
        .collect();
    if s.extension_filter == ExtensionFilter::Include && list.is_empty() {
        return Err(ModuleError::new(
            "No extensions provided for PhotoRec to carve.",
        ));
    }
    if s.extension_filter != ExtensionFilter::NoFilter {
        let invalid: Vec<&str> = list
            .iter()
            .map(String::as_str)
            .filter(|e| !KNOWN_EXTENSIONS.contains(e))
            .collect();
        if !invalid.is_empty() {
            return Err(ModuleError::new(format!(
                "The following extensions are invalid: {}",
                invalid.join(", ")
            )));
        }
    }
    let set = list.into_iter().collect();
    let filter = match s.extension_filter {
        ExtensionFilter::NoFilter => Filter::All,
        ExtensionFilter::Include => Filter::Include(set),
        ExtensionFilter::Exclude => Filter::Exclude(set),
    };
    Ok(pm_carve::Options {
        keep_corrupted: s.keep_corrupted_files,
        filter,
        ..pm_carve::Options::default()
    })
}

/// The `$CarvedFiles` folder of one file system and its current
/// numbered sub-folder.
#[derive(Debug)]
struct Folders {
    base_id: i64,
    base_path: String,
    cur_id: i64,
    cur_number: u64,
    cur_count: u64,
}

impl Folders {
    fn cur_path(&self) -> String {
        format!("{}{}/", self.base_path, self.cur_number)
    }
}

struct Job {
    opts: pm_carve::Options,
    /// By file-system root directory id.
    folders: Mutex<HashMap<i64, Folders>>,
    carved: AtomicU64,
    errors: AtomicU64,
    nanos: AtomicU64,
}

struct JobHandle(Arc<Job>);

impl ModuleJob for JobHandle {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            job: Arc::clone(&self.0),
        }))
    }

    /// Autopsy's summary message ("PhotoRec Results").
    fn finish(&self, ctx: &JobContext) {
        let j = &self.0;
        let ms = j.nanos.load(Ordering::Relaxed) / 1_000_000;
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "PhotoRec Results").details(format!(
                "Number of Files Carved: {}\nNumber of Errors while Carving: {}\nTotal Carving Time: {ms} ms",
                j.carved.load(Ordering::Relaxed),
                j.errors.load(Ordering::Relaxed),
            )),
        );
    }
}

struct Worker {
    job: Arc<Job>,
}

/// The path of a directory row as a parent path (`/`-terminated).
fn dir_path(rec: &FileRecord) -> String {
    let p = rec.path();
    if p.ends_with('/') { p } else { format!("{p}/") }
}

fn virtual_dir(rec_parent: &FileRecord, name: &str) -> NewFile {
    let mut f = NewFile::new(
        rec_parent.ds_id,
        Some(rec_parent.id),
        dir_path(rec_parent),
        name,
        Origin::VirtualDir,
        FileKindCode::Directory,
    );
    f.fs_id = rec_parent.fs_id;
    f
}

impl Job {
    /// Hands out `n` slots in `$CarvedFiles/<k>/` folders of the file
    /// system `root`, creating folders as they fill up: returns
    /// `(folder id, folder path, count)` runs.
    fn slots(
        &self,
        ctx: &JobContext,
        root: &FileRecord,
        mut n: u64,
    ) -> Result<Vec<(i64, String, u64)>, ModuleError> {
        let mut map = self.folders.lock();
        let f = match map.entry(root.id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Self::open_folders(ctx, root)?)
            }
        };
        let mut out = Vec::new();
        while n > 0 {
            if f.cur_count >= FILES_PER_FOLDER {
                let next = f.cur_number + 1;
                let base = ctx
                    .case
                    .db()
                    .file(f.base_id)?
                    .ok_or_else(|| ModuleError::new("$CarvedFiles disappeared"))?;
                let rec = ctx
                    .insert_derived_files(vec![virtual_dir(&base, &next.to_string())])?
                    .pop()
                    .ok_or_else(|| ModuleError::new("carved files folder"))?;
                f.cur_id = rec.id;
                f.cur_number = next;
                f.cur_count = 0;
            }
            let take = n.min(FILES_PER_FOLDER - f.cur_count);
            out.push((f.cur_id, f.cur_path(), take));
            f.cur_count += take;
            n -= take;
        }
        Ok(out)
    }

    /// Finds or creates `$CarvedFiles` and its most recent numbered
    /// sub-folder (TSK `addCarvedFiles`).
    fn open_folders(ctx: &JobContext, root: &FileRecord) -> Result<Folders, ModuleError> {
        let db = ctx.case.db();
        let base = if let Some(b) = db.child_named(root.id, CARVED_DIR)? {
            b
        } else {
            let mut recs = ctx.insert_derived_files(vec![virtual_dir(root, CARVED_DIR)])?;
            recs.pop()
                .ok_or_else(|| ModuleError::new("carved files folder"))?
        };
        let latest = db
            .children(base.id)?
            .into_iter()
            .filter_map(|c| c.name.parse::<u64>().ok().map(|n| (n, c)))
            .max_by_key(|(_, c)| c.id);
        let (cur_number, cur) = if let Some(x) = latest {
            x
        } else {
            let rec = ctx
                .insert_derived_files(vec![virtual_dir(&base, "1")])?
                .pop()
                .ok_or_else(|| ModuleError::new("carved files folder"))?;
            (1, rec)
        };
        let cur_count = db.count_children(cur.id)?;
        Ok(Folders {
            base_id: base.id,
            base_path: dir_path(&base),
            cur_id: cur.id,
            cur_number,
            cur_count,
        })
    }
}

/// The root directory of the file system an unallocated-space file
/// belongs to (`/$Unalloc/Unalloc_…` → `/`), else its parent.
fn fs_root(ctx: &JobContext, unalloc: &FileRecord) -> Result<FileRecord, ModuleError> {
    let db = ctx.case.db();
    let mut cur = unalloc.parent_id;
    let mut last = None;
    for _ in 0..8 {
        let Some(id) = cur else { break };
        let Some(rec) = db.file(id)? else { break };
        cur = rec.parent_id;
        let is_root = rec.parent_id.is_none() || rec.path() == "/";
        last = Some(rec);
        if is_root {
            break;
        }
    }
    last.ok_or_else(|| ModuleError::new("unallocated space file without a parent"))
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        if file.rec.origin != Origin::UnallocBlocks {
            return Ok(());
        }
        let db = ctx.case.db();
        if !db.derived_files_of(file.rec.id, 0, 1)?.is_empty() {
            // Carved by an earlier run.
            return Ok(());
        }
        let layout = db
            .file_layout(file.rec.id)?
            .and_then(|j| pm_case::parse_layout(&j))
            .ok_or_else(|| ModuleError::new(format!("no layout for {}", file.rec.path())))?;
        let src = file.content()?;
        let started = Instant::now();
        let mut found: Vec<Carved> = Vec::new();
        let res = pm_carve::carve(&*src, &self.job.opts, &|| ctx.is_cancelled(), &mut |c| {
            found.push(c);
        });
        self.job
            .nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let stats = match res {
            Ok(s) => s,
            Err(e) => {
                self.job.errors.fetch_add(1, Ordering::Relaxed);
                ctx.post_message(
                    IngestMessage::new(
                        MessageKind::Error,
                        NAME,
                        format!("Error processing {} with PhotoRec carver.", file.rec.name),
                    )
                    .details(e.to_string())
                    .file(file.rec.id),
                );
                return Ok(());
            }
        };
        if stats.cancelled || ctx.is_cancelled() || found.is_empty() {
            return Ok(());
        }
        // Files whose range maps to no image bytes are dropped.
        let rows: Vec<(Carved, String)> = found
            .into_iter()
            .filter_map(|c| {
                let ranges = pm_carve::map_to_image(&layout, c.offset, c.len);
                (!ranges.is_empty()).then(|| (c, pm_case::layout_json(&ranges)))
            })
            .collect();
        let root = fs_root(ctx, &file.rec)?;
        let mut rows = rows.into_iter();
        for (folder, path, n) in self.job.slots(ctx, &root, rows.len() as u64)? {
            let mut batch = Vec::with_capacity(BATCH);
            for (c, layout) in rows.by_ref().take(n as usize) {
                let mut f = NewFile::new(
                    file.rec.ds_id,
                    Some(folder),
                    path.clone(),
                    pm_carve::photorec_name(c.offset, c.ext),
                    Origin::Carved,
                    FileKindCode::Regular,
                );
                f.fs_id = file.rec.fs_id;
                f.name_alloc = false;
                f.meta_alloc = false;
                f.size = c.len;
                f.layout = Some(layout);
                f.derived_from = Some(file.rec.id);
                batch.push(f);
                if batch.len() >= BATCH {
                    self.add(ctx, std::mem::take(&mut batch))?;
                }
            }
            self.add(ctx, batch)?;
        }
        Ok(())
    }
}

impl Worker {
    fn add(&self, ctx: &JobContext, batch: Vec<NewFile>) -> Result<(), ModuleError> {
        if batch.is_empty() {
            return Ok(());
        }
        let recs = ctx.add_derived_files(batch)?;
        self.job
            .carved
            .fetch_add(recs.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_validated_like_autopsy() {
        let s = Settings {
            extension_filter: ExtensionFilter::Include,
            ..Settings::default()
        };
        assert_eq!(
            carve_options(&s).unwrap_err().0,
            "No extensions provided for PhotoRec to carve."
        );
        let s = Settings {
            extension_filter: ExtensionFilter::Exclude,
            extensions: vec!["jpg".into(), "JPG".into(), " nope ".into(), String::new()],
            ..Settings::default()
        };
        assert_eq!(
            carve_options(&s).unwrap_err().0,
            "The following extensions are invalid: JPG, nope"
        );
        let s = Settings {
            extension_filter: ExtensionFilter::Include,
            extensions: vec!["jpg".into(), "png".into()],
            keep_corrupted_files: true,
        };
        let o = carve_options(&s).unwrap();
        assert!(o.keep_corrupted);
        assert!(matches!(o.filter, Filter::Include(ref x) if x.len() == 2));
        assert_eq!(
            carve_options(&Settings::default()).unwrap().filter,
            Filter::All
        );
    }
}
