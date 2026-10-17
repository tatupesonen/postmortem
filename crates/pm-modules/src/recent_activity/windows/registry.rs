//! Windows Registry analyzer (Autopsy `ExtractRegistry`): reads SAM,
//! SOFTWARE, SYSTEM, NTUSER.DAT and UsrClass.dat natively and posts what
//! Autopsy posts from its RegRipper runs, plus the extended OS overview and
//! execution artefacts listed in `docs/agents/windows-artifacts.md`.

use std::collections::{BTreeMap, HashMap};

use pm_casedb::catalog::ValueType;
use pm_casedb::{FileRecord, Value};
use pm_ingest::{JobContext, ModuleError};
use pm_registry::Hive;
use pm_winartifacts::registry::{ntuser, sam, shellbags, shimcache, software, system};
use pm_winartifacts::usb_ids;

use super::accounts::{self, Account};
use super::art::{Batch, NewArt, Types, ds_artifact_exists};
use super::{PathLookup, Visits, files_with_ext, open_hive, user_of};

const BAM_COMMENT: &str = "Recently Executed according to Background Activity Moderator (BAM)";
const DAM_COMMENT: &str = "Recently Executed according to Desktop Activity Moderator (DAM)";
const USERASSIST_COMMENT: &str = "Launched from Explorer according to UserAssist";
const RUNMRU_COMMENT: &str = "Typed in the Run dialog (RunMRU)";
const TYPED_PATHS_COMMENT: &str = "Typed in the Explorer address bar (TypedPaths)";
const WSL_COMMENT: &str = "Windows Subsystem For Linux";

/// Hive files of one kind (`sam`, `system`, `software`, `security`,
/// `ntuser.dat`, `usrclass.dat`), as Autopsy's `findRegistryFiles` finds
/// them.
pub(crate) fn hive_files(ctx: &JobContext, which: &str) -> Result<Vec<FileRecord>, ModuleError> {
    let files = if which.contains('.') {
        files_with_ext(ctx, "dat", Some(which), "")?
    } else {
        files_with_ext(ctx, "", Some(which), "/system32/config")?
    };
    Ok(files
        .into_iter()
        .filter(|f| f.stream.is_none() && f.size > 0)
        .collect())
}

/// Autopsy's `OSInfo` accumulator, extended.
#[derive(Debug, Default)]
struct OsInfo {
    winver: Option<software::WindowsVersion>,
    system: Option<system::SystemInfo>,
    logon: Option<software::LogonInfo>,
    defender: Option<software::DefenderInfo>,
    browsers: Vec<String>,
}

pub(crate) fn extract(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let mut types = Types::default();
    let mut batch = Batch::default();
    let mut paths = PathLookup::default();
    let mut os = OsInfo::default();
    let mut accounts: BTreeMap<String, Account> = BTreeMap::new();
    let mut machine_sids: Vec<String> = Vec::new();
    let mut found_any = false;

    // SAM: local accounts.
    for f in hive_files(ctx, "sam")? {
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        found_any = true;
        let info = sam::sam(&h);
        if let Some(m) = &info.machine_sid {
            machine_sids.push(m.clone());
        }
        for u in &info.users {
            let Some(sid) = u.sid.clone() else { continue };
            let groups = info
                .groups_of(&sid)
                .into_iter()
                .map(str::to_owned)
                .collect();
            accounts.insert(
                sid,
                Account {
                    file_id: f.id,
                    login: u.name.clone(),
                    full_name: u.full_name.clone().or_else(|| u.given_surname.clone()),
                    sam: Some(u.clone()),
                    groups,
                    ..Account::default()
                },
            );
        }
    }

    // SOFTWARE: OS version, programs, profiles, networks, logon, Defender.
    let mut domain_name: Option<String> = None;
    for f in hive_files(ctx, "software")? {
        if ctx.is_cancelled() {
            return Ok(());
        }
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        found_any = true;
        let v = software::windows_version(&h);
        if os.winver.is_none() && v.product_name.is_some() {
            os.winver = Some(v);
        }
        for p in software::installed_programs(&h) {
            batch.push(
                NewArt::std(ctx, "TSK_INSTALLED_PROG", Some(f.id))
                    .text("TSK_PROG_NAME", p.name)
                    .time("TSK_DATETIME", p.last_written)
                    .key(&["TSK_PROG_NAME", "TSK_DATETIME"]),
            );
        }
        for p in software::profile_list(&h) {
            let local = machine_sids
                .iter()
                .any(|m| p.sid.starts_with(&format!("{m}-")));
            let home = p.path.as_deref().map(home_dir);
            let acc = accounts.entry(p.sid.clone()).or_insert_with(|| Account {
                file_id: f.id,
                ..Account::default()
            });
            if acc.login.is_empty() {
                acc.login.clone_from(&p.user);
            }
            if !local {
                acc.domain = true;
            }
            acc.home_dir = home.or(acc.home_dir.take());
        }
        post_network_profiles(ctx, &mut types, &mut batch, &h, f.id);
        let l = software::logon_info(&h);
        if os.logon.is_none() && l != software::LogonInfo::default() {
            os.logon = Some(l);
        }
        let d = software::defender(&h);
        let excl = types.artifact(ctx, "PM_DEFENDER_EXCLUSION", "Defender Exclusions");
        for (kind, value) in &d.exclusions {
            batch.push(
                NewArt::new(ctx, excl, Some(f.id))
                    .text("TSK_CATEGORY", kind.clone())
                    .text("TSK_VALUE", value.clone())
                    .key(&["TSK_CATEGORY", "TSK_VALUE"]),
            );
        }
        if os.defender.is_none() && d != software::DefenderInfo::default() {
            os.defender = Some(d);
        }
        if let Some(b) = software::default_browser(&h) {
            os.browsers.push(format!("System: {b}"));
        }
    }

    let user_names: HashMap<String, String> = accounts
        .iter()
        .filter(|(_, a)| !a.login.is_empty())
        .map(|(s, a)| (s.clone(), a.login.clone()))
        .collect();

    // SYSTEM: machine settings, devices, networks, execution.
    for f in hive_files(ctx, "system")? {
        if ctx.is_cancelled() {
            return Ok(());
        }
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        found_any = true;
        let s = system::system_info(&h);
        if domain_name.is_none() {
            domain_name.clone_from(&s.domain);
        }
        if os.system.is_none() {
            os.system = Some(s);
        }
        system_artifacts(
            ctx,
            &mut types,
            &mut batch,
            &mut paths,
            &h,
            f.id,
            &user_names,
        );
    }

    // NTUSER.DAT / UsrClass.dat: per-user activity.
    for f in hive_files(ctx, "ntuser.dat")? {
        if ctx.is_cancelled() {
            return Ok(());
        }
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        found_any = true;
        let user = user_of(&f.path());
        ntuser_artifacts(ctx, &mut types, &mut batch, &mut paths, &h, f.id, &user);
        post_shellbags(
            ctx,
            &mut types,
            &mut batch,
            &h,
            f.id,
            shellbags::NTUSER_ROOTS,
        );
        if let Some(b) = ntuser::default_browser(&h) {
            os.browsers.push(format!(
                "{}: {b}",
                if user.is_empty() { "user" } else { &user }
            ));
        }
    }
    for f in hive_files(ctx, "usrclass.dat")? {
        if ctx.is_cancelled() {
            return Ok(());
        }
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        found_any = true;
        post_shellbags(
            ctx,
            &mut types,
            &mut batch,
            &h,
            f.id,
            shellbags::USRCLASS_ROOTS,
        );
    }

    // OS accounts (not artifacts, as in Autopsy 4.19+).
    let n_accounts = accounts::record(ctx, &mut types, &accounts, domain_name.as_deref())?;
    tracing::debug!(accounts = n_accounts, "OS accounts recorded");

    if found_any {
        post_os_info(ctx, &mut types, &mut batch, &os);
    }
    let n = batch.flush(ctx)?;
    tracing::debug!(artifacts = n, "registry analysis");
    Ok(())
}

/// `%SystemDrive%\Documents and Settings\x` → `/Documents and Settings/x`.
fn home_dir(p: &str) -> String {
    let s = if p.starts_with('%') {
        p.get(1..)
            .and_then(|r| r.find('%').map(|i| r.get(i + 1..).unwrap_or_default()))
            .unwrap_or(p)
    } else {
        p
    };
    s.replace('\\', "/")
}

#[allow(
    clippy::too_many_arguments,
    reason = "extractor state threaded explicitly"
)]
fn system_artifacts(
    ctx: &JobContext,
    types: &mut Types,
    batch: &mut Batch,
    paths: &mut PathLookup,
    h: &Hive,
    file_id: i64,
    users: &HashMap<String, String>,
) {
    for d in system::usb_devices(h) {
        let (make, model) = if d.device.to_lowercase().contains("vid") {
            usb_ids::lookup(&d.device)
                .map_or((String::new(), d.device.clone()), |i| (i.vendor, i.product))
        } else {
            (String::new(), d.device.clone())
        };
        batch.push(
            NewArt::std(ctx, "TSK_DEVICE_ATTACHED", Some(file_id))
                .time("TSK_DATETIME", d.last_written.filter(|&t| t > 0))
                .text("TSK_DEVICE_MAKE", make)
                .text("TSK_DEVICE_MODEL", model)
                .text("TSK_DEVICE_ID", d.serial)
                .key(&["TSK_DEVICE_ID", "TSK_DEVICE_MODEL"]),
        );
    }
    for s in system::interface_ssids(h) {
        batch.push(
            NewArt::std(ctx, "TSK_WIFI_NETWORK", Some(file_id))
                .text("TSK_SSID", s.ssid)
                .time("TSK_DATETIME", s.last_written)
                .text("TSK_DEVICE_ID", s.adapter)
                .key(&["TSK_SSID", "TSK_DEVICE_ID"]),
        );
    }
    for b in system::bam(h) {
        let user = users.get(&b.sid).cloned().unwrap_or_else(|| b.sid.clone());
        let target = paths.find(ctx, &b.path);
        batch.push(
            NewArt::std(ctx, "TSK_PROG_RUN", Some(file_id))
                .text("TSK_PROG_NAME", b.path)
                .text("TSK_USER_NAME", user)
                .time("TSK_DATETIME", Some(b.last_run))
                .text(
                    "TSK_COMMENT",
                    if b.source == "BAM" {
                        BAM_COMMENT
                    } else {
                        DAM_COMMENT
                    },
                )
                .key(&["TSK_PROG_NAME", "TSK_DATETIME", "TSK_COMMENT"])
                .associate(target),
        );
    }
    for d in system::bluetooth_devices(h) {
        batch.push(
            NewArt::std(ctx, "TSK_BLUETOOTH_PAIRING", Some(file_id))
                .text("TSK_DEVICE_ID", d.address)
                .text_opt("TSK_NAME", d.name)
                .time("TSK_DATETIME", d.last_seen)
                .time("TSK_DATETIME_ACCESSED", d.last_connected)
                .key(&["TSK_DEVICE_ID"]),
        );
    }
    let shim = types.artifact(ctx, "PM_SHIMCACHE", "AppCompatCache (ShimCache)");
    let pos = types.attribute(
        ctx,
        "PM_CACHE_POSITION",
        "Cache Position",
        ValueType::Integer,
    );
    for e in shimcache::shimcache(h) {
        let target = paths.find(ctx, &e.path);
        batch.push(
            NewArt::new(ctx, shim, Some(file_id))
                .text("TSK_PATH", e.path)
                .time("TSK_DATETIME_MODIFIED", e.last_modified)
                .text_opt(
                    "TSK_FLAG",
                    e.executed
                        .map(|x| if x { "Executed" } else { "Not executed" }),
                )
                .custom(pos, Some(Value::Int(e.position as i64)))
                .key(&["TSK_PATH", "TSK_DATETIME_MODIFIED"])
                .associate(target),
        );
    }
    let nic = types.artifact(ctx, "PM_NETWORK_INTERFACE", "Network Interfaces");
    let dhcp = types.attribute(ctx, "PM_DHCP_SERVER", "DHCP Server", ValueType::String);
    let gw = types.attribute(ctx, "PM_GATEWAY", "Default Gateway", ValueType::String);
    let dns = types.attribute(ctx, "PM_DNS_SERVERS", "DNS Servers", ValueType::String);
    let mask = types.attribute(ctx, "PM_SUBNET_MASK", "Subnet Mask", ValueType::String);
    for n in system::network_interfaces(h) {
        let ip = if n.ip_addresses.is_empty() {
            n.dhcp_ip.clone()
        } else {
            Some(n.ip_addresses.join(", "))
        };
        if ip.is_none() && n.connection_name.is_none() {
            continue;
        }
        let gateways = if n.gateways.is_empty() {
            &n.dhcp_gateways
        } else {
            &n.gateways
        };
        let dns_text = if n.dns_servers.is_empty() {
            n.dhcp_dns.clone()
        } else {
            Some(n.dns_servers.join(", "))
        };
        let txt = |s: Option<String>| s.filter(|x| !x.is_empty()).map(Value::Text);
        batch.push(
            NewArt::new(ctx, nic, Some(file_id))
                .text("TSK_DEVICE_ID", n.guid)
                .text_opt("TSK_NAME", n.connection_name)
                .text_opt("TSK_IP_ADDRESS", ip)
                .text_opt("TSK_DOMAIN", n.domain.or(n.dhcp_domain))
                .text(
                    "TSK_FLAG",
                    if n.dhcp_enabled == Some(true) {
                        "DHCP"
                    } else {
                        "Static"
                    },
                )
                .custom(dhcp, txt(n.dhcp_server))
                .custom(gw, txt(Some(gateways.join(", "))))
                .custom(dns, txt(dns_text))
                .custom(
                    mask,
                    txt(n.dhcp_subnet.or_else(|| n.subnet_masks.first().cloned())),
                )
                .time("TSK_DATETIME_START", n.lease_obtained)
                .time("TSK_DATETIME_END", n.lease_expires)
                .time("TSK_DATETIME", n.last_written)
                .key(&["TSK_DEVICE_ID"]),
        );
    }
}

fn post_network_profiles(
    ctx: &JobContext,
    types: &mut Types,
    batch: &mut Batch,
    h: &Hive,
    file_id: i64,
) {
    let prof = types.artifact(ctx, "PM_NETWORK_PROFILE", "Network Profiles");
    for p in software::network_profiles(h) {
        if p.name_type == Some(71) {
            batch.push(
                NewArt::std(ctx, "TSK_WIFI_NETWORK", Some(file_id))
                    .text("TSK_SSID", p.name.clone())
                    .time("TSK_DATETIME", p.last_connected)
                    .text("TSK_DEVICE_ID", p.guid.clone())
                    .key(&["TSK_SSID", "TSK_DEVICE_ID"]),
            );
        }
        batch.push(
            NewArt::new(ctx, prof, Some(file_id))
                .text("TSK_ID", p.guid.clone())
                .text("TSK_NAME", p.name.clone())
                .text_opt("TSK_DESCRIPTION", p.description.clone())
                .text("TSK_CATEGORY", p.type_name())
                .text_opt(
                    "TSK_FLAG",
                    p.managed.map(|m| if m { "Managed" } else { "Unmanaged" }),
                )
                .time("TSK_DATETIME_CREATED", p.first_connected)
                .time("TSK_DATETIME_ACCESSED", p.last_connected)
                .text_opt("TSK_MAC_ADDRESS", p.gateway_mac.clone())
                .text_opt("TSK_DOMAIN", p.dns_suffix.clone())
                .key(&["TSK_ID"]),
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "extractor state threaded explicitly"
)]
fn ntuser_artifacts(
    ctx: &JobContext,
    types: &mut Types,
    batch: &mut Batch,
    paths: &mut PathLookup,
    h: &Hive,
    file_id: i64,
    user: &str,
) {
    for d in ntuser::network_drives(h) {
        batch.push(
            NewArt::std(ctx, "TSK_REMOTE_DRIVE", Some(file_id))
                .text("TSK_LOCAL_PATH", d.local_path)
                .text("TSK_REMOTE_PATH", d.remote_path)
                .key(&["TSK_LOCAL_PATH", "TSK_REMOTE_PATH"]),
        );
    }
    for o in ntuser::office_legacy(h) {
        batch.push(
            NewArt::std(ctx, "TSK_RECENT_OBJECT", Some(file_id))
                .time("TSK_DATETIME_ACCESSED", o.mtime)
                .text("TSK_NAME", o.name)
                .text("TSK_VALUE", o.value)
                .text("TSK_PROG_NAME", o.program)
                .key(&["TSK_NAME", "TSK_VALUE", "TSK_PROG_NAME"]),
        );
    }
    for r in ntuser::recent_files(h) {
        let target = paths.find(ctx, &r.path.replace('\0', ""));
        batch.push(
            NewArt::std(ctx, "TSK_RECENT_OBJECT", Some(file_id))
                .text("TSK_PATH", r.path)
                .time("TSK_DATETIME_ACCESSED", r.accessed)
                .text("TSK_COMMENT", r.comment)
                .key(&["TSK_PATH", "TSK_COMMENT"])
                .associate(target),
        );
    }
    for p in ntuser::typed_paths(h) {
        batch.push(
            NewArt::std(ctx, "TSK_RECENT_OBJECT", Some(file_id))
                .text("TSK_PATH", p)
                .text("TSK_COMMENT", TYPED_PATHS_COMMENT)
                .key(&["TSK_PATH", "TSK_COMMENT"]),
        );
    }
    let wsl = types.artifact(ctx, "WSL_NAME", "Windows Subsystem For Linux");
    let kc = types.attribute(
        ctx,
        "WSL_KERNAL_COMMAND",
        "Kernal Command",
        ValueType::String,
    );
    for d in ntuser::wsl(h) {
        batch.push(
            NewArt::new(ctx, wsl, Some(file_id))
                .text("TSK_NAME", d.name)
                .text("TSK_PATH", d.image_path)
                .time("TSK_DATETIME_ACCESSED", d.last_written)
                .custom(kc, Some(Value::Text(d.kernel_command)))
                .text("TSK_COMMENT", WSL_COMMENT)
                .key(&["TSK_NAME", "TSK_PATH"]),
        );
    }
    for u in ntuser::user_assist(h) {
        let target = paths.find(ctx, &u.name);
        batch.push(
            NewArt::std(ctx, "TSK_PROG_RUN", Some(file_id))
                .text("TSK_PROG_NAME", u.name)
                .text_opt("TSK_USER_NAME", Some(user.to_owned()))
                .time("TSK_DATETIME", u.last_run)
                .int("TSK_COUNT", i64::from(u.run_count))
                .text("TSK_COMMENT", USERASSIST_COMMENT)
                .key(&["TSK_PROG_NAME", "TSK_COMMENT"])
                .associate(target),
        );
    }
    let run_time = pm_winartifacts::registry::ntuser::run_mru(h)
        .first()
        .and(
            h.key("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\RunMRU")
                .ok()
                .flatten(),
        )
        .and_then(|k| k.last_written_unix());
    for (i, cmd) in ntuser::run_mru(h).into_iter().enumerate() {
        batch.push(
            NewArt::std(ctx, "TSK_PROG_RUN", Some(file_id))
                .text("TSK_PROG_NAME", cmd)
                .text_opt("TSK_USER_NAME", Some(user.to_owned()))
                .time("TSK_DATETIME", if i == 0 { run_time } else { None })
                .text("TSK_COMMENT", RUNMRU_COMMENT)
                .key(&["TSK_PROG_NAME", "TSK_COMMENT"]),
        );
    }
    for (url, t) in ntuser::typed_urls(h) {
        batch.push(
            NewArt::std(ctx, "TSK_WEB_HISTORY", Some(file_id))
                .text("TSK_URL", url.clone())
                .time("TSK_DATETIME_ACCESSED", t)
                .text("TSK_PROG_NAME", "Internet Explorer")
                .text("TSK_DOMAIN", crate::recent_activity::extract_domain(&url))
                .text_opt("TSK_USER_NAME", Some(user.to_owned()))
                .text("TSK_COMMENT", "Typed URL")
                .key(&["TSK_URL", "TSK_COMMENT"]),
        );
    }
}

fn post_shellbags(
    ctx: &JobContext,
    types: &mut Types,
    batch: &mut Batch,
    h: &Hive,
    file_id: i64,
    roots: &[&str],
) {
    let bag = types.artifact(ctx, "RA_SHELL_BAG", "Shell Bags");
    let key_attr = types.attribute(ctx, "RA_SHELL_BAG_KEY", "Key", ValueType::String);
    let lw = types.attribute(
        ctx,
        "RA_SHELL_BAG_LAST_WRITE",
        "Last Write",
        ValueType::DateTime,
    );
    for b in shellbags::shellbags(h, roots) {
        batch.push(
            NewArt::new(ctx, bag, Some(file_id))
                .text("TSK_PATH", b.path)
                .custom(key_attr, Some(Value::Text(b.key)))
                .custom(lw, b.last_written.map(Value::DateTime))
                .time("TSK_DATETIME_MODIFIED", b.modified)
                .time("TSK_DATETIME_CREATED", b.created)
                .time("TSK_DATETIME_ACCESSED", b.accessed)
                .key(&["TSK_PATH"])
                .key_ids(&[key_attr]),
        );
    }
}

/// `TSK_OS_INFO` on the data source (only if it has none), with
/// Autopsy's attributes and the extended overview (`PM_*` attributes).
#[allow(clippy::many_single_char_names, reason = "short local names")]
fn post_os_info(ctx: &JobContext, types: &mut Types, batch: &mut Batch, os: &OsInfo) {
    let os_type = pm_casedb::catalog::artifact_type_id("TSK_OS_INFO").unwrap_or(0);
    if ds_artifact_exists(ctx, os_type, None) {
        return;
    }
    let w = os.winver.clone().unwrap_or_default();
    let s = os.system.clone().unwrap_or_default();
    // Autopsy: the last non-empty OS name wins (SOFTWARE is read after
    // SYSTEM), "Windows" by default.
    let prog = w
        .os_name()
        .or_else(|| s.os_env.clone())
        .unwrap_or_else(|| "Windows".to_owned());
    let mut a = NewArt::new(ctx, os_type, None)
        .text_opt("TSK_NAME", s.computer_name.clone())
        .text_opt("TSK_DOMAIN", s.domain.clone())
        .text("TSK_PROG_NAME", prog)
        .text_opt(
            "TSK_PROCESSOR_ARCHITECTURE",
            s.processor_architecture.clone(),
        )
        .text_opt("TSK_TEMP_DIR", s.temp_dir.clone())
        .time("TSK_DATETIME", w.install_date)
        .text_opt("TSK_PATH", w.system_root.clone())
        .text_opt("TSK_PRODUCT_ID", w.product_id.clone())
        .text_opt("TSK_OWNER", w.registered_owner.clone())
        .text_opt("TSK_ORGANIZATION", w.registered_organization.clone())
        .text_opt("TSK_VERSION", w.current_version.clone())
        .text_opt("TSK_HOST", s.hostname.clone());
    let mut add = |name: &str, display: &str, v: Option<Value>| {
        let vt = match v {
            Some(Value::DateTime(_)) => ValueType::DateTime,
            Some(Value::Int(_)) => ValueType::Integer,
            _ => ValueType::String,
        };
        let id = types.attribute(ctx, name, display, vt);
        a = std::mem::replace(&mut a, NewArt::new(ctx, 0, None)).custom(id, v);
    };
    let t = |s: Option<String>| s.filter(|x| !x.is_empty()).map(Value::Text);
    if let Some(tz) = &s.time_zone {
        add("PM_TIMEZONE", "Time Zone", t(tz.name().map(str::to_owned)));
        add(
            "PM_UTC_OFFSET",
            "UTC Offset (minutes)",
            tz.utc_offset_minutes().map(|m| Value::Int(i64::from(m))),
        );
        add(
            "PM_ACTIVE_UTC_OFFSET",
            "Active UTC Offset (minutes)",
            tz.active_bias.map(|b| Value::Int(-i64::from(b))),
        );
        add(
            "PM_DAYLIGHT_NAME",
            "Daylight Time Name",
            t(tz.daylight_name.clone()),
        );
        let rules = match (&tz.standard_start, &tz.daylight_start) {
            (Some(st), Some(dl)) => Some(format!("standard from {st}; daylight from {dl}")),
            _ => None,
        };
        add("PM_DST_RULES", "Daylight Saving Rules", t(rules));
        add(
            "PM_DYNAMIC_DST_DISABLED",
            "Automatic DST Adjustment",
            tz.dynamic_dst_disabled
                .map(|d| Value::Text(if d { "Disabled" } else { "Enabled" }.into())),
        );
    }
    add(
        "PM_LAST_SHUTDOWN",
        "Last Shutdown",
        s.last_shutdown.map(Value::DateTime),
    );
    add("PM_EDITION", "Edition", t(w.edition_id.clone()));
    add("PM_BUILD", "Build", t(w.build()));
    add(
        "PM_DISPLAY_VERSION",
        "Display Version",
        t(w.display_version.clone()),
    );
    add(
        "PM_INSTALLATION_TYPE",
        "Installation Type",
        t(w.installation_type.clone()),
    );
    add("PM_BUILD_LAB", "Build Lab", t(w.build_lab_ex.clone()));
    add("PM_PRODUCT_KEY", "Product Key", t(w.product_key.clone()));
    add("PM_NV_DOMAIN", "Domain (NV)", t(s.nv_domain.clone()));
    add(
        "PM_PROCESSOR_IDENTIFIER",
        "Processor",
        t(s.processor_identifier.clone()),
    );
    add(
        "PM_PROCESSOR_COUNT",
        "Number of Processors",
        t(s.number_of_processors.clone()),
    );
    add(
        "PM_PAGE_FILES",
        "Page Files",
        t(Some(s.page_files.join("; "))),
    );
    add(
        "PM_CLEAR_PAGEFILE",
        "Clear Page File at Shutdown",
        s.clear_page_file_at_shutdown
            .map(|b| Value::Text(yes_no(b).into())),
    );
    add(
        "PM_HIBERNATION",
        "Hibernation",
        s.hibernate_enabled
            .map(|b| Value::Text(if b { "Enabled" } else { "Disabled" }.into())),
    );
    add(
        "PM_CRASH_DUMP",
        "Crash Dump",
        s.crash_dump_enabled.map(|c| {
            let kind = match c {
                0 => "None",
                1 => "Complete memory dump",
                2 => "Kernel memory dump",
                3 => "Small memory dump",
                7 => "Automatic memory dump",
                _ => "Other",
            };
            Value::Text(match &s.crash_dump_file {
                Some(f) => format!("{kind} ({f})"),
                None => kind.to_owned(),
            })
        }),
    );
    add(
        "PM_BITLOCKER",
        "BitLocker Boot Status",
        s.bitlocker_boot_status.map(|b| Value::Int(i64::from(b))),
    );
    let upgrades: Vec<String> = s
        .upgrades
        .iter()
        .map(|u| {
            format!(
                "{} → {}",
                u.key_name,
                [
                    u.product_name.clone(),
                    u.display_version.clone(),
                    u.build.clone()
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
            )
        })
        .collect();
    add(
        "PM_UPGRADE_HISTORY",
        "Upgrade History",
        t(Some(upgrades.join("; "))),
    );
    if let Some(l) = &os.logon {
        add(
            "PM_LAST_LOGGED_ON_USER",
            "Last Logged-on User",
            t(l.last_logged_on_user.clone()),
        );
        add(
            "PM_LAST_LOGGED_ON_SAM_USER",
            "Last Logged-on SAM User",
            t(l.last_logged_on_sam_user.clone()),
        );
        if l.auto_admin_logon {
            let who = [l.default_domain.clone(), l.default_user_name.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\\");
            let pw = if l.default_password.is_some() {
                "; password stored"
            } else {
                ""
            };
            add(
                "PM_AUTOLOGON",
                "Auto Logon",
                Some(Value::Text(format!("Enabled ({who}{pw})"))),
            );
        }
        add(
            "PM_AUTOLOGON_PASSWORD",
            "Auto Logon Password",
            t(l.default_password.clone()),
        );
    }
    if let Some(d) = &os.defender {
        let mut parts = Vec::new();
        if let Some(x) = d.anti_spyware_disabled {
            parts.push(format!(
                "anti-spyware {}",
                if x { "disabled" } else { "enabled" }
            ));
        }
        if let Some(x) = d.realtime_monitoring_disabled {
            parts.push(format!(
                "real-time protection {}",
                if x { "disabled" } else { "enabled" }
            ));
        }
        if !d.exclusions.is_empty() {
            parts.push(format!("{} exclusions", d.exclusions.len()));
        }
        add(
            "PM_DEFENDER",
            "Microsoft Defender",
            t(Some(parts.join(", "))),
        );
    }
    add(
        "PM_DEFAULT_BROWSER",
        "Default Browser",
        t(Some(os.browsers.join("; "))),
    );
    batch.push(a);
}

fn yes_no(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dirs() {
        assert_eq!(
            home_dir("%SystemDrive%\\Documents and Settings\\Mr. Evil"),
            "/Documents and Settings/Mr. Evil"
        );
        assert_eq!(home_dir("C:\\Users\\x"), "C:/Users/x");
    }
}
