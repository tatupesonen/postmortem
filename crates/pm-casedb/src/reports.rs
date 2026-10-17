//! Generated reports recorded in the case (TSK `addReport`), listed by the
//! Reports node.

use rusqlite::params;

use crate::{CaseDb, Result};

/// A report of the case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub id: i64,
    /// The report's main file (e.g. `report.html`).
    pub path: String,
    /// The module that wrote it (`"HTML Report"`).
    pub module: String,
    pub display_name: String,
    pub created_at: i64,
}

impl CaseDb {
    pub fn add_report(
        &self,
        path: &str,
        module: &str,
        display_name: &str,
        created_at: i64,
    ) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO reports (path, module, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![path, module, display_name, created_at],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// Every report, oldest first.
    pub fn reports(&self) -> Result<Vec<Report>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT id, path, module, display_name, created_at FROM reports ORDER BY id",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(Report {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        module: r.get(2)?,
                        display_name: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Removes a report from the case (its files are left on disk, as in
    /// Autopsy's "Delete Report" without "delete files").
    pub fn delete_report(&self, id: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM reports WHERE id = ?1", [id])?;
            Ok(())
        })
    }
}
