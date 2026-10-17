//! Opening browser SQLite databases taken from evidence.
//!
//! The database is a private temp copy (with its `-wal` / `-journal`
//! companions next to it so uncommitted transactions are seen, which
//! Autopsy misses); it is opened read-write only so SQLite can replay the
//! WAL, then locked down: `query_only`, no trusted schema (views and
//! triggers from the file cannot call functions with side effects),
//! defensive mode, cell size checks and tight length limits.

use std::path::Path;

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags, Row};

/// Opens `path` for hostile-input queries.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, 64 << 20);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 1 << 20);
    let _ = conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    conn.execute_batch(
        "PRAGMA cell_size_check = ON; PRAGMA mmap_size = 0; PRAGMA cache_size = -8192;",
    )?;
    // Touch the schema now (replays the WAL), then forbid writes.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    Ok(conn)
}

/// Whether `table` has a column named `column` (Autopsy `Util.checkColumn`,
/// case-sensitive).
pub fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut st) = conn.prepare("SELECT name FROM pragma_table_info(?1)") else {
        return false;
    };
    let Ok(rows) = st.query_map([table], |r| r.get::<_, String>(0)) else {
        return false;
    };
    rows.flatten().any(|n| n == column)
}

/// Runs `sql`, calling `row` per result row until it returns `false`.
/// SQL errors (schema mismatch, corruption) end the scan and are returned;
/// Autopsy treats them as "no rows".
pub fn each(
    conn: &Connection,
    sql: &str,
    mut row: impl FnMut(&Row<'_>) -> bool,
) -> rusqlite::Result<()> {
    let mut st = conn.prepare(sql)?;
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        if !row(r) {
            break;
        }
    }
    Ok(())
}

/// A column as text the way Autopsy's `toString()` of the JDBC value
/// renders it: NULL → `""`, numbers in decimal, BLOBs as `None` (the
/// caller decides; Autopsy prints the Java array identity).
pub fn text(r: &Row<'_>, col: &str) -> Option<String> {
    use rusqlite::types::ValueRef;
    match r.get_ref(col).ok()? {
        ValueRef::Null => Some(String::new()),
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(_) => None,
    }
}

/// Text, with BLOBs decoded lossily (for fields Autopsy reads as strings).
pub fn text_lossy(r: &Row<'_>, col: &str) -> String {
    use rusqlite::types::ValueRef;
    match r.get_ref(col) {
        Ok(ValueRef::Blob(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => text(r, col).unwrap_or_default(),
    }
}

/// Whether the column holds a BLOB (Autopsy's "encrypted" signal).
pub fn is_blob(r: &Row<'_>, col: &str) -> bool {
    matches!(r.get_ref(col), Ok(rusqlite::types::ValueRef::Blob(_)))
}

/// A column as an integer (`None` for NULL / text that is no number).
pub fn int(r: &Row<'_>, col: &str) -> Option<i64> {
    use rusqlite::types::ValueRef;
    match r.get_ref(col).ok()? {
        ValueRef::Integer(i) => Some(i),
        ValueRef::Real(f) if f.is_finite() => Some(f as i64),
        ValueRef::Text(t) => std::str::from_utf8(t).ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// A column as a float.
pub fn real(r: &Row<'_>, col: &str) -> Option<f64> {
    use rusqlite::types::ValueRef;
    match r.get_ref(col).ok()? {
        ValueRef::Integer(i) => Some(i as f64),
        ValueRef::Real(f) => Some(f),
        ValueRef::Text(t) => std::str::from_utf8(t).ok()?.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_read_only_and_checks_columns() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE t(a INTEGER, b TEXT, c BLOB); INSERT INTO t VALUES (1, 'x', x'00ff');\
                 CREATE VIEW v AS SELECT * FROM t;",
            )
            .unwrap();
        }
        let c = open(&p).unwrap();
        assert!(has_column(&c, "t", "b"));
        assert!(!has_column(&c, "t", "B"));
        assert!(!has_column(&c, "missing", "b"));
        let mut seen = Vec::new();
        each(&c, "SELECT a, b, c FROM t", |r| {
            seen.push((int(r, "a"), text(r, "b"), is_blob(r, "c"), text(r, "c")));
            true
        })
        .unwrap();
        assert_eq!(seen, vec![(Some(1), Some("x".into()), true, None)]);
        assert!(
            c.execute_batch("INSERT INTO t VALUES (2, 'y', NULL)")
                .is_err()
        );
        // Views from the file are not usable.
        assert!(each(&c, "SELECT * FROM v", |_| true).is_err());
        assert!(open(&dir.path().join("missing/x.db")).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("g.db");
        std::fs::write(&p, b"definitely not sqlite, but long enough to be a header..........................................................................").unwrap();
        assert!(open(&p).is_err());
    }
}
