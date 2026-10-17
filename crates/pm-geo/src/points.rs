//! TSK's JSON attribute formats for point lists (`GeoTrackPoints`,
//! `GeoWaypoints`, `GeoAreaPoints`, serialised by `BlackboardJsonAttrUtil`):
//! `{"pointList":[{...}, ...]}`.

use serde::{Deserialize, Serialize};

/// A track point (`TSK_GEO_TRACKPOINTS`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackPoint {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub velocity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_from_home_point: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_traveled: Option<f64>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

/// A route waypoint (`TSK_GEO_WAYPOINTS`) or area point
/// (`TSK_GEO_AREAPOINTS`, latitude/longitude only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WayPoint {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointList<T> {
    point_list: Vec<T>,
}

/// Parses a point list (`None` if it is not valid JSON of that shape).
pub fn parse<T: for<'de> Deserialize<'de>>(json: &str) -> Option<Vec<T>> {
    serde_json::from_str::<PointList<T>>(json)
        .ok()
        .map(|p| p.point_list)
}

/// Serialises a point list as TSK does.
pub fn to_json<T: Serialize + Clone>(points: &[T]) -> String {
    serde_json::to_string(&PointList {
        point_list: points.to_vec(),
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_tsk_json() {
        let json = r#"{"pointList":[{"latitude":60.1,"longitude":24.9,"altitude":5.0,"timestamp":1600000000,"distanceTraveled":3.5}]}"#;
        let pts: Vec<TrackPoint> = parse(json).unwrap();
        assert_eq!(pts[0].timestamp, Some(1_600_000_000));
        assert_eq!(pts[0].distance_traveled, Some(3.5));
        assert_eq!(parse::<TrackPoint>(&to_json(&pts)).unwrap(), pts);
        assert!(parse::<WayPoint>("not json").is_none());
    }
}
