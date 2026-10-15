//! SAM hive: local user accounts and group membership (RegRipper
//! `samparse`, read natively; account SIDs are built from the machine SID
//! in `SAM\Domains\Account\V`).

use std::fmt::Write as _;

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;
use pm_registry::Hive;

use super::{key, sub, subkeys};

/// `F`-value account-control flags and their RegRipper names.
pub const ACB_FLAGS: &[(u16, &str)] = &[
    (0x0001, "Account Disabled"),
    (0x0002, "Home directory required"),
    (0x0004, "Password not required"),
    (0x0008, "Temporary duplicate account"),
    (0x0010, "Normal user account"),
    (0x0020, "MNS logon user account"),
    (0x0040, "Interdomain trust account"),
    (0x0080, "Workstation trust account"),
    (0x0100, "Server trust account"),
    (0x0200, "Password does not expire"),
    (0x0400, "Account auto locked"),
];

/// A local account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SamUser {
    pub rid: u32,
    /// Full SID when the machine SID is known.
    pub sid: Option<String>,
    pub name: String,
    pub full_name: Option<String>,
    pub comment: Option<String>,
    /// "Default Admin User", "Custom Limited Acct", "Default Guest Acct".
    pub account_type: Option<&'static str>,
    /// `Names\<user>` key last written (Unix seconds).
    pub created: Option<i64>,
    pub last_login: Option<i64>,
    pub password_reset: Option<i64>,
    pub password_fail: Option<i64>,
    pub account_expires: Option<i64>,
    pub login_count: u16,
    pub failed_count: u16,
    pub acb_flags: u16,
    /// Microsoft-account e-mail (`InternetUserName`).
    pub internet_name: Option<String>,
    pub password_hint: Option<String>,
    /// `GivenName` + " " + `SurName`.
    pub given_surname: Option<String>,
    /// Security questions and answers (`ResetData`, Windows 10).
    pub security_questions: Vec<(String, String)>,
}

impl SamUser {
    /// Names of the set ACB flags.
    pub fn flag_names(&self) -> Vec<&'static str> {
        ACB_FLAGS
            .iter()
            .filter(|(bit, _)| self.acb_flags & bit != 0)
            .map(|(_, n)| *n)
            .collect()
    }

    /// Whether the account type marks an administrator.
    pub fn is_admin(&self) -> bool {
        self.account_type
            .is_some_and(|t| t.to_lowercase().contains("admin"))
    }
}

/// A built-in alias (group).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SamGroup {
    pub name: String,
    pub comment: Option<String>,
    pub members: Vec<String>,
    pub last_written: Option<i64>,
}

/// Everything read from a SAM hive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SamInfo {
    /// `S-1-5-21-a-b-c`.
    pub machine_sid: Option<String>,
    pub users: Vec<SamUser>,
    pub groups: Vec<SamGroup>,
}

impl SamInfo {
    /// Groups a SID belongs to.
    pub fn groups_of(&self, sid: &str) -> Vec<&str> {
        self.groups
            .iter()
            .filter(|g| g.members.iter().any(|m| m.eq_ignore_ascii_case(sid)))
            .map(|g| g.name.as_str())
            .collect()
    }
}

/// Reads users and groups.
pub fn sam(h: &Hive) -> SamInfo {
    let mut info = SamInfo {
        machine_sid: key(h, "SAM\\Domains\\Account")
            .and_then(|k| k.binary_value("V"))
            .and_then(|v| machine_sid(&v)),
        ..SamInfo::default()
    };
    if let Some(users) = key(h, "SAM\\Domains\\Account\\Users") {
        let names = sub(&users, "Names");
        for u in subkeys(&users) {
            let kname = u.name();
            if !kname.starts_with("0000") {
                continue;
            }
            let Ok(rid) = u32::from_str_radix(&kname, 16) else {
                continue;
            };
            let Some(v) = u.binary_value("V") else {
                continue;
            };
            let mut user = parse_v(&v);
            user.rid = rid;
            user.sid = info.machine_sid.as_ref().map(|m| format!("{m}-{rid}"));
            if let Some(f) = u.binary_value("F") {
                apply_f(&mut user, &f);
            }
            user.created = names
                .as_ref()
                .and_then(|n| n.subkey(&user.name).ok().flatten())
                .and_then(|k| k.last_written_unix());
            let wide = |n: &str| {
                u.binary_value(n)
                    .map(|d| utf16le_lossy(&d))
                    .filter(|s| !s.is_empty())
            };
            user.internet_name = wide("InternetUserName");
            user.password_hint = wide("UserPasswordHint");
            let given = wide("GivenName").unwrap_or_default();
            let surname = wide("SurName").unwrap_or_default();
            if !given.is_empty() || !surname.is_empty() {
                user.given_surname = Some(format!("{given} {surname}").trim().to_owned());
            }
            if let Some(reset) = u.binary_value("ResetData") {
                user.security_questions = reset_data(&reset);
            }
            info.users.push(user);
        }
    }
    if let Some(aliases) = key(h, "SAM\\Domains\\Builtin\\Aliases") {
        for g in subkeys(&aliases)
            .into_iter()
            .filter(|k| k.name().starts_with("0000"))
        {
            if let Some(c) = g.binary_value("C") {
                let mut grp = parse_c(&c);
                grp.last_written = g.last_written_unix();
                info.groups.push(grp);
            }
        }
    }
    info
}

/// The machine SID from the last 12 bytes of `Account\V`.
#[allow(clippy::many_single_char_names, reason = "SID sub-authorities")]
fn machine_sid(v: &[u8]) -> Option<String> {
    let n = v.len();
    let tail = v.get(n.checked_sub(12)?..)?;
    let a = tail.u32_le(0).ok()?;
    let b = tail.u32_le(4).ok()?;
    let c = tail.u32_le(8).ok()?;
    Some(format!("S-1-5-21-{a}-{b}-{c}"))
}

/// The `V` value: account type, name, full name, comment.
fn parse_v(v: &[u8]) -> SamUser {
    let field = |i: usize| -> Option<String> {
        let off = v.u32_le(i * 4).ok()? as usize;
        let len = v.u32_le((i + 1) * 4).ok()? as usize;
        if len == 0 {
            return None;
        }
        let s = utf16le_lossy(v.get(off + 0xCC..off + 0xCC + len)?);
        Some(s).filter(|s| !s.is_empty())
    };
    let account_type = match v.u32_le(4).unwrap_or(0) {
        0xBC => Some("Default Admin User"),
        0xD4 => Some("Custom Limited Acct"),
        0xB0 => Some("Default Guest Acct"),
        _ => None,
    };
    SamUser {
        name: field(3).unwrap_or_default(),
        full_name: field(6),
        comment: field(9),
        account_type,
        ..SamUser::default()
    }
}

/// The `F` value: times, counters and flags.
#[allow(clippy::many_single_char_names, reason = "short names for raw buffers")]
fn apply_f(u: &mut SamUser, f: &[u8]) {
    let t = |at: usize| f.u64_le(at).ok().and_then(crate::filetime_unix);
    u.last_login = t(8);
    u.password_reset = t(24);
    u.account_expires = f
        .u64_le(32)
        .ok()
        .filter(|&x| x != 0x7FFF_FFFF_FFFF_FFFF)
        .and_then(crate::filetime_unix);
    u.password_fail = t(40);
    u.acb_flags = f.u16_le(56).unwrap_or(0);
    u.failed_count = f.u16_le(64).unwrap_or(0);
    u.login_count = f.u16_le(66).unwrap_or(0);
}

/// Security questions from the `ResetData` JSON.
fn reset_data(raw: &[u8]) -> Vec<(String, String)> {
    let text = if raw.get(1) == Some(&0) {
        utf16le_lossy(raw)
    } else {
        String::from_utf8_lossy(raw)
            .trim_end_matches('\0')
            .to_owned()
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    v.get("questions")
        .and_then(|q| q.as_array())
        .map(|qs| {
            qs.iter()
                .map(|q| {
                    let s = |k: &str| {
                        q.get(k)
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_owned()
                    };
                    (s("question"), s("answer"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A built-in alias `C` value: name, comment and member SIDs.
fn parse_c(c: &[u8]) -> SamGroup {
    let field = |off_i: usize, len_i: usize| -> Option<String> {
        let off = c.u32_le(off_i * 4).ok()? as usize;
        let len = c.u32_le(len_i * 4).ok()? as usize;
        Some(utf16le_lossy(c.get(0x34 + off..0x34 + off + len)?)).filter(|s| !s.is_empty())
    };
    let mut g = SamGroup {
        name: field(4, 5).unwrap_or_default(),
        comment: field(7, 8),
        ..SamGroup::default()
    };
    let count = c.u32_le(48).unwrap_or(0).min(10_000) as usize;
    let mut at = 0x34 + c.u32_le(40).unwrap_or(0) as usize;
    for _ in 0..count {
        let Some(sid) = c.get(at..).and_then(sid_string) else {
            break;
        };
        let subs = usize::from(c.get(at + 1).copied().unwrap_or(0));
        g.members.push(sid);
        at += 8 + subs * 4;
    }
    g
}

/// A binary SID as `S-R-A-…`.
pub fn sid_string(b: &[u8]) -> Option<String> {
    let rev = *b.first()?;
    let subs = usize::from(*b.get(1)?);
    if rev != 1 || subs > 15 {
        return None;
    }
    let auth = b
        .get(2..8)?
        .iter()
        .fold(0u64, |acc, &x| (acc << 8) | u64::from(x));
    let mut s = format!("S-{rev}-{auth}");
    for i in 0..subs {
        let _ = write!(s, "-{}", b.u32_le(8 + i * 4).ok()?);
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sids() {
        let mut b = vec![1u8, 2, 0, 0, 0, 0, 0, 5];
        b.extend(32u32.to_le_bytes());
        b.extend(544u32.to_le_bytes());
        assert_eq!(sid_string(&b).as_deref(), Some("S-1-5-32-544"));
        assert_eq!(sid_string(&[2, 0]), None);
        let mut v = vec![0u8; 20];
        v.extend(1u32.to_le_bytes());
        v.extend(2u32.to_le_bytes());
        v.extend(3u32.to_le_bytes());
        assert_eq!(machine_sid(&v).as_deref(), Some("S-1-5-21-1-2-3"));
    }

    #[test]
    fn questions() {
        let j = r#"{"version":1,"questions":[{"question":"First pet?","answer":"Rex"}]}"#;
        let u: Vec<u8> = j.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(
            reset_data(&u),
            vec![("First pet?".to_owned(), "Rex".to_owned())]
        );
        assert!(reset_data(b"not json").is_empty());
    }
}
