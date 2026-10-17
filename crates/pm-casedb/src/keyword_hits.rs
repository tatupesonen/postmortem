//! The Keyword Hits tree (Autopsy `datamodel/KeywordHits`): hits grouped
//! by list › keyword (or regular expression) › matched text.
//!
//! Grouping rules, as Autopsy's: a hit without `TSK_SET_NAME` (an ad-hoc
//! search) is listed under "Single Literal Keyword Search" or "Single
//! Regular Expression Search". Exact and substring hits (`TSK_KEYWORD_REGEXP`
//! absent, or search type 1) are grouped by the original term, which
//! substring hits keep in `TSK_KEYWORD_REGEXP`; regular-expression hits are
//! grouped by the expression and then by the matched text.

use rusqlite::params;

use crate::catalog::{artifact_type_id, attribute_type_id};
use crate::{ARTIFACT_COLS, ArtifactRecord, CaseDb, Result};

/// Autopsy's group for ad-hoc exact and substring hits.
pub const SINGLE_LITERAL: &str = "Single Literal Keyword Search";
/// Autopsy's group for ad-hoc regular-expression hits.
pub const SINGLE_REGEX: &str = "Single Regular Expression Search";

/// One leaf group of the tree with its hit count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeywordHitGroup {
    /// List name, or [`SINGLE_LITERAL`] / [`SINGLE_REGEX`].
    pub list: String,
    /// The keyword or regular expression.
    pub term: String,
    /// The matched text, for regular expressions.
    pub instance: Option<String>,
    pub count: u64,
}

/// A node of the tree to list hits for (`None` levels match everything).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct KeywordHitFilter {
    pub list: String,
    pub term: Option<String>,
    pub instance: Option<String>,
}

/// Per-hit `list, term, instance` derived with Autopsy's rules.
const GROUPED: &str = "WITH raw AS (\
     SELECT a.id AS id, \
       NULLIF((SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?2 LIMIT 1), '') AS list, \
       (SELECT value_int FROM attributes WHERE artifact_id = a.id AND type_id = ?3 LIMIT 1) AS st, \
       NULLIF((SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?4 LIMIT 1), '') AS re, \
       COALESCE((SELECT value_text FROM attributes WHERE artifact_id = a.id AND type_id = ?5 LIMIT 1), '') AS kw \
     FROM artifacts a WHERE a.type_id = ?1), \
   g AS (SELECT id, \
       CASE WHEN st = 1 OR re IS NULL THEN 0 ELSE 1 END AS is_re, \
       list, \
       CASE WHEN st = 1 OR re IS NULL THEN COALESCE(re, kw) ELSE re END AS term, \
       CASE WHEN st = 1 OR re IS NULL THEN NULL ELSE kw END AS inst \
     FROM raw), \
   k AS (SELECT id, \
       COALESCE(list, CASE is_re WHEN 1 THEN ?6 ELSE ?7 END) AS list, term, inst FROM g)";

/// Type ids: keyword hit, set name, search type, regexp, keyword.
fn keyword_hit_params() -> [i64; 5] {
    let attr = |n: &str| attribute_type_id(n).unwrap_or(-1);
    [
        artifact_type_id("TSK_KEYWORD_HIT").unwrap_or(9),
        attr("TSK_SET_NAME"),
        attr("TSK_KEYWORD_SEARCH_TYPE"),
        attr("TSK_KEYWORD_REGEXP"),
        attr("TSK_KEYWORD"),
    ]
}

impl CaseDb {
    /// Every leaf group with its count, ordered by list (the two ad-hoc
    /// groups first), term and instance.
    pub fn keyword_hit_groups(&self) -> Result<Vec<KeywordHitGroup>> {
        let [kwh, set, st, re, kw] = keyword_hit_params();
        self.read(|c| {
            let mut stmt = c.prepare_cached(&format!(
                "{GROUPED} SELECT list, term, inst, COUNT(*) FROM k GROUP BY list, term, inst \
                 ORDER BY CASE list WHEN ?7 THEN 0 WHEN ?6 THEN 1 ELSE 2 END, list, term, inst"
            ))?;
            let rows = stmt
                .query_map(
                    params![kwh, set, st, re, kw, SINGLE_REGEX, SINGLE_LITERAL],
                    |r| {
                        Ok(KeywordHitGroup {
                            list: r.get(0)?,
                            term: r.get(1)?,
                            instance: r.get(2)?,
                            count: r.get::<_, i64>(3)?.unsigned_abs(),
                        })
                    },
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One keyset page of the hits under a tree node.
    pub fn keyword_hits_in(
        &self,
        f: &KeywordHitFilter,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ArtifactRecord>> {
        let [kwh, set, st, re, kw] = keyword_hit_params();
        let cols = ARTIFACT_COLS
            .split(',')
            .map(|c| format!("a.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        self.read(|c| {
            let mut stmt = c.prepare_cached(&format!(
                "{GROUPED} SELECT {cols} FROM k JOIN artifacts a ON a.id = k.id \
                 WHERE k.list = ?8 AND (?9 IS NULL OR k.term = ?9) AND (?10 IS NULL OR k.inst = ?10) \
                 AND k.id > ?11 ORDER BY k.id LIMIT ?12"
            ))?;
            let rows = stmt
                .query_map(
                    params![
                        kwh,
                        set,
                        st,
                        re,
                        kw,
                        SINGLE_REGEX,
                        SINGLE_LITERAL,
                        f.list,
                        f.term,
                        f.instance,
                        after_id,
                        i64::try_from(limit).unwrap_or(i64::MAX)
                    ],
                    ArtifactRecord::from_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
