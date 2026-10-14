//! Expert Witness Compression Format, version 1 (`.E01`, `.s01`, `.L01`
//! media data).
//!
//! An image is a chain of segment files. Each segment is a list of sections
//! (`header2`, `header`, `volume`/`disk`, `sectors`, `table`, `table2`,
//! `next`/`done`, `hash`, `digest`, `error2`, …). Media data is stored in
//! fixed-size chunks (usually 32 KiB) that are either zlib-compressed or
//! stored with a trailing Adler-32. `table` sections hold the chunk offsets.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::{Decompress, FlushDecompress, Status};
use parking_lot::Mutex;
use pm_core::decode::{cstr_lossy, to_hex};
use pm_core::io::FileSource;
use pm_core::{Bytes, ReadAt, SharedSource};

use crate::{HashAlgo, ImageError, ImageFormat};

const SECTION_DESCRIPTOR: u64 = 76;
const FILE_HEADER: u64 = 13;
/// Refuse absurd chunk sizes from corrupt volume sections.
const MAX_CHUNK_SIZE: u64 = 64 * 1024 * 1024;
/// Refuse tables claiming more entries than this (corrupt headers).
const MAX_TABLE_ENTRIES: u32 = 16 * 1024 * 1024;
/// Cached decompressed chunks (16 shards × 16 = 256 chunks, 8 MiB at 32 KiB).
const CACHE_SHARDS: usize = 16;
const CACHE_PER_SHARD: usize = 16;

fn corrupt(reason: impl Into<String>) -> ImageError {
    ImageError::Corrupt {
        format: ImageFormat::Ewf,
        reason: reason.into(),
    }
}

/// Where one chunk's bytes live.
#[derive(Debug, Clone, Copy)]
struct ChunkLoc {
    segment: u16,
    offset: u64,
    /// Stored length (compressed data, or raw data + 4-byte checksum).
    len: u64,
    compressed: bool,
}

/// A section descriptor.
#[derive(Debug, Clone)]
struct Section {
    kind: String,
    start: u64,
    /// Exclusive end (start of the next section).
    end: u64,
}

/// Recently decompressed chunks: `(chunk index, data)`, oldest first.
type CacheShard = VecDeque<(u64, Arc<[u8]>)>;

/// Parsed, ready-to-read EWF image.
#[derive(Debug)]
pub struct EwfSource {
    segments: Vec<SharedSource>,
    chunks: Vec<ChunkLoc>,
    chunk_size: u64,
    media_size: u64,
    cache: Vec<Mutex<CacheShard>>,
}

/// Metadata gathered while opening.
#[derive(Debug, Default)]
pub struct EwfMeta {
    pub(crate) bytes_per_sector: u32,
    pub(crate) hashes: Vec<(HashAlgo, String)>,
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) bad_sectors: Vec<(u64, u64)>,
}

/// Name of segment number `n` (1-based) given the first segment's path:
/// `E01..E99`, then `EAA..EZZ`, `FAA..ZZZ` (case follows the first file).
pub fn segment_name(first: &Path, n: u32) -> Option<PathBuf> {
    let ext = first.extension()?.to_str()?;
    let lead = ext.chars().next()?;
    let upper = lead.is_ascii_uppercase();
    let ext = if n <= 99 {
        format!("{lead}{n:02}")
    } else {
        let index = n - 100;
        let first_code = u32::from(lead.to_ascii_uppercase()) + index / (26 * 26);
        let first_letter = char::from_u32(first_code).filter(char::is_ascii_uppercase)?;
        let second = char::from(b'A' + ((index / 26) % 26) as u8);
        let third = char::from(b'A' + (index % 26) as u8);
        let name: String = [first_letter, second, third].iter().collect();
        if upper {
            name
        } else {
            name.to_ascii_lowercase()
        }
    };
    Some(first.with_extension(ext))
}

/// Discovers all segments starting from `first` (which must be segment 1).
pub fn discover(first: &Path) -> Vec<PathBuf> {
    let mut out = vec![first.to_path_buf()];
    for n in 2..=14_971 {
        match segment_name(first, n) {
            Some(p) if p.is_file() => out.push(p),
            _ => break,
        }
    }
    out
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for block in data.chunks(5552) {
        for &x in block {
            a += u32::from(x);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

fn read_sections(seg: &dyn ReadAt, seg_no: usize) -> Result<Vec<Section>, ImageError> {
    let size = seg.size();
    let mut out = Vec::new();
    let mut off = FILE_HEADER;
    let mut visited = std::collections::HashSet::new();
    while off
        .checked_add(SECTION_DESCRIPTOR)
        .is_some_and(|e| e <= size)
        && visited.insert(off)
    {
        let d = seg.read_vec_at(off, SECTION_DESCRIPTOR as usize)?;
        let kind = cstr_lossy(d.sub(0, 16).map_err(|e| corrupt(e.to_string()))?);
        let next = d.u64_le(16).map_err(|e| corrupt(e.to_string()))?;
        let sec_size = d.u64_le(24).map_err(|e| corrupt(e.to_string()))?;
        let end = if next > off {
            next.min(size)
        } else {
            off.saturating_add(sec_size).min(size)
        };
        out.push(Section {
            kind: kind.clone(),
            start: off,
            end,
        });
        if kind == "done" || kind == "next" || next <= off {
            break;
        }
        off = next;
    }
    if out.is_empty() {
        return Err(corrupt(format!("segment {seg_no} has no sections")));
    }
    Ok(out)
}

/// Human-readable names of EWF header keys.
fn label(k: &str) -> &str {
    match k {
        "a" => "Description",
        "c" => "Case number",
        "n" => "Evidence number",
        "e" => "Examiner",
        "t" => "Notes",
        "md" => "Device model",
        "sn" => "Device serial number",
        "av" => "Acquisition software version",
        "ov" => "Acquisition OS",
        "m" => "Acquisition date",
        "u" => "System date",
        "p" => "Password hash",
        "l" => "Device label",
        "pid" => "Process identifier",
        "dc" => "Unknown (dc)",
        "ext" => "Extents",
        "r" => "Compression level",
        other => other,
    }
}

/// Parses `header2` (UTF-16LE) or `header` (ASCII) acquisition metadata.
fn parse_header(data: &[u8], utf16: bool) -> Vec<(String, String)> {
    let mut z = flate2::read::ZlibDecoder::new(data);
    let mut raw = Vec::new();
    if io::Read::read_to_end(&mut io::Read::take(&mut z, 1 << 20), &mut raw).is_err() {
        return Vec::new();
    }
    let text = if utf16 {
        let body = raw.strip_prefix(&[0xFF, 0xFE]).unwrap_or(&raw);
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    };
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    // Line 3 holds tab-separated keys, line 4 the values ("main" category).
    let (Some(keys), Some(values)) = (lines.get(2), lines.get(3)) else {
        return Vec::new();
    };
    keys.split('\t')
        .zip(values.split('\t'))
        .filter(|(_, v)| !v.is_empty() && *v != "0")
        .filter(|(k, _)| *k != "p" && *k != "dc")
        .map(|(k, v)| {
            let v = if (k == "m" || k == "u") && v.bytes().all(|b| b.is_ascii_digit()) {
                v.parse::<i64>()
                    .ok()
                    .and_then(pm_core::time::from_unix_secs)
                    .map_or_else(|| v.to_owned(), pm_core::time::format_utc)
            } else {
                v.to_owned()
            };
            (label(k).to_owned(), v)
        })
        .collect()
}

impl EwfSource {
    /// Opens all segments (as returned by [`discover`]).
    pub(crate) fn open(paths: &[PathBuf]) -> Result<(Self, EwfMeta), ImageError> {
        let mut segments: Vec<SharedSource> = Vec::with_capacity(paths.len());
        for p in paths {
            let f = FileSource::open(p).map_err(|source| ImageError::Open {
                path: p.clone(),
                source,
            })?;
            segments.push(Arc::new(f));
        }
        Self::from_sources(segments)
    }

    /// Opens an image from in-memory or otherwise virtual segment sources
    /// (evidence nested in other evidence, fuzzing). Order does not matter.
    pub fn from_sources(segments: Vec<SharedSource>) -> Result<(Self, EwfMeta), ImageError> {
        // Order by the segment number stored in each file header.
        let mut numbered = Vec::with_capacity(segments.len());
        for (i, s) in segments.into_iter().enumerate() {
            let h = s.read_vec_at(0, FILE_HEADER as usize)?;
            if h.get(..8) != Some(b"EVF\x09\x0d\x0a\xff\x00".as_slice())
                && h.get(..8) != Some(b"LVF\x09\x0d\x0a\xff\x00".as_slice())
            {
                return Err(corrupt(format!("segment #{} is not an EWF segment", i + 1)));
            }
            let n = h.u16_le(9).map_err(|e| corrupt(e.to_string()))?;
            numbered.push((n, s));
        }
        numbered.sort_by_key(|(n, _)| *n);
        for (i, (n, _)) in numbered.iter().enumerate() {
            if usize::from(*n) != i + 1 {
                return Err(corrupt(format!("missing segment {} (found {n})", i + 1)));
            }
        }
        let segments: Vec<SharedSource> = numbered.into_iter().map(|(_, s)| s).collect();

        let mut meta = EwfMeta::default();
        let mut chunk_size = 0u64;
        let mut media_size = 0u64;
        let mut expected_chunks = 0u64;
        let mut chunks: Vec<ChunkLoc> = Vec::new();
        let mut header_fields: Option<Vec<(String, String)>> = None;
        let mut ascii_fields: Option<Vec<(String, String)>> = None;

        for (seg_idx, seg) in segments.iter().enumerate() {
            let sections = read_sections(seg, seg_idx + 1)?;
            for sec in &sections {
                let data_start = sec.start + SECTION_DESCRIPTOR;
                let data_len = sec.end.saturating_sub(data_start);
                match sec.kind.as_str() {
                    "header2" if header_fields.is_none() => {
                        let d = seg.read_vec_at(data_start, data_len.min(1 << 20) as usize)?;
                        header_fields = Some(parse_header(&d, true));
                    }
                    "header" if ascii_fields.is_none() => {
                        let d = seg.read_vec_at(data_start, data_len.min(1 << 20) as usize)?;
                        ascii_fields = Some(parse_header(&d, false));
                    }
                    "volume" | "disk" | "data" if chunk_size == 0 => {
                        let d = seg.read_vec_at(data_start, data_len.min(1052) as usize)?;
                        let e = |t: pm_core::Truncated| corrupt(t.to_string());
                        expected_chunks = u64::from(d.u32_le(4).map_err(e)?);
                        let spc = u64::from(d.u32_le(8).map_err(e)?);
                        let bps = d.u32_le(12).map_err(e)?;
                        let sectors = if data_len >= 24 {
                            d.u64_le(16).map_err(e)?
                        } else {
                            0
                        };
                        chunk_size = spc * u64::from(bps);
                        if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
                            return Err(corrupt(format!("invalid chunk size {chunk_size}")));
                        }
                        meta.bytes_per_sector = bps;
                        media_size = sectors.saturating_mul(u64::from(bps));
                    }
                    "table" => {
                        let locs = read_table(seg, seg_idx, sec, &sections)?;
                        chunks.extend(locs);
                    }
                    "hash" => {
                        let d = seg.read_vec_at(data_start, 16)?;
                        if d.iter().any(|&b| b != 0)
                            && !meta.hashes.iter().any(|h| h.0 == HashAlgo::Md5)
                        {
                            meta.hashes.push((HashAlgo::Md5, to_hex(&d)));
                        }
                    }
                    "digest" => {
                        let d = seg.read_vec_at(data_start, 36)?;
                        let (md5, sha1) = (
                            d.get(..16).unwrap_or_default(),
                            d.get(16..36).unwrap_or_default(),
                        );
                        meta.hashes.retain(|h| h.0 != HashAlgo::Md5);
                        if md5.iter().any(|&b| b != 0) {
                            meta.hashes.push((HashAlgo::Md5, to_hex(md5)));
                        }
                        if sha1.iter().any(|&b| b != 0) {
                            meta.hashes.push((HashAlgo::Sha1, to_hex(sha1)));
                        }
                    }
                    "error2" => {
                        let d = seg.read_vec_at(data_start, data_len.min(1 << 20) as usize)?;
                        let n = d.u32_le(0).unwrap_or(0) as usize;
                        for i in 0..n.min(65_536) {
                            let (Ok(first), Ok(count)) =
                                (d.u32_le(520 + i * 8), d.u32_le(524 + i * 8))
                            else {
                                break;
                            };
                            meta.bad_sectors.push((u64::from(first), u64::from(count)));
                        }
                    }
                    _ => {}
                }
            }
        }
        if chunk_size == 0 {
            return Err(corrupt("no volume section"));
        }
        if expected_chunks != 0 && chunks.len() as u64 != expected_chunks {
            tracing::warn!(
                expected_chunks,
                found = chunks.len(),
                "EWF chunk count mismatch"
            );
        }
        if media_size == 0 {
            media_size = chunks.len() as u64 * chunk_size;
        }
        meta.fields = header_fields
            .filter(|f| !f.is_empty())
            .or(ascii_fields)
            .unwrap_or_default();
        let cache = (0..CACHE_SHARDS)
            .map(|_| Mutex::new(VecDeque::new()))
            .collect();
        Ok((
            Self {
                segments,
                chunks,
                chunk_size,
                media_size,
                cache,
            },
            meta,
        ))
    }

    fn chunk(&self, idx: u64) -> io::Result<Arc<[u8]>> {
        let shard = self
            .cache
            .get(idx as usize % CACHE_SHARDS)
            .ok_or_else(|| io::Error::other("cache shard"))?;
        if let Some((_, c)) = shard.lock().iter().find(|(i, _)| *i == idx) {
            return Ok(Arc::clone(c));
        }
        let loc = self.chunks.get(idx as usize).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("EWF chunk {idx} missing"),
            )
        })?;
        let seg = self.segments.get(usize::from(loc.segment)).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "chunk in unknown segment")
        })?;
        let raw = seg.read_vec_at(loc.offset, loc.len as usize)?;
        let expected =
            self.chunk_size
                .min(self.media_size.saturating_sub(idx * self.chunk_size)) as usize;
        let data: Arc<[u8]> = if loc.compressed {
            let mut out = vec![0u8; self.chunk_size as usize];
            let mut z = Decompress::new(true);
            match z.decompress(&raw, &mut out, FlushDecompress::Finish) {
                Ok(Status::StreamEnd | Status::Ok | Status::BufError) => {
                    out.truncate((z.total_out() as usize).max(expected));
                    out.into()
                }
                Err(e) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("EWF chunk {idx}: {e}"),
                    ));
                }
            }
        } else {
            // Stored chunks carry a trailing Adler-32 of the data.
            let (body, sum) = if raw.len() >= expected + 4 {
                raw.split_at(expected)
            } else {
                (raw.as_slice(), &[][..])
            };
            if sum.len() >= 4 && sum.u32_le(0).ok() != Some(adler32(body)) {
                tracing::warn!(chunk = idx, "EWF stored chunk checksum mismatch");
            }
            body.into()
        };
        let mut s = shard.lock();
        if s.len() >= CACHE_PER_SHARD {
            s.pop_front();
        }
        s.push_back((idx, Arc::clone(&data)));
        Ok(data)
    }
}

/// Reads a `table` section: chunk offsets plus derived lengths.
fn read_table(
    seg: &dyn ReadAt,
    seg_idx: usize,
    sec: &Section,
    sections: &[Section],
) -> Result<Vec<ChunkLoc>, ImageError> {
    let start = sec.start + SECTION_DESCRIPTOR;
    let head = seg.read_vec_at(start, 24)?;
    let e = |t: pm_core::Truncated| corrupt(t.to_string());
    let count = head.u32_le(0).map_err(e)?;
    if count > MAX_TABLE_ENTRIES {
        return Err(corrupt(format!("table with {count} entries")));
    }
    let base = head.u64_le(8).map_err(e)?;
    if base > seg.size() {
        return Err(corrupt(format!(
            "table base offset {base} beyond segment end"
        )));
    }
    let raw = seg.read_vec_at(start + 24, count as usize * 4)?;
    let mut offsets: Vec<(u64, bool)> = Vec::with_capacity(count as usize);
    let mut overflow = false;
    let mut prev = 0u64;
    for i in 0..count as usize {
        let v = raw.u32_le(i * 4).map_err(e)?;
        let (off, compressed) = if overflow {
            (base.saturating_add(u64::from(v)), false)
        } else {
            let off = base.saturating_add(u64::from(v & 0x7FFF_FFFF));
            if i > 0 && off < prev {
                // EnCase 6.7+ segments > 2 GiB: offsets keep growing past
                // 2^31 and the "compressed" bit becomes an address bit.
                overflow = true;
                (base.saturating_add(u64::from(v)), false)
            } else {
                (off, v & 0x8000_0000 != 0)
            }
        };
        prev = off;
        offsets.push((off, compressed));
    }
    let seg_no = u16::try_from(seg_idx).map_err(|_| corrupt("too many segments"))?;
    let mut out = Vec::with_capacity(offsets.len());
    for (i, &(off, compressed)) in offsets.iter().enumerate() {
        // A chunk ends at the next chunk or at the end of its section.
        let section_end = sections
            .iter()
            .find(|s| s.start <= off && off < s.end)
            .map_or(seg.size(), |s| s.end);
        let next = offsets
            .get(i + 1)
            .map_or(section_end, |n| n.0.min(section_end));
        let len = next
            .checked_sub(off)
            .filter(|&l| l > 0)
            .ok_or_else(|| corrupt(format!("chunk {i} has invalid extent")))?;
        out.push(ChunkLoc {
            segment: seg_no,
            offset: off,
            len: len.min(MAX_CHUNK_SIZE + 4),
            compressed,
        });
    }
    Ok(out)
}

impl ReadAt for EwfSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.media_size || buf.is_empty() {
            return Ok(0);
        }
        let want = (self.media_size - offset).min(buf.len() as u64) as usize;
        let mut done = 0;
        while done < want {
            let pos = offset + done as u64;
            let idx = pos / self.chunk_size;
            let within = (pos % self.chunk_size) as usize;
            let chunk = self.chunk(idx)?;
            let avail = chunk.get(within..).unwrap_or_default();
            if avail.is_empty() {
                break;
            }
            let n = avail.len().min(want - done);
            let (Some(dst), Some(src)) = (buf.get_mut(done..done + n), avail.get(..n)) else {
                break;
            };
            dst.copy_from_slice(src);
            done += n;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.media_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_names() {
        let p = Path::new("/x/img.E01");
        let name = |n| segment_name(p, n).unwrap().display().to_string();
        assert_eq!(name(2), "/x/img.E02");
        assert_eq!(name(99), "/x/img.E99");
        assert_eq!(name(100), "/x/img.EAA");
        assert_eq!(name(101), "/x/img.EAB");
        assert_eq!(name(126), "/x/img.EBA");
        assert_eq!(name(100 + 26 * 26), "/x/img.FAA");
        assert_eq!(
            segment_name(Path::new("a.e01"), 100).unwrap(),
            PathBuf::from("a.eaa")
        );
        assert_eq!(
            segment_name(Path::new("a.s01"), 3).unwrap(),
            PathBuf::from("a.s03")
        );
        assert!(segment_name(Path::new("a.Z01"), 100 + 26 * 26).is_none());
    }

    #[test]
    fn adler32_reference() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let big = vec![0xFFu8; 100_000];
        // Cross-check the blocked implementation against the naive one.
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &big {
            a = (a + u64::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(adler32(&big), ((b << 16) | a) as u32);
    }

    #[test]
    fn header2_parsing() {
        let text = "3\nmain\na\tc\tn\te\tt\tm\tp\npm fixture\tCASE-42\tEV-7\tExaminer Ä\tnotes\t1791989497\t0\n\n";
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        io::Write::write_all(&mut enc, &utf16).unwrap();
        let fields = parse_header(&enc.finish().unwrap(), true);
        assert!(fields.contains(&("Examiner".into(), "Examiner Ä".into())));
        assert!(fields.contains(&("Case number".into(), "CASE-42".into())));
        assert!(fields.contains(&("Acquisition date".into(), "2026-10-14 14:51:37 UTC".into())));
        assert!(!fields.iter().any(|(k, _)| k == "Password hash"));
    }
}
