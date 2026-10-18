-- Schema v4: the examiner's notebook. A note may refer to a file or a
-- result; notes marked as findings go into reports.
CREATE TABLE notes (
    id          INTEGER PRIMARY KEY,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL,
    examiner    TEXT NOT NULL,
    text        TEXT NOT NULL,
    ref_kind    INTEGER,                    -- pm_casedb::ObjKind code (file, artifact)
    ref_id      INTEGER,
    finding     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX notes_ref ON notes(ref_kind, ref_id) WHERE ref_id IS NOT NULL;
