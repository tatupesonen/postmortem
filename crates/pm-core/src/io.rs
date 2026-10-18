//! Random-access byte sources.
//!
//! Every layer of evidence (disk image → volume → filesystem → file content →
//! embedded file) is exposed as a [`ReadAt`]: a thread-safe, positional reader
//! with a known size. Positional reads take `&self`, so a single source can be
//! shared across ingest worker threads without locking.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// A thread-safe random-access byte source of known length.
pub trait ReadAt: Send + Sync + fmt::Debug {
    /// Reads up to `buf.len()` bytes starting at `offset`.
    ///
    /// Returns the number of bytes read; `0` means `offset` is at or past the
    /// end. Implementations may return short reads only at end of data.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Total size of the source in bytes.
    fn size(&self) -> u64;

    /// Fills `buf` completely from `offset` or fails with
    /// [`io::ErrorKind::UnexpectedEof`].
    fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read_at(offset, buf) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("read past end of source at offset {offset}"),
                    ));
                }
                Ok(n) => {
                    buf = std::mem::take(&mut buf).get_mut(n..).unwrap_or_default();
                    offset += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Reads `len` bytes at `offset` into a new vector.
    ///
    /// Fails if fewer than `len` bytes are available, which also guards
    /// against allocating huge buffers from corrupted length fields.
    fn read_vec_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let end = offset.checked_add(len as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "offset + length overflows")
        })?;
        if end > self.size() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "requested {len} bytes at offset {offset}, source is {} bytes",
                    self.size()
                ),
            ));
        }
        let mut buf = vec![0; len];
        self.read_exact_at(offset, &mut buf)?;
        Ok(buf)
    }
}

/// Shared, dynamically typed byte source.
pub type SharedSource = Arc<dyn ReadAt>;

impl<T: ReadAt + ?Sized> ReadAt for Arc<T> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn size(&self) -> u64 {
        (**self).size()
    }
}

impl<T: ReadAt + ?Sized> ReadAt for Box<T> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read_at(offset, buf)
    }
    fn size(&self) -> u64 {
        (**self).size()
    }
}

/// Truncates `buf` to at most `max` bytes.
fn clamp_mut(buf: &mut [u8], max: u64) -> &mut [u8] {
    let n = usize::try_from(max).map_or(buf.len(), |m| m.min(buf.len()));
    buf.get_mut(..n).unwrap_or_default()
}

/// Copies from an in-memory slice honouring the [`ReadAt`] contract.
fn read_from_slice(data: &[u8], offset: u64, buf: &mut [u8]) -> usize {
    let Ok(start) = usize::try_from(offset) else {
        return 0;
    };
    let Some(avail) = data.get(start..) else {
        return 0;
    };
    let n = avail.len().min(buf.len());
    if let (Some(dst), Some(src)) = (buf.get_mut(..n), avail.get(..n)) {
        dst.copy_from_slice(src);
    }
    n
}

/// An in-memory byte source.
#[derive(Clone)]
pub struct MemSource {
    data: Arc<[u8]>,
}

impl MemSource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Self {
        Self { data: data.into() }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for MemSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemSource")
            .field("len", &self.data.len())
            .finish()
    }
}

impl ReadAt for MemSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        Ok(read_from_slice(&self.data, offset, buf))
    }
    fn size(&self) -> u64 {
        self.data.len() as u64
    }
}

/// A local file or device read with positional IO (`pread` /
/// `seek_read`).
///
/// Devices (a local disk or volume) differ from files in two ways the
/// source hides: Windows accepts only reads whose offset and length are
/// whole sectors (`\\.\PhysicalDrive0`, `\\.\C:`), and neither a
/// file length nor, on Windows, the end of the file gives a disk's size.
/// Reads of a device are therefore widened to whole sectors, and its size
/// is found by probing when the system does not report it.
#[derive(Debug)]
pub struct FileSource {
    file: File,
    size: u64,
    /// Sector size reads are aligned to (0: no alignment needed).
    align: u64,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        if is_device_path(path) {
            Ok(Self::device(file))
        } else {
            Self::from_file(file)
        }
    }

    /// Wraps an already-open file. The size is sampled once.
    pub fn from_file(file: File) -> io::Result<Self> {
        // Device handles may not answer metadata queries at all.
        if let Ok(md) = file.metadata()
            && md.is_file()
        {
            return Ok(Self {
                file,
                size: md.len(),
                align: 0,
            });
        }
        Ok(Self::device(file))
    }

    /// A disk or volume: sized from its end or by probing, read in whole
    /// sectors on Windows.
    fn device(mut file: File) -> Self {
        let end = std::io::Seek::seek(&mut file, io::SeekFrom::End(0)).unwrap_or(0);
        let _ = std::io::Seek::seek(&mut file, io::SeekFrom::Start(0));
        let mut s = Self {
            file,
            size: u64::MAX,
            align: 0,
        };
        if cfg!(windows) {
            s.align = device_alignment(|off, buf| s.raw_read(off, buf));
        }
        s.size = if end > 0 {
            end
        } else {
            probe_size(|off, buf| s.read_at(off, buf), s.align.max(512))
        };
        s
    }

    fn raw_read(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
        }
    }
}

impl ReadAt for FileSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }
        let buf = clamp_mut(buf, self.size - offset);
        if self.align == 0 {
            self.raw_read(offset, buf)
        } else {
            read_aligned(|o, b| self.raw_read(o, b), self.align, offset, buf)
        }
    }
    fn size(&self) -> u64 {
        self.size
    }
}

/// Whether `path` names a Windows device (`\\.\PhysicalDrive0`,
/// `\\.\C:`), whose handle reports itself as an empty file.
fn is_device_path(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    s.starts_with(r"\\.\") || s.starts_with("//./")
}

/// The sector size a device accepts: 512, or 4096 for 4K-native disks
/// (whose 512-byte reads fail). 0 when even that fails (not a device that
/// needs alignment, or unreadable: errors then surface on use).
fn device_alignment(read: impl Fn(u64, &mut [u8]) -> io::Result<usize>) -> u64 {
    for a in [512u64, 4096] {
        let mut b = vec![0u8; a as usize];
        if read(0, &mut b).is_ok_and(|n| n > 0) {
            return a;
        }
    }
    0
}

/// Reads `buf.len()` bytes at `offset` through `read`, which accepts only
/// offsets and lengths that are multiples of `align`.
pub fn read_aligned(
    read: impl Fn(u64, &mut [u8]) -> io::Result<usize>,
    align: u64,
    offset: u64,
    buf: &mut [u8],
) -> io::Result<usize> {
    let start = offset - offset % align;
    let end = (offset + buf.len() as u64).div_ceil(align) * align;
    if start == offset && end == offset + buf.len() as u64 {
        return read(offset, buf);
    }
    let len = usize::try_from(end - start).map_err(|_| io::Error::other("read too large"))?;
    let mut tmp = vec![0u8; len];
    let mut got = 0usize;
    while got < len {
        let chunk = tmp.get_mut(got..).unwrap_or_default();
        match read(start + got as u64, chunk) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // Past the end of the device a sector read fails: keep what
            // was read.
            Err(_) if got > 0 => break,
            Err(e) => return Err(e),
        }
    }
    let skip = usize::try_from(offset - start).unwrap_or(usize::MAX);
    let avail = got.saturating_sub(skip).min(buf.len());
    if let (Some(dst), Some(src)) = (buf.get_mut(..avail), tmp.get(skip..skip + avail)) {
        dst.copy_from_slice(src);
    }
    Ok(avail)
}

/// The size of a device that does not report one: the end of the last
/// readable `sector` found by doubling, then bisecting.
pub fn probe_size(read: impl Fn(u64, &mut [u8]) -> io::Result<usize>, sector: u64) -> u64 {
    let mut b = vec![0u8; sector as usize];
    let ok = |n: u64, b: &mut [u8]| read(n * sector, b).is_ok_and(|r| r > 0);
    if !ok(0, &mut b) {
        return 0;
    }
    // Sector `lo` is readable, `hi` is not.
    let (mut lo, mut hi) = (0u64, 1u64);
    while ok(hi, &mut b) {
        lo = hi;
        hi = match hi.checked_mul(2) {
            Some(h) if h < u64::MAX / sector => h,
            _ => return (lo + 1) * sector,
        };
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if ok(mid, &mut b) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + 1) * sector
}

/// A window `[start, start + len)` into another source.
///
/// Used for partitions inside a disk image, files inside a filesystem stored
/// contiguously, and so on. The window is clamped to the parent's size.
#[derive(Debug, Clone)]
pub struct SliceSource<S> {
    inner: S,
    start: u64,
    len: u64,
}

impl<S: ReadAt> SliceSource<S> {
    pub fn new(inner: S, start: u64, len: u64) -> Self {
        let parent = inner.size();
        let start = start.min(parent);
        let len = len.min(parent - start);
        Self { inner, start, len }
    }

    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: ReadAt> ReadAt for SliceSource<S> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        self.inner
            .read_at(self.start + offset, clamp_mut(buf, self.len - offset))
    }
    fn size(&self) -> u64 {
        self.len
    }
}

/// A source of `len` zero bytes (sparse regions, unallocated placeholders).
#[derive(Debug, Clone, Copy)]
pub struct ZeroSource {
    len: u64,
}

impl ZeroSource {
    pub fn new(len: u64) -> Self {
        Self { len }
    }
}

impl ReadAt for ZeroSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Ok(0);
        }
        let buf = clamp_mut(buf, self.len - offset);
        buf.fill(0);
        Ok(buf.len())
    }
    fn size(&self) -> u64 {
        self.len
    }
}

/// Adapts a [`ReadAt`] to [`io::Read`] + [`io::Seek`] for interop with crates
/// that expect a stream (zip, image decoders, ...).
#[derive(Debug, Clone)]
pub struct Cursor<S> {
    inner: S,
    pos: u64,
}

impl<S: ReadAt> Cursor<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, pos: 0 }
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: ReadAt> io::Read for Cursor<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<S: ReadAt> io::Seek for Cursor<S> {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let new = match pos {
            io::SeekFrom::Start(p) => Some(p),
            io::SeekFrom::End(d) => self.inner.size().checked_add_signed(d),
            io::SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match new {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::{Read, Seek, SeekFrom, Write};

    fn mem(n: usize) -> MemSource {
        MemSource::new((0..n).map(|i| i as u8).collect::<Vec<_>>())
    }

    #[test]
    fn mem_source_reads_and_reports_eof() {
        let src = mem(10);
        let mut buf = [0u8; 4];
        assert_eq!(src.read_at(8, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], &[8, 9]);
        assert_eq!(src.read_at(10, &mut buf).unwrap(), 0);
        assert_eq!(src.read_at(u64::MAX, &mut buf).unwrap(), 0);
    }

    /// A device that accepts only whole 512-byte sectors, like a Windows
    /// physical drive.
    fn sector_device(data: Vec<u8>) -> impl Fn(u64, &mut [u8]) -> io::Result<usize> {
        move |off, buf| {
            if off % 512 != 0 || buf.len() % 512 != 0 {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let off = usize::try_from(off).unwrap();
            if off >= data.len() {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let n = buf.len().min(data.len() - off);
            buf[..n].copy_from_slice(&data[off..off + n]);
            Ok(n)
        }
    }

    #[test]
    fn unaligned_reads_of_a_sector_device() {
        let data: Vec<u8> = (0..512 * 9).map(|i| (i % 251) as u8).collect();
        let dev = sector_device(data.clone());
        let mut one = [0u8; 1];
        assert!(dev(0, &mut one).is_err(), "the device itself refuses");
        assert_eq!(read_aligned(&dev, 512, 0, &mut one).unwrap(), 1);
        assert_eq!(one[0], data[0]);
        let mut mid = vec![0u8; 1000];
        assert_eq!(read_aligned(&dev, 512, 700, &mut mid).unwrap(), 1000);
        assert_eq!(mid, data[700..1700]);
        // Up to the end, then short.
        let mut tail = vec![0u8; 100];
        assert_eq!(
            read_aligned(&dev, 512, 512 * 9 - 40, &mut tail).unwrap(),
            40
        );
        assert_eq!(probe_size(&dev, 512), 512 * 9);
        assert_eq!(probe_size(sector_device(vec![0; 512]), 512), 512);
        assert_eq!(probe_size(sector_device(Vec::new()), 512), 0);
        assert_eq!(device_alignment(&dev), 512);
    }

    #[test]
    fn read_exact_at_fails_past_end() {
        let src = mem(10);
        let mut buf = [0u8; 4];
        let err = src.read_exact_at(8, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_vec_at_rejects_oversized_lengths_without_allocating() {
        let src = mem(10);
        assert!(src.read_vec_at(0, usize::MAX).is_err());
        assert!(src.read_vec_at(u64::MAX, 2).is_err());
        assert_eq!(src.read_vec_at(2, 3).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn slice_source_is_clamped_to_parent() {
        let s = SliceSource::new(mem(10), 6, 100);
        assert_eq!(s.size(), 4);
        assert_eq!(s.read_vec_at(0, 4).unwrap(), vec![6, 7, 8, 9]);
        let s = SliceSource::new(mem(10), 20, 5);
        assert_eq!(s.size(), 0);
    }

    #[test]
    fn nested_slices_compose() {
        let outer = SliceSource::new(mem(100), 10, 50);
        let inner = SliceSource::new(outer, 5, 5);
        assert_eq!(inner.read_vec_at(0, 5).unwrap(), vec![15, 16, 17, 18, 19]);
    }

    #[test]
    fn zero_source_fills_zeroes() {
        let z = ZeroSource::new(3);
        let mut buf = [0xffu8; 5];
        assert_eq!(z.read_at(1, &mut buf).unwrap(), 2);
        assert_eq!(buf, [0, 0, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn file_source_positional_reads() {
        let dir = std::env::temp_dir().join(format!("pm-core-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world")
            .unwrap();
        let f = FileSource::open(&path).unwrap();
        assert_eq!(f.size(), 11);
        assert_eq!(f.read_vec_at(6, 5).unwrap(), b"world");
        let mut buf = [0u8; 8];
        assert_eq!(f.read_at(9, &mut buf).unwrap(), 2);
        assert_eq!(f.read_at(11, &mut buf).unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cursor_reads_and_seeks() {
        let mut c = Cursor::new(mem(10));
        let mut out = Vec::new();
        c.seek(SeekFrom::End(-3)).unwrap();
        c.read_to_end(&mut out).unwrap();
        assert_eq!(out, vec![7, 8, 9]);
        assert!(c.seek(SeekFrom::Current(-100)).is_err());
        assert_eq!(c.seek(SeekFrom::Start(1)).unwrap(), 1);
    }

    #[test]
    fn shared_source_dyn_dispatch() {
        let s: SharedSource = Arc::new(mem(4));
        let slice = SliceSource::new(s.clone(), 1, 2);
        assert_eq!(slice.read_vec_at(0, 2).unwrap(), vec![1, 2]);
    }

    proptest! {
        #[test]
        fn slice_reads_match_parent(
            data in proptest::collection::vec(any::<u8>(), 0..256),
            start in 0u64..300, len in 0u64..300, off in 0u64..300, n in 0usize..64,
        ) {
            let parent = MemSource::new(data.clone());
            let s = SliceSource::new(parent, start, len);
            let mut buf = vec![0u8; n];
            let got = s.read_at(off, &mut buf).unwrap();
            let lo = (start as usize).min(data.len());
            let hi = (lo + len as usize).min(data.len());
            let window = &data[lo..hi];
            let expect: &[u8] = window.get(off as usize..).map_or(&[], |w| &w[..w.len().min(n)]);
            prop_assert_eq!(&buf[..got], expect);
        }
    }
}
