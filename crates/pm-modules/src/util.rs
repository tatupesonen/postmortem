//! Helpers shared by the built-in modules.

use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{NewArtifact, Priority, Significance, Value};
use pm_ingest::JobContext;

/// Catalogue id of a standard attribute type (`0` if unknown; every name
/// used by the modules is checked by a unit test).
pub(crate) fn attr(name: &str) -> i64 {
    attribute_type_id(name).unwrap_or(0)
}

/// Catalogue id of a standard artifact type.
pub(crate) fn art_type(name: &str) -> i64 {
    artifact_type_id(name).unwrap_or(0)
}

/// Builds an artifact or analysis result for a file.
#[derive(Debug)]
pub(crate) struct Art {
    a: NewArtifact,
    module: &'static str,
}

impl Art {
    /// A data artifact.
    pub(crate) fn data(ctx: &JobContext, module: &'static str, ty: &str, file_id: i64) -> Self {
        Self {
            a: NewArtifact {
                type_id: art_type(ty),
                ds_id: ctx.ds_id,
                file_id: Some(file_id),
                source_artifact_id: None,
                score: None,
                conclusion: None,
                configuration: None,
                justification: None,
                created_at: ctx.now(),
                attributes: Vec::new(),
            },
            module,
        }
    }

    /// A data artifact of a custom (database-registered) type.
    pub(crate) fn data_of_type(
        ctx: &JobContext,
        module: &'static str,
        type_id: i64,
        file_id: i64,
    ) -> Self {
        let mut s = Self::data(ctx, module, "TSK_GEN_INFO", file_id);
        s.a.type_id = type_id;
        s
    }

    /// An attribute of a custom (database-registered) type; empty values
    /// are left out.
    pub(crate) fn custom_text(mut self, type_id: i64, v: Option<String>) -> Self {
        if let Some(v) = v.filter(|v| !v.is_empty()) {
            self.a
                .attributes
                .push((type_id, Value::Text(v), self.module.to_owned()));
        }
        self
    }

    /// An integer attribute of a custom type.
    pub(crate) fn custom_int(mut self, type_id: i64, v: i64) -> Self {
        self.a
            .attributes
            .push((type_id, Value::Int(v), self.module.to_owned()));
        self
    }

    /// An analysis result with a score.
    pub(crate) fn result(
        ctx: &JobContext,
        module: &'static str,
        ty: &str,
        file_id: i64,
        significance: Significance,
    ) -> Self {
        let mut s = Self::data(ctx, module, ty, file_id);
        s.a.score = Some((significance, Priority::Normal));
        s
    }

    pub(crate) fn text(mut self, attr_name: &str, v: impl Into<String>) -> Self {
        self.a.attributes.push((
            attr(attr_name),
            Value::Text(v.into()),
            self.module.to_owned(),
        ));
        self
    }

    /// An attribute of any value type.
    pub(crate) fn value(mut self, attr_name: &str, v: Value) -> Self {
        self.a
            .attributes
            .push((attr(attr_name), v, self.module.to_owned()));
        self
    }

    pub(crate) fn configuration(mut self, c: impl Into<String>) -> Self {
        self.a.configuration = Some(c.into());
        self
    }

    /// A text attribute, left out when empty (Autopsy skips null values).
    pub(crate) fn text_opt(self, attr_name: &str, v: Option<String>) -> Self {
        match v.filter(|s| !s.is_empty()) {
            Some(v) => self.text(attr_name, v),
            None => self,
        }
    }

    /// A date-time attribute (Unix seconds), left out when absent.
    pub(crate) fn time(self, attr_name: &str, v: Option<i64>) -> Self {
        match v {
            Some(t) => self.value(attr_name, Value::DateTime(t)),
            None => self,
        }
    }

    /// Posts it without a duplicate check (see `JobContext::post_artifact_unchecked`).
    pub(crate) fn post_unchecked(self, ctx: &JobContext) {
        ctx.post_artifact_unchecked(self.a);
    }

    /// Posts it; `key` names the attributes that identify a duplicate.
    pub(crate) fn post(self, ctx: &JobContext, key: &[&str]) {
        let key: Vec<i64> = key.iter().map(|k| attr(k)).collect();
        ctx.post_artifact(self.a, &key);
    }
}

/// Lower-case hex.
pub(crate) fn hex(b: &[u8]) -> String {
    pm_core::decode::to_hex(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalogue name the modules use must exist.
    #[test]
    fn names_exist() {
        for a in [
            "TSK_SET_NAME",
            "TSK_HASH_MD5",
            "TSK_COMMENT",
            "TSK_CATEGORY",
            "TSK_DESCRIPTION",
        ] {
            assert!(attribute_type_id(a).is_some(), "{a}");
        }
        for t in [
            "TSK_HASHSET_HIT",
            "TSK_INTERESTING_FILE_HIT",
            "TSK_INTERESTING_ITEM",
            "TSK_EXT_MISMATCH_DETECTED",
            "TSK_ENCRYPTION_DETECTED",
            "TSK_ENCRYPTION_SUSPECTED",
            "TSK_METADATA_EXIF",
        ] {
            assert!(artifact_type_id(t).is_some(), "{t}");
        }
    }
}
