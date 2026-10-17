//! JSON rows for case objects. Text that comes from the evidence (names,
//! paths, attribute values, descriptions, file content, labels) is only
//! ever placed inside an `evidence` object; everything else is generated
//! by postmortem (ids, sizes, times, hashes, detected MIME types, type
//! names).

use pm_casedb::{
    ArtifactRecord, AttributeRecord, DataSourceRecord, FileKindCode, FileRecord, Known, Origin,
    Value as DbValue,
};
use serde_json::{Map, Value, json};

use crate::args::{Ref, hex, iso, iso_json};
use crate::error::ToolResult;
use crate::server::Ctx;

/// Autopsy file type name of an origin (`KAAMOS_DB_FILES_TYPE_ENUM`).
pub fn origin_name(o: Origin) -> &'static str {
    match o {
        Origin::FileSystem => "FS",
        Origin::Carved => "CARVED",
        Origin::Derived => "DERIVED",
        Origin::Local => "LOCAL",
        Origin::UnallocBlocks => "UNALLOC_BLOCKS",
        Origin::VirtualDir => "VIRTUAL_DIR",
        Origin::Slack => "SLACK",
    }
}

/// Autopsy's short name-type letters (`getDirTypeAsString`).
pub fn kind_letter(k: FileKindCode) -> &'static str {
    match k {
        FileKindCode::Unknown => "-",
        FileKindCode::Regular => "r",
        FileKindCode::Directory => "d",
        FileKindCode::Symlink => "l",
        FileKindCode::CharDevice => "c",
        FileKindCode::BlockDevice => "b",
        FileKindCode::Fifo => "p",
        FileKindCode::Socket => "s",
        FileKindCode::Virtual => "v",
    }
}

pub fn known_name(k: Known) -> &'static str {
    match k {
        Known::Unknown => "UNKNOWN",
        Known::Known => "KNOWN",
        Known::Notable => "NOTABLE",
    }
}

fn alloc_name(a: bool) -> &'static str {
    if a { "Allocated" } else { "Unallocated" }
}

/// Unix mode string (`-rw-r--r--`).
pub fn mode_string(kind: FileKindCode, mode: Option<u32>) -> Option<String> {
    let m = mode?;
    let mut s = String::with_capacity(10);
    s.push(match kind {
        FileKindCode::Directory => 'd',
        FileKindCode::Symlink => 'l',
        FileKindCode::CharDevice => 'c',
        FileKindCode::BlockDevice => 'b',
        FileKindCode::Fifo => 'p',
        FileKindCode::Socket => 's',
        _ => '-',
    });
    for shift in [6u32, 3, 0] {
        let bits = (m >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    Some(s)
}

fn time_of(f: &FileRecord, i: usize) -> Option<i64> {
    f.times.get(i).copied().flatten().map(|t| t.0)
}

/// A file row with Autopsy's `query_files` fields.
pub fn file_row(f: &FileRecord) -> Value {
    let mut o = Map::new();
    o.insert("ref".into(), json!(Ref::File(f.id).to_string()));
    o.insert("objectId".into(), json!(f.id));
    o.insert("dataSourceId".into(), json!(f.ds_id));
    o.insert("size".into(), json!(f.size));
    for (i, k) in ["modifiedTime", "accessedTime", "changeTime", "createdTime"]
        .iter()
        .enumerate()
    {
        let t = time_of(f, i);
        o.insert((*k).into(), iso_json(t));
        o.insert(format!("{k}Epoch"), json!(t));
    }
    o.insert("fileType".into(), json!(origin_name(f.origin)));
    o.insert("dirType".into(), json!(kind_letter(f.kind)));
    o.insert("metaType".into(), json!(kind_letter(f.kind)));
    o.insert("dirFlag".into(), json!(alloc_name(f.name_alloc)));
    o.insert("metaFlags".into(), json!(alloc_name(f.meta_alloc)));
    o.insert("deleted".into(), json!(f.deleted()));
    o.insert("modes".into(), json!(mode_string(f.kind, f.mode)));
    o.insert("knownState".into(), json!(known_name(f.known)));
    o.insert("mimeType".into(), json!(f.mime));
    o.insert("md5Hash".into(), json!(f.md5.as_deref().map(hex)));
    o.insert("sha256Hash".into(), json!(f.sha256.as_deref().map(hex)));
    o.insert("sha1Hash".into(), json!(f.sha1.as_deref().map(hex)));
    o.insert("uid".into(), json!(f.uid));
    o.insert("gid".into(), json!(f.gid));
    o.insert("metaAddr".into(), json!(f.inode));
    o.insert("metaSeq".into(), json!(f.seq));
    o.insert(
        "evidence".into(),
        json!({
            "name": f.name,
            "path": f.path(),
            "parentPath": f.parent_path,
            "extension": f.extension,
        }),
    );
    Value::Object(o)
}

/// A short file row (search results, related items).
pub fn file_brief(f: &FileRecord) -> Value {
    json!({
        "ref": Ref::File(f.id).to_string(),
        "objectId": f.id,
        "dataSourceId": f.ds_id,
        "size": f.size,
        "isDirectory": f.kind == FileKindCode::Directory,
        "deleted": f.deleted(),
        "mimeType": f.mime,
        "modifiedTime": iso_json(time_of(f, 0)),
        "md5Hash": f.md5.as_deref().map(hex),
        "evidence": {"name": f.name, "path": f.path()},
    })
}

/// An attribute value as JSON (date-times as ISO 8601 text).
pub fn db_value(v: &DbValue) -> Value {
    match v {
        DbValue::Text(s) => json!(s),
        DbValue::Json(s) => serde_json::from_str(s).unwrap_or_else(|_| json!(s)),
        DbValue::Int(i) => json!(i),
        DbValue::Double(d) => json!(d),
        DbValue::Bytes(b) => json!(hex(b)),
        DbValue::DateTime(t) => iso(*t).map_or(Value::Null, Value::String),
    }
}

/// Attribute list: `[{type, value}]` (plus `epoch` for date-times).
pub fn attributes_json(attrs: &[AttributeRecord]) -> Value {
    Value::Array(
        attrs
            .iter()
            .map(|a| {
                let mut o = json!({"type": a.name, "value": db_value(&a.value)});
                if let DbValue::DateTime(t) = a.value
                    && let Some(m) = o.as_object_mut()
                {
                    m.insert("epoch".into(), json!(t));
                }
                o
            })
            .collect(),
    )
}

/// Reference text of an artifact (`result:` for analysis results).
pub fn artifact_ref(id: i64, analysis: bool) -> String {
    if analysis {
        format!("result:{id}")
    } else {
        format!("artifact:{id}")
    }
}

/// An artifact row with Autopsy's `query_data_artifacts` fields; the
/// attributes go into `evidence`.
pub fn artifact_row(ctx: &Ctx<'_>, a: &ArtifactRecord, with_attributes: bool) -> ToolResult<Value> {
    let (name, display, analysis) = ctx.artifact_type(a.type_id)?;
    let mut o = Map::new();
    o.insert("ref".into(), json!(artifact_ref(a.id, analysis)));
    o.insert("objectId".into(), json!(a.id));
    o.insert(
        "sourceFileId".into(),
        json!(a.file_id.or(a.source_artifact_id)),
    );
    o.insert(
        "sourceRef".into(),
        match (a.source_artifact_id, a.file_id) {
            (Some(s), _) => json!(Ref::Artifact(s).to_string()),
            (None, Some(f)) => json!(Ref::File(f).to_string()),
            _ => Value::Null,
        },
    );
    o.insert("artifactType".into(), json!(name));
    o.insert("artifactTypeDisplayName".into(), json!(display));
    o.insert("analysisResult".into(), json!(analysis));
    o.insert("dataSourceId".into(), json!(a.ds_id));
    if analysis {
        o.insert(
            "score".into(),
            json!({
                "significance": a.significance.map(pm_casedb::Significance::name),
                "priority": match a.priority { Some(1) => "Override", _ => "Normal" },
            }),
        );
    }
    let mut ev = Map::new();
    if with_attributes {
        ev.insert(
            "attributes".into(),
            attributes_json(&ctx.db().attributes(a.id)?),
        );
    }
    for (k, v) in [
        ("conclusion", &a.conclusion),
        ("configuration", &a.configuration),
        ("justification", &a.justification),
    ] {
        if let Some(v) = v {
            ev.insert(k.into(), json!(v));
        }
    }
    o.insert("evidence".into(), Value::Object(ev));
    Ok(Value::Object(o))
}

/// Data source type name (Autopsy's content class).
pub fn ds_type(d: &DataSourceRecord) -> &'static str {
    match d.kind {
        pm_casedb::DataSourceKind::LogicalFiles => "LocalFilesDataSource",
        pm_casedb::DataSourceKind::DiskImage
        | pm_casedb::DataSourceKind::LocalDisk
        | pm_casedb::DataSourceKind::UnallocatedImage => "Image",
    }
}

/// A data source row (`query_data_sources`).
pub fn data_source_row(d: &DataSourceRecord) -> Value {
    let mut o = json!({
        "ref": Ref::DataSource(d.id).to_string(),
        "objectId": d.id,
        "name": d.name,
        "type": ds_type(d),
        "size": d.size,
        "timezone": d.timezone,
        "addedDate": iso_json(Some(d.added_at)),
        "imageType": d.image_format,
        "sectorSize": d.sector_size,
        "paths": d.paths,
        "md5": d.md5,
        "sha1": d.sha1,
        "sha256": d.sha256,
    });
    if !d.acquisition.is_empty()
        && let Some(m) = o.as_object_mut()
    {
        let acq: Map<String, Value> = d
            .acquisition
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        m.insert("evidence".into(), json!({"acquisitionDetails": acq}));
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(
            mode_string(FileKindCode::Regular, Some(0o644)).as_deref(),
            Some("-rw-r--r--")
        );
        assert_eq!(
            mode_string(FileKindCode::Directory, Some(0o755)).as_deref(),
            Some("drwxr-xr-x")
        );
        assert_eq!(mode_string(FileKindCode::Regular, None), None);
    }

    #[test]
    fn values() {
        assert_eq!(
            db_value(&DbValue::DateTime(60)),
            json!("1970-01-01T00:01:00Z")
        );
        assert_eq!(db_value(&DbValue::Bytes(vec![1, 2])), json!("0102"));
        assert_eq!(db_value(&DbValue::Json("[1]".into())), json!([1]));
    }
}
