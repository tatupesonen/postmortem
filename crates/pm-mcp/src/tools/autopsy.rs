//! Autopsy's MCP tools: same names, arguments and,
//! where it does not conflict with the `evidence` rule, result fields.

use pm_casedb::query::{
    ArtifactFilter, AttrPredicate, FileCursor, FileFilter, FileOrder, ScoreClass, SortKey, TagRow,
    TimeColumn,
};
use pm_casedb::{FileKindCode, Known};
use serde_json::{Map, Value, json};

use super::{CURSOR, DS, LIMIT, ToolSpec, tool};
use crate::args::{Args, DEFAULT_LIMIT, MAX_LIMIT, Ref, iso_json, unhex};
use crate::error::{ToolError, ToolResult};
use crate::response::{Output, Page, Row};
use crate::rows::{artifact_ref, artifact_row, data_source_row, file_brief, file_row};
use crate::server::{Ctx, SERVER_NAME};

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "get_server_status",
            "Returns the status of the postmortem MCP server and the open case. Call this first: it confirms the \
             server is running, names the case and says whether it is read-only and whether a keyword index exists.",
            &[],
            &[],
            server_status,
        ),
        tool(
            "get_case_summary",
            "Summary of the open case: name, number, examiner, data sources with their time zones, file and artifact \
             counts, the time span of activity and keyword-index status. Call this first for any general question.",
            &[],
            &[],
            case_summary,
        ),
        tool(
            "query_files",
            "Search for files in the case. All parameters optional. Returns file metadata matching Autopsy's columns: \
             timestamps (modified/changed/accessed/created, ISO 8601 UTC plus *Epoch seconds), size, flags, known \
             status, MIME type, hashes; name and path are inside `evidence`. For substring searches over hundreds of \
             millions of names prefer search_file_names (indexed).",
            &[
                (
                    "nameContains",
                    "string",
                    "Filter by filename substring (case-insensitive)",
                ),
                (
                    "extension",
                    "string",
                    "Filter by file extension without dot, e.g. exe, jpg, pdf (case-insensitive)",
                ),
                (
                    "mimeType",
                    "string",
                    "Filter by MIME type e.g. image/jpeg or image/*",
                ),
                ("minSize", "integer", "Minimum file size in bytes"),
                ("maxSize", "integer", "Maximum file size in bytes"),
                (
                    "modifiedAfter",
                    "string",
                    "ISO 8601 date — files with mtime after this date",
                ),
                (
                    "modifiedBefore",
                    "string",
                    "ISO 8601 date — files with mtime before this date",
                ),
                (
                    "createdAfter",
                    "string",
                    "ISO 8601 date — files with crtime (birth time) after this date",
                ),
                (
                    "createdBefore",
                    "string",
                    "ISO 8601 date — files with crtime (birth time) before this date",
                ),
                (
                    "pathContains",
                    "string",
                    "Filter by parent path substring (case-insensitive)",
                ),
                (
                    "isDirectory",
                    "boolean",
                    "true to return only directories, false for regular files only",
                ),
                (
                    "allocated",
                    "boolean",
                    "true for allocated files only, false for unallocated only",
                ),
                (
                    "knownState",
                    "string",
                    "Filter by known status: UNKNOWN, KNOWN, or NOTABLE",
                ),
                ("md5", "string", "Filter by exact MD5 hash (hex string)"),
                (
                    "sha256",
                    "string",
                    "Filter by exact SHA-256 hash (hex string)",
                ),
                DS,
                LIMIT,
                (
                    "orderBy",
                    "string",
                    "Sort order: size_desc, size_asc, name_asc, name_desc, modified_desc, modified_asc, created_desc, created_asc",
                ),
                CURSOR,
            ],
            &[],
            query_files,
        ),
        tool(
            "query_data_artifacts",
            "Search data artifacts: facts extracted from the evidence such as web history, downloads, recent \
             documents, installed programs, USB devices, accounts, messages, GPS points and event-log entries. Filter \
             by artifactType (e.g. KAAMOS_WEB_HISTORY; list_artifact_types shows the types present with counts), by an \
             attribute (attributeType e.g. KAAMOS_URL, attributeValue substring) and data source. Attributes are inside \
             `evidence`.",
            &[
                (
                    "artifactType",
                    "string",
                    "Artifact type name e.g. KAAMOS_WEB_HISTORY",
                ),
                (
                    "attributeType",
                    "string",
                    "Filter by attribute type name e.g. KAAMOS_URL",
                ),
                (
                    "attributeValue",
                    "string",
                    "Filter by attribute value substring",
                ),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            query_data_artifacts,
        ),
        tool(
            "query_analysis_results",
            "Search analysis results: conclusions of ingest modules such as hash-set hits, keyword hits, \
             interesting items, extension mismatches, encryption and EXIF metadata. Each result has a score \
             (significance Notable, Likely Notable, …). Filter by artifactType (e.g. KAAMOS_KEYWORD_HIT, \
             KAAMOS_HASHSET_HIT), attribute and data source.",
            &[
                (
                    "artifactType",
                    "string",
                    "Artifact type name e.g. KAAMOS_KEYWORD_HIT",
                ),
                (
                    "attributeType",
                    "string",
                    "Filter by attribute type name e.g. KAAMOS_KEYWORD",
                ),
                (
                    "attributeValue",
                    "string",
                    "Filter by attribute value substring",
                ),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            query_analysis_results,
        ),
        tool(
            "query_data_sources",
            "List all data sources (disk images, logical file sets) in the case with objectId, name, type, size, \
             time zone, image type, sector size, image paths and acquisition hashes.",
            &[],
            &[],
            query_data_sources,
        ),
        tool(
            "get_hosts",
            "List the hosts of the case. Each host groups the data sources of one device or machine; this is the top \
             of the storage hierarchy.",
            &[],
            &[],
            get_hosts,
        ),
        tool(
            "get_data_source_tree",
            "The storage hierarchy of one or all data sources: Image → VolumeSystem → Volume → FileSystem, with \
             partition offsets, file-system types, block sizes and root directory references.",
            &[(
                "dataSourceId",
                "integer",
                "Object ID of the data source to inspect. Omit to return all data sources.",
            )],
            &[],
            data_source_tree,
        ),
        tool(
            "query_tags",
            "Files and results tagged by the examiner (or by the assistant, examiner mcp:<client>).",
            &[
                (
                    "tagName",
                    "string",
                    "Filter by tag name e.g. \"Notable Item\"",
                ),
                LIMIT,
                CURSOR,
            ],
            &[],
            query_tags,
        ),
        tool(
            "query_timeline",
            "Timeline events in a time range, oldest first: file-system timestamps (modified, accessed, changed, \
             created) and artifact events (web history, downloads, programs run, devices, messages, …). Use \
             summarize_timeline or timeline_histogram first. Omit startTime/endTime to span the whole case. \
             timeline_events offers type filters and newest-first order.",
            &[
                (
                    "startTime",
                    "string",
                    "ISO 8601 start of time range (inclusive)",
                ),
                (
                    "endTime",
                    "string",
                    "ISO 8601 end of time range (inclusive)",
                ),
                DS,
                (
                    "textFilter",
                    "string",
                    "Filter events whose description (or file path) contains this substring",
                ),
                ("limit", "integer", "Max results, default 100, max 500"),
                CURSOR,
            ],
            &[],
            super::timeline::query_timeline,
        ),
        tool(
            "summarize_timeline",
            "Counts of timeline events by category (File System, Web Activity, Other) for a time range. Omit \
             startTime/endTime to summarise the whole case.",
            &[
                (
                    "startTime",
                    "string",
                    "ISO 8601 start of time range (inclusive)",
                ),
                (
                    "endTime",
                    "string",
                    "ISO 8601 end of time range (inclusive)",
                ),
                DS,
            ],
            &[],
            super::timeline::summarize_timeline,
        ),
        tool(
            "get_os_accounts",
            "OS user accounts found in the case: SID/UID, login and full name, type, status, creation time, extended \
             attributes (home directory, last login, …) and the data sources where each appears.",
            &[DS, LIMIT, CURSOR],
            &[],
            os_accounts,
        ),
        tool(
            "get_communications_accounts",
            "Accounts found in communications data: e-mail addresses, phone numbers, messaging-app user names, … \
             Optionally filter by account type (CREDIT_CARD, EMAIL, PHONE, SKYPE, WHATSAPP, FACEBOOK, …). Returns the \
             account type, identifier and relationship count.",
            &[
                (
                    "accountType",
                    "string",
                    "Filter by account type e.g. EMAIL, PHONE, SKYPE",
                ),
                LIMIT,
                CURSOR,
            ],
            &[],
            comm_accounts,
        ),
        tool(
            "get_file_content",
            "Read the content of a file as UTF-8 text (undecodable bytes replaced) by its object ID. Reads at most \
             maxBytes (default 65536, max 196608) from offset; page through larger files with offset. For extracted \
             document text use read_file_text, for raw bytes read_file_bytes.",
            &[
                (
                    "objectId",
                    "integer",
                    "Object ID of the file (from query_files objectId field)",
                ),
                (
                    "offset",
                    "integer",
                    "Byte offset to start reading from, default 0",
                ),
                (
                    "maxBytes",
                    "integer",
                    "Maximum bytes to read, default 65536, max 196608",
                ),
            ],
            &["objectId"],
            file_content,
        ),
        tool(
            "get_account_relationships",
            "Communications relationships of one account: who it communicated with and how many messages, calls and \
             contacts link them. Supply accountType (e.g. EMAIL) and accountId (e.g. user@example.com or +15551234567).",
            &[
                (
                    "accountType",
                    "string",
                    "Account type e.g. EMAIL, PHONE (required)",
                ),
                (
                    "accountId",
                    "string",
                    "Type-specific identifier e.g. user@example.com or +15551234567 (required)",
                ),
            ],
            &["accountType", "accountId"],
            account_relationships,
        ),
        tool(
            "get_object_children",
            "The parent and children of an object: a directory's files, a file's derived files and artifacts, an \
             artifact's linked results, a data source's volume systems and file systems. objectId is a file id; \
             pass ref (file:1, artifact:2, ds:3) for other objects.",
            &[
                (
                    "objectId",
                    "integer",
                    "Object ID of the file whose children you want",
                ),
                (
                    "ref",
                    "string",
                    "Reference of any object (file:123, artifact:45, ds:1)",
                ),
                LIMIT,
                CURSOR,
            ],
            &[],
            object_children,
        ),
        tool(
            "list_reports",
            "Reports generated for the case: objectId, reportName, sourceModule, path, createdTime, size and \
             contentType (text/plain or text/html).",
            &[],
            &[],
            list_reports,
        ),
        tool(
            "get_report_content",
            "Read a report by its objectId (from list_reports). Reads at most maxBytes (default 65536, max 196608) \
             from offset.",
            &[
                (
                    "objectId",
                    "integer",
                    "Object ID of the report (from list_reports objectId field)",
                ),
                (
                    "offset",
                    "integer",
                    "Byte offset to start reading from, default 0",
                ),
                (
                    "maxBytes",
                    "integer",
                    "Maximum bytes to read, default 65536, max 196608",
                ),
            ],
            &["objectId"],
            report_content,
        ),
    ]
}

/// Largest text read of `get_file_content` / `get_report_content` (the
/// response must stay under 256 KiB).
pub(crate) const MAX_TEXT_READ: u64 = 196_608;

// ------------------------------------------------------------------ status

#[allow(clippy::unnecessary_wraps, reason = "tool handler signature")]
fn server_status(ctx: &Ctx<'_>, _: &Args) -> ToolResult<Output> {
    let cfg = ctx.config();
    Ok(json!({
        "server": SERVER_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "status": "running",
        "caseOpen": true,
        "caseName": ctx.s.case_name(),
        "readOnly": ctx.case().is_read_only(),
        "writesEnabled": cfg.allow_writes && !ctx.case().is_read_only(),
        "imagePreviews": cfg.image_previews,
        "keywordIndex": ctx.index().is_ok(),
    })
    .into())
}

fn case_summary(ctx: &Ctx<'_>, _: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    let details = ctx.case().details();
    let sources = db.data_sources()?;
    let mut zones: Vec<&str> = sources.iter().map(|d| d.timezone.as_str()).collect();
    zones.sort_unstable();
    zones.dedup();
    let span = db.time_span()?;
    let index = ctx.index().ok().map(|ix| ix.content().stats());
    let index = match index {
        Some(Ok(s)) => json!({"available": true, "objects": s.objects, "chunks": s.chunks}),
        _ => json!({"available": false}),
    };
    Ok(json!({
        "caseName": ctx.s.case_name(),
        "caseNumber": details.number,
        "examiner": details.examiner.name,
        "caseUuid": ctx.case().id(),
        "created": ctx.case().created(),
        "readOnly": ctx.case().is_read_only(),
        "dataSources": sources.len(),
        "totalFiles": db.file_count(None)?,
        "totalArtifacts": db.artifact_count(None)?,
        "timeZones": zones,
        "timeSpan": span.map(|(a, b)| json!({
            "start": iso_json(Some(a)), "startEpoch": a, "end": iso_json(Some(b)), "endEpoch": b,
        })),
        "dataSourceList": sources.iter().map(|d| json!({
            "ref": Ref::DataSource(d.id).to_string(), "objectId": d.id, "name": d.name, "timezone": d.timezone,
        })).collect::<Vec<_>>(),
        "keywordIndex": index,
        "notes": "Times are UTC. Data-source time zones only matter for FAT and other local-time file systems.",
    })
    .into())
}

// -------------------------------------------------------------------- files

fn known_arg(s: &str) -> ToolResult<Known> {
    match s.to_ascii_uppercase().as_str() {
        "KNOWN" => Ok(Known::Known),
        "NOTABLE" | "BAD" => Ok(Known::Notable),
        "UNKNOWN" => Ok(Known::Unknown),
        _ => Err(ToolError::arg(
            "knownState must be UNKNOWN, KNOWN or NOTABLE",
        )),
    }
}

pub(crate) fn hash_arg(args: &Args, key: &str) -> ToolResult<Option<Vec<u8>>> {
    match args.str(key)? {
        None => Ok(None),
        Some(h) => unhex(&h)
            .map(Some)
            .ok_or_else(|| ToolError::arg(format!("{key} must be a hex MD5, SHA-1 or SHA-256"))),
    }
}

pub(crate) fn order_arg(s: Option<&str>) -> ToolResult<(FileOrder, bool)> {
    Ok(match s.map(str::to_ascii_lowercase).as_deref() {
        None | Some("" | "id" | "id_asc") => (FileOrder::Id, false),
        Some("id_desc") => (FileOrder::Id, true),
        Some("size_desc") => (FileOrder::Size, true),
        Some("size_asc") => (FileOrder::Size, false),
        Some("name_asc") => (FileOrder::Name, false),
        Some("name_desc") => (FileOrder::Name, true),
        Some("modified_desc") => (FileOrder::Modified, true),
        Some("modified_asc") => (FileOrder::Modified, false),
        Some("created_desc") => (FileOrder::Created, true),
        Some("created_asc") => (FileOrder::Created, false),
        Some(o) => return Err(ToolError::arg(format!("unknown orderBy {o:?}"))),
    })
}

/// File cursor JSON: `[key, id]`.
pub(crate) fn file_cursor(v: &Value) -> ToolResult<FileCursor> {
    let bad = || ToolError::arg("cursor does not belong to this listing");
    let a = v.as_array().ok_or_else(bad)?;
    let id = a.get(1).and_then(Value::as_i64).ok_or_else(bad)?;
    let key = match a.first() {
        Some(Value::Number(n)) => SortKey::Int(n.as_i64().ok_or_else(bad)?),
        Some(Value::String(s)) => SortKey::Text(s.clone()),
        _ => SortKey::None,
    };
    Ok(FileCursor { key, id })
}

pub(crate) fn file_cursor_json(order: FileOrder, f: &pm_casedb::FileRecord) -> Value {
    match order.key(f) {
        SortKey::None => json!([null, f.id]),
        SortKey::Int(i) => json!([i, f.id]),
        SortKey::Text(s) => json!([s, f.id]),
    }
}

/// Runs a file listing and builds the page.
pub(crate) fn file_page(
    ctx: &Ctx<'_>,
    args: &Args,
    filter: &FileFilter,
    order: FileOrder,
    desc: bool,
    brief: bool,
) -> ToolResult<Page> {
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.map(|c| file_cursor(&c)).transpose()?;
    let rows = ctx
        .db()
        .files(filter, order, desc, after.as_ref(), limit + 1)?;
    Page::from_items(rows, limit, |f| {
        Ok(Row {
            value: if brief { file_brief(f) } else { file_row(f) },
            cursor: file_cursor_json(order, f),
        })
    })
}

fn query_files(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let mut f = FileFilter {
        ds_id: args.i64("dataSourceId")?,
        name_contains: args.str("nameContains")?,
        extensions: args.str("extension")?.into_iter().collect(),
        mime: args.str("mimeType")?,
        min_size: args.u64("minSize")?,
        max_size: args.u64("maxSize")?,
        path_contains: args.str("pathContains")?,
        allocated: args.bool("allocated")?,
        md5: hash_arg(args, "md5")?,
        sha256: hash_arg(args, "sha256")?,
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
        f.known = Some(known_arg(&k)?);
    }
    for (col, a, b) in [
        (TimeColumn::Modified, "modifiedAfter", "modifiedBefore"),
        (TimeColumn::Created, "createdAfter", "createdBefore"),
    ] {
        let (from, to) = (args.time(a, false)?, args.time(b, true)?);
        if from.is_some() || to.is_some() {
            f.times.push((col, from, to));
        }
    }
    let (order, desc) = order_arg(args.str("orderBy")?.as_deref())?;
    Ok(file_page(ctx, args, &f, order, desc, false)?.into())
}

// ---------------------------------------------------------------- artifacts

/// Common artifact listing for the two Autopsy tools.
pub(crate) fn artifact_page(
    ctx: &Ctx<'_>,
    args: &Args,
    filter: &ArtifactFilter,
) -> ToolResult<Page> {
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64());
    let rows = ctx.db().artifacts(filter, after, false, limit + 1)?;
    Page::from_items(rows, limit, |a| {
        Ok(Row {
            value: artifact_row(ctx, a, true)?,
            cursor: json!(a.id),
        })
    })
}

fn query_artifacts(ctx: &Ctx<'_>, args: &Args, analysis: bool) -> ToolResult<Output> {
    let mut f = ArtifactFilter {
        analysis: Some(analysis),
        ds_id: args.i64("dataSourceId")?,
        ..Default::default()
    };
    if let Some(t) = args.str("artifactType")? {
        match ctx.db().artifact_type_by_name(&t)? {
            Some((id, _, _)) => f.type_ids.push(id),
            // Unknown type: no results (Autopsy).
            None => {
                return Ok(Page::default()
                    .with("note", json!(format!("no artifact type {t}")))
                    .into());
            }
        }
    }
    let attr_type = match args.str("attributeType")? {
        Some(n) => match ctx.attribute_type(&n)? {
            Some((id, _)) => Some(id),
            None => {
                return Ok(Page::default()
                    .with("note", json!(format!("no attribute type {n}")))
                    .into());
            }
        },
        None => None,
    };
    match (attr_type, args.str("attributeValue")?) {
        (t, Some(v)) => f.attrs.push(AttrPredicate::TextContains {
            type_id: t,
            text: v,
        }),
        (Some(t), None) => f.attrs.push(AttrPredicate::Has { type_id: t }),
        (None, None) => {}
    }
    Ok(artifact_page(ctx, args, &f)?.into())
}

fn query_data_artifacts(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    query_artifacts(ctx, args, false)
}

fn query_analysis_results(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    query_artifacts(ctx, args, true)
}

// ------------------------------------------------------------- data sources

fn query_data_sources(ctx: &Ctx<'_>, _: &Args) -> ToolResult<Output> {
    let rows: Vec<Value> = ctx
        .db()
        .data_sources()?
        .iter()
        .map(data_source_row)
        .collect();
    Ok(Value::Array(rows).into())
}

fn get_hosts(ctx: &Ctx<'_>, _: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    let mut out = Vec::new();
    for h in db.hosts()? {
        let mut sources = Vec::new();
        for id in db.data_sources_of_host(h.id)? {
            if let Some(d) = db.data_source(id)? {
                sources.push(data_source_row(&d));
            }
        }
        out.push(json!({
            "id": h.id,
            "ref": Ref::Host(h.id).to_string(),
            "evidence": {"name": h.name},
            "dataSources": sources,
        }));
    }
    Ok(Value::Array(out).into())
}

fn fs_node(ctx: &Ctx<'_>, fs: &pm_casedb::FileSystemRecord) -> ToolResult<Value> {
    let root = ctx.db().fs_root(fs.id)?;
    let mut ev = Map::new();
    if let Some(l) = &fs.label {
        ev.insert("label".into(), json!(l));
    }
    if let Some(s) = &fs.serial {
        ev.insert("serial".into(), json!(s));
    }
    Ok(json!({
        "objectId": fs.id,
        "type": "FileSystem",
        "fsType": fs.kind,
        "imageOffset": fs.byte_offset,
        "blockSize": fs.block_size,
        "blockCount": fs.block_count,
        "rootInum": fs.root_inode,
        "rootRef": root.map(|r| Ref::File(r.id).to_string()),
        "evidence": ev,
    }))
}

fn ds_tree(ctx: &Ctx<'_>, d: &pm_casedb::DataSourceRecord) -> ToolResult<Value> {
    let db = ctx.db();
    let mut node = data_source_row(d);
    let all_fs = db.file_systems(d.id)?;
    let mut vs_nodes = Vec::new();
    for vs in db.volume_systems(d.id)? {
        let mut vols = Vec::new();
        for (vid, v) in &vs.volumes {
            let mut fss = Vec::new();
            for fs in all_fs.iter().filter(|f| f.volume_id == Some(*vid)) {
                fss.push(fs_node(ctx, fs)?);
            }
            let mut vol = json!({
                "objectId": vid,
                "type": "Volume",
                "addr": v.addr,
                "startSector": v.start_sector,
                "lengthSectors": v.sector_count,
                "size": v.sector_count.saturating_mul(u64::from(v.sector_size)),
                "flags": match v.kind { 0 => "Allocated", 1 => "Unallocated", _ => "Meta" },
                "fileSystems": fss,
            });
            if let Some(o) = vol.as_object_mut() {
                let mut ev = Map::new();
                ev.insert("description".into(), json!(v.description));
                if let Some(n) = &v.name {
                    ev.insert("name".into(), json!(n));
                }
                o.insert("evidence".into(), Value::Object(ev));
            }
            vols.push(vol);
        }
        vs_nodes.push(json!({
            "objectId": vs.id,
            "type": "VolumeSystem",
            "vsType": match vs.kind { 1 => "GPT", _ => "DOS" },
            "blockSize": vs.sector_size,
            "volumes": vols,
        }));
    }
    let mut direct = Vec::new();
    for fs in all_fs.iter().filter(|f| f.volume_id.is_none()) {
        direct.push(fs_node(ctx, fs)?);
    }
    if let Some(o) = node.as_object_mut() {
        o.insert("volumeSystems".into(), Value::Array(vs_nodes));
        o.insert("fileSystems".into(), Value::Array(direct));
    }
    Ok(node)
}

fn data_source_tree(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    if let Some(id) = args.i64("dataSourceId")? {
        let d = db
            .data_source(id)?
            .ok_or_else(|| ToolError::NotFound(format!("data source {id}")))?;
        return Ok(ds_tree(ctx, &d)?.into());
    }
    let mut out = Vec::new();
    for d in db.data_sources()? {
        out.push(ds_tree(ctx, &d)?);
    }
    Ok(Value::Array(out).into())
}

// --------------------------------------------------------------------- tags

fn query_tags(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    let tag_id = match args.str("tagName")? {
        Some(n) => match db
            .tag_names()?
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(&n))
        {
            Some(t) => Some(t.id),
            None => {
                return Ok(Page::default()
                    .with("note", json!(format!("no tag name {n}")))
                    .into());
            }
        },
        None => None,
    };
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| {
        let a = c.as_array()?;
        Some((
            u8::try_from(a.first()?.as_i64()?).ok()?,
            a.get(1)?.as_i64()?,
        ))
    });
    let rows = db.tags(tag_id, after, limit + 1)?;
    Page::from_items(rows, limit, |t| {
        Ok(match t {
            TagRow::File(t) => {
                let name = db.file(t.file_id)?.map(|f| f.name);
                Row {
                    value: json!({
                        "tagId": t.id,
                        "tagName": t.tag_name,
                        "itemType": "file",
                        "objectId": t.file_id,
                        "ref": Ref::File(t.file_id).to_string(),
                        "examiner": t.examiner,
                        "createdTime": iso_json(Some(t.created_at)),
                        "evidence": {"comment": t.comment, "itemName": name},
                    }),
                    cursor: json!([0, t.id]),
                }
            }
            TagRow::Artifact(t) => {
                let a = db.artifact(t.artifact_id)?;
                let (ty, analysis) = match &a {
                    Some(a) => {
                        let (n, _, an) = ctx.artifact_type(a.type_id)?;
                        (Some(n), an)
                    }
                    None => (None, false),
                };
                Row {
                    value: json!({
                        "tagId": t.id,
                        "tagName": t.tag_name,
                        "itemType": "artifact",
                        "objectId": t.artifact_id,
                        "ref": artifact_ref(t.artifact_id, analysis),
                        "itemName": ty,
                        "examiner": t.examiner,
                        "createdTime": iso_json(Some(t.created_at)),
                        "evidence": {"comment": t.comment},
                    }),
                    cursor: json!([1, t.id]),
                }
            }
        })
    })
    .map(Output::from)
}

// --------------------------------------------------------------- accounts

fn os_accounts(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64()).unwrap_or(0);
    let rows = db.os_accounts(args.i64("dataSourceId")?, after, limit + 1)?;
    Page::from_items(rows, limit, |a| {
        let attrs: Vec<Value> = db
            .os_account_attributes(a.id)?
            .iter()
            .map(|(attr, name, _)| {
                json!({"type": name, "value": crate::rows::db_value(&attr.value)})
            })
            .collect();
        let instances: Vec<Value> = db
            .os_account_instances(a.id)?
            .iter()
            .map(|i| json!({"dataSourceId": i.ds_id, "instanceType": i.kind.name()}))
            .collect();
        let realm = db.os_account_realm(a.realm_id).ok();
        Ok(Row {
            value: json!({
                "objectId": a.id,
                "ref": Ref::OsAccount(a.id).to_string(),
                "type": a.kind.map(pm_casedb::os_accounts::OsAccountType::name),
                "status": a.status.map(pm_casedb::os_accounts::OsAccountStatus::name),
                "admin": a.admin,
                "creationTime": iso_json(a.created_at.filter(|t| *t > 0)),
                "instances": instances,
                "evidence": {
                    "addr": a.addr,
                    "loginName": a.login_name,
                    "fullName": a.full_name,
                    "realm": realm.and_then(|r| r.name.or(r.addr)),
                    "extendedAttributes": attrs,
                },
            }),
            cursor: json!(a.id),
        })
    })
    .map(Output::from)
}

fn account_type_id(ctx: &Ctx<'_>, name: &str) -> ToolResult<i64> {
    ctx.db()
        .account_type(&name.to_ascii_uppercase())?
        .map(|t| t.id)
        .ok_or_else(|| ToolError::arg(format!("unknown account type {name}")))
}

fn comm_accounts(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let ty = args
        .str("accountType")?
        .map(|t| account_type_id(ctx, &t))
        .transpose()?;
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let after = args.cursor()?.and_then(|c| c.as_i64()).unwrap_or(0);
    let types: std::collections::HashMap<i64, String> = ctx
        .db()
        .account_types_in_use()?
        .into_iter()
        .map(|t| (t.id, t.display_name))
        .collect();
    let rows = ctx.db().accounts(ty, after, limit + 1)?;
    Page::from_items(rows, limit, |(a, n)| {
        Ok(Row {
            value: json!({
                "ref": Ref::Account(a.id).to_string(),
                "accountType": types.get(&a.type_id).cloned().unwrap_or_else(|| a.type_name.clone()),
                "accountTypeName": a.type_name,
                "relationshipCount": n,
                "evidence": {"accountId": a.identifier},
            }),
            cursor: json!(a.id),
        })
    })
    .map(Output::from)
}

/// Finds an account from `accountType` + `accountId` (or `ref`).
pub(crate) fn account_arg(
    ctx: &Ctx<'_>,
    args: &Args,
    ty_key: &str,
    id_key: &str,
) -> ToolResult<Option<pm_casedb::communications::Account>> {
    if let (Some(t), Some(i)) = (args.str(ty_key)?, args.str(id_key)?) {
        let t = t.to_ascii_uppercase();
        return Ok(ctx.db().account(&t, &i).ok().flatten());
    }
    Ok(None)
}

fn account_relationships(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    args.req_str("accountType")?;
    let id = args.req_str("accountId")?;
    let Some(acc) = account_arg(ctx, args, "accountType", "accountId")? else {
        return Ok(json!({"error": "Account not found", "evidence": {"accountId": id}}).into());
    };
    let related = ctx
        .db()
        .related_accounts(acc.id, &pm_casedb::communications::CommsFilter::default())?;
    let total: u64 = related.iter().map(|(_, n)| n).sum();
    let shown: Vec<Value> = related
        .iter()
        .take(MAX_LIMIT)
        .map(|(a, n)| {
            json!({
                "ref": Ref::Account(a.id).to_string(),
                "accountType": a.type_name,
                "relationshipCount": n,
                "evidence": {"accountId": a.identifier},
            })
        })
        .collect();
    Ok(json!({
        "ref": Ref::Account(acc.id).to_string(),
        "accountType": acc.type_name,
        "totalRelationships": total,
        "relatedAccountCount": related.len(),
        "relatedAccounts": shown,
        "relatedAccountsTruncated": related.len() > MAX_LIMIT,
        "evidence": {"account": acc.identifier},
    })
    .into())
}

// ------------------------------------------------------------ object tree

fn object_children(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let r = match (Ref::arg(args, "ref")?, args.i64("objectId")?) {
        (Some(r), _) => r,
        (None, Some(id)) => Ref::File(id),
        (None, None) => return Err(ToolError::arg("objectId or ref is required")),
    };
    let db = ctx.db();
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    let cursor = args.cursor()?;
    match r {
        Ref::File(id) => {
            let f = db
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(format!("file:{id}")))?;
            let parent = match f.parent_id {
                Some(p) => db.file(p)?.map(|p| file_brief(&p)),
                None => Some(
                    json!({"ref": Ref::DataSource(f.ds_id).to_string(), "objectType": "dataSource"}),
                ),
            };
            // Phase 0: child files by id; phase 1: artifacts of the file.
            let (phase, after) = cursor
                .as_ref()
                .and_then(|c| Some((c.get(0)?.as_i64()?, c.get(1)?.as_i64()?)))
                .unwrap_or((0, 0));
            let mut rows = Vec::new();
            let mut more = false;
            if phase == 0 {
                let kids = db.files(
                    &FileFilter {
                        parent_id: Some(id),
                        ..Default::default()
                    },
                    FileOrder::Id,
                    false,
                    (after > 0).then_some(&FileCursor {
                        key: SortKey::None,
                        id: after,
                    }),
                    limit + 1,
                )?;
                more = kids.len() > limit;
                for k in kids.iter().take(limit) {
                    let mut v = file_brief(k);
                    if let Some(o) = v.as_object_mut() {
                        o.insert("objectType".into(), json!("file"));
                    }
                    rows.push(Row {
                        value: v,
                        cursor: json!([0, k.id]),
                    });
                }
            }
            if !more && rows.len() < limit {
                let after_art = if phase == 1 { Some(after) } else { None };
                let left = limit - rows.len();
                let arts = db.artifacts(
                    &ArtifactFilter {
                        file_id: Some(id),
                        ..Default::default()
                    },
                    after_art,
                    false,
                    left + 1,
                )?;
                more = arts.len() > left;
                for a in arts.iter().take(left) {
                    let mut v = artifact_row(ctx, a, false)?;
                    if let Some(o) = v.as_object_mut() {
                        o.insert("objectType".into(), json!("artifact"));
                    }
                    rows.push(Row {
                        value: v,
                        cursor: json!([1, a.id]),
                    });
                }
            }
            Ok(Page {
                rows,
                more,
                extra: Map::new(),
            }
            .with("object", file_brief(&f))
            .with("parent", parent.unwrap_or(Value::Null))
            .into())
        }
        Ref::Artifact(id) => {
            let a = db
                .artifact(id)?
                .ok_or_else(|| ToolError::NotFound(format!("artifact:{id}")))?;
            let parent = match (a.source_artifact_id, a.file_id) {
                (Some(s), _) => db
                    .artifact(s)?
                    .map(|p| artifact_row(ctx, &p, false))
                    .transpose()?,
                (None, Some(f)) => db.file(f)?.map(|f| file_brief(&f)),
                _ => None,
            };
            let after = cursor.and_then(|c| c.as_i64());
            let kids = db.artifacts(
                &ArtifactFilter {
                    source_artifact_id: Some(id),
                    ..Default::default()
                },
                after,
                false,
                limit + 1,
            )?;
            Ok(Page::from_items(kids, limit, |k| {
                Ok(Row {
                    value: artifact_row(ctx, k, false)?,
                    cursor: json!(k.id),
                })
            })?
            .with("object", artifact_row(ctx, &a, true)?)
            .with("parent", parent.unwrap_or(Value::Null))
            .into())
        }
        Ref::DataSource(id) => {
            let d = db
                .data_source(id)?
                .ok_or_else(|| ToolError::NotFound(format!("ds:{id}")))?;
            let tree = ds_tree(ctx, &d)?;
            Ok(json!({"object": tree}).into())
        }
        other => Err(ToolError::arg(format!("{other} has no children"))),
    }
}

// ------------------------------------------------------------ file content

pub(crate) fn read_range(src: &dyn pm_core::ReadAt, offset: u64, len: u64) -> ToolResult<Vec<u8>> {
    let size = src.size();
    if offset >= size {
        return Ok(Vec::new());
    }
    let n = usize::try_from(len.min(size - offset)).unwrap_or(0);
    let mut buf = vec![0u8; n];
    let mut done = 0;
    while done < n {
        let got = src.read_at(
            offset + done as u64,
            buf.get_mut(done..).unwrap_or_default(),
        )?;
        if got == 0 {
            break;
        }
        done += got;
    }
    buf.truncate(done);
    Ok(buf)
}

fn read_args(args: &Args) -> ToolResult<(u64, u64)> {
    let offset = args.u64("offset")?.unwrap_or(0);
    let max = match args.i64("maxBytes")? {
        Some(n) if n > 0 => (n as u64).min(MAX_TEXT_READ),
        _ => 65_536,
    };
    Ok((offset, max))
}

fn file_content(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let id = args.req_i64("objectId")?;
    let f = ctx
        .db()
        .file(id)?
        .ok_or_else(|| ToolError::NotFound(format!("file:{id}")))?;
    ctx.check_content_allowed(id)?;
    let (offset, max) = read_args(args)?;
    let src = ctx.case().content(&f)?;
    let size = src.size();
    let buf = read_range(&*src, offset, max)?;
    let end = offset.saturating_add(buf.len() as u64);
    Ok(json!({
        "objectId": id,
        "ref": Ref::File(id).to_string(),
        "fileSize": size,
        "offset": offset,
        "bytesRead": buf.len(),
        "truncated": end < size,
        "eof": end >= size,
        "evidence": {"fileName": f.name, "content": String::from_utf8_lossy(&buf)},
    })
    .into())
}

// ------------------------------------------------------------------ reports

fn content_type(path: &str) -> &'static str {
    let p = path.to_ascii_lowercase();
    let ext = std::path::Path::new(&p)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("html" | "htm" | "xhtml")) {
        "text/html"
    } else {
        "text/plain"
    }
}

fn report_path(ctx: &Ctx<'_>, path: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.case().dir().join(p)
    }
}

fn list_reports(ctx: &Ctx<'_>, _: &Args) -> ToolResult<Output> {
    let rows: Vec<Value> = ctx
        .db()
        .report_rows()?
        .iter()
        .map(|r| {
            let size = std::fs::metadata(report_path(ctx, &r.path))
                .map(|m| m.len())
                .ok();
            json!({
                "objectId": r.id,
                "ref": Ref::Report(r.id).to_string(),
                "reportName": r.display_name,
                "sourceModule": r.module,
                "path": r.path,
                "createdTime": iso_json(Some(r.created_at)),
                "size": size,
                "contentType": content_type(&r.path),
            })
        })
        .collect();
    Ok(Value::Array(rows).into())
}

fn report_content(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let id = args.req_i64("objectId")?;
    let r = ctx
        .db()
        .report_rows()?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| ToolError::NotFound(format!("report {id}")))?;
    let (offset, max) = read_args(args)?;
    let path = report_path(ctx, &r.path);
    let src = pm_core::io::FileSource::open(&path)?;
    let size = pm_core::ReadAt::size(&src);
    let buf = read_range(&src, offset, max)?;
    let end = offset.saturating_add(buf.len() as u64);
    Ok(json!({
        "objectId": id,
        "ref": Ref::Report(id).to_string(),
        "reportName": r.display_name,
        "path": r.path,
        "fileSize": size,
        "contentType": content_type(&r.path),
        "offset": offset,
        "bytesRead": buf.len(),
        "truncated": end < size,
        "eof": end >= size,
        "evidence": {"content": String::from_utf8_lossy(&buf)},
    })
    .into())
}

/// Score class argument (`bad`, `suspicious`, `any`).
pub(crate) fn score_arg(s: &str) -> ToolResult<ScoreClass> {
    match s.to_ascii_lowercase().as_str() {
        "bad" | "notable" => Ok(ScoreClass::Bad),
        "suspicious" | "likely_notable" | "likely notable" => Ok(ScoreClass::Suspicious),
        "any" | "all" => Ok(ScoreClass::Any),
        _ => Err(ToolError::arg("score must be bad, suspicious or any")),
    }
}
