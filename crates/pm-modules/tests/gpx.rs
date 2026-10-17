//! GPX Parser end to end, read back through the geolocation model.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

mod common;

use common::DemoCase;
use pm_ingest::{JobSettings, ModuleSetting};
use pm_testkit::ntfs::NtfsBuilder;

const GPX: &str = r#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="60.17" lon="24.94"><name>Home</name></wpt>
  <trk><trkseg>
    <trkpt lat="60.10" lon="24.90"><time>2021-03-04T10:00:00Z</time></trkpt>
    <trkpt lat="60.20" lon="24.80"><time>2021-03-04T10:30:00Z</time></trkpt>
  </trkseg></trk>
  <rte><rtept lat="61" lon="25"><name>A</name></rtept><rtept lat="62" lon="26"/></rte>
</gpx>"#;

#[test]
fn gpx_files_become_bookmarks_tracks_and_routes() {
    let mut b = NtfsBuilder::new(8);
    b.dir("/Users");
    b.file("/Users/run.gpx", GPX.as_bytes());
    let case = DemoCase::with_image(&b.build());
    let modules = pm_modules::core_modules();
    let settings = JobSettings {
        modules: modules
            .iter()
            .map(|m| ModuleSetting {
                id: m.info().id.to_owned(),
                enabled: m.info().id == "gpx_parser",
                settings: m.default_settings(),
            })
            .collect(),
        ..JobSettings::default()
    };
    let (summary, inbox) = case.run(&settings, modules);
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    assert_eq!(case.artifacts("TSK_GPS_BOOKMARK").len(), 1);
    assert_eq!(case.artifacts("TSK_GPS_TRACK").len(), 1);
    assert_eq!(case.artifacts("TSK_GPS_ROUTE").len(), 1);
    assert!(inbox.since(0).iter().any(|m| m.subject == "1 files found"));

    let geo = pm_geo::load(case.case.db(), &pm_geo::GeoFilter::default()).unwrap();
    assert_eq!(geo.waypoints.len(), 1);
    assert_eq!(geo.waypoints[0].label, "Home");
    let track = geo
        .paths
        .iter()
        .find(|p| p.kind == pm_geo::PathKind::Track)
        .unwrap();
    assert_eq!(track.points.len(), 2);
    assert_eq!(track.points[1].time, Some(1_614_853_800));
    let route = geo
        .paths
        .iter()
        .find(|p| p.kind == pm_geo::PathKind::Route)
        .unwrap();
    assert_eq!(route.points[0].label, "A");

    // The newest 0 days: only the newest timed points (plus untimed ones).
    let recent = pm_geo::load(
        case.case.db(),
        &pm_geo::GeoFilter {
            most_recent_days: Some(0),
            include_without_time: false,
            ..pm_geo::GeoFilter::default()
        },
    )
    .unwrap();
    assert_eq!(recent.all_points().count(), 1, "{recent:?}");
}
