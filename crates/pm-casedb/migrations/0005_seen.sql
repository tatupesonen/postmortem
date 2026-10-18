-- Schema v5: review state. A file or result is "seen" once the examiner
-- has looked at it; unseen counts let a case be worked to zero. (A rowid
-- table: SQLite's update hook, which feeds the change bus, skips WITHOUT
-- ROWID tables.)
CREATE TABLE seen (
    id        INTEGER PRIMARY KEY,
    kind      INTEGER NOT NULL,             -- pm_casedb::ObjKind code (file, artifact)
    obj_id    INTEGER NOT NULL,
    examiner  TEXT NOT NULL,
    at        INTEGER NOT NULL,
    UNIQUE (kind, obj_id)
);
