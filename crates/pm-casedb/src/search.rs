//! Read helpers for keyword search (search track; no schema changes).

use std::collections::HashSet;

use rusqlite::params;

use crate::catalog::{artifact_type_id, attribute_type_id};
use crate::{ARTIFACT_COLS, ArtifactRecord, CaseDb, Result};

/// What identifies a keyword hit on a file: keyword, set (list) name,
/// search type, regular expression and associated artifact. Re-running
/// Keyword Search posts a hit only if the file has none with the same
/// identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeywordHitKey {
    pub keyword: String,
    pub set_name: Option<String>,
    pub search_type: Option<i64>,
    pub regexp: Option<String>,
    pub associated_artifact: Option<i64>,
}

impl CaseDb {
    /// The identities of a file's `TSK_KEYWORD_HIT` results, in one query.
    pub fn keyword_hit_keys(&self, file_id: i64) -> Result<HashSet<KeywordHitKey>> {
        let kwh = artifact_type_id("TSK_KEYWORD_HIT").unwrap_or(9);
        let attr = |n: &str| attribute_type_id(n).unwrap_or(-1);
        let (kw, set, st, re, assoc) = (
            attr("TSK_KEYWORD"),
            attr("TSK_SET_NAME"),
            attr("TSK_KEYWORD_SEARCH_TYPE"),
            attr("TSK_KEYWORD_REGEXP"),
            attr("TSK_ASSOCIATED_ARTIFACT"),
        );
        self.read(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT \
                   (SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?3 LIMIT 1), \
                   (SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?4 LIMIT 1), \
                   (SELECT value_int FROM attributes WHERE artifact_id = a.id AND type_id = ?5 LIMIT 1), \
                   (SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?6 LIMIT 1), \
                   (SELECT value_int FROM attributes WHERE artifact_id = a.id AND type_id = ?7 LIMIT 1) \
                 FROM artifacts a WHERE a.file_id = ?1 AND a.type_id = ?2",
            )?;
            let rows = stmt
                .query_map(params![file_id, kwh, kw, set, st, re, assoc], |r| {
                    Ok(KeywordHitKey {
                        keyword: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        set_name: r.get(1)?,
                        search_type: r.get(2)?,
                        regexp: r.get(3)?,
                        associated_artifact: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            Ok(rows)
        })
    }

    /// Artifacts of a data source with ids above `after_id`, by id (keyset
    /// paging), for indexing their text.
    pub fn artifacts_after(
        &self,
        ds_id: i64,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ArtifactRecord>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {ARTIFACT_COLS} FROM artifacts WHERE ds_id = ?1 AND id > ?2 ORDER BY id LIMIT {limit}"
            ))?;
            let rows = st
                .query_map(params![ds_id, after_id], ArtifactRecord::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::{artifact_type_id, attribute_type_id};
    use crate::{CaseDb, NewArtifact, Value};

    fn hit(file_id: i64, keyword: &str, set: &str, regexp: Option<&str>) -> NewArtifact {
        let a = |n: &str| attribute_type_id(n).unwrap();
        let mut attributes = vec![
            (
                a("TSK_KEYWORD"),
                Value::Text(keyword.into()),
                "m".to_owned(),
            ),
            (a("TSK_SET_NAME"), Value::Text(set.into()), "m".to_owned()),
            (a("TSK_KEYWORD_SEARCH_TYPE"), Value::Int(2), "m".to_owned()),
        ];
        if let Some(r) = regexp {
            attributes.push((
                a("TSK_KEYWORD_REGEXP"),
                Value::Text(r.into()),
                "m".to_owned(),
            ));
        }
        NewArtifact {
            type_id: artifact_type_id("TSK_KEYWORD_HIT").unwrap(),
            ds_id: 1,
            file_id: Some(file_id),
            source_artifact_id: None,
            score: None,
            conclusion: None,
            configuration: None,
            justification: None,
            created_at: 0,
            attributes,
        }
    }

    #[test]
    fn hit_keys_and_artifact_paging() {
        let dir = tempfile::tempdir().unwrap();
        let db = CaseDb::create(dir.path().join("case.db")).unwrap();
        let (ds, f) = db
            .bulk(|b| {
                let ds = b.add_data_source(&crate::NewDataSource {
                    name: "img".into(),
                    kind: crate::DataSourceKind::DiskImage,
                    paths: vec!["/img.dd".into()],
                    size: 0,
                    sector_size: 512,
                    timezone: "UTC".into(),
                    image_format: None,
                    md5: None,
                    sha1: None,
                    sha256: None,
                    acquisition: Vec::new(),
                    added_at: 0,
                })?;
                let f = b.add_file(&crate::NewFile::new(
                    ds,
                    None,
                    "/",
                    "f.txt",
                    crate::Origin::Local,
                    crate::FileKindCode::Regular,
                ))?;
                Ok((ds, f))
            })
            .unwrap();
        let mut h1 = hit(f, "secret", "list", Some("sec.*"));
        h1.ds_id = ds;
        let mut h2 = hit(f, "bomb", "notable", None);
        h2.ds_id = ds;
        let id1 = db.add_artifact(&h1).unwrap();
        let id2 = db.add_artifact(&h2).unwrap();
        let keys = db.keyword_hit_keys(f).unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().any(|k| k.keyword == "secret"
            && k.regexp.as_deref() == Some("sec.*")
            && k.search_type == Some(2)
            && k.set_name.as_deref() == Some("list")));
        assert!(
            keys.iter()
                .any(|k| k.keyword == "bomb" && k.regexp.is_none())
        );
        assert!(db.keyword_hit_keys(f + 1).unwrap().is_empty());
        let page = db.artifacts_after(ds, 0, 1).unwrap();
        assert_eq!(page.iter().map(|a| a.id).collect::<Vec<_>>(), [id1]);
        let page = db.artifacts_after(ds, id1, 10).unwrap();
        assert_eq!(page.iter().map(|a| a.id).collect::<Vec<_>>(), [id2]);
        assert!(db.artifacts_after(ds + 1, 0, 10).unwrap().is_empty());
    }
}
