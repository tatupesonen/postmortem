//! Autopsy's "Files - Text" report (`FileReportText`): one line per file
//! (not directories) with the chosen columns, quoted and delimited, in a
//! UTF-8 file with a byte-order mark. Files are streamed a page at a time.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use pm_case::Case;
use pm_casedb::{FileKindCode, FileQuery, FileRecord, Known};

use crate::{Progress, ReportError, ReportSettings, io_err};

const FILE_NAME: &str = "file-report.txt";

/// Autopsy's `FileReportDataTypes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileColumn {
    Name,
    FileExt,
    FileType,
    Deleted,
    ATime,
    CrTime,
    MTime,
    Size,
    Address,
    Hash,
    Known,
    Perms,
    FullPath,
}

impl FileColumn {
    pub const ALL: [Self; 13] = [
        Self::Name,
        Self::FileExt,
        Self::FileType,
        Self::Deleted,
        Self::ATime,
        Self::CrTime,
        Self::MTime,
        Self::Size,
        Self::Address,
        Self::Hash,
        Self::Known,
        Self::Perms,
        Self::FullPath,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::FileExt => "File Extension",
            Self::FileType => "File Type",
            Self::Deleted => "Is Deleted",
            Self::ATime => "Last Accessed",
            Self::CrTime => "File Created",
            Self::MTime => "Last Modified",
            Self::Size => "Size",
            Self::Address => "Address",
            Self::Hash => "Hash Value",
            Self::Known => "Known Status",
            Self::Perms => "Permissions",
            Self::FullPath => "Full Path",
        }
    }
}

/// The report's column and delimiter choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReportSettings {
    pub columns: Vec<FileColumn>,
    pub delimiter: String,
}

impl Default for FileReportSettings {
    fn default() -> Self {
        Self {
            columns: FileColumn::ALL.to_vec(),
            delimiter: "\t".into(),
        }
    }
}

fn meta_type(k: FileKindCode) -> &'static str {
    match k {
        FileKindCode::Regular => "r",
        FileKindCode::Directory => "d",
        _ => "-",
    }
}

/// `ls -l`-style permissions from a Unix mode.
fn perms(mode: Option<u32>) -> String {
    let Some(m) = mode else {
        return String::new();
    };
    let bit = |b: u32, c: char| if m & b != 0 { c } else { '-' };
    [
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
    .collect()
}

fn cell(c: FileColumn, f: &FileRecord, ds: &str, s: &ReportSettings) -> String {
    let time = |i: usize| {
        f.times
            .get(i)
            .copied()
            .flatten()
            .map(|(t, _)| s.format_time(t))
            .unwrap_or_default()
    };
    match c {
        FileColumn::Name => f.name.clone(),
        FileColumn::FileExt => {
            if f.extension.is_empty() {
                String::new()
            } else {
                format!(".{}", f.extension)
            }
        }
        FileColumn::FileType => meta_type(f.kind).into(),
        FileColumn::Deleted => if f.deleted() { "yes" } else { "" }.into(),
        FileColumn::ATime => time(1),
        FileColumn::CrTime => time(3),
        FileColumn::MTime => time(0),
        FileColumn::Size => f.size.to_string(),
        FileColumn::Address => f.inode.map_or_else(|| "0".into(), |i| i.to_string()),
        FileColumn::Hash => f
            .md5
            .as_deref()
            .map(pm_core::decode::to_hex)
            .unwrap_or_default(),
        FileColumn::Known => match f.known {
            Known::Known => "known",
            Known::Notable => "notable",
            Known::Unknown => "unknown",
        }
        .into(),
        FileColumn::Perms => perms(f.mode),
        FileColumn::FullPath => format!("/{ds}{}", f.path()),
    }
}

fn line(cells: &[String], delimiter: &str) -> String {
    let mut out = String::new();
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push_str(delimiter);
        }
        out.push('"');
        out.push_str(&c.replace('"', "\"\""));
        out.push('"');
    }
    out.push('\n');
    out
}

/// Writes `file-report.txt` in `dir`; returns its path.
pub fn write(
    case: &Case,
    dir: &Path,
    settings: &ReportSettings,
    files: &FileReportSettings,
    progress: &dyn Progress,
) -> Result<PathBuf, ReportError> {
    let path = dir.join(FILE_NAME);
    let f = std::fs::File::create(&path).map_err(io_err(&path))?;
    let mut out = std::io::BufWriter::new(f);
    out.write_all("\u{feff}".as_bytes())
        .map_err(io_err(&path))?;
    let headers: Vec<String> = files.columns.iter().map(|c| c.title().to_owned()).collect();
    out.write_all(line(&headers, &files.delimiter).as_bytes())
        .map_err(io_err(&path))?;
    let db = case.db();
    for ds in db.data_sources()? {
        if !settings.data_sources.is_empty() && !settings.data_sources.contains(&ds.id) {
            continue;
        }
        progress.status(&format!("Now processing {}...", ds.name));
        let q = FileQuery {
            ds_id: Some(ds.id),
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
                .filter(|f| f.kind != FileKindCode::Directory && f.name != "." && f.name != "..")
            {
                let cells: Vec<String> = files
                    .columns
                    .iter()
                    .map(|&c| cell(c, f, &ds.name, settings))
                    .collect();
                out.write_all(line(&cells, &files.delimiter).as_bytes())
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
    fn lines_quote_and_permissions_render() {
        assert_eq!(
            line(&["a".into(), "b\"c".into()], "\t"),
            "\"a\"\t\"b\"\"c\"\n"
        );
        assert_eq!(perms(Some(0o754)), "rwxr-xr--");
        assert_eq!(perms(None), "");
    }
}
