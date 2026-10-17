//! Locating and opening parent (backing) images of differencing disks.

use std::path::{Path, PathBuf};

use crate::{Image, ImageError};

/// Parent chains longer than this are refused (also catches cycles).
pub const MAX_PARENT_DEPTH: usize = 16;

/// Turns a stored parent reference into a local path candidate: Windows
/// separators become `/`, `file://` URLs lose their scheme, and a drive
/// letter (`C:`) is kept as-is.
fn normalise(reference: &str) -> String {
    let r = reference.trim().trim_end_matches('\0');
    let r = r.strip_prefix("file://localhost").unwrap_or(r);
    let r = r.strip_prefix("file://").unwrap_or(r);
    let r = r.strip_prefix(r"\\?\").unwrap_or(r);
    r.replace('\\', "/")
}

/// Resolves the first existing parent among `references` (in priority
/// order), relative to the directory of `child`.
///
/// For each reference tries, in order: the path relative to the child's
/// directory, the path as given (absolute), and the bare file name next to
/// the child (images moved together to another machine).
pub fn resolve_with(
    child: &Path,
    references: &[String],
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let dir = child.parent().unwrap_or_else(|| Path::new(""));
    for r in references {
        let n = normalise(r);
        if n.is_empty() {
            continue;
        }
        let p = Path::new(&n);
        let mut candidates = Vec::new();
        if p.is_relative() {
            candidates.push(dir.join(p));
        } else {
            candidates.push(p.to_path_buf());
        }
        if let Some(name) = n.rsplit('/').next().filter(|s| !s.is_empty()) {
            candidates.push(dir.join(name));
        }
        if let Some(found) = candidates.into_iter().find(|c| c != child && exists(c)) {
            return Some(found);
        }
    }
    None
}

/// [`resolve_with`] on the local file system.
pub fn resolve(child: &Path, references: &[String]) -> Option<PathBuf> {
    resolve_with(child, references, Path::is_file)
}

/// Resolves and opens the parent of the differencing image `child`, which
/// itself sits `depth` levels below the image the user opened.
pub(crate) fn open(child: &Path, references: &[String], depth: usize) -> Result<Image, ImageError> {
    let Some(found) = resolve(child, references) else {
        return Err(ImageError::MissingParent {
            child: child.to_path_buf(),
            parent: references
                .first()
                .cloned()
                .unwrap_or_else(|| "(no parent reference)".into()),
        });
    };
    tracing::debug!(child = %child.display(), parent = %found.display(), "opening parent image");
    crate::open_depth(&found, depth + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fs(names: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = names.iter().map(PathBuf::from).collect();
        move |p| set.contains(p)
    }

    #[test]
    fn relative_windows_reference() {
        let e = fs(&["/ev/base/p.vhd"]);
        let got = resolve_with(Path::new("/ev/c.vhd"), &[r".\base\p.vhd".into()], e);
        assert_eq!(got, Some(PathBuf::from("/ev/base/p.vhd")));
    }

    #[test]
    fn absolute_windows_path_falls_back_to_file_name() {
        let e = fs(&["/ev/parent.vhd"]);
        assert_eq!(
            resolve_with(
                Path::new("/ev/child.vhd"),
                &[r"C:\VMs\disks\parent.vhd".into()],
                e
            ),
            Some(PathBuf::from("/ev/parent.vhd"))
        );
    }

    #[test]
    fn file_url_and_priority() {
        let e = fs(&["/srv/a.vmdk", "/ev/b.vmdk"]);
        assert_eq!(
            resolve_with(
                Path::new("/ev/c.vmdk"),
                &["file:///srv/a.vmdk".into(), "b.vmdk".into()],
                e
            ),
            Some(PathBuf::from("/srv/a.vmdk"))
        );
    }

    #[test]
    fn never_resolves_to_the_child_itself() {
        let e = fs(&["/ev/c.vhd"]);
        assert_eq!(
            resolve_with(Path::new("/ev/c.vhd"), &["c.vhd".into()], e),
            None
        );
        assert_eq!(
            resolve_with(Path::new("/ev/c.vhd"), &[String::new()], |_| true),
            None
        );
    }
}
