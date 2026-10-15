//! Timeline events from case-database artifacts (Autopsy creates these
//! when artifacts are posted: web history, downloads, cookies, bookmarks,
//! searches, form autofill, EXIF, …).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pm_case::Case;
use pm_casedb::catalog::artifact_type_id;
use pm_casedb::{AttributeRecord, Value};
use pm_timeline::{Event, EventType, FileCategory, Subject};

use crate::model::DataSource;

/// Artifact type → event type, the time attributes to use (first present
/// wins) and the attributes making up the description.
const MAPPING: &[(&str, EventType, &[&str], &[&str])] = &[
    (
        "TSK_WEB_HISTORY",
        EventType::WebHistory,
        &["TSK_DATETIME_ACCESSED"],
        &["TSK_URL", "TSK_TITLE"],
    ),
    (
        "TSK_WEB_DOWNLOAD",
        EventType::WebDownloads,
        &["TSK_DATETIME_ACCESSED"],
        &["TSK_PATH", "TSK_URL"],
    ),
    (
        "TSK_WEB_COOKIE",
        EventType::WebCookies,
        &["TSK_DATETIME_ACCESSED", "TSK_DATETIME_CREATED"],
        &["TSK_URL", "TSK_NAME", "TSK_VALUE"],
    ),
    (
        "TSK_WEB_BOOKMARK",
        EventType::WebBookmarks,
        &["TSK_DATETIME_CREATED"],
        &["TSK_URL", "TSK_TITLE"],
    ),
    (
        "TSK_WEB_SEARCH_QUERY",
        EventType::WebSearches,
        &["TSK_DATETIME_ACCESSED"],
        &["TSK_TEXT", "TSK_DOMAIN", "TSK_PROG_NAME"],
    ),
    (
        "TSK_WEB_FORM_AUTOFILL",
        EventType::WebFormAutofill,
        &["TSK_DATETIME_ACCESSED", "TSK_DATETIME_CREATED"],
        &["TSK_NAME", "TSK_VALUE"],
    ),
    (
        "TSK_METADATA_EXIF",
        EventType::Exif,
        &["TSK_DATETIME_CREATED"],
        &["TSK_DEVICE_MAKE", "TSK_DEVICE_MODEL"],
    ),
];

const PAGE: usize = 5_000;

/// Where a case file lives among the browsed data sources.
type Location = Option<(u32, u32, u64, bool)>;

/// Loads artifact events; subjects point at each artifact's source file
/// (so the viewer shows it) and carry the event description as the path.
pub(crate) fn load(case: &Arc<Case>, sources: &[DataSource]) -> (Vec<Event>, Vec<Subject>) {
    let db = case.db();
    let mut events = Vec::new();
    let mut subjects = Vec::new();
    let mut locations: HashMap<i64, Location> = HashMap::new();
    // Custom types (e.g. PM_EVENT_LOG) are registered in the case database.
    let custom = db.artifact_type_names().unwrap_or_default();
    for (ty, kind, time_attrs, desc_attrs) in MAPPING {
        let Some(type_id) =
            artifact_type_id(ty).or_else(|| custom.iter().find(|t| t.1 == *ty).map(|t| t.0))
        else {
            continue;
        };
        let mut after = 0;
        loop {
            let Ok(page) = db.artifacts_of_type(type_id, None, after, PAGE) else {
                break;
            };
            let Some(last) = page.last() else { break };
            after = last.id;
            for art in page {
                let Ok(attrs) = db.attributes(art.id) else {
                    continue;
                };
                let Some(time) = time_attrs.iter().find_map(|t| time_of(&attrs, t)) else {
                    continue;
                };
                let loc = art.file_id.and_then(|fid| {
                    *locations
                        .entry(fid)
                        .or_insert_with(|| locate(case, sources, fid))
                });
                let (ds, fs, inode, deleted) = loc.unwrap_or((u32::MAX, 0, 0, false));
                let desc: Vec<String> = desc_attrs
                    .iter()
                    .filter_map(|a| text_of(&attrs, a))
                    .filter(|s| !s.is_empty())
                    .collect();
                let idx = subjects.len() as u32;
                subjects.push(Subject {
                    ds,
                    fs,
                    inode,
                    path: Arc::from(desc.join(" — ")),
                    deleted,
                    size: 0,
                    category: FileCategory::Other,
                });
                events.push(Event {
                    time,
                    kind: *kind,
                    subject: idx,
                });
            }
        }
    }
    (events, subjects)
}

fn time_of(attrs: &[AttributeRecord], name: &str) -> Option<i64> {
    attrs.iter().find_map(|a| match (&a.value, a.name == name) {
        (Value::DateTime(t), true) if *t != 0 => Some(*t),
        _ => None,
    })
}

fn text_of(attrs: &[AttributeRecord], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.value.display())
}

/// The browsed data source / file system index of a case file.
fn locate(case: &Case, sources: &[DataSource], file_id: i64) -> Location {
    let db = case.db();
    let f = db.file(file_id).ok()??;
    let ds = db.data_source(f.ds_id).ok()??;
    let fs_rec = db.file_system(f.fs_id?).ok()??;
    let first = PathBuf::from(ds.paths.first()?);
    let src = sources
        .iter()
        .find(|s| s.evidence.image.info.paths.first() == Some(&first))?;
    let fs_idx = src
        .evidence
        .filesystems
        .iter()
        .position(|x| x.offset == fs_rec.byte_offset)?;
    Some((
        u32::try_from(src.id).ok()?,
        u32::try_from(fs_idx).ok()?,
        f.inode?,
        f.deleted(),
    ))
}

#[cfg(test)]
mod tests {
    use pm_casedb::NewArtifact;
    use pm_casedb::catalog::attribute_type_id;
    use pm_timeline::{Filter, Timeline};

    use super::*;

    /// A case (with the demo disk as its data source) holding `arts`:
    /// `(artifact type, [(attribute, value)])`.
    fn case_with(arts: &[(&str, Vec<(&str, Value)>)]) -> (tempfile::TempDir, Arc<Case>) {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("demo.dd");
        std::fs::write(&img, pm_testkit::demo::demo_disk()).unwrap();
        let case = Case::create(dir.path().join("case"), pm_case::CaseDetails::default()).unwrap();
        let s = pm_case::add_image(
            &case,
            &img,
            &pm_case::LoadOptions::default(),
            &pm_case::LoadProgress::default(),
        )
        .unwrap();
        let custom = case.db().artifact_type_names().unwrap();
        for (ty, attrs) in arts {
            let type_id = artifact_type_id(ty)
                .or_else(|| custom.iter().find(|t| t.1 == *ty).map(|t| t.0))
                .unwrap_or_else(|| panic!("{ty}"));
            case.db()
                .add_artifact(&NewArtifact {
                    type_id,
                    ds_id: s.ds_id,
                    file_id: None,
                    source_artifact_id: None,
                    score: None,
                    conclusion: None,
                    configuration: None,
                    justification: None,
                    created_at: 0,
                    attributes: attrs
                        .iter()
                        .map(|(n, v)| {
                            (
                                attribute_type_id(n).unwrap_or_else(|| panic!("{n}")),
                                v.clone(),
                                "test".to_owned(),
                            )
                        })
                        .collect(),
                })
                .unwrap();
        }
        (dir, Arc::new(case))
    }

    fn kinds(case: &Arc<Case>) -> Vec<EventType> {
        let (events, subjects) = load(case, &[]);
        let t = Timeline::new(events, subjects, Vec::new());
        let (s, e) = t.span().unwrap();
        t.list(s, e, &Filter::default())
            .into_iter()
            .flat_map(|r| r.kinds)
            .collect()
    }

    #[test]
    fn web_artifacts_become_events() {
        let t = |s: i64| Value::DateTime(1_093_000_000 + s);
        let text = |s: &str| Value::Text(s.into());
        let (_dir, case) = case_with(&[
            (
                "TSK_WEB_COOKIE",
                vec![
                    ("TSK_DATETIME_CREATED", t(1)),
                    ("TSK_URL", text("example.com")),
                ],
            ),
            (
                "TSK_WEB_SEARCH_QUERY",
                vec![
                    ("TSK_DATETIME_ACCESSED", t(2)),
                    ("TSK_TEXT", text("forensics")),
                ],
            ),
            (
                "TSK_WEB_BOOKMARK",
                vec![
                    ("TSK_DATETIME_CREATED", t(3)),
                    ("TSK_URL", text("https://example.com")),
                ],
            ),
        ]);
        assert_eq!(
            kinds(&case),
            [
                EventType::WebCookies,
                EventType::WebSearches,
                EventType::WebBookmarks
            ]
        );
    }
}
