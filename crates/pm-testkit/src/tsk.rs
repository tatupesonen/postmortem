//! Differential testing against The Sleuth Kit command-line tools.
//!
//! When `mmls`/`fls` are installed (e.g. `apt install sleuthkit` / `brew install sleuthkit`), tests can
//! compare postmortem's view of an image with TSK's — the engine Autopsy uses.
//! Every function returns `None` when the tool is unavailable so tests can
//! skip gracefully on machines without TSK.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Locates an executable on `PATH`.
pub fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn run(name: &str, args: &[&std::ffi::OsStr]) -> Option<String> {
    let exe = tool(name)?;
    let out = Command::new(exe).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One row of `mmls` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmlsRow {
    /// `Meta`, `-------` (unallocated) or `TTT:SSS` / `NNN` for partitions.
    pub slot: String,
    pub start: u64,
    pub len: u64,
    pub description: String,
}

/// Runs `mmls` on an image; `None` if TSK is missing or mmls fails.
pub fn mmls(image: &Path) -> Option<Vec<MmlsRow>> {
    let text = run("mmls", &[image.as_os_str()])?;
    let rows = text
        .lines()
        .filter_map(|l| {
            let (_, rest) = l.split_once(":  ")?;
            let mut it = rest.split_whitespace();
            let slot = it.next()?.to_owned();
            let start = it.next()?.parse().ok()?;
            let _end: u64 = it.next()?.parse().ok()?;
            let len = it.next()?.parse().ok()?;
            let description = it.collect::<Vec<_>>().join(" ");
            Some(MmlsRow {
                slot,
                start,
                len,
                description,
            })
        })
        .collect();
    Some(rows)
}

/// One row of `fls -r -p` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlsRow {
    /// Name-type/meta-type pair, e.g. `r/r`, `d/d`, `v/v`.
    pub kind: String,
    pub deleted: bool,
    /// Metadata address as printed (may include `-type-id` for NTFS).
    pub inode: String,
    /// Full path relative to the root, without leading `/`.
    pub path: String,
}

/// Runs `fls -r -p` on a filesystem at `offset_sectors` within `image`.
pub fn fls(image: &Path, offset_sectors: u64) -> Option<Vec<FlsRow>> {
    let off = offset_sectors.to_string();
    let text = run(
        "fls",
        &[
            "-r".as_ref(),
            "-p".as_ref(),
            "-o".as_ref(),
            off.as_ref(),
            image.as_os_str(),
        ],
    )?;
    let rows = text
        .lines()
        .filter_map(|l| {
            let (head, path) = l.split_once(":\t")?;
            let mut it = head.split_whitespace();
            let kind = it.next()?.to_owned();
            let mut next = it.next()?;
            let deleted = next == "*";
            if deleted {
                next = it.next()?;
            }
            // "(realloc)" may follow the inode for reallocated entries.
            Some(FlsRow {
                kind,
                deleted,
                inode: next.to_owned(),
                path: path.to_owned(),
            })
        })
        .collect();
    Some(rows)
}
