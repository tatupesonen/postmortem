//! A catalogue of every file in every data source.
//!
//! Built once per data source by a parallel filesystem walk (the equivalent
//! of the file table Autopsy fills when a data source is added), then shared
//! read-only by the analysis tools: Timeline, Data Source Summary, Image
//! Gallery and File Discovery. The case database (M2) will persist the same
//! records; until then this in-memory index plays its role.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{FileKind, FileSystem, InodeId, Times};
use rayon::prelude::*;

/// File categories (Autopsy's Data Source Summary pie-chart slices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileCategory {
    Images,
    Videos,
    Audio,
    Documents,
    Executables,
    Other,
}

impl FileCategory {
    pub const ALL: [Self; 6] = [
        Self::Images,
        Self::Videos,
        Self::Audio,
        Self::Documents,
        Self::Executables,
        Self::Other,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Videos => "Videos",
            Self::Audio => "Audio",
            Self::Documents => "Documents",
            Self::Executables => "Executables",
            Self::Other => "Other",
        }
    }

    /// Autopsy's slice colours (RGB).
    pub fn color(self) -> [u8; 3] {
        match self {
            Self::Images => [156, 39, 176],
            Self::Videos => [255, 235, 59],
            Self::Audio => [33, 150, 243],
            Self::Documents => [76, 175, 80],
            Self::Executables => [0, 188, 212],
            Self::Other => [121, 85, 72],
        }
    }

    /// Classifies by extension (MIME detection replaces this in M4).
    /// Extensions of each category (lower case, without the dot); every
    /// other extension is [`Self::Other`].
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Images => &[
                "jpg", "jpeg", "png", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "ico",
                "psd", "cr2", "nef", "dng", "svg",
            ],
            Self::Videos => &[
                "mp4", "mov", "avi", "mkv", "wmv", "flv", "m4v", "3gp", "3g2", "webm", "mpg",
                "mpeg",
            ],
            Self::Audio => &[
                "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "mid", "midi", "opus", "amr",
            ],
            Self::Documents => &[
                "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "txt", "rtf", "odt", "ods",
                "odp", "csv", "html", "htm", "xml", "json", "md", "log", "css", "js", "wpd",
            ],
            Self::Executables => &[
                "exe", "dll", "sys", "com", "scr", "msi", "bat", "cmd", "ps1", "vbs", "elf", "so",
                "dylib", "sh",
            ],
            Self::Other => &[],
        }
    }

    pub fn from_name(name: &str) -> Self {
        let ext = extension(name).to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|c| c.extensions().contains(&ext.as_str()))
            .unwrap_or(Self::Other)
    }
}

/// Extension of a file name (without the dot), or `""`.
pub fn extension(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.contains(':') => ext,
        _ => "",
    }
}

/// One catalogued file or directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    /// Data source index.
    pub ds: u32,
    /// Filesystem index within the data source.
    pub fs: u32,
    pub inode: InodeId,
    pub path: Arc<str>,
    pub kind: FileKind,
    pub size: u64,
    pub name_allocated: bool,
    pub meta_allocated: bool,
    pub times: Times,
    pub category: FileCategory,
}

impl IndexedFile {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn deleted(&self) -> bool {
        !self.name_allocated || !self.meta_allocated
    }

    pub fn is_regular(&self) -> bool {
        self.kind == FileKind::Regular
    }

    /// Parent directory path (`/` for top-level entries).
    pub fn parent(&self) -> &str {
        match self.path.rfind('/') {
            Some(0) | None => "/",
            Some(i) => self.path.get(..i).unwrap_or("/"),
        }
    }
}

/// A filesystem to index.
#[derive(Debug, Clone)]
pub struct Source {
    pub ds: u32,
    pub fs_index: u32,
    pub fs: Arc<dyn FileSystem>,
}

/// Progress counters, readable while indexing.
#[derive(Debug, Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub cancelled: AtomicBool,
}

/// The catalogue.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub files: Vec<IndexedFile>,
    pub data_sources: Vec<String>,
}

/// Per-data-source statistics (Data Source Summary "Types" tab).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeSummary {
    /// Regular files per category, in [`FileCategory::ALL`] order.
    pub categories: [u64; 6],
    pub allocated_files: u64,
    pub unallocated_files: u64,
    pub directories: u64,
    pub total_bytes: u64,
}

impl FileIndex {
    /// Walks every source in parallel.
    pub fn build(sources: &[Source], data_sources: Vec<String>, progress: &Progress) -> Self {
        let parts: Vec<Vec<IndexedFile>> =
            sources.par_iter().map(|s| index_one(s, progress)).collect();
        let files: Vec<IndexedFile> = parts.into_iter().flatten().collect();
        tracing::info!(files = files.len(), "file index built");
        Self {
            files,
            data_sources,
        }
    }

    pub fn of_data_source(&self, ds: u32) -> impl Iterator<Item = &IndexedFile> {
        self.files.iter().filter(move |f| f.ds == ds)
    }

    pub fn type_summary(&self, ds: u32) -> TypeSummary {
        let mut s = TypeSummary::default();
        for f in self.of_data_source(ds) {
            match f.kind {
                FileKind::Directory => s.directories += 1,
                FileKind::Regular => {
                    if let Some(i) = FileCategory::ALL.iter().position(|c| *c == f.category)
                        && let Some(c) = s.categories.get_mut(i)
                    {
                        *c += 1;
                    }
                    if f.meta_allocated && f.name_allocated {
                        s.allocated_files += 1;
                        s.total_bytes += f.size;
                    } else {
                        s.unallocated_files += 1;
                    }
                }
                _ => {}
            }
        }
        s
    }

    /// The `n` regular files with the latest `time(f)`, newest first.
    pub fn most_recent(
        &self,
        ds: u32,
        n: usize,
        time: impl Fn(&Times) -> Option<jiff::Timestamp>,
    ) -> Vec<&IndexedFile> {
        let mut v: Vec<&IndexedFile> = self
            .of_data_source(ds)
            .filter(|f| f.is_regular() && time(&f.times).is_some())
            .collect();
        v.sort_by_key(|f| std::cmp::Reverse(time(&f.times)));
        v.truncate(n);
        v
    }
}

fn index_one(src: &Source, progress: &Progress) -> Vec<IndexedFile> {
    let mut out = Vec::new();
    let mut push = |path: String, meta: &pm_fs::Metadata, name_allocated: bool, inode: InodeId| {
        let category = FileCategory::from_name(&path);
        out.push(IndexedFile {
            ds: src.ds,
            fs: src.fs_index,
            inode,
            path: Arc::from(path),
            kind: meta.kind,
            size: meta.size,
            name_allocated,
            meta_allocated: meta.is_allocated(),
            times: meta.times,
            category,
        });
        progress.files.fetch_add(1, Ordering::Relaxed);
    };
    walk(&*src.fs, &WalkOptions::default(), |item| {
        if progress.cancelled.load(Ordering::Relaxed) {
            return ControlFlow::Break(());
        }
        if let Ok(meta) = &item.meta
            && meta.kind != FileKind::Virtual
        {
            push(item.path(), meta, item.entry.allocated, item.entry.inode);
        }
        ControlFlow::Continue(())
    });
    if let Ok(orphans) = src.fs.orphans() {
        for o in orphans {
            if let Ok(meta) = src.fs.metadata(o.inode) {
                push(format!("/$OrphanFiles/{}", o.name), &meta, false, o.inode);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;
    use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};
    use rstest::rstest;

    #[rstest]
    #[case("a/b/photo.JPG", FileCategory::Images)]
    #[case("clip.mov", FileCategory::Videos)]
    #[case("song.mp3", FileCategory::Audio)]
    #[case("cv.docx", FileCategory::Documents)]
    #[case("setup.exe", FileCategory::Executables)]
    #[case("Makefile", FileCategory::Other)]
    #[case(".bashrc", FileCategory::Other)]
    #[case("file.exe:Zone.Identifier", FileCategory::Other)]
    fn categories(#[case] name: &str, #[case] want: FileCategory) {
        assert_eq!(FileCategory::from_name(name), want);
    }

    #[test]
    fn builds_and_summarises() {
        let mut b = NtfsBuilder::new(4);
        b.dir("d");
        b.file("d/a.jpg", b"x");
        b.file("d/b.txt", b"yy");
        b.file_with(
            "d/gone.exe",
            b"z",
            FileOpts {
                deleted: Deleted::Unindexed,
                ..Default::default()
            },
        );
        let fs: Arc<dyn FileSystem> =
            Arc::new(pm_fs_ntfs::NtfsFs::open(Arc::new(MemSource::new(b.build()))).unwrap());
        let progress = Progress::default();
        let idx = FileIndex::build(
            &[Source {
                ds: 0,
                fs_index: 0,
                fs,
            }],
            vec!["img".into()],
            &progress,
        );
        let s = idx.type_summary(0);
        assert_eq!(s.categories[0], 1, "one image");
        assert_eq!(s.categories[3], 1, "one document");
        assert_eq!(s.categories[4], 1, "deleted executable still counted");
        assert_eq!(s.unallocated_files, 1);
        assert!(s.directories >= 2, "root-level dirs and d");
        let a = idx.files.iter().find(|f| f.name() == "a.jpg").unwrap();
        assert_eq!(a.parent(), "/d");
        assert_eq!(idx.most_recent(0, 2, |t| t.modified).len(), 2);
        assert_eq!(
            progress.files.load(Ordering::Relaxed) as usize,
            idx.files.len()
        );
    }
}
