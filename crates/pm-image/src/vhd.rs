//! Microsoft Virtual Hard Disk (VHD, "vpc"): fixed, dynamic and
//! differencing disks.
//!
//! Layout (Virtual Hard Disk Image Format Specification, big-endian):
//! a 512-byte `conectix` footer at the end of the file (copied to offset 0
//! for dynamic disks). Dynamic and differencing disks add a 1024-byte
//! `cxsparse` header holding the Block Allocation Table (BAT) location, the
//! block size and, for differencing disks, the parent's name and locators.
//! Each allocated block starts with a sector bitmap (MSB first); in a
//! differencing disk a clear bit means "read this sector from the parent".

use std::io;
use std::path::Path;
use std::sync::Arc;

use pm_core::decode::{to_hex, utf16be_lossy, utf16le_lossy};
use pm_core::io::FileSource;
use pm_core::{Bytes, ReadAt, SharedSource};

use crate::cache::{DEFAULT_PAGES, MetaPages};
use crate::mapped::{BlockMap, MappedSource, Run};
use crate::{Image, ImageError, ImageFormat, ImageInfo, parent};

const SECTOR: u64 = 512;
const UNALLOCATED: u32 = 0xFFFF_FFFF;
/// Largest block size accepted (the specification uses 2 MiB; 512 KiB and
/// 4 MiB occur in the wild).
const MAX_BLOCK_SIZE: u32 = 1 << 30;

fn corrupt(reason: impl Into<String>) -> ImageError {
    ImageError::Corrupt {
        format: ImageFormat::Vhd,
        reason: reason.into(),
    }
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn trunc(e: pm_core::Truncated) -> ImageError {
    corrupt(e.to_string())
}

/// VHD disk type (footer field `Disk Type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskType {
    Fixed,
    Dynamic,
    Differencing,
}

impl DiskType {
    fn name(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Dynamic => "dynamic",
            Self::Differencing => "differencing",
        }
    }
}

/// Parsed footer and (for sparse disks) dynamic header.
#[derive(Debug, Clone)]
pub struct VhdHeader {
    pub disk_type: DiskType,
    /// Virtual disk size in bytes.
    pub size: u64,
    pub unique_id: [u8; 16],
    pub creator_app: String,
    /// Block size (sparse disks).
    pub block_size: u32,
    bat_offset: u64,
    max_entries: u32,
    /// Parent's unique id (differencing).
    pub parent_id: [u8; 16],
    /// Parent references in priority order: relative locator, absolute
    /// locator, Mac locators, then the stored parent name.
    pub parent_refs: Vec<String>,
}

/// Footer checksum: one's complement of the byte sum without the field.
fn checksum(footer: &[u8]) -> u32 {
    let sum = footer
        .iter()
        .enumerate()
        .filter(|(i, _)| !(64..68).contains(i))
        .fold(0u32, |a, (_, &b)| a.wrapping_add(u32::from(b)));
    !sum
}

fn find_footer(src: &dyn ReadAt) -> Result<Vec<u8>, ImageError> {
    let size = src.size();
    let mut tries = Vec::new();
    if size >= SECTOR {
        tries.push(size - SECTOR);
    }
    if size >= 511 {
        // Virtual PC before 2004 wrote a 511-byte footer.
        tries.push(size - 511);
    }
    tries.push(0);
    for off in tries {
        let mut f = vec![0u8; SECTOR as usize];
        let n = src.read_at(off, &mut f)?;
        if n >= 85 && f.starts_with(b"conectix") {
            if off == 0 && size >= SECTOR {
                tracing::warn!("VHD footer at end of file is damaged; using the copy at offset 0");
            }
            return Ok(f);
        }
    }
    Err(corrupt("no `conectix` footer"))
}

fn uuid(b: [u8; 16]) -> String {
    to_hex(&b)
}

/// Parses footer and dynamic header.
pub fn parse(src: &dyn ReadAt) -> Result<VhdHeader, ImageError> {
    let f = find_footer(src)?;
    let stored = f.u32_be(64).map_err(trunc)?;
    if stored != checksum(&f) {
        tracing::warn!("VHD footer checksum mismatch");
    }
    let data_offset = f.u64_be(16).map_err(trunc)?;
    let size = f.u64_be(48).map_err(trunc)?;
    let disk_type = match f.u32_be(60).map_err(trunc)? {
        2 => DiskType::Fixed,
        3 => DiskType::Dynamic,
        4 => DiskType::Differencing,
        other => return Err(corrupt(format!("unknown disk type {other}"))),
    };
    let unique_id = f.array::<16>(68).map_err(trunc)?;
    let creator_app = String::from_utf8_lossy(f.sub(28, 4).map_err(trunc)?)
        .trim_end_matches(['\0', ' '])
        .to_string();
    let mut h = VhdHeader {
        disk_type,
        size,
        unique_id,
        creator_app,
        block_size: 0,
        bat_offset: 0,
        max_entries: 0,
        parent_id: [0; 16],
        parent_refs: Vec::new(),
    };
    if disk_type == DiskType::Fixed {
        return Ok(h);
    }
    let d = src
        .read_vec_at(data_offset, 1024)
        .map_err(|_| corrupt(format!("dynamic header at {data_offset} is out of range")))?;
    if !d.starts_with(b"cxsparse") {
        return Err(corrupt("dynamic header signature `cxsparse` missing"));
    }
    h.bat_offset = d.u64_be(16).map_err(trunc)?;
    h.max_entries = d.u32_be(28).map_err(trunc)?;
    h.block_size = d.u32_be(32).map_err(trunc)?;
    if h.block_size == 0
        || !h.block_size.is_multiple_of(SECTOR as u32)
        || h.block_size > MAX_BLOCK_SIZE
        || !h.block_size.is_power_of_two()
    {
        return Err(corrupt(format!("invalid block size {}", h.block_size)));
    }
    let needed = size.div_ceil(u64::from(h.block_size));
    if u64::from(h.max_entries) < needed {
        tracing::warn!(
            entries = h.max_entries,
            needed,
            "VHD BAT is smaller than the disk; the rest reads as unallocated"
        );
    }
    if disk_type == DiskType::Differencing {
        h.parent_id = d.array::<16>(40).map_err(trunc)?;
        let mut refs = Vec::new();
        let mut abs = Vec::new();
        for i in 0..8 {
            let e = d.sub(576 + i * 24, 24).map_err(trunc)?;
            let code = e.sub(0, 4).map_err(trunc)?;
            let len = e.u32_be(8).map_err(trunc)?;
            let off = e.u64_be(16).map_err(trunc)?;
            if code == [0; 4] || len == 0 || len > 64 * 1024 {
                continue;
            }
            let Ok(raw) = src.read_vec_at(off, len as usize) else {
                tracing::warn!(locator = i, "VHD parent locator data out of range");
                continue;
            };
            let text = match code {
                b"W2ru" | b"W2ku" => utf16le_lossy(&raw),
                _ => String::from_utf8_lossy(&raw)
                    .trim_end_matches('\0')
                    .to_string(),
            };
            match code {
                b"W2ru" | b"Wi2r" => refs.push(text),
                _ => abs.push(text),
            }
        }
        refs.extend(abs);
        let name = utf16be_lossy(d.sub(64, 512).map_err(trunc)?);
        if !name.is_empty() {
            refs.push(name);
        }
        h.parent_refs = refs;
    }
    Ok(h)
}

/// Sparse (dynamic/differencing) block map.
#[derive(Debug)]
pub struct VhdMap {
    size: u64,
    kind: DiskType,
    block_size: u64,
    bitmap_size: u64,
    bat_offset: u64,
    max_entries: u64,
    meta: MetaPages,
}

/// A fixed disk: the data is the file minus the footer.
#[derive(Debug)]
pub struct FixedMap {
    size: u64,
    stored: u64,
    src: SharedSource,
}

impl BlockMap for FixedMap {
    fn size(&self) -> u64 {
        self.size
    }
    fn map(&self, pos: u64) -> io::Result<(Run, u64)> {
        Ok(if pos < self.stored {
            (
                Run::Data {
                    src: Arc::clone(&self.src),
                    offset: pos,
                },
                self.stored - pos,
            )
        } else {
            (Run::Zero, self.size - pos)
        })
    }
}

impl BlockMap for VhdMap {
    fn size(&self) -> u64 {
        self.size
    }

    fn map(&self, pos: u64) -> io::Result<(Run, u64)> {
        let absent = if self.kind == DiskType::Differencing {
            Run::Parent
        } else {
            Run::Zero
        };
        let block = pos / self.block_size;
        let within = pos % self.block_size;
        let to_block_end = (self.block_size - within).min(self.size - pos);
        if block >= self.max_entries {
            return Ok((absent, to_block_end));
        }
        let entry = self.meta.u32_be(0, self.bat_offset + block * 4)?;
        if entry == UNALLOCATED {
            return Ok((absent, to_block_end));
        }
        let start = u64::from(entry) * SECTOR;
        let data = start + self.bitmap_size;
        // Run of sectors sharing the bitmap state of the current one.
        let first = within / SECTOR;
        let last = (within + to_block_end - 1) / SECTOR;
        let bit = |s: u64| -> io::Result<bool> {
            let byte = self.meta.u8(0, start + s / 8)?;
            Ok(byte & (0x80 >> (s % 8)) != 0)
        };
        let present = bit(first)?;
        let mut s = first + 1;
        while s <= last {
            if s.is_multiple_of(8) && s + 7 <= last {
                let byte = self.meta.u8(0, start + s / 8)?;
                if byte == if present { 0xFF } else { 0x00 } {
                    s += 8;
                    continue;
                }
            }
            if bit(s)? != present {
                break;
            }
            s += 1;
        }
        let run_end = (s * SECTOR).min(within + to_block_end);
        let len = run_end - within;
        Ok(if present {
            (
                Run::Data {
                    src: Arc::clone(self.meta.file(0)?),
                    offset: data + within,
                },
                len,
            )
        } else {
            (absent, len)
        })
    }
}

/// An opened VHD (fixed or sparse).
#[derive(Debug)]
pub enum VhdSource {
    Fixed(MappedSource<FixedMap>),
    Sparse(MappedSource<VhdMap>),
}

impl ReadAt for VhdSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Fixed(s) => s.read_at(offset, buf),
            Self::Sparse(s) => s.read_at(offset, buf),
        }
    }
    fn size(&self) -> u64 {
        match self {
            Self::Fixed(s) => s.size(),
            Self::Sparse(s) => s.size(),
        }
    }
}

impl VhdSource {
    /// Opens a VHD from `src`. A differencing disk reads unallocated sectors
    /// from `parent` (zeros if `None`).
    pub fn from_source(
        src: SharedSource,
        parent: Option<SharedSource>,
    ) -> Result<(Self, VhdHeader), ImageError> {
        let h = parse(&*src)?;
        Ok((Self::with_header(src, &h, parent), h))
    }

    fn with_header(src: SharedSource, h: &VhdHeader, parent: Option<SharedSource>) -> Self {
        if h.disk_type == DiskType::Fixed {
            let stored = h.size.min(src.size().saturating_sub(SECTOR));
            if stored < h.size {
                tracing::warn!(
                    size = h.size,
                    stored,
                    "fixed VHD is shorter than its disk size; the rest reads as zeros"
                );
            }
            return Self::Fixed(MappedSource::new(
                FixedMap {
                    size: h.size,
                    stored,
                    src,
                },
                None,
            ));
        }
        let block_size = u64::from(h.block_size);
        let bitmap_size = (block_size / SECTOR).div_ceil(8).next_multiple_of(SECTOR);
        let map = VhdMap {
            size: h.size,
            kind: h.disk_type,
            block_size,
            bitmap_size,
            bat_offset: h.bat_offset,
            max_entries: u64::from(h.max_entries),
            meta: MetaPages::new(vec![src], DEFAULT_PAGES),
        };
        Self::Sparse(MappedSource::new(map, parent))
    }
}

/// Opens a VHD file, following differencing parents.
pub(crate) fn open_path(path: &Path, depth: usize) -> Result<Image, ImageError> {
    let src: SharedSource =
        Arc::new(FileSource::open(path).map_err(|source| ImageError::Open {
            path: path.to_path_buf(),
            source,
        })?);
    let h = parse(&*src)?;
    let mut paths = vec![path.to_path_buf()];
    let mut metadata = vec![
        ("Disk type".to_string(), h.disk_type.name().to_string()),
        ("Unique ID".to_string(), uuid(h.unique_id)),
    ];
    if !h.creator_app.is_empty() {
        metadata.push(("Creator application".into(), h.creator_app.clone()));
    }
    let parent = if h.disk_type == DiskType::Differencing {
        let p = parent::open(path, &h.parent_refs, depth)?;
        metadata.push((
            "Parent image".into(),
            p.info
                .paths
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        metadata.push(("Parent unique ID".into(), uuid(h.parent_id)));
        if p.info.format == ImageFormat::Vhd
            && let Some((_, id)) = p.info.metadata.iter().find(|(k, _)| k == "Unique ID")
            && *id != uuid(h.parent_id)
        {
            tracing::warn!(child = %path.display(), "VHD parent unique ID does not match the child's reference");
        }
        paths.extend(p.info.paths.iter().cloned());
        Some(p.source())
    } else {
        None
    };
    let source: SharedSource = Arc::new(VhdSource::with_header(src, &h, parent));
    Ok(Image::new(
        ImageInfo {
            format: ImageFormat::Vhd,
            paths,
            size: h.size,
            sector_size: 512,
            stored_hashes: Vec::new(),
            metadata,
        },
        source,
    ))
}
