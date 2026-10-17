//! Extension Mismatch Detector (Autopsy `modules/fileextmismatch`,
//! inventory 03 §2.4).
//!
//! Flags files whose extension is not one of those allowed for their MIME
//! type: a `KAAMOS_EXT_MISMATCH_DETECTED` result with score Likely Notable,
//! justification "File has MIME type of <type>" and no attributes. Only
//! MIME types listed in the configuration are checked (Autopsy's default
//! `mismatch_config.xml`, embedded verbatim, or the job's own map).
//!
//! Skipped, as in Autopsy: known files (when "Skip known files" is on),
//! unallocated space, slack, everything that is not a regular file, and
//! files whose name or metadata is unallocated. Check modes: all types,
//! all except `text/plain` files with an extension, or only multimedia,
//! executable and document types (the default).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use pm_casedb::{Known, Origin, Significance};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use serde::{Deserialize, Serialize};

use crate::file_type::{default_detector, mime_or_detect};
use crate::util::Art;

pub const NAME: &str = "Extension Mismatch Detector";

/// Autopsy's default configuration (`mismatch_config.xml`, Apache-2.0,
/// Basis Technology Corp.).
pub const DEFAULT_CONFIG_XML: &str = include_str!("../data/mismatch_config.xml");

/// MIME type → allowed extensions (lower case, without the dot). A type
/// mapped to `None` is listed but not checked (an empty `<signature>`).
pub type MismatchMap = BTreeMap<String, Option<BTreeSet<String>>>;

/// `MEDIA_AND_EXE_MIME_TYPES`: the types checked in the default mode.
pub const MEDIA_AND_EXE_MIME_TYPES: [&str; 28] = [
    "image/bmp",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/tiff",
    "image/x-ms-bmp",
    "application/dos-exe",
    "application/exe",
    "application/x-dosexec",
    "application/x-exe",
    "application/x-msdownload",
    "application/msword",
    "application/pdf",
    "application/rtf",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.text",
    "application/x-msoffice",
    "application/x-ooxml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.openxmlformats-officedocument.presentationml.template",
    "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
];

/// Which files are compared (`FileExtMismatchDetectorModuleSettings.CHECK_TYPE`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckType {
    /// "Check all file types".
    All,
    /// "Check all file types except text files".
    NoTextFiles,
    /// "Check only multimedia and executable files".
    #[default]
    OnlyMediaAndExe,
}

/// Per-job settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtMismatchSettings {
    /// "Skip files without extensions".
    #[serde(default = "yes")]
    pub skip_files_without_extension: bool,
    /// "Skip known files".
    #[serde(default = "yes")]
    pub skip_known_files: bool,
    #[serde(default)]
    pub check_type: CheckType,
    /// The MIME type → extensions map (Autopsy's global "File Extension
    /// Mismatch" options); `None` uses the default configuration.
    #[serde(default)]
    pub mismatch_map: Option<MismatchMap>,
}

fn yes() -> bool {
    true
}

impl Default for ExtMismatchSettings {
    fn default() -> Self {
        Self {
            skip_files_without_extension: true,
            skip_known_files: true,
            check_type: CheckType::OnlyMediaAndExe,
            mismatch_map: None,
        }
    }
}

/// Invalid mismatch configuration XML.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("Error loading config file: {0}")]
    Xml(String),
    #[error("Error loading config file: invalid file format (no signature).")]
    NoSignature,
}

/// Reads a `mismatch_config.xml` (`FileExtMismatchSettings.readXmlSettings`):
/// every `<signature mimetype=…>` element with its `<ext>` children; a
/// later signature for the same type replaces an earlier one.
pub fn parse_config(xml: &str) -> Result<MismatchMap, ConfigError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| ConfigError::Xml(e.to_string()))?;
    let mut map = MismatchMap::new();
    let mut any = false;
    for sig in doc
        .root_element()
        .descendants()
        .filter(|n| n.has_tag_name("signature"))
    {
        any = true;
        let mime = sig.attribute("mimetype").unwrap_or_default().to_owned();
        let exts: BTreeSet<String> = sig
            .descendants()
            .filter(|n| n.has_tag_name("ext"))
            // `getTextContent()`: all descendant text.
            .map(|e| {
                e.descendants()
                    .filter(roxmltree::Node::is_text)
                    .filter_map(|t| t.text())
                    .collect::<String>()
            })
            .collect();
        map.insert(mime, (!exts.is_empty()).then_some(exts));
    }
    if !any {
        return Err(ConfigError::NoSignature);
    }
    Ok(map)
}

/// Writes a map in `mismatch_config.xml` form.
pub fn write_config(map: &MismatchMap) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<mismatch_config>\n",
    );
    for (mime, exts) in map {
        out.push_str(" <signature mimetype=\"");
        out.push_str(&xml_escape(mime));
        out.push_str("\">\n");
        for e in exts.iter().flatten() {
            out.push_str("  <ext>");
            out.push_str(&xml_escape(e));
            out.push_str("</ext>\n");
        }
        out.push_str(" </signature>\n");
    }
    out.push_str("</mismatch_config>\n");
    out
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

/// Autopsy's default map.
pub fn default_map() -> &'static MismatchMap {
    static MAP: LazyLock<MismatchMap> =
        LazyLock::new(|| parse_config(DEFAULT_CONFIG_XML).unwrap_or_default());
    &MAP
}

/// `compareSigTypeToExt` for a file with MIME type `mime` and lower-case
/// extension `ext`.
pub fn is_mismatch(
    settings: &ExtMismatchSettings,
    map: &MismatchMap,
    mime: &str,
    ext: &str,
) -> bool {
    if settings.skip_files_without_extension && ext.is_empty() {
        return false;
    }
    match settings.check_type {
        CheckType::All => {}
        CheckType::NoTextFiles => {
            if !ext.is_empty() && mime == "text/plain" {
                return false;
            }
        }
        CheckType::OnlyMediaAndExe => {
            if !MEDIA_AND_EXE_MIME_TYPES.contains(&mime) {
                return false;
            }
        }
    }
    match map.get(mime) {
        Some(Some(allowed)) => !allowed.contains(ext),
        Some(None) | None => false,
    }
}

#[derive(Debug, Default)]
pub struct ExtMismatchFactory;

impl ModuleFactory for ExtMismatchFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "ext_mismatch",
            name: NAME,
            description: "Flags files that have a non-standard extension based on their file type.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(ExtMismatchSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: ExtMismatchSettings = if settings.is_null() {
            ExtMismatchSettings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("Could not read settings. ({e})")))?
        };
        let map = match &settings.mismatch_map {
            Some(m) => m.clone(),
            None => default_map().clone(),
        };
        Ok(Arc::new(JobRef(Arc::new(Job {
            settings,
            map,
            files: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
        }))))
    }
}

struct Job {
    settings: ExtMismatchSettings,
    map: MismatchMap,
    files: AtomicU64,
    nanos: AtomicU64,
}

struct JobRef(Arc<Job>);

impl ModuleJob for JobRef {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker {
            job: Arc::clone(&self.0),
        }))
    }

    /// "File Extension Mismatch Results", posted when any file was
    /// compared.
    fn finish(&self, ctx: &JobContext) {
        let files = self.0.files.load(Ordering::Relaxed);
        if files == 0 {
            return;
        }
        let ms = self.0.nanos.load(Ordering::Relaxed) / 1_000_000;
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "File Extension Mismatch Results").details(
                format!("{NAME}\nTotal Processing Time: {ms} ms\nTotal Files Processed: {files}"),
            ),
        );
    }
}

struct Worker {
    job: Arc<Job>,
}

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let job = &self.job;
        if job.settings.skip_known_files && file.known() == Known::Known {
            return Ok(());
        }
        if matches!(file.rec.origin, Origin::UnallocBlocks | Origin::Slack) || !file.is_file() {
            return Ok(());
        }
        if !file.rec.meta_alloc || !file.rec.name_alloc {
            return Ok(());
        }
        let start = Instant::now();
        let ext = file.rec.extension.clone();
        // No detection for a file the extension rule already skips.
        let mismatch_mime = if job.settings.skip_files_without_extension && ext.is_empty() {
            None
        } else {
            mime_or_detect(file, default_detector())
                .filter(|mime| is_mismatch(&job.settings, &job.map, mime, &ext))
        };
        job.files.fetch_add(1, Ordering::Relaxed);
        job.nanos.fetch_add(
            u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if let Some(mime) = mismatch_mime {
            post(ctx, file.rec.id, &mime);
        }
        Ok(())
    }
}

fn post(ctx: &JobContext, file_id: i64, mime: &str) {
    Art::result(
        ctx,
        NAME,
        "KAAMOS_EXT_MISMATCH_DETECTED",
        file_id,
        Significance::LikelyNotable,
    )
    .justification(format!("File has MIME type of {mime}"))
    .post(ctx, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps, reason = "compared with map values")]
    fn set(exts: &[&str]) -> Option<BTreeSet<String>> {
        Some(exts.iter().map(|&e| e.to_owned()).collect())
    }

    #[test]
    fn default_configuration() {
        let m = default_map();
        assert_eq!(m.len(), 65);
        assert_eq!(m["application/pdf"], set(&["pdf"]));
        assert_eq!(m["application/rtf"], set(&["doc", "rtf"]));
        let jpeg = m["image/jpeg"].as_ref().unwrap();
        assert!(jpeg.contains("jpg") && jpeg.contains("jpeg"));
        assert!(m.values().all(Option::is_some));
    }

    #[test]
    fn config_xml_round_trip_and_errors() {
        let xml = r#"<?xml version="1.0"?><mismatch_config>
            <signature mimetype="a/b"><ext>x</ext><ext>y</ext></signature>
            <signature mimetype="c/d"/>
            <signature mimetype="a/b"><ext>z</ext></signature>
            <signature mimetype="e/&amp;"><ext>q&lt;</ext></signature>
        </mismatch_config>"#;
        let m = parse_config(xml).unwrap();
        // The later "a/b" wins; an empty signature is listed but unchecked.
        assert_eq!(m["a/b"], set(&["z"]));
        assert_eq!(m["c/d"], None);
        assert_eq!(m["e/&"], set(&["q<"]));
        assert_eq!(parse_config(&write_config(&m)).unwrap(), m);
        assert_eq!(
            parse_config(&write_config(default_map())).unwrap(),
            *default_map()
        );
        assert_eq!(
            parse_config("<mismatch_config/>"),
            Err(ConfigError::NoSignature)
        );
        assert!(matches!(parse_config("<a>"), Err(ConfigError::Xml(_))));
    }

    /// `compareSigTypeToExt` decision table.
    #[test]
    fn decision_table() {
        use CheckType::{All, NoTextFiles, OnlyMediaAndExe as Media};
        let map = default_map();
        let s = |check_type, skip_no_ext| ExtMismatchSettings {
            check_type,
            skip_files_without_extension: skip_no_ext,
            ..ExtMismatchSettings::default()
        };
        let cases: &[(CheckType, bool, &str, &str, bool)] = &[
            // Allowed and disallowed extensions of a checked type.
            (Media, true, "image/jpeg", "jpg", false),
            (Media, true, "image/jpeg", "txt", true),
            (Media, true, "application/pdf", "pdf", false),
            (Media, true, "application/pdf", "doc", true),
            (Media, true, "application/x-msdownload", "exe", false),
            // No extension: skipped, or a mismatch when not skipped.
            (Media, true, "application/pdf", "", false),
            (Media, false, "application/pdf", "", true),
            // Types outside the media/executable set are not checked …
            (Media, true, "text/html", "txt", false),
            (Media, true, "application/zip", "jar", false),
            // … unless every type is checked.
            (All, true, "text/html", "txt", true),
            (All, true, "application/zip", "doc", true),
            (All, true, "text/plain", "jpg", true),
            (All, true, "text/plain", "txt", false),
            // Text files are skipped when they have an extension.
            (NoTextFiles, true, "text/plain", "jpg", false),
            (NoTextFiles, false, "text/plain", "", true),
            (NoTextFiles, true, "text/html", "txt", true),
            // Types missing from the map are never mismatches.
            (All, true, "application/x-unknown", "abc", false),
        ];
        for &(ct, skip, mime, ext, want) in cases {
            assert_eq!(
                is_mismatch(&s(ct, skip), map, mime, ext),
                want,
                "{ct:?} skip={skip} {mime} .{ext}"
            );
        }
        // An unchecked (empty) signature and an explicitly empty set.
        let mut m = MismatchMap::new();
        m.insert("a/b".into(), None);
        m.insert("c/d".into(), Some(BTreeSet::new()));
        assert!(!is_mismatch(&s(All, true), &m, "a/b", "x"));
        assert!(is_mismatch(&s(All, true), &m, "c/d", "x"));
    }

    proptest::proptest! {
        /// Arbitrary (mostly broken) configuration files never panic.
        #[test]
        fn config_parser_never_panics(s in "(<[a-z_]{0,12}( mimetype=\"[a-z/&;]{0,8}\")?/?>|[a-z<>&;/]{0,6}){0,24}") {
            let _ = parse_config(&s);
        }
    }

    #[test]
    fn settings_defaults_and_serde() {
        let d = ExtMismatchSettings::default();
        assert!(d.skip_files_without_extension && d.skip_known_files);
        assert_eq!(d.check_type, CheckType::OnlyMediaAndExe);
        let back: ExtMismatchSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(back, d);
        let v = serde_json::to_value(ExtMismatchSettings {
            check_type: CheckType::NoTextFiles,
            ..d
        })
        .unwrap();
        assert_eq!(v["check_type"], "no_text_files");
    }
}
