//! What to add as a data source (Autopsy's data source processors), and
//! the one entry point that adds any of them.

use std::path::PathBuf;

use crate::load::{LoadOptions, LoadProgress, LoadSummary, add_image};
use crate::logical::{LogicalOptions, add_logical_files};
use crate::raw_sources::{RawOptions, add_local_disk, add_unallocated_image};
use crate::{Case, Result};

/// A data source to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataSourceSpec {
    /// "Disk Image or VM File": raw, split, E01, VMDK, VHD(X), QCOW2.
    DiskImage(PathBuf),
    /// "Local Disk": a physical device of this machine, read in place.
    LocalDisk(PathBuf),
    /// "Logical Files": files and folders, read in place.
    LogicalFiles {
        paths: Vec<PathBuf>,
        /// Record modified / created / accessed times.
        modified: bool,
        created: bool,
        accessed: bool,
    },
    /// "Unallocated Space Image File": a raw file without file-system
    /// analysis; `chunk` splits it (`None`: one piece).
    UnallocatedImage { path: PathBuf, chunk: Option<u64> },
}

impl DataSourceSpec {
    /// Autopsy's name of the processor.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::DiskImage(_) => "Disk Image or VM File",
            Self::LocalDisk(_) => "Local Disk",
            Self::LogicalFiles { .. } => "Logical Files",
            Self::UnallocatedImage { .. } => "Unallocated Space Image File",
        }
    }

    /// A short label for progress displays.
    pub fn label(&self) -> String {
        let name = |p: &PathBuf| {
            p.file_name().map_or_else(
                || p.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        match self {
            Self::DiskImage(p) | Self::LocalDisk(p) | Self::UnallocatedImage { path: p, .. } => {
                name(p)
            }
            Self::LogicalFiles { paths, .. } => match paths.as_slice() {
                [one] => name(one),
                many => format!("{} logical files and folders", many.len()),
            },
        }
    }
}

/// Adds `spec` to the case. `load` supplies the name, time zone, host and
/// (for disk images and local disks) the loader options.
pub fn add_data_source(
    case: &Case,
    spec: &DataSourceSpec,
    load: &LoadOptions,
    progress: &LoadProgress,
) -> Result<LoadSummary> {
    match spec {
        DataSourceSpec::DiskImage(p) => add_image(case, p, load, progress),
        DataSourceSpec::LocalDisk(p) => add_local_disk(case, p, load, progress),
        DataSourceSpec::LogicalFiles {
            paths,
            modified,
            created,
            accessed,
        } => add_logical_files(
            case,
            paths,
            &LogicalOptions {
                name: load.name.clone(),
                modified: *modified,
                created: *created,
                accessed: *accessed,
                host: load.host.clone(),
            },
            progress,
        ),
        DataSourceSpec::UnallocatedImage { path, chunk } => add_unallocated_image(
            case,
            path,
            &RawOptions {
                name: load.name.clone(),
                timezone: load.timezone.clone(),
                chunk: *chunk,
                host: load.host.clone(),
            },
            progress,
        ),
    }
}
