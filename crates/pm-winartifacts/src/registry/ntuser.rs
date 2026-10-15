//! NTUSER.DAT analyses: Autopsy's `autopsyntusernetwork`, `officedocs`,
//! and the RegRipper full-profile MRU plugins it parses (`adobe`,
//! `mpmru`, `sevenzip`, `applets`, `mmc`, `winrar`, `msoffice`, `lxss`),
//! plus `UserAssist`, `RunMRU`, `TypedURLs`/`TypedPaths` and the default browser.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;
use pm_registry::{Hive, Key};

use super::{key, numbered_values, sub, subkeys, text};

/// A mapped network drive (`Network\<letter>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDrive {
    /// `Network\X` (as Autopsy's `TSK_LOCAL_PATH`).
    pub local_path: String,
    pub remote_path: String,
    pub last_written: Option<i64>,
}

/// Mapped network drives.
pub fn network_drives(h: &Hive) -> Vec<NetworkDrive> {
    let Some(n) = key(h, "Network") else {
        return Vec::new();
    };
    subkeys(&n)
        .into_iter()
        .filter_map(|s| {
            Some(NetworkDrive {
                local_path: format!("Network\\{}", s.name()),
                remote_path: s.string_value("RemotePath")?,
                last_written: s.last_written_unix(),
            })
        })
        .collect()
}

/// An item of the legacy Office MRU section Autopsy parses
/// (`officedocs`: Office ≤ 2007).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeLegacyItem {
    /// "Word", "Excel" or "`PowerPoint`" (the plugin's element name).
    pub program: &'static str,
    /// The value name (Word: the whole MRU value).
    pub name: String,
    /// The path (Word: the number of NUL-separated entries).
    pub value: String,
    /// `Software\Microsoft\Office\<ver>` last written.
    pub mtime: Option<i64>,
}

/// The `officedocs` plugin's output (Office 7.0–12.0).
pub fn office_legacy(h: &Hive) -> Vec<OfficeLegacyItem> {
    let mut version = None;
    for v in ["7.0", "8.0", "9.0", "10.0", "11.0", "12.0"] {
        if key(
            h,
            &format!("Software\\Microsoft\\Office\\{v}\\Common\\Open Find"),
        )
        .is_some()
        {
            version = Some(v);
        }
    }
    let Some(of) = version.and_then(|v| key(h, &format!("Software\\Microsoft\\Office\\{v}")))
    else {
        return Vec::new();
    };
    let mtime = of.last_written_unix();
    let mut out = Vec::new();
    for func in ["Open", "Save As", "File Save"] {
        let path =
            format!("Common\\Open Find\\Microsoft Office Word\\Settings\\{func}\\File Name MRU");
        if let Some(k) = sub(&of, &path)
            && let Some(v) = k.value("Value").ok().flatten()
        {
            let raw = v.data().unwrap_or_default();
            let units: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            let whole = String::from_utf16_lossy(&units);
            let parts = whole.split('\0').count();
            out.push(OfficeLegacyItem {
                program: "Word",
                name: whole.replace('\0', " ").trim().to_owned(),
                value: parts.to_string(),
                mtime,
            });
        }
    }
    for (program, path) in [
        ("Excel", "Excel\\Recent Files"),
        ("PowerPoint", "PowerPoint\\Recent File List"),
    ] {
        if let Some(k) = sub(&of, path) {
            for (name, data) in numbered_values(&k, "File") {
                out.push(OfficeLegacyItem {
                    program,
                    name,
                    value: data,
                    mtime,
                });
            }
        }
    }
    out
}

/// A recently used file from an application MRU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFile {
    pub path: String,
    /// Access time when the MRU records one (Unix seconds).
    pub accessed: Option<i64>,
    /// Autopsy's `TSK_COMMENT` for the source.
    pub comment: &'static str,
}

pub const ADOBE: &str = "Recently opened according to Adobe MRU";
pub const MEDIA_PLAYER: &str = "Recently opened according to Media Player MRU";
pub const SEVEN_ZIP: &str = "Recently opened by 7Zip";
pub const APPLETS: &str = "Recently opened according to Applets registry key";
pub const MMC: &str = "Recently opened according to Windows Management Console MRU";
pub const WINRAR: &str = "Recently opened according to WinRAR MRU";
pub const OFFICE: &str = "Recently opened according to Office MRU";
pub const OFFICE_TRUST: &str =
    "Stored in TrustRecords because Office security exception was granted";

/// All application MRUs Autopsy reads (in its order).
pub fn recent_files(h: &Hive) -> Vec<RecentFile> {
    let mut out = adobe(h);
    out.extend(simple_mru(
        h,
        "Software\\Microsoft\\MediaPlayer\\Player\\RecentFileList",
        "File",
        MEDIA_PLAYER,
    ));
    out.extend(seven_zip(h));
    out.extend(simple_mru(
        h,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Applets\\Paint\\Recent File List",
        "File",
        APPLETS,
    ));
    out.extend(simple_mru(
        h,
        "Software\\Microsoft\\Microsoft Management Console\\Recent File List",
        "File",
        MMC,
    ));
    out.extend(simple_mru(h, "Software\\WinRAR\\ArcHistory", "", WINRAR));
    out.extend(office_mru(h));
    out
}

fn simple_mru(h: &Hive, path: &str, prefix: &str, comment: &'static str) -> Vec<RecentFile> {
    key(h, path)
        .map(|k| {
            numbered_values(&k, prefix)
                .into_iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(_, p)| RecentFile {
                    path: p,
                    accessed: None,
                    comment,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Adobe Acrobat / Reader `cRecentFiles` (RegRipper `adobe`).
fn adobe(h: &Hive) -> Vec<RecentFile> {
    let mut out = Vec::new();
    for app in ["Adobe Acrobat", "Acrobat Reader"] {
        let Some(a) = key(h, &format!("Software\\Adobe\\{app}")) else {
            continue;
        };
        // The plugin keeps the last version that has the key.
        let Some(recent) = subkeys(&a)
            .into_iter()
            .filter_map(|v| sub(&v, "AVGeneral\\cRecentFiles"))
            .next_back()
        else {
            continue;
        };
        let mut items: Vec<(u64, RecentFile)> = subkeys(&recent)
            .into_iter()
            .filter_map(|c| {
                let n: u64 = c.name().trim_start_matches(['c', 'C']).parse().ok()?;
                let raw = c
                    .binary_value("sDI")
                    .or_else(|| c.binary_value("tDIText"))?;
                let mut path = if raw.get(1) == Some(&0) {
                    utf16le_lossy(&raw)
                } else {
                    pm_core::decode::cstr_lossy(&raw)
                };
                // "/C/Users/x.pdf" → "C:/Users/x.pdf".
                if let Some(rest) = path.strip_prefix('/')
                    && let Some((drive, tail)) = rest.split_once('/')
                    && drive.len() == 1
                {
                    path = format!("{drive}:/{tail}");
                }
                let accessed = c
                    .binary_value("sDate")
                    .map(|d| pm_core::decode::cstr_lossy(&d))
                    .or_else(|| c.string_value("sDate"))
                    .and_then(|s| adobe_date(&s));
                Some((
                    n,
                    RecentFile {
                        path,
                        accessed,
                        comment: ADOBE,
                    },
                ))
            })
            .collect();
        items.sort_by_key(|(n, _)| *n);
        out.extend(items.into_iter().map(|(_, r)| r));
    }
    out
}

/// `D:20200131104456-05'00'` → Unix seconds.
pub fn adobe_date(s: &str) -> Option<i64> {
    let s = s.trim().trim_start_matches("D:").replace('\'', "");
    let digits = s.get(..14)?;
    let dt = jiff::civil::DateTime::strptime("%Y%m%d%H%M%S", digits).ok()?;
    let zone = s.get(14..).unwrap_or_default();
    let offset_min: i64 = match zone.as_bytes().first() {
        Some(b'+' | b'-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let h: i64 = zone.get(1..3)?.parse().ok()?;
            let m: i64 = zone.get(3..5).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (h * 60 + m)
        }
        _ => 0,
    };
    let utc = pm_core::time::local_to_timestamp(dt, &jiff::tz::TimeZone::UTC)?.as_second();
    Some(utc - offset_min * 60)
}

/// 7-Zip `Compression\ArcHistory` (RegRipper `sevenzip`).
fn seven_zip(h: &Hive) -> Vec<RecentFile> {
    let mut out = Vec::new();
    for base in ["Software\\7-Zip", "Software\\Wow6432Node\\7-Zip"] {
        let Some(raw) =
            key(h, &format!("{base}\\Compression")).and_then(|k| k.binary_value("ArcHistory"))
        else {
            continue;
        };
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        for s in units.split(|&u| u == 0).filter(|s| !s.is_empty()) {
            out.push(RecentFile {
                path: String::from_utf16_lossy(s),
                accessed: None,
                comment: SEVEN_ZIP,
            });
        }
    }
    out
}

/// Office 2010+ MRUs and trust records (RegRipper `msoffice_tln`).
pub fn office_mru(h: &Hive) -> Vec<RecentFile> {
    let Some(office) = key(h, "Software\\Microsoft\\Office") else {
        return Vec::new();
    };
    // Highest numeric version with a "User Settings" subkey.
    let mut versions: Vec<(f64, Key<'_>)> = subkeys(&office)
        .into_iter()
        .filter_map(|k| k.name().parse::<f64>().ok().map(|v| (v, k)))
        .filter(|(_, k)| sub(k, "User Settings").is_some())
        .collect();
    versions.sort_by(|a, b| a.0.total_cmp(&b.0));
    let Some((_, ver)) = versions.pop() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for app in ["Word", "PowerPoint", "Excel", "Access"] {
        if let Some(trs) = sub(
            &ver,
            &format!("{app}\\Security\\Trusted Documents\\TrustRecords"),
        ) {
            for v in trs.values().unwrap_or_default() {
                let t = v
                    .data()
                    .ok()
                    .and_then(|d| d.u64_le(0).ok())
                    .and_then(crate::filetime_unix);
                out.push(RecentFile {
                    path: v.name(),
                    accessed: t,
                    comment: OFFICE_TRUST,
                });
            }
        }
        let mut lists: Vec<Key<'_>> = Vec::new();
        lists.extend(sub(&ver, &format!("{app}\\File MRU")));
        if let Some(um) = sub(&ver, &format!("{app}\\User MRU")) {
            lists.extend(subkeys(&um).iter().filter_map(|u| sub(u, "File MRU")));
        }
        lists.extend(sub(&ver, &format!("{app}\\Place MRU")));
        if let Some(um) = sub(&ver, &format!("{app}\\User MRU")) {
            lists.extend(subkeys(&um).iter().filter_map(|u| sub(u, "Place MRU")));
        }
        for l in lists {
            for (_, data) in numbered_values(&l, "Item ") {
                if let Some((t, file)) = office_mru_value(&data) {
                    out.push(RecentFile {
                        path: file,
                        accessed: t,
                        comment: OFFICE,
                    });
                }
            }
        }
    }
    out
}

/// `[F00000000][T01D0B7D3A1E8A2F0][O00000000]*C:\path` → (time, path).
pub fn office_mru_value(s: &str) -> Option<(Option<i64>, String)> {
    let (meta, file) = s.split_once('*')?;
    let t = meta
        .split(']')
        .find_map(|p| p.trim_start_matches('[').strip_prefix('T'))
        .and_then(|h| u64::from_str_radix(h, 16).ok())
        .and_then(crate::filetime_unix);
    Some((t, file.to_owned()))
}

/// A WSL distribution (`Lxss\{GUID}`, RegRipper `lxss`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    /// `BasePath` + `\ext4.vhdx` (Autopsy's path).
    pub image_path: String,
    pub kernel_command: String,
    pub last_written: Option<i64>,
}

/// WSL distributions.
pub fn wsl(h: &Hive) -> Vec<WslDistro> {
    let Some(l) = key(h, "Software\\Microsoft\\Windows\\CurrentVersion\\Lxss") else {
        return Vec::new();
    };
    subkeys(&l)
        .into_iter()
        .filter_map(|d| {
            let base = text(&d, "BasePath")?;
            Some(WslDistro {
                name: text(&d, "DistributionName").unwrap_or_default(),
                image_path: format!("{base}\\ext4.vhdx"),
                kernel_command: text(&d, "KernelCommandLine").unwrap_or_default(),
                last_written: d.last_written_unix(),
            })
        })
        .collect()
}

/// A `UserAssist` record (program launched through Explorer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAssist {
    /// ROT13-decoded name with known-folder GUIDs replaced.
    pub name: String,
    pub run_count: u32,
    pub focus_count: Option<u32>,
    pub focus_time_ms: Option<u32>,
    pub last_run: Option<i64>,
}

/// `UserAssist` entries with a run count or time.
pub fn user_assist(h: &Hive) -> Vec<UserAssist> {
    let Some(ua) = key(
        h,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist",
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for g in subkeys(&ua) {
        let Some(count) = sub(&g, "Count") else {
            continue;
        };
        for v in count.values().unwrap_or_default() {
            let name = resolve_known_folders(&rot13(&v.name()));
            let Ok(d) = v.data() else { continue };
            let entry = match d.len() {
                16 => {
                    let raw = d.u32_le(4).unwrap_or(0);
                    UserAssist {
                        name,
                        run_count: raw.saturating_sub(5),
                        focus_count: None,
                        focus_time_ms: None,
                        last_run: d.u64_le(8).ok().and_then(crate::filetime_unix),
                    }
                }
                72 => UserAssist {
                    name,
                    run_count: d.u32_le(4).unwrap_or(0),
                    focus_count: d.u32_le(8).ok(),
                    focus_time_ms: d.u32_le(12).ok(),
                    last_run: d.u64_le(60).ok().and_then(crate::filetime_unix),
                },
                _ => continue,
            };
            if entry.run_count > 0 || entry.last_run.is_some() {
                out.push(entry);
            }
        }
    }
    out
}

/// ROT13 on ASCII letters.
pub fn rot13(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => char::from((c as u8 - b'a' + 13) % 26 + b'a'),
            'A'..='Z' => char::from((c as u8 - b'A' + 13) % 26 + b'A'),
            _ => c,
        })
        .collect()
}

/// Replaces leading known-folder GUIDs (`{1AC14E77-…}\cmd.exe`).
pub fn resolve_known_folders(s: &str) -> String {
    const FOLDERS: &[(&str, &str)] = &[
        (
            "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}",
            "C:\\Windows\\System32",
        ),
        (
            "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}",
            "C:\\Windows\\SysWOW64",
        ),
        ("{F38BF404-1D43-42F2-9305-67DE0B28FC23}", "C:\\Windows"),
        (
            "{6D809377-6AF0-444B-8957-A3773F02200E}",
            "C:\\Program Files",
        ),
        (
            "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}",
            "C:\\Program Files (x86)",
        ),
        (
            "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}",
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs",
        ),
        (
            "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}",
            "%APPDATA%\\Microsoft\\Windows\\Start Menu\\Programs",
        ),
        (
            "{9E3995AB-1F9C-4F13-B827-48B24B6C7174}",
            "%APPDATA%\\Microsoft\\Internet Explorer\\Quick Launch\\User Pinned",
        ),
        (
            "{B4BFCC3A-DB2C-424C-B029-7FE99A87C641}",
            "%USERPROFILE%\\Desktop",
        ),
        (
            "{374DE290-123F-4565-9164-39C4925E467B}",
            "%USERPROFILE%\\Downloads",
        ),
        (
            "{FDD39AD0-238F-46AF-ADB4-6C85480369C7}",
            "%USERPROFILE%\\Documents",
        ),
    ];
    for (g, p) in FOLDERS {
        if let Some(rest) = s
            .get(..g.len())
            .filter(|h| h.eq_ignore_ascii_case(g))
            .map(|_| s.get(g.len()..).unwrap_or_default())
        {
            return format!("{p}{rest}");
        }
    }
    s.to_owned()
}

/// Run dialog history (`RunMRU`), most recent first.
pub fn run_mru(h: &Hive) -> Vec<String> {
    mru_list(
        h,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RunMRU",
    )
    .into_iter()
    .map(|s| s.trim_end_matches("\\1").to_owned())
    .collect()
}

/// Values of an `MRUList`-ordered key.
fn mru_list(h: &Hive, path: &str) -> Vec<String> {
    let Some(k) = key(h, path) else {
        return Vec::new();
    };
    let order = text(&k, "MRUList").unwrap_or_default();
    order
        .chars()
        .filter_map(|c| k.string_value(&c.to_string()))
        .collect()
}

/// URLs typed into Internet Explorer / Explorer (`TypedURLs`), with the
/// times from `TypedURLsTime` when present.
pub fn typed_urls(h: &Hive) -> Vec<(String, Option<i64>)> {
    let base = "Software\\Microsoft\\Internet Explorer";
    let Some(k) = key(h, &format!("{base}\\TypedURLs")) else {
        return Vec::new();
    };
    let times = key(h, &format!("{base}\\TypedURLsTime"));
    numbered_values(&k, "url")
        .into_iter()
        .map(|(name, url)| {
            let t = times
                .as_ref()
                .and_then(|t| t.binary_value(&name))
                .and_then(|d| d.u64_le(0).ok())
                .and_then(crate::filetime_unix);
            (url, t)
        })
        .collect()
}

/// Paths typed into the Explorer address bar (`TypedPaths`).
pub fn typed_paths(h: &Hive) -> Vec<String> {
    key(
        h,
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\TypedPaths",
    )
    .map(|k| {
        numbered_values(&k, "url")
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    })
    .unwrap_or_default()
}

/// The user's default browser: the `http` `UserChoice` `ProgId` (Windows
/// 8+), else the per-user `http` open command.
pub fn default_browser(h: &Hive) -> Option<String> {
    key(
        h,
        "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\http\\UserChoice",
    )
    .and_then(|k| text(&k, "ProgId"))
    .or_else(|| key(h, "Software\\Classes\\http\\shell\\open\\command").and_then(|k| text(&k, "")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            rot13("HRZR_EHACNGU:P:\\Jvaqbjf"),
            "UEME_RUNPATH:C:\\Windows"
        );
        assert_eq!(
            resolve_known_folders("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe"),
            "C:\\Windows\\System32\\cmd.exe"
        );
        assert_eq!(adobe_date("D:20200131104456-05'00'"), Some(1_580_485_496));
        assert_eq!(adobe_date("20200131104456Z"), Some(1_580_467_496));
        assert_eq!(adobe_date("bad"), None);
        let (t, f) =
            office_mru_value("[F00000000][T01D0B7D3A1E8A2F0][O00000000]*C:\\x.docx").unwrap();
        assert_eq!(f, "C:\\x.docx");
        assert!(t.is_some());
        assert!(office_mru_value("nostar").is_none());
    }
}
