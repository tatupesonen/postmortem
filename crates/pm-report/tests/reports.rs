//! Reports end to end on the analysed demo disk: HTML, Excel and
//! Files - Text, with tagged files and results.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::io::Read as _;
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_report::{NoProgress, ReportKind, ReportSettings, files_text::FileReportSettings};

/// The demo disk, analysed with every default module plus Keyword Search,
/// with one file and one result tagged.
fn case() -> (tempfile::TempDir, Arc<Case>) {
    let dir = tempfile::tempdir().unwrap();
    let case = Case::create(
        dir.path().join("case"),
        CaseDetails {
            name: "Demo <Case>".into(),
            ..CaseDetails::default()
        },
    )
    .unwrap();
    let s = add_image(
        &case,
        pm_testkit::demo::demo_disk_file(),
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    let case = Arc::new(case);
    let modules = pm_modules::core_modules();
    let settings = pm_ingest::JobSettings {
        modules: modules
            .iter()
            .map(|m| pm_ingest::ModuleSetting {
                id: m.info().id.to_owned(),
                enabled: m.info().enabled_by_default || m.info().id == "keyword_search",
                settings: m.default_settings(),
            })
            .collect(),
        ..pm_ingest::JobSettings::default()
    };
    pm_ingest::run_job(
        &case,
        s.ds_id,
        &settings,
        &modules,
        &Arc::new(pm_ingest::IngestProgress::default()),
        &Arc::new(pm_ingest::Inbox::default()),
    )
    .unwrap();
    let db = case.db();
    let notable = db
        .tag_names()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "Notable Item")
        .unwrap();
    let pw = db
        .query_files(
            &pm_casedb::FileQuery {
                name_contains: Some("passwords".into()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap()
        .remove(0);
    db.tag_file(pw.id, notable.id, "router password", "examiner", 1)
        .unwrap();
    let download_type = pm_casedb::catalog::artifact_type_id("TSK_WEB_DOWNLOAD").unwrap();
    let download = db
        .artifacts_of_type(download_type, None, 0, 10)
        .unwrap()
        .remove(0);
    db.tag_artifact(download.id, notable.id, "installer", "examiner", 2)
        .unwrap();
    (dir, case)
}

fn read(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[test]
fn html_report_has_autopsys_layout_and_tables() {
    let (_d, case) = case();
    let g = pm_report::generate(
        &case,
        ReportKind::Html,
        &ReportSettings::default(),
        &FileReportSettings::default(),
        &NoProgress,
    )
    .unwrap();
    assert!(g.path.ends_with("HTML Report/report.html"));
    let index = read(&g.path);
    assert!(index.contains("<frameset cols=\"350px,*\">"));
    let content = g.path.parent().unwrap().join("content");
    let nav = read(&content.join("nav.html"));
    for page in [
        "Web Downloads",
        "Keyword Hits",
        "Tagged Files",
        "Tagged Results",
    ] {
        assert!(nav.contains(page), "{page} missing from {nav}");
    }
    let summary = read(&content.join("summary.html"));
    assert!(
        summary.contains("Demo &lt;Case&gt; Forensic Report"),
        "escaped case name"
    );
    let downloads = read(&content.join("Web_Downloads.html"));
    for h in [
        "<th>Destination</th>",
        "<th>Source URL</th>",
        "<th>Source File</th>",
        "<th>Tags</th>",
    ] {
        assert!(downloads.contains(h), "{h}");
    }
    assert!(downloads.contains("Notable Item"), "tags column");
    let tagged = read(&content.join("Tagged_Files.html"));
    assert!(tagged.contains("passwords.txt") && tagged.contains("router password"));
    let kw = read(&content.join("Keyword_Hits.html"));
    assert!(kw.contains("Email Addresses"), "keyword list set");
    assert!(kw.contains("<th>Preview</th>"));
    assert_eq!(g.summary.errors, Vec::<String>::new());

    let reports = case.db().reports().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].module, "HTML Report");
}

#[test]
fn tag_filter_limits_results() {
    let (_d, case) = case();
    let settings = ReportSettings {
        tag_names: vec!["Notable Item".into()],
        ..ReportSettings::default()
    };
    let g = pm_report::generate(
        &case,
        ReportKind::Html,
        &settings,
        &FileReportSettings::default(),
        &NoProgress,
    )
    .unwrap();
    let nav = read(&g.path.parent().unwrap().join("content/nav.html"));
    assert!(nav.contains("Web Downloads (1)"), "{nav}");
    assert!(
        !nav.contains("Keyword Hits"),
        "untagged hits left out: {nav}"
    );
}

#[test]
fn excel_report_has_a_sheet_per_data_type() {
    let (_d, case) = case();
    let g = pm_report::generate(
        &case,
        ReportKind::Excel,
        &ReportSettings::default(),
        &FileReportSettings::default(),
        &NoProgress,
    )
    .unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&g.path).unwrap()).unwrap();
    let mut workbook = String::new();
    zip.by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook)
        .unwrap();
    for sheet in ["Summary", "Web Downloads", "Tagged Files", "Keyword Hits"] {
        assert!(
            workbook.contains(&format!("name=\"{sheet}\"")),
            "{sheet}: {workbook}"
        );
    }
}

#[test]
fn files_text_lists_every_file() {
    let (_d, case) = case();
    let g = pm_report::generate(
        &case,
        ReportKind::FilesText,
        &ReportSettings::default(),
        &FileReportSettings::default(),
        &NoProgress,
    )
    .unwrap();
    let text = read(&g.path);
    assert!(text.starts_with('\u{feff}'));
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    assert_eq!(
        lines.next().unwrap(),
        "\"Name\"\t\"File Extension\"\t\"File Type\"\t\"Is Deleted\"\t\"Last Accessed\"\t\"File Created\"\t\"Last Modified\"\t\"Size\"\t\"Address\"\t\"Hash Value\"\t\"Known Status\"\t\"Permissions\"\t\"Full Path\""
    );
    let pw = lines
        .find(|l| l.starts_with("\"passwords.txt\""))
        .expect("passwords.txt row");
    assert!(
        pw.contains("\".txt\"") && pw.contains("/demo.dd/Users/alice/Documents/passwords.txt"),
        "{pw}"
    );
}
