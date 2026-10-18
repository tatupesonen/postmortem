//! The examiner's notebook: free-text notes, optionally about a file or a
//! result; notes marked as findings are listed in reports.

use rusqlite::{OptionalExtension, Row, params};

use crate::{CaseDb, ObjKind, Result};

/// A note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub examiner: String,
    pub text: String,
    /// The file or result the note is about.
    pub about: Option<(ObjKind, i64)>,
    pub finding: bool,
}

impl Note {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let kind: Option<i64> = r.get(5)?;
        let id: Option<i64> = r.get(6)?;
        Ok(Self {
            id: r.get(0)?,
            created_at: r.get(1)?,
            updated_at: r.get(2)?,
            examiner: r.get(3)?,
            text: r.get(4)?,
            about: kind.and_then(ObjKind::from_code).zip(id),
            finding: r.get::<_, i64>(7)? != 0,
        })
    }
}

const NOTE_COLS: &str = "id, created_at, updated_at, examiner, text, ref_kind, ref_id, finding";

impl CaseDb {
    /// Adds a note; returns its id.
    pub fn add_note(
        &self,
        text: &str,
        about: Option<(ObjKind, i64)>,
        examiner: &str,
        now: i64,
    ) -> Result<i64> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO notes (created_at, updated_at, examiner, text, ref_kind, ref_id) \
                 VALUES (?1, ?1, ?2, ?3, ?4, ?5)",
                params![
                    now,
                    examiner,
                    text,
                    about.map(|(k, _)| k as i64),
                    about.map(|(_, id)| id)
                ],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// Changes a note's text and finding mark.
    pub fn update_note(&self, id: i64, text: &str, finding: bool, now: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE notes SET text = ?2, finding = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, text, finding, now],
            )?;
            Ok(())
        })
    }

    pub fn delete_note(&self, id: i64) -> Result<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM notes WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn note(&self, id: i64) -> Result<Option<Note>> {
        self.read(|c| {
            Ok(c.query_row(
                &format!("SELECT {NOTE_COLS} FROM notes WHERE id = ?1"),
                [id],
                Note::from_row,
            )
            .optional()?)
        })
    }

    /// Every note, newest first (`findings_only`: those marked findings).
    pub fn notes(&self, findings_only: bool) -> Result<Vec<Note>> {
        self.read(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {NOTE_COLS} FROM notes WHERE (?1 = 0 OR finding = 1) ORDER BY created_at DESC, id DESC"
            ))?;
            let rows = st
                .query_map([findings_only], Note::from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}
