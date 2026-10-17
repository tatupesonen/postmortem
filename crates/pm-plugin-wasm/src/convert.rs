//! Conversions between the WIT bindings and `pm-plugin-api` types (the
//! two describe the same data; see `wit/plugin.wit`).

use pm_plugin_api as api;

use crate::runtime::postmortem::plugin::types as wit;

pub(crate) fn file_to_wit(f: &api::FileInfo) -> wit::FileInfo {
    wit::FileInfo {
        id: f.id,
        ds_id: f.ds_id,
        name: f.name.clone(),
        parent_path: f.parent_path.clone(),
        extension: f.extension.clone(),
        size: f.size,
        kind: match f.kind {
            api::FileKind::File => wit::FileKind::File,
            api::FileKind::Directory => wit::FileKind::Directory,
            api::FileKind::Unallocated => wit::FileKind::Unallocated,
            api::FileKind::Carved => wit::FileKind::Carved,
            api::FileKind::Derived => wit::FileKind::Derived,
            api::FileKind::Other => wit::FileKind::Other,
        },
        deleted: f.deleted,
        mime: f.mime.clone(),
        md5: f.md5.clone(),
        modified: f.modified,
        accessed: f.accessed,
        created: f.created,
        changed: f.changed,
    }
}

pub(crate) fn ds_to_wit(d: &api::DataSourceInfo) -> wit::DataSourceInfo {
    wit::DataSourceInfo {
        id: d.id,
        name: d.name.clone(),
        timezone: d.timezone.clone(),
        size: d.size,
    }
}

pub(crate) fn find_from_wit(q: wit::FileQuery) -> api::FindFiles {
    api::FindFiles {
        name: q.name,
        extension: q.extension,
        path_contains: q.path_contains,
        mime: q.mime,
        limit: q.limit,
    }
}

fn value_from_wit(v: wit::Value) -> api::Value {
    match v {
        wit::Value::Text(s) => api::Value::Text(s),
        wit::Value::Int(i) => api::Value::Int(i),
        wit::Value::Double(d) => api::Value::Double(d),
        wit::Value::Bytes(b) => api::Value::Bytes(b),
        wit::Value::Time(t) => api::Value::Time(t),
    }
}

pub(crate) fn result_from_wit(r: wit::NewResult) -> api::NewResult {
    api::NewResult {
        artifact_type: r.artifact_type,
        file_id: r.file_id,
        significance: r.significance.map(|s| match s {
            wit::Significance::Unknown => api::Significance::Unknown,
            wit::Significance::LikelyNone => api::Significance::LikelyNone,
            wit::Significance::LikelyNotable => api::Significance::LikelyNotable,
            wit::Significance::None => api::Significance::None,
            wit::Significance::Notable => api::Significance::Notable,
        }),
        conclusion: r.conclusion,
        configuration: r.configuration,
        justification: r.justification,
        attributes: r
            .attributes
            .into_iter()
            .map(|a| api::Attribute {
                kind: a.kind,
                value: value_from_wit(a.value),
            })
            .collect(),
    }
}

pub(crate) fn communication_from_wit(c: wit::Communication) -> api::Communication {
    api::Communication {
        accounts: c
            .accounts
            .into_iter()
            .map(|a| api::AccountId {
                account_type: a.account_type,
                id: a.id,
            })
            .collect(),
        relationship: c.relationship.map(|r| {
            (
                r.members.into_iter().map(|m| m as usize).collect(),
                match r.kind {
                    wit::RelationshipKind::Message => api::RelationshipKind::Message,
                    wit::RelationshipKind::CallLog => api::RelationshipKind::CallLog,
                    wit::RelationshipKind::Contact => api::RelationshipKind::Contact,
                },
                r.time,
            )
        }),
    }
}

pub(crate) fn derived_from_wit(d: wit::DerivedFile) -> api::DerivedFile {
    api::DerivedFile {
        name: d.name,
        data: d.data,
        modified: d.modified,
        accessed: d.accessed,
        created: d.created,
    }
}

pub(crate) fn level_from_wit(l: wit::Level) -> api::Level {
    match l {
        wit::Level::Debug => api::Level::Debug,
        wit::Level::Info => api::Level::Info,
        wit::Level::Warn => api::Level::Warn,
        wit::Level::Error => api::Level::Error,
    }
}

pub(crate) fn message_from_wit(m: wit::Message) -> api::Message {
    api::Message {
        level: level_from_wit(m.level),
        subject: m.subject,
        details: m.details,
        file_id: m.file_id,
    }
}

pub(crate) fn info_from_wit(i: wit::PluginInfo) -> api::PluginInfo {
    api::PluginInfo {
        id: i.id,
        name: i.name,
        description: i.description,
        version: i.version,
        enabled_by_default: i.enabled_by_default,
        file_filter: i.file_filter.map(|f| api::FileFilter {
            mime_types: f.mime_types,
            extensions: f.extensions,
            names: f.names,
            include_unallocated: f.include_unallocated,
            include_directories: f.include_directories,
            max_size: f.max_size,
        }),
        data_source_stage: i.data_source_stage,
    }
}
