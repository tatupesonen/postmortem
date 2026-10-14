//! A lazily generated file system of arbitrary size, for scale tests and
//! benchmarks of everything above the parsers (loading, queries, UI).
//!
//! The tree is a complete `fanout`-ary tree of directories `depth` levels
//! deep; every directory also holds `files_per_dir` regular files. Nothing
//! is stored: names, sizes, times and allocation state are derived from the
//! inode number, so a 100-million-file tree costs no memory.

use std::sync::Arc;

use jiff::Timestamp;
use pm_core::SharedSource;
use pm_core::io::ZeroSource;
use pm_fs::{
    DirEntry, FileKind, FileSystem, FsError, FsInfo, FsKind, InodeId, MetaFlags, Metadata, Result,
    Times,
};

const FILE_BITS: u32 = 24;
const FILE_MASK: u64 = (1 << FILE_BITS) - 1;
const EXTENSIONS: &[&str] = &[
    "jpg", "docx", "txt", "exe", "mp4", "pdf", "png", "dll", "log",
];

/// Synthetic file system; see the module docs.
#[derive(Debug, Clone)]
pub struct SynthFs {
    info: FsInfo,
    fanout: u64,
    depth: u32,
    files_per_dir: u64,
    dirs: u64,
}

impl SynthFs {
    pub fn new(fanout: u64, depth: u32, files_per_dir: u64) -> Self {
        assert!(fanout >= 1 && files_per_dir < FILE_MASK);
        let dirs: u64 = (0..=depth).map(|l| fanout.pow(l)).sum();
        Self {
            info: FsInfo {
                kind: FsKind::Ext4,
                block_size: 4096,
                block_count: 1 << 40,
                root_inode: 0,
                first_inode: 0,
                last_inode: (dirs << FILE_BITS) | files_per_dir,
                label: Some("synthetic".into()),
                serial: None,
            },
            fanout,
            depth,
            files_per_dir,
            dirs,
        }
    }

    /// Number of names below the root (directories + files, excluding the
    /// root itself).
    pub fn names(&self) -> u64 {
        (self.dirs - 1) + self.dirs * self.files_per_dir
    }

    /// Number of names whose entry is deleted (every 10th file).
    pub fn deleted(&self) -> u64 {
        self.dirs * self.files_per_dir.div_ceil(10)
    }

    fn level(&self, dir: u64) -> u32 {
        let mut first = 0u64;
        let mut width = 1u64;
        for l in 0..=self.depth {
            if dir < first + width {
                return l;
            }
            first += width;
            width *= self.fanout;
        }
        u32::MAX
    }

    fn file_name(dir: u64, j: u64) -> String {
        let ext = EXTENSIONS[((dir + j) % EXTENSIONS.len() as u64) as usize];
        format!("file_{j:05}.{ext}")
    }

    fn mix(x: u64) -> u64 {
        let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl FileSystem for SynthFs {
    fn info(&self) -> &FsInfo {
        &self.info
    }

    fn metadata(&self, inode: InodeId) -> Result<Metadata> {
        let dir = inode >> FILE_BITS;
        let j = inode & FILE_MASK;
        if dir >= self.dirs || j > self.files_per_dir {
            return Err(FsError::NotFound(inode));
        }
        let h = Self::mix(inode);
        let base = 1_500_000_000 + (h % 300_000_000) as i64;
        let t = |d: i64| Timestamp::from_second(base + d).ok();
        let mut m = if j == 0 {
            Metadata::new(inode, FileKind::Directory, 0)
        } else {
            Metadata::new(inode, FileKind::Regular, h % 2_000_000)
        };
        m.times = Times {
            modified: t(0),
            accessed: t(86_400),
            changed: t(60),
            created: t(-86_400),
        };
        if j > 0 && (j - 1).is_multiple_of(10) {
            m.flags = MetaFlags::USED;
        }
        Ok(m)
    }

    fn read_dir(&self, inode: InodeId) -> Result<Vec<DirEntry>> {
        let dir = inode >> FILE_BITS;
        if inode & FILE_MASK != 0 || dir >= self.dirs {
            return Err(FsError::NotFound(inode));
        }
        let mut out = Vec::with_capacity((self.fanout + self.files_per_dir) as usize);
        if self.level(dir) < self.depth {
            for c in 1..=self.fanout {
                let child = dir * self.fanout + c;
                out.push(DirEntry {
                    name: format!("dir_{c:03}"),
                    short_name: None,
                    inode: child << FILE_BITS,
                    seq: None,
                    kind: FileKind::Directory,
                    allocated: true,
                });
            }
        }
        for j in 1..=self.files_per_dir {
            out.push(DirEntry {
                name: Self::file_name(dir, j),
                short_name: None,
                inode: (dir << FILE_BITS) | j,
                seq: None,
                kind: FileKind::Regular,
                allocated: !(j - 1).is_multiple_of(10),
            });
        }
        Ok(out)
    }

    fn open(&self, inode: InodeId, _stream: Option<u32>) -> Result<SharedSource> {
        let m = self.metadata(inode)?;
        Ok(Arc::new(ZeroSource::new(m.size)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_match_a_walk() {
        let fs = SynthFs::new(3, 2, 5);
        let mut names = 0u64;
        let mut deleted = 0u64;
        let mut stack = vec![fs.root()];
        while let Some(d) = stack.pop() {
            for e in fs.read_dir(d).unwrap() {
                names += 1;
                deleted += u64::from(!e.allocated);
                let m = fs.metadata(e.inode).unwrap();
                if m.kind == FileKind::Directory {
                    stack.push(e.inode);
                }
            }
        }
        assert_eq!(names, fs.names());
        assert_eq!(deleted, fs.deleted());
        assert_eq!(fs.names(), 12 + 13 * 5);
    }
}
