//! Waypoints and paths from the case (Autopsy `WaypointBuilder`, `Track`,
//! `Route`, `Area`), with the Geolocation window's filters.

use std::collections::HashMap;

use pm_casedb::{ArtifactRecord, AttributeRecord, CaseDb, Value, catalog};

use crate::points::{self, TrackPoint, WayPoint};

/// What a waypoint comes from (Autopsy's waypoint classes and KML folders).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WaypointKind {
    Bookmark,
    LastKnown,
    Exif,
    Search,
    Trackpoint,
    /// A point of a `KAAMOS_GPS_TRACK`.
    TrackPoint,
    /// A point of a `KAAMOS_GPS_ROUTE`.
    RoutePoint,
    /// A corner of a `KAAMOS_GPS_AREA`.
    AreaPoint,
}

impl WaypointKind {
    /// Autopsy's display string (map legend, KML folders).
    pub fn display(self) -> &'static str {
        match self {
            Self::Bookmark => "GPS Bookmark",
            Self::LastKnown => "GPS Last Known Location",
            Self::Exif => "EXIF Metadata With Location",
            Self::Search => "GPS Search",
            Self::Trackpoint => "GPS Trackpoint",
            Self::TrackPoint => "GPS Individual Track Point",
            Self::RoutePoint => "GPS Individual Route Point",
            Self::AreaPoint => "GPS Area Outline Point",
        }
    }
}

/// A located point.
#[derive(Debug, Clone, PartialEq)]
pub struct Waypoint {
    pub kind: WaypointKind,
    pub label: String,
    /// Unix seconds.
    pub time: Option<i64>,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub artifact_id: i64,
    pub file_id: Option<i64>,
    pub ds_id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    Track,
    Route,
    Area,
}

/// A track, route or area outline.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoPath {
    pub kind: PathKind,
    pub label: String,
    pub artifact_id: i64,
    pub ds_id: i64,
    pub points: Vec<Waypoint>,
}

/// The Geolocation window's filters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeoFilter {
    /// Data sources (empty: all).
    pub data_sources: Vec<i64>,
    /// Only the last N days before the newest waypoint (Autopsy's "Show
    /// waypoints from the most recent N days").
    pub most_recent_days: Option<i64>,
    /// With `most_recent_days`: keep waypoints that have no time.
    pub include_without_time: bool,
}

/// Everything to draw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoData {
    pub waypoints: Vec<Waypoint>,
    pub paths: Vec<GeoPath>,
}

impl GeoData {
    /// Every point, path points included.
    pub fn all_points(&self) -> impl Iterator<Item = &Waypoint> {
        self.waypoints
            .iter()
            .chain(self.paths.iter().flat_map(|p| p.points.iter()))
    }
}

struct Attrs<'a>(HashMap<&'a str, &'a Value>);

impl<'a> Attrs<'a> {
    fn new(list: &'a [AttributeRecord]) -> Self {
        Self(list.iter().map(|a| (a.name.as_str(), &a.value)).collect())
    }

    fn f64(&self, name: &str) -> Option<f64> {
        match self.0.get(name)? {
            Value::Double(d) => Some(*d),
            Value::Int(i) => Some(*i as f64),
            Value::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }

    fn time(&self, name: &str) -> Option<i64> {
        match self.0.get(name)? {
            Value::DateTime(t) | Value::Int(t) => Some(*t).filter(|t| *t > 0),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<String> {
        match self.0.get(name)? {
            Value::Text(t) | Value::Json(t) if !t.is_empty() => Some(t.clone()),
            _ => None,
        }
    }
}

fn valid(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
}

/// Waypoint label rules (Autopsy: `KAAMOS_NAME`, then a type-specific fallback).
fn label(kind: WaypointKind, a: &Attrs<'_>, file_name: Option<&str>) -> String {
    let name = a.text("KAAMOS_NAME");
    match kind {
        WaypointKind::Exif => file_name.unwrap_or_default().to_owned(),
        WaypointKind::Search => name
            .or_else(|| a.text("KAAMOS_LOCATION"))
            .unwrap_or_else(|| "GPS Search".into()),
        WaypointKind::Trackpoint => name
            .or_else(|| a.text("KAAMOS_PROG_NAME"))
            .unwrap_or_else(|| "GPS Trackpoint".into()),
        WaypointKind::LastKnown => name.unwrap_or_else(|| "Last Known Location".into()),
        _ => name.unwrap_or_default(),
    }
}

/// Loads every waypoint and path of the case passing `filter`.
pub fn load(db: &CaseDb, filter: &GeoFilter) -> pm_casedb::Result<GeoData> {
    let mut data = GeoData::default();
    let point_types = [
        (
            "KAAMOS_GPS_TRACKPOINT",
            WaypointKind::Trackpoint,
            "KAAMOS_DATETIME",
        ),
        ("KAAMOS_GPS_SEARCH", WaypointKind::Search, "KAAMOS_DATETIME"),
        (
            "KAAMOS_GPS_LAST_KNOWN_LOCATION",
            WaypointKind::LastKnown,
            "KAAMOS_DATETIME",
        ),
        (
            "KAAMOS_GPS_BOOKMARK",
            WaypointKind::Bookmark,
            "KAAMOS_DATETIME",
        ),
        (
            "KAAMOS_METADATA_EXIF",
            WaypointKind::Exif,
            "KAAMOS_DATETIME_CREATED",
        ),
    ];
    let in_scope = |a: &ArtifactRecord| {
        filter.data_sources.is_empty() || filter.data_sources.contains(&a.ds_id)
    };
    for (ty, kind, time_attr) in point_types {
        for (art, attrs) in artifacts(db, ty)? {
            if !in_scope(&art) {
                continue;
            }
            let a = Attrs::new(&attrs);
            let (Some(lat), Some(lon)) =
                (a.f64("KAAMOS_GEO_LATITUDE"), a.f64("KAAMOS_GEO_LONGITUDE"))
            else {
                continue;
            };
            if !valid(lat, lon) {
                continue;
            }
            let file_name = match (kind, art.file_id) {
                (WaypointKind::Exif, Some(id)) => db.file(id)?.map(|f| f.name),
                _ => None,
            };
            data.waypoints.push(Waypoint {
                kind,
                label: label(kind, &a, file_name.as_deref()),
                time: a.time(time_attr),
                latitude: lat,
                longitude: lon,
                altitude: a.f64("KAAMOS_GEO_ALTITUDE"),
                artifact_id: art.id,
                file_id: art.file_id,
                ds_id: art.ds_id,
            });
        }
    }
    for (art, attrs) in artifacts(db, "KAAMOS_GPS_TRACK")? {
        if !in_scope(&art) {
            continue;
        }
        let a = Attrs::new(&attrs);
        let name = a.text("KAAMOS_NAME").unwrap_or_default();
        let pts: Vec<TrackPoint> = a
            .text("KAAMOS_GEO_TRACKPOINTS")
            .and_then(|j| points::parse(&j))
            .unwrap_or_default();
        let points = pts
            .into_iter()
            .filter(|p| valid(p.latitude, p.longitude))
            .map(|p| Waypoint {
                kind: WaypointKind::TrackPoint,
                label: format!("Trackpoint for track: {name}"),
                time: p.timestamp.filter(|t| *t > 0),
                latitude: p.latitude,
                longitude: p.longitude,
                altitude: p.altitude,
                artifact_id: art.id,
                file_id: art.file_id,
                ds_id: art.ds_id,
            })
            .collect();
        data.paths.push(GeoPath {
            kind: PathKind::Track,
            label: name,
            artifact_id: art.id,
            ds_id: art.ds_id,
            points,
        });
    }
    for (ty, kind, json_attr, point_kind) in [
        (
            "KAAMOS_GPS_ROUTE",
            PathKind::Route,
            "KAAMOS_GEO_WAYPOINTS",
            WaypointKind::RoutePoint,
        ),
        (
            "KAAMOS_GPS_AREA",
            PathKind::Area,
            "KAAMOS_GEO_AREAPOINTS",
            WaypointKind::AreaPoint,
        ),
    ] {
        for (art, attrs) in artifacts(db, ty)? {
            if !in_scope(&art) {
                continue;
            }
            let a = Attrs::new(&attrs);
            let time = a.time("KAAMOS_DATETIME");
            let mut pts: Vec<WayPoint> = a
                .text(json_attr)
                .and_then(|j| points::parse(&j))
                .unwrap_or_default();
            // Older routes: start and end coordinates only.
            if pts.is_empty()
                && let (Some(la), Some(lo), Some(lb), Some(lob)) = (
                    a.f64("KAAMOS_GEO_LATITUDE_START"),
                    a.f64("KAAMOS_GEO_LONGITUDE_START"),
                    a.f64("KAAMOS_GEO_LATITUDE_END"),
                    a.f64("KAAMOS_GEO_LONGITUDE_END"),
                )
            {
                pts = vec![
                    WayPoint {
                        latitude: la,
                        longitude: lo,
                        altitude: None,
                        name: None,
                    },
                    WayPoint {
                        latitude: lb,
                        longitude: lob,
                        altitude: None,
                        name: None,
                    },
                ];
            }
            let label = a.text("KAAMOS_NAME").unwrap_or_else(|| match kind {
                PathKind::Route => "As-the-crow-flies Route".into(),
                _ => String::new(),
            });
            let points = pts
                .into_iter()
                .filter(|p| valid(p.latitude, p.longitude))
                .map(|p| Waypoint {
                    kind: point_kind,
                    label: p.name.clone().unwrap_or_else(|| label.clone()),
                    time,
                    latitude: p.latitude,
                    longitude: p.longitude,
                    altitude: p.altitude,
                    artifact_id: art.id,
                    file_id: art.file_id,
                    ds_id: art.ds_id,
                })
                .collect();
            data.paths.push(GeoPath {
                kind,
                label,
                artifact_id: art.id,
                ds_id: art.ds_id,
                points,
            });
        }
    }
    if let Some(days) = filter.most_recent_days {
        let newest = data.all_points().filter_map(|w| w.time).max();
        if let Some(newest) = newest {
            let from = newest.saturating_sub(days.saturating_mul(86_400));
            let keep = |w: &Waypoint| w.time.map_or(filter.include_without_time, |t| t >= from);
            data.waypoints.retain(keep);
            for p in &mut data.paths {
                p.points.retain(keep);
            }
            data.paths.retain(|p| !p.points.is_empty());
        }
    }
    Ok(data)
}

/// Every artifact of a type with its attributes, by id.
fn artifacts(
    db: &CaseDb,
    type_name: &str,
) -> pm_casedb::Result<Vec<(ArtifactRecord, Vec<AttributeRecord>)>> {
    let Some(type_id) = catalog::artifact_type_id(type_name) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = db.artifacts_of_type(type_id, None, after, 5_000)?;
        let Some(last) = page.last() else { break };
        after = last.id;
        for a in page {
            let attrs = db.attributes(a.id)?;
            out.push((a, attrs));
        }
    }
    Ok(out)
}
