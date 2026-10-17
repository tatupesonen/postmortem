#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

//! End-to-end: Recent Activity's Windows extractors on a generated NTFS
//! volume holding registry hives, a Recent shell link, a prefetch file, a
//! recycle-bin pair and a Zone.Identifier stream.

use std::collections::BTreeMap;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::catalog::artifact_type_id;
use pm_ingest::{Inbox, IngestProgress, JobSettings, ModuleFactory, ModuleSetting, run_job};
use pm_lnk::builder::LnkBuilder;
use pm_modules::recent_activity::RecentActivityFactory;
use pm_prefetch::builder::PrefetchBuilder;
use pm_registry::builder::HiveBuilder;
use pm_testkit::ntfs::{FileOpts, NtfsBuilder};

fn ft(unix: i64) -> u64 {
    ((unix + 11_644_473_600) * 10_000_000) as u64
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A SAM `V` value: account type, then name/full name/comment fields
/// (offset, length) pointing into the data area at 0xCC.
fn sam_v(account_type: u32, name: &str, full_name: &str, comment: &str) -> Vec<u8> {
    let mut v = vec![0u8; 0xCC];
    v[4..8].copy_from_slice(&account_type.to_le_bytes());
    let mut data = Vec::new();
    for (i, text) in [(3usize, name), (6, full_name), (9, comment)] {
        let bytes = utf16(text);
        v[i * 4..i * 4 + 4].copy_from_slice(&(data.len() as u32).to_le_bytes());
        v[(i + 1) * 4..(i + 1) * 4 + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        data.extend(bytes);
    }
    v.extend(data);
    v
}

/// A SAM `F` value: last login, password reset/fail, flags, logins.
fn sam_f(last_login: i64, reset: i64, acb: u16, logins: u16) -> Vec<u8> {
    let mut f = vec![0u8; 80];
    f[8..16].copy_from_slice(&ft(last_login).to_le_bytes());
    f[24..32].copy_from_slice(&ft(reset).to_le_bytes());
    f[56..58].copy_from_slice(&acb.to_le_bytes());
    f[66..68].copy_from_slice(&logins.to_le_bytes());
    f
}

fn sid_bytes(subs: &[u32]) -> Vec<u8> {
    let mut b = vec![1u8, subs.len() as u8, 0, 0, 0, 0, 0, 5];
    for s in subs {
        b.extend(s.to_le_bytes());
    }
    b
}

/// SAM of machine S-1-5-21-11-22-33: Administrator (RID 500, admin,
/// e-mail, security question) and bob (RID 1000); bob is an
/// Administrators member.
fn sam_hive() -> Vec<u8> {
    let mut account_v = vec![0u8; 0x30];
    for x in [11u32, 22, 33] {
        account_v.extend(x.to_le_bytes());
    }
    let users = "SAM\\Domains\\Account\\Users";
    let mut c = vec![0u8; 0x34];
    let name = utf16("Administrators");
    let member = sid_bytes(&[21, 11, 22, 33, 1000]);
    let mut data = name.clone();
    let members_off = data.len() as u32;
    data.extend(&member);
    c[16..20].copy_from_slice(&0u32.to_le_bytes());
    c[20..24].copy_from_slice(&(name.len() as u32).to_le_bytes());
    c[40..44].copy_from_slice(&members_off.to_le_bytes());
    c[48..52].copy_from_slice(&1u32.to_le_bytes());
    c.extend(data);
    let reset = utf16(r#"{"version":1,"questions":[{"question":"First pet?","answer":"Rex"}]}"#);
    let mut sam = HiveBuilder::new("SAM");
    sam.binary("SAM\\Domains\\Account", "V", &account_v)
        .binary(
            &format!("{users}\\000001F4"),
            "V",
            &sam_v(0xBC, "Administrator", "Admin Istrator", "Built-in"),
        )
        .binary(
            &format!("{users}\\000001F4"),
            "F",
            &sam_f(1_090_000_000, 1_080_000_000, 0x0210, 7),
        )
        .binary(
            &format!("{users}\\000001F4"),
            "InternetUserName",
            &utf16("Admin@Example.com"),
        )
        .binary(&format!("{users}\\000001F4"), "ResetData", &reset)
        .key_time(&format!("{users}\\Names\\Administrator"), ft(1_070_000_000))
        .binary(
            &format!("{users}\\000003E8"),
            "V",
            &sam_v(0xD4, "bob", "", ""),
        )
        .binary(
            &format!("{users}\\000003E8"),
            "F",
            &sam_f(1_091_000_000, 1_085_000_000, 0x0010, 3),
        )
        .key_time(&format!("{users}\\Names\\bob"), ft(1_075_000_000))
        .binary("SAM\\Domains\\Builtin\\Aliases\\00000220", "C", &c);
    sam.build()
}

fn image() -> Vec<u8> {
    let cs = "ControlSet001";
    let mut system = HiveBuilder::new("SYSTEM");
    system
        .dword("Select", "Current", 1)
        .sz(
            &format!("{cs}\\Control\\ComputerName\\ComputerName"),
            "ComputerName",
            "TESTPC",
        )
        .sz(
            &format!("{cs}\\Control\\Session Manager\\Environment"),
            "PROCESSOR_ARCHITECTURE",
            "x86",
        )
        .sz(
            &format!("{cs}\\Control\\TimeZoneInformation"),
            "StandardName",
            "Central Standard Time",
        )
        .dword(&format!("{cs}\\Control\\TimeZoneInformation"), "Bias", 360)
        .sz(
            &format!("{cs}\\Services\\Tcpip\\Parameters"),
            "Domain",
            "CORP",
        )
        .key_time(
            &format!("{cs}\\Enum\\USB\\VID_0781&PID_5567\\SERIAL123"),
            ft(1_100_000_000),
        );
    let mut software = HiveBuilder::new("SOFTWARE");
    let cv = "Microsoft\\Windows NT\\CurrentVersion";
    software
        .sz(cv, "ProductName", "Microsoft Windows XP")
        .sz(cv, "CSDVersion", "Service Pack 2")
        .sz(cv, "RegisteredOwner", "Tester")
        .dword(cv, "InstallDate", 1_092_955_707)
        .sz(
            "Microsoft\\Windows\\CurrentVersion\\Uninstall\\Tool",
            "DisplayName",
            "Tool",
        )
        .expand_sz(
            &format!("{cv}\\ProfileList\\S-1-5-21-11-22-33-1000"),
            "ProfileImagePath",
            "%SystemDrive%\\Documents and Settings\\bob",
        )
        .expand_sz(
            &format!("{cv}\\ProfileList\\S-1-5-21-77-88-99-1105"),
            "ProfileImagePath",
            "%SystemDrive%\\Documents and Settings\\carol",
        );
    let mut ntuser = HiveBuilder::new("NTUSER");
    ntuser.sz("Network\\Z", "RemotePath", "\\\\srv\\share");
    let lnk = LnkBuilder {
        created: ft(1_093_000_000),
        local_base_path: Some("C:\\Documents and Settings\\bob\\".into()),
        common_path_suffix: "notes.txt".into(),
        ..LnkBuilder::default()
    }
    .build();
    let pf = PrefetchBuilder {
        version: 17,
        executable: "NOTEPAD.EXE".into(),
        hash: 1,
        run_count: 3,
        run_times: vec![ft(1_093_500_000)],
        filenames: vec!["\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\SYSTEM32\\NOTEPAD.EXE".into()],
        volumes: vec![],
        v30_short: false,
    }
    .build();
    let mut dollar_i = 2u64.to_le_bytes().to_vec();
    dollar_i.extend(5u64.to_le_bytes());
    dollar_i.extend(ft(1_094_000_000).to_le_bytes());
    let orig: Vec<u8> = "C:\\secret.txt"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    dollar_i.extend(((orig.len() / 2) as u32).to_le_bytes());
    dollar_i.extend(orig);

    let mut b = NtfsBuilder::new(16);
    for d in [
        "/WINDOWS",
        "/WINDOWS/system32",
        "/WINDOWS/system32/config",
        "/WINDOWS/Prefetch",
        "/Documents and Settings",
        "/Documents and Settings/bob",
        "/Documents and Settings/bob/Recent",
        "/$Recycle.Bin",
        "/$Recycle.Bin/S-1-5-21-1-2-3-1000",
    ] {
        b.dir(d);
    }
    b.file("/WINDOWS/system32/config/SYSTEM", &system.build())
        .file("/WINDOWS/system32/config/SAM", &sam_hive())
        .file("/WINDOWS/system32/config/software", &software.build())
        .file("/WINDOWS/system32/notepad.exe", b"MZ notepad")
        .file("/WINDOWS/Prefetch/NOTEPAD.EXE-336351A9.pf", &pf)
        .file("/Documents and Settings/bob/NTUSER.DAT", &ntuser.build())
        .file("/Documents and Settings/bob/notes.txt", b"notes")
        .file("/Documents and Settings/bob/Recent/notes.txt.lnk", &lnk)
        .file("/$Recycle.Bin/S-1-5-21-1-2-3-1000/$IABC123.txt", &dollar_i)
        .file("/$Recycle.Bin/S-1-5-21-1-2-3-1000/$RABC123.txt", b"secret")
        .file_with(
            "/Documents and Settings/bob/setup.exe",
            b"MZ setup",
            FileOpts {
                ads: vec![(
                    "Zone.Identifier".into(),
                    b"[ZoneTransfer]\r\nZoneId=3\r\nHostUrl=https://dl.example.com/setup.exe\r\n"
                        .to_vec(),
                )],
                ..FileOpts::default()
            },
        );
    b.build()
}

fn run(case: &Arc<Case>, ds_id: i64) {
    let settings = JobSettings {
        modules: vec![ModuleSetting {
            id: "recent_activity".into(),
            enabled: true,
            settings: serde_json::Value::Null,
        }],
        ..JobSettings::default()
    };
    let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(RecentActivityFactory)];
    run_job(
        case,
        ds_id,
        &settings,
        &modules,
        &Arc::new(IngestProgress::default()),
        &Arc::new(Inbox::default()),
    )
    .unwrap();
}

/// `type name → [attribute name → display value]` for every artifact.
fn dump(case: &Case, ds_id: i64) -> BTreeMap<String, Vec<BTreeMap<String, String>>> {
    let db = case.db();
    let mut out: BTreeMap<String, Vec<BTreeMap<String, String>>> = BTreeMap::new();
    for (id, name, _, _) in db.artifact_type_names().unwrap() {
        for a in db.artifacts_of_type(id, Some(ds_id), 0, 10_000).unwrap() {
            let attrs = db
                .attributes(a.id)
                .unwrap()
                .into_iter()
                .map(|x| (x.name, x.value.display()))
                .collect();
            out.entry(name.clone()).or_default().push(attrs);
        }
    }
    out
}

#[test]
fn windows_artifacts_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("win.dd");
    std::fs::write(&img, image()).unwrap();
    let case = Arc::new(Case::create(dir.path().join("case"), CaseDetails::default()).unwrap());
    let s = add_image(
        &case,
        &img,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    run(&case, s.ds_id);
    let d = dump(&case, s.ds_id);
    let one = |ty: &str| -> &BTreeMap<String, String> {
        d.get(ty)
            .and_then(|v| v.first())
            .unwrap_or_else(|| panic!("no {ty}: {d:#?}"))
    };

    let os = one("TSK_OS_INFO");
    assert_eq!(os["TSK_PROG_NAME"], "Microsoft Windows XP Service Pack 2");
    assert_eq!(os["TSK_NAME"], "TESTPC");
    assert_eq!(os["TSK_OWNER"], "Tester");
    assert_eq!(os["TSK_DATETIME"], "1092955707");
    assert_eq!(os["PM_TIMEZONE"], "Central Standard Time");
    assert_eq!(os["PM_UTC_OFFSET"], "-360");

    let usb = one("TSK_DEVICE_ATTACHED");
    assert_eq!(usb["TSK_DEVICE_MAKE"], "SanDisk Corp.");
    assert_eq!(usb["TSK_DEVICE_ID"], "SERIAL123");
    assert_eq!(one("TSK_INSTALLED_PROG")["TSK_PROG_NAME"], "Tool");
    assert_eq!(one("TSK_REMOTE_DRIVE")["TSK_REMOTE_PATH"], "\\\\srv\\share");
    assert_eq!(
        one("TSK_DATA_SOURCE_USAGE")["TSK_DESCRIPTION"],
        "OS Drive (Microsoft Windows XP Service Pack 2)"
    );

    let recent = one("TSK_RECENT_OBJECT");
    assert_eq!(
        recent["TSK_PATH"],
        "C:\\Documents and Settings\\bob\\notes.txt"
    );
    assert_ne!(recent["TSK_PATH_ID"], "-1", "target resolved");

    let pf = one("TSK_PROG_RUN");
    assert_eq!(pf["TSK_PROG_NAME"], "NOTEPAD.EXE");
    assert_eq!(pf["TSK_PATH"], "/WINDOWS/SYSTEM32");
    assert_eq!(pf["TSK_DATETIME"], "1093500000");
    assert_eq!(pf["TSK_COUNT"], "3");
    assert_eq!(pf["TSK_COMMENT"], "Prefetch File");

    let rb = one("TSK_RECYCLE_BIN");
    assert_eq!(rb["TSK_PATH"], "C:\\secret.txt");
    assert_eq!(rb["TSK_DATETIME_DELETED"], "1094000000");
    assert!(!rb.contains_key("TSK_COMMENT"), "content file found");

    let dl = one("TSK_WEB_DOWNLOAD");
    assert_eq!(dl["TSK_URL"], "https://dl.example.com/setup.exe");
    assert_eq!(dl["TSK_DOMAIN"], "example.com");
    assert_eq!(dl["TSK_COMMENT"], "Internet Zone");

    // Associated objects link the LNK target, the executable and the download.
    assert!(
        d.get("TSK_ASSOCIATED_OBJECT").map_or(0, Vec::len) >= 3,
        "{d:#?}"
    );

    // SAM and ProfileList users are OS accounts, not artifacts (Autopsy
    // 4.19+).
    assert!(!d.contains_key("TSK_OS_ACCOUNT"));
    let db = case.db();
    let host = db.host_of_data_source(s.ds_id).unwrap().unwrap();
    let accounts = db.os_accounts(Some(s.ds_id), 0, 100).unwrap();
    let by_sid = |sid: &str| {
        accounts
            .iter()
            .find(|a| a.addr.as_deref() == Some(sid))
            .unwrap_or_else(|| panic!("no account {sid}: {accounts:#?}"))
    };
    let admin = by_sid("S-1-5-21-11-22-33-500");
    assert_eq!(admin.login_name.as_deref(), Some("Administrator"));
    assert_eq!(admin.full_name.as_deref(), Some("Admin Istrator"));
    assert_eq!(admin.created_at, Some(1_070_000_000));
    let realm = db.os_account_realm(admin.realm_id).unwrap();
    assert_eq!(realm.addr.as_deref(), Some("S-1-5-21-11-22-33"));
    assert_eq!(realm.scope_host_id, Some(host.id), "SAM accounts are local");
    let attrs: BTreeMap<String, String> = db
        .os_account_attributes(admin.id)
        .unwrap()
        .into_iter()
        .map(|(a, name, _)| (name, a.value.display()))
        .collect();
    assert_eq!(attrs["TSK_DATETIME_ACCESSED"], "1090000000");
    assert_eq!(attrs["TSK_COUNT"], "7");
    assert_eq!(attrs["TSK_IS_ADMIN"], "1");
    assert_eq!(attrs["TSK_DESCRIPTION"], "Built-in");
    assert_eq!(attrs["TSK_EMAIL"], "Admin@Example.com");
    assert_eq!(attrs["TSK_PASSWORD_SETTINGS"], "Password does not expire");
    assert_eq!(attrs["TSK_FLAG"], "Normal user account");
    assert_eq!(attrs["RA_SAM_QUESTION_1"], "First pet?");
    assert_eq!(attrs["RA_SAM_ANSWER_1"], "Rex");
    assert_eq!(
        db.account("EMAIL", "admin@example.com")
            .unwrap()
            .map(|a| a.identifier)
            .as_deref(),
        Some("admin@example.com"),
        "the SAM e-mail is a communication account"
    );
    let bob = by_sid("S-1-5-21-11-22-33-1000");
    assert_eq!(bob.login_name.as_deref(), Some("bob"));
    assert_eq!(bob.realm_id, admin.realm_id);
    let bob_attrs: BTreeMap<String, String> = db
        .os_account_attributes(bob.id)
        .unwrap()
        .into_iter()
        .map(|(a, name, _)| (name, a.value.display()))
        .collect();
    assert_eq!(bob_attrs["TSK_HOME_DIR"], "/Documents and Settings/bob");
    assert_eq!(bob_attrs["TSK_GROUPS"], "Administrators");
    let carol = by_sid("S-1-5-21-77-88-99-1105");
    assert_eq!(carol.login_name.as_deref(), Some("carol"));
    let carol_realm = db.os_account_realm(carol.realm_id).unwrap();
    assert_eq!(
        carol_realm.scope_host_id, None,
        "ProfileList-only users are domain users"
    );
    assert_eq!(carol_realm.name.as_deref(), Some("CORP"));
    for a in [admin, bob, carol] {
        assert_eq!(
            db.os_account_instances(a.id).unwrap()[0].kind,
            pm_casedb::os_accounts::InstanceKind::Launched
        );
    }

    // Re-running ingest adds nothing.
    let count = |d: &BTreeMap<String, Vec<BTreeMap<String, String>>>| {
        d.values().map(Vec::len).sum::<usize>()
    };
    let before = count(&d);
    let attrs_before = db.os_account_attributes(admin.id).unwrap().len();
    run(&case, s.ds_id);
    assert_eq!(count(&dump(&case, s.ds_id)), before);
    assert_eq!(db.os_accounts(None, 0, 100).unwrap().len(), accounts.len());
    assert_eq!(
        db.os_account_attributes(admin.id).unwrap().len(),
        attrs_before
    );
    assert!(artifact_type_id("TSK_OS_INFO").is_some());
}
