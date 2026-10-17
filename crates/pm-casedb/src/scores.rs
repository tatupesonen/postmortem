//! Aggregate scores (Autopsy `ScoringManager`, ).
//!
//! Every file, artifact and OS account that has an analysis result or a
//! notable tag has one row in `scores` holding the best score of those
//! sources: priority first (an override result beats every normal one),
//! then significance. The row is raised in the same transaction that adds
//! a result or tag, and recomputed from scratch when one is removed, so the
//! Score tree and the "S" column never scan results.

use std::cmp::Ordering;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{CaseDb, Priority, Result, Significance};

/// The kind of object a score belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjKind {
    File = 0,
    Artifact = 1,
    OsAccount = 2,
}

impl ObjKind {
    pub fn from_code(c: i64) -> Option<Self> {
        match c {
            0 => Some(Self::File),
            1 => Some(Self::Artifact),
            2 => Some(Self::OsAccount),
            _ => None,
        }
    }
}

/// A score: significance plus priority (Autopsy `Score`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Score {
    pub significance: Significance,
    pub priority: Priority,
}

impl Score {
    pub const UNKNOWN: Self = Self::normal(Significance::Unknown);
    pub const NOTABLE: Self = Self::normal(Significance::Notable);
    pub const LIKELY_NOTABLE: Self = Self::normal(Significance::LikelyNotable);

    pub const fn normal(significance: Significance) -> Self {
        Self {
            significance,
            priority: Priority::Normal,
        }
    }

    fn from_codes(significance: i64, priority: i64) -> Self {
        Self {
            significance: Significance::from_code(significance).unwrap_or(Significance::Unknown),
            priority: Priority::from_code(priority).unwrap_or(Priority::Normal),
        }
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then(self.significance.cmp(&other.significance))
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The score a tag contributes: notable tags make an item notable, other
/// tags do not score (Autopsy `TaggingManager.getTagScore`).
pub(crate) fn tag_score(notable: bool) -> Score {
    if notable {
        Score::NOTABLE
    } else {
        Score::UNKNOWN
    }
}

/// Raises an object's aggregate score after a result or tag was added
/// (Autopsy `updateAggregateScoreAfterAddition`): the row only changes when
/// the new score is higher, and an unknown score is recorded only for
/// objects that have none yet.
pub(crate) fn raise(
    c: &Connection,
    kind: ObjKind,
    obj_id: i64,
    ds_id: Option<i64>,
    score: Score,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO scores (obj_kind, obj_id, ds_id, significance, priority) VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (obj_kind, obj_id) DO UPDATE SET significance = excluded.significance, priority = excluded.priority \
         WHERE excluded.priority > scores.priority \
            OR (excluded.priority = scores.priority AND excluded.significance > scores.significance)",
    )?
    .execute(params![kind as i64, obj_id, ds_id, score.significance as i64, score.priority as i64])?;
    Ok(())
}

/// Recomputes an object's aggregate score from all of its analysis results
/// and tags (Autopsy `updateAggregateScoreAfterDeletion`).
pub(crate) fn recompute(c: &Connection, kind: ObjKind, obj_id: i64) -> Result<()> {
    let result_rank: Option<i64> = match kind {
        ObjKind::File => c
            .prepare_cached(
                "SELECT MAX(COALESCE(priority, 0) * 10 + significance) FROM artifacts \
                 WHERE file_id = ?1 AND source_artifact_id IS NULL AND significance IS NOT NULL",
            )?
            .query_row([obj_id], |r| r.get(0))?,
        ObjKind::Artifact => c
            .prepare_cached(
                "SELECT MAX(COALESCE(priority, 0) * 10 + significance) FROM artifacts \
                 WHERE source_artifact_id = ?1 AND significance IS NOT NULL",
            )?
            .query_row([obj_id], |r| r.get(0))?,
        ObjKind::OsAccount => None,
    };
    let notable_tag: bool = match kind {
        ObjKind::File => c
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM file_tags t JOIN tag_names n ON n.id = t.tag_name_id \
                 WHERE t.file_id = ?1 AND n.notable = 1)",
            )?
            .query_row([obj_id], |r| r.get(0))?,
        ObjKind::Artifact => c
            .prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM artifact_tags t JOIN tag_names n ON n.id = t.tag_name_id \
                 WHERE t.artifact_id = ?1 AND n.notable = 1)",
            )?
            .query_row([obj_id], |r| r.get(0))?,
        ObjKind::OsAccount => false,
    };
    let mut best = result_rank.map(|r| Score::from_codes(r % 10, r / 10));
    if notable_tag {
        best = best.max(Some(tag_score(true)));
    }
    match best {
        Some(score) => {
            c.prepare_cached(
                "UPDATE scores SET significance = ?3, priority = ?4 WHERE obj_kind = ?1 AND obj_id = ?2",
            )?
            .execute(params![kind as i64, obj_id, score.significance as i64, score.priority as i64])?;
        }
        None => {
            c.prepare_cached("DELETE FROM scores WHERE obj_kind = ?1 AND obj_id = ?2")?
                .execute(params![kind as i64, obj_id])?;
        }
    }
    Ok(())
}

/// The object an analysis result scores: its source artifact if it has
/// one, else its file.
pub(crate) fn scored_object(
    file_id: Option<i64>,
    source_artifact_id: Option<i64>,
) -> Option<(ObjKind, i64)> {
    source_artifact_id
        .map(|a| (ObjKind::Artifact, a))
        .or(file_id.map(|f| (ObjKind::File, f)))
}

/// SQL filter of Autopsy's Score tree nodes ("Bad Items" / "Suspicious
/// Items"): the significance with normal or override priority.
fn significance_filter(bad: bool) -> Significance {
    if bad {
        Significance::Notable
    } else {
        Significance::LikelyNotable
    }
}

impl CaseDb {
    /// An object's aggregate score ([`Score::UNKNOWN`] when unscored).
    pub fn score(&self, kind: ObjKind, obj_id: i64) -> Result<Score> {
        self.read(|c| {
            Ok(c.prepare_cached(
                "SELECT significance, priority FROM scores WHERE obj_kind = ?1 AND obj_id = ?2",
            )?
            .query_row(params![kind as i64, obj_id], |r| {
                Ok(Score::from_codes(r.get(0)?, r.get(1)?))
            })
            .optional()?
            .unwrap_or(Score::UNKNOWN))
        })
    }

    /// Aggregate scores of many objects of one kind (for a result page);
    /// unscored ids are omitted.
    pub fn scores(&self, kind: ObjKind, ids: &[i64]) -> Result<Vec<(i64, Score)>> {
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT significance, priority FROM scores WHERE obj_kind = ?1 AND obj_id = ?2",
            )?;
            let mut out = Vec::new();
            for &id in ids {
                if let Some(s) = st
                    .query_row(params![kind as i64, id], |r| {
                        Ok(Score::from_codes(r.get(0)?, r.get(1)?))
                    })
                    .optional()?
                {
                    out.push((id, s));
                }
            }
            Ok(out)
        })
    }

    /// Number of files and data artifacts in Autopsy's "Bad Items" (`bad`)
    /// or "Suspicious Items" Score node, optionally for one data source.
    pub fn score_count(&self, bad: bool, ds_id: Option<i64>) -> Result<u64> {
        let sig = significance_filter(bad) as i64;
        self.read(|c| {
            let n: i64 = c
                .prepare_cached(
                    "SELECT COUNT(*) FROM scores s WHERE s.significance = ?1 AND (?2 IS NULL OR s.ds_id = ?2) \
                     AND (s.obj_kind = 0 OR (s.obj_kind = 1 AND EXISTS (SELECT 1 FROM artifacts a \
                          JOIN artifact_types t ON t.id = a.type_id WHERE a.id = s.obj_id AND t.category = 0)))",
                )?
                .query_row(params![sig, ds_id], |r| r.get(0))?;
            Ok(n.max(0) as u64)
        })
    }

    /// One keyset page of the objects in a Score node: `(kind, id)` in
    /// kind, id order after `after`.
    pub fn scored_objects(
        &self,
        bad: bool,
        ds_id: Option<i64>,
        after: (ObjKind, i64),
        limit: usize,
    ) -> Result<Vec<(ObjKind, i64)>> {
        let sig = significance_filter(bad) as i64;
        self.read(|c| {
            let mut st = c.prepare_cached(
                "SELECT s.obj_kind, s.obj_id FROM scores s WHERE s.significance = ?1 AND (?2 IS NULL OR s.ds_id = ?2) \
                 AND (s.obj_kind, s.obj_id) > (?3, ?4) \
                 AND (s.obj_kind = 0 OR (s.obj_kind = 1 AND EXISTS (SELECT 1 FROM artifacts a \
                      JOIN artifact_types t ON t.id = a.type_id WHERE a.id = s.obj_id AND t.category = 0))) \
                 ORDER BY s.obj_kind, s.obj_id LIMIT ?5",
            )?;
            let rows = st
                .query_map(
                    params![sig, ds_id, after.0 as i64, after.1, limit as i64],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )?
                .filter_map(|r| match r {
                    Ok((k, id)) => ObjKind::from_code(k).map(|k| Ok((k, id))),
                    Err(e) => Some(Err(e)),
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_matches_autopsy() {
        use Significance::*;
        // NOTABLE > NONE > LIKELY_NOTABLE > LIKELY_NONE > UNKNOWN
        let order = [Unknown, LikelyNone, LikelyNotable, None, Notable];
        for w in order.windows(2) {
            assert!(Score::normal(w[0]) < Score::normal(w[1]));
        }
        let over = Score {
            significance: LikelyNone,
            priority: Priority::Override,
        };
        assert!(over > Score::NOTABLE, "override priority wins");
    }

    #[test]
    fn scored_object_prefers_the_source_artifact() {
        assert_eq!(
            scored_object(Some(5), Some(9)),
            Some((ObjKind::Artifact, 9))
        );
        assert_eq!(scored_object(Some(5), None), Some((ObjKind::File, 5)));
        assert_eq!(scored_object(None, None), None);
    }
}
