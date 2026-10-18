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
use pm_report::{NoProgress, ReportKind, ReportSettings};

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
    let download_type = pm_casedb::catalog::artifact_type_id("KAAMOS_WEB_DOWNLOAD").unwrap();
    let download = db
        .artifacts_of_type(download_type, None, 0, 10)
        .unwrap()
        .remove(0);
    db.tag_artifact(download.id, notable.id, "installer", "examiner", 2)
        .unwrap();
    // The examiner's notes: one finding about a file, one plain note.
    let n = db
        .add_note(
            "Router password <stored> in plain text",
            Some((pm_casedb::ObjKind::File, pw.id)),
            "examiner",
            3,
        )
        .unwrap();
    db.update_note(n, "Router password <stored> in plain text", true, 4)
        .unwrap();
    db.add_note("Check the VPN logs later", None, "examiner", 5)
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
        &pm_report::OtherSettings::default(),
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
    // Findings, not plain notes, are in the summary.
    assert!(summary.contains("<h2>Findings</h2>"));
    assert!(summary.contains("Router password &lt;stored&gt; in plain text"));
    assert!(summary.contains("passwords.txt"), "the noted file's path");
    assert!(!summary.contains("VPN logs"), "plain notes stay out");
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
        &pm_report::OtherSettings::default(),
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
        &pm_report::OtherSettings::default(),
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
        &pm_report::OtherSettings::default(),
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

#[test]
fn body_file_lists_file_system_files_for_mactime() {
    let (_d, case) = case();
    let g = pm_report::generate(
        &case,
        ReportKind::BodyFile,
        &ReportSettings::default(),
        &pm_report::OtherSettings::default(),
        &NoProgress,
    )
    .unwrap();
    assert!(g.path.ends_with("Body File/BodyFile.txt"));
    let text = read(&g.path);
    let line = text
        .lines()
        .find(|l| l.contains("/demo.dd/Users/alice/Documents/passwords.txt|"))
        .expect("passwords.txt line");
    let fields: Vec<&str> = line.split('|').collect();
    assert_eq!(fields.len(), 11, "{line}");
    assert_eq!(fields[0].len(), 32, "MD5 from Hash Lookup: {line}");
    assert!(fields[3].starts_with("r/r"), "{line}");
    assert_ne!(fields[8], "0", "mtime: {line}");
}

#[test]
fn tagged_hashes_go_into_a_hash_set() {
    let (d, case) = case();
    let set = d.path().join("tagged.kdb");
    let other = pm_report::OtherSettings {
        tagged_hashes: pm_report::tagged_hashes::TaggedHashesSettings {
            tag_names: vec!["Notable Item".into()],
            hash_set: set.clone(),
        },
        ..pm_report::OtherSettings::default()
    };
    let g = pm_report::generate(
        &case,
        ReportKind::TaggedHashes,
        &ReportSettings::default(),
        &other,
        &NoProgress,
    )
    .unwrap();
    assert_eq!(g.summary.rows, 1);
    let db = case.db();
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
    let md5: [u8; 16] = pw.md5.unwrap().try_into().unwrap();
    let hit = pm_hashdb::HashDb::open(&set, true)
        .unwrap()
        .lookup(&md5)
        .unwrap()
        .expect("hash added");
    assert_eq!(hit.comments, ["Demo <Case>"]);
    assert_eq!(hit.names, ["passwords.txt"]);
}
