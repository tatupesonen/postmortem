#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::many_single_char_names,
    clippy::doc_markdown,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::Arc;

use pm_core::io::MemSource;
use pm_lnk::builder::{FileItem, dos, file_item, my_computer, volume_item};
use pm_registry::Hive;
use pm_registry::builder::{HiveBuilder, utf16z};
use pm_winartifacts::registry::{amcache, ntuser, sam, shellbags, shimcache, software, system};
use pm_winartifacts::{recentfilecache, recycle, usb_ids, zone};

fn hive(b: &HiveBuilder) -> Hive {
    Hive::open(Arc::new(MemSource::new(b.build()))).unwrap()
}

/// FILETIME for Unix seconds.
fn ft(unix: i64) -> u64 {
    ((unix + 11_644_473_600) * 10_000_000) as u64
}

const CS: &str = "ControlSet001";

#[test]
fn system_hive() {
    let mut b = HiveBuilder::new("SYSTEM");
    b.dword("Select", "Current", 1)
        .sz(
            &format!("{CS}\\Control\\ComputerName\\ComputerName"),
            "ComputerName",
            "N-1A9ODN6ZXK4LQ",
        )
        .sz(
            &format!("{CS}\\Services\\Tcpip\\Parameters"),
            "Hostname",
            "n-1a9odn6zxk4lq",
        )
        .sz(
            &format!("{CS}\\Services\\Tcpip\\Parameters"),
            "Domain",
            "corp.local",
        )
        .sz(
            &format!("{CS}\\Control\\Session Manager\\Environment"),
            "OS",
            "Windows_NT",
        )
        .sz(
            &format!("{CS}\\Control\\Session Manager\\Environment"),
            "PROCESSOR_ARCHITECTURE",
            "x86",
        )
        .expand_sz(
            &format!("{CS}\\Control\\Session Manager\\Environment"),
            "TEMP",
            "%SystemRoot%\\TEMP",
        );
    let tz = format!("{CS}\\Control\\TimeZoneInformation");
    let mut rule = Vec::new();
    for v in [0u16, 11, 0, 1, 2, 0, 0, 0] {
        rule.extend(v.to_le_bytes());
    }
    b.sz(&tz, "StandardName", "Central Standard Time")
        .sz(&tz, "DaylightName", "Central Daylight Time")
        .dword(&tz, "Bias", 360)
        .dword(&tz, "ActiveTimeBias", 300)
        .binary(&tz, "StandardStart", &rule)
        .binary(
            &format!("{CS}\\Control\\Windows"),
            "ShutdownTime",
            &ft(1_093_000_000).to_le_bytes(),
        )
        .multi_sz(
            &format!("{CS}\\Control\\Session Manager\\Memory Management"),
            "PagingFiles",
            &["C:\\pagefile.sys 1536 3072"],
        )
        .dword(
            &format!("{CS}\\Control\\CrashControl"),
            "CrashDumpEnabled",
            3,
        )
        .dword(&format!("{CS}\\Control\\Power"), "HibernateEnabled", 1)
        .sz(
            "Setup\\Source OS (Updated on 1/2/2021 10:00:00)",
            "ProductName",
            "Windows 10 Pro",
        )
        .dword(
            "Setup\\Source OS (Updated on 1/2/2021 10:00:00)",
            "InstallDate",
            1_500_000_000,
        );
    // USB, NIC SSIDs, interfaces, BAM, Bluetooth.
    b.key_time(
        &format!("{CS}\\Enum\\USB\\VID_0781&PID_5567\\4C530103131102119123"),
        ft(1_093_100_000),
    )
    .sz(
        &format!("{CS}\\Enum\\USB\\VID_0781&PID_5567\\4C530103131102119123"),
        "FriendlyName",
        "Cruzer",
    );
    let ifs = format!(
        "{CS}\\Services\\Tcpip\\Parameters\\Interfaces\\{{86FB1E2A-1F55-4C2C-A8B0-5A3B6F1C2D3E}}"
    );
    b.key_time(&ifs, ft(1_093_200_000))
        .dword(&ifs, "EnableDHCP", 1)
        .sz(&ifs, "DhcpIPAddress", "192.168.1.111")
        .sz(&ifs, "DhcpServer", "192.168.1.1")
        .dword(&ifs, "LeaseObtainedTime", 1_093_200_000)
        .key(&format!("{ifs}\\4241"));
    let bam = format!("{CS}\\Services\\bam\\State\\UserSettings\\S-1-5-21-1-2-3-1001");
    let mut bam_data = ft(1_570_493_613).to_le_bytes().to_vec();
    bam_data.extend([0u8; 16]);
    b.binary(
        &bam,
        "\\Device\\HarddiskVolume3\\Program Files\\TechSmith\\Snagit32.exe",
        &bam_data,
    )
    .dword(&bam, "Version", 1);
    let bt = format!("{CS}\\Services\\BTHPORT\\Parameters\\Devices\\0016cb8a2f0c");
    b.binary(&bt, "Name", b"Phone\0")
        .qword(&bt, "LastSeen", ft(1_592_907_874))
        .qword(&bt, "LastConnected", ft(1_592_907_000));
    let h = hive(&b);

    let s = system::system_info(&h);
    assert_eq!(s.control_set.as_deref(), Some(CS));
    assert_eq!(s.computer_name.as_deref(), Some("N-1A9ODN6ZXK4LQ"));
    assert_eq!(s.hostname.as_deref(), Some("n-1a9odn6zxk4lq"));
    assert_eq!(s.domain.as_deref(), Some("corp.local"));
    assert_eq!(s.os_env.as_deref(), Some("Windows_NT"));
    assert_eq!(s.temp_dir.as_deref(), Some("%SystemRoot%\\TEMP"));
    let tz = s.time_zone.as_ref().unwrap();
    assert_eq!(tz.name(), Some("Central Standard Time"));
    assert_eq!(tz.utc_offset_minutes(), Some(-360));
    assert_eq!(tz.active_bias, Some(300));
    assert_eq!(
        tz.standard_start.as_deref(),
        Some("month 11, week 1 Sun, 02:00")
    );
    assert_eq!(s.last_shutdown, Some(1_093_000_000));
    assert_eq!(s.page_files, vec!["C:\\pagefile.sys 1536 3072"]);
    assert_eq!(s.crash_dump_enabled, Some(3));
    assert_eq!(s.hibernate_enabled, Some(true));
    assert_eq!(s.upgrades.len(), 1);
    assert_eq!(s.upgrades[0].install_date, Some(1_500_000_000));
    assert!(system::is_32bit(&h));

    let usb = system::usb_devices(&h);
    assert_eq!(usb.len(), 1);
    assert_eq!(usb[0].device, "VID_0781&PID_5567");
    assert_eq!(usb[0].serial, "4C530103131102119123");
    assert_eq!(usb[0].last_written, Some(1_093_100_000));
    let info = usb_ids::lookup(&usb[0].device).unwrap();
    assert_eq!(
        (info.vendor.as_str(), info.product.as_str()),
        ("SanDisk Corp.", "Cruzer Blade")
    );

    let ssids = system::interface_ssids(&h);
    assert_eq!(ssids[0].ssid, "AB");
    assert_eq!(ssids[0].last_written, Some(1_093_200_000));
    let ni = &system::network_interfaces(&h)[0];
    assert_eq!(ni.dhcp_ip.as_deref(), Some("192.168.1.111"));
    assert_eq!(ni.dhcp_server.as_deref(), Some("192.168.1.1"));
    assert_eq!(ni.lease_obtained, Some(1_093_200_000));

    let bam = system::bam(&h);
    assert_eq!(bam.len(), 1);
    assert_eq!(bam[0].path, "\\Program Files\\TechSmith\\Snagit32.exe");
    assert_eq!(bam[0].last_run, 1_570_493_613);
    assert_eq!(bam[0].sid, "S-1-5-21-1-2-3-1001");

    let bt = system::bluetooth_devices(&h);
    assert_eq!(bt[0].name.as_deref(), Some("Phone"));
    assert_eq!(bt[0].last_seen, Some(1_592_907_874));
    assert_eq!(bt[0].last_connected, Some(1_592_907_000));
}

#[test]
fn shimcache_formats() {
    // Windows 10 format.
    let mut v = vec![0u8; 0x34];
    v[..4].copy_from_slice(&0x34u32.to_le_bytes());
    for (p, exec) in [("\\??\\C:\\Windows\\a.exe", 1u32), ("C:\\b.exe", 0)] {
        let u: Vec<u8> = p.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut e = b"10ts".to_vec();
        e.extend([0u8; 4]);
        let mut body = Vec::new();
        body.extend((u.len() as u16).to_le_bytes());
        body.extend(&u);
        body.extend(ft(1_600_000_000).to_le_bytes());
        body.extend(8u32.to_le_bytes());
        body.extend([0u8; 4]);
        body.extend(exec.to_le_bytes());
        e.extend((body.len() as u32).to_le_bytes());
        e.extend(body);
        v.extend(e);
    }
    let e = shimcache::parse(&v, false);
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].path, "C:\\Windows\\a.exe");
    assert_eq!(e[0].last_modified, Some(1_600_000_000));
    assert_eq!(e[0].executed, Some(true));
    assert_eq!(e[1].executed, Some(false));

    // Windows XP format.
    let mut x = vec![0u8; 400];
    x[..4].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    x[4..8].copy_from_slice(&1u32.to_le_bytes());
    let mut e = vec![0u8; 552];
    let u: Vec<u8> = "C:\\WINDOWS\\x.exe"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    e[..u.len()].copy_from_slice(&u);
    e[528..536].copy_from_slice(&ft(1_000_000_000).to_le_bytes());
    x.extend(e);
    let mut b = HiveBuilder::new("SYSTEM");
    b.dword("Select", "Current", 1).binary(
        &format!("{CS}\\Control\\Session Manager\\AppCompatibility"),
        "AppCompatCache",
        &x,
    );
    let e = shimcache::shimcache(&hive(&b));
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].path, "C:\\WINDOWS\\x.exe");
    assert_eq!(e[0].last_modified, Some(1_000_000_000));

    // Windows 7 x64.
    let mut w = vec![0u8; 128];
    w[..4].copy_from_slice(&0xBADC_0FEEu32.to_le_bytes());
    w[4..8].copy_from_slice(&1u32.to_le_bytes());
    let path_off = 128 + 48;
    let mut e = vec![0u8; 48];
    let u: Vec<u8> = "C:\\x7.exe"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    e[0..2].copy_from_slice(&(u.len() as u16).to_le_bytes());
    e[8..16].copy_from_slice(&(path_off as u64).to_le_bytes());
    e[16..24].copy_from_slice(&ft(1_300_000_000).to_le_bytes());
    e[24..28].copy_from_slice(&2u32.to_le_bytes());
    w.extend(e);
    w.extend(u);
    let e = shimcache::parse(&w, false);
    assert_eq!(e[0].path, "C:\\x7.exe");
    assert_eq!(e[0].executed, Some(true));
    assert!(shimcache::parse(&[1, 2, 3], true).is_empty());
}

#[test]
fn software_hive() {
    let cv = "Microsoft\\Windows NT\\CurrentVersion";
    let mut b = HiveBuilder::new("SOFTWARE");
    b.sz(cv, "ProductName", "Microsoft Windows XP")
        .sz(cv, "CSDVersion", "Service Pack 2")
        .sz(cv, "CurrentVersion", "5.1")
        .sz(cv, "CurrentBuildNumber", "2600")
        .dword(cv, "InstallDate", 1_092_955_707)
        .sz(cv, "RegisteredOwner", "Greg Schardt")
        .sz(cv, "RegisteredOrganization", "N/A")
        .sz(cv, "SystemRoot", "C:\\WINDOWS")
        .sz(cv, "ProductId", "55274-640-0147306-23684");
    let un = "Microsoft\\Windows\\CurrentVersion\\Uninstall";
    b.key_time(&format!("{un}\\Ethereal"), ft(1_093_000_000))
        .sz(&format!("{un}\\Ethereal"), "DisplayName", "Ethereal 0.10.6")
        .sz(&format!("{un}\\Ethereal"), "DisplayVersion", "0.10.6")
        .key_time(&format!("{un}\\NoName"), ft(1_093_000_100));
    let pl = format!("{cv}\\ProfileList\\S-1-5-21-2000478354-688789844-1708537768-1003");
    b.expand_sz(
        &pl,
        "ProfileImagePath",
        "%SystemDrive%\\Documents and Settings\\Mr. Evil.CORP",
    );
    let prof = format!("{cv}\\NetworkList\\Profiles\\{{11111111-2222-3333-4444-555555555555}}");
    let mut st = Vec::new();
    for v in [2021u16, 3, 4, 4, 10, 20, 30, 0] {
        st.extend(v.to_le_bytes());
    }
    b.sz(&prof, "ProfileName", "CoffeeShop")
        .dword(&prof, "NameType", 71)
        .dword(&prof, "Managed", 0)
        .binary(&prof, "DateLastConnected", &st);
    let sig = format!("{cv}\\NetworkList\\Signatures\\Unmanaged\\0102");
    b.sz(
        &sig,
        "ProfileGuid",
        "{11111111-2222-3333-4444-555555555555}",
    )
    .binary(
        &sig,
        "DefaultGatewayMac",
        &[0, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e],
    )
    .sz(&sig, "DnsSuffix", "<none>");
    b.sz(&format!("{cv}\\Winlogon"), "DefaultUserName", "Mr. Evil")
        .sz(&format!("{cv}\\Winlogon"), "AutoAdminLogon", "1")
        .sz(&format!("{cv}\\Winlogon"), "DefaultPassword", "hunter2")
        .dword(
            "Microsoft\\Windows Defender\\Exclusions\\Paths",
            "C:\\tools",
            0,
        )
        .dword(
            "Policies\\Microsoft\\Windows Defender",
            "DisableAntiSpyware",
            1,
        );
    let h = hive(&b);

    let v = software::windows_version(&h);
    assert_eq!(
        v.os_name().as_deref(),
        Some("Microsoft Windows XP Service Pack 2")
    );
    assert_eq!(v.install_date, Some(1_092_955_707));
    assert_eq!(v.registered_owner.as_deref(), Some("Greg Schardt"));
    assert_eq!(v.product_id.as_deref(), Some("55274-640-0147306-23684"));
    assert_eq!(v.build().as_deref(), Some("2600"));

    let progs = software::installed_programs(&h);
    assert_eq!(progs[0].name, "NoName", "newest first, key name fallback");
    assert_eq!(progs[1].name, "Ethereal 0.10.6 v.0.10.6");
    assert_eq!(progs[1].last_written, Some(1_093_000_000));

    let pl = software::profile_list(&h);
    assert_eq!(pl[0].user, "Mr");
    assert_eq!(pl[0].sid, "S-1-5-21-2000478354-688789844-1708537768-1003");

    let np = &software::network_profiles(&h)[0];
    assert_eq!(np.name, "CoffeeShop");
    assert_eq!(np.type_name(), "Wireless");
    assert_eq!(np.managed, Some(false));
    assert_eq!(np.last_connected, Some(1_614_853_230));
    assert_eq!(np.gateway_mac.as_deref(), Some("00:1A:2B:3C:4D:5E"));
    assert_eq!(np.dns_suffix, None);

    let l = software::logon_info(&h);
    assert_eq!(l.last_logged_on_user.as_deref(), Some("Mr. Evil"));
    assert!(l.auto_admin_logon);
    assert_eq!(l.default_password.as_deref(), Some("hunter2"));

    let d = software::defender(&h);
    assert_eq!(
        d.exclusions,
        vec![("Paths".to_owned(), "C:\\tools".to_owned())]
    );
    assert_eq!(d.anti_spyware_disabled, Some(true));
}

fn v_value(name: &str, full: &str, kind: u32) -> Vec<u8> {
    let mut v = vec![0u8; 0xCC];
    let n: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let f: Vec<u8> = full.encode_utf16().flat_map(u16::to_le_bytes).collect();
    v[4..8].copy_from_slice(&kind.to_le_bytes());
    v[12..16].copy_from_slice(&0u32.to_le_bytes());
    v[16..20].copy_from_slice(&(n.len() as u32).to_le_bytes());
    v[24..28].copy_from_slice(&(n.len() as u32).to_le_bytes());
    v[28..32].copy_from_slice(&(f.len() as u32).to_le_bytes());
    v.extend(n);
    v.extend(f);
    v
}

#[test]
fn sam_hive() {
    let mut b = HiveBuilder::new("SAM");
    let mut acct_v = vec![0u8; 40];
    for x in [2_000_478_354u32, 688_789_844, 1_708_537_768] {
        acct_v.extend(x.to_le_bytes());
    }
    b.binary("SAM\\Domains\\Account", "V", &acct_v);
    let users = "SAM\\Domains\\Account\\Users";
    let mut f = vec![0u8; 80];
    f[8..16].copy_from_slice(&ft(1_093_621_604).to_le_bytes());
    f[24..32].copy_from_slice(&ft(1_092_955_000).to_le_bytes());
    f[48..52].copy_from_slice(&1003u32.to_le_bytes());
    f[56..58].copy_from_slice(&0x0210u16.to_le_bytes());
    f[66..68].copy_from_slice(&15u16.to_le_bytes());
    b.binary(
        &format!("{users}\\000003EB"),
        "V",
        &v_value("Mr. Evil", "Greg", 0xBC),
    )
    .binary(&format!("{users}\\000003EB"), "F", &f)
    .binary(
        &format!("{users}\\000003EB"),
        "UserPasswordHint",
        &utf16z("the usual"),
    )
    .key_time(&format!("{users}\\Names\\Mr. Evil"), ft(1_092_955_100));
    // Administrators alias with one member.
    let mut c = vec![0u8; 0x34];
    let gname: Vec<u8> = "Administrators"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    c[16..20].copy_from_slice(&0u32.to_le_bytes());
    c[20..24].copy_from_slice(&(gname.len() as u32).to_le_bytes());
    c[40..44].copy_from_slice(&(gname.len() as u32).to_le_bytes());
    c[48..52].copy_from_slice(&1u32.to_le_bytes());
    c.extend(&gname);
    let mut sid = vec![1u8, 5, 0, 0, 0, 0, 0, 5];
    for x in [21u32, 2_000_478_354, 688_789_844, 1_708_537_768, 1003] {
        sid.extend(x.to_le_bytes());
    }
    c.extend(sid);
    b.binary("SAM\\Domains\\Builtin\\Aliases\\00000220", "C", &c);
    let info = sam::sam(&hive(&b));
    assert_eq!(
        info.machine_sid.as_deref(),
        Some("S-1-5-21-2000478354-688789844-1708537768")
    );
    let u = &info.users[0];
    assert_eq!(u.name, "Mr. Evil");
    assert_eq!(u.full_name.as_deref(), Some("Greg"));
    assert_eq!(u.rid, 1003);
    assert_eq!(
        u.sid.as_deref(),
        Some("S-1-5-21-2000478354-688789844-1708537768-1003")
    );
    assert!(u.is_admin());
    assert_eq!(u.last_login, Some(1_093_621_604));
    assert_eq!(u.password_reset, Some(1_092_955_000));
    assert_eq!(u.login_count, 15);
    assert_eq!(u.created, Some(1_092_955_100));
    assert_eq!(u.password_hint.as_deref(), Some("the usual"));
    assert_eq!(
        u.flag_names(),
        vec!["Normal user account", "Password does not expire"]
    );
    assert_eq!(info.groups[0].name, "Administrators");
    assert_eq!(
        info.groups_of(u.sid.as_deref().unwrap()),
        vec!["Administrators"]
    );
}

#[test]
fn ntuser_hive() {
    let mut b = HiveBuilder::new("NTUSER");
    b.sz("Network\\Z", "RemotePath", "\\\\server\\share")
        .sz(
            "Software\\Microsoft\\MediaPlayer\\Player\\RecentFileList",
            "File1",
            "C:\\b.mp3",
        )
        .sz(
            "Software\\Microsoft\\MediaPlayer\\Player\\RecentFileList",
            "File0",
            "C:\\a.mp3",
        )
        .sz("Software\\WinRAR\\ArcHistory", "0", "C:\\x.rar")
        .sz(
            "Software\\Microsoft\\Microsoft Management Console\\Recent File List",
            "File1",
            "C:\\m.msc",
        )
        .sz(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Applets\\Paint\\Recent File List",
            "File1",
            "C:\\p.bmp",
        );
    let mut arc = utf16z("C:\\one.7z");
    arc.extend(utf16z("D:\\two.zip"));
    b.binary("Software\\7-Zip\\Compression", "ArcHistory", &arc);
    let ad = "Software\\Adobe\\Acrobat Reader\\9.0\\AVGeneral\\cRecentFiles\\c1";
    b.binary(ad, "sDI", b"/C/Docs/x.pdf\0")
        .binary(ad, "sDate", b"D:20200131104456-05'00'\0");
    b.key("Software\\Microsoft\\Office\\14.0\\User Settings")
        .sz(
            "Software\\Microsoft\\Office\\14.0\\Word\\File MRU",
            "Item 1",
            "[F00000000][T01CBAD6CD4D7E1A0]*F:\\Windows_time_Rules_xp.doc",
        )
        .binary(
            "Software\\Microsoft\\Office\\14.0\\Word\\Security\\Trusted Documents\\TrustRecords",
            "%USERPROFILE%/Downloads/evil.doc",
            &ft(1_294_283_922).to_le_bytes(),
        );
    b.key_time("Software\\Microsoft\\Office\\11.0", ft(1_100_000_000))
        .key("Software\\Microsoft\\Office\\11.0\\Common\\Open Find")
        .sz(
            "Software\\Microsoft\\Office\\11.0\\Excel\\Recent Files",
            "File1",
            "C:\\book.xls",
        );
    let lx = "Software\\Microsoft\\Windows\\CurrentVersion\\Lxss\\{12345678-1234-1234-1234-123456789012}";
    b.sz(lx, "DistributionName", "Ubuntu").sz(
        lx,
        "BasePath",
        "C:\\Users\\x\\AppData\\Local\\Packages\\Ubuntu\\LocalState",
    );
    let ua = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\UserAssist\\{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}\\Count";
    let mut d = vec![0u8; 72];
    d[4..8].copy_from_slice(&9u32.to_le_bytes());
    d[60..68].copy_from_slice(&ft(1_400_000_000).to_le_bytes());
    b.binary(
        ua,
        &ntuser::rot13("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\cmd.exe"),
        &d,
    );
    let mut xp = vec![0u8; 16];
    xp[4..8].copy_from_slice(&12u32.to_le_bytes());
    xp[8..16].copy_from_slice(&ft(1_093_000_000).to_le_bytes());
    b.binary(
        ua,
        &ntuser::rot13("UEME_RUNPATH:C:\\Program Files\\Ethereal\\ethereal.exe"),
        &xp,
    );
    b.sz(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RunMRU",
        "a",
        "cmd\\1",
    )
    .sz(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RunMRU",
        "b",
        "regedit\\1",
    )
    .sz(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RunMRU",
        "MRUList",
        "ba",
    )
    .sz(
        "Software\\Microsoft\\Internet Explorer\\TypedURLs",
        "url1",
        "http://www.example.com/",
    )
    .sz(
        "Software\\Microsoft\\Windows\\Shell\\Associations\\UrlAssociations\\http\\UserChoice",
        "ProgId",
        "ChromeHTML",
    );
    let h = hive(&b);

    let nd = ntuser::network_drives(&h);
    assert_eq!(
        (nd[0].local_path.as_str(), nd[0].remote_path.as_str()),
        ("Network\\Z", "\\\\server\\share")
    );

    let r = ntuser::recent_files(&h);
    let paths: Vec<(&str, &str)> = r.iter().map(|x| (x.path.as_str(), x.comment)).collect();
    assert_eq!(
        paths,
        vec![
            ("C:/Docs/x.pdf", ntuser::ADOBE),
            ("C:\\a.mp3", ntuser::MEDIA_PLAYER),
            ("C:\\b.mp3", ntuser::MEDIA_PLAYER),
            ("C:\\one.7z", ntuser::SEVEN_ZIP),
            ("D:\\two.zip", ntuser::SEVEN_ZIP),
            ("C:\\p.bmp", ntuser::APPLETS),
            ("C:\\m.msc", ntuser::MMC),
            ("C:\\x.rar", ntuser::WINRAR),
            ("%USERPROFILE%/Downloads/evil.doc", ntuser::OFFICE_TRUST),
            ("F:\\Windows_time_Rules_xp.doc", ntuser::OFFICE),
        ]
    );
    assert_eq!(r[0].accessed, Some(1_580_485_496));
    assert_eq!(r[8].accessed, Some(1_294_283_922));
    assert_eq!(
        r[9].accessed,
        Some(1_294_296_124),
        "FILETIME 01CBAD6CD4D7E1A0"
    );

    let legacy = ntuser::office_legacy(&h);
    assert_eq!(legacy.len(), 1);
    assert_eq!(legacy[0].program, "Excel");
    assert_eq!(legacy[0].name, "File1");
    assert_eq!(legacy[0].value, "C:\\book.xls");
    assert_eq!(legacy[0].mtime, Some(1_100_000_000));

    let w = ntuser::wsl(&h);
    assert_eq!(w[0].name, "Ubuntu");
    assert!(w[0].image_path.ends_with("LocalState\\ext4.vhdx"));

    let mut ua = ntuser::user_assist(&h);
    ua.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(ua[0].name, "C:\\Windows\\System32\\cmd.exe");
    assert_eq!(ua[0].run_count, 9);
    assert_eq!(ua[0].last_run, Some(1_400_000_000));
    assert_eq!(
        ua[1].name,
        "UEME_RUNPATH:C:\\Program Files\\Ethereal\\ethereal.exe"
    );
    assert_eq!(ua[1].run_count, 7, "XP counts start at 5");

    assert_eq!(ntuser::run_mru(&h), vec!["regedit", "cmd"]);
    assert_eq!(
        ntuser::typed_urls(&h),
        vec![("http://www.example.com/".to_owned(), None)]
    );
    assert_eq!(ntuser::default_browser(&h).as_deref(), Some("ChromeHTML"));
}

#[test]
fn shellbags_from_bagmru() {
    let root = "Local Settings\\Software\\Microsoft\\Windows\\Shell\\BagMRU";
    let mut b = HiveBuilder::new("USRCLASS");
    b.binary(root, "0", &my_computer())
        .binary(&format!("{root}\\0"), "0", &volume_item("C:\\"))
        .binary(
            &format!("{root}\\0\\0"),
            "0",
            &file_item(&FileItem {
                short_name: "TOOLS",
                long_name: "Tools",
                is_dir: true,
                size: 0,
                modified: dos(2020, 1, 2, 3, 4, 6),
                created: dos(2019, 1, 1, 0, 0, 0),
                accessed: dos(2020, 1, 2, 0, 0, 0),
                version: 9,
                mft: (77, 2),
            }),
        )
        .key_time(&format!("{root}\\0\\0\\0"), ft(1_577_934_000));
    let bags = shellbags::shellbags(&hive(&b), shellbags::USRCLASS_ROOTS);
    let paths: Vec<&str> = bags.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(
        paths,
        vec!["My Computer", "My Computer\\C:\\", "My Computer\\C:\\Tools"]
    );
    let t = &bags[2];
    assert_eq!(t.key, format!("{root}\\0\\0\\0"));
    assert_eq!(t.last_written, Some(1_577_934_000));
    assert_eq!(t.modified, Some(1_577_934_246));
    assert_eq!(t.mft_entry, Some(77));
}

#[test]
fn amcache_hive() {
    let mut b = HiveBuilder::new("Amcache");
    let f = "Root\\InventoryApplicationFile\\nc.exe|abc";
    b.sz(f, "LowerCaseLongPath", "c:\\tools\\nc.exe")
        .sz(f, "FileId", "0000A94A8FE5CCB19BA61C4C0873D391E987982FBBD3")
        .qword(f, "Size", 59_392)
        .sz(f, "LinkDate", "12/26/2010 00:00:00")
        .key_time(f, ft(1_600_000_000));
    let o = "Root\\File\\{volume}\\1000a";
    b.sz(o, "15", "C:\\old\\x.exe")
        .sz(o, "101", "0000ffffffffffffffffffffffffffffffffffffffff")
        .qword(o, "11", ft(1_400_000_000));
    let files = amcache::files(&hive(&b));
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "c:\\tools\\nc.exe");
    assert_eq!(
        files[0].sha1.as_deref(),
        Some("a94a8fe5ccb19ba61c4c0873d391e987982fbbd3")
    );
    assert_eq!(files[0].size, Some(59_392));
    assert_eq!(files[0].link_date, Some(1_293_321_600));
    assert_eq!(files[0].key_last_written, Some(1_600_000_000));
    assert_eq!(files[1].path, "C:\\old\\x.exe");
    assert_eq!(files[1].file_modified, Some(1_400_000_000));
}

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/recyclebin")
        .join(name);
    std::fs::read(p).unwrap()
}

/// RBCmd's test files (MIT, see fixtures/README.md); expected values
/// computed independently from the raw bytes.
#[test]
fn recycle_bin_fixtures() {
    let i = recycle::parse_dollar_i(&fixture("win10_I2JRX90")).unwrap();
    assert_eq!(i.version, 2);
    assert_eq!(i.file_size, 14_155_310);
    assert_eq!(i.deleted, Some(1_533_671_698));
    assert_eq!(i.original_path, "C:\\temp");
    let i = recycle::parse_dollar_i(&fixture("win7_ID5HSV3.exe")).unwrap();
    assert_eq!(i.version, 1);
    assert_eq!(i.file_size, 3_943_584);
    assert_eq!(i.deleted, Some(1_333_737_787));
    assert_eq!(i.original_path, "C:\\Windows\\System32\\hythonize.exe");

    let r = recycle::parse_info2(&fixture("xp_INFO2"));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].original_path, "Z:\\WINDOWS\\explorer.exe");
    assert_eq!(r[0].deleted, Some(1_201_705_983));
    assert_eq!(r[0].size, 1_005_568);
    assert_eq!(r[0].content_name(), "Dz1.exe");
    let m = recycle::parse_info2(&fixture("xp_INFO2_MultiFiles"));
    assert_eq!(m.len(), 4);
    assert!(!m[0].removed);
    assert!(m[1].removed, "restored entries lose the ANSI path");
    assert_eq!(
        m[1].original_path,
        "C:\\Documents and Settings\\test\\Desktop\\first file.txt"
    );
    assert_eq!(m[3].content_name(), "Dc4");
}

#[test]
fn small_formats() {
    assert_eq!(
        zone::parse(b"[ZoneTransfer]\nZoneId=3\nHostUrl=http://x/y.exe\n").zone_id,
        Some(3)
    );
    assert!(recentfilecache::parse(b"nope").is_none());
}
