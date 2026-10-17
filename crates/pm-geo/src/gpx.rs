//! GPX 1.0/1.1 (what Autopsy's GPX Parser module reads with gpxpy):
//! waypoints, track segments and routes. Streaming XML; capped at
//! [`MAX_POINTS`] points so a hostile file cannot exhaust memory.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// The most points read from one file.
pub const MAX_POINTS: usize = 5_000_000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpxPoint {
    pub latitude: f64,
    pub longitude: f64,
    pub elevation: Option<f64>,
    /// Unix seconds.
    pub time: Option<i64>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gpx {
    pub waypoints: Vec<GpxPoint>,
    /// Track segments (Autopsy posts one `TSK_GPS_TRACK` per segment).
    pub track_segments: Vec<Vec<GpxPoint>>,
    pub routes: Vec<Vec<GpxPoint>>,
}

#[derive(Debug, thiserror::Error)]
#[error("not a valid GPX file: {0}")]
pub struct GpxError(String);

fn local(name: &[u8]) -> &[u8] {
    name.rsplit(|&b| b == b':').next().unwrap_or(name)
}

fn point(e: &BytesStart<'_>) -> Option<GpxPoint> {
    let mut lat = None;
    let mut lon = None;
    for a in e.attributes().flatten() {
        let v = String::from_utf8_lossy(&a.value);
        match local(a.key.as_ref()) {
            b"lat" => lat = v.trim().parse::<f64>().ok(),
            b"lon" => lon = v.trim().parse::<f64>().ok(),
            _ => {}
        }
    }
    Some(GpxPoint {
        latitude: lat?,
        longitude: lon?,
        ..GpxPoint::default()
    })
}

fn parse_time(s: &str) -> Option<i64> {
    s.trim()
        .parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_second)
}

/// Parses a GPX document.
pub fn parse(xml: &[u8]) -> Result<Gpx, GpxError> {
    let mut r = Reader::from_reader(xml);
    r.config_mut().trim_text(true);
    let mut gpx = Gpx::default();
    let mut seen_root = false;
    let mut cur: Option<(Vec<u8>, GpxPoint)> = None;
    let mut segment: Option<Vec<GpxPoint>> = None;
    let mut route: Option<Vec<GpxPoint>> = None;
    let mut field: Option<Vec<u8>> = None;
    let mut count = 0usize;
    let mut buf = Vec::new();
    loop {
        let ev = r
            .read_event_into(&mut buf)
            .map_err(|e| GpxError(e.to_string()))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = local(e.name().as_ref()).to_vec();
                let empty = matches!(ev, Event::Empty(_));
                match name.as_slice() {
                    b"gpx" => seen_root = true,
                    b"trkseg" if !empty => segment = Some(Vec::new()),
                    b"rte" if !empty => route = Some(Vec::new()),
                    b"wpt" | b"trkpt" | b"rtept" => {
                        if let Some(p) = point(e) {
                            if empty {
                                count += 1;
                                push(&mut gpx, &mut segment, &mut route, &name, p);
                            } else {
                                cur = Some((name.clone(), p));
                            }
                        }
                    }
                    b"ele" | b"time" | b"name" if cur.is_some() => field = Some(name),
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let (Some(f), Some((_, p))) = (field.as_deref(), cur.as_mut()) {
                    let text = t
                        .decode()
                        .map(std::borrow::Cow::into_owned)
                        .unwrap_or_default();
                    match f {
                        b"ele" => p.elevation = text.trim().parse().ok(),
                        b"time" => p.time = parse_time(&text),
                        b"name" => p.name = Some(text),
                        _ => {}
                    }
                }
            }
            Event::End(ref e) => {
                let name = local(e.name().as_ref()).to_vec();
                match name.as_slice() {
                    b"ele" | b"time" | b"name" => field = None,
                    b"wpt" | b"trkpt" | b"rtept" => {
                        if let Some((kind, p)) = cur.take() {
                            count += 1;
                            push(&mut gpx, &mut segment, &mut route, &kind, p);
                        }
                    }
                    b"trkseg" => gpx
                        .track_segments
                        .extend(segment.take().filter(|s| !s.is_empty())),
                    b"rte" => gpx.routes.extend(route.take().filter(|s| !s.is_empty())),
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if count > MAX_POINTS {
            return Err(GpxError(format!("more than {MAX_POINTS} points")));
        }
        buf.clear();
    }
    if !seen_root {
        return Err(GpxError("no <gpx> element".into()));
    }
    Ok(gpx)
}

fn push(
    gpx: &mut Gpx,
    segment: &mut Option<Vec<GpxPoint>>,
    route: &mut Option<Vec<GpxPoint>>,
    kind: &[u8],
    p: GpxPoint,
) {
    match kind {
        b"wpt" => gpx.waypoints.push(p),
        b"trkpt" => {
            if let Some(s) = segment.as_mut() {
                s.push(p);
            }
        }
        _ => {
            if let Some(r) = route.as_mut() {
                r.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waypoints_tracks_and_routes() {
        let xml = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="t" xmlns="http://www.topografix.com/GPX/1/1">
  <wpt lat="60.17" lon="24.94"><ele>12.5</ele><name>Home</name><time>2021-03-04T09:00:00Z</time></wpt>
  <trk><name>Run</name><trkseg>
    <trkpt lat="60.1" lon="24.9"><time>2021-03-04T10:00:00Z</time></trkpt>
    <trkpt lat="60.2" lon="24.8"/>
  </trkseg><trkseg></trkseg></trk>
  <rte><rtept lat="61" lon="25"><name>A</name></rtept><rtept lat="62" lon="26"/></rte>
</gpx>"#;
        let g = parse(xml).unwrap();
        assert_eq!(g.waypoints.len(), 1);
        assert_eq!(g.waypoints[0].name.as_deref(), Some("Home"));
        assert_eq!(g.waypoints[0].elevation, Some(12.5));
        assert_eq!(g.waypoints[0].time, Some(1_614_848_400));
        assert_eq!(g.track_segments.len(), 1, "empty segments are dropped");
        assert_eq!(g.track_segments[0].len(), 2);
        assert_eq!(g.routes[0][0].name.as_deref(), Some("A"));
        assert!(parse(b"<kml/>").is_err());
    }
}
