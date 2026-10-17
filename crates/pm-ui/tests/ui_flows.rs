//! Headless UI tests driving the real app with `egui_kittest`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::format_collect,
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
    harness_full(open, case, pm_ui::IngestPrefs::default())
}

fn harness_full(
    open: Vec<PathBuf>,
    case: bool,
    ingest: pm_ui::IngestPrefs,
) -> Harness<'static, PostmortemApp> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let ctx = egui::Context::default();
    let mut app = PostmortemApp::with_state(
        &ctx,
        Persisted {
            theme: egui::ThemePreference::Light,
            // Never create cases in the user's home folder from tests.
            case_root: Some(CASES.path().to_path_buf()),
            ingest,
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
    let deadline = Instant::now() + Duration::from_mins(1);
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

#[test]
fn analysis_results_are_listed_and_open_the_source_file() {
    // A case whose analysis already found a notable hash-set hit.
    let dir = CASES.path().join("with-results");
    {
        let case = pm_case::Case::create(&dir, pm_case::CaseDetails::default()).unwrap();
        let s = pm_case::add_image(
            &case,
            &DEMO.1,
            &pm_case::LoadOptions::default(),
            &pm_case::LoadProgress::default(),
        )
        .unwrap();
        let exe = case
            .db()
            .query_files(&pm_casedb::FileQuery::default(), 0, 100_000)
            .unwrap()
            .into_iter()
            .find(|f| f.name == "setup.exe")
            .unwrap();
        let set = pm_casedb::catalog::attribute_type_id("TSK_SET_NAME").unwrap();
        case.db()
            .add_artifact(&pm_casedb::NewArtifact {
                type_id: pm_casedb::catalog::artifact_type_id("TSK_HASHSET_HIT").unwrap(),
                ds_id: s.ds_id,
                file_id: Some(exe.id),
                source_artifact_id: None,
                score: Some((
                    pm_casedb::Significance::Notable,
                    pm_casedb::Priority::Normal,
                )),
                conclusion: None,
                configuration: Some("Malware".into()),
                justification: None,
                created_at: 0,
                attributes: vec![(
                    set,
                    pm_casedb::Value::Text("Malware".into()),
                    "Hash Lookup".into(),
                )],
            })
            .unwrap();
    }
    let mut h = harness_with(vec![dir.join("case.pmcase")], false);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });
    click_exact(&mut h, "# Hashset Hits (1)");
    settle(&mut h, |h| h.query_by_label_contains("Notable").is_some());
    // Configuration and Set Name columns.
    assert_eq!(h.get_all_by_label("Malware").count(), 2);
    shot(&mut h, "results-hashset");
    click_exact(&mut h, "setup.exe");
    // The viewer shows the selected result's file.
    settle(&mut h, |h| {
        h.query_by_label_contains("4d 5a").is_some() || h.query_by_label_contains("4D 5A").is_some()
    });
    shot(&mut h, "results-hashset-viewer");
}

/// MD5 of the demo disk's `setup.exe` (read through a throwaway case).
fn setup_exe_md5() -> String {
    use md5::Digest;
    let tmp = tempfile::tempdir().unwrap();
    let case =
        pm_case::Case::create(tmp.path().join("c"), pm_case::CaseDetails::default()).unwrap();
    pm_case::add_image(
        &case,
        &DEMO.1,
        &pm_case::LoadOptions::default(),
        &pm_case::LoadProgress::default(),
    )
    .unwrap();
    let f = case
        .db()
        .query_files(&pm_casedb::FileQuery::default(), 0, 100_000)
        .unwrap()
        .into_iter()
        .find(|f| f.name == "setup.exe")
        .unwrap();
    let src = case.content(&f).unwrap();
    let data = src
        .read_vec_at(0, usize::try_from(src.size()).unwrap())
        .unwrap();
    md5::Md5::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn add_data_source_wizard_runs_configured_hash_lookup() {
    let sets = tempfile::tempdir().unwrap();
    let set = sets.path().join("bad.md5");
    std::fs::write(&set, format!("{}  setup.exe\n", setup_exe_md5())).unwrap();
    let mut ingest = pm_ui::IngestPrefs::default();
    ingest
        .hash_lookup
        .sets
        .push(pm_modules::hash_lookup::HashSetConfig {
            path: set,
            name: Some("Bad stuff".into()),
            kind: pm_modules::hash_lookup::SetKind::Notable,
            search_during_ingest: true,
            send_ingest_messages: None,
        });
    let mut h = harness_full(Vec::new(), true, ingest);
    h.run_steps(2);
    click_exact(&mut h, "➕ Add Data Source");
    h.get_by_label_contains("Step 1 of 2");
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .click();
    h.run_steps(1);
    h.get_all_by_role(egui::accesskit::Role::TextInput)
        .next()
        .unwrap()
        .type_text(&DEMO.1.display().to_string());
    h.run_steps(2);
    click_exact(&mut h, "Next ›");
    h.get_by_label_contains("Step 2 of 2");
    h.get_by_label("Hash Lookup");
    h.get_by_label("Bad stuff");
    shot(&mut h, "add-source-ingest");
    click_exact(&mut h, "Finish");
    settle(&mut h, |h| {
        h.query_by_label_contains("Analysed demo.dd").is_some()
    });
    settle(&mut h, |h| h.query_by_label("# Hashset Hits (1)").is_some());
    shot(&mut h, "add-source-done");
}

#[test]
fn views_list_files_by_type_from_the_case_database() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    // Counts refresh in the background; wait for the loaded value.
    settle(&mut h, |h| {
        h.query_all_by_label_contains("Images (").any(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l != "🖼 Images (0)")
        })
    });
    let label = h
        .query_all_by_label_contains("Images (")
        .next()
        .unwrap()
        .accesskit_node()
        .label()
        .unwrap_or_default();
    assert_ne!(label, "🖼 Images (0)", "{label}");
    click_exact(&mut h, &label);
    settle(&mut h, |h| {
        h.query_by_label_contains("sunset.bmp").is_some()
    });
    click(&mut h, "sunset.bmp");
    settle(&mut h, |h| {
        h.query_by_label_contains("320 × 200 px").is_some()
    });
    shot(&mut h, "views-images");
    // Deleted files view.
    click(&mut h, "Deleted Files (");
    click(&mut h, "File System (");
    settle(&mut h, |h| {
        h.query_by_label_contains("deleted_photo.bmp").is_some()
    });
}

#[test]
fn file_metadata_shows_hashes_after_analysis() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });
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
    // The file itself, not its Zone.Identifier stream.
    click_exact(&mut h, "🗋 setup.exe");
    click(&mut h, "File Metadata");
    let md5 = setup_exe_md5();
    settle(&mut h, |h| h.query_by_label(&md5).is_some());
    h.get_by_label("SHA-1");
    h.get_by_label("SHA-256");
    shot(&mut h, "metadata-hashes");
}

/// Scrolls the table under `pos` horizontally to its right end.
fn scroll_right(h: &mut Harness<'_, PostmortemApp>, pos: egui::Pos2) {
    h.hover_at(pos);
    h.run_steps(2);
    for _ in 0..20 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-200.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(1);
    }
    h.run_steps(3);
}

/// Whether the node labelled `label` lies inside the window horizontally.
fn fully_visible(h: &Harness<'_, PostmortemApp>, label: &str) -> bool {
    h.query_by_label(label).is_some_and(|n| {
        n.accesskit_node()
            .bounding_box()
            .is_some_and(|r| r.x0 >= 0.0 && r.x1 <= 1400.0)
    })
}

#[test]
fn file_listing_scrolls_to_last_column() {
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
    shot(&mut h, "hscroll-before");
    scroll_right(&mut h, egui::pos2(800.0, 300.0));
    shot(&mut h, "hscroll-after");
    assert!(fully_visible(&h, "Meta Addr"));
}

/// A case whose web history has three visits, with enough attribute
/// columns to be wider than the window.
fn case_with_history(name: &str) -> PathBuf {
    let dir = CASES.path().join(name);
    let case = pm_case::Case::create(&dir, pm_case::CaseDetails::default()).unwrap();
    let s = pm_case::add_image(
        &case,
        &DEMO.1,
        &pm_case::LoadOptions::default(),
        &pm_case::LoadProgress::default(),
    )
    .unwrap();
    let attr = |n: &str| pm_casedb::catalog::attribute_type_id(n).unwrap();
    let text = |s: &str| pm_casedb::Value::Text(s.into());
    for (url, t) in [
        ("https://c.example/", 300),
        ("https://a.example/", 100),
        ("https://b.example/", 200),
    ] {
        case.db()
            .add_artifact(&pm_casedb::NewArtifact {
                type_id: pm_casedb::catalog::artifact_type_id("TSK_WEB_HISTORY").unwrap(),
                ds_id: s.ds_id,
                file_id: None,
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: 0,
                attributes: [
                    (attr("TSK_URL"), text(url)),
                    (
                        attr("TSK_DATETIME_ACCESSED"),
                        pm_casedb::Value::DateTime(1_600_000_000 + t),
                    ),
                    (attr("TSK_TITLE"), text("A page title")),
                    (attr("TSK_REFERRER"), text("https://referrer.example/")),
                    (attr("TSK_PROG_NAME"), text("Internet Explorer")),
                    (attr("TSK_DOMAIN"), text("example")),
                    (attr("TSK_USER_NAME"), text("alice")),
                ]
                .into_iter()
                .map(|(a, v)| (a, v, "test".to_owned()))
                .collect(),
            })
            .unwrap();
    }
    dir.join("case.pmcase")
}

/// Top edge of the node labelled `label`.
fn top(h: &Harness<'_, PostmortemApp>, label: &str) -> f64 {
    h.get_by_label(label)
        .accesskit_node()
        .bounding_box()
        .unwrap()
        .y0
}

#[test]
fn result_table_sorts_by_column() {
    let mut h = harness_with(vec![case_with_history("sort-results")], false);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });
    click(&mut h, "Web History (3)");
    settle(&mut h, |h| h.query_by_label("https://c.example/").is_some());
    let order = |h: &Harness<'_, PostmortemApp>| {
        let mut urls = [
            "https://a.example/",
            "https://b.example/",
            "https://c.example/",
        ];
        urls.sort_by(|a, b| top(h, a).total_cmp(&top(h, b)));
        urls
    };
    // Unsorted: database order.
    assert_eq!(
        order(&h),
        [
            "https://c.example/",
            "https://a.example/",
            "https://b.example/"
        ]
    );
    click_exact(&mut h, "URL");
    assert_eq!(
        order(&h),
        [
            "https://a.example/",
            "https://b.example/",
            "https://c.example/"
        ]
    );
    // Again: descending.
    click(&mut h, "URL ⏶");
    assert_eq!(
        order(&h),
        [
            "https://c.example/",
            "https://b.example/",
            "https://a.example/"
        ]
    );
    shot(&mut h, "results-sorted");
}

#[test]
fn result_table_scrolls_to_last_column() {
    let mut h = harness_with(vec![case_with_history("wide-results")], false);
    settle(&mut h, |h| {
        !h.state().data_sources().is_empty() && !h.state().is_loading()
    });
    click(&mut h, "Web History (3)");
    settle(&mut h, |h| h.query_by_label("https://c.example/").is_some());
    assert!(
        !fully_visible(&h, "Path"),
        "table should be wider than the window"
    );
    scroll_right(&mut h, egui::pos2(800.0, 150.0));
    shot(&mut h, "results-scrolled");
    assert!(fully_visible(&h, "Path"));
}

/// Tagging: Ctrl+B bookmarks the selected file; the Add File Tag menu
/// applies a notable tag; the Tags and Score nodes list the results.
#[test]
fn tagging_files_feeds_tags_and_score_nodes() {
    let mut h = harness(vec![DEMO.1.clone()]);
    // Room for the Tags and Score nodes below the expanded tree.
    h.set_size(egui::vec2(1400.0, 1500.0));
    wait_analysed(&mut h);
    settle(&mut h, |h| !h.state().data_sources().is_empty());
    click_exact(&mut h, "🖴 demo.dd");
    for label in ["NTFS / exFAT", "/ (NTFS)", "Users", "alice", "Pictures"] {
        click(&mut h, label);
        settle(&mut h, |h| {
            h.query_all_by_label_contains("Loading").next().is_none()
        });
    }
    click(&mut h, "sunset.bmp");
    h.event(egui::Event::Key {
        key: egui::Key::B,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.run_steps(2);
    // The Tags tree gains Bookmark with one file tag.
    settle(&mut h, |h| {
        h.query_by_label_contains("Bookmark (1)").is_some()
    });
    click(&mut h, "Bookmark (1)");
    click(&mut h, "File Tags (1)");
    settle(&mut h, |h| {
        h.query_all_by_label_contains("sunset.bmp").count() >= 1
            && h.query_by_label_contains("File Path").is_some()
    });

    // Notable Item from the file table's context menu scores the file.
    click_exact(&mut h, "🗀 Pictures");
    settle(&mut h, |h| {
        h.query_by_label_contains("🗋 sunset.bmp").is_some()
    });
    h.query_by_label_contains("🗋 sunset.bmp")
        .unwrap()
        .click_secondary();
    h.run_steps(2);
    click(&mut h, "Add File Tag");
    click(&mut h, "Notable Item (Notable)");
    settle(&mut h, |h| {
        h.query_by_label_contains("Bad Items (1)").is_some()
    });
    click(&mut h, "Bad Items (1)");
    // The Score view lists the file with Autopsy's "File" type.
    settle(&mut h, |h| h.query_all_by_label("File").next().is_some());
    shot(&mut h, "tags_and_score");
}

/// Result tags: the result table's Add Result Tag menu tags the
/// artifact; the Tags tree lists it under Result Tags.
#[test]
fn tagging_results_from_the_result_table() {
    let mut h = harness(vec![DEMO.1.clone()]);
    h.set_size(egui::vec2(1400.0, 1500.0));
    wait_analysed(&mut h);
    click(&mut h, "Web Downloads (1)");
    settle(&mut h, |h| h.query_by_label_contains("1 results").is_some());
    // The result row's source file cell.
    let src = "setup.exe:Zone.Identifier";
    settle(&mut h, |h| h.query_all_by_label(src).next().is_some());
    h.query_all_by_label(src).next().unwrap().click_secondary();
    h.run_steps(2);
    click(&mut h, "Add Result Tag");
    click_exact(&mut h, "Follow Up");
    settle(&mut h, |h| {
        h.query_by_label_contains("Follow Up (1)").is_some()
    });
    click(&mut h, "Follow Up (1)");
    click(&mut h, "Result Tags (1)");
    settle(&mut h, |h| {
        h.query_by_label_contains("Source File Path").is_some()
    });
    // The row's Result Type cell (besides the tree node).
    assert!(h.query_all_by_label("Web Downloads").count() >= 1);
    shot(&mut h, "result_tags");
}

/// File Search by Attributes: a name search lists the matching file.
#[test]
fn file_search_by_name() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    click(&mut h, "Tools");
    click(&mut h, "File Search by Attributes");
    click_exact(&mut h, "Name:");
    // The name field is the first text edit in the window.
    let field = h
        .query_all(egui_kittest::kittest::by().role(egui::accesskit::Role::TextInput))
        .next()
        .expect("name field");
    field.focus();
    field.type_text("SUNSET");
    h.run_steps(2);
    click_exact(&mut h, "Search");
    settle(&mut h, |h| {
        h.query_by_label_contains("File Search Results 1").is_some()
    });
    settle(&mut h, |h| h.query_by_label_contains("1 matches").is_some());
    h.get_by_label_contains("sunset.bmp");
    shot(&mut h, "file_search");
}

/// The data source menu runs ingest again and removes the data source.
#[test]
fn data_source_menu_runs_ingest_and_removes() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| {
        h.query_all_by_label("🖴 demo.dd").next().is_some()
    });
    h.query_all_by_label("🖴 demo.dd")
        .next()
        .unwrap()
        .click_secondary();
    h.run_steps(2);
    click(&mut h, "Run Ingest Modules");
    settle(&mut h, |h| {
        h.query_by_label_contains("Configure ingest for demo.dd")
            .is_some()
    });
    shot(&mut h, "run_ingest_dialog");
    click_exact(&mut h, "Finish");
    // A second job runs and finishes.
    settle(&mut h, |h| {
        h.query_all_by_label_contains("Analysed demo.dd").count() >= 1
            && h.query_by_label_contains("Configure ingest").is_none()
    });

    h.query_all_by_label("🖴 demo.dd")
        .next()
        .unwrap()
        .click_secondary();
    h.run_steps(2);
    settle(&mut h, |h| {
        h.query_by_label("Remove Data Source")
            .is_some_and(|n| !n.accesskit_node().is_disabled())
    });
    click_exact(&mut h, "Remove Data Source");
    click_exact(&mut h, "Remove");
    settle(&mut h, |h| {
        h.query_all_by_label("🖴 demo.dd").next().is_none()
    });
}

/// Archive members extracted by the Embedded File Extractor open like a
/// folder from the archive's row and tree node.
#[test]
fn archives_open_like_folders() {
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
    // The tree shows the archive as an expandable node.
    click_exact(&mut h, "📦 holiday.zip");
    settle(&mut h, |h| h.query_by_label_contains("beach.bmp").is_some());
    assert!(
        h.query_all_by_label("🗀 notes").next().is_some(),
        "member folder"
    );
    click(&mut h, "beach.bmp");
    settle(&mut h, |h| {
        h.query_by_label_contains("48 × 32 px").is_some()
    });
    shot(&mut h, "archive_member");
}

/// Hosts: the image gets TSK's default host; Manage Hosts adds a host;
/// the host menu associates the host with a new person.
#[test]
fn hosts_and_persons() {
    let mut h = harness(vec![DEMO.1.clone()]);
    wait_analysed(&mut h);
    settle(&mut h, |h| {
        h.query_all_by_label("🖥 demo.dd Host").next().is_some()
    });

    click(&mut h, "Case");
    click_exact(&mut h, "Manage Hosts");
    click_exact(&mut h, "New");
    let field = h
        .query_all(egui_kittest::kittest::by().role(egui::accesskit::Role::TextInput))
        .next()
        .expect("host name field");
    field.focus();
    field.type_text("Suspect phone");
    h.run_steps(2);
    click_exact(&mut h, "OK");
    settle(&mut h, |h| {
        h.query_all_by_label("🖥 Suspect phone").next().is_some()
    });
    click_exact(&mut h, "Close");

    h.query_all_by_label("🖥 demo.dd Host")
        .next()
        .unwrap()
        .click_secondary();
    h.run_steps(2);
    click(&mut h, "Associate with Person");
    click_exact(&mut h, "New Person...");
    let field = h
        .query_all(egui_kittest::kittest::by().role(egui::accesskit::Role::TextInput))
        .next()
        .expect("person name field");
    field.focus();
    field.type_text("John Doe");
    h.run_steps(2);
    click_exact(&mut h, "OK");
    // The host's menu now offers removing it from the person.
    let mut label = None;
    let deadline = Instant::now() + Duration::from_mins(1);
    while label.is_none() && Instant::now() < deadline {
        h.step();
        if let Some(n) = h.query_by_label_contains("Remove from Person") {
            label = n.accesskit_node().label();
            break;
        }
        // Re-open the menu once the association has been written.
        if h.query_by_label_contains("Associate with Person").is_none()
            && let Some(n) = h.query_all_by_label("🖥 demo.dd Host").next()
        {
            n.click_secondary();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(label.as_deref(), Some("Remove from Person (John Doe)"));
    shot(&mut h, "hosts_and_persons");
}

/// The case's tag names, read from its database.
fn case_tag_names(h: &Harness<'_, PostmortemApp>) -> Vec<(String, bool)> {
    let dir = h.state().analysis_case_dir().expect("a case");
    let db = pm_casedb::CaseDb::open(dir.join("case.db")).unwrap();
    db.tag_names()
        .unwrap()
        .into_iter()
        .map(|t| (t.name, t.notable))
        .collect()
}

/// Tools › Options › Tags: a new notable tag is saved to the user
/// settings and to the open case.
#[test]
fn options_tags_page_adds_a_notable_tag() {
    let mut h = harness(Vec::new());
    click(&mut h, "Tools");
    click(&mut h, "Options");
    click_exact(&mut h, "Tags");
    click_exact(&mut h, "New Tag");
    let field = h
        .query_all(egui_kittest::kittest::by().role(egui::accesskit::Role::TextInput))
        .next()
        .expect("tag name field");
    field.focus();
    field.type_text("Evidence");
    h.run_steps(2);
    click_exact(&mut h, "Tag indicates item is notable.");
    // The dialog's OK (the dialog is drawn above the Options window).
    h.query_all_by_label("OK").last().unwrap().click();
    h.run_steps(2);
    settle(&mut h, |h| {
        h.query_all_by_label("Evidence").next().is_some()
    });
    click_exact(&mut h, "Evidence");
    h.get_by_label("Yes");
    shot(&mut h, "options_tags");
    click_exact(&mut h, "OK");
    settle(&mut h, |h| {
        h.query_by_label_contains("Tag Names:").is_none()
    });
    settle(&mut h, |h| {
        case_tag_names(h).contains(&("Evidence".to_owned(), true))
    });
}

/// User tag definitions are added to every case that is opened.
#[test]
fn user_tags_are_added_to_new_cases() {
    let ctx = egui::Context::default();
    let mut app = PostmortemApp::with_state(
        &ctx,
        Persisted {
            case_root: Some(CASES.path().to_path_buf()),
            custom_tags: vec![pm_ui::TagDefinition {
                name: "Reviewed".into(),
                description: "Seen by a second examiner".into(),
                notable: false,
            }],
            ..Default::default()
        },
        Vec::new(),
    );
    app.create_case(
        &ctx,
        &CASES.path().join("user-tags"),
        pm_case::CaseDetails {
            name: "user tags".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .build_ui_state(|ui, app: &mut PostmortemApp| app.frame(ui), app);
    settle(&mut h, |h| {
        case_tag_names(h).contains(&("Reviewed".to_owned(), false))
    });
}
