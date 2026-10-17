//! Windows security identifier rules (TSK `WindowsAccountUtils`).

/// Windows uses this SID when the real one is unknown; it is treated as
/// no SID at all.
pub const NULL_SID: &str = "S-1-0-0";

const BACKUP_POSTFIX: &str = ".bak";
const NT_AUTHORITY_PREFIX: &str = "S-1-5";
const NT_AUTHORITY: &str = "NT AUTHORITY";

/// Group (not user) SIDs.
const GROUP_SIDS: &[&str] = &[
    "S-1-1-0",    // Everyone
    "S-1-2-0",    // Local
    "S-1-2-1",    // Console Logon
    "S-1-3-1",    // Creator
    "S-1-3-4",    // Owner rights
    "S-1-5-1",    // Dialup
    "S-1-5-2",    // Network
    "S-1-5-3",    // Batch
    "S-1-5-4",    // Interactive
    "S-1-5-6",    // Service
    "S-1-5-7",    // Anonymous
    "S-1-5-9",    // Enterprise Domain Controllers
    "S-1-5-11",   // Authenticated Users
    "S-1-5-12",   // Restricted Code
    "S-1-5-13",   // Terminal Server Users
    "S-1-5-14",   // Remote Interactive Logon
    "S-1-5-15",   // This Organization
    "S-1-5-80-0", // All Services
    "S-1-5-83-0", // NT Virtual Machine\Virtual Machines
    "S-1-5-90-0", // Windows Manager\Windows Manager Group
];

/// Prefixes of group SIDs (Builtin, task ids).
const GROUP_SID_PREFIXES: &[&str] = &["S-1-5-32", "S-1-5-87"];

/// Relative ids of domain groups (Domain Admins, Domain Users, …).
const DOMAIN_GROUP_SUFFIXES: &[&str] = &[
    "-512", "-513", "-514", "-515", "-516", "-517", "-518", "-519", "-520", "-526", "-527", "-533",
    "-498", "-521", "-571", "-572", "-522",
];

/// A well-known account or realm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WellKnown {
    pub is_user: bool,
    pub realm_addr: &'static str,
    pub realm_name: &'static str,
    pub login_name: &'static str,
    pub description: &'static str,
}

const fn wk(
    is_user: bool,
    realm_addr: &'static str,
    realm_name: &'static str,
    login_name: &'static str,
    description: &'static str,
) -> WellKnown {
    WellKnown {
        is_user,
        realm_addr,
        realm_name,
        login_name,
        description,
    }
}

const SPECIAL_SIDS: &[(&str, WellKnown)] = &[
    (
        "S-1-5-17",
        wk(true, "S-1-5", NT_AUTHORITY, "IUSR", "IIS Default Account"),
    ),
    (
        "S-1-5-18",
        wk(
            true,
            "S-1-5",
            NT_AUTHORITY,
            "SYSTEM",
            "Local System Account",
        ),
    ),
    (
        "S-1-5-19",
        wk(
            true,
            "S-1-5",
            NT_AUTHORITY,
            "LOCAL SERVICE",
            "Local Service Account",
        ),
    ),
    (
        "S-1-5-20",
        wk(
            true,
            "S-1-5",
            NT_AUTHORITY,
            "NETWORK SERVICE",
            "Network Service Account",
        ),
    ),
];

const SPECIAL_SID_PREFIXES: &[(&str, WellKnown)] = &[
    (
        "S-1-5-80",
        wk(
            false,
            "S-1-5-80",
            "NT SERVICE",
            "",
            "NT Service Virtual Account",
        ),
    ),
    (
        "S-1-5-82",
        wk(
            false,
            "S-1-5-82",
            "IIS APPPOOL",
            "",
            "IIS AppPool Virtual Account",
        ),
    ),
    (
        "S-1-5-83",
        wk(
            false,
            "S-1-5-83",
            "NT VIRTUAL MACHINE",
            "",
            "Virtual Machine Virtual Account",
        ),
    ),
    (
        "S-1-5-90",
        wk(
            false,
            "S-1-5-90",
            "Window Manager",
            "",
            "Windows Manager Virtual Account",
        ),
    ),
    (
        "S-1-5-94",
        wk(
            false,
            "S-1-5-94",
            "WinRM Virtual Users",
            "",
            "Windows Remoting Virtual Account",
        ),
    ),
    (
        "S-1-5-96",
        wk(
            false,
            "S-1-5-96",
            "Font Driver Host",
            "",
            "Font Driver Host Virtual Account",
        ),
    ),
];

/// (realm, login) → SID of well-known accounts.
const SPECIAL_ACCOUNTS: &[(&str, &str, &str)] = &[
    (NT_AUTHORITY, "SYSTEM", "S-1-5-18"),
    (NT_AUTHORITY, "LOCAL SERVICE", "S-1-5-19"),
    (NT_AUTHORITY, "NETWORK SERVICE", "S-1-5-20"),
];

/// Localised well-known realm names → English (upper case keys).
const REALM_NAMES: &[(&str, &str)] = &[
    ("NT AUTHORITY", NT_AUTHORITY),
    ("NT-AUTORITÄT", NT_AUTHORITY),
    ("AUTORITE NT", NT_AUTHORITY),
    ("NT INSTANS", NT_AUTHORITY),
];

/// Localised well-known login names → English (upper case keys).
const LOGIN_NAMES: &[(&str, &str)] = &[
    ("SYSTEM", "SYSTEM"),
    ("SYSTÈME", "SYSTEM"),
    ("LOCAL SERVICE", "LOCAL SERVICE"),
    ("LOKALER DIENST", "LOCAL SERVICE"),
    ("SERVICE LOCAL", "LOCAL SERVICE"),
    ("SERVIZIO LOCALE", "LOCAL SERVICE"),
    ("SERVICIO LOC", "LOCAL SERVICE"),
    ("NETWORK SERVICE", "NETWORK SERVICE"),
    ("NETZWERKDIENST", "NETWORK SERVICE"),
    ("NÄTVERKSTJÄNST", "NETWORK SERVICE"),
    ("SERVICE RÉSEAU", "NETWORK SERVICE"),
    ("SERVIZIO DI RETE", "NETWORK SERVICE"),
    ("SERVICIO DE RED", "NETWORK SERVICE"),
];

/// A usable SID: not blank and not the NULL SID.
pub fn real_sid(sid: Option<&str>) -> Option<&str> {
    sid.filter(|s| !s.trim().is_empty() && !s.eq_ignore_ascii_case(NULL_SID))
}

/// Non-blank text.
pub(crate) fn non_blank(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.trim().is_empty())
}

fn strip_backup(sid: &str) -> String {
    // Java `String.replace` removes every occurrence.
    if sid.ends_with(BACKUP_POSTFIX) {
        sid.replace(BACKUP_POSTFIX, "")
    } else {
        sid.to_owned()
    }
}

/// The NN of `S-1-5-NN-…` (TSK's special-account prefix regex).
fn nt_sub_authority(sid: &str) -> Option<u32> {
    let rest = sid
        .strip_prefix("S-1-5-")
        .or_else(|| sid.strip_prefix("s-1-5-"))?;
    let (digits, tail) = rest.split_at(rest.find(|c: char| !c.is_ascii_digit())?);
    if digits.is_empty() || !tail.starts_with('-') {
        return None;
    }
    digits.parse().ok()
}

fn well_known_info(sid: &str) -> Option<WellKnown> {
    let sid = strip_backup(sid);
    SPECIAL_SIDS
        .iter()
        .find(|(s, _)| *s == sid)
        .or_else(|| {
            SPECIAL_SID_PREFIXES
                .iter()
                .find(|(p, _)| sid.starts_with(p))
        })
        .map(|(_, w)| *w)
}

/// A well-known SID (SYSTEM, LOCAL/NETWORK SERVICE, IUSR, service and
/// virtual accounts in `S-1-5-80` … `S-1-5-111`).
pub fn is_well_known_sid(sid: &str) -> bool {
    well_known_info(sid).is_some()
        || nt_sub_authority(&strip_backup(sid)).is_some_and(|n| (80..=111).contains(&n))
}

/// A user (not group) SID.
pub fn is_user_sid(sid: &str) -> bool {
    let sid = strip_backup(sid);
    if GROUP_SIDS.contains(&sid.as_str()) || GROUP_SID_PREFIXES.iter().any(|p| sid.starts_with(p)) {
        return false;
    }
    !(sid.starts_with(NT_AUTHORITY_PREFIX)
        && DOMAIN_GROUP_SUFFIXES.iter().any(|s| sid.ends_with(s)))
}

/// The realm (domain or machine) address of a SID: the SID without its
/// last relative id, or the well-known realm address. `None` for SIDs with
/// fewer than four components.
pub fn realm_address(sid: &str) -> Option<String> {
    let stripped = strip_backup(sid);
    if is_well_known_sid(&stripped) {
        if let Some(w) = well_known_info(sid) {
            return Some(w.realm_addr.to_owned());
        }
        return nt_sub_authority(&stripped).map(|n| format!("{NT_AUTHORITY_PREFIX}-{n}"));
    }
    if stripped.matches('-').count() < 3 {
        return None;
    }
    // TSK cuts the original SID at the stripped SID's last '-'.
    let cut = stripped.rfind('-')?;
    sid.get(..cut).map(str::to_owned)
}

/// Realm name of a well-known SID (`NT AUTHORITY` for `S-1-5` itself).
pub fn well_known_realm_name(sid: &str) -> Option<&'static str> {
    if sid == NT_AUTHORITY_PREFIX {
        return Some(NT_AUTHORITY);
    }
    well_known_info(sid).map(|w| w.realm_name)
}

/// Login name of a well-known user SID (empty for virtual-account
/// prefixes).
pub fn well_known_login_name(sid: &str) -> Option<&'static str> {
    well_known_info(sid).map(|w| w.login_name)
}

/// Description of a well-known SID ("Local System Account", …).
pub fn well_known_full_name(sid: &str) -> Option<&'static str> {
    well_known_info(sid).map(|w| w.description)
}

/// English name of a localised well-known realm name; other names are
/// returned unchanged. `None` for blank input.
pub fn english_realm_name(name: Option<&str>) -> Option<String> {
    let name = non_blank(name)?;
    let upper = name.to_uppercase();
    Some(
        REALM_NAMES
            .iter()
            .find(|(k, _)| *k == upper)
            .map_or_else(|| name.to_owned(), |(_, v)| (*v).to_owned()),
    )
}

/// English name of a localised well-known login name; other names are
/// returned unchanged. `None` for blank input.
pub fn english_login_name(name: Option<&str>) -> Option<String> {
    let name = non_blank(name)?;
    let upper = name.to_uppercase();
    Some(
        LOGIN_NAMES
            .iter()
            .find(|(k, _)| *k == upper)
            .map_or_else(|| name.to_owned(), |(_, v)| (*v).to_owned()),
    )
}

/// SID of a well-known account given by realm and login name
/// (`NT AUTHORITY\SYSTEM` → `S-1-5-18`), in any supported language.
pub fn well_known_account_sid(login: Option<&str>, realm: Option<&str>) -> Option<&'static str> {
    let realm = english_realm_name(realm)?.to_uppercase();
    let login = english_login_name(login)?.to_uppercase();
    SPECIAL_ACCOUNTS
        .iter()
        .find(|(r, l, _)| *r == realm && *l == login)
        .map(|(_, _, sid)| *sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_and_group_sids() {
        assert!(is_user_sid("S-1-5-21-1-2-3-1001"));
        assert!(is_user_sid("S-1-5-18"));
        assert!(!is_user_sid("S-1-1-0"));
        assert!(!is_user_sid("S-1-5-32-544"), "builtin administrators");
        assert!(!is_user_sid("S-1-5-21-1-2-3-512"), "domain admins");
        assert!(!is_user_sid("S-1-5-87-123"));
    }

    #[test]
    fn realm_addresses() {
        assert_eq!(
            realm_address("S-1-5-21-111-222-333-1001").as_deref(),
            Some("S-1-5-21-111-222-333")
        );
        assert_eq!(realm_address("S-1-5-18").as_deref(), Some("S-1-5"));
        assert_eq!(realm_address("S-1-5-80-12-34").as_deref(), Some("S-1-5-80"));
        assert_eq!(
            realm_address("S-1-5-99-1-2").as_deref(),
            Some("S-1-5-99"),
            "80..=111 range"
        );
        assert_eq!(realm_address("S-1-5"), None, "too few components");
        assert_eq!(
            realm_address("S-1-5-21-1-2-3-500.bak").as_deref(),
            Some("S-1-5-21-1-2-3"),
            "backup postfix ignored"
        );
    }

    #[test]
    fn well_known_names_and_translations() {
        assert!(is_well_known_sid("S-1-5-19"));
        assert!(!is_well_known_sid("S-1-5-21-1-2-3-500"));
        assert_eq!(well_known_realm_name("S-1-5-18"), Some("NT AUTHORITY"));
        assert_eq!(well_known_login_name("S-1-5-20"), Some("NETWORK SERVICE"));
        assert_eq!(
            well_known_full_name("S-1-5-18"),
            Some("Local System Account")
        );
        assert_eq!(
            english_login_name(Some("Système")).as_deref(),
            Some("SYSTEM")
        );
        assert_eq!(
            english_realm_name(Some("NT-Autorität")).as_deref(),
            Some("NT AUTHORITY")
        );
        assert_eq!(english_login_name(Some("alice")).as_deref(), Some("alice"));
        assert_eq!(
            well_known_account_sid(Some("Lokaler Dienst"), Some("NT-AUTORITÄT")),
            Some("S-1-5-19")
        );
        assert_eq!(
            well_known_account_sid(Some("bob"), Some("NT AUTHORITY")),
            None
        );
        assert_eq!(real_sid(Some(NULL_SID)), None);
        assert_eq!(real_sid(Some(" ")), None);
    }
}
