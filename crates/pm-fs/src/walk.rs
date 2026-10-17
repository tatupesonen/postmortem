//! Loop-safe traversal of a filesystem's directory tree.
//!
//! Corrupt or hostile filesystems can contain directory cycles (a FAT
//! subdirectory whose cluster points at an ancestor, reused NTFS records,
//! …). The walker visits every directory inode at most once and caps depth,
//! so traversal always terminates.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use crate::{DirEntry, FileSystem, FsError, InodeId, Metadata};

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

/// How many times one directory may be listed in a single walk. Autopsy only
/// guards against cycles on the current path, so a directory reachable
/// through several names is listed under each; the cap keeps crafted
/// images (a lattice of directories) from exploding.
pub const MAX_LISTINGS_PER_DIR: u8 = 8;

/// Directories on the path from the root to the current one.
#[derive(Debug)]
pub struct Ancestors {
    inode: InodeId,
    parent: Option<Arc<Ancestors>>,
}

impl Ancestors {
    pub fn root(inode: InodeId) -> Arc<Self> {
        Arc::new(Self {
            inode,
            parent: None,
        })
    }

    pub fn child(self: &Arc<Self>, inode: InodeId) -> Arc<Self> {
        Arc::new(Self {
            inode,
            parent: Some(Arc::clone(self)),
        })
    }

    /// Whether `inode` is this directory or one of its ancestors.
    pub fn contains(&self, inode: InodeId) -> bool {
        let mut cur = Some(self);
        while let Some(a) = cur {
            if a.inode == inode {
                return true;
            }
            cur = a.parent.as_deref();
        }
        false
    }
}

/// Visits every name reachable from the root, depth-first (pre-order),
/// calling `visit` for each. Returning `ControlFlow::Break` stops the walk.
///
/// Follows Autopsy's rules: metadata comes from [`crate::entry_metadata`],
/// directories are entered per [`crate::should_descend`], and a directory
/// already on the current path is a cycle.
pub fn walk(
    fs: &dyn FileSystem,
    opts: &WalkOptions,
    mut visit: impl FnMut(&WalkItem) -> ControlFlow<()>,
) -> WalkStats {
    let mut stats = WalkStats::default();
    let root = fs.root();
    let mut listings: HashMap<InodeId, u8> = HashMap::new();
    // Stack of (dir inode, dir path, depth, path from the root).
    let mut stack: Vec<(InodeId, Arc<str>, usize, Arc<Ancestors>)> =
        vec![(root, Arc::from("/"), 0, Ancestors::root(root))];

    while let Some((dir, dir_path, depth, ancestors)) = stack.pop() {
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
            let meta = crate::entry_metadata(fs, &entry);
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
            let descend = item
                .meta
                .as_ref()
                .is_ok_and(|m| crate::should_descend(&item.entry, m))
                && (item.entry.allocated || opts.descend_deleted_dirs)
                && depth < opts.max_depth;
            if descend {
                let n = listings.entry(item.entry.inode).or_default();
                if ancestors.contains(item.entry.inode) || *n >= MAX_LISTINGS_PER_DIR {
                    stats.skipped_cycles += 1;
                } else {
                    *n += 1;
                    subdirs.push((
                        item.entry.inode,
                        Arc::from(item.path()),
                        depth + 1,
                        ancestors.child(item.entry.inode),
                    ));
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
    use crate::{FileKind, FsInfo, FsKind};
    use pm_core::SharedSource;
    use pm_core::io::MemSource;
    use std::collections::HashMap;

    /// Tiny in-memory filesystem for walker tests.
    #[derive(Debug)]
    struct MemFs {
        info: FsInfo,
        dirs: HashMap<InodeId, Vec<DirEntry>>,
        kinds: HashMap<InodeId, FileKind>,
        unallocated: std::collections::HashSet<InodeId>,
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
                unallocated: std::collections::HashSet::new(),
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
            let mut m = Metadata::new(inode, kind, 0);
            if self.unallocated.contains(&inode) {
                m.flags = crate::MetaFlags::USED;
            }
            Ok(m)
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
    fn deleted_name_of_reallocated_dir_is_not_followed() {
        let mut fs = MemFs::new();
        // Inode 2 was "old" (deleted name) and is now allocated as "new".
        fs.add(1, "old", 2, FileKind::Directory, false);
        fs.add(1, "new", 2, FileKind::Directory, true);
        fs.add(2, "file", 3, FileKind::Regular, true);
        let (p, _) = paths(&fs, &WalkOptions::default());
        assert_eq!(p, ["/old", "/new", "/new/file"]);
    }

    #[test]
    fn directory_with_two_names_is_listed_under_both() {
        let mut fs = MemFs::new();
        fs.add(1, "a", 2, FileKind::Directory, true);
        fs.add(1, "b", 2, FileKind::Directory, true);
        fs.add(2, "f", 3, FileKind::Regular, true);
        let (p, _) = paths(&fs, &WalkOptions::default());
        assert_eq!(p, ["/a", "/b", "/a/f", "/b/f"]);
    }

    #[test]
    fn lattice_of_shared_directories_is_capped() {
        // Each level has two names for the same next directory: 2^30 paths
        // without a cap.
        let mut fs = MemFs::new();
        for level in 0..30u64 {
            let (dir, next) = (if level == 0 { 1 } else { level + 1 }, level + 2);
            fs.add(dir, "x", next, FileKind::Directory, true);
            fs.add(dir, "y", next, FileKind::Directory, true);
        }
        let (_, stats) = paths(&fs, &WalkOptions::default());
        assert!(stats.directories <= 30 * u64::from(MAX_LISTINGS_PER_DIR) + 1);
    }

    #[test]
    fn deleted_dirs_optional() {
        let mut fs = MemFs::new();
        fs.add(1, "gone", 2, FileKind::Directory, false);
        fs.unallocated.insert(2);
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
