//! Data Source Summary queries (Autopsy `datasourcesummary`): counts,
//! file-type breakdown and activity span of one data source, all from
//! indexed aggregates over `files`.

use rusqlite::params;

use crate::mcp::TimeColumn;
use crate::{CaseDb, Result};

/// File counts of a data source (the Types tab).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataSourceCounts {
    /// Regular files per extension (lower case, `""` for none).
    pub by_extension: Vec<(String, u64)>,
    pub allocated_files: u64,
    pub unallocated_files: u64,
    pub directories: u64,
    /// Bytes in allocated regular files.
    pub allocated_bytes: u64,
}

impl CaseDb {
    pub fn data_source_counts(&self, ds_id: i64) -> Result<DataSourceCounts> {
        self.read(|c| {
            let mut out = DataSourceCounts::default();
            let mut st = c.prepare_cached(
                "SELECT kind, name_alloc AND meta_alloc, COUNT(*), COALESCE(SUM(size), 0) \
                 FROM files WHERE ds_id = ?1 AND kind IN (1, 2) GROUP BY 1, 2",
            )?;
            let rows = st.query_map(params![ds_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, i64>(2)?.max(0).cast_unsigned(),
                    r.get::<_, i64>(3)?.max(0).cast_unsigned(),
                ))
            })?;
            for row in rows {
                let (kind, alloc, n, bytes) = row?;
                match (kind, alloc) {
                    (2, _) => out.directories += n,
                    (_, true) => {
                        out.allocated_files += n;
                        out.allocated_bytes += bytes;
                    }
                    (_, false) => out.unallocated_files += n,
                }
            }
            let mut st = c.prepare_cached(
                "SELECT extension, COUNT(*) FROM files WHERE ds_id = ?1 AND kind = 1 GROUP BY extension",
            )?;
            out.by_extension = st
                .query_map(params![ds_id], |r| {
                    Ok((r.get(0)?, r.get::<_, i64>(1)?.max(0).cast_unsigned()))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(out)
        })
    }

    /// `(earliest, latest)` positive modified/accessed/created time of a
    /// data source's files.
    pub fn data_source_activity_span(&self, ds_id: i64) -> Result<Option<(i64, i64)>> {
        self.read(|c| {
            let mut lo: Option<i64> = None;
            let mut hi: Option<i64> = None;
            for col in [
                TimeColumn::Modified,
                TimeColumn::Accessed,
                TimeColumn::Created,
            ] {
                let n = col.column();
                let (a, b): (Option<i64>, Option<i64>) = c.query_row(
                    &format!("SELECT MIN({n}), MAX({n}) FROM files WHERE ds_id = ?1 AND {n} > 0"),
                    params![ds_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                lo = [lo, a].into_iter().flatten().min();
                hi = [hi, b].into_iter().flatten().max();
            }
            Ok(lo.zip(hi))
        })
    }
}
