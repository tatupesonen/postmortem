//! Raw data sources: "Local Disk" (Autopsy `LocalDiskDSProcessor`, a
//! physical device read in place, read-only) and "Unallocated Space Image
//! File" (`RawDSProcessor`: a raw file without file-system analysis,
//! exposed as unallocated-space layout files, optionally in 2 GB pieces so
//! carving and keyword search can work on them in parallel).

use std::path::{Path, PathBuf};

use pm_casedb::{DataSourceKind, FileKindCode, NewDataSource, NewFile, Origin, default_host_name};

use crate::content::layout_json;
use crate::load::{LoadOptions, LoadProgress, LoadSummary, add_image};
use crate::{Case, CaseError, Result, io_err};

/// Autopsy's "2GB chunks" size for unallocated space image files.
pub const RAW_CHUNK: u64 = 2_000_000_000;

/// A disk of the examiner's machine that can be added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDisk {
    /// Device path (`/dev/sdb`, `/dev/disk2`, `\\.\PhysicalDrive1`).
    pub path: PathBuf,
    /// Size in bytes (`None` when the platform does not tell).
    pub size: Option<u64>,
}

/// The disks (and, on Linux, partitions) that are readable by this
/// process. Reading devices usually needs administrator rights; Autopsy
/// says so when none are found.
pub fn local_disks() -> Vec<LocalDisk> {
    let mut out = Vec::new();
    // Readable as postmortem reads it (whole sectors on Windows), with
    // its size.
    let probe = |p: &Path| -> Option<u64> {
        let s = pm_core::io::FileSource::open(p).ok()?;
        let mut b = [0u8; 1];
        (pm_core::ReadAt::read_at(&s, 0, &mut b).ok()? == 1).then(|| pm_core::ReadAt::size(&s))
    };
    #[cfg(target_os = "linux")]
    {
        // /sys/block lists whole disks; partitions are their sub-entries.
        let skip = |n: &str| n.starts_with("loop") || n.starts_with("ram") || n.starts_with("zram");
        if let Ok(entries) = std::fs::read_dir("/sys/block") {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .filter(|n| !skip(n))
                .collect();
            names.sort();
            for disk in names {
                let mut devs = vec![disk.clone()];
                if let Ok(parts) = std::fs::read_dir(format!("/sys/block/{disk}")) {
                    let mut p: Vec<String> = parts
                        .filter_map(|e| {
                            e.ok().map(|e| e.file_name().to_string_lossy().into_owned())
                        })
                        .filter(|n| n.starts_with(&disk))
                        .collect();
                    p.sort();
                    devs.extend(p);
                }
                for d in devs {
                    let path = PathBuf::from(format!("/dev/{d}"));
                    if let Some(size) = probe(&path) {
                        out.push(LocalDisk {
                            path,
                            size: Some(size),
                        });
                    }
                }
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(entries) = std::fs::read_dir("/dev") {
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                .filter(|n| {
                    n.strip_prefix("disk").is_some_and(|r| {
                        !r.is_empty() && r.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                })
                .collect();
            names.sort();
            for n in names {
                let path = PathBuf::from(format!("/dev/{n}"));
                if let Some(size) = probe(&path) {
                    out.push(LocalDisk {
                        path,
                        size: Some(size),
                    });
                }
            }
        }
    }
    #[cfg(windows)]
    {
        // Autopsy (`PlatformUtil.getPhysicalDrives`) probes
        // \\.\PhysicalDrive0.. and gives up after four misses in a row,
        // then lists the volumes with a drive letter (`getPartitions`).
        let mut misses = 0;
        for i in 0..64 {
            let path = PathBuf::from(format!(r"\\.\PhysicalDrive{i}"));
            if let Some(size) = probe(&path) {
                misses = 0;
                out.push(LocalDisk {
                    path,
                    size: Some(size),
                });
            } else {
                misses += 1;
                if misses >= 4 {
                    break;
                }
            }
        }
        for letter in 'A'..='Z' {
            let path = PathBuf::from(format!(r"\\.\{letter}:"));
            if let Some(size) = probe(&path) {
                out.push(LocalDisk {
                    path,
                    size: Some(size),
                });
            }
        }
    }
    let _ = probe;
    out
}

/// Adds a local disk (read-only, in place) as a data source.
pub fn add_local_disk(
    case: &Case,
    device: &Path,
    opts: &LoadOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    let opts = LoadOptions {
        kind: DataSourceKind::LocalDisk,
        ..opts.clone()
    };
    add_image(case, device, &opts, progress)
}

/// Options of an unallocated space image file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawOptions {
    pub name: Option<String>,
    pub timezone: String,
    /// Piece size; `None` keeps the whole file as one piece ("Do not
    /// break up").
    pub chunk: Option<u64>,
    pub host: Option<String>,
}

impl Default for RawOptions {
    fn default() -> Self {
        Self {
            name: None,
            timezone: "UTC".into(),
            chunk: Some(RAW_CHUNK),
            host: None,
        }
    }
}

/// The `[start, end)` pieces of a `size`-byte file.
pub fn raw_pieces(size: u64, chunk: Option<u64>) -> Vec<(u64, u64)> {
    let step = chunk.filter(|&c| c > 0 && size >= c).unwrap_or(size.max(1));
    let mut out = Vec::new();
    let mut start = 0;
    while start < size {
        let end = start.saturating_add(step).min(size);
        out.push((start, end));
        start = end;
    }
    out
}

/// Adds a raw file as an unallocated space image: no file-system
/// analysis, only layout files covering its bytes.
pub fn add_unallocated_image(
    case: &Case,
    path: &Path,
    opts: &RawOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    let md = std::fs::metadata(path).map_err(io_err(path))?;
    if !md.is_file() {
        return Err(CaseError::NotFound(format!(
            "{} is not a file",
            path.display()
        )));
    }
    std::fs::File::open(path).map_err(io_err(path))?;
    let size = md.len();
    let name = opts.name.clone().unwrap_or_else(|| {
        path.file_name().map_or_else(
            || "unallocated".into(),
            |n| n.to_string_lossy().into_owned(),
        )
    });
    progress.set_stage("Adding unallocated space image");
    let pieces = raw_pieces(size, opts.chunk);
    let ds_id = case.db().bulk(|b| {
        let ds_id = b.add_data_source(&NewDataSource {
            name: name.clone(),
            kind: DataSourceKind::UnallocatedImage,
            paths: vec![path.display().to_string()],
            size,
            sector_size: 512,
            timezone: opts.timezone.clone(),
            image_format: Some("raw".into()),
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: Vec::new(),
            added_at: jiff::Timestamp::now().as_second(),
        })?;
        let host = opts
            .host
            .clone()
            .unwrap_or_else(|| default_host_name(Some(&path.display().to_string()), ds_id));
        b.assign_host(ds_id, &host)?;
        for &(start, end) in &pieces {
            let mut f = NewFile::new(
                ds_id,
                None,
                "/",
                format!("Unalloc_{ds_id}_{start}_{end}"),
                Origin::UnallocBlocks,
                FileKindCode::Regular,
            );
            f.size = end - start;
            f.name_alloc = false;
            f.meta_alloc = false;
            f.layout = Some(layout_json(&[(start, end - start)]));
            b.add_file(&f)?;
        }
        Ok(ds_id)
    })?;
    progress
        .files
        .store(pieces.len() as u64, std::sync::atomic::Ordering::Relaxed);
    progress.set_stage("Done");
    Ok(LoadSummary {
        ds_id,
        fs_ids: Vec::new(),
        files: pieces.len() as u64,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_follow_autopsy_chunking() {
        assert_eq!(raw_pieces(10, None), [(0, 10)]);
        assert_eq!(raw_pieces(10, Some(4)), [(0, 4), (4, 8), (8, 10)]);
        assert_eq!(raw_pieces(3, Some(4)), [(0, 3)], "smaller than a chunk");
        assert!(raw_pieces(0, Some(4)).is_empty());
    }
}
