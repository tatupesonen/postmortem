//! Case-wide tree nodes: Tags (Autopsy `Tags`), Score (`ScoreContent`)
//! and OS Accounts (`OsAccounts`), with the rows of their result views.

use pm_case::Case;
use pm_casedb::os_accounts::OsAccount;
use pm_casedb::{FileRecord, ObjKind};

use crate::format::DisplayPrefs;
use crate::grid::GridData;
use crate::tags_ui::TagCatalog;

/// Rows per view (these views are not paged yet).
const MAX_ROWS: usize = 20_000;

/// A tag name in use, with its file and result tag counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TagNode {
    pub(crate) id: i64,
    /// `<set>: <name> (Notable)` as Autopsy decorates it.
    pub(crate) label: String,
    pub(crate) files: u64,
    pub(crate) results: u64,
}

/// Counts for the case-wide tree nodes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Summary {
    pub(crate) tags: Vec<TagNode>,
    pub(crate) bad: u64,
    pub(crate) suspicious: u64,
    pub(crate) os_accounts: u64,
    /// Generated reports (the Reports node).
    pub(crate) reports: Vec<pm_casedb::Report>,
}

pub(crate) fn load_summary(case: &Case) -> Result<Summary, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let cat = TagCatalog::load(case)?;
    let mut tags = Vec::new();
    for t in db.tag_names_in_use(None, None).map_err(err)? {
        let (files, results) = db.tag_usage(t.id, None, None).map_err(err)?;
        tags.push(TagNode {
            id: t.id,
            label: cat.decorated(&t),
            files,
            results,
        });
    }
    Ok(Summary {
        tags,
        bad: db.score_count(true, None).map_err(err)?,
        suspicious: db.score_count(false, None).map_err(err)?,
        os_accounts: db.os_account_count(None).map_err(err)?,
        reports: db.reports().map_err(err)?,
    })
}

/// What a row of a case-node view refers to.
#[derive(Debug, Clone)]
pub(crate) enum RowRef {
    /// A file, and the file tag the row shows (Tags › File Tags).
    File {
        file: FileRecord,
        tag_id: Option<i64>,
    },
    /// A result, its source file, and the result tag the row shows.
    Artifact {
        artifact_id: i64,
        file: Option<FileRecord>,
        tag_id: Option<i64>,
    },
    OsAccount(i64),
}

/// Rows of one view and what each refers to.
#[derive(Debug, Clone, Default)]
pub(crate) struct ViewData {
    pub(crate) grid: GridData,
    pub(crate) refs: Vec<RowRef>,
}

fn secs(prefs: &DisplayPrefs, t: Option<(i64, u32)>) -> (String, Option<i64>) {
    let ts = t.and_then(|(s, _)| jiff::Timestamp::from_second(s).ok());
    (prefs.time(ts), t.map(|(s, _)| s))
}

fn hex(v: Option<&[u8]>) -> String {
    v.map(pm_core::decode::to_hex).unwrap_or_default()
}

/// Tags › `<name>` › File Tags: Autopsy's content-tag columns.
pub(crate) fn load_tag_files(
    case: &Case,
    tag_id: i64,
    prefs: &DisplayPrefs,
) -> Result<ViewData, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut v = ViewData {
        grid: GridData::new(vec![
            "File",
            "File Path",
            "Comment",
            "Modified Time",
            "Changed Time",
            "Accessed Time",
            "Created Time",
            "Size",
            "MD5 Hash",
            "User Name",
        ]),
        refs: Vec::new(),
    };
    for t in db.file_tags_named(tag_id, None, 0, MAX_ROWS).map_err(err)? {
        let Some(f) = db.file(t.file_id).map_err(err)? else {
            continue;
        };
        let mut cells = vec![
            (f.name.clone(), None),
            (f.path(), None),
            (t.comment.clone(), None),
        ];
        // FileRecord times are modified, accessed, changed, created;
        // Autopsy's columns are modified, changed, accessed, created.
        for i in [0, 2, 1, 3] {
            cells.push(secs(prefs, f.times.get(i).copied().flatten()));
        }
        cells.push((crate::format::grouped(f.size), i64::try_from(f.size).ok()));
        cells.push((hex(f.md5.as_deref()), None));
        cells.push((t.examiner.clone(), None));
        v.grid.push(cells);
        v.refs.push(RowRef::File {
            file: f,
            tag_id: Some(t.id),
        });
    }
    Ok(v)
}

/// Tags › `<name>` › Result Tags: Autopsy's artifact-tag columns.
pub(crate) fn load_tag_results(case: &Case, tag_id: i64) -> Result<ViewData, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let types = db.artifact_type_names().map_err(err)?;
    let mut v = ViewData {
        grid: GridData::new(vec![
            "Source Name",
            "Source File Path",
            "Result Type",
            "Comment",
            "User Name",
        ]),
        refs: Vec::new(),
    };
    for t in db
        .artifact_tags_named(tag_id, None, 0, MAX_ROWS)
        .map_err(err)?
    {
        let Some(a) = db.artifact(t.artifact_id).map_err(err)? else {
            continue;
        };
        let file = match a.file_id {
            Some(id) => db.file(id).map_err(err)?,
            None => None,
        };
        let ty = types
            .iter()
            .find(|x| x.0 == a.type_id)
            .map_or_else(String::new, |x| x.2.clone());
        v.grid.push(vec![
            (
                file.as_ref().map_or_else(String::new, |f| f.name.clone()),
                None,
            ),
            (
                file.as_ref()
                    .map_or_else(|| "Unavailable".into(), FileRecord::path),
                None,
            ),
            (ty, None),
            (t.comment.clone(), None),
            (t.examiner.clone(), None),
        ]);
        v.refs.push(RowRef::Artifact {
            artifact_id: a.id,
            file,
            tag_id: Some(t.id),
        });
    }
    Ok(v)
}

/// Score › Bad Items / Suspicious Items: files and data artifacts.
pub(crate) fn load_score(case: &Case, bad: bool) -> Result<ViewData, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let types = db.artifact_type_names().map_err(err)?;
    let mut v = ViewData {
        grid: GridData::new(vec!["Name", "Type"]),
        refs: Vec::new(),
    };
    for (kind, id) in db
        .scored_objects(bad, None, (ObjKind::File, 0), MAX_ROWS)
        .map_err(err)?
    {
        match kind {
            ObjKind::File => {
                let Some(f) = db.file(id).map_err(err)? else {
                    continue;
                };
                v.grid
                    .push(vec![(f.name.clone(), None), ("File".into(), None)]);
                v.refs.push(RowRef::File {
                    file: f,
                    tag_id: None,
                });
            }
            ObjKind::Artifact => {
                let Some(a) = db.artifact(id).map_err(err)? else {
                    continue;
                };
                let file = match a.file_id {
                    Some(f) => db.file(f).map_err(err)?,
                    None => None,
                };
                let ty = types
                    .iter()
                    .find(|x| x.0 == a.type_id)
                    .map_or_else(String::new, |x| x.2.clone());
                v.grid.push(vec![
                    (
                        file.as_ref().map_or_else(String::new, |f| f.name.clone()),
                        None,
                    ),
                    (ty, None),
                ]);
                v.refs.push(RowRef::Artifact {
                    artifact_id: a.id,
                    file,
                    tag_id: None,
                });
            }
            ObjKind::OsAccount => {}
        }
    }
    Ok(v)
}

/// The name Autopsy shows for an account: its login, else its address.
pub(crate) fn account_name(a: &OsAccount) -> String {
    a.login_name
        .clone()
        .or_else(|| a.addr.clone())
        .unwrap_or_default()
}

/// OS Accounts: Autopsy's columns.
pub(crate) fn load_os_accounts(case: &Case, prefs: &DisplayPrefs) -> Result<ViewData, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut v = ViewData {
        grid: GridData::new(vec![
            "Name",
            "Login Name",
            "Host",
            "Scope",
            "Realm Name",
            "Creation Time",
        ]),
        refs: Vec::new(),
    };
    for a in db.os_accounts(None, 0, MAX_ROWS).map_err(err)? {
        let realm = db.os_account_realm(a.realm_id).map_err(err)?;
        let hosts = db.hosts_of_os_account(a.id).map_err(err)?.join(", ");
        let created = secs(prefs, a.created_at.map(|t| (t, 0)));
        v.grid.push(vec![
            (account_name(&a), None),
            (a.login_name.clone().unwrap_or_default(), None),
            (hosts, None),
            (
                if realm.scope_host_id.is_some() {
                    "Local"
                } else {
                    "Domain"
                }
                .into(),
                None,
            ),
            (realm.name.clone().unwrap_or_default(), None),
            created,
        ]);
        v.refs.push(RowRef::OsAccount(a.id));
    }
    Ok(v)
}

/// Sections of label/value rows.
pub(crate) type Details = Vec<(&'static str, Vec<(String, String)>)>;

/// The details of one OS account (Autopsy's OS Account content viewer):
/// `(section, [(label, value)])`.
pub(crate) fn account_details(
    case: &Case,
    id: i64,
    prefs: &DisplayPrefs,
) -> Result<Details, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let Some(a) = db.os_account(id).map_err(err)? else {
        return Ok(Vec::new());
    };
    let realm = db.os_account_realm(a.realm_id).map_err(err)?;
    let mut general = vec![
        ("Login".into(), a.login_name.clone().unwrap_or_default()),
        ("Full Name".into(), a.full_name.clone().unwrap_or_default()),
        ("Address".into(), a.addr.clone().unwrap_or_default()),
        (
            "Type".into(),
            a.kind.map_or("Unknown", |k| k.name()).to_owned(),
        ),
        (
            "Status".into(),
            a.status.map_or("Unknown", |s| s.name()).to_owned(),
        ),
        (
            "Creation Date".into(),
            secs(prefs, a.created_at.map(|t| (t, 0))).0,
        ),
    ];
    general.retain(|(_, v)| !v.is_empty());
    let realm_rows = vec![
        ("Realm Name".into(), realm.name.clone().unwrap_or_default()),
        (
            "Realm Address".into(),
            realm.addr.clone().unwrap_or_default(),
        ),
        (
            "Realm Scope".into(),
            if realm.scope_host_id.is_some() {
                "Local"
            } else {
                "Domain"
            }
            .into(),
        ),
        (
            "Realm Confidence".into(),
            format!("{:?}", realm.scope_confidence),
        ),
    ];
    let attrs = db
        .os_account_attributes(id)
        .map_err(err)?
        .into_iter()
        .map(|(attr, _, display)| {
            let value = match attr.value {
                pm_casedb::Value::DateTime(t) => secs(prefs, Some((t, 0))).0,
                other => other.display(),
            };
            (display, value)
        })
        .collect();
    let instances = db
        .os_account_instances(id)
        .map_err(err)?
        .into_iter()
        .map(|i| {
            let ds = db
                .data_source(i.ds_id)
                .ok()
                .flatten()
                .map_or_else(|| i.ds_id.to_string(), |d| d.name);
            (ds, i.kind.name().to_owned())
        })
        .collect();
    Ok(vec![
        ("General", general),
        ("Realm", realm_rows),
        ("Extended attributes", attrs),
        ("Data sources", instances),
    ])
}
