//! Autopsy's "Body File" report (`BodyFileReport`): one line per file
//! system file in Autopsy's body format, for `mactime`:
//! `MD5|path|inode|mode|UID|GID|size|atime|mtime|ctime|crtime`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use pm_case::Case;
use pm_casedb::{FileKindCode, FileQuery, FileRecord, Origin};

use crate::{Progress, ReportError, ReportSettings, io_err};

const FILE_NAME: &str = "BodyFile.txt";

/// Autopsy's `getModesAsString`: file type, `/`, metadata type, permissions
/// (`r/rrwxr-xr-x`).
fn modes(kind: FileKindCode, mode: Option<u32>) -> String {
    let t = match kind {
        FileKindCode::Regular => 'r',
        FileKindCode::Directory => 'd',
        _ => '-',
    };
    let m = mode.unwrap_or(0);
    let bit = |b: u32, c: char| if m & b != 0 { c } else { '-' };
    let perms: String = [
        bit(0o400, 'r'),
        bit(0o200, 'w'),
        bit(0o100, 'x'),
        bit(0o040, 'r'),
        bit(0o020, 'w'),
        bit(0o010, 'x'),
        bit(0o004, 'r'),
        bit(0o002, 'w'),
        bit(0o001, 'x'),
    ]
    .iter()
    .collect();
    format!("{t}/{t}{perms}")
}

fn line(f: &FileRecord, ds: &str) -> String {
    let t = |i: usize| f.times.get(i).copied().flatten().map_or(0, |(s, _)| s);
    format!(
        "{}|/{ds}{}|{}|{}|{}|{}|{}|{}|{}|{}|{}\n",
        f.md5
            .as_deref()
            .map(pm_core::decode::to_hex)
            .unwrap_or_default(),
        f.path(),
        f.inode.unwrap_or(0),
        modes(f.kind, f.mode),
        f.uid.unwrap_or(0),
        f.gid.unwrap_or(0),
        f.size,
        t(1),
        t(0),
        t(2),
        t(3),
    )
}

/// Writes `BodyFile.txt` in `dir`; returns its path.
pub fn write(
    case: &Case,
    dir: &Path,
    settings: &ReportSettings,
    progress: &dyn Progress,
) -> Result<PathBuf, ReportError> {
    let path = dir.join(FILE_NAME);
    let f = std::fs::File::create(&path).map_err(io_err(&path))?;
    let mut out = std::io::BufWriter::new(f);
    let db = case.db();
    for ds in db.data_sources()? {
        if !settings.data_sources.is_empty() && !settings.data_sources.contains(&ds.id) {
            continue;
        }
        progress.status(&format!("Now processing {}...", ds.name));
        let q = FileQuery {
            ds_id: Some(ds.id),
            fs_only: true,
            ..FileQuery::default()
        };
        let mut after = 0;
        loop {
            if progress.is_cancelled() {
                return Err(ReportError::Cancelled);
            }
            let page = db.query_files(&q, after, 5_000)?;
            let Some(last) = page.last() else { break };
            after = last.id;
            for f in page
                .iter()
                .filter(|f| f.origin == Origin::FileSystem && f.name != "." && f.name != "..")
            {
                out.write_all(line(f, &ds.name).as_bytes())
                    .map_err(io_err(&path))?;
            }
        }
    }
    out.flush().map_err(io_err(&path))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_follow_autopsy() {
        assert_eq!(modes(FileKindCode::Regular, Some(0o755)), "r/rrwxr-xr-x");
        assert_eq!(modes(FileKindCode::Directory, None), "d/d---------");
    }
}
