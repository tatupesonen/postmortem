//! Search tools: keyword index, file-name index, structured file search,
//! keyword lists and hash lookups.

use pm_casedb::mcp::{ArtifactFilter, FileFilter, TimeColumn};
use pm_casedb::{FileKindCode, FileRecord, Known};
use pm_search::{NameMatch, NameQuery, ObjRef, SearchType, TextQuery};
use serde_json::{Value, json};

use super::autopsy::{file_page, hash_arg, order_arg, score_arg};
use super::{CURSOR, DS, LIMIT, ToolSpec, tool};
use crate::args::{Args, DEFAULT_LIMIT, MAX_LIMIT, Ref, hex, unhex};
use crate::error::{ToolError, ToolResult};
use crate::response::{Output, Page, Row};
use crate::rows::{artifact_ref, file_brief};
use crate::server::Ctx;

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "search_text",
            "Keyword search over the extracted text of files and artifacts (Autopsy's keyword index): exact words, \
             substring or regular expression, case-insensitive, with Autopsy's keyword semantics. Returns one row per \
             object with the first page that has a hit (page 0 is the name), a snippet and the distinct hit terms. \
             Follow up with read_file_text(ref, page). Needs Keyword Search to have run.",
            &[
                (
                    "query",
                    "string",
                    "The keyword, phrase or regular expression",
                ),
                (
                    "searchType",
                    "string",
                    "exact (default, whole words), substring or regex",
                ),
                DS,
                (
                    "mimeType",
                    "string",
                    "Only files of this MIME type (image/* prefixes allowed)",
                ),
                LIMIT,
                CURSOR,
            ],
            &["query"],
            search_text,
        ),
        tool(
            "search_file_names",
            "Indexed file-name search over every file of the case (hundreds of millions of names): substring, \
             prefix or exact match on the name or the full path, case-insensitive. Filters: extensions, size, deleted, \
             directories, data source. Returns file references with paths in `evidence` and the total match count.",
            &[
                (
                    "query",
                    "string",
                    "Text to find; empty lists every file matching the filters",
                ),
                ("mode", "string", "substring (default), prefix or exact"),
                (
                    "inPath",
                    "boolean",
                    "Match against the full path /dir/name instead of the name",
                ),
                (
                    "extensions",
                    "array",
                    "Extensions without the dot, e.g. [\"pdf\", \"docx\"]",
                ),
                ("minSize", "integer", "Minimum size in bytes"),
                ("maxSize", "integer", "Maximum size in bytes"),
                ("excludeDeleted", "boolean", "Leave out deleted files"),
                ("excludeDirectories", "boolean", "Leave out directories"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            search_file_names,
        ),
        tool(
            "find_files",
            "Structured file search (Autopsy's File Search by Attributes and more): name, extension, MIME type, size, \
             each timestamp, deleted / allocated, known status, hashes, tag, score (bad / suspicious), hash-set hits, \
             extension mismatches, and everything inside a folder.",
            &[
                (
                    "nameContains",
                    "string",
                    "File name substring (case-insensitive)",
                ),
                ("name", "string", "Exact file name (case-insensitive)"),
                ("extensions", "array", "Extensions without the dot"),
                (
                    "mimeType",
                    "string",
                    "MIME type, or a prefix ending in * (image/*)",
                ),
                ("minSize", "integer", "Minimum size in bytes"),
                ("maxSize", "integer", "Maximum size in bytes"),
                ("modifiedAfter", "string", "ISO 8601"),
                ("modifiedBefore", "string", "ISO 8601"),
                ("accessedAfter", "string", "ISO 8601"),
                ("accessedBefore", "string", "ISO 8601"),
                ("changedAfter", "string", "ISO 8601"),
                ("changedBefore", "string", "ISO 8601"),
                ("createdAfter", "string", "ISO 8601"),
                ("createdBefore", "string", "ISO 8601"),
                (
                    "deleted",
                    "boolean",
                    "true: only deleted files; false: only allocated ones",
                ),
                (
                    "isDirectory",
                    "boolean",
                    "true: directories only; false: regular files only",
                ),
                ("knownState", "string", "UNKNOWN, KNOWN or NOTABLE"),
                ("md5", "string", "MD5 (hex)"),
                ("sha1", "string", "SHA-1 (hex)"),
                ("sha256", "string", "SHA-256 (hex)"),
                ("tagName", "string", "Tagged with this tag name"),
                (
                    "score",
                    "string",
                    "bad, suspicious or any (Autopsy's Score tree)",
                ),
                ("hashSetHit", "boolean", "Only files with a hash-set hit"),
                (
                    "extensionMismatch",
                    "boolean",
                    "Only files whose extension does not match their type",
                ),
                (
                    "insideFolder",
                    "string",
                    "A folder path (/Users/alice/) or reference (file:12): everything below it",
                ),
                ("pathContains", "string", "Parent path substring"),
                DS,
                (
                    "orderBy",
                    "string",
                    "id (default), size_desc, size_asc, name_asc, name_desc, modified_desc, modified_asc, created_desc, created_asc",
                ),
                LIMIT,
                CURSOR,
            ],
            &[],
            find_files,
        ),
        tool(
            "lookup_hash",
            "Looks up an MD5, SHA-1 or SHA-256 in the case: every file with that hash (with paths), its known \
             status and hash-set hits. External hash sets and NSRL are only reflected through the case's Hash Lookup \
             results.",
            &[
                ("hash", "string", "Hex MD5 (32), SHA-1 (40) or SHA-256 (64)"),
                LIMIT,
                CURSOR,
            ],
            &["hash"],
            lookup_hash,
        ),
        tool(
            "run_keyword_list",
            "Runs a keyword list over the keyword index and returns hit counts per keyword with the first matching \
             objects. Lists: the built-in Phone Numbers, IP Addresses, Email Addresses, URLs, Credit Card Numbers, and \
             the examiner's lists (omit list to see the names).",
            &[
                (
                    "list",
                    "string",
                    "Keyword list name; omit to list the available lists",
                ),
                DS,
                (
                    "maxObjectsPerKeyword",
                    "integer",
                    "Objects counted per keyword, default 200, max 1000",
                ),
            ],
            &[],
            run_keyword_list,
        ),
    ]
}

pub(crate) fn search_type_arg(s: Option<&str>) -> ToolResult<SearchType> {
    Ok(match s.map(str::to_ascii_lowercase).as_deref() {
        None | Some("exact" | "literal" | "whole_word") => SearchType::Exact,
        Some("substring") => SearchType::Substring,
        Some("regex" | "regexp" | "regular_expression") => SearchType::Regex,
        Some(o) => {
            return Err(ToolError::arg(format!(
                "searchType must be exact, substring or regex, not {o:?}"
            )));
        }
    })
}

fn search_type_name(t: SearchType) -> &'static str {
    match t {
        SearchType::Exact => "exact",
        SearchType::Substring => "substring",
        SearchType::Regex => "regex",
    }
}

/// A search-hit row.
fn hit_row(
    ctx: &Ctx<'_>,
    h: &pm_search::ObjectHit,
    file: Option<&FileRecord>,
) -> ToolResult<Value> {
    Ok(match h.obj {
        ObjRef::File(id) => json!({
            "ref": Ref::File(id).to_string(),
            "objectId": id,
            "objectType": "file",
            "page": h.page,
            "dataSourceId": file.map(|f| f.ds_id),
            "mimeType": file.and_then(|f| f.mime.clone()),
            "size": file.map(|f| f.size),
            "evidence": {
                "path": file.map(FileRecord::path),
                "preview": h.preview,
                "hits": h.hits,
            },
        }),
        ObjRef::Artifact(id) => {
            let a = ctx.db().artifact(id)?;
            let (ty, analysis) = match &a {
                Some(a) => {
                    let (n, _, an) = ctx.artifact_type(a.type_id)?;
                    (Some(n), an)
                }
                None => (None, false),
            };
            json!({
                "ref": artifact_ref(id, analysis),
                "objectId": id,
                "objectType": "artifact",
                "artifactType": ty,
                "page": h.page,
                "dataSourceId": a.as_ref().map(|a| a.ds_id),
                "sourceRef": a.as_ref().and_then(|a| a.file_id).map(|f| Ref::File(f).to_string()),
                "evidence": {"preview": h.preview, "hits": h.hits},
            })
        }
    })
}

fn mime_matches(m: Option<&str>, want: &str) -> bool {
    match (m, want.strip_suffix('*')) {
        (Some(m), Some(prefix)) => m.starts_with(prefix),
        (Some(m), None) => m.eq_ignore_ascii_case(want),
        (None, _) => false,
    }
}

/// Hits with their files, and the key to continue after.
pub(crate) type HitPage = (Vec<(pm_search::ObjectHit, Option<FileRecord>)>, Option<i64>);

/// Runs a text query: one page of rows (MIME filter applied).
pub(crate) fn text_search_page(
    ctx: &Ctx<'_>,
    q: &TextQuery,
    mime: Option<&str>,
    after: Option<i64>,
    limit: usize,
) -> ToolResult<HitPage> {
    let ix = ctx.index()?;
    let mut out = Vec::new();
    let mut cursor = after;
    // A MIME filter may discard rows: keep reading, but boundedly.
    for _ in 0..20 {
        let page = ix.content().search(q, cursor, limit, &|| false)?;
        for h in page.hits {
            let file = match h.obj {
                ObjRef::File(id) => ctx.db().file(id)?,
                ObjRef::Artifact(_) => None,
            };
            if let Some(m) = mime
                && !mime_matches(file.as_ref().and_then(|f| f.mime.as_deref()), m)
            {
                continue;
            }
            let key = h.obj.key();
            out.push((h, file));
            if out.len() >= limit {
                return Ok((out, Some(key)));
            }
        }
        match page.next {
            Some(n) => cursor = Some(n),
            None => return Ok((out, None)),
        }
    }
    Ok((out, cursor))
}

fn search_text(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let term = args.req_str("query")?;
    let st = search_type_arg(args.str("searchType")?.as_deref())?;
    let q = TextQuery {
        term,
        search_type: st,
        ds_ids: args.i64("dataSourceId")?.into_iter().collect(),
    };
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64());
    let mime = args.str("mimeType")?;
    let (hits, next) = text_search_page(ctx, &q, mime.as_deref(), after, limit)?;
    let mut rows = Vec::with_capacity(hits.len());
    for (h, f) in &hits {
        rows.push(Row {
            value: hit_row(ctx, h, f.as_ref())?,
            cursor: json!(h.obj.key()),
        });
    }
    // `next` is the key to continue after; when it differs from the last
    // row (MIME filtering skipped rows) the last row's cursor still
    // continues correctly because the index pages by key.
    let more = next.is_some();
    if let (Some(n), Some(last)) = (next, rows.last_mut()) {
        last.cursor = json!(n);
    }
    Ok(Page {
        rows,
        more,
        extra: serde_json::Map::new(),
    }
    .with("searchType", json!(search_type_name(st)))
    .into())
}

fn search_file_names(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let mode = match args.str("mode")?.map(|m| m.to_ascii_lowercase()).as_deref() {
        None | Some("substring" | "contains") => NameMatch::Substring,
        Some("prefix" | "starts_with") => NameMatch::Prefix,
        Some("exact") => NameMatch::Exact,
        Some(m) => {
            return Err(ToolError::arg(format!(
                "mode must be substring, prefix or exact, not {m:?}"
            )));
        }
    };
    let q = NameQuery {
        text: args.str("query")?.unwrap_or_default(),
        mode,
        in_path: args.flag("inPath")?,
        ds_ids: args.i64("dataSourceId")?.into_iter().collect(),
        exclude_deleted: args.flag("excludeDeleted")?,
        exclude_dirs: args.flag("excludeDirectories")?,
        extensions: args
            .str_list("extensions")?
            .into_iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect(),
        min_size: args.u64("minSize")?,
        max_size: args.u64("maxSize")?,
    };
    let ix = ctx.index().map_err(|_| {
        ToolError::Unavailable(
            "the case has no file-name index yet (it is built when Keyword Search runs or the GUI opens the case); use find_files".into(),
        )
    })?;
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64());
    let page = ix.names().search(&q, after, limit)?;
    let total = ix.names().count(&q)?;
    let files = ctx.db().files_by_ids(&page.ids)?;
    let rows = files
        .iter()
        .map(|f| Row {
            value: file_brief(f),
            cursor: json!(f.id),
        })
        .collect();
    let mut p = Page {
        rows,
        more: page.next.is_some(),
        extra: serde_json::Map::new(),
    };
    if let (Some(n), Some(last)) = (page.next, p.rows.last_mut()) {
        last.cursor = json!(n);
    }
    Ok(p.with("totalMatches", json!(total)).into())
}

fn find_files(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let mut f = FileFilter {
        ds_id: args.i64("dataSourceId")?,
        name_contains: args.str("nameContains")?,
        name_equals: args.str("name")?,
        extensions: args.str_list("extensions")?,
        mime: args.str("mimeType")?,
        min_size: args.u64("minSize")?,
        max_size: args.u64("maxSize")?,
        path_contains: args.str("pathContains")?,
        deleted: args.bool("deleted")?,
        md5: hash_arg(args, "md5")?,
        sha1: hash_arg(args, "sha1")?,
        sha256: hash_arg(args, "sha256")?,
        hash_set_hit: args.flag("hashSetHit")?,
        extension_mismatch: args.flag("extensionMismatch")?,
        ..Default::default()
    };
    f.kind = args.bool("isDirectory")?.map(|d| {
        if d {
            FileKindCode::Directory
        } else {
            FileKindCode::Regular
        }
    });
    if let Some(k) = args.str("knownState")? {
        f.known = Some(match k.to_ascii_uppercase().as_str() {
            "KNOWN" => Known::Known,
            "NOTABLE" | "BAD" => Known::Notable,
            "UNKNOWN" => Known::Unknown,
            _ => {
                return Err(ToolError::arg(
                    "knownState must be UNKNOWN, KNOWN or NOTABLE",
                ));
            }
        });
    }
    for (col, a, b) in [
        (TimeColumn::Modified, "modifiedAfter", "modifiedBefore"),
        (TimeColumn::Accessed, "accessedAfter", "accessedBefore"),
        (TimeColumn::Changed, "changedAfter", "changedBefore"),
        (TimeColumn::Created, "createdAfter", "createdBefore"),
    ] {
        let (from, to) = (args.time(a, false)?, args.time(b, true)?);
        if from.is_some() || to.is_some() {
            f.times.push((col, from, to));
        }
    }
    if let Some(t) = args.str("tagName")? {
        let tag = ctx
            .db()
            .tag_names()?
            .into_iter()
            .find(|n| n.name.eq_ignore_ascii_case(&t))
            .ok_or_else(|| ToolError::arg(format!("no tag name {t:?}")))?;
        f.tag_name_id = Some(tag.id);
    }
    if let Some(s) = args.str("score")? {
        f.score = Some(score_arg(&s)?);
    }
    if let Some(folder) = args.str("insideFolder")? {
        let path = match Ref::parse(&folder) {
            Some(Ref::File(id)) if !folder.starts_with('/') => {
                let d = ctx
                    .db()
                    .file(id)?
                    .ok_or_else(|| ToolError::NotFound(format!("file:{id}")))?;
                if f.ds_id.is_none() {
                    f.ds_id = Some(d.ds_id);
                }
                format!("{}/", d.path())
            }
            _ => folder,
        };
        f.under_path = Some(path);
    }
    let (order, desc) = order_arg(args.str("orderBy")?.as_deref())?;
    Ok(file_page(ctx, args, &f, order, desc, true)?.into())
}

fn lookup_hash(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let h = args.req_str("hash")?;
    let bytes =
        unhex(&h).ok_or_else(|| ToolError::arg("hash must be a hex MD5, SHA-1 or SHA-256"))?;
    let mut f = FileFilter::default();
    let kind = match bytes.len() {
        16 => {
            f.md5 = Some(bytes);
            "md5"
        }
        20 => {
            f.sha1 = Some(bytes);
            "sha1"
        }
        _ => {
            f.sha256 = Some(bytes);
            "sha256"
        }
    };
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64());
    let cursor = after.map(|id| pm_casedb::mcp::FileCursor {
        key: pm_casedb::mcp::SortKey::None,
        id,
    });
    let files = ctx.db().mcp_files(
        &f,
        pm_casedb::mcp::FileOrder::Id,
        false,
        cursor.as_ref(),
        limit + 1,
    )?;
    let set_name = ctx.attribute_type("TSK_SET_NAME")?.map(|t| t.0);
    let page = Page::from_items(files, limit, |file| {
        let hits = ctx.db().mcp_artifacts(
            &ArtifactFilter {
                file_id: Some(file.id),
                type_ids: vec![10],
                ..Default::default()
            },
            None,
            false,
            20,
        )?;
        let mut sets = Vec::new();
        for a in &hits {
            for at in ctx.db().attributes(a.id)? {
                if Some(at.type_id) == set_name {
                    sets.push(at.value.display());
                }
            }
        }
        let mut v = file_brief(file);
        if let Some(o) = v.as_object_mut() {
            o.insert(
                "knownState".into(),
                json!(crate::rows::known_name(file.known)),
            );
            o.insert("hashSetHits".into(), json!(sets));
            o.insert("sha1Hash".into(), json!(file.sha1.as_deref().map(hex)));
            o.insert("sha256Hash".into(), json!(file.sha256.as_deref().map(hex)));
        }
        Ok(Row {
            value: v,
            cursor: json!(file.id),
        })
    })?;
    Ok(page
        .with("hashType", json!(kind))
        .with(
            "note",
            json!("Only the case database is searched; known/notable status and hash-set hits come from the Hash Lookup module (see list_ingest_jobs)."),
        )
        .into())
}

fn keyword_lists(ctx: &Ctx<'_>) -> Vec<pm_search::KeywordList> {
    match ctx.config().keyword_lists {
        Some(p) => match pm_search::KeywordListStore::load(&p) {
            Ok(store) => store.lists,
            Err(e) => {
                tracing::warn!("cannot load keyword lists {}: {e}", p.display());
                pm_search::keywords::builtin_lists()
            }
        },
        None => pm_search::keywords::builtin_lists(),
    }
}

fn run_keyword_list(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let lists = keyword_lists(ctx);
    let Some(name) = args.str("list")? else {
        let names: Vec<Value> = lists
            .iter()
            .map(|l| json!({"list": l.name, "keywords": l.keywords.len(), "builtIn": l.locked}))
            .collect();
        return Ok(json!({"lists": names}).into());
    };
    let list = lists
        .iter()
        .find(|l| l.name.eq_ignore_ascii_case(&name))
        .ok_or_else(|| {
            ToolError::arg(format!(
                "no keyword list {name:?}; omit list to see the names"
            ))
        })?;
    let cap = args
        .i64("maxObjectsPerKeyword")?
        .unwrap_or(200)
        .clamp(1, 1000) as usize;
    let ds: Vec<i64> = args.i64("dataSourceId")?.into_iter().collect();
    let ix = ctx.index()?;
    let mut rows = Vec::new();
    for k in list.keywords.iter().take(MAX_LIMIT) {
        let q = TextQuery {
            term: k.term.clone(),
            search_type: k.search_type(),
            ds_ids: ds.clone(),
        };
        let mut count = 0usize;
        let mut first = Vec::new();
        let mut after = None;
        let mut capped = false;
        let mut error = None;
        loop {
            match ix.content().search(&q, after, 100, &|| false) {
                Ok(page) => {
                    for h in &page.hits {
                        count += 1;
                        if first.len() < 5 {
                            first.push(match h.obj {
                                ObjRef::File(id) => Ref::File(id).to_string(),
                                ObjRef::Artifact(id) => Ref::Artifact(id).to_string(),
                            });
                        }
                    }
                    if count >= cap {
                        capped = page.next.is_some() || count > cap;
                        break;
                    }
                    match page.next {
                        Some(n) => after = Some(n),
                        None => break,
                    }
                }
                Err(e) => {
                    error = Some(e.to_string());
                    break;
                }
            }
        }
        rows.push(json!({
            "searchType": search_type_name(k.search_type()),
            "objectsWithHits": count.min(cap),
            "countCapped": capped,
            "firstRefs": first,
            "error": error,
            "keyword": k.term,
        }));
    }
    Ok(json!({"list": list.name, "keywords": rows}).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_types_and_mime() {
        assert_eq!(search_type_arg(None).unwrap(), SearchType::Exact);
        assert_eq!(search_type_arg(Some("REGEX")).unwrap(), SearchType::Regex);
        assert!(search_type_arg(Some("fuzzy")).is_err());
        assert!(mime_matches(Some("image/png"), "image/*"));
        assert!(!mime_matches(Some("text/plain"), "image/*"));
        assert!(mime_matches(Some("text/plain"), "TEXT/PLAIN"));
        assert!(!mime_matches(None, "text/plain"));
    }
}
