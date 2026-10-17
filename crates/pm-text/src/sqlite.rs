//! SQLite databases (Autopsy `SqliteTextExtractor`): every table dumped
//! in Autopsy's exact format. The database (and its `-wal` file) is
//! copied to a temporary folder first, as Autopsy does.

use std::io::Write;

use pm_core::ReadAt;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

use crate::util::java_double_to_string;
use crate::{ExtractOptions, Metadata, Stop, TextError, TextSink};

const FLUSH: usize = 64 * 1024;

pub(crate) fn extract(
    src: &dyn ReadAt,
    sink: &mut dyn TextSink,
    opts: &ExtractOptions,
) -> Result<Metadata, TextError> {
    let dir = opts.temp_dir.clone().unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir)?;
    let tmp = tempfile::Builder::new()
        .prefix("pm-sqlite-")
        .tempdir_in(&dir)?;
    let db_path = tmp.path().join("db.sqlite");
    copy(src, &db_path)?;
    if let Some(wal) = &opts.sqlite_wal {
        copy(&**wal, &tmp.path().join("db.sqlite-wal"))?;
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| TextError::malformed("SQLite", e))?;
    dump(&conn, sink)?;
    Ok(Metadata::new())
}

fn copy(src: &dyn ReadAt, to: &std::path::Path) -> Result<(), TextError> {
    let mut f = std::fs::File::create(to)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut off = 0u64;
    let size = src.size();
    while off < size {
        let want = usize::try_from((size - off).min(buf.len() as u64)).unwrap_or(0);
        let n = src.read_at(off, buf.get_mut(..want).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        f.write_all(buf.get(..n).unwrap_or_default())?;
        off += n as u64;
    }
    Ok(())
}

/// Autopsy's dump: per table `"\n<table>\n\n\t"` + column names separated
/// by spaces + `"\n"`, then each row as `"\t"` + values separated by
/// spaces + `"\n"` (a one-column row is `"\tv \n"`); BLOBs and NULLs are
/// empty, REALs formatted like Java's `Double.toString`.
fn dump(conn: &Connection, sink: &mut dyn TextSink) -> Result<(), TextError> {
    let tables: Vec<String> = {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type= 'table' ")
            .map_err(|e| TextError::malformed("SQLite", e))?;
        st.query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| TextError::malformed("SQLite", e))?
            .filter_map(Result::ok)
            .collect()
    };
    let mut out = String::new();
    for table in tables {
        if let Err(e) = dump_table(conn, &table, &mut out, sink) {
            match e {
                DumpError::Stop => return Err(TextError::Stopped),
                DumpError::Sql(e) => tracing::debug!(table, "table not read: {e}"),
            }
        }
    }
    if !out.is_empty() {
        sink.push(&out)?;
    }
    Ok(())
}

enum DumpError {
    Stop,
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for DumpError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

impl From<Stop> for DumpError {
    fn from(_: Stop) -> Self {
        Self::Stop
    }
}

fn dump_table(
    conn: &Connection,
    table: &str,
    out: &mut String,
    sink: &mut dyn TextSink,
) -> Result<(), DumpError> {
    let sql = format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""));
    let mut st = conn.prepare(&sql)?;
    let names: Vec<String> = st.column_names().into_iter().map(str::to_owned).collect();
    let total = names.len();
    if total == 0 {
        return Ok(());
    }
    for (i, name) in names.iter().enumerate() {
        if i == 0 {
            out.push('\n');
            out.push_str(table);
            out.push_str("\n\n\t");
        }
        out.push_str(name);
        out.push(if i + 1 == total { '\n' } else { ' ' });
    }
    let mut rows = st.query([])?;
    while let Some(row) = rows.next()? {
        for i in 0..total {
            let v = match row.get_ref(i)? {
                ValueRef::Null | ValueRef::Blob(_) => String::new(),
                ValueRef::Integer(n) => n.to_string(),
                ValueRef::Real(f) => java_double_to_string(f),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
            };
            let idx = i + 1;
            if idx == 1 {
                out.push('\t');
                out.push_str(&v);
                out.push(' ');
            } else {
                out.push_str(&v);
                if idx < total {
                    out.push(' ');
                }
            }
            if idx == total {
                out.push('\n');
            }
        }
        if out.len() >= FLUSH {
            sink.push(out)?;
            out.clear();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    #[test]
    fn autopsy_table_dump_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE people (id INTEGER, name TEXT, score REAL, pic BLOB);
                 INSERT INTO people VALUES (1, 'Ann', 2.5, x'0102'), (2, NULL, 1e10, NULL);
                 CREATE TABLE one (v TEXT); INSERT INTO one VALUES ('x');",
            )
            .unwrap();
        }
        let src = pm_core::io::MemSource::new(std::fs::read(&p).unwrap());
        let mut sink = StringSink::default();
        let opts = ExtractOptions {
            temp_dir: Some(dir.path().to_path_buf()),
            sqlite_wal: None,
        };
        extract(&src, &mut sink, &opts).unwrap();
        assert_eq!(
            sink.text,
            "\npeople\n\n\tid name score pic\n\t1 Ann 2.5 \n\t2  1.0E10 \n\none\n\n\tv\n\tx \n"
        );
    }

    #[test]
    fn not_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let src = pm_core::io::MemSource::new(b"SQLite format 3\0garbage".to_vec());
        let mut sink = StringSink::default();
        let opts = ExtractOptions {
            temp_dir: Some(dir.path().to_path_buf()),
            sqlite_wal: None,
        };
        assert!(extract(&src, &mut sink, &opts).is_err());
    }
}
