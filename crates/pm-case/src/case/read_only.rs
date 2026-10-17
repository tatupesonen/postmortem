//! Opening a case without write access (the MCP server, viewers).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use pm_casedb::CaseDb;

use super::{CASE_FILE, Case, CaseFile, DB_FILE, FORMAT, FORMAT_VERSION};
use crate::{CaseError, Result, io_err};

impl Case {
    /// Opens a case from its folder or `case.pmcase` without writing
    /// anything: the database is opened read-only (no migrations, no
    /// default tags), sub-folders are not created. The GUI can keep
    /// writing the case at the same time (SQLite WAL).
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let file = dir.join(CASE_FILE);
        let text = std::fs::read_to_string(&file).map_err(io_err(&file))?;
        let bad = |reason: String| CaseError::BadCaseFile {
            path: file.clone(),
            reason,
        };
        let meta: CaseFile = toml::from_str(&text).map_err(|e| bad(e.to_string()))?;
        if meta.format != FORMAT {
            return Err(bad(format!("unknown format {:?}", meta.format)));
        }
        if meta.version > FORMAT_VERSION {
            return Err(bad(format!(
                "case file version {} is newer than this postmortem supports ({FORMAT_VERSION})",
                meta.version
            )));
        }
        let db_path = dir.join(DB_FILE);
        if !db_path.is_file() {
            return Err(bad(format!("missing {DB_FILE}")));
        }
        let db = CaseDb::open_read_only(&db_path)?;
        tracing::info!(dir = %dir.display(), "case opened read-only");
        Ok(Self {
            dir,
            meta: RwLock::new(meta),
            db: Arc::new(db),
            sources: Mutex::new(HashMap::new()),
        })
    }

    /// Whether the case was opened with [`Case::open_read_only`].
    pub fn is_read_only(&self) -> bool {
        self.db.is_read_only()
    }
}

#[cfg(test)]
mod tests {
    use crate::{Case, CaseDetails};

    #[test]
    fn read_only_case_sees_data_and_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c");
        let case = Case::create(
            &p,
            CaseDetails {
                name: "RO".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!case.is_read_only());
        let ro = Case::open_read_only(&p).unwrap();
        assert!(ro.is_read_only());
        assert_eq!(ro.details().name, "RO");
        assert!(ro.db().tag_names().unwrap().len() >= 3);
        assert!(ro.db().add_tag_name("x", "", false).is_err());
        assert!(Case::open_read_only(dir.path().join("missing")).is_err());
    }
}
