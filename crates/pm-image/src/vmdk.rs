//! `VMware` Virtual Disk (VMDK): descriptor files, hosted sparse extents
//! (`KDMV`, monolithic or split into 2 GB extents), flat extents, ZERO
//! extents, streamOptimized (deflate-compressed grains with markers) and
//! delta links (`parentFileNameHint`).
//!
//! A VMDK is a text *descriptor* (a separate file, or embedded in the first
//! sparse extent) listing *extents* in virtual order. A sparse extent maps
//! grains (usually 64 KiB) through a grain directory (GD) of grain tables
//! (GT, 512 entries each). GTE 0 is unallocated (read the parent of a delta
//! link, or zeros); GTE 1 is a zeroed grain when the header enables it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::{Decompress, FlushDecompress};
use pm_core::io::FileSource;
use pm_core::{Bytes, ReadAt, SharedSource};

use crate::cache::{DEFAULT_PAGES, Lru, MetaPages};
use crate::mapped::{BlockMap, MappedSource, Run};
use crate::{Image, ImageError, ImageFormat, ImageInfo, parent};

const SECTOR: u64 = 512;
const GD_AT_END: u64 = u64::MAX;
const FLAG_ZERO_GRAIN: u32 = 1 << 2;
const FLAG_COMPRESSED: u32 = 1 << 16;
const FLAG_MARKERS: u32 = 1 << 17;
/// Largest descriptor read (real ones are a few hundred bytes).
const MAX_DESCRIPTOR: u64 = 1 << 20;
/// Largest grain accepted (the default is 128 sectors = 64 KiB).
const MAX_GRAIN_SECTORS: u64 = 1 << 16;
const MAX_GTES: u32 = 1 << 16;
/// Decompressed grains kept (64 × 64 KiB = 4 MiB).
const GRAIN_CACHE: usize = 64;
/// Most extents a descriptor may list.
const MAX_EXTENTS: usize = 1 << 16;

fn corrupt(reason: impl Into<String>) -> ImageError {
    ImageError::Corrupt {
        format: ImageFormat::Vmdk,
        reason: reason.into(),
    }
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn trunc(e: pm_core::Truncated) -> ImageError {
    corrupt(e.to_string())
}

/// The header of a hosted sparse extent (`KDMV`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparseHeader {
    pub version: u32,
    pub flags: u32,
    /// Capacity in sectors.
    pub capacity: u64,
    /// Grain size in sectors.
    pub grain: u64,
    pub descriptor_offset: u64,
    pub descriptor_size: u64,
    pub gtes_per_gt: u32,
    pub rgd_offset: u64,
    pub gd_offset: u64,
    pub compress_algorithm: u16,
}

fn parse_sparse_bytes(h: &[u8]) -> Result<SparseHeader, ImageError> {
    if !h.starts_with(b"KDMV") {
        return Err(corrupt("sparse extent header signature `KDMV` missing"));
    }
    Ok(SparseHeader {
        version: h.u32_le(4).map_err(trunc)?,
        flags: h.u32_le(8).map_err(trunc)?,
        capacity: h.u64_le(12).map_err(trunc)?,
        grain: h.u64_le(20).map_err(trunc)?,
        descriptor_offset: h.u64_le(28).map_err(trunc)?,
        descriptor_size: h.u64_le(36).map_err(trunc)?,
        gtes_per_gt: h.u32_le(44).map_err(trunc)?,
        rgd_offset: h.u64_le(48).map_err(trunc)?,
        gd_offset: h.u64_le(56).map_err(trunc)?,
        compress_algorithm: h.u16_le(77).map_err(trunc)?,
    })
}

/// Reads a sparse extent header, using the footer copy of streamOptimized
/// extents whose header says the grain directory is at the end.
pub fn parse_sparse(src: &dyn ReadAt) -> Result<SparseHeader, ImageError> {
    let h = src
        .read_vec_at(0, 512)
        .map_err(|_| corrupt("sparse extent shorter than its header"))?;
    let mut hdr = parse_sparse_bytes(&h)?;
    if hdr.gd_offset == GD_AT_END {
        let size = src.size();
        let footer = size
            .checked_sub(1024)
            .and_then(|off| src.read_vec_at(off, 512).ok())
            .ok_or_else(|| corrupt("streamOptimized footer missing"))?;
        let f = parse_sparse_bytes(&footer)
            .map_err(|_| corrupt("streamOptimized footer is not a sparse header"))?;
        hdr.gd_offset = f.gd_offset;
        hdr.rgd_offset = f.rgd_offset;
        if hdr.gd_offset == GD_AT_END {
            return Err(corrupt("streamOptimized footer has no grain directory"));
        }
    }
    if hdr.grain == 0 || hdr.grain > MAX_GRAIN_SECTORS || !hdr.grain.is_power_of_two() {
        return Err(corrupt(format!("invalid grain size {} sectors", hdr.grain)));
    }
    if hdr.gtes_per_gt == 0 || hdr.gtes_per_gt > MAX_GTES {
        return Err(corrupt(format!(
            "invalid grain table size {}",
            hdr.gtes_per_gt
        )));
    }
    if hdr.flags & FLAG_COMPRESSED != 0 && hdr.compress_algorithm != 1 {
        return Err(corrupt(format!(
            "unknown grain compression {}",
            hdr.compress_algorithm
        )));
    }
    Ok(hdr)
}

/// Extent types of descriptor lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtentType {
    Sparse,
    Flat,
    Zero,
    /// Types postmortem cannot read (VMFSSPARSE, SESPARSE, VMFSRDM, …).
    Other(String),
}

/// One extent line of a descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtentLine {
    pub access: String,
    /// Size in sectors.
    pub sectors: u64,
    pub kind: ExtentType,
    pub file: Option<String>,
    /// Start of the data in the file (sectors, FLAT only).
    pub offset: u64,
}

/// A parsed descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Descriptor {
    pub create_type: String,
    pub cid: Option<String>,
    pub parent_cid: Option<String>,
    pub parent_hint: Option<String>,
    pub extents: Vec<ExtentLine>,
}

impl Descriptor {
    /// True for a delta link (a child of another VMDK).
    pub fn has_parent(&self) -> bool {
        self.parent_cid
            .as_deref()
            .is_some_and(|c| !c.eq_ignore_ascii_case("ffffffff"))
            || self.parent_hint.is_some()
    }
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s)
}

fn parse_extent(line: &str) -> Option<ExtentLine> {
    let mut words = line.split_whitespace();
    let access = words.next()?.to_ascii_uppercase();
    if !matches!(access.as_str(), "RW" | "RDONLY" | "NOACCESS") {
        return None;
    }
    let sectors: u64 = words.next()?.parse().ok()?;
    let kind = match words.next()?.to_ascii_uppercase().as_str() {
        "SPARSE" => ExtentType::Sparse,
        "FLAT" | "VMFS" => ExtentType::Flat,
        "ZERO" => ExtentType::Zero,
        other => ExtentType::Other(other.to_string()),
    };
    // The file name is quoted and may contain spaces.
    let (file, rest) = match (line.find('"'), line.rfind('"')) {
        (Some(a), Some(b)) if b > a => (
            Some(line.get(a + 1..b)?.to_string()),
            line.get(b + 1..).unwrap_or_default(),
        ),
        _ => (None, ""),
    };
    let offset = rest
        .split_whitespace()
        .next()
        .and_then(|w| w.parse().ok())
        .unwrap_or(0);
    Some(ExtentLine {
        access,
        sectors,
        kind,
        file,
        offset,
    })
}

/// Parses descriptor text.
pub fn parse_descriptor(text: &str) -> Result<Descriptor, ImageError> {
    let mut d = Descriptor::default();
    for raw in text.lines() {
        let line = raw.trim().trim_end_matches('\0');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(e) = parse_extent(line) {
            if d.extents.len() >= MAX_EXTENTS {
                return Err(corrupt("too many extents in descriptor"));
            }
            d.extents.push(e);
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = unquote(v).to_string();
        match k.trim().to_ascii_lowercase().as_str() {
            "createtype" => d.create_type = v,
            "cid" => d.cid = Some(v),
            "parentcid" => d.parent_cid = Some(v),
            "parentfilenamehint" if !v.is_empty() => d.parent_hint = Some(v),
            _ => {}
        }
    }
    if d.extents.is_empty() {
        return Err(corrupt("descriptor lists no extents"));
    }
    Ok(d)
}

/// How one extent stores its bytes.
#[derive(Debug)]
enum Kind {
    Flat { file: usize, offset: u64 },
    Zero,
    Sparse(Sparse),
}

#[derive(Debug)]
struct Sparse {
    file: usize,
    grain: u64,
    gtes: u64,
    gd_offset: u64,
    gd_entries: u64,
    compressed: bool,
    zero_grain: bool,
}

#[derive(Debug)]
struct Extent {
    /// Virtual start, bytes.
    start: u64,
    len: u64,
    kind: Kind,
}

/// The VMDK block map over all extents.
#[derive(Debug)]
pub struct VmdkMap {
    size: u64,
    extents: Vec<Extent>,
    has_parent: bool,
    meta: MetaPages,
    grains: Lru<(usize, u64), Arc<[u8]>>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl VmdkMap {
    fn absent(&self) -> Run {
        if self.has_parent {
            Run::Parent
        } else {
            Run::Zero
        }
    }

    /// Reads and inflates the compressed grain at sector `gte`.
    fn compressed_grain(&self, s: &Sparse, gte: u64) -> io::Result<Arc<[u8]>> {
        if let Some(g) = self.grains.get(&(s.file, gte)) {
            return Ok(g);
        }
        let src = self.meta.file(s.file)?;
        let at = gte
            .checked_mul(SECTOR)
            .ok_or_else(|| invalid("grain offset overflows".into()))?;
        let mut head = [0u8; 12];
        src.read_exact_at(at, &mut head)?;
        let len = u64::from(head.u32_le(8).map_err(|e| invalid(e.to_string()))?);
        let grain_bytes = s.grain * SECTOR;
        if len == 0 || len > grain_bytes * 2 + 4096 {
            return Err(invalid(format!(
                "compressed grain at sector {gte} has invalid size {len}"
            )));
        }
        let raw = src.read_vec_at(at + 12, len as usize)?;
        let mut out = vec![0u8; grain_bytes as usize];
        let mut z = Decompress::new(true);
        z.decompress(&raw, &mut out, FlushDecompress::Finish)
            .map_err(|e| invalid(format!("compressed grain at sector {gte}: {e}")))?;
        let g: Arc<[u8]> = out.into();
        self.grains.insert((s.file, gte), Arc::clone(&g));
        Ok(g)
    }

    fn map_sparse(&self, s: &Sparse, rel: u64, avail: u64) -> io::Result<(Run, u64)> {
        let grain_bytes = s.grain * SECTOR;
        let gt_bytes = grain_bytes * s.gtes;
        let grain_idx = rel / grain_bytes;
        let gt_idx = grain_idx / s.gtes;
        let mut gte_idx = grain_idx % s.gtes;
        let within = rel % grain_bytes;
        if gt_idx >= s.gd_entries {
            return Ok((self.absent(), avail));
        }
        let gd = self
            .meta
            .u32_le(s.file, s.gd_offset.saturating_add(gt_idx * 4))?;
        if gd == 0 {
            return Ok((self.absent(), (gt_bytes - rel % gt_bytes).min(avail)));
        }
        let gt = u64::from(gd) * SECTOR;
        let gte = u64::from(self.meta.u32_le(s.file, gt + gte_idx * 4)?);
        let to_grain_end = (grain_bytes - within).min(avail);
        if gte == 0 {
            return Ok((self.absent(), to_grain_end));
        }
        if gte == 1 && s.zero_grain {
            return Ok((Run::Zero, to_grain_end));
        }
        if s.compressed {
            let data = self.compressed_grain(s, gte)?;
            return Ok((
                Run::Buffer {
                    data,
                    offset: within as usize,
                },
                to_grain_end,
            ));
        }
        // Coalesce physically contiguous grains within this grain table.
        let mut len = to_grain_end;
        let mut next = gte;
        while len < avail && gte_idx + 1 < s.gtes {
            gte_idx += 1;
            let g = u64::from(self.meta.u32_le(s.file, gt + gte_idx * 4)?);
            if g != next + s.grain {
                break;
            }
            next = g;
            len = (len + grain_bytes).min(avail);
        }
        Ok((
            Run::Data {
                src: Arc::clone(self.meta.file(s.file)?),
                offset: gte * SECTOR + within,
            },
            len,
        ))
    }
}

impl BlockMap for VmdkMap {
    fn size(&self) -> u64 {
        self.size
    }

    fn map(&self, pos: u64) -> io::Result<(Run, u64)> {
        let idx = self
            .extents
            .partition_point(|e| e.start <= pos)
            .saturating_sub(1);
        let e = self
            .extents
            .get(idx)
            .ok_or_else(|| invalid(format!("no extent at offset {pos}")))?;
        let rel = pos - e.start;
        let avail = e.len.saturating_sub(rel);
        if avail == 0 {
            return Err(invalid(format!("no extent at offset {pos}")));
        }
        match &e.kind {
            Kind::Flat { file, offset } => Ok((
                Run::Data {
                    src: Arc::clone(self.meta.file(*file)?),
                    offset: offset.saturating_add(rel),
                },
                avail,
            )),
            Kind::Zero => Ok((Run::Zero, avail)),
            Kind::Sparse(s) => self.map_sparse(s, rel, avail),
        }
    }
}

/// An opened VMDK.
pub type VmdkSource = MappedSource<VmdkMap>;

/// Extent data handed to [`build`].
#[derive(Debug)]
pub struct ExtentData {
    pub line: ExtentLine,
    /// The opened extent file (`None` for ZERO extents).
    pub src: Option<SharedSource>,
}

/// Builds a reader from a descriptor's extents; `parent` serves
/// unallocated grains of a delta link (zeros if `None`).
pub fn build(
    extents: Vec<ExtentData>,
    has_parent: bool,
    parent: Option<SharedSource>,
) -> Result<VmdkSource, ImageError> {
    let mut files = Vec::new();
    let mut out = Vec::with_capacity(extents.len());
    let mut start = 0u64;
    for (i, ext) in extents.into_iter().enumerate() {
        let len = ext
            .line
            .sectors
            .checked_mul(SECTOR)
            .ok_or_else(|| corrupt(format!("extent {i} is too large")))?;
        let kind = match (&ext.line.kind, ext.src) {
            (ExtentType::Zero, _) => Kind::Zero,
            (ExtentType::Flat, Some(src)) => {
                files.push(src);
                Kind::Flat {
                    file: files.len() - 1,
                    offset: ext
                        .line
                        .offset
                        .checked_mul(SECTOR)
                        .ok_or_else(|| corrupt(format!("extent {i} offset overflows")))?,
                }
            }
            (ExtentType::Sparse, Some(src)) => {
                let h = parse_sparse(&*src)?;
                let gtes = u64::from(h.gtes_per_gt);
                let coverage = h.grain * gtes;
                let sectors = ext.line.sectors.max(h.capacity);
                let gd_entries = sectors.div_ceil(coverage);
                let gd_sector = if h.gd_offset != 0 {
                    h.gd_offset
                } else {
                    h.rgd_offset
                };
                if gd_sector == 0 {
                    return Err(corrupt(format!("extent {i} has no grain directory")));
                }
                files.push(src);
                Kind::Sparse(Sparse {
                    file: files.len() - 1,
                    grain: h.grain,
                    gtes,
                    gd_offset: gd_sector
                        .checked_mul(SECTOR)
                        .ok_or_else(|| corrupt("grain directory offset overflows"))?,
                    gd_entries,
                    compressed: h.flags & FLAG_COMPRESSED != 0,
                    zero_grain: h.flags & FLAG_ZERO_GRAIN != 0,
                })
            }
            (ExtentType::Other(t), _) => {
                return Err(ImageError::Corrupt {
                    format: ImageFormat::Vmdk,
                    reason: format!("extent type {t} is not supported"),
                });
            }
            (_, None) => return Err(corrupt(format!("extent {i} has no file"))),
        };
        if len > 0 {
            out.push(Extent { start, len, kind });
        }
        start = start
            .checked_add(len)
            .ok_or_else(|| corrupt("disk size overflows"))?;
    }
    let map = VmdkMap {
        size: start,
        extents: out,
        has_parent,
        meta: MetaPages::new(files, DEFAULT_PAGES),
        grains: Lru::new(GRAIN_CACHE),
    };
    Ok(MappedSource::new(map, parent))
}

/// Reads the descriptor of a VMDK: the file itself if it is a text
/// descriptor, else the descriptor embedded in a sparse extent. A sparse
/// extent without one is described as a single extent of itself.
fn read_descriptor(src: &dyn ReadAt, name: &str) -> Result<(Descriptor, bool), ImageError> {
    let mut magic = [0u8; 4];
    src.read_exact_at(0, &mut magic)
        .map_err(|_| corrupt("file too short"))?;
    if &magic == b"KDMV" {
        let h = parse_sparse(src)?;
        if h.descriptor_offset != 0 && h.descriptor_size != 0 {
            let len = h.descriptor_size.saturating_mul(SECTOR).min(MAX_DESCRIPTOR);
            let off = h
                .descriptor_offset
                .checked_mul(SECTOR)
                .ok_or_else(|| corrupt("descriptor offset overflows"))?;
            let raw = src
                .read_vec_at(off, len as usize)
                .map_err(|_| corrupt("embedded descriptor out of range"))?;
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            let text = String::from_utf8_lossy(raw.get(..end).unwrap_or_default());
            return Ok((parse_descriptor(&text)?, true));
        }
        let d = Descriptor {
            create_type: "monolithicSparse".into(),
            extents: vec![ExtentLine {
                access: "RW".into(),
                sectors: h.capacity,
                kind: ExtentType::Sparse,
                file: Some(name.to_string()),
                offset: 0,
            }],
            ..Descriptor::default()
        };
        return Ok((d, true));
    }
    if &magic == b"COWD" {
        return Err(ImageError::Corrupt {
            format: ImageFormat::Vmdk,
            reason: "ESX COWD (vmfsSparse) extents are not supported".into(),
        });
    }
    let len = src.size().min(MAX_DESCRIPTOR);
    let raw = src.read_vec_at(0, len as usize)?;
    let text = String::from_utf8_lossy(&raw);
    Ok((parse_descriptor(&text)?, false))
}

/// Opens a single-file VMDK (a monolithic sparse or streamOptimized
/// extent, with or without an embedded descriptor) from memory or any
/// source. Delta links read their unallocated grains from `parent`.
pub fn from_source(
    src: SharedSource,
    parent: Option<SharedSource>,
) -> Result<(VmdkSource, Descriptor), ImageError> {
    let (d, embedded) = read_descriptor(&*src, "")?;
    let mut own = Some(src);
    let mut extents = Vec::with_capacity(d.extents.len());
    for line in &d.extents {
        let src = match line.kind {
            ExtentType::Zero => None,
            ExtentType::Sparse if embedded && d.extents.len() == 1 => own.take(),
            _ => {
                return Err(corrupt(
                    "descriptor refers to extent files; open it from a path instead",
                ));
            }
        };
        extents.push(ExtentData {
            line: line.clone(),
            src,
        });
    }
    let has_parent = d.has_parent();
    Ok((build(extents, has_parent, parent)?, d))
}

fn open_file(path: &Path) -> Result<SharedSource, ImageError> {
    Ok(Arc::new(FileSource::open(path).map_err(|source| {
        ImageError::Open {
            path: path.to_path_buf(),
            source,
        }
    })?))
}

/// Opens a VMDK descriptor or sparse extent file, its extents and its
/// parent chain.
pub(crate) fn open_path(path: &Path, depth: usize) -> Result<Image, ImageError> {
    let src = open_file(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (d, embedded) = read_descriptor(&*src, &name)?;
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let single = d.extents.len() == 1;
    let mut paths: Vec<PathBuf> = vec![path.to_path_buf()];
    let mut extents = Vec::with_capacity(d.extents.len());
    for line in &d.extents {
        let src = match (&line.kind, &line.file) {
            (ExtentType::Zero, _) | (_, None) => None,
            (_, Some(f)) => {
                let p = dir.join(f.replace('\\', "/"));
                if embedded && (p == path || (single && !p.is_file())) {
                    // The descriptor's own extent (possibly renamed).
                    Some(Arc::clone(&src))
                } else {
                    if !p.is_file() {
                        return Err(ImageError::Open {
                            path: p,
                            source: io::Error::new(
                                io::ErrorKind::NotFound,
                                "VMDK extent file listed in the descriptor is missing",
                            ),
                        });
                    }
                    if !paths.contains(&p) {
                        paths.push(p.clone());
                    }
                    Some(open_file(&p)?)
                }
            }
        };
        extents.push(ExtentData {
            line: line.clone(),
            src,
        });
    }
    let mut metadata = vec![("Create type".to_string(), d.create_type.clone())];
    if let Some(cid) = &d.cid {
        metadata.push(("CID".into(), cid.clone()));
    }
    let has_parent = d.has_parent();
    let parent = if has_parent {
        let refs: Vec<String> = d.parent_hint.iter().cloned().collect();
        let p = parent::open(path, &refs, depth)?;
        metadata.push((
            "Parent image".into(),
            p.info
                .paths
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        if let (Some(want), Some((_, got))) = (
            &d.parent_cid,
            p.info.metadata.iter().find(|(k, _)| k == "CID"),
        ) && !want.eq_ignore_ascii_case(got)
        {
            tracing::warn!(
                child = %path.display(),
                parent_cid = %want,
                actual = %got,
                "VMDK parent CID does not match; the parent may have changed since the snapshot"
            );
        }
        paths.extend(p.info.paths.iter().cloned());
        Some(p.source())
    } else {
        None
    };
    let source = build(extents, has_parent, parent)?;
    let size = source.size();
    let source: SharedSource = Arc::new(source);
    Ok(Image::new(
        ImageInfo {
            format: ImageFormat::Vmdk,
            paths,
            size,
            sector_size: 512,
            stored_hashes: Vec::new(),
            metadata,
        },
        source,
    ))
}

/// Flag constants (for tests building images by hand).
pub mod flags {
    pub const ZERO_GRAIN: u32 = super::FLAG_ZERO_GRAIN;
    pub const COMPRESSED: u32 = super::FLAG_COMPRESSED;
    pub const MARKERS: u32 = super::FLAG_MARKERS;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
    use super::*;

    const DESC: &str = r#"# Disk DescriptorFile
version=1
encoding="UTF-8"
CID=7e5d1f3a
parentCID=ffffffff
createType="twoGbMaxExtentSparse"

# Extent description
RW 4192256 SPARSE "disk-s001.vmdk"
RW 2048 FLAT "my disk-flat.vmdk" 128
RW 100 ZERO
RDONLY 8 VMFSSPARSE "x-delta.vmdk"

# The Disk Data Base
ddb.adapterType = "ide"
"#;

    #[test]
    fn parses_descriptor_lines() {
        let d = parse_descriptor(DESC).unwrap();
        assert_eq!(d.create_type, "twoGbMaxExtentSparse");
        assert_eq!(d.cid.as_deref(), Some("7e5d1f3a"));
        assert!(!d.has_parent());
        assert_eq!(d.extents.len(), 4);
        assert_eq!(d.extents[0].kind, ExtentType::Sparse);
        assert_eq!(d.extents[1].file.as_deref(), Some("my disk-flat.vmdk"));
        assert_eq!(d.extents[1].offset, 128);
        assert_eq!(d.extents[2].kind, ExtentType::Zero);
        assert_eq!(d.extents[2].file, None);
        assert_eq!(d.extents[3].kind, ExtentType::Other("VMFSSPARSE".into()));
    }

    #[test]
    fn delta_link_has_parent() {
        let d = parse_descriptor(
            "parentCID=12345678\nparentFileNameHint=\"base.vmdk\"\nRW 8 SPARSE \"d.vmdk\"\n",
        )
        .unwrap();
        assert!(d.has_parent());
        assert_eq!(d.parent_hint.as_deref(), Some("base.vmdk"));
    }

    #[test]
    fn descriptor_without_extents_is_corrupt() {
        assert!(parse_descriptor("# Disk DescriptorFile\nversion=1\n").is_err());
    }

    #[test]
    fn unsupported_extent_type_is_reported() {
        let d = parse_descriptor("RW 8 SESPARSE \"x.vmdk\"\n").unwrap();
        let e = build(
            vec![ExtentData {
                line: d.extents[0].clone(),
                src: None,
            }],
            false,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("SESPARSE"), "{e}");
    }
}
