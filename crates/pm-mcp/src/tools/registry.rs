//! `read_registry_key`: a key of a registry hive inside the evidence.

use pm_casedb::FileRecord;
use pm_registry::{Hive, ValueType};
use serde_json::{Value, json};

use super::{DS, ToolSpec, tool};
use crate::args::{Args, MAX_LIMIT, Ref, hex, iso_json};
use crate::error::{ToolError, ToolResult};
use crate::response::Output;
use crate::server::Ctx;

/// Largest value data returned.
const MAX_VALUE_BYTES: usize = 4096;

pub(super) fn tools() -> Vec<ToolSpec> {
    vec![tool(
        "read_registry_key",
        "Values and subkeys (with last-written times) of one key of a registry hive in the evidence, read \
         read-only. hive is a file reference (file:123), a path in the image (/Windows/System32/config/SYSTEM, \
         /Users/alice/NTUSER.DAT) or one of SYSTEM, SOFTWARE, SAM, SECURITY, DEFAULT. Transaction logs are not \
         replayed.",
        &[
            (
                "hive",
                "string",
                "file:123, an image path, or SYSTEM / SOFTWARE / SAM / SECURITY / DEFAULT",
            ),
            (
                "key",
                "string",
                "Key path below the hive root, e.g. ControlSet001\\Enum\\USBSTOR (empty: the root)",
            ),
            DS,
        ],
        &["hive"],
        read_registry_key,
    )]
}

fn find_hive(ctx: &Ctx<'_>, args: &Args) -> ToolResult<FileRecord> {
    let h = args.req_str("hive")?;
    if h.starts_with("file:") {
        return match Ref::parse(&h) {
            Some(Ref::File(id)) => ctx
                .db()
                .file(id)?
                .ok_or_else(|| ToolError::NotFound(h.clone())),
            _ => Err(ToolError::arg(format!("bad hive reference {h:?}"))),
        };
    }
    let path = match h.to_ascii_uppercase().as_str() {
        n @ ("SYSTEM" | "SOFTWARE" | "SAM" | "SECURITY" | "DEFAULT") => {
            format!("/Windows/System32/config/{n}")
        }
        _ => h.clone(),
    };
    let sources = match args.i64("dataSourceId")? {
        Some(d) => vec![d],
        None => ctx.db().data_sources()?.iter().map(|d| d.id).collect(),
    };
    for ds in sources {
        if let Some(f) = ctx
            .db()
            .resolve_path(ds, &path)?
            .into_iter()
            .find(|f| f.stream.is_none())
        {
            return Ok(f);
        }
    }
    Err(ToolError::NotFound(format!("no hive at {path}")))
}

fn value_json(v: &pm_registry::Value<'_>) -> Value {
    let ty = v.data_type();
    let data = match ty {
        ValueType::Sz | ValueType::ExpandSz | ValueType::Link => v.string().map(Value::String),
        ValueType::MultiSz => v.multi_string().map(|m| json!(m)),
        ValueType::Dword | ValueType::DwordBigEndian | ValueType::Qword => {
            v.data().ok().and_then(|d| {
                let n = match (ty, d.len()) {
                    (ValueType::Dword, 4..) => d
                        .get(..4)?
                        .try_into()
                        .ok()
                        .map(|b| u64::from(u32::from_le_bytes(b))),
                    (ValueType::DwordBigEndian, 4..) => d
                        .get(..4)?
                        .try_into()
                        .ok()
                        .map(|b| u64::from(u32::from_be_bytes(b))),
                    (ValueType::Qword, 8..) => d.get(..8)?.try_into().ok().map(u64::from_le_bytes),
                    _ => None,
                }?;
                Some(json!(n))
            })
        }
        _ => None,
    };
    let (data, hex_data, truncated) = match data {
        Some(d) => (d, None, false),
        None => match v.data() {
            Ok(b) => (
                Value::Null,
                Some(hex(b.get(..MAX_VALUE_BYTES).unwrap_or(&b))),
                b.len() > MAX_VALUE_BYTES,
            ),
            Err(_) => (Value::Null, None, false),
        },
    };
    json!({
        "type": ty.name(),
        "size": v.data_size(),
        "evidence": {"name": v.name(), "data": data, "hex": hex_data, "hexTruncated": truncated},
    })
}

fn read_registry_key(ctx: &Ctx<'_>, args: &Args) -> ToolResult<Output> {
    let f = find_hive(ctx, args)?;
    ctx.check_content_allowed(f.id)?;
    let src = ctx.case().content(&f)?;
    let hive =
        Hive::open(src).map_err(|e| ToolError::Unavailable(format!("not a readable hive: {e}")))?;
    let key_path = args.str("key")?.unwrap_or_default();
    let reg = |e: pm_registry::Error| ToolError::Unavailable(format!("registry error: {e}"));
    let key = if key_path.trim_matches(['\\', '/']).is_empty() {
        hive.root().map_err(reg)?
    } else {
        hive.key(&key_path.replace('/', "\\"))
            .map_err(reg)?
            .ok_or_else(|| ToolError::NotFound(format!("key {key_path:?}")))?
    };
    let values: Vec<Value> = key
        .values()
        .map_err(reg)?
        .iter()
        .take(MAX_LIMIT)
        .map(value_json)
        .collect();
    let subkeys = key.subkeys().map_err(reg)?;
    let sub: Vec<Value> = subkeys
        .iter()
        .take(MAX_LIMIT)
        .map(|k| json!({"lastWritten": iso_json(k.last_written_unix()), "evidence": {"name": k.name()}}))
        .collect();
    Ok(json!({
        "hiveRef": Ref::File(f.id).to_string(),
        "lastWritten": iso_json(key.last_written_unix()),
        "lastWrittenEpoch": key.last_written_unix(),
        "subkeyCount": subkeys.len(),
        "subkeysTruncated": subkeys.len() > MAX_LIMIT,
        "values": values,
        "subkeys": sub,
        "evidence": {"hivePath": f.path(), "key": key_path, "keyName": key.name()},
    })
    .into())
}
