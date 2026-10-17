//! Queries used by the web-browser extractors of Recent Activity: one
//! keyset-paged candidate scan by file name, a cheap "already analysed"
//! check, and artifact pages with selected attribute values.

use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;

use crate::{CaseDb, FILE_COLS, FileRecord, Result, Value};

/// Which files a candidate scan returns (names compare ASCII
/// case-insensitively, like Autopsy's `LIKE` patterns).
#[derive(Debug, Clone, Default)]
pub struct NameFilter {
    /// Exact file names.
    pub names: Vec<String>,
    /// Name prefixes (`History` matches `History-journal`).
    pub prefixes: Vec<String>,
    /// Name substrings.
    pub contains: Vec<String>,
    /// `(extension, parent path substring)` pairs.
    pub ext_in_dir: Vec<(String, String)>,
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// One artifact with the requested attribute values.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRow {
    pub id: i64,
    pub type_id: i64,
    pub file_id: Option<i64>,
    /// `(attribute type id, value)` in insertion order.
    pub attributes: Vec<(i64, Value)>,
}

impl ArtifactRow {
    /// The first value of attribute type `type_id`.
    pub fn get(&self, type_id: i64) -> Option<&Value> {
        self.attributes
            .iter()
            .find(|(t, _)| *t == type_id)
            .map(|(_, v)| v)
    }

    /// The first text value of attribute type `type_id`.
    pub fn text(&self, type_id: i64) -> Option<&str> {
        match self.get(type_id)? {
            Value::Text(s) | Value::Json(s) => Some(s),
            _ => None,
        }
    }

    /// The first integer / date-time value of attribute type `type_id`.
    pub fn int(&self, type_id: i64) -> Option<i64> {
        match self.get(type_id)? {
            Value::Int(i) | Value::DateTime(i) => Some(*i),
            _ => None,
        }
    }
}

impl CaseDb {
    /// Regular files (not streams) of a data source matching `filter`,
    /// after id `after_id`, in id order. One pass over the data source's
    /// files per page; page with the last id.
    pub fn files_matching_names(
        &self,
        ds_id: i64,
        filter: &NameFilter,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileRecord>> {
        let mut args: Vec<SqlValue> = vec![ds_id.into(), after_id.into()];
        let mut conds: Vec<String> = Vec::new();
        let push = |v: String, args: &mut Vec<SqlValue>| -> String {
            args.push(v.into());
            format!("?{}", args.len())
        };
        if !filter.names.is_empty() {
            let ps: Vec<String> = filter
                .names
                .iter()
                .map(|n| push(n.to_ascii_lowercase(), &mut args))
                .collect();
            conds.push(format!("lower(name) IN ({})", ps.join(", ")));
        }
        for p in &filter.prefixes {
            let a = push(format!("{}%", like_escape(p)), &mut args);
            conds.push(format!("name LIKE {a} ESCAPE '\\'"));
        }
        for p in &filter.contains {
            let a = push(format!("%{}%", like_escape(p)), &mut args);
            conds.push(format!("name LIKE {a} ESCAPE '\\'"));
        }
        for (ext, dir) in &filter.ext_in_dir {
            let e = push(ext.to_ascii_lowercase(), &mut args);
            let d = push(format!("%{}%", like_escape(dir)), &mut args);
            conds.push(format!(
                "(extension = {e} AND parent_path LIKE {d} ESCAPE '\\')"
            ));
        }
        if conds.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {FILE_COLS} FROM files WHERE ds_id = ?1 AND kind = 1 AND stream IS NULL AND id > ?2 AND ({}) ORDER BY id LIMIT {limit}",
            conds.join(" OR ")
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params_from_iter(args.iter()), FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Children of `parent_id` named `name` (ASCII case-insensitive),
    /// streams excluded, by id (uses the `(parent_id, name)` index).
    pub fn children_named(&self, parent_id: i64, name: &str) -> Result<Vec<FileRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {FILE_COLS} FROM files WHERE parent_id = ?1 AND name = ?2 COLLATE NOCASE AND stream IS NULL ORDER BY id LIMIT 64"
            ))?;
            let rows = st
                .query_map(rusqlite::params![parent_id, name], FileRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Whether `file_id` has any artifact of `type_id` (one indexed probe).
    pub fn file_has_artifact_type(&self, file_id: i64, type_id: i64) -> Result<bool> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM artifacts WHERE file_id = ?1 AND type_id = ?2)",
            )?;
            Ok(st.query_row([file_id, type_id], |r| r.get::<_, bool>(0))?)
        })
    }

    /// Artifacts of the given types in a data source, after id `after_id`
    /// in id order, each with its values of the attribute types `attrs`.
    pub fn artifact_rows(
        &self,
        ds_id: i64,
        types: &[i64],
        attrs: &[i64],
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ArtifactRow>> {
        if types.is_empty() {
            return Ok(Vec::new());
        }
        let tps = types
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT id, type_id, file_id FROM artifacts WHERE type_id IN ({tps}) AND ds_id = ?1 AND id > ?2 \
                 AND review_status <> 2 ORDER BY id LIMIT {limit}"
            ))?;
            let mut rows: Vec<ArtifactRow> = st
                .query_map([ds_id, after_id], |r| {
                    Ok(ArtifactRow {
                        id: r.get(0)?,
                        type_id: r.get(1)?,
                        file_id: r.get(2)?,
                        attributes: Vec::new(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if rows.is_empty() || attrs.is_empty() {
                return Ok(rows);
            }
            let (Some(first), Some(last)) = (rows.first().map(|r| r.id), rows.last().map(|r| r.id))
            else {
                return Ok(rows);
            };
            let aps = attrs
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let mut at = c.prepare_cached(&format!(
                "SELECT a.artifact_id, a.type_id, t.value_type, a.value_text, a.value_int, a.value_double, a.value_blob \
                 FROM attributes a JOIN attribute_types t ON t.id = a.type_id \
                 WHERE a.artifact_id BETWEEN ?1 AND ?2 AND a.type_id IN ({aps}) ORDER BY a.artifact_id, a.rowid"
            ))?;
            let vals = at.query_map([first, last], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    Value::from_columns(r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?),
                ))
            })?;
            let mut i = 0;
            for v in vals {
                let (aid, ty, val) = v?;
                while rows.get(i).is_some_and(|r| r.id < aid) {
                    i += 1;
                }
                if let Some(r) = rows.get_mut(i).filter(|r| r.id == aid) {
                    r.attributes.push((ty, val));
                }
            }
            Ok(rows)
        })
    }
}
