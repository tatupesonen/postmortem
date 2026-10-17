//! The timeline over the case database: file times and artifact events.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::sync::Arc;

use jiff::tz::TimeZone;
use pm_casedb::{NewArtifact, Value, catalog};
use pm_timeline::db::DbTimeline;
use pm_timeline::{
    CountsQuery, DeletedFilter, EventType, FileCategory, Filter, TimeUnit, TypeZoom,
};

fn demo_case() -> (tempfile::TempDir, pm_case::Case) {
    let dir = tempfile::tempdir().unwrap();
    let case =
        pm_case::Case::create(dir.path().join("c"), pm_case::CaseDetails::default()).unwrap();
    let s = pm_case::add_image(
        &case,
        pm_testkit::demo::demo_disk_file(),
        &pm_case::LoadOptions::default(),
        &pm_case::LoadProgress::default(),
    )
    .unwrap();
    let attr = |n: &str| catalog::attribute_type_id(n).unwrap();
    case.db()
        .add_artifact(&NewArtifact {
            type_id: catalog::artifact_type_id("TSK_PROG_RUN").unwrap(),
            ds_id: s.ds_id,
            file_id: None,
            source_artifact_id: None,
            score: None,
            conclusion: None,
            configuration: None,
            justification: None,
            created_at: 0,
            attributes: vec![
                (
                    attr("TSK_PROG_NAME"),
                    Value::Text("SETUP.EXE".into()),
                    "t".into(),
                ),
                (
                    attr("TSK_DATETIME"),
                    Value::DateTime(1_622_505_490),
                    "t".into(),
                ),
            ],
        })
        .unwrap();
    (dir, case)
}

#[test]
fn counts_lists_and_filters_from_the_database() {
    let (_dir, case) = demo_case();
    let t = DbTimeline::open(Arc::clone(case.db())).unwrap();
    let (s, e) = t.span().unwrap();
    let all = Filter::default();
    let total = t.count(s, e, &all).unwrap();
    assert!(total > 40, "MACB times of the demo files, got {total}");

    // Counts per bin add up to the total in the units the window offers
    // for the whole span (finer ones stop at `max_bins`).
    let tz = TimeZone::UTC;
    let auto = TimeUnit::for_range(s, e);
    for unit in [TimeUnit::Years, auto] {
        let bins = t
            .counts(&CountsQuery {
                start: s,
                end: e,
                unit,
                tz: &tz,
                filter: &all,
                zoom: TypeZoom::Event,
                max_bins: 400,
            })
            .unwrap();
        let sum: u64 = bins.iter().map(pm_timeline::Bin::total).sum();
        assert_eq!(sum, total, "{unit:?}");
    }

    // Around the demo's fixed timestamp: setup.exe's MACB row and the
    // program run from the artifact.
    let around = t.list(1_622_505_000, 1_622_506_000, &all, 10_000).unwrap();
    let setup = around
        .rows
        .iter()
        .find(|r| {
            around
                .subject(r.subject)
                .unwrap()
                .description
                .ends_with("/setup.exe")
        })
        .expect("setup.exe row");
    assert!(setup.macb().is_some());
    let run = around
        .rows
        .iter()
        .find(|r| r.kinds == [EventType::ProgramExecution])
        .expect("program run row");
    let subj = around.subject(run.subject).unwrap();
    assert_eq!(&*subj.description, "SETUP.EXE");
    assert!(subj.artifact_id.is_some());

    // Type, data-source, deleted and category filters.
    let runs = Filter {
        event_types: [EventType::ProgramExecution].into(),
        ..Filter::default()
    };
    assert_eq!(t.count(s, e, &runs).unwrap(), 1);
    let ds = t.data_sources()[0].0;
    let in_ds = Filter {
        data_sources: [u32::try_from(ds).unwrap()].into(),
        ..Filter::default()
    };
    assert_eq!(t.count(s, e, &in_ds).unwrap(), total);
    let deleted = Filter {
        deleted: DeletedFilter::OnlyDeleted,
        ..Filter::default()
    };
    let d = t.count(s, e, &deleted).unwrap();
    assert!(d > 0 && d < total);
    let images = Filter {
        file_categories: [FileCategory::Images].into(),
        ..Filter::default()
    };
    let listed = t.list(s, e, &images, 10_000).unwrap();
    assert!(!listed.rows.is_empty());
    assert!(
        listed
            .subjects
            .iter()
            .all(|x| { FileCategory::from_name(&x.description) == FileCategory::Images })
    );
    // A small limit truncates.
    let page = t.list(s, e, &all, 5).unwrap();
    assert!(page.truncated && page.rows.len() <= 5);
}
