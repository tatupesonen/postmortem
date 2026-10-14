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

    pub(crate) fn configuration(mut self, c: impl Into<String>) -> Self {
        self.a.configuration = Some(c.into());
        self
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
