//! Headless UI tests driving the real app with `egui_kittest`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pm_ui::{Persisted, PostmortemApp};

static DEMO: LazyLock<(tempfile::TempDir, PathBuf)> = LazyLock::new(|| {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo.dd");
    std::fs::write(&path, pm_testkit::demo::demo_disk()).unwrap();
    (dir, path)
});

/// Cases created by the app during tests.
static CASES: LazyLock<tempfile::TempDir> = LazyLock::new(|| tempfile::tempdir().unwrap());

fn harness_with(open: Vec<PathBuf>, case: bool) -> Harness<'static, PostmortemApp> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let ctx = egui::Context::default();
    let mut app = PostmortemApp::with_state(
        &ctx,
        Persisted {
            theme: egui::ThemePreference::Light,
            // Never create cases in the user's home folder from tests.
            case_root: Some(CASES.path().to_path_buf()),
            ..Default::default()
        },
        Vec::new(),
    );
    if case {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        app.create_case(
            &ctx,
            &CASES.path().join(format!("case{n}")),
            pm_case::CaseDetails {
                name: format!("Test case {n}"),
                ..Default::default()
            },
        )
        .unwrap();
    }
    for p in open {
        if p.extension().is_some_and(|e| e == "pmcase") {
            app.open_case(&ctx, &p).unwrap();
        } else {
            app.add_data_source(&ctx, p);
        }
    }
    Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_theme(egui::Theme::Light)
        .build_ui_state(|ui, app: &mut PostmortemApp| app.frame(ui), app)
}

/// A harness with a fresh case containing `open`.
fn harness(open: Vec<PathBuf>) -> Harness<'static, PostmortemApp> {
    harness_with(open, true)
}

/// Steps until `done` holds (background tasks finish off-thread).
fn settle(
    h: &mut Harness<'_, PostmortemApp>,
    mut done: impl FnMut(&mut Harness<'_, PostmortemApp>) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        h.step();
        if done(h) {
            break;
        }
        if Instant::now() >= deadline {
            shot(h, "timeout");
            panic!("UI did not settle");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// Waits until background analysis has finished (the status bar shows
/// the file being analysed, which would match label queries for paths).
fn wait_analysed(h: &mut Harness<'_, PostmortemApp>) {
    settle(h, |h| {
        h.query_by_label_contains("Analysed demo.dd").is_some()
    });
}

/// Saves a screenshot when `PM_UI_SCREENSHOT_DIR` is set.
fn shot(h: &mut Harness<'_, PostmortemApp>, name: &str) {
    if let Ok(dir) = std::env::var("PM_UI_SCREENSHOT_DIR") {
        h.render()
            .unwrap()
            .save(PathBuf::from(dir).join(format!("{name}.png")))
            .unwrap();
    }
}

/// Clicks the first node whose label contains `label` (the tree renders
/// before the table, so tree nodes win when both show the same name).
/// Waits for the node to appear: listings load on background threads.
fn click(h: &mut Harness<'_, PostmortemApp>, label: &str) {
    settle(h, |h| h.query_all_by_label_contains(label).next().is_some());
    h.query_all_by_label_contains(label)
        .next()
        .unwrap_or_else(|| panic!("no node labelled {label:?}"))
        .click();
    h.run_steps(2);
}

/// Clicks the first node whose label is exactly `label`.
fn click_exact(h: &mut Harness<'_, PostmortemApp>, label: &str) {
    settle(h, |h| h.query_all_by_label(label).next().is_some());
    h.query_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("no node labelled exactly {label:?}"))
        .click();
    h.run_steps(2);
}

#[test]
fn without_a_case_only_case_actions_are_offered() {
    let mut h = harness_with(Vec::new(), false);
    h.run_steps(2);
    h.get_by_label_contains("No case open");
    h.get_by_label("🗁  New Case");
    h.get_by_label("📂  Open Case");
    assert!(
        h.get_by_label("➕ Add Data Source")
            .accesskit_node()
            .is_disabled()
    );
    shot(&mut h, "welcome-no-case");
}

#[test]
fn empty_case_offers_add_data_source() {
    let mut h = harness(Vec::new());
    h.run_steps(2);
    h.get_by_label_contains("No data sources");
    h.get_by_label("➕  Add Data Source");
    assert!(
        !h.get_by_label("➕ Add Data Source")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn new_case_wizard_creates_the_case() {
    let mut h = harness_with(Vec::new(), false);
    h.run_steps(2);
    click_exact(&mut h, "🗁 New Case");
    h.get_by_label_contains("Step 1 of 2");
    // Next without a name is refused.
    click_exact(&mut h, "Next ›");
    h.get_by_label_contains("Enter a case name");
    let name_box = h
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap();
    name_box.click();
    h.run_steps(1);
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .type_text("Operation Teapot");
    h.run_steps(2);
    h.get_by_label_contains("Operation Teapot");
    shot(&mut h, "new-case-step1");
    click_exact(&mut h, "Next ›");
    h.get_by_label_contains("Step 2 of 2");
    shot(&mut h, "new-case-step2");
    click_exact(&mut h, "Finish");
    h.run_steps(3);
    assert!(h.state().has_case());
    let dir = h.state().analysis_case_dir().unwrap();
    assert_eq!(dir, CASES.path().join("Operation Teapot"));
    assert!(dir.join("case.pmcase").is_file());
    h.get_by_label_contains("No data sources");
}

#[test]
fn browse_demo_disk_and_view_picture() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });

    // Data-source summary is shown first.
    settle(&mut h, |h| {
        h.query_by_label_contains("Allocated files").is_some()
    });

    // Expand: data source → NTFS volume → root → Users → alice → Pictures.
    click_exact(&mut h, "🖴 demo.dd");
    for (label, wait) in [
        ("NTFS / exFAT", false),
        ("/ (NTFS)", true),
        ("Users", true),
        ("alice", true),
        ("Pictures", true),
    ] {
        click(&mut h, label);
        if wait {
            settle(&mut h, |h| {
                h.query_all_by_label_contains("Loading").next().is_none()
            });
        }
    }
    // The table lists the pictures; select one.
    settle(&mut h, |h| {
        h.query_by_label_contains("sunset.bmp").is_some()
    });
    click(&mut h, "sunset.bmp");
    settle(&mut h, |h| {
        h.query_by_label_contains("320 × 200 px").is_some()
    });

    if let Ok(dir) = std::env::var("PM_UI_SCREENSHOT_DIR") {
        let img = h.render().unwrap();
        img.save(PathBuf::from(dir).join("browse_picture.png"))
            .unwrap();
    }
}

#[test]
fn deleted_files_and_metadata_tab() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| !h.state().data_sources().is_empty());
    click_exact(&mut h, "🖴 demo.dd");
    for label in ["NTFS / exFAT", "/ (NTFS)", "Users", "alice", "Downloads"] {
        click(&mut h, label);
        settle(&mut h, |h| {
            h.query_all_by_label_contains("Loading").next().is_none()
        });
    }
    settle(&mut h, |h| {
        h.query_by_label_contains("secret_plan.docx").is_some()
    });
    // Deleted file is marked and its ADS sibling row is listed.
    h.get_by_label_contains("secret_plan.docx ✖");
    click(&mut h, "setup.exe:Zone.Identifier");
    click(&mut h, "File Metadata");
    shot(&mut h, "metadata_tab");
    h.get_by_label_contains("$FILE_NAME times");
    if let Ok(dir) = std::env::var("PM_UI_SCREENSHOT_DIR") {
        h.render()
            .unwrap()
            .save(PathBuf::from(dir).join("deleted_metadata.png"))
            .unwrap();
    }
}

#[test]
fn data_source_summary_tabs() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| !h.state().data_sources().is_empty());
    settle(&mut h, |h| {
        h.query_by_label_contains("Allocated files").is_some()
    });
    h.get_by_label_contains("Images:");
    shot(&mut h, "summary_types");
    click_exact(&mut h, "Recent Files");
    settle(&mut h, |h| {
        h.query_by_label_contains("Recently Modified Files")
            .is_some()
    });
    click_exact(&mut h, "Timeline");
    settle(&mut h, |h| {
        h.query_by_label_contains("View in Timeline").is_some()
    });
    shot(&mut h, "summary_timeline");
    click_exact(&mut h, "Container");
    h.get_by_label_contains("Image format");
}

#[test]
fn welcome_screen_screenshot() {
    let mut h = harness_with(Vec::new(), false);
    h.run_steps(4);
    shot(&mut h, "welcome");
}

#[test]
fn adding_an_image_loads_and_analyses_it() {
    let mut h = harness(vec![DEMO.1.clone()]);
    // The status bar reports the finished analysis.
    settle(&mut h, |h| {
        h.query_by_label_contains("Analysed demo.dd").is_some()
    });
    shot(&mut h, "analysis-done");
    let case = h.state().analysis_case_dir().expect("case created");
    assert!(case.starts_with(CASES.path()));
    assert!(case.join("case.pmcase").is_file());
    // Hash Lookup ran: every regular file got an MD5.
    let hashed = h.state().analysis_hashed_files();
    assert!(hashed > 20, "{hashed} files hashed");
}

#[test]
fn reopening_a_case_restores_its_data_sources_without_reanalysis() {
    let dir = {
        let mut h = harness(vec![DEMO.1.clone()]);
        settle(&mut h, |h| {
            h.query_by_label_contains("Analysed demo.dd").is_some()
        });
        h.state().analysis_case_dir().unwrap()
    };
    let mut h = harness_with(vec![dir.join("case.pmcase")], false);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });
    assert!(h.state().has_case());
    assert_eq!(h.state().data_sources().len(), 1);
    // Results from the first session are still there; nothing re-ran.
    assert!(h.state().analysis_hashed_files() > 20);
    assert!(h.query_by_label_contains("Analysed demo.dd").is_none());
    shot(&mut h, "reopened-case");
}
