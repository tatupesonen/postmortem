//! Disk image containers.
//!
//! [`open`] takes the first file of an image, discovers its segments, sniffs
//! the container format and returns an [`Image`]: a [`ReadAt`] over the
//! *logical* (decompressed, de-sparsed) disk bytes plus container metadata.

pub mod concat;
pub mod ewf;
pub mod segments;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pm_core::io::FileSource;
use pm_core::{ReadAt, SharedSource};
use thiserror::Error;

pub use concat::ConcatSource;

/// Errors opening a disk image.
#[derive(Debug, Error)]
pub enum ImageError {
    #[error("cannot open image segment {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("I/O error in image: {0}")]
    Io(#[from] std::io::Error),
    #[error("{format} images are not supported yet")]
    Unsupported { format: ImageFormat },
    #[error("corrupt {format} image: {reason}")]
    Corrupt { format: ImageFormat, reason: String },
    #[error("no image paths given")]
    NoPaths,
}

/// Container format of a disk image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Raw,
    /// Expert Witness / EnCase (E01, Ex01 is `Ewf2`).
    Ewf,
    Ewf2,
    Vmdk,
    Vhd,
    Vhdx,
    Qcow,
    Dmg,
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Raw => "raw",
            Self::Ewf => "EWF (E01)",
            Self::Ewf2 => "EWF2 (Ex01)",
            Self::Vmdk => "VMDK",
            Self::Vhd => "VHD",
            Self::Vhdx => "VHDX",
            Self::Qcow => "QCOW",
            Self::Dmg => "DMG",
        })
    }
}

/// Hash algorithms whose values containers may store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
}

/// Metadata about an opened image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub format: ImageFormat,
    /// All segment files, in order.
    pub paths: Vec<PathBuf>,
    /// Logical (media) size in bytes.
    pub size: u64,
    /// Sector size reported by the container, else 512.
    pub sector_size: u32,
    /// Hashes stored inside the container at acquisition time.
    pub stored_hashes: Vec<(HashAlgo, String)>,
    /// Free-form acquisition metadata (examiner, case number, notes, …).
    pub metadata: Vec<(String, String)>,
}

/// An opened disk image.
#[derive(Debug, Clone)]
pub struct Image {
    pub info: ImageInfo,
    source: SharedSource,
}

impl Image {
    /// Wraps an already opened source (in-memory or synthetic evidence).
    pub fn new(info: ImageInfo, source: SharedSource) -> Self {
        Self { info, source }
    }

    /// The logical disk content.
    pub fn source(&self) -> SharedSource {
        Arc::clone(&self.source)
    }
}

/// Identifies a container format from leading/trailing magic values.
pub fn sniff(first_segment: &dyn ReadAt) -> ImageFormat {
    let mut head = [0u8; 512];
    let n = first_segment.read_at(0, &mut head).unwrap_or(0);
    let head = head.get(..n).unwrap_or_default();
    let starts = |m: &[u8]| head.starts_with(m);
    if starts(b"EVF\x09\x0d\x0a\xff\x00") || starts(b"LVF\x09\x0d\x0a\xff\x00") {
        return ImageFormat::Ewf;
    }
    if starts(b"EVF2\x0d\x0a\x81\x00") || starts(b"LEF2\x0d\x0a\x81\x00") {
        return ImageFormat::Ewf2;
    }
    if starts(b"KDMV") || starts(b"COWD") || starts(b"# Disk DescriptorFile") {
        return ImageFormat::Vmdk;
    }
    if starts(b"vhdxfile") {
        return ImageFormat::Vhdx;
    }
    if starts(b"QFI\xfb") {
        return ImageFormat::Qcow;
    }
    if starts(b"conectix") {
        // Dynamic/differencing VHDs keep a footer copy at offset 0.
        return ImageFormat::Vhd;
    }
    let size = first_segment.size();
    if size >= 512 {
        let mut tail = [0u8; 512];
        if first_segment.read_exact_at(size - 512, &mut tail).is_ok() {
            if tail.starts_with(b"conectix") {
                return ImageFormat::Vhd;
            }
            if tail.starts_with(b"koly") {
                return ImageFormat::Dmg;
            }
        }
    }
    ImageFormat::Raw
}

fn open_file(path: &Path) -> Result<FileSource, ImageError> {
    FileSource::open(path).map_err(|source| ImageError::Open {
        path: path.to_path_buf(),
        source,
    })
}

/// Opens an image given its first segment, discovering the rest.
pub fn open(first: impl AsRef<Path>) -> Result<Image, ImageError> {
    let first = first.as_ref();
    let head = open_file(first)?;
    match sniff(&head) {
        ImageFormat::Raw => open_paths(&segments::discover(first)),
        ImageFormat::Ewf => open_ewf(&ewf::discover(first)),
        format => Err(ImageError::Unsupported { format }),
    }
}

/// Opens an EWF (E01/s01) image from an explicit list of segments.
pub fn open_ewf(paths: &[PathBuf]) -> Result<Image, ImageError> {
    if paths.is_empty() {
        return Err(ImageError::NoPaths);
    }
    let (src, meta) = ewf::EwfSource::open(paths)?;
    let source: SharedSource = Arc::new(src);
    Ok(Image {
        info: ImageInfo {
            format: ImageFormat::Ewf,
            paths: paths.to_vec(),
            size: source.size(),
            sector_size: if meta.bytes_per_sector == 0 {
                512
            } else {
                meta.bytes_per_sector
            },
            stored_hashes: meta.hashes,
            metadata: meta.fields,
        },
        source,
    })
}

/// Opens an explicit, ordered list of raw segments.
pub fn open_paths(paths: &[PathBuf]) -> Result<Image, ImageError> {
    if paths.is_empty() {
        return Err(ImageError::NoPaths);
    }
    let files = paths
        .iter()
        .map(|p| open_file(p))
        .collect::<Result<Vec<_>, _>>()?;
    let source: SharedSource = if files.len() == 1 {
        Arc::new(files.into_iter().next().ok_or(ImageError::NoPaths)?)
    } else {
        Arc::new(ConcatSource::new(files)?)
    };
    Ok(Image {
        info: ImageInfo {
            format: ImageFormat::Raw,
            paths: paths.to_vec(),
            size: source.size(),
            sector_size: 512,
            stored_hashes: Vec::new(),
            metadata: Vec::new(),
        },
        source,
    })
}
