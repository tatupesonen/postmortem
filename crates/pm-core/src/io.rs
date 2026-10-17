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

/// A local file read with positional IO (`pread` / `seek_read`).
#[derive(Debug)]
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_file(File::open(path)?)
    }

    /// Wraps an already-open file. The size is sampled once.
    pub fn from_file(mut file: File) -> io::Result<Self> {
        let md = file.metadata()?;
        let size = if md.is_file() {
            md.len()
        } else {
            // Block devices (a local disk) report no length; their end
            // does.
            use std::io::Seek;
            let end = file.seek(io::SeekFrom::End(0)).unwrap_or(0);
            file.seek(io::SeekFrom::Start(0))?;
            end
        };
        Ok(Self { file, size })
    }
}

impl ReadAt for FileSource {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size {
            return Ok(0);
        }
        let buf = clamp_mut(buf, self.size - offset);
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
        }
    }
    fn size(&self) -> u64 {
        self.size
    }
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
