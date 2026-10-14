//! Loop-safe traversal of a filesystem's directory tree.
//!
//! Corrupt or hostile filesystems can contain directory cycles (a FAT
//! subdirectory whose cluster points at an ancestor, reused NTFS records,
//! …). The walker visits every directory inode at most once and caps depth,
//! so traversal always terminates.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;

use crate::{DirEntry, FileKind, FileSystem, FsError, InodeId, Metadata};

/// Traversal options.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Descend into directories whose *name* is deleted.
    pub descend_deleted_dirs: bool,
    /// Maximum directory depth below the root.
    pub max_depth: usize,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            descend_deleted_dirs: true,
            max_depth: 512,
        }
    }
}

/// One visited name.
#[derive(Debug)]
pub struct WalkItem {
    /// Path of the parent directory, `/`-separated, starting with `/`.
    pub parent_path: Arc<str>,
    /// Inode of the parent directory.
    pub parent_inode: InodeId,
    pub entry: DirEntry,
    /// Metadata the name points at (errors are reported, not fatal).
    pub meta: Result<Metadata, FsError>,
    pub depth: usize,
}

impl WalkItem {
    pub fn path(&self) -> String {
        if self.parent_path.ends_with('/') {
            format!("{}{}", self.parent_path, self.entry.name)
        } else {
            format!("{}/{}", self.parent_path, self.entry.name)
        }
    }
}

/// A non-fatal problem encountered while walking.
#[derive(Debug)]
pub struct WalkWarning {
    pub path: String,
    pub inode: InodeId,
    pub error: FsError,
}

/// Summary returned by [`walk`].
#[derive(Debug, Default)]
pub struct WalkStats {
    pub names: u64,
    pub directories: u64,
    pub skipped_cycles: u64,
    pub warnings: Vec<WalkWarning>,
}

/// Visits every name reachable from the root, depth-first (pre-order),
/// calling `visit` for each. Returning `ControlFlow::Break` stops the walk.
pub fn walk(
    fs: &dyn FileSystem,
    opts: &WalkOptions,
    mut visit: impl FnMut(&WalkItem) -> ControlFlow<()>,
) -> WalkStats {
    let mut stats = WalkStats::default();
    let root = fs.root();
    let mut visited: HashSet<InodeId> = HashSet::from([root]);
    // Stack of (dir inode, dir path, depth).
    let mut stack: Vec<(InodeId, Arc<str>, usize)> = vec![(root, Arc::from("/"), 0)];

    while let Some((dir, dir_path, depth)) = stack.pop() {
        stats.directories += 1;
        let entries = match fs.read_dir(dir) {
            Ok(e) => e,
            Err(error) => {
                stats.warnings.push(WalkWarning {
                    path: dir_path.to_string(),
                    inode: dir,
                    error,
                });
                continue;
            }
        };
        let mut subdirs = Vec::new();
        for entry in entries {
            let meta = fs.metadata(entry.inode);
            let is_dir = match &meta {
                Ok(m) => m.kind == FileKind::Directory,
                Err(_) => entry.kind == FileKind::Directory,
            };
            let item = WalkItem {
                parent_path: Arc::clone(&dir_path),
                parent_inode: dir,
                entry,
                meta,
                depth,
            };
            stats.names += 1;
            if visit(&item).is_break() {
                return stats;
            }
            let descend = is_dir
                && item.meta.is_ok()
                && (item.entry.allocated || opts.descend_deleted_dirs)
                && depth < opts.max_depth;
            if descend {
                if visited.insert(item.entry.inode) {
                    subdirs.push((item.entry.inode, Arc::from(item.path()), depth + 1));
                } else {
                    stats.skipped_cycles += 1;
                }
            }
        }
        // Push in reverse so children are visited in directory order.
        stack.extend(subdirs.into_iter().rev());
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FsInfo, FsKind};
    use pm_core::SharedSource;
    use pm_core::io::MemSource;
    use std::collections::HashMap;

    /// Tiny in-memory filesystem for walker tests.
    #[derive(Debug)]
    struct MemFs {
        info: FsInfo,
        dirs: HashMap<InodeId, Vec<DirEntry>>,
        kinds: HashMap<InodeId, FileKind>,
    }

    impl MemFs {
        fn new() -> Self {
            let info = FsInfo {
                kind: FsKind::Fat32,
                block_size: 512,
                block_count: 0,
                root_inode: 1,
                first_inode: 1,
                last_inode: 100,
                label: None,
                serial: None,
            };
            let mut s = Self {
                info,
                dirs: HashMap::new(),
                kinds: HashMap::new(),
            };
            s.kinds.insert(1, FileKind::Directory);
            s
        }
        fn add(
            &mut self,
            parent: InodeId,
            name: &str,
            inode: InodeId,
            kind: FileKind,
            allocated: bool,
        ) {
            self.kinds.insert(inode, kind);
            self.dirs.entry(parent).or_default().push(DirEntry {
                name: name.into(),
                short_name: None,
                inode,
                seq: None,
                kind,
                allocated,
            });
        }
    }

    impl FileSystem for MemFs {
        fn info(&self) -> &FsInfo {
            &self.info
        }
        fn metadata(&self, inode: InodeId) -> crate::Result<Metadata> {
            let kind = *self.kinds.get(&inode).ok_or(FsError::NotFound(inode))?;
            Ok(Metadata::new(inode, kind, 0))
        }
        fn read_dir(&self, inode: InodeId) -> crate::Result<Vec<DirEntry>> {
            Ok(self.dirs.get(&inode).cloned().unwrap_or_default())
        }
        fn open(&self, _inode: InodeId, _stream: Option<u32>) -> crate::Result<SharedSource> {
            Ok(Arc::new(MemSource::new(Vec::new())))
        }
    }

    fn paths(fs: &MemFs, opts: &WalkOptions) -> (Vec<String>, WalkStats) {
        let mut out = Vec::new();
        let stats = walk(fs, opts, |i| {
            out.push(i.path());
            ControlFlow::Continue(())
        });
        (out, stats)
    }

    #[test]
    fn visits_in_preorder_directory_order() {
        let mut fs = MemFs::new();
        fs.add(1, "a", 2, FileKind::Directory, true);
        fs.add(2, "a1.txt", 3, FileKind::Regular, true);
        fs.add(1, "b.txt", 4, FileKind::Regular, true);
        fs.add(2, "sub", 5, FileKind::Directory, true);
        fs.add(5, "deep", 6, FileKind::Regular, true);
        let (p, stats) = paths(&fs, &WalkOptions::default());
        assert_eq!(p, ["/a", "/b.txt", "/a/a1.txt", "/a/sub", "/a/sub/deep"]);
        assert_eq!(stats.names, 5);
        assert_eq!(stats.directories, 3);
    }

    #[test]
    fn cycles_terminate() {
        let mut fs = MemFs::new();
        fs.add(1, "a", 2, FileKind::Directory, true);
        fs.add(2, "loop", 1, FileKind::Directory, true); // points back at root
        fs.add(2, "self", 2, FileKind::Directory, true);
        let (p, stats) = paths(&fs, &WalkOptions::default());
        assert_eq!(p, ["/a", "/a/loop", "/a/self"]);
        assert_eq!(stats.skipped_cycles, 2);
    }

    #[test]
    fn deleted_dirs_optional() {
        let mut fs = MemFs::new();
        fs.add(1, "gone", 2, FileKind::Directory, false);
        fs.add(2, "inner", 3, FileKind::Regular, false);
        let (p, _) = paths(&fs, &WalkOptions::default());
        assert_eq!(p, ["/gone", "/gone/inner"]);
        let (p, _) = paths(
            &fs,
            &WalkOptions {
                descend_deleted_dirs: false,
                ..Default::default()
            },
        );
        assert_eq!(p, ["/gone"]);
    }

    #[test]
    fn dangling_inodes_reported_not_fatal() {
        let mut fs = MemFs::new();
        fs.add(1, "ok", 2, FileKind::Regular, true);
        fs.dirs.entry(1).or_default().push(DirEntry {
            name: "dangling".into(),
            short_name: None,
            inode: 99,
            seq: None,
            kind: FileKind::Directory,
            allocated: false,
        });
        let mut errs = 0;
        walk(&fs, &WalkOptions::default(), |i| {
            errs += usize::from(i.meta.is_err());
            ControlFlow::Continue(())
        });
        assert_eq!(errs, 1);
    }

    #[test]
    fn depth_limit_and_break() {
        let mut fs = MemFs::new();
        for i in 1..50 {
            fs.add(i, "d", i + 1, FileKind::Directory, true);
        }
        let (p, _) = paths(
            &fs,
            &WalkOptions {
                max_depth: 3,
                ..Default::default()
            },
        );
        assert_eq!(p.len(), 4);
        let mut n = 0;
        walk(&fs, &WalkOptions::default(), |_| {
            n += 1;
            if n == 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        assert_eq!(n, 2);
    }
}
