//! Tags on files and results (TSK `TaggingManager`, Autopsy `TagsManager`).
//!
//! * A notable tag scores the item notable; removing it recomputes the
//!   aggregate score.
//! * Tag names in a tag set are mutually exclusive on one item: adding one
//!   removes the item's other tags from the same set (TSK
//!   `addContentTag` / `addArtifactTag`).

use rusqlite::{Connection, OptionalExtension, params};

use crate::scores::{self, ObjKind};
use crate::{CaseDb, FileTag, Result, TagName};

/// A tag on an artifact or analysis result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactTag {
    pub id: i64,
    pub artifact_id: i64,
    pub tag_name_id: i64,
    pub tag_name: String,
    pub comment: String,
    pub examiner: String,
    pub created_at: i64,
}

/// A set of mutually exclusive tag names (e.g. Project VIC categories).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagSet {
    pub id: i64,
    pub name: String,
    /// Members in rank order.
    pub names: Vec<TagName>,
}

/// A tag name to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTagName {
    pub name: String,
    pub description: String,
    /// TSK `TagName.HTML_COLOR` name (`NONE`, `RED`, …).
    pub color: String,
    pub notable: bool,
}

/// Name of the Project VIC tag set (Autopsy Image Gallery categories).
pub const PROJECT_VIC: &str = "Project VIC";

pub(crate) const TAG_NAME_COLS: &str = "id, name, description, color, notable, tag_set_id, rank";

fn artifact_tag_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactTag> {
    Ok(ArtifactTag {
        id: r.get(0)?,
        artifact_id: r.get(1)?,
        tag_name_id: r.get(2)?,
        tag_name: r.get(3)?,
        comment: r.get(4)?,
        examiner: r.get(5)?,
        created_at: r.get(6)?,
    })
}

const FILE_TAG_SELECT: &str = "SELECT ft.id, ft.file_id, ft.tag_name_id, tn.name, ft.comment, ft.examiner, ft.created_at \
     FROM file_tags ft JOIN tag_names tn ON tn.id = ft.tag_name_id";
const ARTIFACT_TAG_SELECT: &str = "SELECT at.id, at.artifact_id, at.tag_name_id, tn.name, at.comment, at.examiner, at.created_at \
     FROM artifact_tags at JOIN tag_names tn ON tn.id = at.tag_name_id";

/// `(notable, tag set)` of a tag name.
fn tag_name_info(c: &Connection, tag_name_id: i64) -> Result<(bool, Option<i64>)> {
    Ok(
        c.prepare_cached("SELECT notable, tag_set_id FROM tag_names WHERE id = ?1")?
            .query_row([tag_name_id], |r| Ok((r.get(0)?, r.get(1)?)))?,
    )
}

impl CaseDb {
    pub fn tag_names(&self) -> Result<Vec<TagName>> {
        self.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {TAG_NAME_COLS} FROM tag_names ORDER BY name COLLATE NOCASE"
            ))?;
            let rows = st
                .query_map([], TagName::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn add_tag_name(&self, name: &str, description: &str, notable: bool) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO tag_names (name, description, notable) VALUES (?1, ?2, ?3)",
                params![name, description, notable],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// Changes a tag name's description, colour and notable flag; the
    /// scores of everything carrying it are recomputed.
    pub fn update_tag_name(
        &self,
        id: i64,
        description: &str,
        color: &str,
        notable: bool,
    ) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE tag_names SET description = ?2, color = ?3, notable = ?4 WHERE id = ?1",
                params![id, description, color, notable],
            )?;
            let files: Vec<i64> = tx
                .prepare("SELECT DISTINCT file_id FROM file_tags WHERE tag_name_id = ?1")?
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for f in files {
                rescore(tx, ObjKind::File, f)?;
            }
            let arts: Vec<i64> = tx
                .prepare("SELECT DISTINCT artifact_id FROM artifact_tags WHERE tag_name_id = ?1")?
                .query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for a in arts {
                rescore(tx, ObjKind::Artifact, a)?;
            }
            Ok(())
        })
    }

    /// Creates a tag set with its members in rank order (TSK `addTagSet`).
    pub fn add_tag_set(&self, name: &str, members: &[NewTagName]) -> Result<TagSet> {
        self.write(|tx| {
            tx.execute("INSERT INTO tag_sets (name) VALUES (?1)", [name])?;
            let set_id = tx.last_insert_rowid();
            for (rank, m) in members.iter().enumerate() {
                tx.execute(
                    "INSERT INTO tag_names (name, description, color, notable, tag_set_id, rank) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT (name) DO UPDATE SET tag_set_id = excluded.tag_set_id, rank = excluded.rank",
                    params![m.name, m.description, m.color, m.notable, set_id, rank as i64],
                )?;
            }
            tag_set(tx, set_id)
        })
    }

    pub fn tag_sets(&self) -> Result<Vec<TagSet>> {
        self.read(|c| {
            let ids: Vec<i64> = c
                .prepare("SELECT id FROM tag_sets ORDER BY name")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            ids.into_iter().map(|id| tag_set(c, id)).collect()
        })
    }

    /// Tag names used on at least one file or result, optionally only by
    /// one examiner or within one data source (Autopsy's Tags tree).
    pub fn tag_names_in_use(
        &self,
        examiner: Option<&str>,
        ds_id: Option<i64>,
    ) -> Result<Vec<TagName>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "SELECT {TAG_NAME_COLS} FROM tag_names n WHERE \
                   EXISTS (SELECT 1 FROM file_tags t JOIN files f ON f.id = t.file_id WHERE t.tag_name_id = n.id \
                           AND (?1 IS NULL OR t.examiner = ?1) AND (?2 IS NULL OR f.ds_id = ?2)) \
                OR EXISTS (SELECT 1 FROM artifact_tags t JOIN artifacts a ON a.id = t.artifact_id WHERE t.tag_name_id = n.id \
                           AND (?1 IS NULL OR t.examiner = ?1) AND (?2 IS NULL OR a.ds_id = ?2)) \
                 ORDER BY name COLLATE NOCASE"
            ))?;
            let rows = st
                .query_map(params![examiner, ds_id], TagName::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// `(file tags, result tags)` counts of a tag name.
    pub fn tag_usage(
        &self,
        tag_name_id: i64,
        examiner: Option<&str>,
        ds_id: Option<i64>,
    ) -> Result<(u64, u64)> {
        self.read(|c| {
            let files: i64 = c
                .prepare_cached(
                    "SELECT COUNT(*) FROM file_tags t JOIN files f ON f.id = t.file_id WHERE t.tag_name_id = ?1 \
                     AND (?2 IS NULL OR t.examiner = ?2) AND (?3 IS NULL OR f.ds_id = ?3)",
                )?
                .query_row(params![tag_name_id, examiner, ds_id], |r| r.get(0))?;
            let results: i64 = c
                .prepare_cached(
                    "SELECT COUNT(*) FROM artifact_tags t JOIN artifacts a ON a.id = t.artifact_id WHERE t.tag_name_id = ?1 \
                     AND (?2 IS NULL OR t.examiner = ?2) AND (?3 IS NULL OR a.ds_id = ?3)",
                )?
                .query_row(params![tag_name_id, examiner, ds_id], |r| r.get(0))?;
            Ok((files.max(0) as u64, results.max(0) as u64))
        })
    }

    // ---------------------------------------------------------- file tags

    /// Tags a file (`begin_end`: an optional byte range, for content
    /// viewer region tags).
    pub fn tag_file(
        &self,
        file_id: i64,
        tag_name_id: i64,
        comment: &str,
        examiner: &str,
        now: i64,
    ) -> Result<i64> {
        self.write(|tx| {
            let (notable, set) = tag_name_info(tx, tag_name_id)?;
            let mut removed = false;
            if let Some(set) = set {
                removed = tx.execute(
                    "DELETE FROM file_tags WHERE file_id = ?1 AND tag_name_id IN (SELECT id FROM tag_names WHERE tag_set_id = ?2)",
                    params![file_id, set],
                )? > 0;
            }
            tx.execute(
                "INSERT INTO file_tags (file_id, tag_name_id, comment, examiner, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![file_id, tag_name_id, comment, examiner, now],
            )?;
            let id = tx.last_insert_rowid();
            if removed {
                rescore(tx, ObjKind::File, file_id)?;
            } else if notable {
                let ds_id: i64 = tx.query_row("SELECT ds_id FROM files WHERE id = ?1", [file_id], |r| r.get(0))?;
                scores::raise(tx, ObjKind::File, file_id, Some(ds_id), scores::tag_score(true))?;
            }
            Ok(id)
        })
    }

    pub fn untag_file(&self, file_tag_id: i64) -> Result<()> {
        self.write(|tx| {
            let file_id: Option<i64> = tx
                .query_row(
                    "SELECT file_id FROM file_tags WHERE id = ?1",
                    [file_tag_id],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute("DELETE FROM file_tags WHERE id = ?1", [file_tag_id])?;
            if let Some(file_id) = file_id {
                rescore(tx, ObjKind::File, file_id)?;
            }
            Ok(())
        })
    }

    pub fn file_tags(&self, file_id: i64) -> Result<Vec<FileTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{FILE_TAG_SELECT} WHERE ft.file_id = ?1 ORDER BY ft.id"
            ))?;
            let rows = st
                .query_map([file_id], FileTag::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// The tags of many files (one listing page), by file.
    pub fn file_tags_of(&self, file_ids: &[i64]) -> Result<Vec<FileTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{FILE_TAG_SELECT} WHERE ft.file_id = ?1 ORDER BY ft.id"
            ))?;
            let mut out = Vec::new();
            for id in file_ids {
                for t in st.query_map([id], FileTag::from_row)? {
                    out.push(t?);
                }
            }
            Ok(out)
        })
    }

    /// The tags of many artifacts (one result page).
    pub fn artifact_tags_of(&self, artifact_ids: &[i64]) -> Result<Vec<ArtifactTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{ARTIFACT_TAG_SELECT} WHERE at.artifact_id = ?1 ORDER BY at.id"
            ))?;
            let mut out = Vec::new();
            for id in artifact_ids {
                for t in st.query_map([id], artifact_tag_from_row)? {
                    out.push(t?);
                }
            }
            Ok(out)
        })
    }

    /// Creates Autopsy's "Project VIC" tag set unless the case already has
    /// it (the Image Gallery service does this when a case opens).
    pub fn ensure_project_vic_tags(&self) -> Result<()> {
        if self.tag_sets()?.iter().any(|s| s.name == PROJECT_VIC) {
            return Ok(());
        }
        let n = |name: &str, color: &str, notable: bool| NewTagName {
            name: name.into(),
            description: String::new(),
            color: color.into(),
            notable,
        };
        self.add_tag_set(
            PROJECT_VIC,
            &[
                n("Non-Pertinent", "GREEN", false),
                n("Child Abuse Material - (CAM)", "RED", true),
                n("Child Exploitive (Non-CAM) Age Difficult", "YELLOW", true),
                n("CGI/Animation - Child Exploitive", "FUCHSIA", true),
                n("Comparison Images", "BLUE", false),
            ],
        )?;
        Ok(())
    }

    /// One keyset page of the file tags with a tag name.
    pub fn file_tags_named(
        &self,
        tag_name_id: i64,
        examiner: Option<&str>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<FileTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{FILE_TAG_SELECT} WHERE ft.tag_name_id = ?1 AND (?2 IS NULL OR ft.examiner = ?2) AND ft.id > ?3 ORDER BY ft.id LIMIT ?4"
            ))?;
            let rows = st
                .query_map(params![tag_name_id, examiner, after_id, limit as i64], FileTag::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn files_with_tag(&self, tag_name_id: i64) -> Result<Vec<(FileTag, crate::FileRecord)>> {
        let tags = self.file_tags_named(tag_name_id, None, 0, usize::MAX >> 1)?;
        let mut out = Vec::with_capacity(tags.len());
        for t in tags {
            if let Some(f) = self.file(t.file_id)? {
                out.push((t, f));
            }
        }
        Ok(out)
    }

    /// `(tag name id, count)` of tagged files.
    pub fn tag_counts(&self) -> Result<Vec<(i64, u64)>> {
        self.read(|c| {
            let mut st =
                c.prepare("SELECT tag_name_id, COUNT(*) FROM file_tags GROUP BY tag_name_id")?;
            let rows = st
                .query_map([], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?.max(0) as u64))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // ------------------------------------------------------ artifact tags

    /// Tags an artifact or analysis result.
    pub fn tag_artifact(
        &self,
        artifact_id: i64,
        tag_name_id: i64,
        comment: &str,
        examiner: &str,
        now: i64,
    ) -> Result<i64> {
        self.write(|tx| {
            let (notable, set) = tag_name_info(tx, tag_name_id)?;
            let mut removed = false;
            if let Some(set) = set {
                removed = tx.execute(
                    "DELETE FROM artifact_tags WHERE artifact_id = ?1 AND tag_name_id IN (SELECT id FROM tag_names WHERE tag_set_id = ?2)",
                    params![artifact_id, set],
                )? > 0;
            }
            tx.execute(
                "INSERT INTO artifact_tags (artifact_id, tag_name_id, comment, examiner, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![artifact_id, tag_name_id, comment, examiner, now],
            )?;
            let id = tx.last_insert_rowid();
            if removed {
                rescore(tx, ObjKind::Artifact, artifact_id)?;
            } else if notable {
                let ds_id: i64 = tx.query_row("SELECT ds_id FROM artifacts WHERE id = ?1", [artifact_id], |r| r.get(0))?;
                scores::raise(tx, ObjKind::Artifact, artifact_id, Some(ds_id), scores::tag_score(true))?;
            }
            Ok(id)
        })
    }

    pub fn untag_artifact(&self, artifact_tag_id: i64) -> Result<()> {
        self.write(|tx| {
            let artifact_id: Option<i64> = tx
                .query_row(
                    "SELECT artifact_id FROM artifact_tags WHERE id = ?1",
                    [artifact_tag_id],
                    |r| r.get(0),
                )
                .optional()?;
            tx.execute("DELETE FROM artifact_tags WHERE id = ?1", [artifact_tag_id])?;
            if let Some(a) = artifact_id {
                rescore(tx, ObjKind::Artifact, a)?;
            }
            Ok(())
        })
    }

    pub fn artifact_tags(&self, artifact_id: i64) -> Result<Vec<ArtifactTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{ARTIFACT_TAG_SELECT} WHERE at.artifact_id = ?1 ORDER BY at.id"
            ))?;
            let rows = st
                .query_map([artifact_id], artifact_tag_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// One keyset page of the result tags with a tag name.
    pub fn artifact_tags_named(
        &self,
        tag_name_id: i64,
        examiner: Option<&str>,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ArtifactTag>> {
        self.read(|c| {
            let mut st = c.prepare_cached(&format!(
                "{ARTIFACT_TAG_SELECT} WHERE at.tag_name_id = ?1 AND (?2 IS NULL OR at.examiner = ?2) AND at.id > ?3 ORDER BY at.id LIMIT ?4"
            ))?;
            let rows = st
                .query_map(params![tag_name_id, examiner, after_id, limit as i64], artifact_tag_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

/// Recomputes the score of a tagged object whose own row may not exist
/// yet (a set swap from a notable to a non-notable tag).
fn rescore(c: &Connection, kind: ObjKind, obj_id: i64) -> Result<()> {
    let ds_id: Option<i64> = match kind {
        ObjKind::File => c
            .query_row("SELECT ds_id FROM files WHERE id = ?1", [obj_id], |r| {
                r.get(0)
            })
            .optional()?,
        ObjKind::Artifact => c
            .query_row("SELECT ds_id FROM artifacts WHERE id = ?1", [obj_id], |r| {
                r.get(0)
            })
            .optional()?,
        ObjKind::OsAccount => None,
    };
    // Make sure a row exists so `recompute` can set (or drop) it.
    scores::raise(c, kind, obj_id, ds_id, crate::Score::UNKNOWN)?;
    scores::recompute(c, kind, obj_id)
}

fn tag_set(c: &Connection, id: i64) -> Result<TagSet> {
    let name: String = c.query_row("SELECT name FROM tag_sets WHERE id = ?1", [id], |r| {
        r.get(0)
    })?;
    let names = c
        .prepare(&format!(
            "SELECT {TAG_NAME_COLS} FROM tag_names WHERE tag_set_id = ?1 ORDER BY rank, id"
        ))?
        .query_map([id], TagName::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(TagSet { id, name, names })
}
