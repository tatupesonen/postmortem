//! Autopsy's "Save Tagged Hashes" report module (`SaveTaggedHashesToHashDb`):
//! adds the MD5 of every file carrying one of the chosen tags to a hash set
//! (a writable `.kdb`, created when missing), with the file name and the
//! case name as the comment.

use std::path::{Path, PathBuf};

use pm_case::Case;

use crate::{Progress, ReportError, ReportSettings};

/// The tags to export and the hash set to add them to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaggedHashesSettings {
    pub tag_names: Vec<String>,
    pub hash_set: PathBuf,
}

fn hash_err(path: &Path, e: impl std::fmt::Display) -> ReportError {
    ReportError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other(e.to_string()),
    }
}

/// Adds the hashes; returns the hash set path and the number of hashes
/// added (files without an MD5, i.e. not yet hashed, are skipped).
pub fn write(
    case: &Case,
    settings: &ReportSettings,
    hashes: &TaggedHashesSettings,
    progress: &dyn Progress,
) -> Result<(PathBuf, u64), ReportError> {
    let path = &hashes.hash_set;
    let db_set = if path.exists() {
        pm_hashdb::HashDb::open(path, false).map_err(|e| hash_err(path, e))?
    } else {
        pm_hashdb::HashDb::create(path).map_err(|e| hash_err(path, e))?
    };
    if !db_set.is_updateable() {
        return Err(hash_err(
            path,
            "the hash set cannot be added to (only .kdb sets can)",
        ));
    }
    let comment = case.details().name.clone();
    let db = case.db();
    let mut added = 0;
    for t in db.tag_names()? {
        if !hashes.tag_names.contains(&t.name) {
            continue;
        }
        progress.status(&format!("Adding hashes of files tagged {}...", t.name));
        for (_, f) in db.files_with_tag(t.id)? {
            if progress.is_cancelled() {
                return Err(ReportError::Cancelled);
            }
            if !settings.data_sources.is_empty() && !settings.data_sources.contains(&f.ds_id) {
                continue;
            }
            let Some(md5) = f.md5.as_deref().and_then(|m| <[u8; 16]>::try_from(m).ok()) else {
                continue;
            };
            db_set
                .add(
                    &md5,
                    f.sha1.as_deref(),
                    f.sha256.as_deref(),
                    Some(&f.name),
                    Some(&comment),
                )
                .map_err(|e| hash_err(path, e))?;
            added += 1;
        }
    }
    Ok((path.clone(), added))
}
