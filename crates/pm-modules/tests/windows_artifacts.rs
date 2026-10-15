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

    // Re-running ingest adds nothing.
    let count = |d: &BTreeMap<String, Vec<BTreeMap<String, String>>>| {
        d.values().map(Vec::len).sum::<usize>()
    };
    let before = count(&d);
    run(&case, s.ds_id);
    assert_eq!(count(&dump(&case, s.ds_id)), before);
    assert!(artifact_type_id("TSK_OS_INFO").is_some());
}
