//! OS accounts and realms (Autopsy `OsAccountManager`, `OsAccountRealmManager`).
//!
//! An OS account (a Windows SID, a Unix uid, …) lives in a realm: a domain
//! (no scope host) or one machine (scoped to a host). Accounts are
//! identified by address (SID) or, failing that, by login name; realms by
//! address (the SID prefix) or name. Learning a missing SID or name later
//! can reveal that two realms or accounts are the same, so they are merged
//! the way Autopsy merges them. Every operation runs in one write transaction,
//! so concurrent modules cannot create duplicates.

pub mod windows;

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{CaseDb, DbError, Result, Value};
use windows::{non_blank, real_sid};

/// Scope of a realm when it is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealmScope {
    /// Inferred: local to the referring host unless that host already has
    /// a known local realm.
    Unknown = 0,
    Local = 1,
    Domain = 2,
}

/// Whether a realm's scope is known or inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeConfidence {
    Known = 0,
    Inferred = 1,
}

/// Whether a realm or account row is current or was merged away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStatus {
    Active = 0,
    Merged = 1,
    Deleted = 2,
}

impl RecordStatus {
    fn from_code(c: i64) -> Self {
        match c {
            1 => Self::Merged,
            2 => Self::Deleted,
            _ => Self::Active,
        }
    }
}

/// An OS account realm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realm {
    pub id: i64,
    pub name: Option<String>,
    pub addr: Option<String>,
    pub signature: String,
    /// `None`: a domain realm.
    pub scope_host_id: Option<i64>,
    pub scope_confidence: ScopeConfidence,
    pub status: RecordStatus,
}

impl Realm {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            name: r.get(1)?,
            addr: r.get(2)?,
            signature: r.get(3)?,
            scope_host_id: r.get(4)?,
            scope_confidence: if r.get::<_, i64>(5)? == 1 {
                ScopeConfidence::Inferred
            } else {
                ScopeConfidence::Known
            },
            status: RecordStatus::from_code(r.get(6)?),
        })
    }
}

/// `os_accounts.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsAccountStatus {
    Unknown = 0,
    Active = 1,
    Disabled = 2,
    Deleted = 3,
    NonExistent = 4,
}

impl OsAccountStatus {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::Unknown,
            1 => Self::Active,
            2 => Self::Disabled,
            3 => Self::Deleted,
            4 => Self::NonExistent,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Active => "Active",
            Self::Disabled => "Disabled",
            Self::Deleted => "Deleted",
            Self::NonExistent => "Non Existent",
        }
    }
}

/// `os_accounts.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsAccountType {
    Unknown = 0,
    Service = 1,
    Interactive = 2,
}

impl OsAccountType {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::Unknown,
            1 => Self::Service,
            2 => Self::Interactive,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Service => "Service",
            Self::Interactive => "Interactive",
        }
    }
}

/// How an account appears on a data source, most significant first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstanceKind {
    /// Logged on or launched a program on the host.
    Launched = 0,
    /// Accessed a resource (read/write, authentication).
    Accessed = 1,
    /// Only referenced (in a log, the registry, …).
    Referenced = 2,
}

impl InstanceKind {
    pub fn from_code(c: i64) -> Option<Self> {
        Some(match c {
            0 => Self::Launched,
            1 => Self::Accessed,
            2 => Self::Referenced,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Launched => "Launched",
            Self::Accessed => "Accessed",
            Self::Referenced => "Referenced",
        }
    }
}

/// An OS account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsAccount {
    pub id: i64,
    pub realm_id: i64,
    pub login_name: Option<String>,
    pub full_name: Option<String>,
    pub addr: Option<String>,
    pub signature: String,
    pub status: Option<OsAccountStatus>,
    pub kind: Option<OsAccountType>,
    pub admin: Option<bool>,
    pub created_at: Option<i64>,
    pub db_status: RecordStatus,
}

const ACCOUNT_COLS: &str = "id, realm_id, login_name, full_name, addr, signature, status, kind, admin, created_at, db_status";
const REALM_COLS: &str = "id, name, addr, signature, scope_host_id, scope_confidence, status";

impl OsAccount {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            realm_id: r.get(1)?,
            login_name: r.get(2)?,
            full_name: r.get(3)?,
            addr: r.get(4)?,
            signature: r.get(5)?,
            status: r
                .get::<_, Option<i64>>(6)?
                .and_then(OsAccountStatus::from_code),
            kind: r
                .get::<_, Option<i64>>(7)?
                .and_then(OsAccountType::from_code),
            admin: r.get(8)?,
            created_at: r.get(9)?,
            db_status: RecordStatus::from_code(r.get(10)?),
        })
    }
}

/// An instance of an account on a data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsAccountInstance {
    pub id: i64,
    pub os_account_id: i64,
    pub ds_id: i64,
    pub kind: InstanceKind,
}

/// An extended account attribute (`KAAMOS_*` attribute type).
#[derive(Debug, Clone, PartialEq)]
pub struct OsAccountAttribute {
    pub type_id: i64,
    pub value: Value,
    pub host_id: Option<i64>,
    pub source_file_id: Option<i64>,
}

/// Standard account properties to set (`None` leaves a field unchanged).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountUpdate {
    pub full_name: Option<String>,
    pub kind: Option<OsAccountType>,
    pub status: Option<OsAccountStatus>,
    pub created_at: Option<i64>,
    pub admin: Option<bool>,
}

fn not_user_sid(sid: &str) -> DbError {
    DbError::Invalid(format!("SID = {sid} is not a user SID."))
}

fn merged_signature() -> String {
    format!("MERGED {}", uuid::Uuid::new_v4())
}

// ------------------------------------------------------------------ realms

/// `"<addr or name>_<host id | DOMAIN>"` (Autopsy `makeRealmSignature`).
fn realm_signature(addr: Option<&str>, name: Option<&str>, host: Option<i64>) -> Result<String> {
    let key = non_blank(addr)
        .or(non_blank(name))
        .ok_or_else(|| DbError::Invalid("Realm address and name can't both be null.".into()))?;
    Ok(match host {
        Some(h) => format!("{key}_{h}"),
        None => format!("{key}_DOMAIN"),
    })
}

fn realm_by_id(c: &Connection, id: i64) -> Result<Realm> {
    Ok(c.prepare_cached(&format!(
        "SELECT {REALM_COLS} FROM os_account_realms WHERE id = ?1"
    ))?
    .query_row([id], Realm::from_row)?)
}

/// Active realm by address (or name) visible from `host`: a realm scoped
/// to the host or a domain realm, host-scoped first.
fn realm_by(c: &Connection, column: &str, value: &str, host: Option<i64>) -> Result<Option<Realm>> {
    let sql = format!(
        "SELECT {REALM_COLS} FROM os_account_realms WHERE {column} = ?1 COLLATE NOCASE \
         AND (?2 IS NULL OR scope_host_id = ?2 OR scope_host_id IS NULL) AND status = 0 \
         ORDER BY scope_host_id IS NULL, scope_host_id LIMIT 1"
    );
    Ok(c.prepare_cached(&sql)?
        .query_row(params![value, host], Realm::from_row)
        .optional()?)
}

/// Another active realm with the same address or name and the same scope.
fn another_realm_by(
    c: &Connection,
    realm: &Realm,
    column: &str,
    value: &str,
) -> Result<Option<Realm>> {
    let sql = format!(
        "SELECT {REALM_COLS} FROM os_account_realms WHERE {column} = ?1 COLLATE NOCASE \
         AND scope_host_id IS ?2 AND id <> ?3 AND status = 0 LIMIT 1"
    );
    Ok(c.prepare_cached(&sql)?
        .query_row(
            params![value, realm.scope_host_id, realm.id],
            Realm::from_row,
        )
        .optional()?)
}

/// Autopsy `getWindowsRealm`.
fn windows_realm(
    c: &Connection,
    sid: Option<&str>,
    realm_name: Option<&str>,
    host: i64,
) -> Result<Option<Realm>> {
    let sid = real_sid(sid);
    if sid.is_none() && non_blank(realm_name).is_none() {
        return Err(DbError::Invalid(
            "Realm address or name is required get a realm.".into(),
        ));
    }
    if let Some(sid) = sid {
        if !windows::is_user_sid(sid) {
            return Err(not_user_sid(sid));
        }
        let addr = windows::realm_address(sid)
            .ok_or_else(|| DbError::Invalid(format!("Invalid SID {sid} for a host/domain")))?;
        if let Some(r) = realm_by(c, "addr", &addr, Some(host))? {
            return Ok(Some(r));
        }
    }
    let Some(name) = windows::english_realm_name(realm_name) else {
        return Ok(None);
    };
    let realm = realm_by(c, "name", &name, Some(host))?;
    // A SID was given and did not match: a realm with another address is
    // not the same realm.
    Ok(realm.filter(|r| sid.is_none() || r.addr.is_none()))
}

/// Fills in a realm's missing address or name and refreshes its signature
/// (Autopsy `updateRealm`). Returns whether anything changed.
fn update_realm(
    c: &Connection,
    realm: &Realm,
    addr: Option<&str>,
    name: Option<&str>,
) -> Result<bool> {
    let mut changed = false;
    if realm
        .addr
        .as_deref()
        .and_then(|a| non_blank(Some(a)))
        .is_none()
        && let Some(addr) = real_sid(addr)
    {
        c.execute(
            "UPDATE os_account_realms SET addr = ?2 WHERE id = ?1",
            params![realm.id, addr],
        )?;
        changed = true;
    }
    if realm
        .name
        .as_deref()
        .and_then(|n| non_blank(Some(n)))
        .is_none()
        && let Some(name) = non_blank(name)
    {
        c.execute(
            "UPDATE os_account_realms SET name = ?2 WHERE id = ?1",
            params![realm.id, name],
        )?;
        changed = true;
    }
    if changed {
        let cur = realm_by_id(c, realm.id)?;
        let sig = realm_signature(cur.addr.as_deref(), cur.name.as_deref(), cur.scope_host_id)?;
        c.execute(
            "UPDATE os_account_realms SET signature = CASE WHEN status = 0 THEN ?2 ELSE signature END WHERE id = ?1",
            params![realm.id, sig],
        )?;
    }
    Ok(changed)
}

/// Autopsy `getAndUpdateWindowsRealm` plus the realm merges Autopsy runs after an
/// update (another realm with the same name but no address, or the same
/// address but no name, is the same realm).
fn find_and_update_windows_realm(
    c: &Connection,
    sid: Option<&str>,
    realm_name: Option<&str>,
    host: i64,
) -> Result<Option<Realm>> {
    let Some(realm) = windows_realm(c, sid, realm_name, host)? else {
        return Ok(None);
    };
    let addr = real_sid(sid).and_then(windows::realm_address);
    let name = windows::english_realm_name(realm_name);
    if update_realm(c, &realm, addr.as_deref(), name.as_deref())? {
        let realm = realm_by_id(c, realm.id)?;
        // Autopsy passes the realm *name* to its by-address lookup here (a
        // bug that makes the address merge a no-op); we look up the
        // address as intended.
        if let Some(name) = name.as_deref()
            && let Some(other) = another_realm_by(c, &realm, "name", name)?
            && other.addr.is_none()
        {
            merge_realms(c, &other, &realm)?;
        }
        if let Some(addr) = addr.as_deref()
            && let Some(other) = another_realm_by(c, &realm, "addr", addr)?
            && other.name.is_none()
        {
            merge_realms(c, &other, &realm)?;
        }
        return Ok(Some(realm_by_id(c, realm.id)?));
    }
    Ok(Some(realm))
}

/// Autopsy `newWindowsRealm`.
fn new_windows_realm(
    c: &Connection,
    sid: Option<&str>,
    realm_name: Option<&str>,
    host: i64,
    scope: RealmScope,
) -> Result<Realm> {
    let sid = real_sid(sid);
    if sid.is_none() && non_blank(realm_name).is_none() {
        return Err(DbError::Invalid(
            "Either an address or a name is required to create a realm.".into(),
        ));
    }
    let (mut scope_host, mut confidence) = match scope {
        RealmScope::Domain => (None, ScopeConfidence::Known),
        RealmScope::Local => (Some(host), ScopeConfidence::Known),
        RealmScope::Unknown => {
            let host_has_known_realm: bool = c
                .prepare_cached(
                    "SELECT EXISTS (SELECT 1 FROM os_account_realms WHERE scope_host_id = ?1 AND scope_confidence = 0 AND status = 0)",
                )?
                .query_row([host], |r| r.get(0))?;
            if host_has_known_realm {
                (None, ScopeConfidence::Known)
            } else {
                (Some(host), ScopeConfidence::Inferred)
            }
        }
    };
    let mut name = windows::english_realm_name(realm_name);
    let mut addr = None;
    if let Some(sid) = sid {
        if !windows::is_user_sid(sid) {
            return Err(not_user_sid(sid));
        }
        addr = Some(
            windows::realm_address(sid)
                .ok_or_else(|| DbError::Invalid(format!("Invalid SID {sid} for a host/domain")))?,
        );
        if windows::is_well_known_sid(sid) {
            // Well-known accounts (SYSTEM, …) live in a realm local to
            // the host, under their English realm name.
            scope_host = Some(host);
            confidence = ScopeConfidence::Known;
            if let Some(n) = windows::well_known_realm_name(sid).filter(|n| !n.is_empty()) {
                name = Some(n.to_owned());
            }
        }
    }
    let signature = realm_signature(addr.as_deref(), name.as_deref(), scope_host)?;
    if let Some(existing) = c
        .prepare_cached(&format!(
            "SELECT {REALM_COLS} FROM os_account_realms WHERE signature = ?1"
        ))?
        .query_row([&signature], Realm::from_row)
        .optional()?
    {
        return Ok(existing);
    }
    c.prepare_cached(
        "INSERT INTO os_account_realms (name, addr, signature, scope_host_id, scope_confidence) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![name, addr, signature, scope_host, confidence as i64])?;
    Ok(Realm {
        id: c.last_insert_rowid(),
        name,
        addr,
        signature,
        scope_host_id: scope_host,
        scope_confidence: confidence,
        status: RecordStatus::Active,
    })
}

/// Autopsy `mergeRealms`: accounts move or merge into `dest`, `source` is
/// marked merged, and `dest` takes a missing address or name from it.
fn merge_realms(c: &Connection, source: &Realm, dest: &Realm) -> Result<()> {
    merge_accounts_of_realms(c, source, dest)?;
    c.execute(
        "UPDATE os_account_realms SET status = 1, merged_into = ?2, signature = ?3 WHERE id = ?1",
        params![source.id, dest.id, merged_signature()],
    )?;
    if dest.addr.is_none() && source.addr.is_some() {
        update_realm(c, dest, source.addr.as_deref(), None)?;
    } else if dest.name.is_none() && source.name.is_some() {
        update_realm(c, dest, None, source.name.as_deref())?;
    }
    Ok(())
}

/// Autopsy `moveOrMergeRealm` (used when hosts are merged).
pub(crate) fn move_or_merge_realm(c: &Connection, realm_id: i64, dest_host: i64) -> Result<()> {
    let source = realm_by_id(c, realm_id)?;
    let by_addr = match source.addr.as_deref() {
        Some(a) => realm_by(c, "addr", a, Some(dest_host))?,
        None => None,
    };
    let by_name = match source.name.as_deref() {
        Some(n) => realm_by(c, "name", n, Some(dest_host))?,
        None => None,
    };
    let dest = match (by_addr, by_name) {
        (Some(a), Some(n)) if a.id == n.id => Some(a),
        (Some(a), Some(n)) => {
            if n.addr.is_none() {
                merge_realms(c, &n, &a)?;
                Some(realm_by_id(c, a.id)?)
            } else {
                Some(a)
            }
        }
        (Some(a), None) => Some(a),
        (None, Some(n)) => (!(n.addr.is_some() && source.addr.is_some())).then_some(n),
        (None, None) => None,
    };
    match dest {
        Some(d) if d.id != source.id => merge_realms(c, &source, &d),
        Some(_) => Ok(()),
        None => {
            c.execute(
                "UPDATE os_account_realms SET scope_host_id = ?2 WHERE id = ?1",
                params![realm_id, dest_host],
            )?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- accounts

/// Signature of an account: its address, else its login name.
fn account_signature(addr: Option<&str>, login: Option<&str>) -> Result<String> {
    non_blank(addr)
        .or(non_blank(login))
        .map(str::to_owned)
        .ok_or_else(|| {
            DbError::Invalid("OS Account must have either a uniqueID or a login name.".into())
        })
}

fn account_by_id(c: &Connection, id: i64) -> Result<OsAccount> {
    Ok(c.prepare_cached(&format!(
        "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE id = ?1"
    ))?
    .query_row([id], OsAccount::from_row)?)
}

fn account_in_realm(
    c: &Connection,
    column: &str,
    value: &str,
    realm_id: i64,
) -> Result<Option<OsAccount>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE {column} = ?1 COLLATE NOCASE AND realm_id = ?2 AND db_status = 0 LIMIT 1"
    );
    Ok(c.prepare_cached(&sql)?
        .query_row(params![value, realm_id], OsAccount::from_row)
        .optional()?)
}

fn active_accounts_of_realm(c: &Connection, realm_id: i64) -> Result<Vec<OsAccount>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE realm_id = ?1 AND db_status = 0 ORDER BY id"
    ))?;
    let rows = st
        .query_map([realm_id], OsAccount::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn update_standard(c: &Connection, id: i64, u: &AccountUpdate) -> Result<bool> {
    let mut changed = false;
    if let Some(n) = non_blank(u.full_name.as_deref()) {
        c.execute(
            "UPDATE os_accounts SET full_name = ?2 WHERE id = ?1",
            params![id, n],
        )?;
        changed = true;
    }
    if let Some(k) = u.kind {
        c.execute(
            "UPDATE os_accounts SET kind = ?2 WHERE id = ?1",
            params![id, k as i64],
        )?;
        changed = true;
    }
    if let Some(s) = u.status {
        c.execute(
            "UPDATE os_accounts SET status = ?2 WHERE id = ?1",
            params![id, s as i64],
        )?;
        changed = true;
    }
    if let Some(t) = u.created_at {
        c.execute(
            "UPDATE os_accounts SET created_at = ?2 WHERE id = ?1",
            params![id, t],
        )?;
        changed = true;
    }
    if let Some(a) = u.admin {
        c.execute(
            "UPDATE os_accounts SET admin = ?2 WHERE id = ?1",
            params![id, a],
        )?;
        changed = true;
    }
    Ok(changed)
}

/// Autopsy `updateOsAccountCore`: sets a missing address or login name; an
/// address can never change once set.
fn update_core(
    c: &Connection,
    account: &OsAccount,
    addr: Option<&str>,
    login: Option<&str>,
) -> Result<bool> {
    let new_addr = real_sid(addr);
    if let (Some(new), Some(old)) = (new_addr, non_blank(account.addr.as_deref()))
        && !new.eq_ignore_ascii_case(old)
    {
        return Err(DbError::Invalid(format!(
            "Account ({}) already has an address ({old}), address cannot be updated.",
            account.id
        )));
    }
    let mut changed = false;
    if non_blank(account.addr.as_deref()).is_none()
        && let Some(a) = new_addr
    {
        c.execute(
            "UPDATE os_accounts SET addr = ?2 WHERE id = ?1",
            params![account.id, a],
        )?;
        changed = true;
    }
    if non_blank(account.login_name.as_deref()).is_none()
        && let Some(l) = non_blank(login)
    {
        c.execute(
            "UPDATE os_accounts SET login_name = ?2 WHERE id = ?1",
            params![account.id, l],
        )?;
        changed = true;
    }
    if changed {
        let cur = account_by_id(c, account.id)?;
        let sig = account_signature(cur.addr.as_deref(), cur.login_name.as_deref())?;
        c.execute(
            "UPDATE os_accounts SET signature = ?2 WHERE id = ?1",
            params![account.id, sig],
        )?;
    }
    Ok(changed)
}

/// Autopsy `getMatchingAccountForMerge`: same address, else same login name
/// when at most one of the two has an address.
fn matching_account<'a>(source: &OsAccount, candidates: &'a [OsAccount]) -> Option<&'a OsAccount> {
    let same = |a: Option<&str>, b: Option<&str>| match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    };
    if source.addr.is_some()
        && let Some(m) = candidates
            .iter()
            .find(|p| same(p.addr.as_deref(), source.addr.as_deref()))
    {
        return Some(m);
    }
    source.login_name.as_deref()?;
    candidates.iter().find(|p| {
        p.login_name
            .as_deref()
            .zip(source.login_name.as_deref())
            .is_some_and(|(a, b)| a.to_lowercase() == b.to_lowercase())
            && (source.addr.is_none() || p.addr.is_none())
    })
}

/// Autopsy `mergeOsAccounts`: every reference to `source` moves to `dest`,
/// `source` is marked merged, and `dest` takes the fields it lacks.
fn merge_accounts(c: &Connection, source: &OsAccount, dest: &OsAccount) -> Result<()> {
    c.execute(
        "UPDATE os_account_attributes SET os_account_id = ?2 WHERE os_account_id = ?1",
        params![source.id, dest.id],
    )?;
    c.execute(
        "DELETE FROM os_account_instances WHERE id IN (SELECT s.id FROM os_account_instances d \
         JOIN os_account_instances s ON d.ds_id = s.ds_id AND d.kind = s.kind \
         WHERE d.os_account_id = ?2 AND s.os_account_id = ?1)",
        params![source.id, dest.id],
    )?;
    for sql in [
        "UPDATE os_account_instances SET os_account_id = ?2 WHERE os_account_id = ?1",
        "UPDATE files SET os_account_id = ?2 WHERE os_account_id = ?1",
        "UPDATE artifacts SET os_account_id = ?2 WHERE os_account_id = ?1",
    ] {
        c.execute(sql, params![source.id, dest.id])?;
    }
    c.execute(
        "UPDATE os_accounts SET merged_into = ?2, db_status = 1, signature = ?3 WHERE id = ?1",
        params![source.id, dest.id, merged_signature()],
    )?;
    // Scores follow the surviving account.
    c.execute(
        "UPDATE OR IGNORE scores SET obj_id = ?2 WHERE obj_kind = 2 AND obj_id = ?1",
        params![source.id, dest.id],
    )?;
    c.execute(
        "DELETE FROM scores WHERE obj_kind = 2 AND obj_id = ?1",
        [source.id],
    )?;
    let login = dest
        .login_name
        .is_none()
        .then_some(source.login_name.as_deref())
        .flatten();
    let addr = dest
        .addr
        .is_none()
        .then_some(source.addr.as_deref())
        .flatten();
    update_core(c, dest, addr, login)?;
    update_standard(
        c,
        dest.id,
        &AccountUpdate {
            full_name: dest
                .full_name
                .is_none()
                .then(|| source.full_name.clone())
                .flatten(),
            created_at: dest
                .created_at
                .is_none()
                .then_some(source.created_at)
                .flatten(),
            ..AccountUpdate::default()
        },
    )?;
    Ok(())
}

/// Autopsy `mergeOsAccountsForRealms`.
fn merge_accounts_of_realms(c: &Connection, source: &Realm, dest: &Realm) -> Result<()> {
    for account in active_accounts_of_realm(c, source.id)? {
        let mut dest_accounts = active_accounts_of_realm(c, dest.id)?;
        // A source account with both address and login name may match two
        // destination accounts (one by each); combine those first.
        if account.addr.is_some() && account.login_name.is_some() {
            let dupes: Vec<OsAccount> = dest_accounts
                .iter()
                .filter(|p| {
                    p.addr == account.addr
                        || (p.login_name == account.login_name && p.addr.is_none())
                })
                .cloned()
                .collect();
            if let Some((first, rest)) = dupes.split_first() {
                for d in rest {
                    merge_accounts(c, d, &account_by_id(c, first.id)?)?;
                }
                if !rest.is_empty() {
                    dest_accounts = active_accounts_of_realm(c, dest.id)?;
                }
            }
        }
        match matching_account(&account, &dest_accounts) {
            Some(m) => merge_accounts(c, &account, &m.clone())?,
            None => {
                c.execute(
                    "UPDATE os_accounts SET realm_id = ?2 WHERE id = ?1",
                    params![account.id, dest.id],
                )?;
            }
        }
    }
    Ok(())
}

/// Autopsy `newWindowsOsAccount(sid, login, realm)`.
fn new_account_in_realm(
    c: &Connection,
    sid: Option<&str>,
    login: Option<&str>,
    realm: &Realm,
) -> Result<OsAccount> {
    let sid = real_sid(sid);
    if sid.is_none() && non_blank(login).is_none() {
        return Err(DbError::Invalid(
            "Cannot create OS account with both uniqueId and loginName as null.".into(),
        ));
    }
    if let Some(s) = sid
        && !windows::is_user_sid(s)
    {
        return Err(not_user_sid(s));
    }
    let mut login = windows::english_login_name(login);
    let well_known = sid.filter(|s| windows::is_well_known_sid(s));
    if let Some(n) = well_known
        .and_then(windows::well_known_login_name)
        .filter(|n| !n.is_empty())
    {
        login = Some(n.to_owned());
    }
    let signature = account_signature(sid, login.as_deref())?;
    let inserted = c
        .prepare_cached(
            "INSERT INTO os_accounts (realm_id, login_name, addr, signature, status) VALUES (?1, ?2, ?3, ?4, 0) \
             ON CONFLICT (signature, realm_id) DO NOTHING",
        )?
        .execute(params![realm.id, login, sid, signature])?;
    if inserted == 0 {
        // Already there: find it the way Autopsy does after its insert fails.
        if let Some(s) = sid
            && let Some(a) = account_in_realm(c, "addr", s, realm.id)?
        {
            return Ok(a);
        }
        if let Some(l) = login.as_deref()
            && let Some(a) = account_in_realm(c, "login_name", l, realm.id)?
        {
            return Ok(a);
        }
        return Err(DbError::Invalid(format!(
            "Error creating OsAccount with sid = {}, loginName = {}",
            sid.unwrap_or("Null"),
            login.as_deref().unwrap_or("Null")
        )));
    }
    let id = c.last_insert_rowid();
    if let Some(full) = well_known
        .and_then(windows::well_known_full_name)
        .filter(|f| !f.is_empty())
    {
        update_standard(
            c,
            id,
            &AccountUpdate {
                full_name: Some(full.to_owned()),
                ..AccountUpdate::default()
            },
        )?;
    }
    account_by_id(c, id)
}

/// Autopsy `getWindowsOsAccount`.
fn windows_account(
    c: &Connection,
    sid: Option<&str>,
    login: Option<&str>,
    realm_name: Option<&str>,
    host: i64,
) -> Result<Option<OsAccount>> {
    let mut sid = real_sid(sid);
    if sid.is_none() && non_blank(login).is_none() {
        return Err(DbError::Invalid(
            "Cannot get an OS account with both SID and loginName as null.".into(),
        ));
    }
    if sid.is_none() {
        sid = windows::well_known_account_sid(login, realm_name);
    }
    let Some(realm) = windows_realm(c, sid, realm_name, host)? else {
        return Ok(None);
    };
    if let Some(s) = sid {
        if !windows::is_user_sid(s) {
            return Err(not_user_sid(s));
        }
        if let Some(a) = account_in_realm(c, "addr", s, realm.id)? {
            return Ok(Some(a));
        }
    }
    match windows::english_login_name(login) {
        Some(l) => account_in_realm(c, "login_name", &l, realm.id),
        None => Ok(None),
    }
}

impl CaseDb {
    /// Creates (or finds) a Windows account referred to on `host` (Autopsy
    /// `newWindowsOsAccount`). Needs a SID or a login name, and a SID or a
    /// realm name. Group SIDs are refused.
    pub fn new_windows_os_account(
        &self,
        sid: Option<&str>,
        login: Option<&str>,
        realm_name: Option<&str>,
        host_id: i64,
        scope: RealmScope,
    ) -> Result<OsAccount> {
        let mut sid = real_sid(sid);
        if sid.is_none() && non_blank(login).is_none() {
            return Err(DbError::Invalid(
                "Cannot create OS account with both uniqueId and loginName as null.".into(),
            ));
        }
        if sid.is_none() && non_blank(realm_name).is_none() {
            return Err(DbError::Invalid(
                "Realm name or SID is required to create a Windows account.".into(),
            ));
        }
        if let Some(s) = sid
            && !windows::is_user_sid(s)
        {
            return Err(not_user_sid(s));
        }
        if sid.is_none() {
            sid = windows::well_known_account_sid(login, realm_name);
        }
        self.write(|tx| {
            let realm = match find_and_update_windows_realm(tx, sid, realm_name, host_id)? {
                Some(r) => r,
                None => new_windows_realm(tx, sid, realm_name, host_id, scope)?,
            };
            new_account_in_realm(tx, sid, login, &realm)
        })
    }

    /// Finds a Windows account as seen from `host` (Autopsy
    /// `getWindowsOsAccount`).
    pub fn windows_os_account(
        &self,
        sid: Option<&str>,
        login: Option<&str>,
        realm_name: Option<&str>,
        host_id: i64,
    ) -> Result<Option<OsAccount>> {
        self.read(|c| windows_account(c, sid, login, realm_name, host_id))
    }

    /// Fills in an account's missing SID, login name or realm details and
    /// merges it with an account that turns out to be the same (Autopsy
    /// `updateCoreWindowsOsAccountAttributes`).
    pub fn update_windows_os_account_core(
        &self,
        account_id: i64,
        sid: Option<&str>,
        login: Option<&str>,
        realm_name: Option<&str>,
        host_id: i64,
    ) -> Result<OsAccount> {
        self.write(|tx| {
            if real_sid(sid).is_some() || non_blank(realm_name).is_some() {
                find_and_update_windows_realm(tx, sid, realm_name, host_id)?;
            }
            let account = account_by_id(tx, account_id)?;
            let login = windows::english_login_name(login);
            if update_core(tx, &account, sid, login.as_deref())? {
                let updated = account_by_id(tx, account_id)?;
                let others: Vec<OsAccount> = active_accounts_of_realm(tx, updated.realm_id)?
                    .into_iter()
                    .filter(|a| a.id != updated.id)
                    .collect();
                if let Some(m) = matching_account(&updated, &others) {
                    merge_accounts(tx, &m.clone(), &updated)?;
                }
            }
            account_by_id(tx, account_id)
        })
    }

    /// Sets standard account properties (Autopsy
    /// `updateStandardOsAccountAttributes`).
    pub fn update_os_account(&self, account_id: i64, update: &AccountUpdate) -> Result<OsAccount> {
        self.write(|tx| {
            update_standard(tx, account_id, update)?;
            account_by_id(tx, account_id)
        })
    }

    /// Adds extended attributes to an account.
    pub fn add_os_account_attributes(
        &self,
        account_id: i64,
        attrs: &[OsAccountAttribute],
    ) -> Result<()> {
        self.write(|tx| {
            let mut st = tx.prepare_cached(
                "INSERT INTO os_account_attributes (os_account_id, host_id, source_file_id, type_id, value_text, value_int, value_double, value_blob) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for a in attrs {
                let (text, int, double, blob) = a.value.columns();
                st.execute(params![account_id, a.host_id, a.source_file_id, a.type_id, text, int, double, blob])?;
            }
            Ok(())
        })
    }

    /// Extended attributes of an account, in insertion order:
    /// `(attribute, attribute name, display name)`.
    pub fn os_account_attributes(
        &self,
        account_id: i64,
    ) -> Result<Vec<(OsAccountAttribute, String, String)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT a.type_id, a.host_id, a.source_file_id, a.value_text, a.value_int, a.value_double, a.value_blob, t.value_type, t.name, t.display_name \
                 FROM os_account_attributes a JOIN attribute_types t ON t.id = a.type_id WHERE a.os_account_id = ?1 ORDER BY a.id",
            )?;
            let rows = st
                .query_map([account_id], |r| {
                    let value = Value::from_columns(r.get(7)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
                    Ok((
                        OsAccountAttribute {
                            type_id: r.get(0)?,
                            value,
                            host_id: r.get(1)?,
                            source_file_id: r.get(2)?,
                        },
                        r.get(8)?,
                        r.get(9)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Records that an account appears on a data source. Only the most
    /// significant kind per data source is kept (launched > accessed >
    /// referenced), as Autopsy reports it.
    pub fn add_os_account_instance(
        &self,
        account_id: i64,
        ds_id: i64,
        kind: InstanceKind,
    ) -> Result<()> {
        self.write(|tx| {
            let added = tx
                .prepare_cached(
                    "INSERT INTO os_account_instances (os_account_id, ds_id, kind) SELECT ?1, ?2, ?3 \
                     WHERE NOT EXISTS (SELECT 1 FROM os_account_instances WHERE os_account_id = ?1 AND ds_id = ?2 AND kind <= ?3)",
                )?
                .execute(params![account_id, ds_id, kind as i64])?;
            if added > 0 {
                tx.prepare_cached(
                    "DELETE FROM os_account_instances WHERE os_account_id = ?1 AND ds_id = ?2 AND kind > ?3",
                )?
                .execute(params![account_id, ds_id, kind as i64])?;
            }
            Ok(())
        })
    }

    /// Instances of an account (one per data source).
    pub fn os_account_instances(&self, account_id: i64) -> Result<Vec<OsAccountInstance>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT id, os_account_id, ds_id, kind FROM os_account_instances WHERE os_account_id = ?1 ORDER BY ds_id, kind",
            )?;
            let rows = st
                .query_map([account_id], |r| {
                    Ok(OsAccountInstance {
                        id: r.get(0)?,
                        os_account_id: r.get(1)?,
                        ds_id: r.get(2)?,
                        kind: InstanceKind::from_code(r.get(3)?).unwrap_or(InstanceKind::Referenced),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn os_account(&self, id: i64) -> Result<Option<OsAccount>> {
        self.read(|c| {
            Ok(c.prepare_cached(&format!(
                "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE id = ?1"
            ))?
            .query_row([id], OsAccount::from_row)
            .optional()?)
        })
    }

    /// Active accounts, optionally only those with an instance on a data
    /// source (Autopsy `getOsAccountsByDataSourceObjId`), keyset paged by id.
    pub fn os_accounts(
        &self,
        ds_id: Option<i64>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<OsAccount>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE db_status = 0 AND id > ?2 \
                 AND (?1 IS NULL OR id IN (SELECT os_account_id FROM os_account_instances WHERE ds_id = ?1)) \
                 ORDER BY id LIMIT ?3"
            ))?;
            let rows = st
                .query_map(params![ds_id, after_id, limit as i64], OsAccount::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The number of active accounts (optionally seen on one data source).
    pub fn os_account_count(&self, ds_id: Option<i64>) -> Result<u64> {
        self.read(|c| {
            let n: i64 = c
                .prepare_cached(
                    "SELECT COUNT(*) FROM os_accounts WHERE db_status = 0 \
                     AND (?1 IS NULL OR id IN (SELECT os_account_id FROM os_account_instances WHERE ds_id = ?1))",
                )?
                .query_row([ds_id], |r| r.get(0))?;
            Ok(n.unsigned_abs())
        })
    }

    /// Active accounts seen on any data source of a host (Autopsy
    /// `getOsAccounts(Host)`).
    pub fn os_accounts_of_host(&self, host_id: i64) -> Result<Vec<OsAccount>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ACCOUNT_COLS} FROM os_accounts WHERE db_status = 0 AND id IN \
                 (SELECT i.os_account_id FROM os_account_instances i JOIN data_sources d ON d.id = i.ds_id WHERE d.host_id = ?1) ORDER BY id"
            ))?;
            let rows = st
                .query_map([host_id], OsAccount::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Hosts an account was seen on (names, sorted).
    pub fn hosts_of_os_account(&self, account_id: i64) -> Result<Vec<String>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT DISTINCT h.name FROM hosts h JOIN data_sources d ON d.host_id = h.id \
                 JOIN os_account_instances i ON i.ds_id = d.id WHERE i.os_account_id = ?1 ORDER BY h.name COLLATE NOCASE",
            )?;
            let rows = st
                .query_map([account_id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn os_account_realm(&self, realm_id: i64) -> Result<Realm> {
        self.read(|c| realm_by_id(c, realm_id))
    }

    /// Creates a Windows realm (Autopsy `newWindowsRealm`); an existing realm
    /// with the same signature is returned instead.
    pub fn new_windows_realm(
        &self,
        sid: Option<&str>,
        realm_name: Option<&str>,
        host_id: i64,
        scope: RealmScope,
    ) -> Result<Realm> {
        self.write(|tx| new_windows_realm(tx, sid, realm_name, host_id, scope))
    }

    /// Finds the realm of a SID or realm name as seen from a host (Autopsy
    /// `getWindowsRealm`).
    pub fn windows_realm(
        &self,
        sid: Option<&str>,
        realm_name: Option<&str>,
        host_id: i64,
    ) -> Result<Option<Realm>> {
        self.read(|c| windows_realm(c, sid, realm_name, host_id))
    }

    /// Fills in a realm's missing address or name (Autopsy `updateRealm`);
    /// returns whether it changed.
    pub fn update_os_account_realm(
        &self,
        realm_id: i64,
        addr: Option<&str>,
        name: Option<&str>,
    ) -> Result<bool> {
        self.write(|tx| {
            let realm = realm_by_id(tx, realm_id)?;
            update_realm(tx, &realm, addr, name)
        })
    }

    /// Merges realm `source` into `dest` (Autopsy `mergeRealms`).
    pub fn merge_os_account_realms(&self, source: i64, dest: i64) -> Result<()> {
        self.write(|tx| {
            let (s, d) = (realm_by_id(tx, source)?, realm_by_id(tx, dest)?);
            merge_realms(tx, &s, &d)
        })
    }

    /// Active accounts of a realm.
    pub fn os_accounts_of_realm(&self, realm_id: i64) -> Result<Vec<OsAccount>> {
        self.read(|c| active_accounts_of_realm(c, realm_id))
    }

    /// Links a data artifact to the account it belongs to.
    pub fn set_artifact_os_account(&self, artifact_id: i64, account_id: Option<i64>) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE artifacts SET os_account_id = ?2 WHERE id = ?1",
                params![artifact_id, account_id],
            )?;
            Ok(())
        })
    }

    /// Sets a file's owner (SID/uid text and the resolved account).
    pub fn set_file_owner(
        &self,
        file_id: i64,
        owner_uid: Option<&str>,
        account_id: Option<i64>,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE files SET owner_uid = ?2, os_account_id = ?3 WHERE id = ?1",
                params![file_id, owner_uid, account_id],
            )?;
            Ok(())
        })
    }
}
