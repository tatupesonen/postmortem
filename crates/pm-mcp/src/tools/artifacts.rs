//! Artifact, pivot, communications, places and coverage tools.

use pm_casedb::ObjKind;
use pm_casedb::query::{
    ArtifactFilter, AttrPredicate, EventFilter, EventKeyset, FileCursor, FileFilter, FileOrder,
    SortKey,
};
use serde_json::{Map, Value, json};

use super::autopsy::{account_arg, artifact_page};
use super::{CURSOR, DS, LIMIT, ToolSpec, tool};
use crate::args::{Args, DEFAULT_LIMIT, MAX_LIMIT, Ref, iso_json};
use crate::error::{ToolError, ToolResult};
use crate::response::{Output, Page, Row};
use crate::rows::{artifact_ref, artifact_row, file_brief};
use crate::server::Ctx;

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![
        tool(
            "list_artifact_types",
            "The data-artifact and analysis-result types present in the case with their counts (Autopsy's result \
             tree), so you can see what exists before querying.",
            &[DS],
            &[],
            list_artifact_types,
        ),
        tool(
            "get_artifact",
            "One artifact or analysis result with all attributes, its source file, score, tags, linked artifacts, \
             related communication accounts and timeline events.",
            &[("ref", "string", "artifact:45 or result:46")],
            &["ref"],
            get_artifact,
        ),
        tool(
            "get_related",
            "Pivot from a reference: same_hash (other files with the same content), same_folder (siblings), \
             artifacts (results and artifacts of a file, or results about an artifact), accounts (communication \
             accounts of an artifact), events (timeline events of a file or artifact). relation=all (default) returns \
             the first rows of each; a single relation is paged.",
            &[
                ("ref", "string", "file:123 or artifact:45"),
                (
                    "relation",
                    "string",
                    "all (default), same_hash, same_folder, artifacts, accounts or events",
                ),
                LIMIT,
                CURSOR,
            ],
            &["ref"],
            get_related,
        ),
        tool(
            "list_notable",
            "Autopsy's Score tree: files and data artifacts scored Notable (bad) or Likely Notable (suspicious), each \
             with the analysis results that explain the score (hash-set hits, interesting items, keyword hits, …).",
            &[
                ("score", "string", "bad (default), suspicious or any"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            list_notable,
        ),
        tool(
            "query_event_logs",
            "Windows event-log entries parsed by the Windows Event Logs module (EvtxECmd maps): filter by channel, \
             event ID, provider, time window, text and data source. Logons, RDP, services, PowerShell, USB, log \
             clearing, …",
            &[
                (
                    "channel",
                    "string",
                    "Channel, e.g. Security or Microsoft-Windows-TerminalServices-LocalSessionManager/Operational",
                ),
                ("eventId", "integer", "Event ID, e.g. 4624"),
                ("provider", "string", "Provider name substring"),
                ("startTime", "string", "ISO 8601 (inclusive)"),
                ("endTime", "string", "ISO 8601 (inclusive)"),
                ("text", "string", "Substring of any field"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            query_event_logs,
        ),
        tool(
            "get_messages",
            "Messages and e-mails: of a conversation thread (threadId), between two accounts, or of one account \
             (accountType + accountId, optionally otherAccountType + otherAccountId), oldest first. Without filters \
             all messages are listed by id.",
            &[
                ("accountType", "string", "e.g. PHONE, EMAIL"),
                ("accountId", "string", "The account identifier"),
                ("otherAccountType", "string", "Type of the other party"),
                ("otherAccountId", "string", "The other party's identifier"),
                ("threadId", "string", "KAAMOS_THREAD_ID of the conversation"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            get_messages,
        ),
        tool(
            "get_contacts",
            "Contact-book entries (KAAMOS_CONTACT), optionally containing a text.",
            &[
                ("text", "string", "Substring of any field"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            get_contacts,
        ),
        tool(
            "geolocation_points",
            "Geolocated artifacts (GPS points, bookmarks, searches, last known locations, routes, EXIF positions) in \
             a bounding box and time window.",
            &[
                ("minLatitude", "number", "South edge"),
                ("maxLatitude", "number", "North edge"),
                ("minLongitude", "number", "West edge"),
                ("maxLongitude", "number", "East edge"),
                ("startTime", "string", "ISO 8601 (inclusive)"),
                ("endTime", "string", "ISO 8601 (inclusive)"),
                DS,
                LIMIT,
                CURSOR,
            ],
            &[],
            geolocation_points,
        ),
        tool(
            "list_ingest_jobs",
            "Coverage: the ingest jobs run per data source with their modules, settings, start/end and status. Use \
             it to say \"Keyword Search was not run\" instead of \"no hits\".",
            &[DS],
            &[],
            list_ingest_jobs,
        ),
    ]
}

fn list_artifact_types(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let rows: Vec<Value> = ctx
        .db()
        .artifact_type_counts(args.i64("dataSourceId")?)?
        .into_iter()
        .map(|t| {
            json!({
                "artifactType": t.name,
                "displayName": t.display_name,
                "category": if t.analysis { "analysis_result" } else { "data_artifact" },
                "count": t.count,
                "queryWith": if t.analysis { "query_analysis_results" } else { "query_data_artifacts" },
            })
        })
        .collect();
    Ok(Value::Array(rows).into())
}

fn artifact_id(r: Ref) -> ToolResult<i64> {
    match r {
        Ref::Artifact(id) => Ok(id),
        other => Err(ToolError::arg(format!(
            "{other} is not an artifact reference"
        ))),
    }
}

fn get_artifact(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let id = artifact_id(Ref::req(args, "ref")?)?;
    let db = ctx.db();
    let a = db
        .artifact(id)?
        .ok_or_else(|| ToolError::NotFound(format!("artifact:{id}")))?;
    let mut v = artifact_row(ctx, &a, true)?;
    let source = match a.file_id {
        Some(f) => db.file(f)?.map(|f| file_brief(&f)),
        None => None,
    };
    let score = db.score(ObjKind::Artifact, id)?;
    let tags: Vec<Value> = db
        .artifact_tags(id)?
        .iter()
        .map(|t| {
            json!({"tagId": t.id, "tagName": t.tag_name, "examiner": t.examiner,
                   "createdTime": iso_json(Some(t.created_at)), "evidence": {"comment": t.comment}})
        })
        .collect();
    let assoc_type = ctx
        .attribute_type("KAAMOS_ASSOCIATED_ARTIFACT")?
        .map(|t| t.0);
    let mut linked = Vec::new();
    for at in db.attributes(id)? {
        if Some(at.type_id) == assoc_type
            && let pm_casedb::Value::Int(other) = at.value
            && let Some(o) = db.artifact(other)?
        {
            linked.push(artifact_row(ctx, &o, false)?);
        }
    }
    let results: Vec<Value> = db
        .artifacts(
            &ArtifactFilter {
                source_artifact_id: Some(id),
                ..Default::default()
            },
            None,
            false,
            50,
        )?
        .iter()
        .map(|r| artifact_row(ctx, r, true))
        .collect::<Result<_, _>>()?;
    let accounts: Vec<Value> = db
        .accounts_of_artifact(id)?
        .iter()
        .map(|acc| json!({"ref": Ref::Account(acc.id).to_string(), "accountType": acc.type_name, "evidence": {"accountId": acc.identifier}}))
        .collect();
    let events: Vec<Value> = db
        .events_of_artifact(id)?
        .iter()
        .take(50)
        .map(|e| json!({"ref": Ref::Event(e.id).to_string(), "time": iso_json(Some(e.time)), "timeEpoch": e.time, "eventTypeId": e.type_id, "evidence": {"description": e.full_description}}))
        .collect();
    if let Some(o) = v.as_object_mut() {
        o.insert("createdTime".into(), iso_json(Some(a.created_at)));
        o.insert(
            "reviewStatus".into(),
            json!(match a.review_status {
                1 => "approved",
                2 => "rejected",
                _ => "undecided",
            }),
        );
        o.insert("sourceFile".into(), source.unwrap_or(Value::Null));
        o.insert(
            "aggregateScore".into(),
            json!({"significance": score.significance.name()}),
        );
        o.insert("tags".into(), Value::Array(tags));
        o.insert("linkedArtifacts".into(), Value::Array(linked));
        o.insert("resultsAboutThis".into(), Value::Array(results));
        o.insert("accounts".into(), Value::Array(accounts));
        o.insert("events".into(), Value::Array(events));
    }
    Ok(v.into())
}

const RELATIONS: &[&str] = &[
    "same_hash",
    "same_folder",
    "artifacts",
    "accounts",
    "events",
];

fn related_page(
    ctx: &Ctx<'_>,
    r: Ref,
    relation: &str,
    args: &Args,
    limit: usize,
) -> ToolResult<Page> {
    let db = ctx.db();
    let cursor = args.cursor()?;
    let after = cursor.as_ref().and_then(Value::as_i64);
    let file_page = |filter: FileFilter| -> ToolResult<Page> {
        let c = after.map(|id| FileCursor {
            key: SortKey::None,
            id,
        });
        let rows = db.files(&filter, FileOrder::Id, false, c.as_ref(), limit + 1)?;
        Page::from_items(rows, limit, |f| {
            Ok(Row {
                value: file_brief(f),
                cursor: json!(f.id),
            })
        })
    };
    let art_page = |filter: ArtifactFilter| -> ToolResult<Page> {
        let rows = db.artifacts(&filter, after, false, limit + 1)?;
        Page::from_items(rows, limit, |a| {
            Ok(Row {
                value: artifact_row(ctx, a, true)?,
                cursor: json!(a.id),
            })
        })
    };
    match (r, relation) {
        (Ref::File(id), "same_hash") => {
            let f = db
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            let mut filter = FileFilter {
                exclude_id: Some(id),
                ..Default::default()
            };
            match (&f.md5, &f.sha256) {
                (Some(m), _) => filter.md5 = Some(m.clone()),
                (None, Some(s)) => filter.sha256 = Some(s.clone()),
                (None, None) => {
                    return Ok(Page::default().with(
                        "note",
                        json!("the file has not been hashed (Hash Lookup did not run on it)"),
                    ));
                }
            }
            file_page(filter)
        }
        (Ref::File(id), "same_folder") => {
            let f = db
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(r.to_string()))?;
            let Some(parent) = f.parent_id else {
                return Ok(Page::default());
            };
            file_page(FileFilter {
                parent_id: Some(parent),
                exclude_id: Some(id),
                ..Default::default()
            })
        }
        (Ref::File(id), "artifacts") => art_page(ArtifactFilter {
            file_id: Some(id),
            ..Default::default()
        }),
        (Ref::Artifact(id), "artifacts") => art_page(ArtifactFilter {
            source_artifact_id: Some(id),
            ..Default::default()
        }),
        (Ref::Artifact(id), "accounts") => {
            let rows = db.accounts_of_artifact(id)?;
            Page::from_items(rows, usize::MAX, |a| {
                Ok(Row {
                    value: json!({"ref": Ref::Account(a.id).to_string(), "accountType": a.type_name, "evidence": {"accountId": a.identifier}}),
                    cursor: json!(a.id),
                })
            })
        }
        (Ref::File(_) | Ref::Artifact(_), "events") => {
            let mut f = EventFilter::default();
            match r {
                Ref::File(id) => f.file_id = Some(id),
                _ => f.artifact_id = Some(artifact_id(r)?),
            }
            let ks = cursor
                .as_ref()
                .and_then(|c| {
                    Some(EventKeyset::After {
                        time: c.get(0)?.as_i64()?,
                        id: c.get(1)?.as_i64()?,
                    })
                })
                .unwrap_or(EventKeyset::Start);
            let rows = db.events(&[], &f, ks, false, limit + 1)?;
            Page::from_items(rows, limit, |e| {
                Ok(Row {
                    value: json!({"ref": Ref::Event(e.id).to_string(), "time": iso_json(Some(e.time)), "timeEpoch": e.time,
                                  "eventTypeId": e.type_id, "evidence": {"description": e.full_description}}),
                    cursor: json!([e.time, e.id]),
                })
            })
        }
        (_, rel) if RELATIONS.contains(&rel) => Err(ToolError::arg(format!(
            "relation {rel} does not apply to {r}"
        ))),
        (_, rel) => Err(ToolError::arg(format!(
            "unknown relation {rel:?}; use one of {}",
            RELATIONS.join(", ")
        ))),
    }
}

fn get_related(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let r = Ref::req(args, "ref")?;
    let relation = args
        .str("relation")?
        .unwrap_or_else(|| "all".into())
        .to_ascii_lowercase();
    if relation != "all" {
        let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
        return Ok(related_page(ctx, r, &relation, args, limit)?
            .with("relation", json!(relation))
            .into());
    }
    let mut out = Map::new();
    out.insert("ref".into(), json!(r.to_string()));
    let empty = Args::default();
    for rel in RELATIONS {
        match related_page(ctx, r, rel, &empty, 10) {
            Ok(p) => {
                out.insert(
                    (*rel).into(),
                    json!({"rows": p.rows.into_iter().map(|r| r.value).collect::<Vec<_>>(), "more": p.more}),
                );
            }
            Err(ToolError::InvalidArgument(_)) => {}
            Err(e) => return Err(e),
        }
    }
    out.insert(
        "note".into(),
        json!("call again with relation=<name> to page through one relation"),
    );
    Ok(Value::Object(out).into())
}

fn list_notable(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let classes: &[bool] = match args
        .str("score")?
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        None | Some("bad" | "notable") => &[true],
        Some("suspicious" | "likely_notable") => &[false],
        Some("any" | "all") => &[true, false],
        Some(o) => {
            return Err(ToolError::arg(format!(
                "score must be bad, suspicious or any, not {o:?}"
            )));
        }
    };
    let ds = args.i64("dataSourceId")?;
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    // Cursor: [phase, kind, id].
    let (phase, kind, id) = args
        .cursor()?
        .and_then(|c| {
            Some((
                c.get(0)?.as_u64()? as usize,
                c.get(1)?.as_i64()?,
                c.get(2)?.as_i64()?,
            ))
        })
        .unwrap_or((0, 0, 0));
    let db = ctx.db();
    let mut items = Vec::new();
    for (p, bad) in classes.iter().enumerate().skip(phase) {
        let after = if p == phase {
            (ObjKind::from_code(kind).unwrap_or(ObjKind::File), id)
        } else {
            (ObjKind::File, 0)
        };
        let left = limit + 1 - items.len();
        for (k, oid) in db.scored_objects(*bad, ds, after, left)? {
            items.push((p, *bad, k, oid));
        }
        if items.len() > limit {
            break;
        }
    }
    Page::from_items(items, limit, |(p, bad, k, oid)| {
        let (object, reasons_filter) = match k {
            ObjKind::File => (
                db.file(*oid)?.map(|f| file_brief(&f)),
                ArtifactFilter {
                    file_id: Some(*oid),
                    analysis: Some(true),
                    ..Default::default()
                },
            ),
            ObjKind::Artifact => (
                db.artifact(*oid)?
                    .map(|a| artifact_row(ctx, &a, false))
                    .transpose()?,
                ArtifactFilter {
                    source_artifact_id: Some(*oid),
                    analysis: Some(true),
                    ..Default::default()
                },
            ),
            ObjKind::OsAccount => (
                Some(json!({"ref": Ref::OsAccount(*oid).to_string()})),
                ArtifactFilter {
                    ids: Some(Vec::new()),
                    ..Default::default()
                },
            ),
        };
        let reasons: Vec<Value> = db
            .artifacts(&reasons_filter, None, false, 5)?
            .iter()
            .map(|a| artifact_row(ctx, a, true))
            .collect::<Result<_, _>>()?;
        let tags: Vec<String> = match k {
            ObjKind::File => db
                .file_tags(*oid)?
                .into_iter()
                .map(|t| t.tag_name)
                .collect(),
            ObjKind::Artifact => db
                .artifact_tags(*oid)?
                .into_iter()
                .map(|t| t.tag_name)
                .collect(),
            ObjKind::OsAccount => Vec::new(),
        };
        let r = match k {
            ObjKind::File => Ref::File(*oid).to_string(),
            ObjKind::Artifact => artifact_ref(*oid, false),
            ObjKind::OsAccount => Ref::OsAccount(*oid).to_string(),
        };
        Ok(Row {
            value: json!({
                "ref": r,
                "significance": if *bad { "Notable" } else { "Likely Notable" },
                "object": object,
                "reasons": reasons,
                "tags": tags,
            }),
            cursor: json!([p, *k as i64, oid]),
        })
    })
    .map(Output::from)
}

/// Id of an attribute type that must exist.
fn attr_id(ctx: &Ctx<'_>, name: &str) -> ToolResult<i64> {
    ctx.attribute_type(name)?
        .map(|t| t.0)
        .ok_or_else(|| ToolError::Unavailable(format!("attribute type {name} is not in this case")))
}

fn time_window(args: &Args, types: Vec<i64>) -> ToolResult<Option<AttrPredicate>> {
    let (from, to) = (args.time("startTime", false)?, args.time("endTime", true)?);
    Ok(
        (from.is_some() || to.is_some()).then_some(AttrPredicate::IntRange {
            type_ids: types,
            from,
            to,
        }),
    )
}

fn query_event_logs(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let Some((type_id, _, _)) = ctx.db().artifact_type_by_name("KAAMOS_EVENT_LOG")? else {
        return Err(ToolError::Unavailable(
            "no event-log entries: the Windows Event Logs module has not run on this case (see list_ingest_jobs)".into(),
        ));
    };
    let mut f = ArtifactFilter {
        type_ids: vec![type_id],
        ds_id: args.i64("dataSourceId")?,
        ..Default::default()
    };
    if let Some(c) = args.str("channel")? {
        f.attrs.push(AttrPredicate::TextEquals {
            type_id: attr_id(ctx, "KAAMOS_EVENT_CHANNEL")?,
            text: c,
        });
    }
    if let Some(e) = args.i64("eventId")? {
        f.attrs.push(AttrPredicate::IntEquals {
            type_id: attr_id(ctx, "KAAMOS_EVENT_ID")?,
            value: e,
        });
    }
    if let Some(p) = args.str("provider")? {
        f.attrs.push(AttrPredicate::TextContains {
            type_id: Some(attr_id(ctx, "KAAMOS_EVENT_PROVIDER")?),
            text: p,
        });
    }
    if let Some(t) = args.str("text")? {
        f.attrs.push(AttrPredicate::TextContains {
            type_id: None,
            text: t,
        });
    }
    if let Some(p) = time_window(args, vec![attr_id(ctx, "KAAMOS_DATETIME")?])? {
        f.attrs.push(p);
    }
    Ok(artifact_page(ctx, args, &f)?.into())
}

fn type_ids(ctx: &Ctx<'_>, names: &[&str]) -> ToolResult<Vec<i64>> {
    let mut out = Vec::new();
    for n in names {
        if let Some((id, _, _)) = ctx.db().artifact_type_by_name(n)? {
            out.push(id);
        }
    }
    Ok(out)
}

fn get_messages(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let db = ctx.db();
    let limit = args.limit(DEFAULT_LIMIT, MAX_LIMIT)?;
    if let Some(thread) = args.str("threadId")? {
        let f = ArtifactFilter {
            type_ids: type_ids(ctx, &["KAAMOS_MESSAGE", "KAAMOS_EMAIL_MSG"])?,
            ds_id: args.i64("dataSourceId")?,
            attrs: vec![AttrPredicate::TextEquals {
                type_id: attr_id(ctx, "KAAMOS_THREAD_ID")?,
                text: thread,
            }],
            ..Default::default()
        };
        return Ok(artifact_page(ctx, args, &f)?.into());
    }
    if args.str("accountId")?.is_some() {
        let acc = account_arg(ctx, args, "accountType", "accountId")?.ok_or_else(|| {
            ToolError::NotFound("account (check accountType and accountId)".into())
        })?;
        let other = match args.str("otherAccountId")? {
            Some(_) => Some(
                account_arg(ctx, args, "otherAccountType", "otherAccountId")?
                    .ok_or_else(|| ToolError::NotFound("other account".into()))?
                    .id,
            ),
            None => None,
        };
        let after = args
            .cursor()?
            .and_then(|c| Some((c.get(0)?.as_i64()?, c.get(1)?.as_i64()?)));
        let rows = db.relationships(acc.id, other, &[1], after, limit + 1)?;
        return Page::from_items(rows, limit, |(dt, art, _)| {
            let a = db
                .artifact(*art)?
                .ok_or_else(|| ToolError::NotFound(format!("artifact:{art}")))?;
            let mut v = artifact_row(ctx, &a, true)?;
            if let Some(o) = v.as_object_mut() {
                o.insert("time".into(), iso_json(*dt));
                o.insert("timeEpoch".into(), json!(dt));
            }
            Ok(Row {
                value: v,
                cursor: json!([dt.unwrap_or(-1), art]),
            })
        })
        .map(Output::from);
    }
    let f = ArtifactFilter {
        type_ids: type_ids(ctx, &["KAAMOS_MESSAGE", "KAAMOS_EMAIL_MSG"])?,
        ds_id: args.i64("dataSourceId")?,
        ..Default::default()
    };
    Ok(artifact_page(ctx, args, &f)?.into())
}

fn get_contacts(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let mut f = ArtifactFilter {
        type_ids: type_ids(ctx, &["KAAMOS_CONTACT"])?,
        ds_id: args.i64("dataSourceId")?,
        ..Default::default()
    };
    if let Some(t) = args.str("text")? {
        f.attrs.push(AttrPredicate::TextContains {
            type_id: None,
            text: t,
        });
    }
    Ok(artifact_page(ctx, args, &f)?.into())
}

fn float_arg(args: &Args, key: &str) -> ToolResult<Option<f64>> {
    match args.0.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_f64()),
        Some(Value::String(s)) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| ToolError::arg(format!("{key} must be a number"))),
        Some(_) => Err(ToolError::arg(format!("{key} must be a number"))),
    }
}

fn geolocation_points(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let lat = attr_id(ctx, "KAAMOS_GEO_LATITUDE")?;
    let lon = attr_id(ctx, "KAAMOS_GEO_LONGITUDE")?;
    let mut f = ArtifactFilter {
        type_ids: type_ids(
            ctx,
            &[
                "KAAMOS_GPS_TRACKPOINT",
                "KAAMOS_GPS_BOOKMARK",
                "KAAMOS_GPS_LAST_KNOWN_LOCATION",
                "KAAMOS_GPS_SEARCH",
                "KAAMOS_GPS_ROUTE",
                "KAAMOS_GPS_TRACK",
                "KAAMOS_METADATA_EXIF",
            ],
        )?,
        ds_id: args.i64("dataSourceId")?,
        attrs: vec![AttrPredicate::Has { type_id: lat }],
        ..Default::default()
    };
    let (a, b) = (
        float_arg(args, "minLatitude")?,
        float_arg(args, "maxLatitude")?,
    );
    if a.is_some() || b.is_some() {
        f.attrs.push(AttrPredicate::DoubleRange {
            type_id: lat,
            from: a,
            to: b,
        });
    }
    let (a, b) = (
        float_arg(args, "minLongitude")?,
        float_arg(args, "maxLongitude")?,
    );
    if a.is_some() || b.is_some() {
        f.attrs.push(AttrPredicate::DoubleRange {
            type_id: lon,
            from: a,
            to: b,
        });
    }
    let mut times = vec![attr_id(ctx, "KAAMOS_DATETIME")?];
    if let Some((id, _)) = ctx.attribute_type("KAAMOS_DATETIME_CREATED")? {
        times.push(id);
    }
    if let Some(p) = time_window(args, times)? {
        f.attrs.push(p);
    }
    let mut page = artifact_page(ctx, args, &f)?;
    for row in &mut page.rows {
        let attrs = row
            .value
            .pointer("/evidence/attributes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let get = |n: &str| {
            attrs
                .iter()
                .find(|a| a["type"] == n)
                .map(|a| a["value"].clone())
        };
        if let Some(o) = row.value.as_object_mut() {
            o.insert(
                "latitude".into(),
                get("KAAMOS_GEO_LATITUDE").unwrap_or(Value::Null),
            );
            o.insert(
                "longitude".into(),
                get("KAAMOS_GEO_LONGITUDE").unwrap_or(Value::Null),
            );
            o.insert(
                "altitude".into(),
                get("KAAMOS_GEO_ALTITUDE").unwrap_or(Value::Null),
            );
        }
    }
    Ok(page.into())
}

fn list_ingest_jobs(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let rows: Vec<Value> = ctx
        .db()
        .ingest_jobs(args.i64("dataSourceId")?, MAX_LIMIT)?
        .into_iter()
        .map(|j| {
            json!({
                "jobId": j.id,
                "dataSourceId": j.ds_id,
                "dataSourceRef": Ref::DataSource(j.ds_id).to_string(),
                "started": iso_json(Some(j.started_at)),
                "ended": iso_json(j.ended_at),
                "status": match j.status { 1 => "cancelled", 2 => "completed", _ => "running or interrupted" },
                "modules": j.modules.iter().map(|(m, v)| json!({"module": m, "version": v})).collect::<Vec<_>>(),
                "settings": serde_json::from_str::<Value>(&j.settings).unwrap_or(Value::String(j.settings.clone())),
            })
        })
        .collect();
    Ok(json!({
        "jobs": rows,
        "note": "Per-file errors and skipped files are reported in the ingest messages of the GUI, not stored in the case.",
    })
    .into())
}
