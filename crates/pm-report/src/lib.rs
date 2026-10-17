//! Reports (Autopsy's Generate Report): the HTML report, the Excel report
//! and the "Files - Text" file list, each written into its own folder
//! under the case's `Reports` folder and recorded in the case database so
//! the Reports node lists it.
//!
//! The table reports share [`table::TableReportModule`], fed by a port of
//! Autopsy's `TableReportGenerator` (same tables, order and columns).

pub mod bodyfile;
mod columns;
pub mod excel;
pub mod files_text;
pub mod html;
pub mod table;
pub mod tagged_hashes;

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
    BodyFile,
    TaggedHashes,
    Kml,
}

impl ReportKind {
    pub const ALL: [Self; 6] = [
        Self::Html,
        Self::Excel,
        Self::FilesText,
        Self::BodyFile,
        Self::TaggedHashes,
        Self::Kml,
    ];

    /// Autopsy's module name (shown in the wizard and the Reports node).
    pub fn name(self) -> &'static str {
        match self {
            Self::Html => "HTML Report",
            Self::Excel => "Excel Report",
            Self::FilesText => "Files - Text",
            Self::BodyFile => "Body File",
            Self::TaggedHashes => "Save Tagged Hashes",
            Self::Kml => "Google Earth KML",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Html => "A report about results and tagged items in HTML format.",
            Self::Excel => "A report about results and tagged items in Excel (XLS) format.",
            Self::FilesText => {
                "A tab delimited text file containing information about individual files in the case."
            }
            Self::BodyFile => {
                "Body file format report with MAC times for every file. This format can be used for a timeline view."
            }
            Self::TaggedHashes => "Adds hashes of tagged files to a hash set.",
            Self::Kml => {
                "KML format report with coordinates for relevant files. This format can be used for google earth views."
            }
        }
    }

    /// Whether the report uses the result/tag selection.
    pub fn is_table_report(self) -> bool {
        matches!(self, Self::Html | Self::Excel)
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
/// Settings of the reports that are not table reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtherSettings {
    pub files: files_text::FileReportSettings,
    pub tagged_hashes: tagged_hashes::TaggedHashesSettings,
}

pub fn generate(
    case: &Case,
    kind: ReportKind,
    settings: &ReportSettings,
    other: &OtherSettings,
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
        ReportKind::FilesText => Generated {
            path: files_text::write(case, &dir, settings, &other.files, progress)?,
            summary: TableSummary::default(),
        },
        ReportKind::BodyFile => Generated {
            path: bodyfile::write(case, &dir, settings, progress)?,
            summary: TableSummary::default(),
        },
        ReportKind::Kml => {
            let geo = pm_geo::load(
                db,
                &pm_geo::GeoFilter {
                    data_sources: settings.data_sources.clone(),
                    ..pm_geo::GeoFilter::default()
                },
            )?;
            let path = dir.join("ReportKML.kml");
            let kml = pm_geo::kml::write(&geo, &format!("{} KML Report", case.details().name));
            std::fs::write(&path, kml).map_err(io_err(&path))?;
            let style = dir.join("style.kml");
            std::fs::write(&style, pm_geo::kml::STYLE_KML).map_err(io_err(&style))?;
            Generated {
                path,
                summary: TableSummary {
                    rows: geo.all_points().count(),
                    ..TableSummary::default()
                },
            }
        }
        ReportKind::TaggedHashes => {
            let (path, added) =
                tagged_hashes::write(case, settings, &other.tagged_hashes, progress)?;
            Generated {
                path,
                summary: TableSummary {
                    rows: usize::try_from(added).unwrap_or(usize::MAX),
                    ..TableSummary::default()
                },
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
