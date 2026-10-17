//! Autopsy's SQLite hash database (`.kdb`, Autopsy `sqlite_hdb.cpp`), read and
//! written in its own schema so sets stay interchangeable with Autopsy.

use std::path::Path;

use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::{HashDbError, HashHit, Result};

const SCHEMA: &str = "
CREATE TABLE db_properties (name TEXT NOT NULL, value TEXT);
INSERT INTO db_properties (name, value) VALUES ('Schema Version', '1');
CREATE TABLE hashes (id INTEGER PRIMARY KEY AUTOINCREMENT, md5 BINARY(16) UNIQUE, sha1 BINARY(20), sha2_256 BINARY(32));
CREATE TABLE file_names (name TEXT NOT NULL, hash_id INTEGER NOT NULL, PRIMARY KEY(name, hash_id));
CREATE TABLE comments (comment TEXT NOT NULL, hash_id INTEGER NOT NULL, PRIMARY KEY(comment, hash_id));
CREATE INDEX md5_index ON hashes(md5);
";

#[derive(Debug)]
pub(crate) struct Kdb {
    conn: Mutex<Connection>,
    writable: bool,
}

impl Kdb {
    pub(crate) fn create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Err(HashDbError::Exists(path.to_path_buf()));
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA page_size = 4096;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            writable: true,
        })
    }

    pub(crate) fn open(path: &Path, writable: bool) -> Result<Self> {
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let conn = Connection::open_with_flags(path, flags)?;
        Ok(Self {
            conn: Mutex::new(conn),
            writable,
        })
    }

    pub(crate) fn writable(&self) -> bool {
        self.writable
    }

    pub(crate) fn len(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .lock()
            .query_row("SELECT COUNT(*) FROM hashes", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    pub(crate) fn contains(&self, md5: &[u8; 16]) -> Result<bool> {
        let c = self.conn.lock();
        Ok(
            c.prepare_cached("SELECT 1 FROM hashes WHERE md5 = ?1 LIMIT 1")?
                .exists([md5.as_slice()])?,
        )
    }

    pub(crate) fn lookup(&self, md5: &[u8; 16]) -> Result<Option<HashHit>> {
        let c = self.conn.lock();
        let id: Option<i64> = c
            .prepare_cached("SELECT id FROM hashes WHERE md5 = ?1 LIMIT 1")?
            .query_row([md5.as_slice()], |r| r.get(0))
            .optional()?;
        let Some(id) = id else { return Ok(None) };
        let strings = |sql: &str| -> Result<Vec<String>> {
            Ok(c.prepare_cached(sql)?
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?)
        };
        Ok(Some(HashHit {
            md5: *md5,
            names: strings("SELECT name FROM file_names WHERE hash_id = ?1 ORDER BY rowid")?,
            comments: strings("SELECT comment FROM comments WHERE hash_id = ?1 ORDER BY rowid")?,
        }))
    }

    pub(crate) fn add(
        &self,
        md5: &[u8; 16],
        sha1: Option<&[u8]>,
        sha256: Option<&[u8]>,
        name: Option<&str>,
        comment: Option<&str>,
    ) -> Result<()> {
        if !self.writable {
            return Err(HashDbError::ReadOnly);
        }
        let mut c = self.conn.lock();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO hashes (md5) VALUES (?1)",
            [md5.as_slice()],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM hashes WHERE md5 = ?1",
            [md5.as_slice()],
            |r| r.get(0),
        )?;
        if let Some(s) = sha1 {
            tx.execute("UPDATE hashes SET sha1 = ?2 WHERE id = ?1", params![id, s])?;
        }
        if let Some(s) = sha256 {
            tx.execute(
                "UPDATE hashes SET sha2_256 = ?2 WHERE id = ?1",
                params![id, s],
            )?;
        }
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            tx.execute(
                "INSERT OR IGNORE INTO file_names (name, hash_id) VALUES (?1, ?2)",
                params![n, id],
            )?;
        }
        if let Some(cm) = comment.filter(|c| !c.is_empty()) {
            tx.execute(
                "INSERT OR IGNORE INTO comments (comment, hash_id) VALUES (?1, ?2)",
                params![cm, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
