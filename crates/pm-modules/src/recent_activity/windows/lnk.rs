//! Recent documents from shell links (Autopsy `RecentDocumentsByLnk`) and
//! jump lists (Autopsy `ExtractJumpLists`).
//!
//! Autopsy adds each jump-list LNK stream as a derived file and lets the
//! LNK extractor post `KAAMOS_RECENT_OBJECT` on it. Until derived files exist
//! here, the same `KAAMOS_RECENT_OBJECT` is posted on the jump-list file
//! itself, with the `DestList` access time and the application name.

use std::collections::HashSet;

use pm_casedb::catalog::ValueType;
use pm_casedb::{FileRecord, Value};
use pm_ingest::{JobContext, ModuleError};
use pm_lnk::ShellLink;
use pm_lnk::jumplist::{app_id, app_name, parse_automatic, parse_custom};

use super::art::{Batch, NewArt, Types};
use super::{PathLookup, Visits, files_with_ext, read_small};

/// Custom attributes carrying what Autopsy ignores in a link.
struct LinkAttrs {
    target_created: i64,
    target_modified: i64,
    target_accessed: i64,
    machine: i64,
    arguments: i64,
}

impl LinkAttrs {
    fn new(ctx: &JobContext, types: &mut Types) -> Self {
        Self {
            target_created: types.attribute(
                ctx,
                "KAAMOS_TARGET_CREATED",
                "Target Created",
                ValueType::DateTime,
            ),
            target_modified: types.attribute(
                ctx,
                "KAAMOS_TARGET_MODIFIED",
                "Target Modified",
                ValueType::DateTime,
            ),
            target_accessed: types.attribute(
                ctx,
                "KAAMOS_TARGET_ACCESSED",
                "Target Accessed",
                ValueType::DateTime,
            ),
            machine: types.attribute(
                ctx,
                "KAAMOS_LNK_MACHINE_ID",
                "Link Machine ID",
                ValueType::String,
            ),
            arguments: types.attribute(
                ctx,
                "KAAMOS_LNK_ARGUMENTS",
                "Link Arguments",
                ValueType::String,
            ),
        }
    }

    fn apply(&self, a: NewArt, l: &ShellLink) -> NewArt {
        let tracker = l.extra.tracker.as_ref();
        a.custom(self.target_created, l.created_unix().map(Value::DateTime))
            .custom(self.target_modified, l.modified_unix().map(Value::DateTime))
            .custom(self.target_accessed, l.accessed_unix().map(Value::DateTime))
            .custom(
                self.machine,
                tracker
                    .map(|t| t.machine_id.clone())
                    .filter(|m| !m.is_empty())
                    .map(Value::Text),
            )
            .custom(
                self.arguments,
                l.arguments
                    .clone()
                    .filter(|a| !a.is_empty())
                    .map(Value::Text),
            )
            .text_opt(
                "KAAMOS_MAC_ADDRESS",
                tracker.and_then(|t| t.mac_address.clone()),
            )
    }
}

fn crtime(f: &FileRecord) -> Option<i64> {
    f.times.get(3).copied().flatten().map(|t| t.0)
}

/// `KAAMOS_RECENT_OBJECT` from `.lnk` files under `Recent` folders.
pub(crate) fn extract_recent_documents(
    ctx: &JobContext,
    _v: &mut Visits,
) -> Result<(), ModuleError> {
    let mut types = Types::default();
    let attrs = LinkAttrs::new(ctx, &mut types);
    let mut paths = PathLookup::default();
    let mut batch = Batch::default();
    let mut seen = HashSet::new();
    for f in files_with_ext(ctx, "lnk", None, "recent")? {
        if ctx.is_cancelled() {
            break;
        }
        if f.stream.is_some() {
            continue;
        }
        let Some(data) = read_small(ctx, &f) else {
            continue;
        };
        let Ok(l) = ShellLink::parse(&data) else {
            if !f.deleted() {
                tracing::debug!(file = %f.path(), "not a parsable shell link");
            }
            continue;
        };
        let path = l.best_path();
        if !seen.insert(format!("{path}/{}", f.name)) {
            continue;
        }
        let target = paths.find(ctx, &path);
        let a = NewArt::std(ctx, "KAAMOS_RECENT_OBJECT", Some(f.id))
            .text("KAAMOS_PATH", path)
            .attr(attr_id("KAAMOS_PATH_ID"), Value::Int(target.unwrap_or(-1)))
            .time("KAAMOS_DATETIME_ACCESSED", crtime(&f))
            .key(&["KAAMOS_PATH"])
            .associate(target);
        batch.push(attrs.apply(a, &l));
    }
    batch.flush(ctx)?;
    Ok(())
}

fn attr_id(name: &str) -> i64 {
    pm_casedb::catalog::attribute_type_id(name).unwrap_or(0)
}

/// `KAAMOS_RECENT_OBJECT` per jump-list entry (automatic and custom
/// destinations).
pub(crate) fn extract_jump_lists(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let mut types = Types::default();
    let attrs = LinkAttrs::new(ctx, &mut types);
    let stream_attr = types.attribute(
        ctx,
        "KAAMOS_JUMPLIST_ENTRY",
        "Jump List Entry",
        ValueType::String,
    );
    let mut paths = PathLookup::default();
    let mut batch = Batch::default();
    let auto = files_with_ext(ctx, "", Some("%.automaticDestinations-ms"), "")?;
    let custom = files_with_ext(ctx, "", Some("%.customDestinations-ms"), "")?;
    for f in auto.iter().chain(&custom) {
        if ctx.is_cancelled() {
            break;
        }
        if f.stream.is_some() {
            continue;
        }
        let Some(data) = read_small(ctx, f) else {
            continue;
        };
        let id = app_id(&f.name);
        let program = app_name(id).map_or_else(|| id.to_owned(), str::to_owned);
        let entries: Vec<(String, ShellLink, Option<i64>)> =
            if f.name.to_lowercase().ends_with(".automaticdestinations-ms") {
                match parse_automatic(&data) {
                    Ok(j) => j
                        .entries
                        .into_iter()
                        .map(|e| {
                            let t = e
                                .dest
                                .as_ref()
                                .and_then(pm_lnk::jumplist::DestListEntry::last_access_unix);
                            (e.stream_name, e.link, t)
                        })
                        .collect(),
                    Err(e) => {
                        tracing::debug!(file = %f.path(), %e, "unreadable jump list");
                        continue;
                    }
                }
            } else {
                parse_custom(&data)
                    .into_iter()
                    .enumerate()
                    .map(|(i, l)| (format!("custom {i}"), l, None))
                    .collect()
            };
        for (stream, l, accessed) in entries {
            let path = l.best_path();
            let target = paths.find(ctx, &path);
            let a = NewArt::std(ctx, "KAAMOS_RECENT_OBJECT", Some(f.id))
                .text("KAAMOS_PATH", path)
                .attr(attr_id("KAAMOS_PATH_ID"), Value::Int(target.unwrap_or(-1)))
                .time("KAAMOS_DATETIME_ACCESSED", accessed)
                .text("KAAMOS_PROG_NAME", program.clone())
                .custom(stream_attr, Some(Value::Text(stream)))
                .key(&["KAAMOS_PATH"])
                .key_ids(&[stream_attr])
                .associate(target);
            batch.push(attrs.apply(a, &l));
        }
    }
    batch.flush(ctx)?;
    Ok(())
}
