//! Timeline tools. Stored artifact events and the virtual file-system
//! events (ADR-018) are merged into one stream ordered by
//! `(time, source, id)` and paged with a keyset cursor over that triple.

use std::collections::{HashMap, HashSet};

use pm_casedb::FileRecord;
use pm_casedb::events::EventRecord;
use pm_casedb::mcp::{ArtifactFilter, EventFilter, EventKeyset, TimeColumn};
use serde_json::{Value, json};

use super::{CURSOR, DS, LIMIT, ToolSpec, tool};
use crate::args::{Args, MAX_LIMIT, Ref, iso_json};
use crate::error::{ToolError, ToolResult};
use crate::response::{Output, Page, Row};
use crate::rows::artifact_ref;
use crate::server::Ctx;

pub(super) fn tools() -> Vec<ToolSpec> {
    let filters: &[super::Param] = &[
        (
            "startTime",
            "string",
            "ISO 8601 start (inclusive); default: the start of the case's activity",
        ),
        (
            "endTime",
            "string",
            "ISO 8601 end (inclusive); default: the end of the case's activity",
        ),
        (
            "eventTypes",
            "array",
            "Event types or categories, e.g. [\"Web History Accessed\", \"Program Run\", \"File System\", \"Web Activity\"]; default all",
        ),
        DS,
        ("text", "string", "Description (or file path) substring"),
        (
            "taggedOnly",
            "boolean",
            "Only events whose file or result is tagged",
        ),
        (
            "hashHitsOnly",
            "boolean",
            "Only events whose file has a hash-set hit",
        ),
    ];
    let mut events_params = filters.to_vec();
    events_params.extend([
        ("order", "string", "oldest (default) or newest first"),
        LIMIT,
        CURSOR,
    ]);
    let mut hist_params = filters.to_vec();
    hist_params.push((
        "buckets",
        "integer",
        "Number of equal time buckets, default 24, max 200",
    ));
    vec![
        tool(
            "timeline_events",
            "Timeline events with filters: event types or categories, time window, data source, text, tagged / \
             hash-hit only; oldest or newest first. File-system events are the MACB times of files; the others come \
             from artifacts (web history, downloads, program runs, devices, messages, …). Each row cites its event, \
             file and artifact references.",
            &events_params,
            &[],
            timeline_events,
        ),
        tool(
            "timeline_histogram",
            "Event counts in equal time buckets per category (File System, Web Activity, Other) to find the busy \
             periods before listing events.",
            &hist_params,
            &[],
            timeline_histogram,
        ),
        tool(
            "events_around",
            "Events within ±minutes of a time or of an object (\"what happened around this download?\"): give time, \
             or ref (event:…, artifact:… uses its event or date, file:… its modified time).",
            &[
                ("time", "string", "ISO 8601 centre time"),
                (
                    "ref",
                    "string",
                    "Reference whose time is the centre (event:900, artifact:45, file:123)",
                ),
                (
                    "minutes",
                    "integer",
                    "Window half-width in minutes, default 10, max 10080",
                ),
                (
                    "eventTypes",
                    "array",
                    "Event types or categories (default all)",
                ),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            events_around,
        ),
    ]
}

/// Super-type (category) ids.
const FILE_SYSTEM: i64 = 1;

/// The event type table: id → (name, super type id).
fn event_types(ctx: &Ctx<'_>) -> ToolResult<HashMap<i64, (String, Option<i64>)>> {
    Ok(ctx
        .db()
        .event_types()?
        .into_iter()
        .map(|t| (t.id, (t.name, t.super_type_id)))
        .collect())
}

/// Category (level-1 type) of an event type.
fn category(types: &HashMap<i64, (String, Option<i64>)>, mut id: i64) -> i64 {
    for _ in 0..8 {
        match types.get(&id) {
            Some((_, Some(p))) if *p != 0 => id = *p,
            _ => return id,
        }
    }
    id
}

/// Resolves `eventTypes` (names, ids or categories) to leaf type ids.
fn type_filter(
    args: &Args,
    key: &str,
    types: &HashMap<i64, (String, Option<i64>)>,
) -> ToolResult<Option<HashSet<i64>>> {
    let wanted = args.str_list(key)?;
    if wanted.is_empty() {
        return Ok(None);
    }
    let mut out = HashSet::new();
    for w in wanted {
        let ids: Vec<i64> = match w.parse::<i64>() {
            Ok(id) => vec![id],
            Err(_) => types
                .iter()
                .filter(|(_, (n, _))| n.eq_ignore_ascii_case(&w))
                .map(|(id, _)| *id)
                .collect(),
        };
        if ids.is_empty() {
            return Err(ToolError::arg(format!(
                "unknown event type {w:?}; categories are File System, Web Activity, Other"
            )));
        }
        for id in ids {
            out.insert(id);
            // A category stands for its children.
            out.extend(
                types
                    .iter()
                    .filter(|(_, (_, p))| *p == Some(id))
                    .map(|(c, _)| *c),
            );
        }
    }
    Ok(Some(out))
}

#[derive(Debug)]
enum Ev {
    Stored(EventRecord),
    File(TimeColumn, Box<FileRecord>, i64),
}

impl Ev {
    fn key(&self) -> (i64, u8, i64) {
        match self {
            Self::Stored(e) => (e.time, 0, e.id),
            Self::File(c, f, t) => (*t, rank(*c), f.id),
        }
    }
}

fn rank(c: TimeColumn) -> u8 {
    match c {
        TimeColumn::Modified => 1,
        TimeColumn::Accessed => 2,
        TimeColumn::Changed => 3,
        TimeColumn::Created => 4,
    }
}

fn keyset(cursor: Option<(i64, u8, i64)>, r: u8, desc: bool) -> EventKeyset {
    match cursor {
        None => EventKeyset::Start,
        Some((t, cr, id)) => {
            if r == cr {
                EventKeyset::After { time: t, id }
            } else if (r < cr) != desc {
                EventKeyset::AfterTime(t)
            } else {
                EventKeyset::FromTime(t)
            }
        }
    }
}

/// One merged page (up to `limit + 1` rows).
fn merged(
    ctx: &Ctx<'_>,
    f: &EventFilter,
    types: Option<&HashSet<i64>>,
    cursor: Option<(i64, u8, i64)>,
    desc: bool,
    limit: usize,
) -> ToolResult<Vec<Ev>> {
    let db = ctx.db();
    let mut all = Vec::new();
    let stored_types: Vec<i64> = types
        .map(|t| {
            t.iter()
                .copied()
                .filter(|id| !matches!(*id, 0 | 1 | 4..=7))
                .collect()
        })
        .unwrap_or_default();
    let want_stored = types.is_none_or(|_| !stored_types.is_empty());
    if want_stored {
        all.extend(
            db.mcp_events(&stored_types, f, keyset(cursor, 0, desc), desc, limit + 1)?
                .into_iter()
                .map(Ev::Stored),
        );
    }
    if f.artifact_id.is_none() {
        for col in TimeColumn::ALL {
            if types.is_some_and(|t| !t.contains(&col.event_type())) {
                continue;
            }
            for rec in
                db.mcp_file_events(col, f, keyset(cursor, rank(col), desc), desc, limit + 1)?
            {
                let t = rec
                    .times
                    .get(col.index())
                    .copied()
                    .flatten()
                    .map_or(0, |x| x.0);
                all.push(Ev::File(col, Box::new(rec), t));
            }
        }
    }
    all.sort_by_key(Ev::key);
    if desc {
        all.reverse();
    }
    all.truncate(limit + 1);
    Ok(all)
}

fn event_row(
    ctx: &Ctx<'_>,
    e: &Ev,
    types: &HashMap<i64, (String, Option<i64>)>,
) -> ToolResult<Value> {
    let name = |id: i64| types.get(&id).map(|t| t.0.clone());
    Ok(match e {
        Ev::Stored(e) => {
            let analysis = match e.artifact_id {
                Some(a) => match ctx.db().artifact(a)? {
                    Some(r) => ctx.artifact_type(r.type_id)?.2,
                    None => false,
                },
                None => false,
            };
            json!({
                "ref": Ref::Event(e.id).to_string(),
                "time": iso_json(Some(e.time)),
                "timeEpoch": e.time,
                "eventType": name(e.type_id),
                "eventTypeId": e.type_id,
                "category": name(category(types, e.type_id)),
                "sourceFileId": e.file_id,
                "fileRef": e.file_id.map(|f| Ref::File(f).to_string()),
                "artifactId": e.artifact_id,
                "artifactRef": e.artifact_id.map(|a| artifact_ref(a, analysis)),
                "dataSourceId": e.ds_id,
                "tagged": e.tagged,
                "hashHit": e.hash_hit,
                "evidence": {"description": e.full_description},
            })
        }
        Ev::File(col, f, t) => {
            let tagged = !ctx.db().file_tags(f.id)?.is_empty();
            let hash_hit = !ctx
                .db()
                .mcp_artifacts(
                    &ArtifactFilter {
                        file_id: Some(f.id),
                        type_ids: vec![10],
                        ..Default::default()
                    },
                    None,
                    false,
                    1,
                )?
                .is_empty();
            json!({
                "ref": Ref::File(f.id).to_string(),
                "time": iso_json(Some(*t)),
                "timeEpoch": t,
                "eventType": name(col.event_type()),
                "eventTypeId": col.event_type(),
                "category": name(FILE_SYSTEM),
                "sourceFileId": f.id,
                "fileRef": Ref::File(f.id).to_string(),
                "artifactId": null,
                "dataSourceId": f.ds_id,
                "tagged": tagged,
                "hashHit": hash_hit,
                "deleted": f.deleted(),
                "evidence": {"description": f.path()},
            })
        }
    })
}

fn timeline_cursor(args: &Args) -> ToolResult<Option<(i64, u8, i64)>> {
    match args.cursor()? {
        None => Ok(None),
        Some(c) => {
            let a = c.as_array();
            let get = |i: usize| a.and_then(|a| a.get(i)).and_then(Value::as_i64);
            match (get(0), get(1).and_then(|r| u8::try_from(r).ok()), get(2)) {
                (Some(t), Some(r), Some(id)) => Ok(Some((t, r, id))),
                _ => Err(ToolError::arg("cursor does not belong to this listing")),
            }
        }
    }
}

/// `[start, end)` from `startTime`/`endTime` (inclusive end).
fn window(args: &Args) -> ToolResult<(Option<i64>, Option<i64>)> {
    let start = args.time("startTime", false)?;
    let end = args.time("endTime", true)?.map(|e| e.saturating_add(1));
    Ok((start, end))
}

fn event_filter(args: &Args, text_key: &str) -> ToolResult<EventFilter> {
    let (start, end) = window(args)?;
    Ok(EventFilter {
        start,
        end,
        ds_id: args.i64("dataSourceId")?,
        text: args.str(text_key)?,
        tagged_only: args.flag("taggedOnly")?,
        hash_hit_only: args.flag("hashHitsOnly")?,
        ..EventFilter::default()
    })
}

fn event_page(
    ctx: &Ctx<'_>,
    args: &Args,
    f: &EventFilter,
    types: Option<&HashSet<i64>>,
    desc: bool,
    limit: usize,
) -> ToolResult<Page> {
    let names = event_types(ctx)?;
    let cursor = timeline_cursor(args)?;
    let evs = merged(ctx, f, types, cursor, desc, limit)?;
    Page::from_items(evs, limit, |e| {
        let (t, r, id) = e.key();
        Ok(Row {
            value: event_row(ctx, e, &names)?,
            cursor: json!([t, r, id]),
        })
    })
}

pub(super) fn query_timeline(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let limit = args.limit(100, MAX_LIMIT)?;
    let f = event_filter(args, "textFilter")?;
    Ok(event_page(ctx, args, &f, None, false, limit)?.into())
}

fn timeline_events(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let limit = args.limit(crate::args::DEFAULT_LIMIT, MAX_LIMIT)?;
    let names = event_types(ctx)?;
    let types = type_filter(args, "eventTypes", &names)?;
    let desc = match args
        .str("order")?
        .map(|o| o.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("oldest" | "asc" | "oldest_first") => false,
        Some("newest" | "desc" | "newest_first") => true,
        Some(o) => {
            return Err(ToolError::arg(format!(
                "order must be oldest or newest, not {o:?}"
            )));
        }
    };
    let f = event_filter(args, "text")?;
    Ok(event_page(ctx, args, &f, types.as_ref(), desc, limit)?.into())
}

/// Counts per category over `[start, end)` in `buckets` buckets.
fn histogram(
    ctx: &Ctx<'_>,
    f: &EventFilter,
    types: Option<&HashSet<i64>>,
    start: i64,
    end: i64,
    buckets: i64,
) -> ToolResult<Vec<HashMap<i64, u64>>> {
    let names = event_types(ctx)?;
    let span = end.saturating_sub(start).max(1);
    let width = (span + buckets - 1) / buckets;
    let n = usize::try_from(buckets).unwrap_or(1);
    let mut out: Vec<HashMap<i64, u64>> = vec![HashMap::new(); n];
    let mut f = f.clone();
    f.start = Some(start);
    f.end = Some(end);
    for (b, ty, c) in ctx.db().mcp_event_histogram(&f, start, width)? {
        if types.is_some_and(|t| !t.contains(&ty)) {
            continue;
        }
        if let Some(slot) = usize::try_from(b).ok().and_then(|b| out.get_mut(b)) {
            *slot.entry(category(&names, ty)).or_default() += c;
        }
    }
    for col in TimeColumn::ALL {
        if types.is_some_and(|t| !t.contains(&col.event_type())) {
            continue;
        }
        for (b, c) in ctx.db().mcp_file_event_histogram(col, &f, start, width)? {
            if let Some(slot) = usize::try_from(b).ok().and_then(|b| out.get_mut(b)) {
                *slot.entry(FILE_SYSTEM).or_default() += c;
            }
        }
    }
    Ok(out)
}

fn span_or_window(ctx: &Ctx<'_>, f: &EventFilter) -> ToolResult<Option<(i64, i64)>> {
    let span = ctx.db().mcp_time_span()?;
    let start = f.start.or(span.map(|s| s.0));
    let end = f.end.or(span.map(|s| s.1.saturating_add(1)));
    Ok(start.zip(end).filter(|(a, b)| b > a))
}

fn category_names(
    ctx: &Ctx<'_>,
    counts: &HashMap<i64, u64>,
) -> ToolResult<serde_json::Map<String, Value>> {
    let names = event_types(ctx)?;
    let mut m = serde_json::Map::new();
    for id in [1, 2, 3] {
        let name = names
            .get(&id)
            .map_or_else(|| id.to_string(), |n| n.0.clone());
        m.insert(name, json!(counts.get(&id).copied().unwrap_or(0)));
    }
    Ok(m)
}

pub(super) fn summarize_timeline(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let f = event_filter(args, "textFilter")?;
    let Some((start, end)) = span_or_window(ctx, &f)? else {
        return Ok(json!({"totalEvents": 0, "byCategory": {}}).into());
    };
    let h = histogram(ctx, &f, None, start, end, 1)?;
    let counts = h.into_iter().next().unwrap_or_default();
    let total: u64 = counts.values().sum();
    Ok(json!({
        "startTime": iso_json(Some(start)),
        "startEpoch": start,
        "endTime": iso_json(Some(end - 1)),
        "endEpoch": end - 1,
        "totalEvents": total,
        "byCategory": category_names(ctx, &counts)?,
    })
    .into())
}

fn timeline_histogram(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let names = event_types(ctx)?;
    let types = type_filter(args, "eventTypes", &names)?;
    let f = event_filter(args, "text")?;
    let buckets = args.i64("buckets")?.unwrap_or(24).clamp(1, 200);
    let Some((start, end)) = span_or_window(ctx, &f)? else {
        return Ok(Value::Array(Vec::new()).into());
    };
    let width = (end - start + buckets - 1) / buckets;
    let h = histogram(ctx, &f, types.as_ref(), start, end, buckets)?;
    let mut rows = Vec::new();
    for (i, counts) in h.iter().enumerate() {
        let b0 = start + width * i as i64;
        let b1 = (b0 + width).min(end);
        if b0 >= end {
            break;
        }
        rows.push(json!({
            "start": iso_json(Some(b0)),
            "startEpoch": b0,
            "end": iso_json(Some(b1)),
            "endEpoch": b1,
            "total": counts.values().sum::<u64>(),
            "byCategory": category_names(ctx, counts)?,
        }));
    }
    Ok(json!({"bucketSeconds": width, "buckets": rows}).into())
}

/// The time an object stands for.
fn ref_time(ctx: &Ctx<'_>, r: Ref) -> ToolResult<i64> {
    let db = ctx.db();
    match r {
        Ref::Event(id) => db
            .mcp_event(id)?
            .map(|e| e.time)
            .ok_or_else(|| ToolError::NotFound(r.to_string())),
        Ref::Artifact(id) => {
            if let Some(e) = db.events_of_artifact(id)?.first() {
                return Ok(e.time);
            }
            db.attributes(id)?
                .iter()
                .find_map(|a| match a.value {
                    pm_casedb::Value::DateTime(t) if t > 0 => Some(t),
                    _ => None,
                })
                .ok_or_else(|| ToolError::NotFound(format!("{r} has no time")))
        }
        Ref::File(id) => {
            let f = db
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            f.times
                .iter()
                .flatten()
                .map(|t| t.0)
                .next()
                .ok_or_else(|| ToolError::NotFound(format!("{r} has no time")))
        }
        _ => Err(ToolError::arg(format!("{r} has no time"))),
    }
}

fn events_around(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let centre = match (args.time("time", false)?, Ref::arg(args, "ref")?) {
        (Some(t), _) => t,
        (None, Some(r)) => ref_time(ctx, r)?,
        (None, None) => return Err(ToolError::arg("time or ref is required")),
    };
    let minutes = args.i64("minutes")?.unwrap_or(10).clamp(0, 10_080);
    let names = event_types(ctx)?;
    let types = type_filter(args, "eventTypes", &names)?;
    let f = EventFilter {
        start: Some(centre.saturating_sub(minutes * 60)),
        end: Some(centre.saturating_add(minutes * 60 + 1)),
        ds_id: args.i64("dataSourceId")?,
        ..Default::default()
    };
    let limit = args.limit(crate::args::DEFAULT_LIMIT, MAX_LIMIT)?;
    Ok(event_page(ctx, args, &f, types.as_ref(), false, limit)?
        .with("centre", iso_json(Some(centre)))
        .with("centreEpoch", json!(centre))
        .with("minutes", json!(minutes))
        .into())
}
