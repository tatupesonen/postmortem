//! SYSTEM hive analyses.

use pm_core::Bytes;
use pm_registry::{Hive, Key};

use super::{filetime_value, key, sub, subkeys, text, text_any};

/// The current control set (`Select\Current`, default `ControlSet001`).
pub fn current_control_set(h: &Hive) -> Option<Key<'_>> {
    let n = key(h, "Select")
        .and_then(|s| s.dword_value("Current"))
        .unwrap_or(1);
    key(h, &format!("ControlSet{n:03}")).or_else(|| key(h, "ControlSet001"))
}

/// Time-zone settings (`Control\TimeZoneInformation`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimeZoneInfo {
    /// `TimeZoneKeyName` (Vista+), e.g. "Central Standard Time".
    pub key_name: Option<String>,
    pub standard_name: Option<String>,
    pub daylight_name: Option<String>,
    /// Minutes to add to local time to get UTC.
    pub bias: Option<i32>,
    pub active_bias: Option<i32>,
    pub standard_bias: Option<i32>,
    pub daylight_bias: Option<i32>,
    /// "month M, week W, day D, hh:mm" rule text.
    pub standard_start: Option<String>,
    pub daylight_start: Option<String>,
    pub dynamic_dst_disabled: Option<bool>,
}

impl TimeZoneInfo {
    /// The best display name (key name, else standard name).
    pub fn name(&self) -> Option<&str> {
        self.key_name
            .as_deref()
            .filter(|s| !s.starts_with('@'))
            .or(self.standard_name.as_deref())
    }

    /// UTC offset in minutes (`-bias`), e.g. `-360` for UTC−6.
    pub fn utc_offset_minutes(&self) -> Option<i32> {
        self.bias.map(|b| -b)
    }

    /// The IANA zone of the Windows zone (CLDR mapping), e.g.
    /// "America/Chicago" for "Central Standard Time".
    pub fn iana(&self) -> Option<&'static str> {
        pm_core::windows_zones::iana_zone(self.name()?)
    }
}

/// A Windows upgrade recorded under `Setup\Source OS (Updated on …)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Upgrade {
    pub key_name: String,
    pub product_name: Option<String>,
    pub build: Option<String>,
    pub display_version: Option<String>,
    /// Install date of the replaced OS (Unix seconds).
    pub install_date: Option<i64>,
}

/// Machine settings from the SYSTEM hive (Autopsy `autopsycompname`,
/// `autopsyprofiler`; the rest goes beyond Autopsy).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemInfo {
    pub control_set: Option<String>,
    pub computer_name: Option<String>,
    pub hostname: Option<String>,
    /// Tcpip `Domain` (what Autopsy reports).
    pub domain: Option<String>,
    /// Tcpip `NV Domain`.
    pub nv_domain: Option<String>,
    /// `Environment\OS` (e.g. `Windows_NT`).
    pub os_env: Option<String>,
    pub processor_architecture: Option<String>,
    pub processor_identifier: Option<String>,
    pub number_of_processors: Option<String>,
    pub temp_dir: Option<String>,
    pub time_zone: Option<TimeZoneInfo>,
    /// `Control\Windows\ShutdownTime` (Unix seconds).
    pub last_shutdown: Option<i64>,
    pub page_files: Vec<String>,
    pub clear_page_file_at_shutdown: Option<bool>,
    pub hibernate_enabled: Option<bool>,
    /// `CrashControl\CrashDumpEnabled` (0 none, 1 complete, 2 kernel,
    /// 3 small, 7 automatic).
    pub crash_dump_enabled: Option<u32>,
    pub crash_dump_file: Option<String>,
    /// `Control\BitLockerStatus\BootStatus`.
    pub bitlocker_boot_status: Option<u32>,
    pub upgrades: Vec<Upgrade>,
}

/// Reads [`SystemInfo`].
pub fn system_info(h: &Hive) -> SystemInfo {
    let mut s = SystemInfo::default();
    let Some(cs) = current_control_set(h) else {
        return s;
    };
    s.control_set = Some(cs.name());
    if let Some(k) = sub(&cs, "Control\\ComputerName\\ComputerName") {
        s.computer_name = text(&k, "ComputerName");
    }
    if let Some(k) = sub(&cs, "Services\\Tcpip\\Parameters") {
        s.hostname = text(&k, "Hostname");
        s.domain = text(&k, "Domain");
        s.nv_domain = text(&k, "NV Domain");
    }
    if let Some(k) = sub(&cs, "Control\\Session Manager\\Environment") {
        s.os_env = text(&k, "OS");
        s.processor_architecture = text(&k, "PROCESSOR_ARCHITECTURE");
        s.processor_identifier = text(&k, "PROCESSOR_IDENTIFIER");
        s.number_of_processors = text(&k, "NUMBER_OF_PROCESSORS");
        s.temp_dir = text(&k, "TEMP");
    }
    if let Some(k) = sub(&cs, "Control\\TimeZoneInformation") {
        let i32v = |n: &str| k.dword_value(n).map(|v| v as i32);
        let rule = |n: &str| k.binary_value(n).and_then(|b| tz_rule(&b));
        s.time_zone = Some(TimeZoneInfo {
            key_name: text_any(&k, "TimeZoneKeyName"),
            standard_name: text(&k, "StandardName"),
            daylight_name: text(&k, "DaylightName"),
            bias: i32v("Bias"),
            active_bias: i32v("ActiveTimeBias"),
            standard_bias: i32v("StandardBias"),
            daylight_bias: i32v("DaylightBias"),
            standard_start: rule("StandardStart"),
            daylight_start: rule("DaylightStart"),
            dynamic_dst_disabled: k.dword_value("DynamicDaylightTimeDisabled").map(|v| v != 0),
        });
    }
    if let Some(k) = sub(&cs, "Control\\Windows") {
        s.last_shutdown = filetime_value(&k, "ShutdownTime");
    }
    if let Some(k) = sub(&cs, "Control\\Session Manager\\Memory Management") {
        s.page_files = k
            .value("PagingFiles")
            .ok()
            .flatten()
            .and_then(|v| v.multi_string())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect();
        s.clear_page_file_at_shutdown = k.dword_value("ClearPageFileAtShutdown").map(|v| v != 0);
    }
    if let Some(k) = sub(&cs, "Control\\Power") {
        s.hibernate_enabled = k.dword_value("HibernateEnabled").map(|v| v != 0);
    }
    if let Some(k) = sub(&cs, "Control\\CrashControl") {
        s.crash_dump_enabled = k.dword_value("CrashDumpEnabled");
        s.crash_dump_file = text(&k, "DumpFile");
    }
    if let Some(k) = sub(&cs, "Control\\BitLockerStatus") {
        s.bitlocker_boot_status = k.dword_value("BootStatus");
    }
    if let Some(setup) = key(h, "Setup") {
        for u in subkeys(&setup)
            .into_iter()
            .filter(|k| k.name().starts_with("Source OS"))
        {
            s.upgrades.push(Upgrade {
                key_name: u.name(),
                product_name: text(&u, "ProductName"),
                build: text(&u, "CurrentBuild"),
                display_version: text(&u, "DisplayVersion").or_else(|| text(&u, "ReleaseId")),
                install_date: u
                    .dword_value("InstallDate")
                    .map(i64::from)
                    .filter(|&d| d > 0),
            });
        }
    }
    s
}

/// A `SYSTEMTIME`-encoded DST rule as text.
fn tz_rule(b: &[u8]) -> Option<String> {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month = b.u16_le(2).ok()?;
    if month == 0 {
        return None;
    }
    let dow = b.u16_le(4).ok()?;
    let week = b.u16_le(6).ok()?;
    let hour = b.u16_le(8).ok()?;
    let min = b.u16_le(10).ok()?;
    let day = DAYS.get(usize::from(dow)).copied().unwrap_or("?");
    let week = if week >= 5 {
        "last".to_owned()
    } else {
        format!("week {week}")
    };
    Some(format!("month {month}, {week} {day}, {hour:02}:{min:02}"))
}

/// A USB device (`Enum\USB\<VID_x&PID_y>\<serial>`, RegRipper
/// `autopsyusb`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    /// `VID_0781&PID_5567`.
    pub device: String,
    /// Instance id (serial number when the device has one).
    pub serial: String,
    /// Instance key last written (Unix seconds).
    pub last_written: Option<i64>,
    pub friendly_name: Option<String>,
    pub location: Option<String>,
}

/// Reads USB devices.
pub fn usb_devices(h: &Hive) -> Vec<UsbDevice> {
    let mut out = Vec::new();
    let Some(usb) = current_control_set(h).and_then(|cs| sub(&cs, "Enum\\USB")) else {
        return out;
    };
    for class in subkeys(&usb) {
        let device = class.name();
        for inst in subkeys(&class) {
            out.push(UsbDevice {
                device: device.clone(),
                serial: inst.name(),
                last_written: inst.last_written_unix(),
                friendly_name: text(&inst, "FriendlyName"),
                location: text(&inst, "LocationInformation"),
            });
        }
    }
    out
}

/// A network seen on an interface (RegRipper `autopsynic2`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceSsid {
    /// Adapter GUID.
    pub adapter: String,
    pub ssid: String,
    /// Adapter key last written (Unix seconds).
    pub last_written: Option<i64>,
}

/// SSIDs from `Tcpip\Parameters\Interfaces\<GUID>\<hint>` subkeys.
pub fn interface_ssids(h: &Hive) -> Vec<InterfaceSsid> {
    let mut out = Vec::new();
    let Some(ifs) =
        current_control_set(h).and_then(|cs| sub(&cs, "Services\\Tcpip\\Parameters\\Interfaces"))
    else {
        return out;
    };
    for g in subkeys(&ifs) {
        for s in subkeys(&g) {
            let bytes = perl_pack_h_reversed(&s.name());
            let ssid = String::from_utf8_lossy(&bytes)
                .replace('\0', " ")
                .trim()
                .to_owned();
            out.push(InterfaceSsid {
                adapter: g.name(),
                ssid,
                last_written: g.last_written_unix(),
            });
        }
    }
    out
}

/// Perl's `pack("h*", reverse $s)` (low nibble first), as `autopsynic2`
/// decodes `DhcpNetworkHint`-style names.
pub fn perl_pack_h_reversed(s: &str) -> Vec<u8> {
    let rev: Vec<u8> = s.bytes().rev().collect();
    let nib = |c: u8| -> u8 {
        if c.is_ascii_alphabetic() {
            ((c & 15) + 9) & 15
        } else {
            c & 15
        }
    };
    rev.chunks(2)
        .map(|p| {
            let lo = p.first().map_or(0, |&c| nib(c));
            let hi = p.get(1).map_or(0, |&c| nib(c));
            lo | (hi << 4)
        })
        .collect()
}

/// TCP/IP settings of a network interface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkInterface {
    pub guid: String,
    /// `Control\Network\…\Connection\Name` ("Local Area Connection").
    pub connection_name: Option<String>,
    pub dhcp_enabled: Option<bool>,
    pub ip_addresses: Vec<String>,
    pub subnet_masks: Vec<String>,
    pub gateways: Vec<String>,
    pub dns_servers: Vec<String>,
    pub domain: Option<String>,
    pub dhcp_ip: Option<String>,
    pub dhcp_server: Option<String>,
    pub dhcp_subnet: Option<String>,
    pub dhcp_gateways: Vec<String>,
    pub dhcp_dns: Option<String>,
    pub dhcp_domain: Option<String>,
    pub lease_obtained: Option<i64>,
    pub lease_expires: Option<i64>,
    pub last_written: Option<i64>,
}

/// Interfaces under `Tcpip\Parameters\Interfaces`.
pub fn network_interfaces(h: &Hive) -> Vec<NetworkInterface> {
    let Some(cs) = current_control_set(h) else {
        return Vec::new();
    };
    let Some(ifs) = sub(&cs, "Services\\Tcpip\\Parameters\\Interfaces") else {
        return Vec::new();
    };
    let names = sub(
        &cs,
        "Control\\Network\\{4D36E972-E325-11CE-BFC1-08002BE10318}",
    );
    let multi = |k: &Key<'_>, n: &str| -> Vec<String> {
        k.value(n)
            .ok()
            .flatten()
            .and_then(|v| v.multi_string())
            .unwrap_or_default()
            .into_iter()
            .flat_map(|s| s.split([',', ' ']).map(str::to_owned).collect::<Vec<_>>())
            .filter(|s| !s.is_empty() && s != "0.0.0.0")
            .collect()
    };
    subkeys(&ifs)
        .into_iter()
        .map(|g| {
            let guid = g.name();
            NetworkInterface {
                connection_name: names
                    .as_ref()
                    .and_then(|n| sub(n, &format!("{guid}\\Connection")))
                    .and_then(|c| text(&c, "Name")),
                dhcp_enabled: g.dword_value("EnableDHCP").map(|v| v != 0),
                ip_addresses: multi(&g, "IPAddress"),
                subnet_masks: multi(&g, "SubnetMask"),
                gateways: multi(&g, "DefaultGateway"),
                dns_servers: multi(&g, "NameServer"),
                domain: text(&g, "Domain"),
                dhcp_ip: text(&g, "DhcpIPAddress").filter(|s| s != "0.0.0.0"),
                dhcp_server: text(&g, "DhcpServer").filter(|s| s != "255.255.255.255"),
                dhcp_subnet: text(&g, "DhcpSubnetMask"),
                dhcp_gateways: multi(&g, "DhcpDefaultGateway"),
                dhcp_dns: text(&g, "DhcpNameServer"),
                dhcp_domain: text(&g, "DhcpDomain"),
                lease_obtained: g
                    .dword_value("LeaseObtainedTime")
                    .map(i64::from)
                    .filter(|&t| t > 0),
                lease_expires: g
                    .dword_value("LeaseTerminatesTime")
                    .map(i64::from)
                    .filter(|&t| t > 0),
                last_written: g.last_written_unix(),
                guid,
            }
        })
        .collect()
}

/// A Background/Desktop Activity Moderator record (RegRipper `bam`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BamEntry {
    pub sid: String,
    /// Path with `\Device\HarddiskVolumeN` removed (as Autopsy).
    pub path: String,
    /// Last execution (Unix seconds).
    pub last_run: i64,
    /// "BAM" or "DAM".
    pub source: &'static str,
}

/// BAM and DAM entries.
pub fn bam(h: &Hive) -> Vec<BamEntry> {
    let mut out = Vec::new();
    let Some(cs) = current_control_set(h) else {
        return out;
    };
    for (path, source) in [
        ("Services\\bam\\State\\UserSettings", "BAM"),
        ("Services\\bam\\UserSettings", "BAM"),
        ("Services\\dam\\State\\UserSettings", "DAM"),
        ("Services\\dam\\UserSettings", "DAM"),
    ] {
        let Some(us) = sub(&cs, path) else { continue };
        for sid_key in subkeys(&us) {
            let sid = sid_key.name();
            for v in sid_key.values().unwrap_or_default() {
                let name = v.name();
                if name == "Version" || name == "SequenceNumber" {
                    continue;
                }
                let Some(t) = v
                    .data()
                    .ok()
                    .and_then(|d| d.u64_le(0).ok())
                    .and_then(crate::filetime_unix)
                else {
                    continue;
                };
                out.push(BamEntry {
                    sid: sid.clone(),
                    path: strip_volume_device(&name),
                    last_run: t,
                    source,
                });
            }
        }
    }
    out
}

/// Removes `\Device\HarddiskVolumeN` (Autopsy's BAM handling).
pub fn strip_volume_device(p: &str) -> String {
    let lower = p.to_lowercase();
    if lower.starts_with("\\device\\harddiskvolume") {
        return p
            .get(16..)
            .and_then(|rest| {
                rest.find('\\')
                    .map(|i| rest.get(i..).unwrap_or_default().to_owned())
            })
            .unwrap_or_default();
    }
    p.to_owned()
}

/// A paired Bluetooth device (RegRipper `bthport`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    /// Key name (the MAC address in hex).
    pub address: String,
    pub name: Option<String>,
    pub last_seen: Option<i64>,
    pub last_connected: Option<i64>,
}

/// Devices under `Services\BTHPORT\Parameters\Devices`.
pub fn bluetooth_devices(h: &Hive) -> Vec<BluetoothDevice> {
    let Some(d) =
        current_control_set(h).and_then(|cs| sub(&cs, "Services\\BTHPORT\\Parameters\\Devices"))
    else {
        return Vec::new();
    };
    subkeys(&d)
        .into_iter()
        .map(|k| BluetoothDevice {
            address: k.name(),
            name: text_any(&k, "Name"),
            last_seen: filetime_value(&k, "LastSeen"),
            last_connected: filetime_value(&k, "LastConnected"),
        })
        .collect()
}

/// Whether the system is 32-bit (`PROCESSOR_ARCHITECTURE` = x86).
pub fn is_32bit(h: &Hive) -> bool {
    current_control_set(h)
        .and_then(|cs| sub(&cs, "Control\\Session Manager\\Environment"))
        .and_then(|k| text(&k, "PROCESSOR_ARCHITECTURE"))
        .is_none_or(|a| a.eq_ignore_ascii_case("x86"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perl_pack() {
        // The name is reversed, then packed low nibble first.
        assert_eq!(perl_pack_h_reversed("4241"), vec![0x41, 0x42]);
        assert_eq!(perl_pack_h_reversed("1"), vec![0x01]);
        assert_eq!(perl_pack_h_reversed("a"), vec![0x0A]);
    }

    #[test]
    fn volume_device() {
        assert_eq!(
            strip_volume_device("\\Device\\HarddiskVolume3\\Program Files\\x.exe"),
            "\\Program Files\\x.exe"
        );
        assert_eq!(strip_volume_device("C:\\x.exe"), "C:\\x.exe");
    }
}
