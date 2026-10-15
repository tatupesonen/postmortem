//! SOFTWARE hive analyses.

use pm_core::Bytes;
use pm_registry::{Hive, Key};

use super::{key, sub, subkeys, systemtime, text};

const CURRENT_VERSION: &str = "Microsoft\\Windows NT\\CurrentVersion";

/// `Microsoft\Windows NT\CurrentVersion` (RegRipper `autopsywinver`, plus
/// edition/build fields Autopsy does not report).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsVersion {
    pub product_name: Option<String>,
    pub csd_version: Option<String>,
    /// `CurrentVersion` ("5.1", "6.3").
    pub current_version: Option<String>,
    pub current_build: Option<String>,
    /// Update build revision (Windows 10+).
    pub ubr: Option<u32>,
    pub edition_id: Option<String>,
    /// `DisplayVersion` (21H2…) or `ReleaseId` (1909…).
    pub display_version: Option<String>,
    pub installation_type: Option<String>,
    pub build_lab_ex: Option<String>,
    /// `InstallDate` (Unix seconds) or `InstallTime` (FILETIME).
    pub install_date: Option<i64>,
    pub registered_owner: Option<String>,
    pub registered_organization: Option<String>,
    pub system_root: Option<String>,
    pub product_id: Option<String>,
    /// Decoded from `DigitalProductId`.
    pub product_key: Option<String>,
}

impl WindowsVersion {
    /// Autopsy's OS name: `ProductName` + " " + `CSDVersion`.
    pub fn os_name(&self) -> Option<String> {
        let p = self.product_name.as_deref()?;
        Some(match &self.csd_version {
            Some(c) => format!("{p} {c}"),
            None => p.to_owned(),
        })
    }

    /// "10.0.19045.3803"-style build text.
    pub fn build(&self) -> Option<String> {
        let b = self.current_build.as_deref()?;
        Some(match self.ubr {
            Some(u) => format!("{b}.{u}"),
            None => b.to_owned(),
        })
    }
}

/// Reads [`WindowsVersion`].
pub fn windows_version(h: &Hive) -> WindowsVersion {
    let Some(k) = key(h, CURRENT_VERSION) else {
        return WindowsVersion::default();
    };
    let install_date = k
        .dword_value("InstallDate")
        .map(i64::from)
        .filter(|&d| d > 0)
        .or_else(|| {
            k.value("InstallTime")
                .ok()
                .flatten()
                .and_then(|v| v.qword())
                .and_then(crate::filetime_unix)
        });
    WindowsVersion {
        product_name: text(&k, "ProductName"),
        csd_version: text(&k, "CSDVersion"),
        current_version: text(&k, "CurrentVersion"),
        current_build: text(&k, "CurrentBuild").or_else(|| text(&k, "CurrentBuildNumber")),
        ubr: k.dword_value("UBR"),
        edition_id: text(&k, "EditionID"),
        display_version: text(&k, "DisplayVersion").or_else(|| text(&k, "ReleaseId")),
        installation_type: text(&k, "InstallationType"),
        build_lab_ex: text(&k, "BuildLabEx").or_else(|| text(&k, "BuildLab")),
        install_date,
        registered_owner: text(&k, "RegisteredOwner"),
        registered_organization: text(&k, "RegisteredOrganization"),
        system_root: text(&k, "SystemRoot"),
        product_id: text(&k, "ProductId"),
        product_key: k
            .binary_value("DigitalProductId")
            .and_then(|d| decode_product_key(&d)),
    }
}

/// Decodes the product key in a `DigitalProductId` (bytes 52–66,
/// base 24); Windows 8+ keys (flag bit in byte 66) get the "N" inserted
/// at the encoded position. Returns `None` for all-zero / generic data.
pub fn decode_product_key(dpid: &[u8]) -> Option<String> {
    const CHARS: &[u8] = b"BCDFGHJKMPQRTVWXY2346789";
    let mut key: Vec<u8> = dpid.get(52..67)?.to_vec();
    if key.iter().all(|&b| b == 0) {
        return None;
    }
    let win8 = (key.get(14).copied().unwrap_or(0) / 6) & 1;
    if let Some(b) = key.get_mut(14) {
        *b = (*b & 0xF7) | ((win8 & 2) * 4);
    }
    let mut out: Vec<u8> = Vec::with_capacity(25);
    let mut last = 0usize;
    for _ in 0..25 {
        let mut cur: u32 = 0;
        for b in key.iter_mut().rev() {
            cur = (cur << 8) | u32::from(*b);
            *b = (cur / 24) as u8;
            cur %= 24;
        }
        out.insert(0, *CHARS.get(cur as usize)?);
        last = cur as usize;
    }
    let mut text = String::from_utf8(out).ok()?;
    if win8 == 1 {
        let rest = text.get(1..)?;
        let (a, b) = rest.split_at(last.min(rest.len()));
        text = format!("{a}N{b}");
    }
    let chunks: Vec<&str> = (0..5).filter_map(|i| text.get(i * 5..i * 5 + 5)).collect();
    Some(chunks.join("-"))
}

/// An installed program (RegRipper `autopsyuninstall`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledProgram {
    /// `DisplayName` (else the key name) + " v." + `DisplayVersion`.
    pub name: String,
    /// Uninstall subkey last written (Unix seconds).
    pub last_written: Option<i64>,
    pub publisher: Option<String>,
    pub install_location: Option<String>,
    /// `InstallDate` text (`YYYYMMDD`).
    pub install_date: Option<String>,
}

/// Programs from both `Uninstall` keys (64- and 32-bit views).
pub fn installed_programs(h: &Hive) -> Vec<InstalledProgram> {
    let mut out = Vec::new();
    for path in [
        "Microsoft\\Windows\\CurrentVersion\\Uninstall",
        "Wow6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
    ] {
        let Some(k) = key(h, path) else { continue };
        for s in subkeys(&k) {
            let mut name = s
                .value("DisplayName")
                .ok()
                .flatten()
                .and_then(|v| v.string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| s.name());
            if let Some(v) = s.value("DisplayVersion").ok().flatten() {
                name = format!("{name} v.{}", v.string().unwrap_or_default());
            }
            out.push(InstalledProgram {
                name,
                last_written: s.last_written_unix(),
                publisher: text(&s, "Publisher"),
                install_location: text(&s, "InstallLocation"),
                install_date: text(&s, "InstallDate"),
            });
        }
    }
    // RegRipper lists newest first.
    out.sort_by_key(|p| std::cmp::Reverse(p.last_written));
    out
}

/// A user profile (RegRipper `autopsyprofilelist`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub sid: String,
    /// `ProfileImagePath`.
    pub path: Option<String>,
    /// Last path component up to the first '.' (the plugin's user name).
    pub user: String,
    pub last_written: Option<i64>,
}

/// Profiles from `ProfileList`.
pub fn profile_list(h: &Hive) -> Vec<Profile> {
    let Some(k) = key(h, &format!("{CURRENT_VERSION}\\ProfileList")) else {
        return Vec::new();
    };
    subkeys(&k)
        .into_iter()
        .map(|s| {
            let path = text(&s, "ProfileImagePath");
            let user = path
                .as_deref()
                .and_then(|p| p.rsplit('\\').next())
                .map(|u| u.split('.').next().unwrap_or_default().to_owned())
                .unwrap_or_default();
            Profile {
                sid: s.name(),
                path,
                user,
                last_written: s.last_written_unix(),
            }
        })
        .collect()
}

/// A network the machine joined (`NetworkList\Profiles`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkProfile {
    pub guid: String,
    pub name: String,
    pub description: Option<String>,
    /// 6 = wired, 23 = VPN, 71 = wireless, 243 = mobile broadband.
    pub name_type: Option<u32>,
    /// 0 public, 1 private, 2 domain.
    pub category: Option<u32>,
    pub managed: Option<bool>,
    /// Local time stored as `SYSTEMTIME`, read as UTC.
    pub first_connected: Option<i64>,
    pub last_connected: Option<i64>,
    pub dns_suffix: Option<String>,
    pub gateway_mac: Option<String>,
}

impl NetworkProfile {
    /// Human-readable network type.
    pub fn type_name(&self) -> &'static str {
        match self.name_type {
            Some(6) => "Wired",
            Some(23) => "VPN",
            Some(71) => "Wireless",
            Some(243) => "Mobile Broadband",
            _ => "Unknown",
        }
    }
}

/// Profiles joined with their signatures (gateway MAC, DNS suffix).
pub fn network_profiles(h: &Hive) -> Vec<NetworkProfile> {
    let base = format!("{CURRENT_VERSION}\\NetworkList");
    let Some(profiles) = key(h, &format!("{base}\\Profiles")) else {
        return Vec::new();
    };
    let mut sigs: Vec<(String, Option<String>, Option<String>, bool)> = Vec::new();
    for (kind, managed) in [("Managed", true), ("Unmanaged", false)] {
        if let Some(k) = key(h, &format!("{base}\\Signatures\\{kind}")) {
            for s in subkeys(&k) {
                let guid = text(&s, "ProfileGuid").unwrap_or_default();
                let mac = s
                    .binary_value("DefaultGatewayMac")
                    .filter(|m| m.len() == 6)
                    .map(|m| {
                        m.iter()
                            .map(|b| format!("{b:02X}"))
                            .collect::<Vec<_>>()
                            .join(":")
                    });
                sigs.push((guid, text(&s, "DnsSuffix"), mac, managed));
            }
        }
    }
    subkeys(&profiles)
        .into_iter()
        .map(|p| {
            let guid = p.name();
            let sig = sigs.iter().find(|s| s.0.eq_ignore_ascii_case(&guid));
            NetworkProfile {
                name: text(&p, "ProfileName").unwrap_or_default(),
                description: text(&p, "Description"),
                name_type: p.dword_value("NameType"),
                category: p.dword_value("Category"),
                managed: p
                    .dword_value("Managed")
                    .map(|v| v != 0)
                    .or(sig.map(|s| s.3)),
                first_connected: p.binary_value("DateCreated").and_then(|b| systemtime(&b)),
                last_connected: p
                    .binary_value("DateLastConnected")
                    .and_then(|b| systemtime(&b)),
                dns_suffix: sig.and_then(|s| s.1.clone()).filter(|s| s != "<none>"),
                gateway_mac: sig.and_then(|s| s.2.clone()),
                guid,
            }
        })
        .collect()
}

/// Logon-related settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogonInfo {
    /// `Authentication\LogonUI\LastLoggedOnUser` (Vista+), else the
    /// Winlogon `DefaultUserName` (XP shows the last user there).
    pub last_logged_on_user: Option<String>,
    pub last_logged_on_sam_user: Option<String>,
    pub last_logged_on_display_name: Option<String>,
    pub last_logged_on_sid: Option<String>,
    pub default_user_name: Option<String>,
    pub default_domain: Option<String>,
    pub auto_admin_logon: bool,
    /// A plaintext `DefaultPassword` stored for auto-logon.
    pub default_password: Option<String>,
}

/// Reads [`LogonInfo`].
pub fn logon_info(h: &Hive) -> LogonInfo {
    let mut l = LogonInfo::default();
    if let Some(k) = key(
        h,
        "Microsoft\\Windows\\CurrentVersion\\Authentication\\LogonUI",
    ) {
        l.last_logged_on_user = text(&k, "LastLoggedOnUser");
        l.last_logged_on_sam_user = text(&k, "LastLoggedOnSAMUser");
        l.last_logged_on_display_name = text(&k, "LastLoggedOnDisplayName");
        l.last_logged_on_sid = text(&k, "LastLoggedOnUserSID");
    }
    if let Some(k) = key(h, &format!("{CURRENT_VERSION}\\Winlogon")) {
        l.default_user_name = text(&k, "DefaultUserName");
        l.default_domain = text(&k, "DefaultDomainName");
        l.auto_admin_logon = text(&k, "AutoAdminLogon").is_some_and(|v| v == "1")
            || k.dword_value("AutoAdminLogon") == Some(1);
        l.default_password = k
            .value("DefaultPassword")
            .ok()
            .flatten()
            .and_then(|v| v.string());
    }
    if l.last_logged_on_user.is_none() {
        l.last_logged_on_user.clone_from(&l.default_user_name);
    }
    l
}

/// Microsoft Defender settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefenderInfo {
    /// `(kind, value)`: kind is `Paths`, `Extensions`, `Processes`, `IpAddresses`
    /// or `TemporaryPaths`; policy exclusions are included.
    pub exclusions: Vec<(String, String)>,
    pub anti_spyware_disabled: Option<bool>,
    pub realtime_monitoring_disabled: Option<bool>,
}

/// Reads [`DefenderInfo`].
pub fn defender(h: &Hive) -> DefenderInfo {
    let mut d = DefenderInfo::default();
    for root in [
        "Microsoft\\Windows Defender",
        "Policies\\Microsoft\\Windows Defender",
    ] {
        let Some(r) = key(h, root) else { continue };
        if let Some(v) = r.dword_value("DisableAntiSpyware") {
            d.anti_spyware_disabled = Some(v != 0);
        }
        if let Some(v) =
            sub(&r, "Real-Time Protection").and_then(|k| k.dword_value("DisableRealtimeMonitoring"))
        {
            d.realtime_monitoring_disabled = Some(v != 0);
        }
        if let Some(ex) = sub(&r, "Exclusions") {
            for kind in subkeys(&ex) {
                for v in kind.values().unwrap_or_default() {
                    let name = v.name();
                    if !name.is_empty() {
                        d.exclusions.push((kind.name(), name));
                    }
                }
            }
        }
    }
    d.exclusions.sort();
    d.exclusions.dedup();
    d
}

/// The machine-wide default web browser command
/// (`Classes\http\shell\open\command`).
pub fn default_browser(h: &Hive) -> Option<String> {
    key(h, "Classes\\http\\shell\\open\\command").and_then(|k| text(&k, ""))
}

/// Returns the key for `path` when present (re-exported for callers that
/// need ad-hoc lookups).
pub fn lookup<'h>(h: &'h Hive, path: &str) -> Option<Key<'h>> {
    key(h, path)
}

/// Reads a `SYSTEMTIME`, as used by [`network_profiles`].
pub fn read_systemtime(b: &[u8]) -> Option<i64> {
    if b.len() < 16 || b.u16_le(0).ok()? == 0 {
        return None;
    }
    systemtime(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_keys() {
        // Generic Windows 10 Pro key (publicly documented KMS client key).
        let mut d = vec![0u8; 164];
        // Encode VK7JG-NPHTM-C97JM-9MPGT-3V66T the inverse way.
        let key = b"VK7JGNPHTMC97JM9MPGT3V66T";
        let chars = b"BCDFGHJKMPQRTVWXY2346789";
        // N position 4 → win8 encoding: digits without the N.
        let without_n: Vec<u8> = key.iter().copied().filter(|&c| c != b'N').collect();
        let pos = key.iter().position(|&c| c == b'N').unwrap();
        let mut digits: Vec<u32> = vec![pos as u32];
        digits.extend(
            without_n
                .iter()
                .map(|c| chars.iter().position(|x| x == c).unwrap() as u32),
        );
        let mut num = [0u8; 15];
        for dgt in &digits {
            // num = num * 24 + dgt
            let mut carry = *dgt;
            for b in &mut num {
                let v = u32::from(*b) * 24 + carry;
                *b = (v & 0xFF) as u8;
                carry = v >> 8;
            }
        }
        num[14] |= 0x08;
        d[52..67].copy_from_slice(&num);
        assert_eq!(
            decode_product_key(&d).as_deref(),
            Some("VK7JG-NPHTM-C97JM-9MPGT-3V66T")
        );
        assert_eq!(decode_product_key(&[0u8; 100]), None);
        assert_eq!(decode_product_key(&[1u8; 10]), None);
    }
}
