//! Microsoft VHDX (MS-VHDX v1): fixed, dynamic and differencing disks.
//!
//! Layout (little-endian): the `vhdxfile` identifier at 0, two 4 KiB
//! headers at 64 KiB and 128 KiB (the valid one with the higher sequence
//! number wins), two region tables at 192 KiB and 256 KiB locating the Block
//! Allocation Table (BAT) and the metadata region. The BAT interleaves
//! payload-block entries with one sector-bitmap entry per *chunk*
//! (`2^23 × logical sector size / block size` payload blocks).
//!
//! The log is not replayed (read-only access never needs it for a cleanly
//! closed disk); a non-empty log is reported as a warning because the image
//! may then lack its most recent metadata updates.

use std::io;
use std::path::Path;
use std::sync::Arc;

use pm_core::decode::{to_hex, utf16le_lossy};
use pm_core::io::FileSource;
use pm_core::{Bytes, ReadAt, SharedSource};

use crate::cache::{DEFAULT_PAGES, MetaPages};
use crate::mapped::{BlockMap, MappedSource, Run};
use crate::{Image, ImageError, ImageFormat, ImageInfo, parent};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// On-disk (mixed-endian) form of a GUID written as
/// `d1-d2-d3-d4[0..2]-d4[2..8]`.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const BAT_REGION: [u8; 16] = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const METADATA_REGION: [u8; 16] = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const FILE_PARAMETERS: [u8; 16] = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const VIRTUAL_DISK_SIZE: [u8; 16] = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const VIRTUAL_DISK_ID: [u8; 16] = guid(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const LOGICAL_SECTOR_SIZE: [u8; 16] = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const PARENT_LOCATOR: [u8; 16] = guid(
    0xA8D3_5F2D,
    0xB30B,
    0x454D,
    [0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C],
);

const MAX_REGION_ENTRIES: u32 = 2047;
const MAX_METADATA_ENTRIES: u16 = 2047;

// Payload block states (BAT entry bits 0..3).
const PAYLOAD_NOT_PRESENT: u64 = 0;
const PAYLOAD_UNDEFINED: u64 = 1;
const PAYLOAD_ZERO: u64 = 2;
const PAYLOAD_UNMAPPED: u64 = 3;
const PAYLOAD_FULLY_PRESENT: u64 = 6;
const PAYLOAD_PARTIALLY_PRESENT: u64 = 7;
const SB_PRESENT: u64 = 6;

fn corrupt(reason: impl Into<String>) -> ImageError {
    ImageError::Corrupt {
        format: ImageFormat::Vhdx,
        reason: reason.into(),
    }
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn trunc(e: pm_core::Truncated) -> ImageError {
    corrupt(e.to_string())
}

/// CRC-32C (Castagnoli), as used by VHDX headers and region tables.
#[allow(clippy::indexing_slicing, reason = "const table construction, i < 256")]
pub fn crc32c(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut t = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0x82F6_3B78
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut crc = !0u32;
    for &b in data {
        let idx = ((crc ^ u32::from(b)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE.get(idx).copied().unwrap_or(0);
    }
    !crc
}

/// CRC with the 4-byte checksum field at `field` taken as zero.
fn crc_without(data: &[u8], field: usize) -> u32 {
    let mut copy = data.to_vec();
    if let Some(f) = copy.get_mut(field..field + 4) {
        f.fill(0);
    }
    crc32c(&copy)
}

/// Parsed VHDX headers and metadata.
#[derive(Debug, Clone)]
pub struct VhdxHeader {
    pub size: u64,
    pub block_size: u32,
    pub logical_sector_size: u32,
    pub has_parent: bool,
    /// The log GUID of the current header is set: the log may hold updates
    /// that were never applied (the disk was not closed cleanly).
    pub dirty_log: bool,
    pub disk_id: [u8; 16],
    pub data_write_guid: [u8; 16],
    /// Parent locator entries (key, value).
    pub parent_locator: Vec<(String, String)>,
    bat_offset: u64,
    bat_len: u64,
}

impl VhdxHeader {
    /// Parent references in resolution order.
    pub fn parent_refs(&self) -> Vec<String> {
        ["relative_path", "absolute_win32_path", "volume_path"]
            .iter()
            .filter_map(|k| {
                self.parent_locator
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            })
            .collect()
    }
}

fn read_header(src: &dyn ReadAt, off: u64) -> Option<(u64, Vec<u8>)> {
    let h = src.read_vec_at(off, 4096).ok()?;
    if !h.starts_with(b"head") {
        return None;
    }
    let stored = h.u32_le(4).ok()?;
    if stored != crc_without(&h, 4) {
        tracing::warn!(
            offset = off,
            "VHDX header checksum mismatch; ignoring this copy"
        );
        return None;
    }
    Some((h.u64_le(8).ok()?, h))
}

fn read_region_table(src: &dyn ReadAt) -> Result<Vec<u8>, ImageError> {
    let mut fallback = None;
    for off in [192 * KIB, 256 * KIB] {
        let Ok(t) = src.read_vec_at(off, 64 * KIB as usize) else {
            continue;
        };
        if !t.starts_with(b"regi") {
            continue;
        }
        if t.u32_le(4).ok() == Some(crc_without(&t, 4)) {
            return Ok(t);
        }
        fallback.get_or_insert(t);
    }
    match fallback {
        Some(t) => {
            tracing::warn!("VHDX region table checksums do not match; using the first copy");
            Ok(t)
        }
        None => Err(corrupt("no region table")),
    }
}

/// Parses headers, region table and metadata.
pub fn parse(src: &dyn ReadAt) -> Result<VhdxHeader, ImageError> {
    let mut ident = [0u8; 8];
    src.read_exact_at(0, &mut ident)
        .map_err(|_| corrupt("file too short"))?;
    if &ident != b"vhdxfile" {
        return Err(corrupt("missing `vhdxfile` identifier"));
    }
    let h1 = read_header(src, 64 * KIB);
    let h2 = read_header(src, 128 * KIB);
    let (_, head) = match (h1, h2) {
        (Some(a), Some(b)) => {
            if b.0 > a.0 {
                b
            } else {
                a
            }
        }
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => return Err(corrupt("no valid header")),
    };
    let version = head.u16_le(66).map_err(trunc)?;
    if version != 1 {
        return Err(corrupt(format!("unsupported version {version}")));
    }
    let data_write_guid = head.array::<16>(32).map_err(trunc)?;
    let dirty_log = head.sub(48, 16).map_err(trunc)?.iter().any(|&b| b != 0);

    let rt = read_region_table(src)?;
    let count = rt.u32_le(8).map_err(trunc)?;
    if count > MAX_REGION_ENTRIES {
        return Err(corrupt(format!("{count} region table entries")));
    }
    let (mut bat, mut meta) = (None, None);
    for i in 0..count as usize {
        let e = rt.sub(16 + i * 32, 32).map_err(trunc)?;
        let id = e.array::<16>(0).map_err(trunc)?;
        let off = e.u64_le(16).map_err(trunc)?;
        let len = u64::from(e.u32_le(24).map_err(trunc)?);
        let required = e.u32_le(28).map_err(trunc)? & 1 != 0;
        if id == BAT_REGION {
            bat = Some((off, len));
        } else if id == METADATA_REGION {
            meta = Some((off, len));
        } else if required {
            return Err(corrupt(format!("unknown required region {}", to_hex(&id))));
        }
    }
    let (bat_offset, bat_len) = bat.ok_or_else(|| corrupt("no BAT region"))?;
    let (meta_off, meta_len) = meta.ok_or_else(|| corrupt("no metadata region"))?;
    if meta_len > 64 * MIB {
        return Err(corrupt(format!("metadata region of {meta_len} bytes")));
    }
    let md = src
        .read_vec_at(meta_off, meta_len as usize)
        .map_err(|_| corrupt("metadata region out of range"))?;
    if !md.starts_with(b"metadata") {
        return Err(corrupt("metadata table signature missing"));
    }
    let entries = md.u16_le(10).map_err(trunc)?;
    if entries > MAX_METADATA_ENTRIES {
        return Err(corrupt(format!("{entries} metadata entries")));
    }
    let item = |want: [u8; 16]| -> Result<Option<&[u8]>, ImageError> {
        for i in 0..usize::from(entries) {
            let e = md.sub(32 + i * 32, 32).map_err(trunc)?;
            if e.array::<16>(0).map_err(trunc)? != want {
                continue;
            }
            let off = e.u32_le(16).map_err(trunc)? as usize;
            let len = e.u32_le(20).map_err(trunc)? as usize;
            return md.sub(off, len).map(Some).map_err(trunc);
        }
        Ok(None)
    };
    let fp = item(FILE_PARAMETERS)?.ok_or_else(|| corrupt("no file parameters"))?;
    let block_size = fp.u32_le(0).map_err(trunc)?;
    let has_parent = fp.u32_le(4).map_err(trunc)? & 2 != 0;
    if !(MIB..=256 * MIB).contains(&u64::from(block_size)) || !block_size.is_power_of_two() {
        return Err(corrupt(format!("invalid block size {block_size}")));
    }
    let size = item(VIRTUAL_DISK_SIZE)?
        .ok_or_else(|| corrupt("no virtual disk size"))?
        .u64_le(0)
        .map_err(trunc)?;
    let logical_sector_size = match item(LOGICAL_SECTOR_SIZE)? {
        Some(b) => b.u32_le(0).map_err(trunc)?,
        None => 512,
    };
    if logical_sector_size != 512 && logical_sector_size != 4096 {
        return Err(corrupt(format!(
            "invalid logical sector size {logical_sector_size}"
        )));
    }
    let disk_id = match item(VIRTUAL_DISK_ID)? {
        Some(b) => b.array::<16>(0).map_err(trunc)?,
        None => [0; 16],
    };
    let mut parent_locator = Vec::new();
    if has_parent && let Some(pl) = item(PARENT_LOCATOR)? {
        let n = pl.u16_le(18).map_err(trunc)?;
        for i in 0..usize::from(n) {
            let e = pl.sub(20 + i * 12, 12).map_err(trunc)?;
            let ko = e.u32_le(0).map_err(trunc)? as usize;
            let vo = e.u32_le(4).map_err(trunc)? as usize;
            let kl = usize::from(e.u16_le(8).map_err(trunc)?);
            let vl = usize::from(e.u16_le(10).map_err(trunc)?);
            let key = utf16le_lossy(pl.sub(ko, kl).map_err(trunc)?);
            let value = utf16le_lossy(pl.sub(vo, vl).map_err(trunc)?);
            parent_locator.push((key, value));
        }
    }
    Ok(VhdxHeader {
        size,
        block_size,
        logical_sector_size,
        has_parent,
        dirty_log,
        disk_id,
        data_write_guid,
        parent_locator,
        bat_offset,
        bat_len,
    })
}

/// VHDX block map.
#[derive(Debug)]
pub struct VhdxMap {
    size: u64,
    block_size: u64,
    sector: u64,
    chunk_ratio: u64,
    has_parent: bool,
    bat_offset: u64,
    bat_len: u64,
    meta: MetaPages,
}

impl VhdxMap {
    fn bat(&self, index: u64) -> io::Result<u64> {
        let off = index
            .checked_mul(8)
            .filter(|o| o + 8 <= self.bat_len)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("VHDX BAT entry {index} beyond the BAT region"),
                )
            })?;
        self.meta.u64_le(0, self.bat_offset.saturating_add(off))
    }
}

impl BlockMap for VhdxMap {
    fn size(&self) -> u64 {
        self.size
    }

    fn map(&self, pos: u64) -> io::Result<(Run, u64)> {
        let absent = if self.has_parent {
            Run::Parent
        } else {
            Run::Zero
        };
        let block = pos / self.block_size;
        let within = pos % self.block_size;
        let to_end = (self.block_size - within).min(self.size - pos);
        let entry = self.bat(block + block / self.chunk_ratio)?;
        let file_off = entry & !(MIB - 1);
        match entry & 7 {
            PAYLOAD_FULLY_PRESENT => Ok((
                Run::Data {
                    src: Arc::clone(self.meta.file(0)?),
                    offset: file_off.saturating_add(within),
                },
                to_end,
            )),
            PAYLOAD_PARTIALLY_PRESENT if self.has_parent => {
                let chunk = block / self.chunk_ratio;
                let sb = self.bat(chunk * (self.chunk_ratio + 1) + self.chunk_ratio)?;
                if sb & 7 != SB_PRESENT {
                    return Ok((Run::Parent, to_end));
                }
                let bitmap = sb & !(MIB - 1);
                let chunk_bytes = self.chunk_ratio * self.block_size;
                let in_chunk = pos % chunk_bytes;
                let first = in_chunk / self.sector;
                let last = (in_chunk + to_end - 1) / self.sector;
                let bit = |s: u64| -> io::Result<bool> {
                    let byte = self.meta.u8(0, bitmap.saturating_add(s / 8))?;
                    Ok(byte & (1 << (s % 8)) != 0)
                };
                let present = bit(first)?;
                let mut s = first + 1;
                while s <= last {
                    if s.is_multiple_of(8) && s + 7 <= last {
                        let byte = self.meta.u8(0, bitmap.saturating_add(s / 8))?;
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
                let len = (s * self.sector).min(in_chunk + to_end) - in_chunk;
                Ok(if present {
                    (
                        Run::Data {
                            src: Arc::clone(self.meta.file(0)?),
                            offset: file_off.saturating_add(within),
                        },
                        len,
                    )
                } else {
                    (Run::Parent, len)
                })
            }
            PAYLOAD_NOT_PRESENT | PAYLOAD_UNDEFINED => Ok((absent, to_end)),
            PAYLOAD_ZERO | PAYLOAD_UNMAPPED | PAYLOAD_PARTIALLY_PRESENT => Ok((Run::Zero, to_end)),
            state => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("VHDX block {block} has invalid state {state}"),
            )),
        }
    }
}

/// An opened VHDX.
pub type VhdxSource = MappedSource<VhdxMap>;

/// Opens a VHDX from `src`; a differencing disk reads absent sectors from
/// `parent` (zeros if `None`).
pub fn from_source(
    src: SharedSource,
    parent: Option<SharedSource>,
) -> Result<(VhdxSource, VhdxHeader), ImageError> {
    let h = parse(&*src)?;
    Ok((with_header(src, &h, parent), h))
}

fn with_header(src: SharedSource, h: &VhdxHeader, parent: Option<SharedSource>) -> VhdxSource {
    let block_size = u64::from(h.block_size);
    let sector = u64::from(h.logical_sector_size);
    // Validated: block size 1..256 MiB, sector 512/4096 → ratio ≥ 16.
    let chunk_ratio = ((1u64 << 23) * sector / block_size).max(1);
    let map = VhdxMap {
        size: h.size,
        block_size,
        sector,
        chunk_ratio,
        has_parent: h.has_parent,
        bat_offset: h.bat_offset,
        bat_len: h.bat_len,
        meta: MetaPages::new(vec![src], DEFAULT_PAGES),
    };
    MappedSource::new(map, parent)
}

/// Opens a VHDX file, following differencing parents.
pub(crate) fn open_path(path: &Path, depth: usize) -> Result<Image, ImageError> {
    let src: SharedSource =
        Arc::new(FileSource::open(path).map_err(|source| ImageError::Open {
            path: path.to_path_buf(),
            source,
        })?);
    let h = parse(&*src)?;
    let mut paths = vec![path.to_path_buf()];
    let kind = if h.has_parent {
        "differencing"
    } else {
        "dynamic or fixed"
    };
    let mut metadata = vec![
        ("Disk type".to_string(), kind.to_string()),
        ("Virtual disk ID".to_string(), to_hex(&h.disk_id)),
        ("Block size".to_string(), h.block_size.to_string()),
    ];
    if h.dirty_log {
        tracing::warn!(
            image = %path.display(),
            "VHDX log is not empty (disk not closed cleanly); the log is not replayed and recent writes may be missing"
        );
        metadata.push((
            "Warning".into(),
            "log not replayed: the disk was not closed cleanly and recent writes may be missing"
                .into(),
        ));
    }
    let parent = if h.has_parent {
        let p = parent::open(path, &h.parent_refs(), depth)?;
        metadata.push((
            "Parent image".into(),
            p.info
                .paths
                .first()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ));
        paths.extend(p.info.paths.iter().cloned());
        Some(p.source())
    } else {
        None
    };
    let source: SharedSource = Arc::new(with_header(src, &h, parent));
    Ok(Image::new(
        ImageInfo {
            format: ImageFormat::Vhdx,
            paths,
            size: h.size,
            sector_size: h.logical_sector_size,
            stored_hashes: Vec::new(),
            metadata,
        },
        source,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_known_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn guid_layout_matches_spec() {
        assert_eq!(to_hex(&BAT_REGION), "6677c22d23f600429d64115e9bfd4a08");
    }
}
