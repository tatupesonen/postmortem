//! `TSK_METADATA` from extractor metadata (Autopsy
//! `KeywordSearchIngestModule.createMetadataArtifact`, inventory 05
//! §1.4.3).
//!
//! Each attribute takes the value of the earliest key in its list that is
//! present. Values that are empty or start with a space are dropped. Dates
//! (`yyyy-MM-ddTHH:mm:ss…Z`) are read as UTC; Autopsy reads them in the
//! examiner's time zone (a bug), and drops what does not parse.

use pm_casedb::Value;
use pm_text::Metadata;

/// Attribute type, value kind, metadata keys in priority order (Tika's
/// names; `pm-text` produces the first of each list).
const MAPPING: &[(&str, bool, &[&str])] = &[
    (
        "TSK_DATETIME_MODIFIED",
        true,
        &[
            "Last-Save-Date",
            "dcterms:modified",
            "fs:modified",
            "pdf:docinfo:modified",
            "xmp:ModifyDate",
        ],
    ),
    (
        "TSK_USER_ID",
        false,
        &["Last-Author", "meta:last-author", "dcterms:modifier"],
    ),
    (
        "TSK_DATETIME_CREATED",
        true,
        &[
            "Creation-Date",
            "dcterms:created",
            "fs:created",
            "meta:creation-date",
            "pdf:docinfo:created",
            "xmp:CreateDate",
        ],
    ),
    (
        "TSK_ORGANIZATION",
        false,
        &["Company", "dc:publisher", "extended-properties:Company"],
    ),
    (
        "TSK_OWNER",
        false,
        &[
            "Author",
            "dc:creator",
            "meta:initial-author",
            "meta:author",
            "pdf:docinfo:creator",
        ],
    ),
    (
        "TSK_PROG_NAME",
        false,
        &[
            "Application-Name",
            "Producer",
            "extended-properties:Application",
        ],
    ),
    (
        "TSK_LAST_PRINTED_DATETIME",
        true,
        &["Last-Printed", "cp:lastPrinted"],
    ),
    (
        "TSK_DESCRIPTION",
        false,
        &["Title", "dc:title", "pdf:docinfo:title"],
    ),
    ("TSK_VERSION", false, &["pdf:PDFVersion", "cp:version"]),
];

/// The `TSK_METADATA` attributes for `meta`: `(attribute type name,
/// value)`, in Autopsy's order.
pub fn metadata_attributes(meta: &Metadata) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    for &(attr, is_date, keys) in MAPPING {
        let Some(v) = keys.iter().find_map(|k| meta.get(*k)) else {
            continue;
        };
        if v.is_empty() || v.starts_with(' ') {
            continue;
        }
        if is_date {
            if let Some(t) = parse_date(v) {
                out.push((attr, Value::DateTime(t)));
            }
        } else {
            out.push((attr, Value::Text(v.clone())));
        }
    }
    out
}

/// `yyyy-MM-dd HH:mm:ss` at the start of the value (`T` → space, `Z`
/// removed; trailing text such as fractions ignored), as UTC seconds.
fn parse_date(v: &str) -> Option<i64> {
    let s: String = v.replace('T', " ").replace('Z', "");
    let head = s.get(..19)?;
    let dt = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S", head).ok()?;
    let ts = dt.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
    Some(ts.as_second())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_like_autopsy() {
        let mut m = Metadata::new();
        m.insert("Author".into(), "Jane".into());
        m.insert("dc:creator".into(), "ignored".into());
        m.insert("Last-Author".into(), "Bob".into());
        m.insert("Creation-Date".into(), "2020-01-02T03:04:05Z".into());
        m.insert("Last-Save-Date".into(), "2021-02-03T04:05:06.789Z".into());
        m.insert("Last-Printed".into(), "not a date".into());
        m.insert("Title".into(), " leading space".into());
        m.insert("Company".into(), String::new());
        m.insert("Producer".into(), "Writer".into());
        m.insert("pdf:PDFVersion".into(), "1.7".into());
        let got = metadata_attributes(&m);
        assert_eq!(
            got,
            [
                ("TSK_DATETIME_MODIFIED", Value::DateTime(1_612_325_106)),
                ("TSK_USER_ID", Value::Text("Bob".into())),
                ("TSK_DATETIME_CREATED", Value::DateTime(1_577_934_245)),
                ("TSK_OWNER", Value::Text("Jane".into())),
                ("TSK_PROG_NAME", Value::Text("Writer".into())),
                ("TSK_VERSION", Value::Text("1.7".into())),
            ]
        );
        assert!(metadata_attributes(&Metadata::new()).is_empty());
        assert_eq!(parse_date("2020-01-02"), None);
    }
}
