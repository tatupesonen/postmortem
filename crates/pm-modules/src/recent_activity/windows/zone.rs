//! Zone.Identifier analyzer (Autopsy `ExtractZoneIdentifier`): NTFS
//! `file:Zone.Identifier` streams become `TSK_WEB_DOWNLOAD` artifacts
//! unless a browser extractor already reported the download.

use std::collections::HashSet;

use pm_casedb::Value;
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_ingest::{JobContext, ModuleError};
use pm_winartifacts::zone;

use super::art::{Batch, NewArt};
use super::{Visits, files_with_ext, read_small};

const STREAM: &str = ":Zone.Identifier";

pub(crate) fn extract(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let streams = files_with_ext(ctx, "", Some("%:Zone.Identifier"), "")?;
    if streams.is_empty() {
        return Ok(());
    }
    let known = known_downloads(ctx)?;
    let db = ctx.case.db();
    let mut batch = Batch::default();
    for s in streams {
        if ctx.is_cancelled() {
            break;
        }
        let Some(base_name) = s.name.strip_suffix(STREAM).or_else(|| {
            let n = s.name.len().checked_sub(STREAM.len())?;
            s.name
                .get(n..)
                .filter(|t| t.eq_ignore_ascii_case(STREAM))
                .and(s.name.get(..n))
        }) else {
            continue;
        };
        let downloaded = s
            .parent_id
            .and_then(|p| db.child_named_nocase(p, base_name).ok().flatten())
            .filter(|d| d.inode == s.inode && d.stream.is_none());
        let Some(d) = downloaded else { continue };
        if known.contains(&d.id) {
            continue;
        }
        let Some(data) = read_small(ctx, &s) else {
            continue;
        };
        let z = zone::parse(&data);
        let url = z.host_url.clone().unwrap_or_default();
        batch.push(
            NewArt::std(ctx, "TSK_WEB_DOWNLOAD", Some(s.id))
                .text("TSK_PATH", d.path())
                .attr(
                    attribute_type_id("TSK_PATH_ID").unwrap_or(0),
                    Value::Int(d.id),
                )
                .text_opt("TSK_URL", z.host_url.clone())
                .text("TSK_DOMAIN", crate::recent_activity::extract_domain(&url))
                .text_opt("TSK_COMMENT", z.zone_id.map(zone::zone_name))
                .text_opt("TSK_REFERRER", z.referrer_url)
                .key(&["TSK_PATH"])
                .associate(Some(d.id)),
        );
    }
    batch.flush(ctx)?;
    Ok(())
}

/// `TSK_PATH_ID`s of the data source's web downloads.
fn known_downloads(ctx: &JobContext) -> Result<HashSet<i64>, ModuleError> {
    let db = ctx.case.db();
    let Some(ty) = artifact_type_id("TSK_WEB_DOWNLOAD") else {
        return Ok(HashSet::new());
    };
    let mut out = HashSet::new();
    let mut after = 0;
    loop {
        let page = db.artifacts_of_type(ty, Some(ctx.ds_id), after, 5_000)?;
        let Some(last) = page.last() else { break };
        after = last.id;
        for a in page {
            for attr in db.attributes(a.id)? {
                if attr.name == "TSK_PATH_ID"
                    && let Value::Int(id) = attr.value
                {
                    out.insert(id);
                }
            }
        }
    }
    Ok(out)
}
