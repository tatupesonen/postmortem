//! Artifact building and batched, idempotent posting for the Windows
//! extractors (standard and custom types, associated objects).

use std::collections::HashMap;

use pm_casedb::catalog::{Category, ValueType, artifact_type_id, attribute_type_id};
use pm_casedb::{NewArtifact, Value};
use pm_ingest::{JobContext, ModuleError};

use crate::recent_activity::NAME;

/// Custom artifact and attribute types (registered on first use, as
/// Autopsy's `getOrAddArtifactType`).
#[derive(Debug, Default)]
pub(crate) struct Types {
    ids: HashMap<String, i64>,
}

impl Types {
    /// A standard or custom artifact type id.
    pub(crate) fn artifact(&mut self, ctx: &JobContext, name: &str, display: &str) -> i64 {
        if let Some(id) = artifact_type_id(name) {
            return id;
        }
        let key = format!("art:{name}");
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = ctx
            .case
            .db()
            .artifact_type(name, display, Category::DataArtifact)
            .unwrap_or(0);
        self.ids.insert(key, id);
        id
    }

    /// A standard or custom attribute type id.
    pub(crate) fn attribute(
        &mut self,
        ctx: &JobContext,
        name: &str,
        display: &str,
        vt: ValueType,
    ) -> i64 {
        if let Some(id) = attribute_type_id(name) {
            return id;
        }
        let key = format!("attr:{name}");
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = ctx.case.db().attribute_type(name, display, vt).unwrap_or(0);
        self.ids.insert(key, id);
        id
    }
}

/// An artifact under construction.
#[derive(Debug)]
pub(crate) struct NewArt {
    a: NewArtifact,
    key: Vec<i64>,
    associate: Option<i64>,
}

fn std_attr(name: &str) -> i64 {
    attribute_type_id(name).unwrap_or(0)
}

impl NewArt {
    /// A data artifact of type `type_id` on `file_id` (`None` = on the
    /// data source).
    pub(crate) fn new(ctx: &JobContext, type_id: i64, file_id: Option<i64>) -> Self {
        Self {
            a: NewArtifact {
                type_id,
                ds_id: ctx.ds_id,
                file_id,
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: ctx.now(),
                attributes: Vec::new(),
            },
            key: Vec::new(),
            associate: None,
        }
    }

    /// A standard artifact type by name.
    pub(crate) fn std(ctx: &JobContext, ty: &str, file_id: Option<i64>) -> Self {
        Self::new(ctx, artifact_type_id(ty).unwrap_or(0), file_id)
    }

    /// Adds an attribute by type id.
    pub(crate) fn attr(mut self, type_id: i64, v: Value) -> Self {
        self.a.attributes.push((type_id, v, NAME.to_owned()));
        self
    }

    /// A text attribute (always added, even when empty).
    pub(crate) fn text(self, name: &str, v: impl Into<String>) -> Self {
        self.attr(std_attr(name), Value::Text(v.into()))
    }

    /// A text attribute, left out when absent or empty.
    pub(crate) fn text_opt(self, name: &str, v: Option<impl Into<String>>) -> Self {
        match v.map(Into::into).filter(|s: &String| !s.is_empty()) {
            Some(s) => self.text(name, s),
            None => self,
        }
    }

    /// A date-time attribute (Unix seconds), left out when absent.
    pub(crate) fn time(self, name: &str, v: Option<i64>) -> Self {
        match v {
            Some(t) => self.attr(std_attr(name), Value::DateTime(t)),
            None => self,
        }
    }

    /// An integer attribute.
    pub(crate) fn int(self, name: &str, v: i64) -> Self {
        self.attr(std_attr(name), Value::Int(v))
    }

    /// A custom attribute (id from [`Types::attribute`]), left out when
    /// absent.
    pub(crate) fn custom(self, type_id: i64, v: Option<Value>) -> Self {
        match v {
            Some(v) if type_id != 0 => self.attr(type_id, v),
            _ => self,
        }
    }

    /// Standard attributes that identify a duplicate on re-ingest.
    pub(crate) fn key(mut self, names: &[&str]) -> Self {
        self.key = names.iter().map(|n| std_attr(n)).collect();
        self
    }

    /// Custom attribute ids that identify a duplicate.
    pub(crate) fn key_ids(mut self, ids: &[i64]) -> Self {
        self.key.extend_from_slice(ids);
        self
    }

    /// Also posts a `TSK_ASSOCIATED_OBJECT` on `file_id` pointing here.
    pub(crate) fn associate(mut self, file_id: Option<i64>) -> Self {
        self.associate = file_id;
        self
    }
}

/// Artifacts collected by one extractor, written in one transaction.
#[derive(Debug, Default)]
pub(crate) struct Batch {
    items: Vec<NewArt>,
}

impl Batch {
    pub(crate) fn push(&mut self, a: NewArt) {
        self.items.push(a);
    }

    /// Writes everything; returns how many new artifacts were added.
    pub(crate) fn flush(self, ctx: &JobContext) -> Result<usize, ModuleError> {
        if self.items.is_empty() {
            return Ok(0);
        }
        let assoc_type = artifact_type_id("TSK_ASSOCIATED_OBJECT").unwrap_or(0);
        let assoc_attr = std_attr("TSK_ASSOCIATED_ARTIFACT");
        let created = ctx.now();
        let ds = ctx.ds_id;
        let n = ctx.case.db().bulk(|b| {
            let mut n = 0;
            for it in &self.items {
                let id = if it.a.file_id.is_some() {
                    b.add_artifact_unless_exists(&it.a, &it.key)?
                } else {
                    Some(b.add_artifact(&it.a)?)
                };
                let Some(id) = id else { continue };
                n += 1;
                if let Some(target) = it.associate {
                    let assoc = NewArtifact {
                        type_id: assoc_type,
                        ds_id: ds,
                        file_id: Some(target),
                        source_artifact_id: None,
                        score: None,
                        conclusion: None,
                        configuration: None,
                        justification: None,
                        created_at: created,
                        attributes: vec![(assoc_attr, Value::Int(id), NAME.to_owned())],
                    };
                    b.add_artifact(&assoc)?;
                }
            }
            Ok(n)
        })?;
        Ok(n)
    }
}

/// Whether the data source already has a data-source-level artifact of
/// `type_id` whose text attribute `attr` equals `value` (`None` = any).
pub(crate) fn ds_artifact_exists(
    ctx: &JobContext,
    type_id: i64,
    attr: Option<(&str, &str)>,
) -> bool {
    let db = ctx.case.db();
    let Ok(arts) = db.artifacts_of_type(type_id, Some(ctx.ds_id), 0, 10_000) else {
        return false;
    };
    arts.iter().any(|a| match attr {
        None => true,
        Some((name, value)) => db.attributes(a.id).is_ok_and(|attrs| {
            attrs
                .iter()
                .any(|x| x.name == name && x.value.display() == value)
        }),
    })
}
