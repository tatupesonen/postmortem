//! Which files a module's per-file work is called for. The host applies
//! the filter, so a plugin is not even entered for other files.

use serde::{Deserialize, Serialize};

use crate::{FileInfo, FileKind};

/// File selection for [`crate::FileIngest`]. Empty lists match anything;
/// the lists that are set must all match (MIME type *and* extension *and*
/// name).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFilter {
    /// MIME types; `"image/*"` matches a whole family.
    pub mime_types: Vec<String>,
    /// Lower-case extensions without the dot.
    pub extensions: Vec<String>,
    /// Whole file names, case-insensitive (`"places.sqlite"`).
    pub names: Vec<String>,
    /// Also unallocated blocks and slack (off: Autopsy modules skip them).
    pub include_unallocated: bool,
    /// Also directories.
    pub include_directories: bool,
    /// Skip files larger than this many bytes.
    pub max_size: Option<u64>,
}

impl FileFilter {
    /// Every regular file.
    pub fn all_files() -> Self {
        Self::default()
    }

    pub fn matches(&self, f: &FileInfo) -> bool {
        let kind_ok = match f.kind {
            FileKind::Directory => self.include_directories,
            FileKind::Unallocated => self.include_unallocated,
            FileKind::File | FileKind::Carved | FileKind::Derived => true,
            FileKind::Other => false,
        };
        kind_ok
            && self.max_size.is_none_or(|m| f.size <= m)
            && (self.mime_types.is_empty()
                || f.mime
                    .as_deref()
                    .is_some_and(|m| self.mime_types.iter().any(|p| mime_matches(p, m))))
            && (self.extensions.is_empty()
                || self
                    .extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&f.extension)))
            && (self.names.is_empty() || self.names.iter().any(|n| n.eq_ignore_ascii_case(&f.name)))
    }
}

fn mime_matches(pattern: &str, mime: &str) -> bool {
    match pattern.strip_suffix("/*") {
        Some(family) => mime
            .split_once('/')
            .is_some_and(|(f, _)| f.eq_ignore_ascii_case(family)),
        None => pattern.eq_ignore_ascii_case(mime),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, ext: &str, mime: Option<&str>, kind: FileKind) -> FileInfo {
        FileInfo {
            id: 1,
            ds_id: 1,
            name: name.into(),
            parent_path: "/".into(),
            extension: ext.into(),
            size: 10,
            kind,
            deleted: false,
            mime: mime.map(Into::into),
            md5: None,
            modified: None,
            accessed: None,
            created: None,
            changed: None,
        }
    }

    #[test]
    fn criteria_combine_and_kinds_are_opt_in() {
        let jpg = file("a.JPG", "jpg", Some("image/jpeg"), FileKind::File);
        let images = FileFilter {
            mime_types: vec!["image/*".into()],
            ..FileFilter::default()
        };
        assert!(images.matches(&jpg));
        assert!(!images.matches(&file("a.txt", "txt", Some("text/plain"), FileKind::File)));
        assert!(
            !images.matches(&file("x", "", None, FileKind::File)),
            "no MIME yet"
        );
        let named = FileFilter {
            names: vec!["places.sqlite".into()],
            extensions: vec!["sqlite".into()],
            ..FileFilter::default()
        };
        assert!(named.matches(&file("Places.SQLite", "sqlite", None, FileKind::Derived)));
        assert!(!FileFilter::all_files().matches(&file("d", "", None, FileKind::Directory)));
        assert!(!FileFilter::all_files().matches(&file("u", "", None, FileKind::Unallocated)));
        let small = FileFilter {
            max_size: Some(5),
            ..FileFilter::default()
        };
        assert!(!small.matches(&jpg));
    }
}
