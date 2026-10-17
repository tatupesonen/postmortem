//! Autopsy's GPX Parser module (`InternalPythonModules/GPX_Module`), on
//! `pm-plugin-api`: waypoints become `KAAMOS_GPS_BOOKMARK`, each track
//! segment a `KAAMOS_GPS_TRACK` and each route a `KAAMOS_GPS_ROUTE`, with Autopsy's
//! point-list JSON attributes.

use pm_geo::points::{self, TrackPoint, WayPoint};
use pm_plugin_api::{
    Attribute, FileFilter, FileInfo, FileIngest, Host, HostReader, Level, Message, NewResult,
    Plugin, PluginInfo, Result, Value,
};

pub const ID: &str = "gpx_parser";
pub const NAME: &str = "GPX Parser";

/// The largest GPX file read.
const MAX_GPX: u64 = 256 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct GpxParser;

impl Plugin for GpxParser {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: ID.into(),
            name: NAME.into(),
            description: "Module that extracts GEO data from GPX files.".into(),
            version: "1.2".into(),
            enabled_by_default: true,
            file_filter: Some(FileFilter {
                extensions: vec!["gpx".into()],
                ..FileFilter::default()
            }),
            data_source_stage: None,
        }
    }

    fn file_ingest(&self, _settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> {
        Ok(Some(Box::new(Worker { files: 0 })))
    }
}

struct Worker {
    files: u64,
}

impl FileIngest for Worker {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> Result<()> {
        let xml = HostReader::new(host, file).read_all(MAX_GPX)?;
        let gpx = match pm_geo::gpx::parse(&xml) {
            Ok(g) => g,
            Err(e) => {
                host.log(
                    Level::Warn,
                    &format!("Error parsing file {}: {e}", file.path()),
                );
                return Ok(());
            }
        };
        for segment in &gpx.track_segments {
            let pts: Vec<TrackPoint> = segment
                .iter()
                .map(|p| TrackPoint {
                    latitude: p.latitude,
                    longitude: p.longitude,
                    altitude: Some(p.elevation.unwrap_or(0.0)),
                    name: None,
                    velocity: Some(0.0),
                    distance_from_home_point: Some(0.0),
                    distance_traveled: Some(0.0),
                    timestamp: Some(p.time.unwrap_or(0)),
                })
                .collect();
            host.post(
                NewResult::new("KAAMOS_GPS_TRACK", Some(file.id))
                    .with(Attribute::text("KAAMOS_NAME", "Track"))
                    .with(Attribute::new(
                        "KAAMOS_GEO_TRACKPOINTS",
                        Value::Text(points::to_json(&pts)),
                    )),
            )?;
        }
        for w in &gpx.waypoints {
            host.post(
                NewResult::new("KAAMOS_GPS_BOOKMARK", Some(file.id))
                    .with(Attribute::new(
                        "KAAMOS_GEO_LATITUDE",
                        Value::Double(w.latitude),
                    ))
                    .with(Attribute::new(
                        "KAAMOS_GEO_LONGITUDE",
                        Value::Double(w.longitude),
                    ))
                    .with(Attribute::text("KAAMOS_FLAG", "Waypoint"))
                    .with(Attribute::text(
                        "KAAMOS_NAME",
                        w.name.clone().unwrap_or_default(),
                    ))
                    .with(Attribute::text("KAAMOS_PROG_NAME", "GPXParser")),
            )?;
        }
        for route in &gpx.routes {
            let pts: Vec<WayPoint> = route
                .iter()
                .map(|p| WayPoint {
                    latitude: p.latitude,
                    longitude: p.longitude,
                    altitude: p.elevation,
                    name: p.name.clone(),
                })
                .collect();
            host.post(
                NewResult::new("KAAMOS_GPS_ROUTE", Some(file.id)).with(Attribute::new(
                    "KAAMOS_GEO_WAYPOINTS",
                    Value::Text(points::to_json(&pts)),
                )),
            )?;
        }
        self.files += 1;
        Ok(())
    }

    fn finish(&mut self, host: &dyn Host) {
        if self.files > 0 {
            host.message(Message {
                level: Level::Info,
                subject: format!("{} files found", self.files),
                details: String::new(),
                file_id: None,
            });
        }
    }
}
