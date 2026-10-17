//! Hosts and persons (Autopsy `HostManager`, `PersonManager`).
//!
//! Every data source belongs to a host (a device); hosts can be grouped
//! under persons. Merging a host moves its data sources and OS account
//! realms into another host, merging realms that describe the same
//! domain.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::os_accounts::move_or_merge_realm;
use crate::{CaseDb, DbError, Result};

/// A host (device) in the case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: i64,
    pub name: String,
    pub person_id: Option<i64>,
}

/// A person owning one or more hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: i64,
    pub name: String,
}

/// `hosts.status` of a host merged into another.
const MERGED: i64 = 1;

fn host_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Host> {
    Ok(Host {
        id: r.get(0)?,
        name: r.get(1)?,
        person_id: r.get(2)?,
    })
}

/// Finds or creates the host `name` (Autopsy `newHost`: a duplicate name
/// returns the existing host).
pub(crate) fn host_named(c: &Connection, name: &str) -> Result<Host> {
    let name = name.trim();
    if name.is_empty() {
        return Err(DbError::Invalid("a host name is required".into()));
    }
    if let Some(h) = c
        .prepare_cached("SELECT id, name, person_id FROM hosts WHERE name = ?1 COLLATE NOCASE")?
        .query_row([name], host_from_row)
        .optional()?
    {
        return Ok(h);
    }
    c.prepare_cached("INSERT INTO hosts (name) VALUES (?1)")?
        .execute([name])?;
    Ok(Host {
        id: c.last_insert_rowid(),
        name: name.to_owned(),
        person_id: None,
    })
}

/// The host Autopsy creates for an image added without one: the first image
/// file's name plus `" Host"` (`Autopsy.addImageInfo`).
pub fn default_host_name(first_path: Option<&str>, ds_id: i64) -> String {
    let file = first_path
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p))
        .filter(|n| !n.is_empty());
    match file {
        Some(n) => format!("{n} Host"),
        None => format!("Image_{ds_id} Host"),
    }
}

impl CaseDb {
    /// Finds or creates a host by name.
    pub fn host_by_name_or_new(&self, name: &str) -> Result<Host> {
        self.write(|tx| host_named(tx, name))
    }

    /// Active hosts, by name.
    pub fn hosts(&self) -> Result<Vec<Host>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT id, name, person_id FROM hosts WHERE status = 0 ORDER BY name COLLATE NOCASE",
            )?;
            let rows = st
                .query_map([], host_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn host(&self, id: i64) -> Result<Option<Host>> {
        self.read(|c| {
            Ok(
                c.prepare_cached("SELECT id, name, person_id FROM hosts WHERE id = ?1")?
                    .query_row([id], host_from_row)
                    .optional()?,
            )
        })
    }

    /// The host of a data source.
    pub fn host_of_data_source(&self, ds_id: i64) -> Result<Option<Host>> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT h.id, h.name, h.person_id FROM data_sources d JOIN hosts h ON h.id = d.host_id WHERE d.id = ?1",
            )?
            .query_row([ds_id], host_from_row)
            .optional()?)
        })
    }

    /// Data source ids of a host.
    pub fn data_sources_of_host(&self, host_id: i64) -> Result<Vec<i64>> {
        self.read(|c| {
            let mut st =
                c.prepare_cached("SELECT id FROM data_sources WHERE host_id = ?1 ORDER BY id")?;
            let rows = st
                .query_map([host_id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Assigns a data source to a host.
    pub fn set_data_source_host(&self, ds_id: i64, host_id: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE data_sources SET host_id = ?2 WHERE id = ?1",
                params![ds_id, host_id],
            )?;
            Ok(())
        })
    }

    /// Renames a host (names are unique, case-insensitively).
    pub fn rename_host(&self, host_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("a host name is required".into()));
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE hosts SET name = ?2 WHERE id = ?1",
                params![host_id, name],
            )?;
            Ok(())
        })
    }

    /// Deletes a host that nothing refers to (Autopsy `deleteHost`: refused
    /// while it has data sources, realms, account attributes or address
    /// mappings).
    pub fn delete_host(&self, host_id: i64) -> Result<()> {
        self.write(|tx| {
            let used: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM data_sources WHERE host_id = ?1) \
                     OR EXISTS (SELECT 1 FROM os_account_realms WHERE scope_host_id = ?1) \
                     OR EXISTS (SELECT 1 FROM os_account_attributes WHERE host_id = ?1) \
                     OR EXISTS (SELECT 1 FROM host_address_map WHERE host_id = ?1)",
                [host_id],
                |r| r.get(0),
            )?;
            if used {
                return Err(DbError::Invalid(
                    "the host has child data and cannot be deleted".into(),
                ));
            }
            tx.execute("DELETE FROM hosts WHERE id = ?1", [host_id])?;
            Ok(())
        })
    }

    /// Merges `source` into `dest` (Autopsy `mergeHosts`): realms are moved or
    /// merged, data sources, account attributes and address mappings move,
    /// and the source host is marked merged under a unique dummy name.
    pub fn merge_hosts(&self, source: i64, dest: i64) -> Result<()> {
        if source == dest {
            return Ok(());
        }
        self.write(|tx| merge_hosts(tx, source, dest))
    }

    // ------------------------------------------------------------ persons

    /// Creates a person (names are unique).
    pub fn new_person(&self, name: &str) -> Result<Person> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("a person name is required".into()));
        }
        self.write(|tx| {
            tx.execute("INSERT INTO persons (name) VALUES (?1)", [name])?;
            Ok(Person {
                id: tx.last_insert_rowid(),
                name: name.to_owned(),
            })
        })
    }

    pub fn persons(&self) -> Result<Vec<Person>> {
        self.read(|c| {
            let mut st = c.prepare("SELECT id, name FROM persons ORDER BY name COLLATE NOCASE")?;
            let rows = st
                .query_map([], |r| {
                    Ok(Person {
                        id: r.get(0)?,
                        name: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn rename_person(&self, person_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DbError::Invalid("a person name is required".into()));
        }
        self.write(|tx| {
            tx.execute(
                "UPDATE persons SET name = ?2 WHERE id = ?1",
                params![person_id, name],
            )?;
            Ok(())
        })
    }

    /// Deletes a person; its hosts become person-less.
    pub fn delete_person(&self, person_id: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM persons WHERE id = ?1", [person_id])?;
            Ok(())
        })
    }

    /// Associates a host with a person (`None` removes the association).
    pub fn set_host_person(&self, host_id: i64, person_id: Option<i64>) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE hosts SET person_id = ?2 WHERE id = ?1",
                params![host_id, person_id],
            )?;
            Ok(())
        })
    }
}

fn merge_hosts(tx: &Transaction<'_>, source: i64, dest: i64) -> Result<()> {
    let realms: Vec<i64> = {
        let mut st = tx.prepare(
            "SELECT id FROM os_account_realms WHERE scope_host_id = ?1 AND status = 0 ORDER BY id",
        )?;
        st.query_map([source], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for realm in realms {
        move_or_merge_realm(tx, realm, dest)?;
    }
    tx.execute(
        "DELETE FROM host_address_map WHERE id IN (SELECT s.id FROM host_address_map d \
         JOIN host_address_map s ON d.address_id = s.address_id AND d.time IS s.time \
         WHERE d.host_id = ?2 AND s.host_id = ?1)",
        params![source, dest],
    )?;
    for sql in [
        "UPDATE host_address_map SET host_id = ?2 WHERE host_id = ?1",
        "UPDATE os_account_attributes SET host_id = ?2 WHERE host_id = ?1",
        "UPDATE data_sources SET host_id = ?2 WHERE host_id = ?1",
    ] {
        tx.execute(sql, params![source, dest])?;
    }
    tx.execute(
        "UPDATE hosts SET merged_into = ?2, status = ?3, name = ?4 WHERE id = ?1",
        params![
            source,
            dest,
            MERGED,
            format!("MERGED {}", uuid::Uuid::new_v4())
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_host_names_follow_autopsy() {
        assert_eq!(
            default_host_name(Some("/evidence/disk1.E01"), 7),
            "disk1.E01 Host"
        );
        assert_eq!(
            default_host_name(Some(r"C:\cases\img.dd"), 7),
            "img.dd Host"
        );
        assert_eq!(default_host_name(None, 7), "Image_7 Host");
    }
}
