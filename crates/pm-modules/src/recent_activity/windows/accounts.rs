//! OS accounts from the SAM and `ProfileList` (Autopsy `ExtractRegistry`
//! `createOrUpdateOsAccount` / `updateOsAccount`).
//!
//! Autopsy records Windows users as OS accounts (not artifacts): an account
//! per SID in the host's realm, a "launched" instance on the data source,
//! the login and full name, the creation time, and extended attributes for
//! everything else the SAM holds.

use std::collections::BTreeMap;

use pm_casedb::catalog::{ValueType, attribute_type_id};
use pm_casedb::communications::AccountInstanceSpec;
use pm_casedb::os_accounts::{AccountUpdate, InstanceKind, OsAccountAttribute, RealmScope};
use pm_casedb::{CaseDb, DbError, Host, Value, default_host_name};
use pm_ingest::{JobContext, ModuleError};
use pm_winartifacts::registry::sam;

use super::art::Types;

/// What one account looks like after all hives were read.
#[derive(Debug, Default)]
pub(super) struct Account {
    /// The hive the account was found in.
    pub(super) file_id: i64,
    pub(super) login: String,
    pub(super) full_name: Option<String>,
    /// Seen in `ProfileList` but not in the local SAM: a domain account.
    pub(super) domain: bool,
    pub(super) home_dir: Option<String>,
    pub(super) sam: Option<sam::SamUser>,
    pub(super) groups: Vec<String>,
}

/// The data source's host, created with TSK's default name if the data
/// source has none (cases loaded before schema v2).
fn host(db: &CaseDb, ds_id: i64) -> Result<Host, DbError> {
    if let Some(h) = db.host_of_data_source(ds_id)? {
        return Ok(h);
    }
    let first = db
        .data_source(ds_id)?
        .and_then(|d| d.paths.into_iter().next());
    let h = db.host_by_name_or_new(&default_host_name(first.as_deref(), ds_id))?;
    db.set_data_source_host(ds_id, h.id)?;
    Ok(h)
}

/// Creates or updates the OS account of every SID; returns how many were
/// recorded.
pub(super) fn record(
    ctx: &JobContext,
    types: &mut Types,
    accounts: &BTreeMap<String, Account>,
    domain_name: Option<&str>,
) -> Result<usize, ModuleError> {
    let db = ctx.case.db();
    let host = host(db, ctx.ds_id)?;
    let questions: Vec<(i64, i64)> = (1..=3)
        .map(|i| {
            (
                types.attribute(
                    ctx,
                    &format!("RA_SAM_QUESTION_{i}"),
                    &format!("Security Question {i}"),
                    ValueType::String,
                ),
                types.attribute(
                    ctx,
                    &format!("RA_SAM_ANSWER_{i}"),
                    &format!("Security Answer {i}"),
                    ValueType::String,
                ),
            )
        })
        .collect();
    let mut n = 0;
    for (sid, a) in accounts {
        if ctx.is_cancelled() {
            break;
        }
        match record_one(ctx, db, &host, sid, a, domain_name, &questions) {
            Ok(()) => n += 1,
            // Group SIDs and malformed SIDs are skipped, as Autopsy logs
            // `NotUserSIDException` and moves on.
            Err(e) => tracing::warn!(sid, error = %e, "OS account not recorded"),
        }
    }
    Ok(n)
}

fn record_one(
    ctx: &JobContext,
    db: &CaseDb,
    host: &Host,
    sid: &str,
    a: &Account,
    domain_name: Option<&str>,
    questions: &[(i64, i64)],
) -> Result<(), ModuleError> {
    let login = Some(a.login.as_str()).filter(|l| !l.is_empty());
    // ProfileList accounts not in the local SAM are domain accounts.
    let (domain, scope) = if a.domain && a.sam.is_none() {
        (domain_name.filter(|d| !d.is_empty()), RealmScope::Domain)
    } else {
        (None, RealmScope::Local)
    };
    let account = match db.windows_os_account(Some(sid), None, None, host.id)? {
        Some(existing) => existing,
        // SAM users are created without a login, which the update below
        // sets (Autopsy order); ProfileList users with theirs.
        None if a.sam.is_some() => {
            db.new_windows_os_account(Some(sid), None, None, host.id, scope)?
        }
        None => db.new_windows_os_account(Some(sid), login, domain, host.id, scope)?,
    };
    db.add_os_account_instance(account.id, ctx.ds_id, InstanceKind::Launched)?;
    if login.is_some() || domain.is_some() {
        db.update_windows_os_account_core(account.id, None, login, domain, host.id)?;
    }

    let mut attrs = Vec::new();
    let mut add = |name: &str, value: Option<Value>| {
        if let (Some(type_id), Some(value)) = (attribute_type_id(name), value) {
            attrs.push((type_id, value));
        }
    };
    if let Some(dir) = &a.home_dir {
        add("TSK_HOME_DIR", Some(Value::Text(dir.clone())));
    }
    if let Some(u) = &a.sam {
        add("TSK_DATETIME_ACCESSED", u.last_login.map(Value::DateTime));
        add("TSK_COUNT", Some(Value::Int(i64::from(u.login_count))));
        if u.is_admin() {
            add("TSK_IS_ADMIN", Some(Value::Int(1)));
        }
        add("TSK_DESCRIPTION", u.comment.clone().map(Value::Text));
        add("TSK_EMAIL", u.internet_name.clone().map(Value::Text));
        add(
            "TSK_DATETIME_PASSWORD_RESET",
            u.password_reset.map(Value::DateTime),
        );
        add(
            "TSK_PASSWORD_HINT",
            u.password_hint.clone().map(Value::Text),
        );
        add(
            "TSK_DATETIME_PASSWORD_FAIL",
            u.password_fail.map(Value::DateTime),
        );
        let flags = |names: &[&str]| {
            let s = settings(u, names);
            (!s.is_empty()).then_some(Value::Text(s))
        };
        add(
            "TSK_PASSWORD_SETTINGS",
            flags(&["Password does not expire", "Password not required"]),
        );
        add(
            "TSK_ACCOUNT_SETTINGS",
            flags(&[
                "Account auto locked",
                "Home directory required",
                "Account Disabled",
            ]),
        );
        add(
            "TSK_FLAG",
            flags(&[
                "Normal user account",
                "Server trust account",
                "Workstation trust account",
                "Interdomain trust account",
                "MNS logon user account",
                "Temporary duplicate account",
            ]),
        );
        // Autopsy only adds groups when the list is *empty* (an inverted
        // test); postmortem adds them when there are any.
        if !a.groups.is_empty() {
            add("TSK_GROUPS", Some(Value::Text(a.groups.join(", "))));
        }
        if let Some(email) = &u.internet_name {
            let spec = AccountInstanceSpec {
                type_name: "EMAIL",
                id: email,
                module: super::super::NAME,
                file_id: a.file_id,
                ds_id: ctx.ds_id,
                extra: Vec::new(),
                now: ctx.now(),
            };
            if let Err(e) = db.account_instance(spec) {
                tracing::debug!(error = %e, email, "SAM e-mail is not a valid address");
            }
        }
    }
    let mut extended: Vec<(i64, Value)> = attrs;
    if let Some(u) = &a.sam {
        for ((q, an), (qt, at)) in u.security_questions.iter().zip(questions) {
            extended.push((*qt, Value::Text(q.clone())));
            extended.push((*at, Value::Text(an.clone())));
        }
    }
    add_new_attributes(db, account.id, host.id, a.file_id, extended)?;

    let full_name = a.full_name.clone();
    let created = a.sam.as_ref().and_then(|u| u.created);
    if full_name.is_some() || created.is_some() {
        db.update_os_account(
            account.id,
            &AccountUpdate {
                full_name,
                created_at: created,
                ..AccountUpdate::default()
            },
        )?;
    }
    Ok(())
}

/// Adds the attributes the account does not already have from the same
/// source file, so re-running ingest adds nothing (Autopsy appends
/// duplicates on every run).
fn add_new_attributes(
    db: &CaseDb,
    account_id: i64,
    host_id: i64,
    file_id: i64,
    attrs: Vec<(i64, Value)>,
) -> Result<(), DbError> {
    let existing = db.os_account_attributes(account_id)?;
    let new: Vec<OsAccountAttribute> = attrs
        .into_iter()
        .filter(|(t, v)| {
            !existing.iter().any(|(e, _, _)| {
                e.type_id == *t && e.value == *v && e.source_file_id == Some(file_id)
            })
        })
        .map(|(type_id, value)| OsAccountAttribute {
            type_id,
            value,
            host_id: Some(host_id),
            source_file_id: Some(file_id),
        })
        .collect();
    if new.is_empty() {
        return Ok(());
    }
    db.add_os_account_attributes(account_id, &new)
}

/// Autopsy's `getSettingsFromMap`: the account's flag names that are in
/// `keys`, comma-joined.
fn settings(u: &sam::SamUser, keys: &[&str]) -> String {
    let set = u.flag_names();
    keys.iter()
        .filter(|k| set.contains(k))
        .copied()
        .collect::<Vec<_>>()
        .join(", ")
}
