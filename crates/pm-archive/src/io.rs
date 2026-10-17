//! Byte-source helpers shared by the format readers.

use std::io::{self, Read, Write};
use std::path::Path;

use pm_core::ReadAt;

use crate::driver::CHUNK;

/// A borrowed `dyn ReadAt` as a sized [`ReadAt`] (for generic adapters
/// such as [`pm_core::io::Cursor`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct DynSource<'a>(pub(crate) &'a dyn ReadAt);

impl ReadAt for DynSource<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read_at(offset, buf)
    }

    fn size(&self) -> u64 {
        self.0.size()
    }
}

/// Sequential reader over `[start, end)` of a [`ReadAt`].
#[derive(Debug)]
pub(crate) struct RangeReader<'a> {
    src: &'a dyn ReadAt,
    pos: u64,
    end: u64,
}

impl<'a> RangeReader<'a> {
    pub(crate) fn new(src: &'a dyn ReadAt, start: u64, len: u64) -> Self {
        let end = start.saturating_add(len).min(src.size());
        Self {
            src,
            pos: start.min(end),
            end,
        }
    }
}

impl Read for RangeReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.end.saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let want = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        let n = self
            .src
            .read_at(self.pos, buf.get_mut(..want).unwrap_or_default())?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// Reads up to `len` bytes at `offset` (fewer at the end of the source).
pub(crate) fn read_upto(src: &dyn ReadAt, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let avail = src.size().saturating_sub(offset);
    let len = usize::try_from(avail).map_or(len, |a| a.min(len));
    let mut buf = vec![0u8; len];
    let mut got = 0;
    while got < len {
        let n = src.read_at(offset + got as u64, buf.get_mut(got..).unwrap_or_default())?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

/// Copies a whole source into a temporary file (for libraries that need a
/// path), checking `cancel` between chunks.
pub(crate) fn spool(
    src: &dyn ReadAt,
    dir: Option<&Path>,
    cancel: &dyn Fn() -> bool,
) -> io::Result<tempfile::NamedTempFile> {
    let mut tmp = match dir {
        Some(d) => {
            std::fs::create_dir_all(d)?;
            tempfile::Builder::new()
                .prefix("pm-archive")
                .tempfile_in(d)?
        }
        None => tempfile::Builder::new().prefix("pm-archive").tempfile()?,
    };
    let mut r = RangeReader::new(src, 0, src.size());
    let mut buf = vec![0u8; CHUNK * 4];
    loop {
        if cancel() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        tmp.write_all(buf.get(..n).unwrap_or_default())?;
    }
    tmp.flush()?;
    Ok(tmp)
}

/// Reads a whole source into memory if it is at most `max` bytes.
pub(crate) fn read_all_bounded(src: &dyn ReadAt, max: u64) -> io::Result<Option<Vec<u8>>> {
    if src.size() > max {
        return Ok(None);
    }
    let len = usize::try_from(src.size()).map_err(io::Error::other)?;
    read_upto(src, 0, len).map(Some)
}

/// Reads at most `limit` bytes of a decoder's output into memory; `None`
/// when the output is larger.
pub(crate) fn read_to_end_bounded(r: &mut dyn Read, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut take = r.take(limit.saturating_add(1));
    take.read_to_end(&mut out)?;
    if out.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    #[test]
    fn range_reader_is_bounded() {
        let src = MemSource::new((0u8..100).collect::<Vec<_>>());
        let mut r = RangeReader::new(&src, 90, 50);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out, (90u8..100).collect::<Vec<_>>());
        let mut r = RangeReader::new(&src, 500, 5);
        assert_eq!(r.read(&mut [0; 4]).unwrap(), 0);
        let mut r = RangeReader::new(&src, u64::MAX, u64::MAX);
        assert_eq!(r.read(&mut [0; 4]).unwrap(), 0);
        assert_eq!(read_upto(&src, 98, 10).unwrap(), vec![98, 99]);
    }

    #[test]
    fn bounded_reads() {
        let src = MemSource::new(vec![1u8; 10]);
        assert!(read_all_bounded(&src, 9).unwrap().is_none());
        assert_eq!(read_all_bounded(&src, 10).unwrap().unwrap().len(), 10);
        let mut r: &[u8] = &[0u8; 10];
        assert!(read_to_end_bounded(&mut r, 9).unwrap().is_none());
        let mut r: &[u8] = &[0u8; 10];
        assert_eq!(read_to_end_bounded(&mut r, 10).unwrap().unwrap().len(), 10);
        let dir = tempfile::tempdir().unwrap();
        let t = spool(&src, Some(dir.path()), &|| false).unwrap();
        assert_eq!(std::fs::read(t.path()).unwrap(), vec![1u8; 10]);
        assert!(spool(&src, Some(dir.path()), &|| true).is_err());
    }
}
