//! File Discovery (Autopsy's Discovery tool for images, videos and
//! documents): filter the file catalogue, group the results and order the
//! groups and their members, with Autopsy's exact size buckets.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use pm_index::{FileCategory, FileIndex, IndexedFile};

/// What to discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultType {
    Images,
    Videos,
    Documents,
}

impl ResultType {
    pub const ALL: [Self; 3] = [Self::Images, Self::Videos, Self::Documents];

    pub fn name(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Videos => "Videos",
            Self::Documents => "Documents",
        }
    }

    fn category(self) -> FileCategory {
        match self {
            Self::Images => FileCategory::Images,
            Self::Videos => FileCategory::Videos,
            Self::Documents => FileCategory::Documents,
        }
    }
}

/// Autopsy's size buckets (decimal units; lower bounds exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SizeBucket {
    XXLarge,
    XLarge,
    Large,
    Medium,
    Small,
    XSmall,
}

const KB: u64 = 1_000;
const MB: u64 = 1_000_000;
const GB: u64 = 1_000_000_000;

impl SizeBucket {
    pub const ALL: [Self; 6] = [
        Self::XXLarge,
        Self::XLarge,
        Self::Large,
        Self::Medium,
        Self::Small,
        Self::XSmall,
    ];

    /// `(min, max)` with `min < size <= max` (`max = None` is open-ended).
    pub fn bounds(self, video: bool) -> (u64, Option<u64>) {
        match (video, self) {
            (false, Self::XXLarge) => (200 * MB, None),
            (false, Self::XLarge) => (50 * MB, Some(200 * MB)),
            (false, Self::Large) => (MB, Some(50 * MB)),
            (false, Self::Medium) => (100 * KB, Some(MB)),
            (false, Self::Small) => (16 * KB, Some(100 * KB)),
            (false, Self::XSmall) => (0, Some(16 * KB)),
            (true, Self::XXLarge) => (10 * GB, None),
            (true, Self::XLarge) => (5 * GB, Some(10 * GB)),
            (true, Self::Large) => (GB, Some(5 * GB)),
            (true, Self::Medium) => (100 * MB, Some(GB)),
            (true, Self::Small) => (500 * KB, Some(100 * MB)),
            (true, Self::XSmall) => (0, Some(500 * KB)),
        }
    }

    pub fn of(size: u64, video: bool) -> Self {
        Self::ALL
            .into_iter()
            .find(|b| {
                let (min, max) = b.bounds(video);
                (size > min || (min == 0 && size == 0)) && max.is_none_or(|m| size <= m)
            })
            .unwrap_or(Self::XSmall)
    }

    /// Display label, e.g. `Large: 1-50MB`.
    pub fn label(self, video: bool) -> String {
        let name = match self {
            Self::XXLarge => "XXLarge",
            Self::XLarge => "XLarge",
            Self::Large => "Large",
            Self::Medium => "Medium",
            Self::Small => "Small",
            Self::XSmall => "XSmall",
        };
        let fmt = |b: u64| {
            if b >= GB {
                format!("{}GB", b / GB)
            } else if b >= MB {
                format!("{}MB", b / MB)
            } else {
                format!("{}kB", b / KB)
            }
        };
        match self.bounds(video) {
            (min, None) => format!("{name}: {}+", fmt(min)),
            (min, Some(max)) => format!(
                "{name}: {}-{}",
                if min == 0 { "0".to_owned() } else { fmt(min) },
                fmt(max)
            ),
        }
    }
}

/// A parent-folder term (Autopsy: Full/Substring × Include/Exclude).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTerm {
    pub text: String,
    pub substring: bool,
    pub include: bool,
}

/// Search criteria. Empty sets mean "no restriction".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criteria {
    pub result_type: ResultType,
    pub sizes: HashSet<SizeBucket>,
    pub data_sources: HashSet<u32>,
    pub folders: Vec<FolderTerm>,
    pub include_deleted: bool,
}

impl Criteria {
    /// Autopsy's defaults: size filter on (Large, `XLarge`, `XXLarge`), and
    /// `/Windows/` and `/Program Files/` excluded.
    pub fn new(result_type: ResultType) -> Self {
        Self {
            result_type,
            sizes: [SizeBucket::XXLarge, SizeBucket::XLarge, SizeBucket::Large].into(),
            data_sources: HashSet::new(),
            folders: vec![
                FolderTerm {
                    text: "/Windows/".into(),
                    substring: true,
                    include: false,
                },
                FolderTerm {
                    text: "/Program Files/".into(),
                    substring: true,
                    include: false,
                },
            ],
            include_deleted: true,
        }
    }

    fn folder_ok(&self, f: &IndexedFile) -> bool {
        // Autopsy compares against parent paths that end with '/'.
        let parent = format!("{}/", f.parent().trim_end_matches('/'));
        let hit = |t: &FolderTerm| {
            let (p, needle) = (parent.to_lowercase(), t.text.to_lowercase());
            if t.substring {
                p.contains(&needle)
            } else {
                p == needle || p.trim_end_matches('/') == needle.trim_end_matches('/')
            }
        };
        let includes: Vec<&FolderTerm> = self.folders.iter().filter(|t| t.include).collect();
        let excluded = self.folders.iter().filter(|t| !t.include).any(hit);
        !excluded && (includes.is_empty() || includes.iter().any(|t| hit(t)))
    }

    pub fn matches(&self, f: &IndexedFile) -> bool {
        let video = self.result_type == ResultType::Videos;
        f.is_regular()
            && f.category == self.result_type.category()
            && (self.include_deleted || !f.deleted())
            && (self.sizes.is_empty() || self.sizes.contains(&SizeBucket::of(f.size, video)))
            && (self.data_sources.is_empty() || self.data_sources.contains(&f.ds))
            && self.folder_ok(f)
    }
}

/// Grouping attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupBy {
    FileSize,
    ParentFolder,
    DataSource,
    Extension,
    None,
}

impl GroupBy {
    pub const ALL: [Self; 5] = [
        Self::ParentFolder,
        Self::FileSize,
        Self::DataSource,
        Self::Extension,
        Self::None,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::FileSize => "File Size",
            Self::ParentFolder => "Parent Folder",
            Self::DataSource => "Data Source",
            Self::Extension => "Extension",
            Self::None => "None (all files)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupOrder {
    GroupSize,
    GroupName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemOrder {
    FileSize,
    FullPath,
    FileName,
    DataSource,
}

impl ItemOrder {
    pub const ALL: [Self; 4] = [
        Self::FileName,
        Self::FileSize,
        Self::FullPath,
        Self::DataSource,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::FileSize => "File Size",
            Self::FullPath => "Full Path",
            Self::FileName => "File Name",
            Self::DataSource => "Data Source",
        }
    }
}

/// Group key with its natural ordering (Autopsy's `BY_GROUP_NAME` order).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroupKey {
    /// Larger buckets first.
    Size(SizeBucket),
    Text(String),
    DataSource(u32),
    All,
}

/// One result group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub key: GroupKey,
    pub label: String,
    /// Indices into the file index, in item order.
    pub files: Vec<usize>,
}

/// Runs a discovery search over `index`.
pub fn search(
    index: &FileIndex,
    c: &Criteria,
    group_by: GroupBy,
    group_order: GroupOrder,
    item_order: ItemOrder,
) -> Vec<Group> {
    let video = c.result_type == ResultType::Videos;
    let mut groups: BTreeMap<GroupKey, Vec<usize>> = BTreeMap::new();
    for (i, f) in index.files.iter().enumerate() {
        if !c.matches(f) {
            continue;
        }
        let key = match group_by {
            GroupBy::FileSize => GroupKey::Size(SizeBucket::of(f.size, video)),
            GroupBy::ParentFolder => GroupKey::Text(f.parent().to_owned()),
            GroupBy::DataSource => GroupKey::DataSource(f.ds),
            GroupBy::Extension => GroupKey::Text(pm_index::extension(f.name()).to_lowercase()),
            GroupBy::None => GroupKey::All,
        };
        groups.entry(key).or_default().push(i);
    }
    let item_cmp = |a: &usize, b: &usize| -> Ordering {
        let (Some(fa), Some(fb)) = (index.files.get(*a), index.files.get(*b)) else {
            return Ordering::Equal;
        };
        let primary = match item_order {
            ItemOrder::FileSize => fb.size.cmp(&fa.size),
            ItemOrder::FullPath => fa.parent().to_lowercase().cmp(&fb.parent().to_lowercase()),
            ItemOrder::FileName => Ordering::Equal,
            ItemOrder::DataSource => fa.ds.cmp(&fb.ds),
        };
        // Autopsy's default tie-break: name, then identity.
        primary
            .then_with(|| fa.name().to_lowercase().cmp(&fb.name().to_lowercase()))
            .then(a.cmp(b))
    };
    let mut out: Vec<Group> = groups
        .into_iter()
        .map(|(key, mut files)| {
            files.sort_by(item_cmp);
            let label = match &key {
                GroupKey::Size(b) => b.label(video),
                GroupKey::Text(t) if t.is_empty() => "(none)".to_owned(),
                GroupKey::Text(t) => t.clone(),
                GroupKey::DataSource(d) => index
                    .data_sources
                    .get(*d as usize)
                    .map_or_else(|| format!("data source {d}"), |n| format!("{n} (ID: {d})")),
                GroupKey::All => "All Files".to_owned(),
            };
            Group { key, label, files }
        })
        .collect();
    if group_order == GroupOrder::GroupSize {
        out.sort_by(|a, b| {
            b.files
                .len()
                .cmp(&a.files.len())
                .then_with(|| a.key.cmp(&b.key))
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_fs::{FileKind, Times};
    use rstest::rstest;
    use std::sync::Arc;

    fn file(ds: u32, path: &str, size: u64, deleted: bool) -> IndexedFile {
        IndexedFile {
            ds,
            fs: 0,
            inode: size,
            path: Arc::from(path),
            kind: FileKind::Regular,
            size,
            name_allocated: !deleted,
            meta_allocated: !deleted,
            times: Times::default(),
            category: pm_index::FileCategory::from_name(path),
        }
    }

    #[rstest]
    #[case(0, false, SizeBucket::XSmall)]
    #[case(16_000, false, SizeBucket::XSmall)]
    #[case(16_001, false, SizeBucket::Small)]
    #[case(1_000_000, false, SizeBucket::Medium)]
    #[case(1_000_001, false, SizeBucket::Large)]
    #[case(200_000_001, false, SizeBucket::XXLarge)]
    #[case(500_000, true, SizeBucket::XSmall)]
    #[case(500_001, true, SizeBucket::Small)]
    #[case(10_000_000_001, true, SizeBucket::XXLarge)]
    fn size_buckets_match_autopsy(
        #[case] size: u64,
        #[case] video: bool,
        #[case] want: SizeBucket,
    ) {
        assert_eq!(SizeBucket::of(size, video), want);
    }

    #[test]
    fn labels() {
        assert_eq!(SizeBucket::Large.label(false), "Large: 1MB-50MB");
        assert_eq!(SizeBucket::XXLarge.label(false), "XXLarge: 200MB+");
        assert_eq!(SizeBucket::XSmall.label(true), "XSmall: 0-500kB");
    }

    fn index() -> FileIndex {
        FileIndex {
            files: vec![
                file(0, "/Users/a/Pictures/big.jpg", 5_000_000, false),
                file(0, "/Users/a/Pictures/small.png", 20_000, false),
                file(0, "/Windows/Web/wall.jpg", 3_000_000, false),
                file(1, "/DCIM/IMG_1.jpg", 2_000_000, true),
                file(1, "/DCIM/IMG_2.jpg", 9_000_000, false),
                file(0, "/Users/a/doc.pdf", 2_000_000, false),
            ],
            data_sources: vec!["laptop".into(), "usb".into()],
        }
    }

    #[test]
    fn default_criteria_exclude_windows_and_small() {
        let idx = index();
        let c = Criteria::new(ResultType::Images);
        let groups = search(
            &idx,
            &c,
            GroupBy::ParentFolder,
            GroupOrder::GroupName,
            ItemOrder::FileName,
        );
        let labels: Vec<_> = groups
            .iter()
            .map(|g| (g.label.as_str(), g.files.len()))
            .collect();
        assert_eq!(labels, [("/DCIM", 2), ("/Users/a/Pictures", 1)]);
    }

    #[test]
    fn grouping_and_ordering() {
        let idx = index();
        let mut c = Criteria::new(ResultType::Images);
        c.sizes.clear();
        c.folders.clear();
        let by_size = search(
            &idx,
            &c,
            GroupBy::FileSize,
            GroupOrder::GroupName,
            ItemOrder::FileSize,
        );
        assert_eq!(by_size[0].key, GroupKey::Size(SizeBucket::Large));
        assert_eq!(
            idx.files[by_size[0].files[0]].name(),
            "IMG_2.jpg",
            "largest first"
        );
        let by_count = search(
            &idx,
            &c,
            GroupBy::DataSource,
            GroupOrder::GroupSize,
            ItemOrder::FileName,
        );
        assert_eq!(by_count[0].label, "laptop (ID: 0)");
        assert_eq!(by_count[0].files.len(), 3);
        c.include_deleted = false;
        let all = search(
            &idx,
            &c,
            GroupBy::None,
            GroupOrder::GroupName,
            ItemOrder::FileName,
        );
        assert_eq!(all[0].files.len(), 4);
    }

    #[test]
    fn folder_terms() {
        let idx = index();
        let mut c = Criteria::new(ResultType::Images);
        c.sizes.clear();
        c.folders = vec![FolderTerm {
            text: "/DCIM".into(),
            substring: false,
            include: true,
        }];
        let g = search(
            &idx,
            &c,
            GroupBy::None,
            GroupOrder::GroupName,
            ItemOrder::FileName,
        );
        assert_eq!(g[0].files.len(), 2);
        c.folders = vec![FolderTerm {
            text: "pictures".into(),
            substring: true,
            include: true,
        }];
        let g = search(
            &idx,
            &c,
            GroupBy::None,
            GroupOrder::GroupName,
            ItemOrder::FileName,
        );
        assert_eq!(g[0].files.len(), 2);
    }

    #[test]
    fn documents_type() {
        let idx = index();
        let mut c = Criteria::new(ResultType::Documents);
        c.sizes.clear();
        let g = search(
            &idx,
            &c,
            GroupBy::None,
            GroupOrder::GroupName,
            ItemOrder::FileName,
        );
        assert_eq!(g[0].files.len(), 1);
    }
}
