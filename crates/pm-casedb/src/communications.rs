//! Communication accounts and relationships (Autopsy `CommunicationsManager`).
//!
//! An *account* (a phone number, an e-mail address, a Skype id, …) is unique
//! per case; an *account instance* is a `KAAMOS_ACCOUNT` artifact on the file
//! where the account was found. Messages, call logs and contacts relate
//! every pair of their participants. The Communications view reads the
//! relationship table through its indexes, so it never scans artifacts.

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog::{artifact_type_id, attribute_type_id};
use crate::{CaseDb, DbError, NewArtifact, Result, Value};

/// A communication account type (`PHONE`, `EMAIL`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountType {
    pub id: i64,
    pub name: String,
    pub display_name: String,
}

/// A case-level account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub type_id: i64,
    pub type_name: String,
    /// Normalised identifier.
    pub identifier: String,
}

/// Relationship kinds (Autopsy `Relationship.Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationshipKind {
    Message = 1,
    CallLog = 2,
    Contact = 3,
}

impl RelationshipKind {
    pub fn from_code(c: i64) -> Option<Self> {
        match c {
            1 => Some(Self::Message),
            2 => Some(Self::CallLog),
            3 => Some(Self::Contact),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Message => "Message",
            Self::CallLog => "Call Log",
            Self::Contact => "Contact",
        }
    }

    /// The artifact types a relationship of this kind may come from
    /// (Autopsy `Relationship.Type.isCreatableFrom`).
    pub fn source_artifact_types(self) -> &'static [&'static str] {
        match self {
            Self::Message => &["KAAMOS_EMAIL_MSG", "KAAMOS_MESSAGE"],
            Self::CallLog => &["KAAMOS_CALLLOG"],
            Self::Contact => &["KAAMOS_CONTACT"],
        }
    }
}

/// Phone characters allowed besides digits, spaces and a leading `+`.
const TELEPHONY_CHARS: &[char] = &['-', '(', ')', '#', '*', ','];
const MIN_PHONE_LEN: usize = 3;

/// Autopsy `CommunicationsUtils.isValidPhoneNumber`.
pub fn is_valid_phone_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars().enumerate().all(|(i, c)| {
            (c == '+' && i == 0)
                || c.is_whitespace()
                || c.is_numeric()
                || TELEPHONY_CHARS.contains(&c)
        })
}

/// Autopsy `CommunicationsUtils.isValidEmailAddress`.
pub fn is_valid_email_address(s: &str) -> bool {
    if !s.contains('@') || !s.contains('.') {
        return false;
    }
    // Java `split` drops trailing empty strings.
    let mut parts = s.split('@');
    let (Some(user), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if user.is_empty() || domain.is_empty() {
        return false;
    }
    let mut labels = domain.split('.');
    matches!((labels.next(), labels.next()), (Some(a), Some(b)) if !a.is_empty() && !b.is_empty())
}

/// Normalises an account identifier (Autopsy `normalizeAccountID`): phone
/// numbers lose spaces, dashes and parentheses; e-mail addresses are lower
/// cased without `;`; other ids are lower cased and trimmed.
pub fn normalize_account_id(type_name: &str, id: &str) -> Result<String> {
    if id.is_empty() {
        return Err(DbError::Invalid("Account id is null or empty.".into()));
    }
    match type_name {
        "PHONE" => {
            if !is_valid_phone_number(id) {
                return Err(DbError::Invalid(format!(
                    "Input string is not a valid phone number: {id}"
                )));
            }
            let n: String = id
                .trim()
                .chars()
                .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')'))
                .collect();
            if n.chars().count() < MIN_PHONE_LEN {
                return Err(DbError::Invalid(format!(
                    "Invalid phone number string {id}"
                )));
            }
            Ok(n)
        }
        "EMAIL" => {
            if !is_valid_email_address(id) {
                return Err(DbError::Invalid(format!(
                    "Input string is not a valid email address: {id}"
                )));
            }
            Ok(id.to_lowercase().replace(';', "").trim().to_owned())
        }
        _ => Ok(id.to_lowercase().trim().to_owned()),
    }
}

/// Filters of the Communications view (Autopsy `CommunicationsFilter`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommsFilter {
    /// Only relationships from these data sources (empty: all).
    pub ds_ids: Vec<i64>,
    /// Only accounts of these types (empty: all).
    pub account_type_ids: Vec<i64>,
    /// Only these relationship kinds (empty: all).
    pub kinds: Vec<RelationshipKind>,
    /// Inclusive Unix-second range on the relationship time.
    pub start: Option<i64>,
    pub end: Option<i64>,
}

impl CommsFilter {
    /// SQL conditions on `account_relationships` alias `r`.
    fn relationship_sql(&self) -> String {
        let mut w = Vec::new();
        if !self.ds_ids.is_empty() {
            w.push(format!("r.ds_id IN ({})", join(&self.ds_ids)));
        }
        if !self.kinds.is_empty() {
            let k: Vec<i64> = self.kinds.iter().map(|k| *k as i64).collect();
            w.push(format!("r.kind IN ({})", join(&k)));
        }
        if let Some(s) = self.start {
            w.push(format!("r.date_time >= {s}"));
        }
        if let Some(e) = self.end {
            w.push(format!("r.date_time <= {e}"));
        }
        if w.is_empty() {
            "1".into()
        } else {
            w.join(" AND ")
        }
    }

    fn account_type_sql(&self, alias: &str) -> String {
        if self.account_type_ids.is_empty() {
            "1".into()
        } else {
            format!("{alias}.type_id IN ({})", join(&self.account_type_ids))
        }
    }
}

fn join(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

/// Where and how an account was found (for
/// [`CaseDb::account_instance`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AccountInstanceSpec<'a> {
    /// Account type name (`PHONE`, `EMAIL`, …).
    pub type_name: &'a str,
    /// The identifier as found; normalised before it is stored.
    pub id: &'a str,
    /// Producing module (attribute source).
    pub module: &'a str,
    pub file_id: i64,
    pub ds_id: i64,
    /// Further attributes of the instance artifact.
    pub extra: Vec<(i64, Value, String)>,
    pub now: i64,
}

/// An account as seen on one data source, with its relationship count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeviceInstance {
    pub account: Account,
    pub ds_id: i64,
    pub relationships: u64,
}

fn account_type_named(c: &Connection, name: &str) -> Result<Option<AccountType>> {
    Ok(
        c.prepare_cached("SELECT id, name, display_name FROM account_types WHERE name = ?1")?
            .query_row([name], |r| {
                Ok(AccountType {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    display_name: r.get(2)?,
                })
            })
            .optional()?,
    )
}

const ACCOUNT_SELECT: &str = "SELECT a.id, a.type_id, t.name, a.identifier FROM accounts a JOIN account_types t ON t.id = a.type_id";

fn account_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        type_id: r.get(1)?,
        type_name: r.get(2)?,
        identifier: r.get(3)?,
    })
}

fn account_or_new(c: &Connection, type_name: &str, id: &str) -> Result<Account> {
    let ty = account_type_named(c, type_name)?
        .ok_or_else(|| DbError::Invalid(format!("unknown account type {type_name}")))?;
    let ident = normalize_account_id(type_name, id)?;
    c.prepare_cached("INSERT OR IGNORE INTO accounts (type_id, identifier) VALUES (?1, ?2)")?
        .execute(params![ty.id, ident])?;
    Ok(c.prepare_cached(&format!(
        "{ACCOUNT_SELECT} WHERE a.type_id = ?1 AND a.identifier = ?2"
    ))?
    .query_row(params![ty.id, ident], account_from_row)?)
}

impl CaseDb {
    /// Registers a custom account type (or returns the existing one).
    pub fn add_account_type(&self, name: &str, display_name: &str) -> Result<AccountType> {
        self.write(|tx| {
            if let Some(t) = account_type_named(tx, name)? {
                return Ok(t);
            }
            tx.execute(
                "INSERT INTO account_types (name, display_name) VALUES (?1, ?2)",
                params![name, display_name],
            )?;
            Ok(AccountType {
                id: tx.last_insert_rowid(),
                name: name.to_owned(),
                display_name: display_name.to_owned(),
            })
        })
    }

    pub fn account_type(&self, name: &str) -> Result<Option<AccountType>> {
        self.read(|c| account_type_named(c, name))
    }

    /// Account types that have at least one account.
    pub fn account_types_in_use(&self) -> Result<Vec<AccountType>> {
        self.read(|c| {
            let mut st = c.prepare(
                "SELECT id, name, display_name FROM account_types t WHERE EXISTS (SELECT 1 FROM accounts a WHERE a.type_id = t.id) ORDER BY display_name",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(AccountType {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        display_name: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Finds or creates a case-level account.
    pub fn account_or_new(&self, type_name: &str, id: &str) -> Result<Account> {
        self.write(|tx| account_or_new(tx, type_name, id))
    }

    pub fn account(&self, type_name: &str, id: &str) -> Result<Option<Account>> {
        let ident = normalize_account_id(type_name, id)?;
        self.read(|c| {
            Ok(c.prepare_cached(&format!(
                "{ACCOUNT_SELECT} WHERE t.name = ?1 AND a.identifier = ?2"
            ))?
            .query_row(params![type_name, ident], account_from_row)
            .optional()?)
        })
    }

    pub fn account_by_id(&self, id: i64) -> Result<Option<Account>> {
        self.read(|c| {
            Ok(
                c.prepare_cached(&format!("{ACCOUNT_SELECT} WHERE a.id = ?1"))?
                    .query_row([id], account_from_row)
                    .optional()?,
            )
        })
    }

    /// Creates an account instance (Autopsy `createAccountFileInstance`): the
    /// account, and a `KAAMOS_ACCOUNT` artifact on `file_id` with
    /// `KAAMOS_ACCOUNT_TYPE` and the normalised `KAAMOS_ID` unless the file
    /// already has one. Returns the account and the instance artifact id.
    pub fn account_instance(&self, inst: AccountInstanceSpec<'_>) -> Result<(Account, i64)> {
        self.write(|tx| account_instance_tx(tx, inst))
    }

    /// Relates every pair of `participants` through `source_artifact_id`
    /// (Autopsy `addRelationships`; duplicates are ignored, the direction is
    /// not kept). `date_time` ≤ 0 is stored as unknown.
    pub fn add_relationships(
        &self,
        participants: &[i64],
        source_artifact_id: i64,
        kind: RelationshipKind,
        date_time: Option<i64>,
        ds_id: i64,
    ) -> Result<()> {
        self.write(|tx| {
            relationships_tx(tx, participants, source_artifact_id, kind, date_time, ds_id)
        })
    }

    /// Accounts with at least one relationship passing `filter`, per data
    /// source, with their relationship counts (Autopsy
    /// `getAccountDeviceInstancesWithRelationships`).
    pub fn account_device_instances(
        &self,
        filter: &CommsFilter,
    ) -> Result<Vec<AccountDeviceInstance>> {
        let rel = filter.relationship_sql();
        let ty = filter.account_type_sql("a");
        let sql = format!(
            "WITH ends AS (SELECT r.account1_id AS acc, r.ds_id AS ds, r.id AS rid FROM account_relationships r WHERE {rel} \
                     UNION ALL SELECT r.account2_id, r.ds_id, r.id FROM account_relationships r WHERE {rel}) \
             SELECT a.id, a.type_id, t.name, a.identifier, e.ds, COUNT(DISTINCT e.rid) FROM ends e \
             JOIN accounts a ON a.id = e.acc JOIN account_types t ON t.id = a.type_id \
             WHERE {ty} AND t.name <> 'DEVICE' GROUP BY a.id, e.ds ORDER BY t.display_name, a.identifier"
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map([], |r| {
                    Ok(AccountDeviceInstance {
                        account: account_from_row(r)?,
                        ds_id: r.get(4)?,
                        relationships: r.get::<_, i64>(5)?.max(0) as u64,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Accounts related to `account_id` with the number of relationships
    /// shared with it (Autopsy `getRelatedAccountDeviceInstances` +
    /// `getRelationshipCountsPairwise`).
    pub fn related_accounts(
        &self,
        account_id: i64,
        filter: &CommsFilter,
    ) -> Result<Vec<(Account, u64)>> {
        let rel = filter.relationship_sql();
        let ty = filter.account_type_sql("a");
        let sql = format!(
            "WITH other AS (SELECT r.account2_id AS acc FROM account_relationships r WHERE r.account1_id = ?1 AND {rel} \
                      UNION ALL SELECT r.account1_id FROM account_relationships r WHERE r.account2_id = ?1 AND {rel}) \
             SELECT a.id, a.type_id, t.name, a.identifier, COUNT(*) FROM other o \
             JOIN accounts a ON a.id = o.acc JOIN account_types t ON t.id = a.type_id \
             WHERE {ty} GROUP BY a.id ORDER BY COUNT(*) DESC, a.identifier"
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map([account_id], |r| {
                    Ok((account_from_row(r)?, r.get::<_, i64>(4)?.max(0) as u64))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Artifact ids of the messages, calls and contacts relating
    /// `account_id` (and `other`, when given), newest first.
    pub fn relationship_sources(
        &self,
        account_id: i64,
        other: Option<i64>,
        filter: &CommsFilter,
    ) -> Result<Vec<i64>> {
        let rel = filter.relationship_sql();
        let sql = format!(
            "SELECT DISTINCT r.source_artifact_id FROM account_relationships r \
             WHERE ((r.account1_id = ?1 AND (?2 IS NULL OR r.account2_id = ?2)) \
                 OR (r.account2_id = ?1 AND (?2 IS NULL OR r.account1_id = ?2))) AND {rel} \
             ORDER BY r.date_time DESC, r.source_artifact_id"
        );
        self.read(|c| {
            let mut st = c.prepare(&sql)?;
            let rows = st
                .query_map(params![account_id, other], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Accounts taking part in the relationships of an artifact (Autopsy
    /// `getAccountsRelatedToArtifact`).
    pub fn accounts_of_artifact(&self, artifact_id: i64) -> Result<Vec<Account>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{ACCOUNT_SELECT} WHERE a.id IN (SELECT account1_id FROM account_relationships WHERE source_artifact_id = ?1 \
                 UNION SELECT account2_id FROM account_relationships WHERE source_artifact_id = ?1) ORDER BY a.id"
            ))?;
            let rows = st
                .query_map([artifact_id], account_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// `KAAMOS_ACCOUNT` instance artifacts of an account (Autopsy
    /// `getAccountFileInstances`).
    pub fn account_instances_of(&self, account: &Account) -> Result<Vec<i64>> {
        let art_type = artifact_type_id("KAAMOS_ACCOUNT").unwrap_or(39);
        let (Some(type_attr), Some(id_attr)) = (
            attribute_type_id("KAAMOS_ACCOUNT_TYPE"),
            attribute_type_id("KAAMOS_ID"),
        ) else {
            return Ok(Vec::new());
        };
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT a.id FROM attributes i JOIN artifacts a ON a.id = i.artifact_id \
                 JOIN attributes t ON t.artifact_id = a.id AND t.type_id = ?3 AND t.value_text = ?4 \
                 WHERE i.type_id = ?1 AND i.value_text = ?2 AND a.type_id = ?5 ORDER BY a.id",
            )?;
            let rows = st
                .query_map(
                    params![id_attr, account.identifier, type_attr, account.type_name, art_type],
                    |r| r.get(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

/// [`CaseDb::account_instance`] inside a transaction.
pub(crate) fn account_instance_tx(
    tx: &Connection,
    inst: AccountInstanceSpec<'_>,
) -> Result<(Account, i64)> {
    let AccountInstanceSpec {
        type_name,
        id,
        module,
        file_id,
        ds_id,
        extra,
        now,
    } = inst;
    let art_type = artifact_type_id("KAAMOS_ACCOUNT").unwrap_or(39);
    let (Some(type_attr), Some(id_attr)) = (
        attribute_type_id("KAAMOS_ACCOUNT_TYPE"),
        attribute_type_id("KAAMOS_ID"),
    ) else {
        return Err(DbError::Invalid(
            "account attributes missing from the catalogue".into(),
        ));
    };
    let account = account_or_new(tx, type_name, id)?;
    let existing: Option<i64> = tx
        .prepare_cached(
            "SELECT a.id FROM artifacts a \
             JOIN attributes t ON t.artifact_id = a.id AND t.type_id = ?3 AND t.value_text = ?4 \
             JOIN attributes i ON i.artifact_id = a.id AND i.type_id = ?5 AND i.value_text = ?6 \
             WHERE a.file_id = ?1 AND a.type_id = ?2 LIMIT 1",
        )?
        .query_row(
            params![
                file_id,
                art_type,
                type_attr,
                type_name,
                id_attr,
                account.identifier
            ],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(a) = existing {
        return Ok((account, a));
    }
    let mut attributes = vec![
        (
            type_attr,
            Value::Text(type_name.to_owned()),
            module.to_owned(),
        ),
        (
            id_attr,
            Value::Text(account.identifier.clone()),
            module.to_owned(),
        ),
    ];
    attributes.extend(extra);
    let art = crate::insert_artifact(
        tx,
        &NewArtifact {
            type_id: art_type,
            ds_id,
            file_id: Some(file_id),
            source_artifact_id: None,
            score: None,
            conclusion: None,
            configuration: None,
            justification: None,
            created_at: now,
            attributes,
        },
    )?;
    Ok((account, art))
}

/// [`CaseDb::add_relationships`] inside a transaction.
pub(crate) fn relationships_tx(
    tx: &Connection,
    participants: &[i64],
    source_artifact_id: i64,
    kind: RelationshipKind,
    date_time: Option<i64>,
    ds_id: i64,
) -> Result<()> {
    let date_time = date_time.filter(|t| *t > 0);
    let mut st = tx.prepare_cached(
        "INSERT OR IGNORE INTO account_relationships (account1_id, account2_id, source_artifact_id, date_time, kind, ds_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (i, a) in participants.iter().enumerate() {
        for b in participants.iter().skip(i + 1) {
            st.execute(params![
                a,
                b,
                source_artifact_id,
                date_time,
                kind as i64,
                ds_id
            ])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_normalisation_matches_autopsy() {
        assert_eq!(
            normalize_account_id("PHONE", " +1 (555) 123-4567 ").unwrap(),
            "+15551234567"
        );
        assert_eq!(normalize_account_id("PHONE", "*86#").unwrap(), "*86#");
        assert!(normalize_account_id("PHONE", "12a").is_err());
        assert!(
            normalize_account_id("PHONE", "1+2").is_err(),
            "+ only leading"
        );
        assert!(
            normalize_account_id("PHONE", "(1)").is_err(),
            "too short after cleanup"
        );
    }

    #[test]
    fn email_normalisation_matches_autopsy() {
        assert_eq!(
            normalize_account_id("EMAIL", "Bob@Example.COM;").unwrap(),
            "bob@example.com"
        );
        assert!(normalize_account_id("EMAIL", "bob@example").is_err());
        assert!(normalize_account_id("EMAIL", "@example.com").is_err());
        assert!(normalize_account_id("EMAIL", "bob@.com").is_err());
    }

    #[test]
    fn other_ids_are_lower_cased() {
        assert_eq!(
            normalize_account_id("SKYPE", " Live:Bob ").unwrap(),
            "live:bob"
        );
        assert!(normalize_account_id("SKYPE", "").is_err());
    }
}
