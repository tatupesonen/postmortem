//! A generic [`ReadAt`] over block-mapped virtual disks.
//!
//! Every sparse/differencing format answers the same question for a virtual
//! offset: *where do the bytes from here on come from, and for how long?*
//! A [`BlockMap`] answers it; [`MappedSource`] turns the answers into reads,
//! streaming data straight from the image files (no data caching) and
//! falling back to the parent image (or zeros) for unallocated ranges.

use std::fmt;
use std::io;
use std::sync::Arc;

use pm_core::{ReadAt, SharedSource};

/// Where a run of virtual bytes is stored.
#[derive(Debug, Clone)]
pub enum Run {
    /// Stored contiguously in `src` starting at `offset`.
    Data { src: SharedSource, offset: u64 },
    /// Reads as zeros.
    Zero,
    /// Not present in this image: read from the parent (zeros without one).
    Parent,
    /// Held in memory (a decompressed grain/cluster), from `offset`.
    Buffer { data: Arc<[u8]>, offset: usize },
}

/// Maps virtual offsets of an image to storage.
pub trait BlockMap: Send + Sync + fmt::Debug {
    /// Virtual (media) size in bytes.
    fn size(&self) -> u64;
    /// The storage of the bytes at `pos` (`pos < size()`) and the number of
    /// following bytes (≥ 1) stored the same way.
    fn map(&self, pos: u64) -> io::Result<(Run, u64)>;
}

/// A virtual disk presented as its logical bytes.
pub struct MappedSource<M> {
    map: M,
    parent: Option<SharedSource>,
}

impl<M: fmt::Debug> fmt::Debug for MappedSource<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedSource")
            .field("map", &self.map)
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl<M: BlockMap> MappedSource<M> {
    /// A source reading through `map`, with `parent` serving
    /// [`Run::Parent`] ranges (zeros when `None`).
    pub fn new(map: M, parent: Option<SharedSource>) -> Self {
        Self { map, parent }
    }
}

/// Reads `buf` from `src` at `offset`; bytes past the end of `src` are
/// zero (a parent smaller than its child).
fn read_padded(src: &dyn ReadAt, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    let avail = src.size().saturating_sub(offset).min(buf.len() as u64) as usize;
    let (head, tail) = buf.split_at_mut(avail);
    if !head.is_empty() {
        src.read_exact_at(offset, head)?;
    }
    tail.fill(0);
    Ok(())
}

impl<M: BlockMap> ReadAt for MappedSource<M> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let size = self.map.size();
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let want = (size - offset).min(buf.len() as u64) as usize;
        let mut done = 0usize;
        while done < want {
            let pos = offset + done as u64;
            let (run, len) = self.map.map(pos)?;
            if len == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("image maps an empty run at offset {pos}"),
                ));
            }
            let n = len.min((want - done) as u64) as usize;
            let Some(dst) = buf.get_mut(done..done + n) else {
                break;
            };
            match run {
                Run::Data { src, offset } => src.read_exact_at(offset, dst)?,
                Run::Zero => dst.fill(0),
                Run::Parent => match &self.parent {
                    Some(p) => read_padded(&**p, pos, dst)?,
                    None => dst.fill(0),
                },
                Run::Buffer { data, offset } => {
                    let src = offset
                        .checked_add(n)
                        .and_then(|end| data.get(offset..end))
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("decompressed block too short at offset {pos}"),
                            )
                        })?;
                    dst.copy_from_slice(src);
                }
            }
            done += n;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.map.size()
    }
}
