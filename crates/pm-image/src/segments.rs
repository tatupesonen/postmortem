//! Discovery of the remaining segments of a split image from its first file.
//!
//! Supported naming schemes (as accepted by TSK/Autopsy):
//!
//! * numeric extensions: `name.000`/`name.001`, `.002`, … of any width
//! * alphabetic extensions: `name.aa`, `.ab`, … `.az`, `.ba`, …
//! * `name.dmg` followed by `name.002.dmgpart` is *not* handled here (DMG).

use std::path::{Path, PathBuf};

/// Hard cap on segments followed; real images rarely exceed a few thousand.
const MAX_SEGMENTS: usize = 100_000;

/// The naming scheme recognised from a first segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Scheme {
    Numeric { width: usize, first: u64 },
    Alpha { width: usize },
}

fn scheme_for(ext: &str) -> Option<Scheme> {
    if !ext.is_empty() && ext.bytes().all(|b| b.is_ascii_digit()) {
        let first = ext.parse().ok()?;
        // A single image named "x.5" is not a split set; only 0/1 starts count.
        return (first <= 1).then_some(Scheme::Numeric {
            width: ext.len(),
            first,
        });
    }
    if ext.len() >= 2
        && ext.bytes().all(|b| b.is_ascii_lowercase())
        && ext.bytes().all(|b| b == b'a')
    {
        return Some(Scheme::Alpha { width: ext.len() });
    }
    None
}

fn alpha_ext(mut n: u64, width: usize) -> Option<String> {
    let mut out = vec![b'a'; width];
    for slot in out.iter_mut().rev() {
        *slot = b'a' + (n % 26) as u8;
        n /= 26;
    }
    (n == 0).then(|| String::from_utf8(out).unwrap_or_default())
}

fn numeric_ext(n: u64, width: usize) -> String {
    format!("{n:0width$}")
}

/// Returns `first` followed by any consecutive sibling segments that exist.
///
/// If `first` does not use a split naming scheme, returns just `[first]`.
pub fn discover(first: &Path) -> Vec<PathBuf> {
    discover_with(first, Path::is_file)
}

/// [`discover`] with an injectable existence check (for tests and virtual
/// filesystems).
pub fn discover_with(first: &Path, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Some(ext) = first.extension().and_then(|e| e.to_str()) else {
        return vec![first.to_path_buf()];
    };
    let lower = ext.to_ascii_lowercase();
    let Some(scheme) = scheme_for(&lower) else {
        return vec![first.to_path_buf()];
    };
    let upper = ext.chars().any(|c| c.is_ascii_uppercase());
    let mut out = vec![first.to_path_buf()];
    for i in 1..MAX_SEGMENTS as u64 {
        let next = match &scheme {
            Scheme::Numeric { width, first: f } => numeric_ext(f + i, *width),
            Scheme::Alpha { width } => match alpha_ext(i, *width) {
                Some(e) => e,
                None => break,
            },
        };
        let next = if upper {
            next.to_ascii_uppercase()
        } else {
            next
        };
        let candidate = first.with_extension(next);
        if !exists(&candidate) {
            break;
        }
        out.push(candidate);
    }
    out
}

/// If `path` is `name.001` and `name.000` exists, returns the `.000` path:
/// Autopsy asks the user whether the set really starts at `.000`.
pub fn zero_segment_candidate(path: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let ext = path.extension()?.to_str()?;
    if ext.len() >= 3 && ext.bytes().all(|b| b.is_ascii_digit()) && ext.parse::<u64>().ok()? == 1 {
        let zero = path.with_extension("0".repeat(ext.len()));
        return exists(&zero).then_some(zero);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fs(names: &[&str]) -> impl Fn(&Path) -> bool {
        let set: HashSet<PathBuf> = names.iter().map(PathBuf::from).collect();
        move |p| set.contains(p)
    }

    fn names(v: &[PathBuf]) -> Vec<String> {
        v.iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn numeric_sequence() {
        let e = fs(&["d/img.001", "d/img.002", "d/img.003", "d/img.005"]);
        assert_eq!(
            names(&discover_with(Path::new("d/img.001"), e)),
            ["d/img.001", "d/img.002", "d/img.003"]
        );
    }

    #[test]
    fn numeric_starting_at_zero_and_wide() {
        let e = fs(&["img.0000", "img.0001"]);
        assert_eq!(
            names(&discover_with(Path::new("img.0000"), e)),
            ["img.0000", "img.0001"]
        );
    }

    #[test]
    fn alpha_sequence_rolls_over() {
        let mut v: Vec<String> = Vec::new();
        for i in 0..30 {
            v.push(format!("x.{}", alpha_ext(i, 2).unwrap()));
        }
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        let got = discover_with(Path::new("x.aa"), fs(&refs));
        assert_eq!(got.len(), 30);
        assert_eq!(got[26], PathBuf::from("x.ba"));
    }

    #[test]
    fn uppercase_extensions_preserved() {
        let e = fs(&["IMG.001", "IMG.002"]);
        assert_eq!(
            names(&discover_with(Path::new("IMG.001"), e)),
            ["IMG.001", "IMG.002"]
        );
    }

    #[test]
    fn non_split_names_are_single() {
        for n in ["disk.dd", "disk.img", "disk", "disk.ab", "disk.7"] {
            assert_eq!(discover_with(Path::new(n), |_| true).len(), 1, "{n}");
        }
    }

    #[test]
    fn alpha_ext_overflow() {
        assert_eq!(alpha_ext(26 * 26, 2), None);
        assert_eq!(alpha_ext(27, 2).as_deref(), Some("bb"));
    }

    #[test]
    fn zero_candidate() {
        let e = fs(&["a.000"]);
        assert_eq!(
            zero_segment_candidate(Path::new("a.001"), &e),
            Some(PathBuf::from("a.000"))
        );
        assert_eq!(zero_segment_candidate(Path::new("a.002"), &e), None);
        assert_eq!(zero_segment_candidate(Path::new("b.001"), &e), None);
    }
}
