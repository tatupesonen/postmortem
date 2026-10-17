//! OS detection (Autopsy `ExtractOs`) and data source usage (Autopsy
//! `DataSourceUsageAnalyzer`).

use pm_casedb::catalog::artifact_type_id;
use pm_ingest::{JobContext, ModuleError};

use super::Visits;
use super::art::{Batch, NewArt, ds_artifact_exists};

/// `(marker paths, KAAMOS_OS_INFO label, usage label)`; Windows has no label
/// here (the registry analyzer reports it).
const OS_TYPES: &[(&[&str], &str, &str)] = &[
    (&["/windows/system32"], "", "OS Drive (Windows)"),
    (
        &["/System/Library/CoreServices/SystemVersion.plist"],
        "Mac OS X",
        "OS Drive (OS X)",
    ),
    (
        &["/data/com.android.providers.settings/databases/settings.db"],
        "Android",
        "OS Drive (Android)",
    ),
    (
        &["/etc/redhat-release", "/etc/redhat_version"],
        "Linux (Redhat)",
        "OS Drive (Linux Redhat)",
    ),
    (
        &["/etc/SUSE-release"],
        "Linux (Novell SUSE)",
        "OS Drive (Linux Novell SUSE)",
    ),
    (
        &["/etc/fedora-release"],
        "Linux (Fedora)",
        "OS Drive (Linux Fedora)",
    ),
    (
        &["/etc/slackware-release", "/etc/slackware-version"],
        "Linux (Slackware)",
        "OS Drive (Linux Slackware)",
    ),
    (
        &["/etc/debian_release", "/etc/debian_version"],
        "Linux (Debian)",
        "OS Drive (Linux Debian)",
    ),
    (
        &["/etc/mandrake-release"],
        "Linux (Mandrake)",
        "OS Drive (Linux Mandrake)",
    ),
    (
        &["/etc/yellowdog-release"],
        "Linux (Yellow Dog)",
        "OS Drive (Linux Yellow Dog)",
    ),
    (
        &["/etc/sun-release"],
        "Linux (Sun JDS)",
        "OS Drive (Linux Sun JDS)",
    ),
    (
        &["/etc/release"],
        "Linux (Solaris/Sparc)",
        "OS Drive (Linux Solaris/Sparc)",
    ),
    (
        &["/etc/gentoo-release"],
        "Linux (Gentoo)",
        "OS Drive (Linux Gentoo)",
    ),
    (
        &["/etc/UnitedLinux-release"],
        "Linux (United Linux)",
        "OS Drive (Linux United Linux)",
    ),
    (
        &["/etc/lsb-release"],
        "Linux (Ubuntu)",
        "OS Drive (Linux Ubuntu)",
    ),
];

const HUNDRED_GIB: u64 = 100 << 30;
const ANDROID_ROOTS: &[&str] = &[
    ".android_secure",
    "android",
    "audio",
    "photos",
    "dcim",
    "music",
    "pictures",
    "videos",
];

fn first_found(ctx: &JobContext, paths: &[&str]) -> Option<i64> {
    paths.iter().find_map(|p| {
        ctx.case
            .db()
            .resolve_path(ctx.ds_id, p)
            .ok()
            .and_then(|v| v.first().map(|f| f.id))
    })
}

/// `KAAMOS_OS_INFO` on the first marker file of each non-Windows OS.
pub(crate) fn extract_os(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let os_type = artifact_type_id("KAAMOS_OS_INFO").unwrap_or(0);
    let mut batch = Batch::default();
    for (paths, label, _) in OS_TYPES {
        if label.is_empty() {
            continue;
        }
        let Some(file) = first_found(ctx, paths) else {
            continue;
        };
        let has = ctx
            .case
            .db()
            .artifacts_for_file(file)
            .is_ok_and(|a| a.iter().any(|a| a.type_id == os_type));
        if !has {
            batch.push(
                NewArt::new(ctx, os_type, Some(file))
                    .text("KAAMOS_PROG_NAME", *label)
                    .key(&["KAAMOS_PROG_NAME"]),
            );
        }
    }
    batch.flush(ctx)?;
    Ok(())
}

/// `KAAMOS_DATA_SOURCE_USAGE` results (deduplicated by description).
pub(crate) fn extract_usage(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let db = ctx.case.db();
    let usage = artifact_type_id("KAAMOS_DATA_SOURCE_USAGE").unwrap_or(0);
    let os_type = artifact_type_id("KAAMOS_OS_INFO").unwrap_or(0);
    let mut labels: Vec<String> = Vec::new();
    let mut windows = false;
    for a in db.artifacts_of_type(os_type, Some(ctx.ds_id), 0, 10_000)? {
        let Some(name) = db
            .attributes(a.id)?
            .into_iter()
            .find(|x| x.name == "KAAMOS_PROG_NAME")
            .map(|x| x.value.display())
        else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        if name.to_lowercase().contains("windows") {
            windows = true;
            labels.push(format!("OS Drive ({name})"));
        } else if let Some((_, _, u)) = OS_TYPES.iter().find(|(_, l, _)| *l == name) {
            labels.push((*u).to_owned());
        } else {
            labels.push(format!("OS Drive ({name})"));
        }
    }
    if !windows && first_found(ctx, &["/windows/system32"]).is_some() {
        labels.push("OS Drive (Windows)".to_owned());
    }
    // Flash drive / Android media card: ≤ 100 GiB, exactly one FAT/exFAT.
    let ds = db.data_source(ctx.ds_id)?;
    let fss = db.file_systems(ctx.ds_id)?;
    if ds.is_some_and(|d| d.size <= HUNDRED_GIB)
        && fss.len() == 1
        && fss.first().is_some_and(|f| {
            let k = f.kind.to_lowercase();
            k.contains("fat")
        })
        && let Some(fs) = fss.first()
        && let Some(root) = db.fs_root(fs.id)?
    {
        let android = db
            .children(root.id)?
            .iter()
            .any(|c| ANDROID_ROOTS.iter().any(|r| c.name.eq_ignore_ascii_case(r)));
        labels.push(
            if android {
                "Android Media Card"
            } else {
                "Flash Drive"
            }
            .to_owned(),
        );
    }
    if !super::files_with_ext(ctx, "dat", Some("FLY___.DAT"), "")?.is_empty() {
        labels.push("DJI Internal SD Card".to_owned());
    }
    let mut batch = Batch::default();
    labels.dedup();
    for l in labels {
        if !ds_artifact_exists(ctx, usage, Some(("KAAMOS_DESCRIPTION", &l))) {
            batch.push(NewArt::new(ctx, usage, None).text("KAAMOS_DESCRIPTION", l));
        }
    }
    batch.flush(ctx)?;
    Ok(())
}
