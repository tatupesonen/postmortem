-- Schema v3: the timeline reads file-system events from the time columns
-- of `files` (ADR-017); every column needs an index for range counts.
CREATE INDEX IF NOT EXISTS files_atime  ON files(atime)  WHERE atime IS NOT NULL;
CREATE INDEX IF NOT EXISTS files_ctime  ON files(ctime)  WHERE ctime IS NOT NULL;
CREATE INDEX IF NOT EXISTS files_crtime ON files(crtime) WHERE crtime IS NOT NULL;
