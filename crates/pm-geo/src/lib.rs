//! Geolocation (Autopsy `geolocation/datamodel`): waypoints and paths
//! built from GPS and EXIF results, the Autopsy point-list JSON formats, a GPX
//! parser (Autopsy's GPX Parser module) and a KML writer (Autopsy's KML
//! report).

pub mod gpx;
pub mod kml;
mod model;
pub mod points;

pub use model::{GeoData, GeoFilter, GeoPath, PathKind, Waypoint, WaypointKind, load};
