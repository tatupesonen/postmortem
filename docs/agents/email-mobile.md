# Track: email-mobile (email, communication accounts, mobile & specialised analysers)

> **Historical spec** from the parallel-agent period (2026-10-17). The work it describes is merged or folded into other crates; current status is in `docs/PARITY.md`, current direction in `docs/DIRECTION.md`.

Status: **paused at the coordinator's request** after items 1 (email +
accounts) and 2 (Android Analyzer). Items 3 (GPX, DJI, YARA, VM extractor)
are not started.

## Verification state (WIP commit)
The full gate (`scripts/gate.sh`) was **not run to completion** (stopped
at the coordinator's request). What was verified: `cargo fmt --all`;
`cargo clippy -p pm-email -p pm-casedb -p pm-modules --all-targets
--all-features -- -D warnings` clean; `cargo test` for pm-casedb, pm-email
(unit, `pst`, `never_panic`) and pm-modules (lib, `email_parser`,
`android`) all passing. **Unverified:** the workspace-wide clippy/test run
(other crates, e.g. pm-ui, were not rebuilt against the new `Cargo.toml`
entries), and the fuzz crate (`fuzz/`) build with the three new targets.

## Done

### Communications model — `crates/pm-casedb/src/communications.rs`
Autopsy's Communications Manager without a schema migration:
* **Account instance** = `TSK_ACCOUNT` artifact on the source file with
  `TSK_ACCOUNT_TYPE`, `TSK_ID` (normalised id) and custom attribute
  `PM_ACCOUNT` = `TYPE:id` (indexed key).
* **Relationship source** = each `TSK_EMAIL_MSG` / `TSK_MESSAGE` /
  `TSK_CALLLOG` / `TSK_CONTACT` carries `PM_ACCOUNT_SENDER` and
  `PM_ACCOUNT_RECIPIENT` keys; as in TSK `addRelationships`, every pair of
  participants is related; type from the artifact type, date from its
  date/time attributes.
* Query helpers (all through the `attributes(type_id, value_text)` index,
  keyset paged): `accounts`, `account_type_counts`, `account_instances`,
  `communications_of`, `communication`, `related_accounts`,
  `communications_between`; `normalize_phone/email/account_id` (TSK
  `CommunicationsUtils`); `ACCOUNT_TYPES` (TSK 4.15 predefined types,
  verified with `javap` on the sleuthkit-4.15.0 jar, plus `WWF`);
  `device_account_id`; `resolve_path` (TSK `FileAttachment` lookup with
  Android `/storage/emulated/N` ↔ `/media/N` aliases, indexed per path
  component).
* Custom attribute types are registered on demand with
  `CaseDb::attribute_type` (no migration). **Coordinator:** if the UI wants
  stable ids, these three could move into the catalogue.

### `crates/pm-email` (new)
* `mbox`: streaming splitter over `ReadAt` (1 MiB windows, LF and CRLF, no
  temp copy, no 1000 MiB threshold); `looks_like_mbox`; Thunderbird
  `TSK_PATH` (`/Mail/`, `/ImapMail/`, `.sbd`, literal not regex).
* `mime`: `mail-parser` 0.11.9 (adopted per ADR-009: lenient, RFC
  2045–2049/2047/2231, encoding_rs charsets) mapped like
  `MimeJ4MessageParser` (first text/plain + first text/html bodies, every
  other leaf an attachment, nested `message/rfc822` parsed recursively up
  to depth 8); true top-level header block; `Received:`-based receive time;
  proper `<id>` tokeniser; `attachment_content` re-extracts a part.
* `pst`: **in-house PST/OST reader** — NDB (header, NBT/BBT B-trees, blocks,
  XBLOCK/XXBLOCK, SLBLOCK/SIBLOCK, bounded caches), LTP (heap-on-node, BTH,
  PC, TC with row matrices in subnodes), messaging (folder hierarchy with
  Autopsy's `\\Top of Personal Folders\Inbox` paths, contents tables,
  recipient table with SMTP addresses, attachments incl. embedded messages,
  LZFu compressed RTF, PR_HTML code pages), ANSI + Unicode, permutative and
  cyclic encoding (cyclic is decoded, not reported as encrypted). Falls back
  to the store's IPM subtree when the root folder node is missing and to
  type-matched recipient/attachment table subnodes. OST 2013 (4 KiB pages)
  → `Unsupported`.
* `vcard`: 2.1/3.0/4.0 (folding, groups, bare 2.1 types, QP + CHARSET,
  base64 / data-URI photos, BOM/UTF-16 tolerant detection).
* `thread`: deterministic JWZ threading on 64-bit key hashes, iterative (no
  stack overflow on 50k-message chains), stable UUID-formatted thread ids,
  `Re/Fwd/Fw/AW/SV/VS/Antw/WG/TR`, `Re[n]:`, `[list]` subject prefixes.
* `address`: Autopsy's address regex (TLDs > 4 letters allowed).

### Email Parser module — `crates/pm-modules/src/email.rs`
File module `email_parser` ("Email Parser"), in `core_modules()` after the
Picture Analyzer. Autopsy detection order and gates (known/unalloc/size
> 64, vCard > 22); MBOX/PST two passes (header-only threading pass for
MBOX); `TSK_EMAIL_MSG` attributes in Autopsy order + `TSK_EMAIL_BCC`,
`TSK_MSG_REPLY_ID`, Message-ID in `TSK_MSG_ID` for MIME, `TSK_ATTACHMENTS`
JSON; `EMAIL_HAS_ATTACHMENT`; sender account only for exactly one address;
attached messages as extra artifacts; vCard `TSK_CONTACT` with
`TSK_PHONE_NUMBER_<TYPE>` / `TSK_EMAIL_<TYPE>` custom types and DEVICE→
contact relationships; unknown PST encryption → `TSK_ENCRYPTION_DETECTED`
("File-level Encryption"); idempotent re-runs.

### Communication helpers — `crates/pm-modules/src/comms.rs`
`CommunicationArtifactsHelper` (addContact/addMessage/addCalllog with TSK
4.15 semantics checked by disassembling the jar: from/to always go to
`TSK_PHONE_NUMBER_FROM/_TO` (or `TSK_PHONE_NUMBER` for unknown direction),
self account fills the missing side, TSK's argument checks),
`MessageAttachments` / `GeoTrackPoints` / `GeoWaypoints` JSON exactly as
Gson writes them, `addTrack` / `addRoute`.

### Android Analyzer — `crates/pm-modules/src/android/`
Data-source module `android_analyzer`, all **22** Autopsy parsers in
Autopsy's order: contacts, call log, SMS, Tango, Words With Friends, Google
Maps, browser location, cache location (binary), IMO, Xender, Zapya,
SHAREit, LINE, WhatsApp, TextNow, Skype, Viber, Facebook Messenger,
Samsung Internet, Opera, OruxMaps, Google Play library. Autopsy's SQL,
parser/module names and account types; one pass over the file table finds
every candidate DB; DBs copied with `-wal`/`-journal`; `ATTACH` for LINE /
WhatsApp. Fixes listed in the module docs (self-account `next()`, call-log
end time, TextNow loop abort, FB leaks, browser downloads/addresses that
never worked in Autopsy, AOSP `calllog.db` per SQLECmd
`Android_Calls.smap`). Idempotent per DB and parser.

## Tests
* pm-casedb: 5 unit + 3 integration (`tests/communications.rs`).
* pm-email: 36 unit; `tests/pst.rs` 5 (Unicode PSTs written by the
  independent `outlook-pst-rw` 1.2.4 dev-dependency, XBLOCK data trees,
  320 KiB attachment, re-encoded with permutative and cyclic encoding,
  truncation and 200 random corruptions); `tests/never_panic.rs` 6
  proptests (MIME, MBOX, vCard, LZFu, PST, threading).
* LZFu verified with the two MS-OXRTFCP spec vectors; permutation tables
  verified to be permutations and inverse.
* pm-modules: unit tests for detection, helpers, JSON shapes, location
  cache, Google geo strings, Tango/Facebook payloads; end-to-end
  `tests/email_parser.rs` (MBOX/EML/PST/vCard on an NTFS image: 7 messages,
  threads, accounts, relationships, re-run) and `tests/android.rs` (WhatsApp,
  SMS, contacts, AOSP call log, Google Maps, OruxMaps, Play library,
  Samsung Internet; accounts/relationships; re-run). Shared image builder
  `tests/support/mod.rs`.
* Fuzz targets added: `pst_read`, `mime_parse` (MBOX + MIME + threading),
  `vcard_parse` (not yet smoke-run).
* Autopsy has no automated tests for thunderbirdparser/Android; nothing to
  port. CentralRepoAccountsTest normalisation cases are ported in
  `communications.rs` unit tests.

## ADR-009 evaluations (for the coordinator to fold into DECISIONS.md)
* `mail-parser` 0.11.9 → **adopted** (MIME/EML).
* `outlook-pst` 1.2.0 → **not adopted**: opens files by path only (needs a
  temp copy), `unwrap`/indexing on input, `Rc` object graph. In-house reader
  instead; `outlook-pst-rw` =1.2.4 (MIT fork with a Unicode PST writer) is a
  **dev-only fixture generator**.
* `calcard` → not adopted (vCard needs are small; in-house lenient parser).
* `compressed-rtf` → not adopted (LZFu is ~60 lines; spec vectors tested).

## Known gaps vs Autopsy / follow-ups
* **Attachments are not extracted as derived files** (the derived-file API
  `JobContext::add_derived_files` is not on master). Metadata is in
  `TSK_ATTACHMENTS`; `AttachmentRef` (MIME part path / PST subnode path)
  plus `mime::attachment_content` / `Pst::attachment_content` are ready for
  the follow-up. vCard photos likewise not extracted.
* PST: ANSI path implemented but only unit-tested via header parsing (no
  ANSI fixture writer); OST 2013 4 KiB pages unsupported; IPM.Contact /
  Appointment items are emitted as e-mails (Autopsy parity) — mapping to
  `TSK_CONTACT`/`TSK_CALENDAR_ENTRY` needs named-property support.
* Threading is per container (Autopsy parity); case-wide threading is a
  possible post-pass.
* `.msg`, `.emlx`, Maildir not parsed (Autopsy doesn't either).
* Android: newer WhatsApp schema (`message` table, 2021+) not parsed
  (Autopsy neither); cache-location keys (BSSID/cell id) discarded as in
  Autopsy.
* Contact relationships relate all of a contact's accounts with each
  other (pairwise model) whereas TSK relates each only to the device.
* Not started: GPX Parser, DJI Drone Analyzer, YARA Analyzer (yara-x),
  Virtual Machine Extractor, fuzz smoke runs, benchmarks / PERF.md numbers
  for MBOX/PST throughput.
