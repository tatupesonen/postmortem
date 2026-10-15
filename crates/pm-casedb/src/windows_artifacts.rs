//! File lookups used by the Windows-artefact extractors (Recent Activity):
//! case-insensitive child/path resolution and indexed extension queries.

use rusqlite::{OptionalExtension, params};

use crate::{CaseDb, FILE_COLS, FileRecord, Result};

impl CaseDb {
    /// The child of `parent_id` named `name` (ASCII case-insensitive), using
    /// the `(parent_id, name)` index.
    pub fn child_named(&self, parent_id: i64, name: &str) -> Result<Option<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE parent_id = ?1 AND name = ?2 COLLATE NOCASE ORDER BY id LIMIT 1"
            ))?;
            Ok(st
                .query_row(params![parent_id, name], FileRecord::from_row)
                .optional()?)
        })
    }

    /// Resolves a `/`- or `\`-separated path below the root of every file
    /// system of the data source (a leading drive letter such as `C:` is
    /// ignored; components compare case-insensitively). One match per file
    /// system at most.
    pub fn resolve_path(&self, ds_id: i64, path: &str) -> Result<Vec<FileRecord>> {
        let mut parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
        if parts
            .first()
            .is_some_and(|p| p.len() == 2 && p.ends_with(':'))
        {
            parts.remove(0);
        }
        let mut out = Vec::new();
        for fs in self.file_systems(ds_id)? {
            let Some(mut cur) = self.fs_root(fs.id)? else {
                continue;
            };
            let mut found = true;
            for p in &parts {
                if let Some(c) = self.child_named(cur.id, p)? {
                    cur = c;
                } else {
                    found = false;
                    break;
                }
            }
            if found && !parts.is_empty() {
                out.push(cur);
            }
        }
        Ok(out)
    }

    /// Files of a data source (regular files and their named streams) with
    /// extension `ext` (`""` for none) whose name matches the SQL `LIKE`
    /// pattern `name_like` (ASCII case-insensitive), after id `after_id`.
    /// Uses the extension index; page with the last id.
    pub fn files_by_extension(
        &self,
        ds_id: i64,
        ext: &str,
        name_like: Option<&str>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE extension = ?1 AND ds_id = ?2 AND kind = 1 AND id > ?3 \
                 AND (?4 IS NULL OR name LIKE ?4) ORDER BY id LIMIT {limit}"
            ))?;
            let rows = st
                .query_map(params![ext.to_lowercase(), ds_id, after_id, name_like], FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
