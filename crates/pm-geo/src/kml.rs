//! Google Earth KML (Autopsy's KML report): one folder per waypoint kind
//! with point placemarks, tracks and routes as line strings, areas as
//! polygons; styles in a separate `style.kml`.

use std::fmt::Write as _;

use crate::{GeoData, PathKind, Waypoint, WaypointKind};

/// `style.kml`: Autopsy's coloured feature styles.
pub const STYLE_KML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2">
<Document>
<Style id="redFeature"><IconStyle><color>ff0000ff</color></IconStyle><LineStyle><color>ff0000ff</color><width>5</width></LineStyle></Style>
<Style id="greenFeature"><IconStyle><color>ff00ff00</color></IconStyle><LineStyle><color>ff00ff00</color><width>5</width></LineStyle></Style>
<Style id="blueFeature"><IconStyle><color>ffff0000</color></IconStyle><LineStyle><color>ffff0000</color><width>5</width></LineStyle></Style>
<Style id="purpleFeature"><IconStyle><color>ffff00ff</color></IconStyle><LineStyle><color>ffff00ff</color><width>5</width></LineStyle></Style>
<Style id="whiteFeature"><IconStyle><color>ffffffff</color></IconStyle><LineStyle><color>ffffffff</color><width>5</width></LineStyle></Style>
<Style id="yellowFeature"><IconStyle><color>ff00ffff</color></IconStyle><LineStyle><color>ff00ffff</color><width>5</width></LineStyle><PolyStyle><color>7f00ffff</color></PolyStyle></Style>
</Document>
</kml>
"#;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn time(t: Option<i64>) -> Option<String> {
    let t = jiff::Timestamp::from_second(t?).ok()?;
    Some(t.strftime("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Autopsy's colour per kind.
fn style(kind: WaypointKind) -> &'static str {
    match kind {
        WaypointKind::Exif => "style.kml#purpleFeature",
        WaypointKind::Bookmark => "style.kml#blueFeature",
        WaypointKind::LastKnown => "style.kml#redFeature",
        WaypointKind::Search => "style.kml#whiteFeature",
        WaypointKind::Trackpoint | WaypointKind::TrackPoint => "style.kml#greenFeature",
        WaypointKind::RoutePoint | WaypointKind::AreaPoint => "style.kml#yellowFeature",
    }
}

fn folder_name(kind: WaypointKind) -> &'static str {
    match kind {
        WaypointKind::Exif => "GPS EXIF Metadata",
        WaypointKind::Bookmark => "GPS Bookmarks",
        WaypointKind::LastKnown => "GPS Last Known Location",
        WaypointKind::Search => "GPS Searches",
        WaypointKind::Trackpoint | WaypointKind::TrackPoint => "GPS Trackpoints",
        WaypointKind::RoutePoint => "GPS Routes",
        WaypointKind::AreaPoint => "GPS Areas",
    }
}

fn coordinates(w: &Waypoint) -> String {
    format!(
        "{},{},{}",
        w.longitude,
        w.latitude,
        w.altitude.unwrap_or(0.0)
    )
}

fn placemark(out: &mut String, w: &Waypoint) {
    let _ = write!(out, "<Placemark>\n<name>{}</name>\n", esc(&w.label));
    if let Some(t) = time(w.time) {
        let _ = writeln!(out, "<TimeStamp><when>{t}</when></TimeStamp>");
    }
    let _ = write!(
        out,
        "<description><![CDATA[<h3>{kind}</h3><table><tr><td>Latitude:</td><td>{lat}</td></tr><tr><td>Longitude:</td><td>{lon}</td></tr>{alt}</table>]]></description>\n<styleUrl>{style}</styleUrl>\n<Point><coordinates>{c}</coordinates></Point>\n</Placemark>\n",
        kind = w.kind.display(),
        lat = w.latitude,
        lon = w.longitude,
        alt = w
            .altitude
            .map(|a| format!("<tr><td>Altitude:</td><td>{a}</td></tr>"))
            .unwrap_or_default(),
        style = style(w.kind),
        c = coordinates(w),
    );
}

/// The KML document for `data`.
pub fn write(data: &GeoData, title: &str) -> String {
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<kml xmlns=\"http://www.opengis.net/kml/2.2\">\n<Document>\n<name>{}</name>\n",
        esc(title)
    );
    let mut kinds: Vec<WaypointKind> = data.waypoints.iter().map(|w| w.kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    for kind in kinds {
        let _ = write!(out, "<Folder>\n<name>{}</name>\n", folder_name(kind));
        for w in data.waypoints.iter().filter(|w| w.kind == kind) {
            placemark(&mut out, w);
        }
        out.push_str("</Folder>\n");
    }
    for (kind, name) in [
        (PathKind::Track, "GPS Tracks"),
        (PathKind::Route, "GPS Routes"),
        (PathKind::Area, "GPS Areas"),
    ] {
        let paths: Vec<_> = data
            .paths
            .iter()
            .filter(|p| p.kind == kind && !p.points.is_empty())
            .collect();
        if paths.is_empty() {
            continue;
        }
        let _ = write!(out, "<Folder>\n<name>{name}</name>\n");
        for p in paths {
            let coords: Vec<String> = p.points.iter().map(coordinates).collect();
            let geometry = match kind {
                PathKind::Area => format!(
                    "<Polygon><outerBoundaryIs><LinearRing><coordinates>{} {}</coordinates></LinearRing></outerBoundaryIs></Polygon>",
                    coords.join(" "),
                    coords.first().cloned().unwrap_or_default()
                ),
                _ => format!(
                    "<LineString><tessellate>1</tessellate><coordinates>{}</coordinates></LineString>",
                    coords.join(" ")
                ),
            };
            let style = match kind {
                PathKind::Track => "style.kml#greenFeature",
                _ => "style.kml#yellowFeature",
            };
            let _ = write!(
                out,
                "<Placemark>\n<name>{}</name>\n<styleUrl>{style}</styleUrl>\n{geometry}\n</Placemark>\n",
                esc(&p.label)
            );
            for w in &p.points {
                placemark(&mut out, w);
            }
        }
        out.push_str("</Folder>\n");
    }
    out.push_str("</Document>\n</kml>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GeoPath, Waypoint};

    fn wp(kind: WaypointKind, label: &str) -> Waypoint {
        Waypoint {
            kind,
            label: label.into(),
            time: Some(1_614_848_400),
            latitude: 60.17,
            longitude: 24.94,
            altitude: None,
            artifact_id: 1,
            file_id: None,
            ds_id: 1,
        }
    }

    #[test]
    fn folders_points_and_paths() {
        let data = GeoData {
            waypoints: vec![wp(WaypointKind::Bookmark, "Home & away")],
            paths: vec![GeoPath {
                kind: PathKind::Track,
                label: "Run".into(),
                artifact_id: 2,
                ds_id: 1,
                points: vec![
                    wp(WaypointKind::TrackPoint, "p1"),
                    wp(WaypointKind::TrackPoint, "p2"),
                ],
            }],
        };
        let k = write(&data, "Case");
        assert!(k.contains("<name>GPS Bookmarks</name>"));
        assert!(k.contains("<name>Home &amp; away</name>"));
        assert!(k.contains("<coordinates>24.94,60.17,0</coordinates>"));
        assert!(k.contains("<when>2021-03-04T09:00:00Z</when>"));
        assert!(k.contains("<LineString>"));
    }
}
