//! Logical evidence files (EnCase L01) in a "Logical Files" data source.
//! Autopsy runs `ewfexport` to extract an L01 into
//! `ModuleOutput/L01/<name>` and adds the extracted folder as local files;
//! here the file-entry tree (`ltree` section) is read natively, every file
//! is extracted from the set's media data into the same folder, and the
//! tree is added with the names and times recorded in the L01.
//!
//! The tree follows libewf's reading: a `5` line, then five categories
//! (`rec`, `perm`, `srce`, `sub`, `entry`) each ended by an empty line. In
//! `entry`, a count line and the tab-separated type line precede the
//! entries; every entry is a `<0|26>\t<children>` line and a value line,
//! children following depth first. The root entry has no name. A file's
//! data is its binary extents (`be`: hexadecimal count, then `[S] offset
//! size` in hexadecimal, `S` sparse) or, for duplicates, `du`.

use std::io::Write;
use std::path::{Path, PathBuf};

use pm_casedb::{FileKindCode, NewFile, Origin};
use pm_core::ReadAt;

use crate::load::{LoadProgress, LoadSummary};
use crate::{Case, CaseError, Result, io_err};

/// Deepest folder nesting accepted (libewf's limit is similar).
const MAX_DEPTH: usize = 256;
/// Read size when extracting.
const COPY_BUF: usize = 1 << 20;

/// A file or folder of an L01.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) dir: bool,
    pub(crate) size: u64,
    /// `(media offset, length, sparse)`.
    pub(crate) extents: Vec<(u64, u64, bool)>,
    /// Offset of the data this duplicate shares.
    pub(crate) duplicate: Option<u64>,
    /// Creation, access, written (modified), entry modified.
    pub(crate) times: [Option<i64>; 4],
    pub(crate) md5: Option<String>,
    pub(crate) children: Vec<Entry>,
}

/// Parses the `ltree` text into the root entry.
pub(crate) fn parse_tree(text: &str) -> Result<Entry> {
    let bad = |m: &str| CaseError::Invalid(format!("L01 file-entry tree: {m}"));
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    if lines.next().map(str::trim) != Some("5") {
        return Err(bad("unsupported number of categories"));
    }
    while let Some(category) = lines.next() {
        match category.trim() {
            "entry" => {
                lines.next().ok_or_else(|| bad("missing entry count"))?;
                let types: Vec<String> = lines
                    .next()
                    .ok_or_else(|| bad("missing entry types"))?
                    .split('\t')
                    .map(|t| t.trim().to_owned())
                    .collect();
                return parse_entry(&mut lines, &types, 0).map_err(|e| bad(&e));
            }
            "" => {}
            _ => {
                // Other categories end at an empty line.
                for l in lines.by_ref() {
                    if l.is_empty() {
                        break;
                    }
                }
            }
        }
    }
    Err(bad("no entry category"))
}

fn parse_entry<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    types: &[String],
    depth: usize,
) -> std::result::Result<Entry, String> {
    if depth > MAX_DEPTH {
        return Err("folders nested too deep".into());
    }
    let count = lines.next().ok_or("missing entry count line")?;
    let children = count
        .split('\t')
        .nth(1)
        .and_then(|n| n.trim().parse::<usize>().ok())
        .ok_or_else(|| format!("bad entry count line {count:?}"))?;
    let values = lines.next().ok_or("missing entry values")?;
    let mut e = Entry::default();
    for (t, v) in types.iter().zip(values.split('\t')) {
        let int = || v.trim().parse::<i64>().ok();
        match t.as_str() {
            "n" => e.name = v.replace('\u{2}', "\r"),
            "p" => e.dir = v.trim() == "1",
            "ls" => e.size = v.trim().parse().unwrap_or(0),
            "be" => e.extents = parse_extents(v),
            "du" => e.duplicate = int().and_then(|d| u64::try_from(d).ok()),
            "cr" => e.times[0] = int().filter(|&t| t > 0),
            "ac" => e.times[1] = int().filter(|&t| t > 0),
            "wr" => e.times[2] = int().filter(|&t| t > 0),
            "mo" => e.times[3] = int().filter(|&t| t > 0),
            "ha" => {
                let h = v.trim().to_ascii_lowercase();
                if h.len() == 32 && h.bytes().any(|b| b != b'0') {
                    e.md5 = Some(h);
                }
            }
            _ => {}
        }
    }
    // A child count beyond the remaining text fails at the missing lines.
    for _ in 0..children {
        e.children.push(parse_entry(lines, types, depth + 1)?);
    }
    Ok(e)
}

/// `be`: `<count hex> ([S] <offset hex> <size hex>)*`.
fn parse_extents(v: &str) -> Vec<(u64, u64, bool)> {
    let mut it = v.split_whitespace().skip(1).peekable();
    let mut out = Vec::new();
    loop {
        let sparse = it.next_if_eq(&"S").is_some();
        let (Some(off), Some(len)) = (it.next(), it.next()) else {
            break;
        };
        let (Ok(off), Ok(len)) = (u64::from_str_radix(off, 16), u64::from_str_radix(len, 16))
        else {
            break;
        };
        out.push((off, len, sparse));
    }
    out
}

/// Adds the L01 set starting at `first` under `parent` (a folder named
/// after it) of logical file set `ds_id`.
pub(crate) fn add_l01(
    case: &Case,
    ds_id: i64,
    first: &Path,
    progress: &LoadProgress,
    summary: &mut LoadSummary,
) -> Result<u64> {
    let segments = pm_image::ewf::discover(first);
    let text = pm_image::ewf::logical_tree(&segments)
        .map_err(pm_evidence::EvidenceError::from)?
        .ok_or_else(|| CaseError::Invalid(format!("{} has no file-entry tree", first.display())))?;
    let root = parse_tree(&text)?;
    let media = pm_image::open_segments(&segments)
        .map_err(pm_evidence::EvidenceError::from)?
        .source();
    let stem = first
        .file_stem()
        .map_or_else(|| "L01".to_owned(), |s| s.to_string_lossy().into_owned());
    let base = case.module_output_dir().join("L01");
    let mut dest = base.join(&stem);
    let mut n = 1;
    while dest.exists() {
        dest = base.join(format!("{stem}_{n}"));
        n += 1;
    }
    std::fs::create_dir_all(&dest).map_err(io_err(&dest))?;
    let top_name = first
        .file_name()
        .map_or_else(|| stem.clone(), |s| s.to_string_lossy().into_owned());
    let db = case.db();
    let mut top = NewFile::new(
        ds_id,
        None,
        "/",
        &top_name,
        Origin::Local,
        FileKindCode::Directory,
    );
    top.local_path = Some(dest.display().to_string());
    let top_id = db.bulk(|b| b.add_file(&top))?;
    summary.files += 1;
    let mut total = 0u64;
    let mut next_id = 0u64;
    // (entry, parent row, parent path, extraction folder)
    let mut stack: Vec<(&Entry, i64, String, PathBuf)> = root
        .children
        .iter()
        .rev()
        .map(|c| (c, top_id, format!("/{top_name}/"), dest.clone()))
        .collect();
    let mut buf = vec![0u8; COPY_BUF];
    while let Some((e, parent, parent_path, folder)) = stack.pop() {
        if progress.is_cancelled() {
            return Ok(total);
        }
        next_id += 1;
        let name = if e.name.is_empty() {
            "NoName".to_owned()
        } else {
            e.name.clone()
        };
        // Names may hold path separators; on disk they are replaced.
        let disk = folder.join(format!(
            "{next_id}_{}",
            name.replace(['/', '\\', ':', '\0'], "_")
        ));
        let mut f = NewFile::new(
            ds_id,
            Some(parent),
            &parent_path,
            &name,
            Origin::Local,
            if e.dir {
                FileKindCode::Directory
            } else {
                FileKindCode::Regular
            },
        );
        let [cr, ac, wr, mo] = e.times;
        let t = |s: Option<i64>| s.map(|s| (s, 0));
        f.times = [t(wr), t(ac), t(mo), t(cr)];
        f.local_path = Some(disk.display().to_string());
        if e.dir {
            std::fs::create_dir_all(&disk).map_err(io_err(&disk))?;
            let id = db.bulk(|b| b.add_file(&f))?;
            let child_path = format!("{parent_path}{name}/");
            for c in e.children.iter().rev() {
                stack.push((c, id, child_path.clone(), disk.clone()));
            }
        } else {
            match extract(&*media, e, &disk, &mut buf) {
                Ok(n) => {
                    f.size = n;
                    total = total.saturating_add(n);
                }
                Err(err) => summary.warnings.push(format!("{parent_path}{name}: {err}")),
            }
            db.bulk(|b| b.add_file(&f))?;
        }
        summary.files += 1;
        progress
            .files
            .store(summary.files, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(total)
}

/// Writes an entry's data to `to`; returns its size.
fn extract(media: &dyn ReadAt, e: &Entry, to: &Path, buf: &mut [u8]) -> std::io::Result<u64> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(to)?);
    let runs: Vec<(u64, u64, bool)> = if e.extents.is_empty() {
        e.duplicate
            .map(|d| vec![(d, e.size, false)])
            .unwrap_or_default()
    } else {
        e.extents.clone()
    };
    let mut written = 0u64;
    for (off, len, sparse) in runs {
        let mut done = 0u64;
        while done < len {
            let n = usize::try_from(len - done)
                .unwrap_or(usize::MAX)
                .min(buf.len());
            let chunk = buf.get_mut(..n).unwrap_or_default();
            if sparse {
                chunk.fill(0);
            } else {
                media.read_exact_at(off + done, chunk)?;
            }
            out.write_all(chunk)?;
            done += n as u64;
        }
        written += len;
    }
    out.flush()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = "5\nrec\ntb\tcl\n4096\t1\n\nperm\n0\t1\np\tn\ts\tpr\tnta\tnti\n0\t0\n1\t\t\t10\t0\t0\n\n\
        entry\n3\t1\nmid\tls\tbe\tid\tcr\tp\tn\n\
        0\t2\n\t0\t\t0\t\t1\t\n\
        0\t1\n\t0\t\t1\t1600000000\t1\tdocs\n\
        0\t0\n\t17\t1 0 11\t2\t1600000001\t\ta.txt\n\
        0\t0\n\t512\t2 200 100 S 300 100\t3\t\t\tb.bin\n\n";

    #[test]
    fn tree_parses() {
        let root = parse_tree(TREE).unwrap();
        assert_eq!(root.children.len(), 2);
        let docs = root.children.first().unwrap();
        assert!(docs.dir);
        assert_eq!(docs.times[0], Some(1_600_000_000));
        let a = docs.children.first().unwrap();
        assert_eq!((a.name.as_str(), a.size), ("a.txt", 17));
        assert_eq!(a.extents, [(0, 0x11, false)]);
        let b = root.children.get(1).unwrap();
        assert_eq!(b.extents, [(0x200, 0x100, false), (0x300, 0x100, true)]);
        assert!(parse_tree("4\n").is_err());
    }
}
