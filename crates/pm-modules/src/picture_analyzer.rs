//! Picture Analyzer (Autopsy `modules/pictureanalyzer`, inventory 03 §2.5).
//!
//! For JPEG, TIFF and WAV files it reads the EXIF metadata Autopsy's
//! `EXIFProcessor` extracts and posts a `TSK_METADATA_EXIF` result (score
//! None) with:
//! * `TSK_DATETIME_CREATED` ← `DateTimeOriginal`, interpreted in the data
//!   source's time zone unless the value carries an offset;
//! * `TSK_GEO_LATITUDE` / `TSK_GEO_LONGITUDE` ← the GPS position (decimal
//!   degrees);
//! * `TSK_GEO_ALTITUDE` ← `GPSAltitude` (infinity becomes 0; the altitude
//!   reference is ignored, as in Autopsy);
//! * `TSK_DEVICE_MODEL` / `TSK_DEVICE_MAKE` ← IFD0 `Model` / `Make`.
//!
//! Alongside it, a `TSK_USER_CONTENT_SUSPECTED` result (score Unknown,
//! comment "EXIF metadata data exists for this file.").
//!
//! HEIC/HEIF conversion (Autopsy's `HEICProcessor`) is not implemented: it
//! needs an HEVC decoder.

use std::sync::Arc;

use pm_casedb::{Known, Significance, Value};
use pm_ingest::{
    FileModule, IngestFile, JobContext, ModuleError, ModuleFactory, ModuleInfo, ModuleJob,
};

use crate::exif;
use crate::util::Art;

pub const NAME: &str = "Picture Analyzer";
const USER_CONTENT_COMMENT: &str = "EXIF metadata data exists for this file.";

/// MIME types handled by the EXIF processor.
const EXIF_MIME_TYPES: [&str; 3] = ["audio/x-wav", "image/jpeg", "image/tiff"];

#[derive(Debug, Default)]
pub struct PictureAnalyzerFactory;

impl ModuleFactory for PictureAnalyzerFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "picture_analyzer",
            name: NAME,
            description: "Performs general analysis on picture files, including extracting EXIF metadata and converting between formats.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn start(
        &self,
        ctx: &JobContext,
        _settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let tz = ctx
            .case
            .db()
            .data_source(ctx.ds_id)?
            .and_then(|d| jiff::tz::TimeZone::get(&d.timezone).ok())
            .unwrap_or(jiff::tz::TimeZone::UTC);
        Ok(Arc::new(Job { tz }))
    }
}

struct Job {
    /// Time zone for EXIF dates without an offset (Autopsy: the image's
    /// time zone).
    tz: jiff::tz::TimeZone,
}

impl ModuleJob for Job {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            tz: self.tz.clone(),
        }))
    }
}

struct Worker {
    tz: jiff::tz::TimeZone,
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        if !file.is_file() || file.known() == Known::Known || file.is_unallocated_space() {
            return Ok(());
        }
        let Some(mime) = mime_of(file) else {
            return Ok(());
        };
        if !EXIF_MIME_TYPES
            .iter()
            .any(|m| m.eq_ignore_ascii_case(&mime))
        {
            return Ok(());
        }
        let src = file.content()?;
        let data = if mime.eq_ignore_ascii_case("image/jpeg") {
            exif::read_jpeg(&*src)?
        } else if mime.eq_ignore_ascii_case("image/tiff") {
            exif::read_tiff(&*src)?
        } else {
            // metadata-extractor reads no Exif directories from WAV files.
            return Ok(());
        };
        if ctx.is_cancelled() {
            return Ok(());
        }
        let attrs = attributes(&data, &self.tz);
        if attrs.is_empty() {
            return Ok(());
        }
        let mut art = Art::result(
            ctx,
            NAME,
            "TSK_METADATA_EXIF",
            file.rec.id,
            Significance::None,
        );
        let mut key = Vec::new();
        for (name, value) in attrs {
            art = art.value(name, value);
            key.push(name);
        }
        art.post(ctx, &key);
        Art::result(
            ctx,
            NAME,
            "TSK_USER_CONTENT_SUSPECTED",
            file.rec.id,
            Significance::Unknown,
        )
        .text("TSK_COMMENT", USER_CONTENT_COMMENT)
        .post(ctx, &["TSK_COMMENT"]);
        Ok(())
    }
}

/// The file's MIME type, or a minimal signature check when File Type
/// Identification has not typed it (Autopsy's module runs the detector
/// itself in that case).
fn mime_of(file: &IngestFile) -> Option<String> {
    if let Some(m) = file.mime() {
        return Some(m.split(';').next().unwrap_or(m).trim().to_owned());
    }
    let h = file.head();
    let m = if h.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if h.starts_with(b"II*\0") || h.starts_with(b"MM\0*") {
        "image/tiff"
    } else if h.starts_with(b"RIFF") && h.get(8..12) == Some(b"WAVE") {
        "audio/x-wav"
    } else {
        return None;
    };
    Some(m.to_owned())
}

/// Autopsy's attributes, in its order.
fn attributes(d: &exif::ExifData, tz: &jiff::tz::TimeZone) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if let Some(when) = d
        .sub_ifd
        .as_ref()
        .and_then(|s| s.date_time_original.as_deref())
        .and_then(|s| exif_timestamp(s, tz))
    {
        out.push(("TSK_DATETIME_CREATED", Value::DateTime(when)));
    }
    if let Some(gps) = &d.gps {
        if let Some((lat, lon)) = gps
            .geo_location()
            .filter(|(a, b)| a.is_finite() && b.is_finite())
        {
            out.push(("TSK_GEO_LATITUDE", Value::Double(lat)));
            out.push(("TSK_GEO_LONGITUDE", Value::Double(lon)));
        }
        if let Some(alt) = gps.altitude {
            let mut alt = alt.to_f64();
            if alt.is_infinite() {
                alt = 0.0;
            }
            if alt.is_finite() {
                out.push(("TSK_GEO_ALTITUDE", Value::Double(alt)));
            }
        }
    }
    if let Some(ifd0) = &d.ifd0 {
        if let Some(model) = ifd0.model.as_deref().filter(|s| !s.trim().is_empty()) {
            out.push(("TSK_DEVICE_MODEL", Value::Text(model.to_owned())));
        }
        if let Some(make) = ifd0.make.as_deref().filter(|s| !s.trim().is_empty()) {
            out.push(("TSK_DEVICE_MAKE", Value::Text(make.to_owned())));
        }
    }
    out
}

/// Unix seconds of an EXIF date string in `tz` (or its own offset).
pub fn exif_timestamp(s: &str, tz: &jiff::tz::TimeZone) -> Option<i64> {
    let (dt, offset) = exif::parse_exif_date(s)?;
    let ts = match offset {
        Some(secs) => {
            let off = jiff::tz::Offset::from_seconds(secs).ok()?;
            off.to_timestamp(dt).ok()?
        }
        None => tz.to_ambiguous_zoned(dt).compatible().ok()?.timestamp(),
    };
    Some(ts.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_use_zone_or_offset() {
        let hel = jiff::tz::TimeZone::get("Europe/Helsinki").unwrap();
        // 2021-03-04 10:20:30 EET (+02:00) = 08:20:30 UTC.
        assert_eq!(
            exif_timestamp("2021:03:04 10:20:30", &hel),
            Some(1_614_846_030)
        );
        assert_eq!(
            exif_timestamp("2021:03:04 10:20:30", &jiff::tz::TimeZone::UTC),
            Some(1_614_853_230)
        );
        assert_eq!(
            exif_timestamp("2021:03:04 10:20:30+02:00", &jiff::tz::TimeZone::UTC),
            Some(1_614_846_030)
        );
        assert_eq!(exif_timestamp("0000:00:00 00:00:00", &hel), None);
    }

    #[test]
    fn attribute_set_and_order() {
        let d = exif::ExifData {
            ifd0: Some(exif::Ifd0 {
                make: Some("Canon".into()),
                model: Some("  ".into()),
            }),
            sub_ifd: Some(exif::SubIfd {
                date_time_original: Some("2021:03:04 10:20:30".into()),
            }),
            gps: Some(exif::Gps {
                altitude: Some(exif::Rational { num: 7, den: 0 }),
                ..Default::default()
            }),
        };
        let a = attributes(&d, &jiff::tz::TimeZone::UTC);
        let names: Vec<_> = a.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "TSK_DATETIME_CREATED",
                "TSK_GEO_ALTITUDE",
                "TSK_DEVICE_MAKE"
            ]
        );
        assert_eq!(a[1].1, Value::Double(0.0));
    }
}
