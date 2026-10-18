//! "Logical Files" data sources (Autopsy `LocalFilesDSProcessor` with
//! `FileManager.addLocalFilesDataSource`): files and folders of the
//! examiner's machine added as a logical file set. Files are read in place
//! from their absolute paths (they are not copied into the case).
//!
//! As in Autopsy, the set is named `LogicalFileSet<N>` unless named, every
//! path must exist and be readable, folders are added recursively, and the
//! modified / created / accessed times are recorded only when asked for
//! (copying a file usually changes them). Symbolic links and special files
//! (devices, sockets, pipes) are not followed: they would pull in files
//! outside the selection or block on read.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use pm_casedb::{
    CaseDb, DataSourceKind, FileKindCode, NewDataSource, NewFile, Origin, default_host_name,
};

use crate::load::{LoadProgress, LoadSummary};
use crate::{Case, CaseError, Result, io_err};

/// Options of a logical file set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogicalOptions {
    /// Display name; default `LogicalFileSet<N>`.
    pub name: Option<String>,
    /// Record the file's modified time.
    pub modified: bool,
    /// Record the file's creation time.
    pub created: bool,
    /// Record the file's last access time.
    pub accessed: bool,
    /// Host the set belongs to; default `"<name> Host"`.
    pub host: Option<String>,
}

/// Files written per transaction.
const BATCH: usize = 5_000;

/// Adds `paths` (files and folders) as one logical file set.
pub fn add_logical_files(
    case: &Case,
    paths: &[PathBuf],
    opts: &LogicalOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    if paths.is_empty() {
        return Err(CaseError::NotFound("no files or folders to add".into()));
    }
    for p in paths {
        let md = std::fs::metadata(p).map_err(io_err(p))?;
        if md.is_file() {
            std::fs::File::open(p).map_err(io_err(p))?;
        } else if md.is_dir() {
            std::fs::read_dir(p).map_err(io_err(p))?;
        }
    }
    let db = case.db();
    let name = match &opts.name {
        Some(n) if !n.trim().is_empty() => n.trim().to_owned(),
        _ => next_set_name(db)?,
    };
    progress.set_stage("Adding logical files");
    let ds_id = db.bulk(|b| {
        let ds_id = b.add_data_source(&NewDataSource {
            name: name.clone(),
            kind: DataSourceKind::LogicalFiles,
            paths: paths.iter().map(|p| p.display().to_string()).collect(),
            size: 0,
            sector_size: 0,
            timezone: String::new(),
            image_format: None,
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: Vec::new(),
            added_at: jiff::Timestamp::now().as_second(),
        })?;
        let host = opts
            .host
            .clone()
            .unwrap_or_else(|| default_host_name(Some(&name), ds_id));
        b.assign_host(ds_id, &host)?;
        Ok(ds_id)
    })?;
    let mut summary = LoadSummary {
        ds_id,
        fs_ids: Vec::new(),
        files: 0,
        warnings: Vec::new(),
    };
    let mut batch: Vec<(NewFile, Option<PathBuf>)> = Vec::new();
    let mut total_size = 0u64;
    // (path, parent row id, parent path) to visit; parents are written
    // before their children, so ids are known.
    let mut stack: Vec<(PathBuf, Option<i64>, String)> = paths
        .iter()
        .rev()
        .map(|p| (p.clone(), None, "/".to_owned()))
        .collect();
    while let Some((path, parent_id, parent_path)) = stack.pop() {
        if progress.is_cancelled() {
            break;
        }
        let md = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                summary.warnings.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let ft = md.file_type();
        if !(ft.is_file() || ft.is_dir()) {
            summary.warnings.push(format!(
                "{}: skipped (symbolic link or special file)",
                path.display()
            ));
            continue;
        }
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let time = |t: std::io::Result<SystemTime>, wanted: bool| -> Option<(i64, u32)> {
            if !wanted {
                return None;
            }
            let d = t.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
            Some((i64::try_from(d.as_secs()).ok()?, d.subsec_nanos()))
        };
        if ft.is_file() && parent_id.is_none() && pm_image::ewf::is_logical_evidence(&path) {
            // An L01 is extracted and added as the files it holds.
            flush(db, &mut batch, &mut summary)?;
            let added = crate::l01::add_l01(case, ds_id, &path, progress, &mut summary)?;
            total_size = total_size.saturating_add(added);
            continue;
        }
        let size = if ft.is_file() { md.len() } else { 0 };
        let mut f = NewFile::new(
            ds_id,
            parent_id,
            &parent_path,
            &name,
            Origin::Local,
            if ft.is_dir() {
                FileKindCode::Directory
            } else {
                FileKindCode::Regular
            },
        );
        f.size = size;
        f.times = [
            time(md.modified(), opts.modified),
            time(md.accessed(), opts.accessed),
            None,
            time(md.created(), opts.created),
        ];
        f.local_path = Some(absolute(&path).display().to_string());
        total_size = total_size.saturating_add(size);
        if ft.is_dir() {
            // Directories are written at once: their children need the id.
            flush(db, &mut batch, &mut summary)?;
            let id = db.bulk(|b| b.add_file(&f))?;
            summary.files += 1;
            progress
                .dirs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let child_path = format!("{parent_path}{name}/");
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    let mut children: Vec<PathBuf> =
                        entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
                    // Deterministic order (the file system's is not).
                    children.sort();
                    for c in children.into_iter().rev() {
                        stack.push((c, Some(id), child_path.clone()));
                    }
                }
                Err(e) => summary.warnings.push(format!("{}: {e}", path.display())),
            }
        } else {
            batch.push((f, None));
            if batch.len() >= BATCH {
                flush(db, &mut batch, &mut summary)?;
            }
        }
        progress.files.store(
            summary.files + batch.len() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    flush(db, &mut batch, &mut summary)?;
    db.set_data_source_size(ds_id, total_size)?;
    if progress.is_cancelled() {
        case.delete_data_source(ds_id)?;
        return Err(CaseError::Cancelled);
    }
    progress.set_stage("Done");
    Ok(summary)
}

fn flush(
    db: &CaseDb,
    batch: &mut Vec<(NewFile, Option<PathBuf>)>,
    summary: &mut LoadSummary,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    db.bulk(|b| {
        for (f, _) in batch.iter() {
            b.add_file(f)?;
        }
        Ok(())
    })?;
    summary.files += batch.len() as u64;
    batch.clear();
    Ok(())
}

/// `LogicalFileSet<N+1>`, N the number of sets already named so.
fn next_set_name(db: &CaseDb) -> Result<String> {
    let n = db
        .data_sources()?
        .iter()
        .filter(|d| d.name.starts_with("LogicalFileSet"))
        .count();
    Ok(format!("LogicalFileSet{}", n + 1))
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}
