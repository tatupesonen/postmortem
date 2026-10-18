//! Sorted on-disk MD5 index (`<set>-md5.pmidx`) for the text and binary
//! hash-set formats.
//!
//! Layout: a 64-byte header, then `count` records of `md5[16] ‖ locator
//! u64 LE` sorted by MD5. The locator points back into the source (byte
//! offset of the line or record, or the NSRL v3 row id) so names and
//! comments can be fetched for hits.
//!
//! * **Build** is an external merge sort: runs of at most `chunk` records
//!   are sorted in memory and spilled, then k-way merged, so indexing the
//!   full NSRL needs a bounded amount of RAM.
//! * **Lookup** keeps every [`FENCE`]th MD5 in memory (≈ 64 kB per million
//!   hashes), then reads one block of records with a positional read.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pm_core::ReadAt;
use pm_core::io::FileSource;

use crate::formats::{
    ENCASE_RECORD, ENCASE_RECORDS, Format, NsrlLayout, parse_line, parse_md5_hex,
};
use crate::{HashDbError, Result};

const MAGIC: &[u8; 8] = b"PMHIDX\0\x01";
const HEADER: u64 = 64;
const REC: usize = 24;
/// Records per in-memory fence entry.
pub(crate) const FENCE: u64 = 256;
/// Records sorted in memory per run while building (96 MB).
pub(crate) const DEFAULT_CHUNK: usize = 4_000_000;

type Record = [u8; REC];

/// Identity of the source an index was built from (stale detection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    len: u64,
    mtime_ns: i64,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Result<Self> {
        let m = std::fs::metadata(path).map_err(|e| HashDbError::io(path, e))?;
        let mtime_ns = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX));
        Ok(Self {
            len: m.len(),
            mtime_ns,
        })
    }
}

/// Index file path for a hash-set source.
pub fn index_path(source: &Path) -> PathBuf {
    let mut name = source.file_name().unwrap_or_default().to_os_string();
    name.push("-md5.pmidx");
    source.with_file_name(name)
}

fn record(md5: &[u8; 16], locator: u64) -> Record {
    let mut r = [0u8; REC];
    let (a, b) = r.split_at_mut(16);
    a.copy_from_slice(md5);
    b.copy_from_slice(&locator.to_le_bytes());
    r
}

fn rec_md5(r: &Record) -> [u8; 16] {
    let mut m = [0u8; 16];
    m.copy_from_slice(r.get(..16).unwrap_or(&[0; 16]));
    m
}

fn rec_locator(r: &Record) -> u64 {
    let mut l = [0u8; 8];
    l.copy_from_slice(r.get(16..).unwrap_or(&[0; 8]));
    u64::from_le_bytes(l)
}

/// Streams `(md5, locator)` pairs from a source.
pub(crate) fn for_each_entry(
    source: &Path,
    format: Format,
    mut f: impl FnMut([u8; 16], u64) -> Result<()>,
) -> Result<()> {
    match format {
        Format::Kdb => Ok(()),
        Format::EnCase => {
            let file = File::open(source).map_err(|e| HashDbError::io(source, e))?;
            let len = file
                .metadata()
                .map_err(|e| HashDbError::io(source, e))?
                .len();
            let mut r = BufReader::with_capacity(1 << 20, file);
            let mut skip = vec![0u8; ENCASE_RECORDS as usize];
            r.read_exact(&mut skip)
                .map_err(|e| HashDbError::io(source, e))?;
            let mut off = ENCASE_RECORDS;
            let mut rec = [0u8; ENCASE_RECORD as usize];
            while off + ENCASE_RECORD <= len {
                r.read_exact(&mut rec)
                    .map_err(|e| HashDbError::io(source, e))?;
                let mut md5 = [0u8; 16];
                md5.copy_from_slice(rec.get(..16).unwrap_or(&[0; 16]));
                f(md5, off)?;
                off += ENCASE_RECORD;
            }
            Ok(())
        }
        Format::NsrlV3 => {
            let conn = rusqlite::Connection::open_with_flags(
                source,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            let mut st = conn.prepare("SELECT rowid, md5 FROM FILE")?;
            let mut rows = st.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let md5: Option<String> = row.get(1)?;
                if let Some(m) = md5.as_deref().and_then(parse_md5_hex) {
                    f(m, u64::try_from(id).unwrap_or(0))?;
                }
            }
            Ok(())
        }
        Format::Md5sum | Format::NsrlV2 | Format::HashKeeper | Format::HfindIdx => {
            let file = File::open(source).map_err(|e| HashDbError::io(source, e))?;
            let mut r = BufReader::with_capacity(1 << 20, file);
            let mut buf = Vec::new();
            let mut off = 0u64;
            let mut layout = NsrlLayout::default();
            let mut first = true;
            loop {
                buf.clear();
                let n = r
                    .read_until(b'\n', &mut buf)
                    .map_err(|e| HashDbError::io(source, e))?;
                if n == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&buf);
                if first && format == Format::NsrlV2 {
                    layout = NsrlLayout::from_header(line.trim_end());
                }
                first = false;
                if let Some(e) = parse_line(format, layout, &line) {
                    f(e.md5, off)?;
                }
                off += n as u64;
            }
            Ok(())
        }
    }
}

/// Runs merged at once (open files and read buffers).
const MAX_FAN_IN: usize = 64;

/// Builds the index for `source`; returns the number of records.
pub(crate) fn build(
    source: &Path,
    format: Format,
    chunk: usize,
    progress: &AtomicU64,
) -> Result<u64> {
    let dest = index_path(source);
    let tmp = dest.with_extension("pmidx.tmp");
    let stamp = Stamp::of(source)?;
    let mut runs: Vec<PathBuf> = Vec::new();
    let mut buf: Vec<Record> = Vec::with_capacity(chunk.min(1 << 20));
    let spill = |buf: &mut Vec<Record>, runs: &mut Vec<PathBuf>| -> Result<()> {
        buf.sort_unstable();
        let p = dest.with_extension(format!("pmidx.run{}", runs.len()));
        write_records(&p, buf)?;
        runs.push(p);
        buf.clear();
        Ok(())
    };
    let result = (|| -> Result<u64> {
        for_each_entry(source, format, |md5, loc| {
            buf.push(record(&md5, loc));
            progress.fetch_add(1, Ordering::Relaxed);
            if buf.len() >= chunk {
                spill(&mut buf, &mut runs)?;
            }
            Ok(())
        })?;
        let out = File::create(&tmp).map_err(|e| HashDbError::io(&tmp, e))?;
        let mut w = BufWriter::with_capacity(1 << 20, out);
        w.write_all(&[0u8; HEADER as usize])
            .map_err(|e| HashDbError::io(&tmp, e))?;
        let count = if runs.is_empty() {
            buf.sort_unstable();
            for r in &buf {
                w.write_all(r).map_err(|e| HashDbError::io(&tmp, e))?;
            }
            buf.len() as u64
        } else {
            if !buf.is_empty() {
                spill(&mut buf, &mut runs)?;
            }
            // Merge at most MAX_FAN_IN runs at a time: each open run is a
            // file handle and a read buffer.
            let mut pass = 0usize;
            while runs.len() > MAX_FAN_IN {
                let mut next = Vec::with_capacity(runs.len().div_ceil(MAX_FAN_IN));
                for (g, group) in runs.chunks(MAX_FAN_IN).enumerate() {
                    let p = dest.with_extension(format!("pmidx.pass{pass}.{g}"));
                    let f = File::create(&p).map_err(|e| HashDbError::io(&p, e))?;
                    let mut out = BufWriter::with_capacity(1 << 20, f);
                    merge(group, &mut out, &p)?;
                    out.flush().map_err(|e| HashDbError::io(&p, e))?;
                    next.push(p);
                }
                for r in &runs {
                    let _ = std::fs::remove_file(r);
                }
                runs = next;
                pass += 1;
            }
            merge(&runs, &mut w, &tmp)?
        };
        let mut file = w
            .into_inner()
            .map_err(|e| HashDbError::io(&tmp, e.into_error()))?;
        write_header(&mut file, count, stamp, format, &tmp)?;
        file.sync_all().map_err(|e| HashDbError::io(&tmp, e))?;
        Ok(count)
    })();
    for r in &runs {
        let _ = std::fs::remove_file(r);
    }
    match result {
        Ok(count) => {
            std::fs::rename(&tmp, &dest).map_err(|e| HashDbError::io(&dest, e))?;
            tracing::info!(source = %source.display(), count, "hash set indexed");
            Ok(count)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn write_records(path: &Path, recs: &[Record]) -> Result<()> {
    let f = File::create(path).map_err(|e| HashDbError::io(path, e))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);
    for r in recs {
        w.write_all(r).map_err(|e| HashDbError::io(path, e))?;
    }
    w.flush().map_err(|e| HashDbError::io(path, e))
}

fn merge(runs: &[PathBuf], w: &mut impl Write, dest: &Path) -> Result<u64> {
    let mut readers = runs
        .iter()
        .map(|p| {
            File::open(p)
                .map(|f| BufReader::with_capacity(1 << 18, f))
                .map_err(|e| HashDbError::io(p, e))
        })
        .collect::<Result<Vec<_>>>()?;
    let next = |r: &mut BufReader<File>| -> Option<Record> {
        let mut rec = [0u8; REC];
        r.read_exact(&mut rec).ok().map(|()| rec)
    };
    let mut heap = BinaryHeap::new();
    for (i, r) in readers.iter_mut().enumerate() {
        if let Some(rec) = next(r) {
            heap.push(Reverse((rec, i)));
        }
    }
    let mut count = 0u64;
    while let Some(Reverse((rec, i))) = heap.pop() {
        w.write_all(&rec).map_err(|e| HashDbError::io(dest, e))?;
        count += 1;
        if let Some(r) = readers.get_mut(i)
            && let Some(more) = next(r)
        {
            heap.push(Reverse((more, i)));
        }
    }
    Ok(count)
}

fn write_header(
    file: &mut File,
    count: u64,
    stamp: Stamp,
    format: Format,
    path: &Path,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut h = [0u8; HEADER as usize];
    let mut put = |at: usize, b: &[u8]| {
        if let Some(d) = h.get_mut(at..at + b.len()) {
            d.copy_from_slice(b);
        }
    };
    put(0, MAGIC);
    put(8, &(REC as u32).to_le_bytes());
    put(12, &[format.code()]);
    put(16, &count.to_le_bytes());
    put(24, &stamp.len.to_le_bytes());
    put(32, &stamp.mtime_ns.to_le_bytes());
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.write_all(&h))
        .map_err(|e| HashDbError::io(path, e))
}

/// An open index.
#[derive(Debug)]
pub(crate) struct SortedIndex {
    src: FileSource,
    count: u64,
    fence: Vec<[u8; 16]>,
}

impl SortedIndex {
    /// Opens the index of `source`, failing with [`HashDbError::NeedsIndex`]
    /// when it is missing, stale or of another format.
    pub(crate) fn open(source: &Path, format: Format) -> Result<Self> {
        let path = index_path(source);
        let needs = || HashDbError::NeedsIndex(source.to_path_buf());
        let src = FileSource::open(&path).map_err(|_| needs())?;
        let mut h = [0u8; HEADER as usize];
        src.read_exact_at(0, &mut h).map_err(|_| needs())?;
        let u64_at = |o: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(h.get(o..o + 8).unwrap_or(&[0; 8]));
            u64::from_le_bytes(b)
        };
        let stamp = Stamp::of(source)?;
        let ok = h.get(..8) == Some(MAGIC.as_slice())
            && h.get(12) == Some(&format.code())
            && u64_at(24) == stamp.len
            && u64_at(32) == stamp.mtime_ns.cast_unsigned();
        let count = u64_at(16);
        if !ok || src.size() != HEADER + count * REC as u64 {
            return Err(needs());
        }
        // Read the fence: one MD5 per FENCE records.
        let mut fence = Vec::with_capacity(count.div_ceil(FENCE) as usize);
        let mut i = 0;
        while i < count {
            let mut m = [0u8; 16];
            src.read_exact_at(HEADER + i * REC as u64, &mut m)
                .map_err(|e| HashDbError::io(&path, e))?;
            fence.push(m);
            i += FENCE;
        }
        Ok(Self { src, count, fence })
    }

    pub(crate) fn len(&self) -> u64 {
        self.count
    }

    /// Locators of every record with this MD5.
    pub(crate) fn find(&self, md5: &[u8; 16]) -> Result<Vec<u64>> {
        // fence[i] is the first MD5 of block i; equal keys may start at the
        // end of the block before the first fence that is >= the key.
        let block = self.fence.partition_point(|f| f < md5).saturating_sub(1) as u64;
        let mut out = Vec::new();
        let mut start = block * FENCE;
        while start < self.count {
            let end = (start + FENCE).min(self.count);
            let mut buf = vec![0u8; (end - start) as usize * REC];
            self.src
                .read_exact_at(HEADER + start * REC as u64, &mut buf)
                .map_err(|e| HashDbError::Io {
                    path: PathBuf::from("<index>"),
                    source: e,
                })?;
            let recs = buf.as_chunks::<REC>().0;
            let first = recs.partition_point(|r| rec_md5(r) < *md5);
            for r in recs.get(first..).unwrap_or_default() {
                if rec_md5(r) != *md5 {
                    return Ok(out);
                }
                out.push(rec_locator(r));
            }
            start = end;
        }
        Ok(out)
    }
}
