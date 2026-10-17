-- postmortem case database, schema v2 (WP-0): hosts/persons, OS accounts,
-- communication accounts, timeline events, aggregate scores, file
-- attributes and hash lookup indexes. Table and enum semantics follow The
-- Autopsy's case database (CaseDatabaseFactory, Autopsy 4.12); ids of files,
-- artifacts and OS accounts live in separate tables here, so references
-- that Autopsy makes through its shared object table name the table explicitly.

-- Hash lookups by SHA-1/SHA-256 (MD5 is indexed since v1).
CREATE INDEX files_sha1   ON files(sha1)   WHERE sha1 IS NOT NULL;
CREATE INDEX files_sha256 ON files(sha256) WHERE sha256 IS NOT NULL;

-- Significance codes now sort like Autopsy's Score.Significance ordinals:
-- 0 unknown, 1 likely none, 2 likely notable, 3 none, 4 notable
-- (v1 had 2 = none, 3 = likely notable).
UPDATE artifacts SET significance = 5 - significance WHERE significance IN (2, 3);

-- ------------------------------------------------------------- persons/hosts

CREATE TABLE persons (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

ALTER TABLE hosts ADD COLUMN person_id   INTEGER REFERENCES persons(id) ON DELETE SET NULL;
ALTER TABLE hosts ADD COLUMN status      INTEGER NOT NULL DEFAULT 0;   -- 0 active, 1 merged, 2 deleted
ALTER TABLE hosts ADD COLUMN merged_into INTEGER REFERENCES hosts(id);
CREATE INDEX data_sources_host ON data_sources(host_id);

CREATE TABLE host_addresses (
    id      INTEGER PRIMARY KEY,
    kind    INTEGER NOT NULL,               -- 0 DNS auto, 1 host name, 2 IPv4, 3 IPv6, 4 Ethernet MAC, 5 Wi-Fi MAC, 6 Bluetooth MAC
    address TEXT NOT NULL,
    UNIQUE (kind, address)
);

CREATE TABLE host_address_map (
    id             INTEGER PRIMARY KEY,
    host_id        INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    address_id     INTEGER NOT NULL REFERENCES host_addresses(id),
    source_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    source_artifact_id INTEGER REFERENCES artifacts(id) ON DELETE SET NULL,
    time           INTEGER,
    UNIQUE (host_id, address_id, time)
);

CREATE TABLE host_address_dns_ip_map (
    id             INTEGER PRIMARY KEY,
    dns_address_id INTEGER NOT NULL REFERENCES host_addresses(id) ON DELETE CASCADE,
    ip_address_id  INTEGER NOT NULL REFERENCES host_addresses(id) ON DELETE CASCADE,
    source_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    source_artifact_id INTEGER REFERENCES artifacts(id) ON DELETE SET NULL,
    time           INTEGER,
    UNIQUE (dns_address_id, ip_address_id, time)
);

CREATE TABLE host_address_usage (
    id          INTEGER PRIMARY KEY,
    address_id  INTEGER NOT NULL REFERENCES host_addresses(id) ON DELETE CASCADE,
    file_id     INTEGER REFERENCES files(id) ON DELETE CASCADE,
    artifact_id INTEGER REFERENCES artifacts(id) ON DELETE CASCADE,
    ds_id       INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    UNIQUE (address_id, file_id, artifact_id)
);

-- --------------------------------------------------------------- OS accounts

CREATE TABLE os_account_realms (
    id               INTEGER PRIMARY KEY,
    name             TEXT,                  -- may be NULL for an address-only realm
    addr             TEXT,                  -- SID prefix / other identifier, may be NULL
    signature        TEXT NOT NULL UNIQUE,  -- "<addr or name>_<scope host id | DOMAIN>"
    scope_host_id    INTEGER REFERENCES hosts(id) ON DELETE CASCADE,
    scope_confidence INTEGER NOT NULL,      -- 0 known, 1 inferred
    status           INTEGER NOT NULL DEFAULT 0, -- 0 active, 1 merged, 2 deleted
    merged_into      INTEGER REFERENCES os_account_realms(id)
);
CREATE INDEX os_account_realms_addr ON os_account_realms(addr COLLATE NOCASE) WHERE addr IS NOT NULL;
CREATE INDEX os_account_realms_name ON os_account_realms(name COLLATE NOCASE) WHERE name IS NOT NULL;
CREATE INDEX os_account_realms_host ON os_account_realms(scope_host_id);

CREATE TABLE os_accounts (
    id          INTEGER PRIMARY KEY,
    realm_id    INTEGER NOT NULL REFERENCES os_account_realms(id) ON DELETE CASCADE,
    login_name  TEXT,
    full_name   TEXT,
    addr        TEXT,                       -- SID / UID
    signature   TEXT NOT NULL,              -- addr, else login name
    status      INTEGER,                    -- 0 unknown, 1 active, 2 disabled, 3 deleted, 4 non-existent
    kind        INTEGER,                    -- 0 unknown, 1 service, 2 interactive
    admin       INTEGER,                    -- 0/1, NULL unknown
    created_at  INTEGER,
    db_status   INTEGER NOT NULL DEFAULT 0, -- 0 active, 1 merged, 2 deleted
    merged_into INTEGER REFERENCES os_accounts(id),
    UNIQUE (signature, realm_id)
);
CREATE INDEX os_accounts_addr  ON os_accounts(addr COLLATE NOCASE) WHERE addr IS NOT NULL;
CREATE INDEX os_accounts_login ON os_accounts(realm_id, login_name COLLATE NOCASE);

CREATE TABLE os_account_attributes (
    id             INTEGER PRIMARY KEY,
    os_account_id  INTEGER NOT NULL REFERENCES os_accounts(id) ON DELETE CASCADE,
    host_id        INTEGER REFERENCES hosts(id) ON DELETE CASCADE,
    source_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    type_id        INTEGER NOT NULL REFERENCES attribute_types(id),
    value_text     TEXT,
    value_int      INTEGER,
    value_double   REAL,
    value_blob     BLOB
);
CREATE INDEX os_account_attributes_account ON os_account_attributes(os_account_id);

CREATE TABLE os_account_instances (
    id            INTEGER PRIMARY KEY,
    os_account_id INTEGER NOT NULL REFERENCES os_accounts(id) ON DELETE CASCADE,
    ds_id         INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    kind          INTEGER NOT NULL,         -- 0 launched, 1 accessed, 2 referenced
    UNIQUE (os_account_id, ds_id, kind)
);
CREATE INDEX os_account_instances_ds ON os_account_instances(ds_id);

-- The account a data artifact belongs to and a
-- file's owner (owner_uid / os_account_obj_id).
ALTER TABLE artifacts ADD COLUMN os_account_id INTEGER REFERENCES os_accounts(id) ON DELETE SET NULL;
ALTER TABLE files ADD COLUMN owner_uid TEXT;
ALTER TABLE files ADD COLUMN os_account_id INTEGER REFERENCES os_accounts(id) ON DELETE SET NULL;
CREATE INDEX artifacts_os_account ON artifacts(os_account_id) WHERE os_account_id IS NOT NULL;
CREATE INDEX files_os_account ON files(os_account_id) WHERE os_account_id IS NOT NULL;
CREATE INDEX artifacts_source ON artifacts(source_artifact_id) WHERE source_artifact_id IS NOT NULL;

-- ------------------------------------------------- communication accounts

CREATE TABLE account_types (
    id           INTEGER PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL
);

CREATE TABLE accounts (
    id         INTEGER PRIMARY KEY,
    type_id    INTEGER NOT NULL REFERENCES account_types(id),
    identifier TEXT NOT NULL,               -- normalised (CommunicationsUtils)
    UNIQUE (type_id, identifier)
);

CREATE TABLE account_relationships (
    id                 INTEGER PRIMARY KEY,
    account1_id        INTEGER NOT NULL REFERENCES accounts(id),
    account2_id        INTEGER NOT NULL REFERENCES accounts(id),
    source_artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    date_time          INTEGER,
    kind               INTEGER NOT NULL,    -- 1 message, 2 call log, 3 contact
    ds_id              INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    UNIQUE (account1_id, account2_id, source_artifact_id)
);
CREATE INDEX account_relationships_a1 ON account_relationships(account1_id, date_time);
CREATE INDEX account_relationships_a2 ON account_relationships(account2_id, date_time);
CREATE INDEX account_relationships_source ON account_relationships(source_artifact_id);

INSERT INTO account_types (id, name, display_name) VALUES
    (1, 'CREDIT_CARD', 'Credit Card'),
    (2, 'DEVICE', 'Device'),
    (3, 'PHONE', 'Phone'),
    (4, 'EMAIL', 'Email'),
    (5, 'FACEBOOK', 'Facebook'),
    (6, 'TWITTER', 'Twitter'),
    (7, 'INSTAGRAM', 'Instagram'),
    (8, 'WHATSAPP', 'WhatsApp'),
    (9, 'MESSAGING_APP', 'MessagingApp'),
    (10, 'WEBSITE', 'Website'),
    (11, 'IMO', 'IMO'),
    (12, 'LINE', 'LINE'),
    (13, 'SKYPE', 'Skype'),
    (14, 'TANGO', 'Tango'),
    (15, 'TEXTNOW', 'TextNow'),
    (16, 'THREEMA', 'ThreeMa'),
    (17, 'VIBER', 'Viber'),
    (18, 'XENDER', 'Xender'),
    (19, 'ZAPYA', 'Zapya'),
    (20, 'SHAREIT', 'ShareIt');

-- ---------------------------------------------------------- timeline events

CREATE TABLE event_types (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,     -- Autopsy display name
    super_type_id INTEGER REFERENCES event_types(id)
);

CREATE TABLE event_descriptions (
    id                INTEGER PRIMARY KEY,
    full_description  TEXT NOT NULL,
    med_description   TEXT,
    short_description TEXT,
    ds_id             INTEGER NOT NULL REFERENCES data_sources(id) ON DELETE CASCADE,
    file_id           INTEGER REFERENCES files(id) ON DELETE CASCADE,       -- content the event is about
    artifact_id       INTEGER REFERENCES artifacts(id) ON DELETE CASCADE,   -- NULL for file-system events
    hash_hit          INTEGER NOT NULL DEFAULT 0,
    tagged            INTEGER NOT NULL DEFAULT 0,
    UNIQUE (full_description, file_id, artifact_id)
);
CREATE INDEX event_descriptions_file     ON event_descriptions(file_id);
CREATE INDEX event_descriptions_artifact ON event_descriptions(artifact_id) WHERE artifact_id IS NOT NULL;
CREATE INDEX event_descriptions_ds       ON event_descriptions(ds_id);

CREATE TABLE events (
    id             INTEGER PRIMARY KEY,
    type_id        INTEGER NOT NULL REFERENCES event_types(id),
    description_id INTEGER NOT NULL REFERENCES event_descriptions(id) ON DELETE CASCADE,
    time           INTEGER NOT NULL,        -- Unix seconds, UTC
    UNIQUE (type_id, description_id, time)
);
CREATE INDEX events_time ON events(time, type_id);
CREATE INDEX events_description ON events(description_id);

INSERT INTO event_types (id, name, super_type_id) VALUES
    (0, 'Event Types', NULL),
    (1, 'File System', 0),
    (2, 'Web Activity', 0),
    (3, 'Other', 0),
    (4, 'File Modified', 1),
    (5, 'File Accessed', 1),
    (6, 'File Created', 1),
    (7, 'File Changed', 1),
    (8, 'Web Downloads', 2),
    (9, 'Web Cookies Create', 2),
    (10, 'Web Bookmarks', 2),
    (11, 'Web History Accessed', 2),
    (12, 'Web Searches', 2),
    (13, 'Messages', 3),
    (14, 'GPS Routes', 3),
    (15, 'GPS Trackpoint', 3),
    (16, 'Call Begin', 3),
    (17, 'Email Sent', 3),
    (18, 'Recent Documents', 3),
    (19, 'Program Installed', 3),
    (20, 'Exif', 3),
    (21, 'Devices Attached', 3),
    (23, 'Standard Artifact Event', 3),
    (24, 'Log Entry', 3),
    (25, 'Registry', 3),
    (26, 'Custom Artifact Event', 3),
    (27, 'Web Form Autofill Created', 2),
    (28, 'Web Form Address Created', 2),
    (29, 'GPS Bookmark', 3),
    (30, 'GPS Last Known Location', 3),
    (31, 'GPS Search', 3),
    (32, 'GPS Track', 3),
    (33, 'Document Last Printed', 3),
    (34, 'Document Last Saved', 3),
    (35, 'Document Created', 3),
    (36, 'Program Run', 3),
    (37, 'Web Form Autofill Accessed', 2),
    (38, 'Call End', 3),
    (39, 'Email Received', 3),
    (40, 'Web Form Address Modified', 2),
    (41, 'Web Cookies Accessed', 2),
    (42, 'Web Cookies End', 2),
    (43, 'Backup Begin', 3),
    (44, 'Backup End', 3),
    (45, 'Bluetooth Pairing', 3),
    (46, 'Calendar Entry Begin', 3),
    (47, 'Calendar Entry End', 3),
    (48, 'Program Deleted', 3),
    (49, 'Operating System Information', 3),
    (50, 'Program Notification', 3),
    (51, 'Screen Shot', 3),
    (52, 'Service Account', 3),
    (53, 'User Activity Begin', 3),
    (54, 'User Activity End', 3),
    (55, 'Web Cache', 2),
    (56, 'Wifi Network', 3),
    (57, 'Web History Created', 2),
    (58, 'Bluetooth Adapter', 3),
    (59, 'Bluetooth Pairing Last Connection', 3),
    (60, 'Manually Created Event', 3);

-- ---------------------------------------------------------- aggregate scores

-- One row per scored object (Autopsy's aggregate score): the highest score of
-- its analysis results and notable tags, priority first.
CREATE TABLE scores (
    obj_kind     INTEGER NOT NULL,          -- 0 file, 1 artifact, 2 OS account
    obj_id       INTEGER NOT NULL,
    ds_id        INTEGER REFERENCES data_sources(id) ON DELETE CASCADE,
    significance INTEGER NOT NULL,
    priority     INTEGER NOT NULL,          -- 0 normal, 1 override
    PRIMARY KEY (obj_kind, obj_id)
) WITHOUT ROWID;
CREATE INDEX scores_significance ON scores(significance, obj_kind, ds_id);

-- Artifact tags carry the examiner like file tags; tag names gain Autopsy's
-- known status semantics through `notable`.
CREATE INDEX artifact_tags_name ON artifact_tags(tag_name_id);

-- ---------------------------------------------------------- file attributes

CREATE TABLE file_attributes (
    id           INTEGER PRIMARY KEY,
    file_id      INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    type_id      INTEGER NOT NULL REFERENCES attribute_types(id),
    value_text   TEXT,
    value_int    INTEGER,
    value_double REAL,
    value_blob   BLOB
);
CREATE INDEX file_attributes_file ON file_attributes(file_id);

-- Backfill aggregate scores of v1 cases: the best analysis result per
-- object (priority first, then significance), then notable tags.
INSERT INTO scores (obj_kind, obj_id, ds_id, significance, priority)
SELECT kind, obj, MIN(ds_id), MAX(rank) % 10, MAX(rank) / 10
FROM (
    SELECT CASE WHEN source_artifact_id IS NULL THEN 0 ELSE 1 END AS kind,
           COALESCE(source_artifact_id, file_id) AS obj,
           ds_id,
           COALESCE(priority, 0) * 10 + significance AS rank
    FROM artifacts
    WHERE significance IS NOT NULL AND COALESCE(source_artifact_id, file_id) IS NOT NULL
)
GROUP BY kind, obj;

INSERT INTO scores (obj_kind, obj_id, ds_id, significance, priority)
SELECT DISTINCT 0, ft.file_id, f.ds_id, 4, 0
FROM file_tags ft JOIN tag_names tn ON tn.id = ft.tag_name_id JOIN files f ON f.id = ft.file_id
WHERE tn.notable = 1
ON CONFLICT (obj_kind, obj_id) DO UPDATE SET significance = 4
WHERE scores.priority = 0 AND scores.significance < 4;

INSERT INTO scores (obj_kind, obj_id, ds_id, significance, priority)
SELECT DISTINCT 1, at.artifact_id, a.ds_id, 4, 0
FROM artifact_tags at JOIN tag_names tn ON tn.id = at.tag_name_id JOIN artifacts a ON a.id = at.artifact_id
WHERE tn.notable = 1
ON CONFLICT (obj_kind, obj_id) DO UPDATE SET significance = 4
WHERE scores.priority = 0 AND scores.significance < 4;
