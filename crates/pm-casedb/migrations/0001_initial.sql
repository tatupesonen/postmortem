-- postmortem case database, schema v1 (ADR-002).
-- Enum columns hold small integers documented next to each column.
-- Times are Unix seconds plus a nanosecond fraction column.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE examiners (
    id           INTEGER PRIMARY KEY,
    login        TEXT NOT NULL UNIQUE,
    display_name TEXT
);

CREATE TABLE hosts (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE data_sources (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL,
    kind         INTEGER NOT NULL,          -- 0 disk image, 1 local disk, 2 logical files, 3 unallocated-space image
    host_id      INTEGER REFERENCES hosts(id),
    paths        TEXT NOT NULL,             -- JSON array of segment paths
    size         INTEGER NOT NULL,
    sector_size  INTEGER NOT NULL,
    timezone     TEXT NOT NULL,             -- IANA zone used for local-time file systems
    image_format TEXT,
    md5          TEXT,
    sha1         TEXT,
    sha256       TEXT,
    acquisition  TEXT,                      -- JSON object of container metadata
    device_id    TEXT,
    added_at     INTEGER NOT NULL
);

CREATE TABLE volume_systems (
    id          INTEGER PRIMARY KEY,
    ds_id       INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    kind        INTEGER NOT NULL,           -- 0 DOS, 1 GPT
    sector_size INTEGER NOT NULL,
    disk_guid   TEXT
);

CREATE TABLE volumes (
    id           INTEGER PRIMARY KEY,
    vs_id        INTEGER NOT NULL REFERENCES volume_systems(id) ON DELETE CASCADE,
    addr         INTEGER NOT NULL,
    kind         INTEGER NOT NULL,          -- 0 allocated, 1 unallocated, 2 meta
    start_sector INTEGER NOT NULL,
    sector_count INTEGER NOT NULL,
    description  TEXT NOT NULL,
    name         TEXT,
    flags        INTEGER NOT NULL
);

CREATE TABLE file_systems (
    id          INTEGER PRIMARY KEY,
    ds_id       INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    volume_id   INTEGER REFERENCES volumes(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,              -- display name, e.g. "NTFS"
    byte_offset INTEGER NOT NULL,
    block_size  INTEGER NOT NULL,
    block_count INTEGER NOT NULL,
    root_inode  INTEGER NOT NULL,
    label       TEXT,
    serial      TEXT
);

CREATE TABLE files (
    id          INTEGER PRIMARY KEY,
    ds_id       INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    fs_id       INTEGER REFERENCES file_systems(id) ON DELETE CASCADE,
    parent_id   INTEGER REFERENCES files(id) ON DELETE CASCADE,
    origin      INTEGER NOT NULL,           -- 0 fs, 1 carved, 2 derived, 3 local, 4 unalloc blocks, 5 virtual dir, 6 slack
    inode       INTEGER,
    stream      INTEGER,                    -- alternate data stream id (NULL = default stream)
    name        TEXT NOT NULL,
    parent_path TEXT NOT NULL,              -- '/'-terminated
    kind        INTEGER NOT NULL,           -- pm_fs::FileKind code (see FileKindCode)
    name_alloc  INTEGER NOT NULL,
    meta_alloc  INTEGER NOT NULL,
    meta_flags  INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    mtime INTEGER, mtime_ns INTEGER,
    atime INTEGER, atime_ns INTEGER,
    ctime INTEGER, ctime_ns INTEGER,
    crtime INTEGER, crtime_ns INTEGER,
    mode        INTEGER,
    uid         INTEGER,
    gid         INTEGER,
    seq         INTEGER,
    attributes  INTEGER,
    extension   TEXT NOT NULL,              -- lower case, without dot
    mime        TEXT,
    md5         BLOB,
    sha1        BLOB,
    sha256      BLOB,
    known       INTEGER NOT NULL DEFAULT 0, -- 0 unknown, 1 known, 2 notable
    derived_from INTEGER REFERENCES files(id) ON DELETE CASCADE,
    local_path  TEXT,
    layout      TEXT                        -- JSON ranges for carved/unallocated files
);
CREATE INDEX files_parent  ON files(parent_id, name);
CREATE INDEX files_ds_kind ON files(ds_id, kind);
CREATE INDEX files_ext     ON files(extension, ds_id) WHERE kind = 1;
CREATE INDEX files_mime    ON files(mime, ds_id) WHERE mime IS NOT NULL;
CREATE INDEX files_md5     ON files(md5) WHERE md5 IS NOT NULL;
CREATE INDEX files_mtime   ON files(mtime);
CREATE INDEX files_size    ON files(size) WHERE kind = 1;
CREATE INDEX files_deleted ON files(ds_id) WHERE name_alloc = 0 OR meta_alloc = 0;
CREATE INDEX files_inode   ON files(fs_id, inode);

CREATE TABLE artifact_types (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    category     INTEGER NOT NULL,          -- 0 data artifact, 1 analysis result
    custom       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE attribute_types (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    value_type   INTEGER NOT NULL,          -- 0 string, 1 integer, 2 long, 3 double, 4 bytes, 5 datetime, 6 json
    custom       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE artifacts (
    id            INTEGER PRIMARY KEY,
    type_id       INTEGER NOT NULL REFERENCES artifact_types(id),
    ds_id         INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    file_id       INTEGER REFERENCES files(id) ON DELETE CASCADE,
    source_artifact_id INTEGER REFERENCES artifacts(id) ON DELETE CASCADE,
    significance  INTEGER,                  -- analysis results: 0 unknown .. 4 notable (see Significance)
    priority      INTEGER,                  -- 0 normal, 1 override
    conclusion    TEXT,
    configuration TEXT,
    justification TEXT,
    review_status INTEGER NOT NULL DEFAULT 3, -- 1 approved, 2 rejected, 3 undecided
    created_at    INTEGER NOT NULL
);
CREATE INDEX artifacts_type ON artifacts(type_id, ds_id);
CREATE INDEX artifacts_file ON artifacts(file_id);

CREATE TABLE attributes (
    artifact_id  INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    type_id      INTEGER NOT NULL REFERENCES attribute_types(id),
    source       TEXT NOT NULL,             -- producing module
    value_text   TEXT,
    value_int    INTEGER,
    value_double REAL,
    value_blob   BLOB
);
CREATE INDEX attributes_artifact  ON attributes(artifact_id);
CREATE INDEX attributes_type_text ON attributes(type_id, value_text);

CREATE TABLE tag_sets (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

CREATE TABLE tag_names (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    color       TEXT NOT NULL DEFAULT 'NONE',
    notable     INTEGER NOT NULL DEFAULT 0,
    tag_set_id  INTEGER REFERENCES tag_sets(id),
    rank        INTEGER
);

CREATE TABLE file_tags (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    tag_name_id INTEGER NOT NULL REFERENCES tag_names(id),
    comment     TEXT NOT NULL DEFAULT '',
    begin_byte  INTEGER,
    end_byte    INTEGER,
    examiner    TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX file_tags_file ON file_tags(file_id);
CREATE INDEX file_tags_name ON file_tags(tag_name_id);

CREATE TABLE artifact_tags (
    id          INTEGER PRIMARY KEY,
    artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    tag_name_id INTEGER NOT NULL REFERENCES tag_names(id),
    comment     TEXT NOT NULL DEFAULT '',
    examiner    TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX artifact_tags_artifact ON artifact_tags(artifact_id);

CREATE TABLE ingest_jobs (
    id         INTEGER PRIMARY KEY,
    ds_id      INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER,
    status     INTEGER NOT NULL,            -- 0 started, 1 cancelled, 2 completed
    settings   TEXT NOT NULL                -- JSON
);

CREATE TABLE ingest_job_modules (
    job_id  INTEGER NOT NULL REFERENCES ingest_jobs(id) ON DELETE CASCADE,
    module  TEXT NOT NULL,
    version TEXT NOT NULL
);

CREATE TABLE reports (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL,
    module       TEXT NOT NULL,
    display_name TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

-- Autopsy's default tag names.
INSERT INTO tag_names (name, description, color, notable) VALUES
    ('Bookmark', 'Default tag for marking content', 'NONE', 0),
    ('Follow Up', 'Default tag for marking content for follow up', 'NONE', 0),
    ('Notable Item', 'Default tag for marking content as notable', 'RED', 1);
