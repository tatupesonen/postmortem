//! QEMU Copy-On-Write version 2 and 3 (QCOW2).
//!
//! Layout (big-endian): a header (`QFI\xfb`), a two-level map from guest
//! clusters to host clusters (L1 table → L2 tables), optional compressed
//! clusters (raw deflate, or zstd when the v3 compression type says so),
//! extended L2 entries with 32 subclusters, and an optional backing file
//! that supplies unallocated clusters. Snapshots are ignored (the current
//! state is read). Encrypted images and external data files are refused.

use std::io;
use std::path::Path;
use std::sync::Arc;

use flate2::{Decompress, FlushDecompress};
use pm_core::io::FileSource;
use pm_core::{Bytes, ReadAt, SharedSource};

use crate::cache::{DEFAULT_PAGES, Lru, MetaPages};
use crate::mapped::{BlockMap, MappedSource, Run};
use crate::{Image, ImageError, ImageFormat, ImageInfo, parent};

/// Host offset bits of standard L1/L2 entries (9..55).
const OFFSET_MASK: u64 = 0x00FF_FFFF_FFFF_FE00;
const L2_COMPRESSED: u64 = 1 << 62;
const L2_ZERO: u64 = 1;
const INCOMPAT_DIRTY: u64 = 1;
const INCOMPAT_CORRUPT: u64 = 1 << 1;
const INCOMPAT_DATA_FILE: u64 = 1 << 2;
const INCOMPAT_COMPRESSION: u64 = 1 << 3;
const INCOMPAT_EXTL2: u64 = 1 << 4;
const KNOWN_INCOMPAT: u64 =
    INCOMPAT_DIRTY | INCOMPAT_CORRUPT | INCOMPAT_DATA_FILE | INCOMPAT_COMPRESSION | INCOMPAT_EXTL2;
const EXT_END: u32 = 0;
const EXT_FULL_DISK_ENCRYPTION: u32 = 0x0537_be77;
const EXT_DATA_FILE: u32 = 0x4441_5441;
/// Decompressed clusters cached: up to this many bytes.
const CLUSTER_CACHE_BYTES: usize = 8 << 20;

fn corrupt(reason: impl Into<String>) -> ImageError {
    ImageError::Corrupt {
        format: ImageFormat::Qcow,
        reason: reason.into(),
    }
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` callback")]
fn trunc(e: pm_core::Truncated) -> ImageError {
    corrupt(e.to_string())
}

/// Cluster compression of a v3 image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Deflate,
    Zstd,
}

/// Parsed QCOW2 header.
#[derive(Debug, Clone)]
pub struct QcowHeader {
    pub version: u32,
    pub cluster_bits: u32,
    pub size: u64,
    pub l1_size: u32,
    pub l1_offset: u64,
    pub backing_file: Option<String>,
    pub compression: Compression,
    pub extended_l2: bool,
    pub snapshots: u32,
    /// Image was not closed cleanly (lazy refcounts); mapping is still valid.
    pub dirty: bool,
}

/// Parses and validates a QCOW2 header.
pub fn parse(src: &dyn ReadAt) -> Result<QcowHeader, ImageError> {
    let mut b = vec![0u8; 112];
    let n = src.read_at(0, &mut b)?;
    b.truncate(n);
    if !b.starts_with(b"QFI\xfb") {
        return Err(corrupt("signature `QFI\\xfb` missing"));
    }
    let version = b.u32_be(4).map_err(trunc)?;
    if version == 1 {
        return Err(ImageError::Unsupported {
            format: ImageFormat::Qcow,
        });
    }
    if version != 2 && version != 3 {
        return Err(corrupt(format!("unknown version {version}")));
    }
    let backing_offset = b.u64_be(8).map_err(trunc)?;
    let backing_size = b.u32_be(16).map_err(trunc)?;
    let cluster_bits = b.u32_be(20).map_err(trunc)?;
    if !(9..=21).contains(&cluster_bits) {
        return Err(corrupt(format!("invalid cluster bits {cluster_bits}")));
    }
    let size = b.u64_be(24).map_err(trunc)?;
    let crypt = b.u32_be(32).map_err(trunc)?;
    if crypt != 0 {
        let method = match crypt {
            1 => "AES-CBC",
            2 => "LUKS",
            _ => "unknown method",
        };
        return Err(ImageError::Encrypted {
            format: ImageFormat::Qcow,
            method: method.into(),
        });
    }
    let l1_size = b.u32_be(36).map_err(trunc)?;
    let l1_offset = b.u64_be(40).map_err(trunc)?;
    let snapshots = b.u32_be(60).map_err(trunc)?;
    let (mut incompat, mut header_len, mut compression_type) = (0u64, 72u32, 0u8);
    if version == 3 {
        incompat = b.u64_be(72).map_err(trunc)?;
        header_len = b.u32_be(100).map_err(trunc)?;
        if header_len < 104 {
            return Err(corrupt(format!("invalid header length {header_len}")));
        }
        if header_len > 104 {
            compression_type = b.u8_at(104).map_err(trunc)?;
        }
    }
    if incompat & !KNOWN_INCOMPAT != 0 {
        return Err(corrupt(format!(
            "unknown incompatible features {:#x}",
            incompat & !KNOWN_INCOMPAT
        )));
    }
    if incompat & INCOMPAT_CORRUPT != 0 {
        tracing::warn!("QCOW2 image is marked corrupt by QEMU; reading it anyway");
    }
    if incompat & INCOMPAT_DATA_FILE != 0 {
        return Err(corrupt("external data files are not supported"));
    }
    let compression = match compression_type {
        0 => Compression::Deflate,
        1 => Compression::Zstd,
        t => return Err(corrupt(format!("unknown compression type {t}"))),
    };
    // Header extensions (v3 after header_len, v2 after 72 bytes).
    let mut off = u64::from(header_len);
    let cluster = 1u64 << cluster_bits;
    for _ in 0..64 {
        if off + 8 > cluster {
            break;
        }
        let mut e = [0u8; 8];
        if src.read_exact_at(off, &mut e).is_err() {
            break;
        }
        let kind = e.u32_be(0).map_err(trunc)?;
        let len = u64::from(e.u32_be(4).map_err(trunc)?);
        match kind {
            EXT_END => break,
            EXT_FULL_DISK_ENCRYPTION => {
                return Err(ImageError::Encrypted {
                    format: ImageFormat::Qcow,
                    method: "LUKS".into(),
                });
            }
            EXT_DATA_FILE => return Err(corrupt("external data files are not supported")),
            _ => {}
        }
        off = off + 8 + len.next_multiple_of(8);
    }
    let backing_file = if backing_offset != 0 && backing_size != 0 {
        if backing_size > 1023 {
            return Err(corrupt(format!(
                "backing file name of {backing_size} bytes"
            )));
        }
        let raw = src
            .read_vec_at(backing_offset, backing_size as usize)
            .map_err(|_| corrupt("backing file name out of range"))?;
        Some(String::from_utf8_lossy(&raw).into_owned())
    } else {
        None
    };
    // Each L1 entry covers cluster * (cluster / entry_size) bytes.
    let l2_entry = if incompat & INCOMPAT_EXTL2 != 0 {
        16
    } else {
        8
    };
    let per_l1 = cluster.saturating_mul(cluster / l2_entry);
    let needed = size.div_ceil(per_l1);
    if u64::from(l1_size) < needed {
        return Err(corrupt(format!(
            "L1 table has {l1_size} entries, the disk needs {needed}"
        )));
    }
    Ok(QcowHeader {
        version,
        cluster_bits,
        size,
        l1_size,
        l1_offset,
        backing_file,
        compression,
        extended_l2: incompat & INCOMPAT_EXTL2 != 0,
        snapshots,
        dirty: incompat & INCOMPAT_DIRTY != 0,
    })
}

/// QCOW2 block map.
#[derive(Debug)]
pub struct QcowMap {
    size: u64,
    cluster_bits: u32,
    l2_entries: u64,
    l2_entry_size: u64,
    l1_offset: u64,
    l1_size: u64,
    extended: bool,
    compression: Compression,
    meta: MetaPages,
    clusters: Lru<u64, Arc<[u8]>>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl QcowMap {
    fn cluster(&self) -> u64 {
        1 << self.cluster_bits
    }

    fn decompress(&self, entry: u64) -> io::Result<Arc<[u8]>> {
        let cb = u64::from(self.cluster_bits);
        let x = 62 - (cb - 8);
        let host = entry & ((1u64 << x) - 1);
        if let Some(c) = self.clusters.get(&host) {
            return Ok(c);
        }
        let sectors = ((entry >> x) & ((1u64 << (cb - 8)) - 1)) + 1;
        let len = (sectors * 512).saturating_sub(host & 511);
        let src = self.meta.file(0)?;
        let avail = src.size().saturating_sub(host).min(len);
        let raw = src.read_vec_at(host, avail as usize)?;
        let mut out = vec![0u8; self.cluster() as usize];
        match self.compression {
            Compression::Deflate => {
                let mut z = Decompress::new(false);
                z.decompress(&raw, &mut out, FlushDecompress::Finish)
                    .map_err(|e| invalid(format!("compressed cluster at {host}: {e}")))?;
            }
            Compression::Zstd => {
                let mut dec = ruzstd::decoding::StreamingDecoder::new(raw.as_slice())
                    .map_err(|e| invalid(format!("zstd cluster at {host}: {e}")))?;
                let mut filled = 0usize;
                loop {
                    let dst = out.get_mut(filled..).unwrap_or_default();
                    if dst.is_empty() {
                        break;
                    }
                    match io::Read::read(&mut dec, dst) {
                        Ok(0) => break,
                        Ok(n) => filled += n,
                        Err(e) => return Err(invalid(format!("zstd cluster at {host}: {e}"))),
                    }
                }
            }
        }
        let c: Arc<[u8]> = out.into();
        self.clusters.insert(host, Arc::clone(&c));
        Ok(c)
    }
}

impl BlockMap for QcowMap {
    fn size(&self) -> u64 {
        self.size
    }

    fn map(&self, pos: u64) -> io::Result<(Run, u64)> {
        let cluster = self.cluster();
        let span = cluster * self.l2_entries;
        let l1_idx = pos / span;
        let mut l2_idx = (pos % span) / cluster;
        let within = pos % cluster;
        let avail = self.size - pos;
        if l1_idx >= self.l1_size {
            return Ok((Run::Parent, (span - pos % span).min(avail)));
        }
        let l1 = self.meta.u64_be(0, self.l1_offset + l1_idx * 8)? & OFFSET_MASK;
        if l1 == 0 {
            return Ok((Run::Parent, (span - pos % span).min(avail)));
        }
        if !l1.is_multiple_of(cluster) {
            return Err(invalid(format!("misaligned L2 table at {l1}")));
        }
        let entry_at = |i: u64| -> io::Result<(u64, u64)> {
            let at = l1 + i * self.l2_entry_size;
            let e = self.meta.u64_be(0, at)?;
            let bitmap = if self.extended {
                self.meta.u64_be(0, at + 8)?
            } else {
                0
            };
            Ok((e, bitmap))
        };
        let (entry, bitmap) = entry_at(l2_idx)?;
        let to_cluster_end = (cluster - within).min(avail);
        if entry & L2_COMPRESSED != 0 {
            let data = self.decompress(entry & !(L2_COMPRESSED | (1 << 63)))?;
            return Ok((
                Run::Buffer {
                    data,
                    offset: within as usize,
                },
                to_cluster_end,
            ));
        }
        let host = entry & OFFSET_MASK;
        if self.extended {
            // 32 subclusters: bit i allocated, bit 32+i reads as zeros.
            let sub = cluster / 32;
            let i = within / sub;
            let state = |j: u64| ((bitmap >> j) & 1, (bitmap >> (32 + j)) & 1);
            let first = state(i);
            let mut j = i + 1;
            while j < 32 && state(j) == first {
                j += 1;
            }
            let len = (j * sub - within).min(avail);
            return Ok(match first {
                (1, _) if host != 0 => (
                    Run::Data {
                        src: Arc::clone(self.meta.file(0)?),
                        offset: host + within,
                    },
                    len,
                ),
                (_, 1) => (Run::Zero, len),
                _ => (Run::Parent, len),
            });
        }
        if entry & L2_ZERO != 0 {
            return Ok((Run::Zero, to_cluster_end));
        }
        if host == 0 {
            return Ok((Run::Parent, to_cluster_end));
        }
        // Coalesce physically contiguous clusters of this L2 table.
        let mut len = to_cluster_end;
        let mut next = host;
        while len < avail && l2_idx + 1 < self.l2_entries {
            l2_idx += 1;
            let (e, _) = entry_at(l2_idx)?;
            if e & (L2_COMPRESSED | L2_ZERO) != 0 || e & OFFSET_MASK != next + cluster {
                break;
            }
            next += cluster;
            len = (len + cluster).min(avail);
        }
        Ok((
            Run::Data {
                src: Arc::clone(self.meta.file(0)?),
                offset: host + within,
            },
            len,
        ))
    }
}

/// An opened QCOW2 image.
pub type QcowSource = MappedSource<QcowMap>;

/// Opens a QCOW2 image from `src`; unallocated clusters read from
/// `backing` (zeros if `None`).
pub fn from_source(
    src: SharedSource,
    backing: Option<SharedSource>,
) -> Result<(QcowSource, QcowHeader), ImageError> {
    let h = parse(&*src)?;
    Ok((with_header(src, &h, backing), h))
}

fn with_header(src: SharedSource, h: &QcowHeader, backing: Option<SharedSource>) -> QcowSource {
    let cluster = 1usize << h.cluster_bits;
    let l2_entry_size = if h.extended_l2 { 16 } else { 8 };
    let map = QcowMap {
        size: h.size,
        cluster_bits: h.cluster_bits,
        l2_entries: (cluster as u64) / l2_entry_size,
        l2_entry_size,
        l1_offset: h.l1_offset,
        l1_size: u64::from(h.l1_size),
        extended: h.extended_l2,
        compression: h.compression,
        meta: MetaPages::new(vec![src], DEFAULT_PAGES),
        clusters: Lru::new((CLUSTER_CACHE_BYTES / cluster).max(8)),
    };
    MappedSource::new(map, backing)
}

/// Opens a QCOW2 file and its backing chain.
pub(crate) fn open_path(path: &Path, depth: usize) -> Result<Image, ImageError> {
    let src: SharedSource =
        Arc::new(FileSource::open(path).map_err(|source| ImageError::Open {
            path: path.to_path_buf(),
            source,
        })?);
    let h = parse(&*src)?;
    let mut paths = vec![path.to_path_buf()];
    let mut metadata = vec![
        ("Version".to_string(), h.version.to_string()),
        (
            "Cluster size".to_string(),
            (1u64 << h.cluster_bits).to_string(),
        ),
    ];
    if h.snapshots > 0 {
        metadata.push(("Internal snapshots".into(), h.snapshots.to_string()));
    }
    if h.dirty {
        tracing::warn!(image = %path.display(), "QCOW2 image is marked dirty (not closed cleanly)");
    }
    let backing = match &h.backing_file {
        Some(b) => {
            let p = parent::open(path, std::slice::from_ref(b), depth)?;
            metadata.push((
                "Backing file".into(),
                p.info
                    .paths
                    .first()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ));
            paths.extend(p.info.paths.iter().cloned());
            Some(p.source())
        }
        None => None,
    };
    let source: SharedSource = Arc::new(with_header(src, &h, backing));
    Ok(Image::new(
        ImageInfo {
            format: ImageFormat::Qcow,
            paths,
            size: h.size,
            sector_size: 512,
            stored_hashes: Vec::new(),
            metadata,
        },
        source,
    ))
}
