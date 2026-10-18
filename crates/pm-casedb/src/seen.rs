//! Review state: which files and results the examiner has seen.

use std::collections::{HashMap, HashSet};

use rusqlite::params;

use crate::{CaseDb, ObjKind, Result};

impl CaseDb {
    /// Marks objects seen (already seen ones keep their first time).
    pub fn mark_seen(&self, kind: ObjKind, ids: &[i64], examiner: &str, now: i64) -> Result<()> {
        self.write(|tx| {
            let mut st = tx.prepare_cached(
                "INSERT OR IGNORE INTO seen (kind, obj_id, examiner, at) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for id in ids {
                st.execute(params![kind as i64, id, examiner, now])?;
            }
            Ok(())
        })
    }

    /// Marks objects unseen again.
    pub fn mark_unseen(&self, kind: ObjKind, ids: &[i64]) -> Result<()> {
        self.write(|tx| {
            let mut st = tx.prepare_cached("DELETE FROM seen WHERE kind = ?1 AND obj_id = ?2")?;
            for id in ids {
                st.execute(params![kind as i64, id])?;
            }
            Ok(())
        })
    }

    /// Which of `ids` are seen.
    pub fn seen_of(&self, kind: ObjKind, ids: &[i64]) -> Result<HashSet<i64>> {
        self.read(|c| {
            let mut st = c.prepare_cached("SELECT 1 FROM seen WHERE kind = ?1 AND obj_id = ?2")?;
            let mut out = HashSet::new();
            for id in ids {
                if st.exists(params![kind as i64, id])? {
                    out.insert(*id);
                }
            }
            Ok(out)
        })
    }

    /// Unseen results per artifact type (types with none are absent).
    pub fn unseen_artifact_counts(&self, ds_id: Option<i64>) -> Result<HashMap<i64, u64>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT a.type_id, COUNT(*) FROM artifacts a \
                 WHERE a.review_status <> 2 AND (?1 IS NULL OR a.ds_id = ?1) \
                   AND NOT EXISTS (SELECT 1 FROM seen s WHERE s.kind = 1 AND s.obj_id = a.id) \
                 GROUP BY a.type_id",
            )?;
            let rows = st
                .query_map([ds_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?.max(0).cast_unsigned(),
                    ))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            Ok(rows)
        })
    }
}
