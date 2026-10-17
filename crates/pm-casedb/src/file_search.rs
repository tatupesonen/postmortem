//! File Search by Attributes (Autopsy `filesearch`): name, size, dates,
//! known status, hashes, MIME types, data sources and deleted state, all
//! combined with AND, keyset-paged by file id.
//!
//! Differences from Autopsy (bugs fixed): the name is a bound parameter
//! (a `'` no longer breaks the query) and `%`/`_` match literally; the
//! "to" date includes its whole day; an empty "to" means no upper limit.
//! The name filter scans `files`; the Tantivy file-name index replaces it
//! on large cases once available (ADR-014).

use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, params_from_iter};

use crate::{CaseDb, FILE_COLS, FileRecord, Known, Result};

/// Size comparison (Autopsy "equal to" / "greater than" / "less than").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeOp {
    Equal,
    Greater,
    Less,
}

/// Which file times a date filter checks (any of them may match).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DateKinds {
    pub modified: bool,
    pub changed: bool,
    pub accessed: bool,
    pub created: bool,
}

/// A date range in Unix seconds (`to` is exclusive; `None` is open).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateFilter {
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub kinds: DateKinds,
}

/// The enabled filters of a search; `None`/empty means "not enabled".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSearch {
    /// Case-insensitive substring of the file name (not the path).
    pub name: Option<String>,
    pub size: Option<(SizeOp, u64)>,
    pub dates: Option<DateFilter>,
    pub known: Vec<Known>,
    pub md5: Option<Vec<u8>>,
    pub sha256: Option<Vec<u8>>,
    pub mime: Vec<String>,
    pub ds_ids: Vec<i64>,
    pub deleted: bool,
}

impl FileSearch {
    /// Whether any filter is enabled (Autopsy: "At least one filter must
    /// be selected.").
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// `WHERE` conditions and their parameters.
    fn sql(&self) -> (Vec<String>, Vec<SqlValue>) {
        let mut w = Vec::new();
        let mut p: Vec<SqlValue> = Vec::new();
        if let Some(n) = &self.name {
            let escaped = n
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            w.push("name LIKE ? ESCAPE '\\'".to_owned());
            p.push(SqlValue::Text(format!("%{escaped}%")));
        }
        if let Some((op, n)) = self.size {
            let op = match op {
                SizeOp::Equal => "=",
                SizeOp::Greater => ">",
                SizeOp::Less => "<",
            };
            w.push(format!("size {op} ?"));
            p.push(SqlValue::Integer(i64::try_from(n).unwrap_or(i64::MAX)));
        }
        if let Some(d) = self.dates {
            let mut any = Vec::new();
            for (on, col) in [
                (d.kinds.modified, "mtime"),
                (d.kinds.changed, "ctime"),
                (d.kinds.accessed, "atime"),
                (d.kinds.created, "crtime"),
            ] {
                if on {
                    any.push(format!("({col} >= ? AND {col} < ?)"));
                    p.push(SqlValue::Integer(d.from.unwrap_or(i64::MIN)));
                    p.push(SqlValue::Integer(d.to.unwrap_or(i64::MAX)));
                }
            }
            if any.is_empty() {
                w.push("0".into());
            } else {
                w.push(format!("({})", any.join(" OR ")));
            }
        }
        if !self.known.is_empty() {
            let list: Vec<String> = self.known.iter().map(|k| (*k as i64).to_string()).collect();
            w.push(format!("known IN ({})", list.join(",")));
        }
        if let Some(h) = &self.md5 {
            w.push("md5 = ?".into());
            p.push(SqlValue::Blob(h.clone()));
        }
        if let Some(h) = &self.sha256 {
            w.push("sha256 = ?".into());
            p.push(SqlValue::Blob(h.clone()));
        }
        if !self.mime.is_empty() {
            let q = vec!["?"; self.mime.len()].join(",");
            w.push(format!("mime IN ({q})"));
            p.extend(self.mime.iter().map(|m| SqlValue::Text(m.clone())));
        }
        if !self.ds_ids.is_empty() {
            let list: Vec<String> = self.ds_ids.iter().map(i64::to_string).collect();
            w.push(format!("ds_id IN ({})", list.join(",")));
        }
        if self.deleted {
            w.push("(name_alloc = 0 OR meta_alloc = 0)".into());
        }
        (w, p)
    }
}

fn run(c: &Connection, s: &FileSearch, after_id: i64, limit: usize) -> Result<Vec<FileRecord>> {
    let (mut w, mut p) = s.sql();
    w.push("id > ?".into());
    p.push(SqlValue::Integer(after_id));
    p.push(SqlValue::Integer(i64::try_from(limit).unwrap_or(i64::MAX)));
    let sql = format!(
        "SELECT {FILE_COLS} FROM files WHERE {} ORDER BY id LIMIT ?",
        w.join(" AND ")
    );
    let mut st = c.prepare(&sql)?;
    let rows = st
        .query_map(params_from_iter(p), FileRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

impl CaseDb {
    /// One keyset page of the files matching `search` (ids above
    /// `after_id`, in id order).
    pub fn search_files(
        &self,
        search: &FileSearch,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        self.read(|c| run(c, search, after_id, limit))
    }

    /// MIME types present in the case (the MIME filter's list).
    pub fn mime_types(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut st =
                c.prepare("SELECT DISTINCT mime FROM files WHERE mime IS NOT NULL ORDER BY mime")?;
            let rows = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_search_has_no_filters() {
        assert!(FileSearch::default().is_empty());
        assert!(
            !FileSearch {
                deleted: true,
                ..FileSearch::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn name_wildcards_are_escaped() {
        let s = FileSearch {
            name: Some("100%_o'k".into()),
            ..FileSearch::default()
        };
        let (_, p) = s.sql();
        assert_eq!(p, [SqlValue::Text("%100\\%\\_o'k%".into())]);
    }
}
