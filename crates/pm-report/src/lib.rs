//! Reports (Autopsy's Generate Report): the HTML report, the Excel report
//! and the "Files - Text" file list, each written into its own folder
//! under the case's `Reports` folder and recorded in the case database so
//! the Reports node lists it.
//!
//! The table reports share [`table::TableReportModule`], fed by a port of
//! Autopsy's `TableReportGenerator` (same tables, order and columns).

mod columns;
pub mod excel;
pub mod files_text;
pub mod html;
pub mod table;

use std::path::{Path, PathBuf};

use pm_case::Case;

pub use table::{TableReportModule, TableSummary};

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error(transparent)]
    Db(#[from] pm_casedb::DbError),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write the spreadsheet: {0}")]
    Excel(String),
    #[error("report generation was cancelled")]
    Cancelled,
}

pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ReportError + '_ {
    move |source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Progress and cancellation (the report progress panel).
pub trait Progress: Sync {
    fn status(&self, _message: &str) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// No progress reporting, never cancelled.
#[derive(Debug, Default)]
pub struct NoProgress;

impl Progress for NoProgress {}

/// What a table report contains (Autopsy's report wizard choices).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSettings {
    /// Data sources to report on (empty: all).
    pub data_sources: Vec<i64>,
    /// Artifact type names (`None`: every type with results).
    pub artifact_types: Option<Vec<String>>,
    /// Only results tagged with one of these (empty: no tag filter); also
    /// limits the Tagged Files / Tagged Results tables.
    pub tag_names: Vec<String>,
    pub tagged_files: bool,
    pub tagged_results: bool,
    /// Time zone of the times in the report.
    pub timezone: jiff::tz::TimeZone,
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            data_sources: Vec::new(),
            artifact_types: None,
            tag_names: Vec::new(),
            tagged_files: true,
            tagged_results: true,
            timezone: jiff::tz::TimeZone::UTC,
        }
    }
}

impl ReportSettings {
    /// Autopsy's `dateToString`: `yyyy/MM/dd HH:mm:ss` in the report's
    /// time zone (empty for 0).
    pub fn format_time(&self, unix_seconds: i64) -> String {
        if unix_seconds == 0 {
            return String::new();
        }
        jiff::Timestamp::from_second(unix_seconds)
            .map(|t| {
                t.to_zoned(self.timezone.clone())
                    .strftime("%Y/%m/%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    }
}

/// The report formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportKind {
    Html,
    Excel,
    FilesText,
}

impl ReportKind {
    pub const ALL: [Self; 3] = [Self::Html, Self::Excel, Self::FilesText];

    /// Autopsy's module name (shown in the wizard and the Reports node).
    pub fn name(self) -> &'static str {
        match self {
            Self::Html => "HTML Report",
            Self::Excel => "Excel Report",
            Self::FilesText => "Files - Text",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Html => "A report about results and tagged items in HTML format.",
            Self::Excel => "A report about results and tagged items in Excel (XLS) format.",
            Self::FilesText => {
                "A tab delimited text file containing information about individual files in the case."
            }
        }
    }

    /// Whether the report uses the result/tag selection.
    pub fn is_table_report(self) -> bool {
        !matches!(self, Self::FilesText)
    }
}

/// A finished report.
#[derive(Debug)]
pub struct Generated {
    /// The file to open (e.g. `report.html`).
    pub path: PathBuf,
    pub summary: TableSummary,
}

/// Writes a report into a new folder below `<case>/Reports` (Autopsy's
/// `<case name> <date time>/<module>/`) and records it in the case.
pub fn generate(
    case: &Case,
    kind: ReportKind,
    settings: &ReportSettings,
    files: &files_text::FileReportSettings,
    progress: &dyn Progress,
) -> Result<Generated, ReportError> {
    let stamp = jiff::Zoned::now().strftime("%m-%d-%Y-%H-%M-%S");
    let dir = case
        .dir()
        .join("Reports")
        .join(format!("{} {stamp}", sanitize(&case.details().name)))
        .join(kind.name());
    std::fs::create_dir_all(&dir).map_err(io_err(&dir))?;
    let db = case.db();
    let generated = match kind {
        ReportKind::Html => {
            let mut m = html::HtmlReport::new(&dir, case, settings);
            let summary = table::Generator::new(db, settings, progress)?.run(&mut m)?;
            Generated {
                path: m.index_path(),
                summary,
            }
        }
        ReportKind::Excel => {
            let mut m = excel::ExcelReport::new(&dir, case);
            let summary = table::Generator::new(db, settings, progress)?.run(&mut m)?;
            Generated {
                path: m.path(),
                summary,
            }
        }
        ReportKind::FilesText => {
            let path = files_text::write(case, &dir, settings, files, progress)?;
            Generated {
                path,
                summary: TableSummary::default(),
            }
        }
    };
    db.add_report(
        &generated.path.display().to_string(),
        kind.name(),
        kind.name(),
        jiff::Timestamp::now().as_second(),
    )?;
    Ok(generated)
}

/// A file name part without path separators or reserved characters.
pub(crate) fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let out = out.trim().to_owned();
    if out.is_empty() { "Case".into() } else { out }
}
