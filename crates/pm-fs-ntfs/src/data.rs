//! Reading attribute content (non-resident, sparse, compressed).

use std::io;
use std::sync::Arc;

use pm_core::cache::ShardedLru;
use pm_core::{ReadAt, SharedSource};
use pm_fs::{Extent, ExtentKind, ExtentSource};

use crate::lznt1;
use crate::record::Run;

/// Converts runs to byte extents, clipped to `limit` bytes of logical data.
pub(crate) fn runs_to_extents(runs: &[Run], cluster_size: u64, limit: u64) -> Vec<Extent> {
    let mut out = Vec::with_capacity(runs.len());
    for r in runs {
        let logical = r.vcn.saturating_mul(cluster_size);
        if logical >= limit {
            break;
        }
        let len = r.len.saturating_mul(cluster_size).min(limit - logical);
        out.push(match r.lcn {
            Some(lcn) => Extent {
                logical_offset: logical,
                physical_offset: lcn.saturating_mul(cluster_size),
                len,
                kind: ExtentKind::Allocated,
            },
            None => Extent {
                logical_offset: logical,
                physical_offset: 0,
                len,
                kind: ExtentKind::Sparse,
            },
        });
    }
    out
}

/// An uncompressed non-resident stream. Bytes between the initialized size
/// and the real size read as zeros (as Windows returns them).
pub(crate) fn plain_stream(
    fs: SharedSource,
    runs: &[Run],
    cluster_size: u64,
    real: u64,
    init: u64,
) -> SharedSource {
    let extents = runs_to_extents(runs, cluster_size, init.min(real));
    Arc::new(ExtentSource::new(fs, extents, real))
}

/// A compressed (LZNT1) non-resident stream, decompressed one compression
/// unit at a time with a small cache.
#[derive(Debug)]
pub(crate) struct CompressedStream {
    fs: SharedSource,
    runs: Vec<Run>,
    cluster_size: u64,
    unit_clusters: u64,
    size: u64,
    cache: ShardedLru<u64, Arc<[u8]>>,
}

impl CompressedStream {
    pub(crate) fn new(
        fs: SharedSource,
        runs: Vec<Run>,
        cluster_size: u64,
        unit_exp: u16,
        size: u64,
    ) -> Self {
        Self {
            fs,
            runs,
            cluster_size,
            unit_clusters: 1u64 << unit_exp.min(16),
            size,
            cache: ShardedLru::new(4, 4),
        }
    }

    fn unit_bytes(&self) -> u64 {
        self.unit_clusters * self.cluster_size
    }

    /// Physical cluster for each VCN of unit `u` (`None` = sparse).
    fn unit_lcns(&self, u: u64) -> Vec<Option<u64>> {
        let first = u * self.unit_clusters;
        (first..first + self.unit_clusters)
            .map(|vcn| {
                self.runs
                    .iter()
                    .find(|r| vcn >= r.vcn && vcn < r.vcn + r.len)
                    .and_then(|r| r.lcn.map(|l| l.saturating_add(vcn - r.vcn)))
            })
            .collect()
    }

    fn unit(&self, u: u64) -> io::Result<Arc<[u8]>> {
        self.cache.get_or_try_insert(&u, || {
            let ub = self.unit_bytes() as usize;
            let lcns = self.unit_lcns(u);
            let stored: Vec<u64> = lcns.iter().map_while(|l| *l).collect();
            let mut raw = Vec::with_capacity(stored.len() * self.cluster_size as usize);
            for lcn in &stored {
                let mut c = vec![0u8; self.cluster_size as usize];
                let off = lcn.checked_mul(self.cluster_size).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "cluster offset overflows")
                })?;
                self.fs.read_exact_at(off, &mut c)?;
                raw.extend_from_slice(&c);
            }
            let out: Vec<u8> = if stored.is_empty() {
                vec![0; ub] // fully sparse unit
            } else if stored.len() as u64 == self.unit_clusters {
                raw // stored uncompressed
            } else {
                let mut out = Vec::with_capacity(ub);
                lznt1::decompress(&raw, &mut out, ub);
                out.resize(ub, 0);
                out
            };
            Ok(Arc::from(out))
        })
    }
}

impl ReadAt for CompressedStream {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let want = (self.size - offset).min(buf.len() as u64) as usize;
        let ub = self.unit_bytes();
        let mut done = 0;
        while done < want {
            let pos = offset + done as u64;
            let unit = self.unit(pos / ub)?;
            let within = (pos % ub) as usize;
            let src = unit.get(within..).unwrap_or_default();
            let n = src.len().min(want - done);
            if n == 0 {
                break;
            }
            let (Some(dst), Some(src)) = (buf.get_mut(done..done + n), src.get(..n)) else {
                break;
            };
            dst.copy_from_slice(src);
            done += n;
        }
        Ok(done)
    }

    fn size(&self) -> u64 {
        self.size
    }
}
