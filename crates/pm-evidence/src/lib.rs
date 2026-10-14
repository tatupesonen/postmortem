//! Layered evidence detection: disk image → volume system → filesystems.
//!
//! [`open`] mirrors how Autopsy's disk-image data source processor walks an
//! image: if a partition table is found, each allocated partition is probed
//! for a filesystem; otherwise the whole image is probed (a bare volume).

use std::path::Path;
use std::sync::Arc;

use jiff::tz::TimeZone;
use pm_core::io::SliceSource;
use pm_core::{ReadAt, SharedSource};
use pm_fs::FileSystem;
use pm_image::{Image, ImageError};
use pm_volume::{Partition, VolumeError, VolumeSystem};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Volume(#[from] VolumeError),
}

/// Options for opening evidence.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Time zone for filesystems that store local times (FAT).
    pub timezone: TimeZone,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            timezone: TimeZone::UTC,
        }
    }
}

/// A filesystem found in the evidence.
#[derive(Debug, Clone)]
pub struct FoundFs {
    /// Partition that holds it (`None` = the whole image is one volume).
    pub partition: Option<Partition>,
    /// Byte offset of the filesystem within the image.
    pub offset: u64,
    pub fs: Arc<dyn FileSystem>,
}

/// An opened disk image with its detected structure.
#[derive(Debug, Clone)]
pub struct Evidence {
    pub image: Image,
    pub volume_system: Option<VolumeSystem>,
    pub filesystems: Vec<FoundFs>,
}

impl Evidence {
    /// Filesystem found in a given partition slot, if any.
    pub fn fs_for_partition(&self, addr: u32) -> Option<&FoundFs> {
        self.filesystems
            .iter()
            .find(|f| f.partition.as_ref().is_some_and(|p| p.addr == addr))
    }
}

/// Probes `src` for a supported filesystem.
pub fn probe_fs(src: SharedSource, opts: &OpenOptions) -> Option<Arc<dyn FileSystem>> {
    if pm_fs_ntfs::probe(&*src) {
        match pm_fs_ntfs::NtfsFs::open(Arc::clone(&src)) {
            Ok(fs) => return Some(Arc::new(fs)),
            Err(e) => tracing::warn!("NTFS boot sector found but open failed: {e}"),
        }
    }
    if pm_fs_fat::probe(&*src) {
        let fat_opts = pm_fs_fat::FatOptions {
            timezone: opts.timezone.clone(),
        };
        match pm_fs_fat::FatFs::open(src, &fat_opts) {
            Ok(fs) => return Some(Arc::new(fs)),
            Err(e) => tracing::warn!("FAT boot sector found but open failed: {e}"),
        }
    }
    None
}

/// Opens a disk image (first segment path) and detects its structure.
pub fn open(path: impl AsRef<Path>, opts: &OpenOptions) -> Result<Evidence, EvidenceError> {
    let image = pm_image::open(path)?;
    Ok(analyze(image, opts)?)
}

/// Detects volumes and filesystems in an already opened image.
pub fn analyze(image: Image, opts: &OpenOptions) -> Result<Evidence, VolumeError> {
    let src = image.source();
    let volume_system = pm_volume::detect(&*src)?;
    let mut filesystems = Vec::new();
    match &volume_system {
        Some(vs) => {
            for p in vs.allocated() {
                let offset = vs.byte_offset(p);
                let part: SharedSource =
                    Arc::new(SliceSource::new(Arc::clone(&src), offset, vs.byte_len(p)));
                if let Some(fs) = probe_fs(part, opts) {
                    filesystems.push(FoundFs {
                        partition: Some(p.clone()),
                        offset,
                        fs,
                    });
                }
            }
        }
        None => {
            if let Some(fs) = probe_fs(Arc::clone(&src), opts) {
                filesystems.push(FoundFs {
                    partition: None,
                    offset: 0,
                    fs,
                });
            }
        }
    }
    tracing::info!(
        size = src.size(),
        partitions = volume_system.as_ref().map_or(0, |v| v.allocated().count()),
        filesystems = filesystems.len(),
        "evidence analysed"
    );
    Ok(Evidence {
        image,
        volume_system,
        filesystems,
    })
}
