//! Opt-in write tools. The examiner is recorded as `mcp:<client>`; the
//! assistant can only remove tags it created itself.

use pm_search::ObjRef;
use pm_search::results::{KeywordHitSpec, keyword_hit_artifact};
use serde_json::json;

use super::search::{search_type_arg, text_search_page};
use super::{DS, ToolSpec, tool};
use crate::args::{Args, MAX_LIMIT, Ref};
use crate::error::{ToolError, ToolResult};
use crate::response::Output;
use crate::server::Ctx;

fn write(mut t: ToolSpec) -> ToolSpec {
    t.write = true;
    t
}

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![
        write(tool(
            "add_tag",
            "Tags a file or result with an existing tag name (e.g. Bookmark, Follow Up, Notable Item) and a \
             comment explaining why. The tag is recorded with examiner mcp:<client> for review. Only available \
             when the examiner allowed writes.",
            &[
                ("ref", "string", "file:123 or artifact:45"),
                ("tagName", "string", "An existing tag name"),
                ("comment", "string", "Why the item is tagged"),
            ],
            &["ref", "tagName"],
            add_tag,
        )),
        write(tool(
            "remove_tag",
            "Removes tags with a tag name from a file or result; only tags created by this assistant (examiner \
             mcp:<client>) can be removed.",
            &[
                ("ref", "string", "file:123 or artifact:45"),
                ("tagName", "string", "Tag name"),
            ],
            &["ref", "tagName"],
            remove_tag,
        )),
        write(tool(
            "save_search_results",
            "Runs a search_text query and stores its hits as keyword-hit analysis results (Autopsy's \"Save search \
             results\"), at most 500 objects per call; existing identical hits are skipped.",
            &[
                (
                    "query",
                    "string",
                    "The keyword, phrase or regular expression",
                ),
                (
                    "searchType",
                    "string",
                    "exact (default), substring or regex",
                ),
                (
                    "listName",
                    "string",
                    "Set name of the results (default: none, a single-term search)",
                ),
                DS,
                ("limit", "integer", "Objects to save, default 100, max 500"),
            ],
            &["query"],
            save_search_results,
        )),
    ]
}

fn tag_name_id(ctx: &Ctx<'_>, name: &str) -> ToolResult<i64> {
    let names = ctx.db().tag_names()?;
    names
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .map(|t| t.id)
        .ok_or_else(|| {
            ToolError::arg(format!(
                "no tag name {name:?}; existing names: {}",
                names
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn add_tag(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let r = Ref::req(args, "ref")?;
    let tag = tag_name_id(ctx, &args.req_str("tagName")?)?;
    let comment = args.str("comment")?.unwrap_or_default();
    let examiner = ctx.examiner();
    let db = ctx.db();
    let id = match r {
        Ref::File(id) => {
            db.file(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            let t = db.tag_file(id, tag, &comment, &examiner, now())?;
            db.refresh_event_tag_flags(Some(id), None)?;
            t
        }
        Ref::Artifact(id) => {
            db.artifact(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            let t = db.tag_artifact(id, tag, &comment, &examiner, now())?;
            db.refresh_event_tag_flags(None, Some(id))?;
            t
        }
        other => return Err(ToolError::arg(format!("{other} cannot be tagged"))),
    };
    Ok(json!({"tagged": r.to_string(), "tagId": id, "examiner": examiner}).into())
}

fn remove_tag(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let r = Ref::req(args, "ref")?;
    let tag = tag_name_id(ctx, &args.req_str("tagName")?)?;
    let examiner = ctx.examiner();
    let db = ctx.db();
    let (mine, others) = match r {
        Ref::File(id) => {
            let tags: Vec<_> = db
                .file_tags(id)?
                .into_iter()
                .filter(|t| t.tag_name_id == tag)
                .collect();
            let (mine, others): (Vec<_>, Vec<_>) =
                tags.into_iter().partition(|t| t.examiner == examiner);
            for t in &mine {
                db.untag_file(t.id)?;
            }
            db.refresh_event_tag_flags(Some(id), None)?;
            (mine.len(), others.len())
        }
        Ref::Artifact(id) => {
            let tags: Vec<_> = db
                .artifact_tags(id)?
                .into_iter()
                .filter(|t| t.tag_name_id == tag)
                .collect();
            let (mine, others): (Vec<_>, Vec<_>) =
                tags.into_iter().partition(|t| t.examiner == examiner);
            for t in &mine {
                db.untag_artifact(t.id)?;
            }
            db.refresh_event_tag_flags(None, Some(id))?;
            (mine.len(), others.len())
        }
        other => return Err(ToolError::arg(format!("{other} cannot be tagged"))),
    };
    if mine == 0 && others > 0 {
        return Err(ToolError::Refused(format!(
            "{r} has {others} such tag(s), but they were made by the examiner or another client; the assistant may only remove its own tags"
        )));
    }
    Ok(json!({"ref": r.to_string(), "removed": mine, "keptOtherExaminers": others}).into())
}

fn save_search_results(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let term = args.req_str("query")?;
    let st = search_type_arg(args.str("searchType")?.as_deref())?;
    let list = args.str("listName")?.unwrap_or_default();
    let limit = args.limit(100, MAX_LIMIT)?;
    let q = pm_search::TextQuery {
        term: term.clone(),
        search_type: st,
        ds_ids: args.i64("dataSourceId")?.into_iter().collect(),
    };
    let (hits, next) = text_search_page(ctx, &q, None, None, limit)?;
    let db = ctx.db();
    let created_at = now();
    let mut saved = 0usize;
    let mut skipped = 0usize;
    for (h, _) in &hits {
        let (ds_id, file_id, assoc) = match h.obj {
            ObjRef::File(id) => match db.file(id)? {
                Some(f) => (f.ds_id, Some(id), None),
                None => continue,
            },
            ObjRef::Artifact(id) => match db.artifact(id)? {
                Some(a) => (a.ds_id, a.file_id, Some(id)),
                None => continue,
            },
        };
        let existing = match file_id {
            Some(f) => db.keyword_hit_keys(f)?,
            None => std::collections::HashSet::default(),
        };
        let terms: Vec<&str> = if h.hits.is_empty() {
            vec![term.as_str()]
        } else {
            h.hits.iter().map(String::as_str).collect()
        };
        for hit in terms {
            let (mut artifact, key) = keyword_hit_artifact(&KeywordHitSpec {
                ds_id,
                file_id,
                associated_artifact: assoc,
                list_name: &list,
                term: &term,
                search_type: st,
                selector: None,
                hit,
                snippet: &h.preview,
                module: "Keyword Search",
                created_at,
            });
            if existing.contains(&key) {
                skipped += 1;
                continue;
            }
            artifact.justification = Some(format!("Saved by {} (MCP)", ctx.examiner()));
            db.add_artifact(&artifact)?;
            saved += 1;
        }
    }
    Ok(json!({
        "query": term,
        "objects": hits.len(),
        "saved": saved,
        "skippedExisting": skipped,
        "moreHitsNotSaved": next.is_some(),
        "examiner": ctx.examiner(),
    })
    .into())
}
