//! Content tools: full file metadata, extracted text pages, raw bytes,
//! strings and image previews.

use base64::Engine as _;
use pm_casedb::mcp::{ArtifactFilter, FileFilter, FileOrder};
use pm_casedb::{FileRecord, ObjKind};
use pm_search::{ObjRef, PageText};
use pm_text::{Chunk, ChunkingSink, Stop, StringSink};
use serde_json::{Map, Value, json};

use super::autopsy::read_range;
use super::{ToolSpec, tool};
use crate::args::{Args, Ref, hex, iso_json};
use crate::error::{ToolError, ToolResult};
use crate::response::Output;
use crate::rows::{artifact_row, file_brief, file_row};
use crate::server::Ctx;

/// Largest `read_file_bytes` read.
pub(crate) const MAX_BYTES: u64 = 65_536;
/// Largest range `get_file_strings` scans per call.
pub(crate) const MAX_STRINGS_SCAN: u64 = 16 * 1024 * 1024;
/// Largest strings output per call.
const MAX_STRINGS_OUT: usize = 128 * 1024;
/// Largest picture decoded for a preview.
const MAX_PREVIEW_INPUT: u64 = 64 * 1024 * 1024;

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "get_file_metadata",
            "Everything known about one file (Autopsy's File Metadata tab): all timestamps with nanoseconds, MFT \
             entry and sequence, alternate data streams, hashes, MIME type, extension mismatch, score with the \
             results that gave it, tags, owner, derived-from chain, file-system and data-source references.",
            &[("ref", "string", "File reference (file:123) or object id")],
            &["ref"],
            file_metadata,
        ),
        tool(
            "read_file_text",
            "Extracted text of a file or artifact, one page (Autopsy's Extracted Text viewer pages, about 32 KB each; \
             page 0 is the name). With query, starts at the first page with a hit and returns the hit byte ranges. \
             Files that were not indexed are extracted on the fly.",
            &[
                ("ref", "string", "file:123 or artifact:45"),
                (
                    "page",
                    "integer",
                    "Page number, default 1 (or the first hit page with query)",
                ),
                (
                    "query",
                    "string",
                    "Keyword to locate (same semantics as search_text)",
                ),
                (
                    "searchType",
                    "string",
                    "exact (default), substring or regex",
                ),
            ],
            &["ref"],
            read_file_text,
        ),
        tool(
            "read_file_bytes",
            "Raw bytes of a file at an offset, as hex (default) or base64, at most 65536 bytes per call. Cite \
             findings as file:123 @ 0x<offset>.",
            &[
                ("ref", "string", "file:123"),
                ("offset", "integer", "Byte offset, default 0"),
                (
                    "length",
                    "integer",
                    "Bytes to read, default 4096, max 65536",
                ),
                ("format", "string", "hex (default) or base64"),
            ],
            &["ref"],
            read_file_bytes,
        ),
        tool(
            "get_file_strings",
            "Printable strings in a byte range of a file (Autopsy's string extraction): Latin text by default, or \
             international scripts with UTF-8 / UTF-16. Scans at most 16 MiB per call; continue at nextOffset.",
            &[
                ("ref", "string", "file:123"),
                ("offset", "integer", "Start offset, default 0"),
                (
                    "length",
                    "integer",
                    "Bytes to scan, default 1048576, max 16777216",
                ),
                (
                    "scripts",
                    "array",
                    "Autopsy script names, e.g. [\"Latin - Basic\"] (default) or [\"Latin - Extended\", \"Cyrillic\", \"Arabic\"]",
                ),
                (
                    "utf16",
                    "boolean",
                    "Also find UTF-16 strings (international scan), default true",
                ),
            ],
            &["ref"],
            file_strings,
        ),
        tool(
            "get_image_preview",
            "A downscaled PNG of a picture file, returned as an image so a vision model can look at it. Disabled \
             unless the examiner allows image previews.",
            &[
                ("ref", "string", "file:123"),
                (
                    "maxSize",
                    "integer",
                    "Longest side in pixels, default 512, max 1024",
                ),
            ],
            &["ref"],
            image_preview,
        ),
    ]
}

fn file_arg(ctx: &Ctx<'_>, args: &Args) -> ToolResult<FileRecord> {
    match Ref::req(args, "ref")? {
        Ref::File(id) => ctx
            .db()
            .file(id)?
            .ok_or_else(|| ToolError::NotFound(format!("file:{id}"))),
        other => Err(ToolError::arg(format!("{other} is not a file reference"))),
    }
}

fn file_metadata(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let f = file_arg(ctx, args)?;
    let db = ctx.db();
    let mut v = file_row(&f);
    let mut times = Map::new();
    for (i, k) in ["modified", "accessed", "changed", "created"]
        .iter()
        .enumerate()
    {
        if let Some((s, ns)) = f.times.get(i).copied().flatten() {
            times.insert(
                (*k).into(),
                json!({"iso": iso_json(Some(s)), "epoch": s, "nanos": ns}),
            );
        }
    }
    let streams = match (f.fs_id, f.inode) {
        (Some(fs), Some(inode)) => db
            .mcp_files(
                &FileFilter {
                    inode: Some((fs, inode)),
                    exclude_id: Some(f.id),
                    ..Default::default()
                },
                FileOrder::Id,
                false,
                None,
                50,
            )?
            .iter()
            .map(file_brief)
            .collect(),
        _ => Vec::new(),
    };
    let score = db.score(ObjKind::File, f.id)?;
    let reasons: Vec<Value> = db
        .mcp_artifacts(
            &ArtifactFilter {
                file_id: Some(f.id),
                analysis: Some(true),
                ..Default::default()
            },
            None,
            false,
            50,
        )?
        .iter()
        .map(|a| artifact_row(ctx, a, true))
        .collect::<Result<_, _>>()?;
    let mismatch = reasons
        .iter()
        .any(|r| r["artifactType"] == "TSK_EXT_MISMATCH_DETECTED");
    let tags: Vec<Value> = db
        .file_tags(f.id)?
        .iter()
        .map(|t| {
            json!({"tagId": t.id, "tagName": t.tag_name, "examiner": t.examiner,
                   "createdTime": iso_json(Some(t.created_at)), "evidence": {"comment": t.comment}})
        })
        .collect();
    let (owner_uid, owner_account) = db.mcp_file_owner(f.id)?;
    let owner = match owner_account {
        Some(id) => db.os_account(id)?.map(|a| {
            json!({"ref": Ref::OsAccount(a.id).to_string(),
                   "evidence": {"loginName": a.login_name, "addr": a.addr}})
        }),
        None => None,
    };
    let mut chain = Vec::new();
    let mut cur = f.derived_from;
    while let Some(id) = cur {
        if chain.len() >= 16 {
            break;
        }
        match db.file(id)? {
            Some(p) => {
                cur = p.derived_from;
                chain.push(file_brief(&p));
            }
            None => break,
        }
    }
    let file_attrs: Vec<Value> = db
        .file_attribute_values(f.id)?
        .iter()
        .map(|(n, val)| json!({"type": n, "value": crate::rows::db_value(val)}))
        .collect();
    let artifacts = db
        .mcp_artifacts(
            &ArtifactFilter {
                file_id: Some(f.id),
                analysis: Some(false),
                ..Default::default()
            },
            None,
            false,
            1,
        )?
        .len();
    if let Some(o) = v.as_object_mut() {
        o.insert("times".into(), Value::Object(times));
        o.insert("fileSystemId".into(), json!(f.fs_id));
        o.insert(
            "dataSourceRef".into(),
            json!(Ref::DataSource(f.ds_id).to_string()),
        );
        o.insert(
            "parentRef".into(),
            json!(f.parent_id.map(|p| Ref::File(p).to_string())),
        );
        o.insert("stream".into(), json!(f.stream));
        o.insert("attributesFlags".into(), json!(f.attributes));
        o.insert("metaFlagsRaw".into(), json!(f.meta_flags));
        o.insert("alternateStreams".into(), Value::Array(streams));
        o.insert(
            "score".into(),
            json!({"significance": score.significance.name(),
                   "priority": if score.priority == pm_casedb::Priority::Override { "Override" } else { "Normal" }}),
        );
        o.insert("scoreReasons".into(), Value::Array(reasons));
        o.insert("extensionMismatch".into(), json!(mismatch));
        o.insert("tags".into(), Value::Array(tags));
        o.insert("owner".into(), owner.unwrap_or(Value::Null));
        o.insert("derivedFrom".into(), Value::Array(chain));
        o.insert("childCount".into(), json!(db.count_children(f.id)?));
        o.insert("hasDataArtifacts".into(), json!(artifacts > 0));
        if let Some(ev) = o.get_mut("evidence").and_then(Value::as_object_mut) {
            ev.insert("ownerUid".into(), json!(owner_uid));
            ev.insert("fileAttributes".into(), Value::Array(file_attrs));
        }
    }
    Ok(v.into())
}

/// Chunk `page` (1-based) of a file extracted now; `(text, base, done)`
/// where `done` says whether extraction finished (so `chunks` is the
/// page count).
fn extract_page(
    ctx: &Ctx<'_>,
    f: &FileRecord,
    page: u32,
) -> ToolResult<(Option<Chunk>, usize, bool)> {
    let src = ctx.case().content(f)?;
    let mime = if let Some(m) = &f.mime {
        m.clone()
    } else {
        let head = read_range(&*src, 0, 8192)?;
        pm_text::sniff_mime(&head, &f.name).to_owned()
    };
    let Some(kind) = pm_text::kind_for_mime(&mime, f.size) else {
        return Err(ToolError::Unavailable(format!(
            "no text extractor for {mime}; use get_file_strings or read_file_bytes"
        )));
    };
    let mut seen = 0u32;
    let mut found = None;
    let mut sink = ChunkingSink::new(|c: Chunk| {
        seen += 1;
        if seen == page {
            found = Some(c);
            return Err(Stop);
        }
        Ok(())
    });
    let opts = pm_text::ExtractOptions {
        temp_dir: Some(ctx.case().temp_dir()),
        sqlite_wal: None,
    };
    let res = pm_text::extract(kind, &*src, &mut sink, &opts);
    let finished = sink.finish();
    let done = res.is_ok() && finished.is_ok() && found.is_none();
    if let Err(e) = res
        && found.is_none()
        && !matches!(e, pm_text::TextError::Stopped)
    {
        tracing::debug!("extraction of file {} failed: {e}", f.id);
    }
    let total = seen as usize;
    Ok((found, total, done))
}

fn read_file_text(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let r = Ref::req(args, "ref")?;
    let (obj, file) = match r {
        Ref::File(id) => {
            let f = ctx
                .db()
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            ctx.check_content_allowed(id)?;
            (ObjRef::File(id), Some(f))
        }
        Ref::Artifact(id) => (ObjRef::Artifact(id), None),
        other => return Err(ToolError::arg(format!("{other} has no text"))),
    };
    let query = args.str("query")?;
    let st = super::search::search_type_arg(args.str("searchType")?.as_deref())?;
    let explicit_page = args
        .u64("page")?
        .map(|p| u32::try_from(p).unwrap_or(u32::MAX));
    let ix = ctx.index().ok();
    let pages = match ix {
        Some(ix) => ix.content().num_chunks(obj)?,
        None => None,
    };
    if let (Some(ix), Some(n)) = (ix, pages) {
        let hits = match &query {
            Some(q) => ix.content().object_hits(
                obj,
                &pm_search::TextQuery {
                    term: q.clone(),
                    search_type: st,
                    ds_ids: Vec::new(),
                },
            )?,
            None => Vec::new(),
        };
        let page = explicit_page
            .or_else(|| hits.first().map(|h| h.page))
            .unwrap_or(u32::from(n > 0));
        let Some(PageText { text, base }) = ix.content().page(obj, page)? else {
            return Err(ToolError::NotFound(format!(
                "{r} page {page} (pages 0..={n})"
            )));
        };
        let ranges: Vec<Value> = hits
            .iter()
            .filter(|h| h.page == page)
            .flat_map(|h| h.ranges.iter().map(|x| json!([x.start, x.end])))
            .collect();
        let hit_pages: Vec<u32> = hits.iter().map(|h| h.page).collect();
        return Ok(json!({
            "ref": r.to_string(),
            "indexed": true,
            "page": page,
            "pages": n,
            "overlapStartByte": base.min(text.len()),
            "hitPages": hit_pages,
            "hitRanges": ranges,
            "evidence": {"text": text},
        })
        .into());
    }
    let Some(f) = file else {
        return Err(ToolError::Unavailable(format!(
            "{r} is not in the keyword index (Keyword Search has not indexed artifacts); use get_artifact"
        )));
    };
    let page = explicit_page.unwrap_or(1);
    if page == 0 {
        return Ok(json!({"ref": r.to_string(), "indexed": false, "page": 0, "evidence": {"text": f.name}}).into());
    }
    let (chunk, seen, done) = extract_page(ctx, &f, page)?;
    let Some(c) = chunk else {
        return Err(ToolError::NotFound(format!("{r} has {seen} text page(s)")));
    };
    let mut ranges = Vec::new();
    if let Some(q) = &query
        && let Ok(m) = pm_search::Matcher::single(q, st)
    {
        // Offsets in the lower-cased text equal those in the text only
        // when lower-casing kept every length (ASCII and most scripts).
        if c.lower.len() == c.text.len() {
            for e in 0..m.entries().len() {
                for h in m.occurrences(e, &c.lower) {
                    ranges.push(json!([h.start, h.end]));
                }
            }
        }
    }
    Ok(json!({
        "ref": r.to_string(),
        "indexed": false,
        "page": page,
        "pages": if done { Some(seen) } else { None },
        "overlapStartByte": c.base_bytes,
        "hitRanges": ranges,
        "evidence": {"text": c.text},
    })
    .into())
}

fn read_file_bytes(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let f = file_arg(ctx, args)?;
    ctx.check_content_allowed(f.id)?;
    let offset = args.u64("offset")?.unwrap_or(0);
    let len = args.u64("length")?.unwrap_or(4096).clamp(1, MAX_BYTES);
    let base64 = match args
        .str("format")?
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("hex") => false,
        Some("base64") => true,
        Some(o) => {
            return Err(ToolError::arg(format!(
                "format must be hex or base64, not {o:?}"
            )));
        }
    };
    let src = ctx.case().content(&f)?;
    let size = src.size();
    let buf = read_range(&*src, offset, len)?;
    let data = if base64 {
        base64::engine::general_purpose::STANDARD.encode(&buf)
    } else {
        hex(&buf)
    };
    let end = offset.saturating_add(buf.len() as u64);
    Ok(json!({
        "ref": Ref::File(f.id).to_string(),
        "offset": offset,
        "offsetHex": format!("0x{offset:x}"),
        "length": buf.len(),
        "fileSize": size,
        "eof": end >= size,
        "format": if base64 { "base64" } else { "hex" },
        "evidence": {"data": data},
    })
    .into())
}

fn file_strings(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let f = file_arg(ctx, args)?;
    ctx.check_content_allowed(f.id)?;
    let offset = args.u64("offset")?.unwrap_or(0);
    let len = args
        .u64("length")?
        .unwrap_or(1 << 20)
        .clamp(1, MAX_STRINGS_SCAN);
    let names = args.str_list("scripts")?;
    let mut scripts = Vec::new();
    for n in &names {
        scripts.push(
            pm_text::strings::Script::from_name(n)
                .ok_or_else(|| ToolError::arg(format!("unknown script {n:?}")))?,
        );
    }
    if scripts.is_empty() {
        scripts.push(pm_text::strings::Script::Latin1);
    }
    let cfg = pm_text::strings::StringsConfig {
        utf8: true,
        utf16: args.bool("utf16")?.unwrap_or(true),
        scripts,
    };
    let src = ctx.case().content(&f)?;
    let size = src.size();
    let window = pm_core::io::SliceSource::new(src, offset, len);
    let scanned = pm_core::ReadAt::size(&window);
    let mut sink = StringSink::with_limit(MAX_STRINGS_OUT);
    let res = pm_text::strings::extract(&window, &cfg, &mut sink);
    let cut = sink.text.len() >= MAX_STRINGS_OUT;
    if let Err(e) = res
        && !cut
        && !matches!(e, pm_text::TextError::Stopped)
    {
        return Err(ToolError::Unavailable(format!(
            "string extraction failed: {e}"
        )));
    }
    let end = offset.saturating_add(scanned);
    Ok(json!({
        "ref": Ref::File(f.id).to_string(),
        "offset": offset,
        "scannedBytes": scanned,
        "fileSize": size,
        "outputTruncated": cut,
        "nextOffset": if end < size { Some(end) } else { None },
        "evidence": {"strings": sink.text},
    })
    .into())
}

fn image_preview(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    if !ctx.config().image_previews {
        return Err(ToolError::Refused(
            "image previews are disabled; the examiner can enable them in Options › AI: MCP Server"
                .into(),
        ));
    }
    let f = file_arg(ctx, args)?;
    ctx.check_content_allowed(f.id)?;
    if f.size > MAX_PREVIEW_INPUT {
        return Err(ToolError::Refused(format!(
            "file is larger than {MAX_PREVIEW_INPUT} bytes"
        )));
    }
    let max = args.u64("maxSize")?.unwrap_or(512).clamp(16, 1024) as u32;
    let src = ctx.case().content(&f)?;
    let data = read_range(&*src, 0, f.size)?;
    let img = image::load_from_memory(&data)
        .map_err(|e| ToolError::Unavailable(format!("not a decodable picture: {e}")))?;
    let (w, h) = (img.width(), img.height());
    let thumb = img.thumbnail(max, max);
    let mut png = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| ToolError::Unavailable(format!("cannot encode preview: {e}")))?;
    Ok(Output::Image {
        meta: json!({
            "ref": Ref::File(f.id).to_string(),
            "width": w,
            "height": h,
            "previewWidth": thumb.width(),
            "previewHeight": thumb.height(),
            "mimeType": f.mime,
            "evidence": {"name": f.name},
        }),
        png,
    })
}
