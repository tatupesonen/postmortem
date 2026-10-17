# Hashing track: status (paused)

Paused on 2026-10-15 at the coordinator's request (usage limit). Almost no
feature work is done yet. This file records the audit findings and the
exact next steps.

## Where this work lives
The agent's isolated worktree was created in the **Autopsy** repo
(`/home/tatu/slop/autopsy/.claude/worktrees/agent-aecc9bf4627e4b9c9`), not
in postmortem. The isolation guard blocked git commands in the postmortem
checkout, so the work went into a copy of postmortem `master` (`57b8baf`,
the user's uncommitted `assets/brand` changes left out) at
`/private/tmp/claude-501/-Users-tatu-slop/8b2331ac-9e24-4047-81e9-6bcc26d682c1/scratchpad/pm`,
branch `hashing`. Fetch it from there with
`git fetch <that path> hashing`. It is a temp directory, so fetch soon.

## Done
- **Fixed master build break.** `CaseDb::child_named` was defined twice:
  `derived.rs` (exact match) and `windows_artifacts.rs` (case-insensitive),
  left over from the windows-artifacts merge. `pm-casedb` did not compile.
  The case-insensitive one is renamed to `child_named_nocase`, and its
  callers in `pm-casedb/src/windows_artifacts.rs` and
  `pm-modules/src/recent_activity/windows/{mod,recycle,zone}.rs` are
  updated. `cargo test --no-run -p pm-casedb -p pm-modules -p pm-ui`
  compiled. No tests, clippy or fmt were run.

## Parity audit findings (not yet fixed)
Reference: `HashDbIngestModule.java`, `IngestTasksScheduler.shouldEnqueueFileTask`.
1. **Skip rule is too broad.** Autopsy skips only `UNALLOC_BLOCKS`/`SLACK`
   and `isDir()`. Our `hash_lookup.rs` also skips everything that is not
   `FileKindCode::Regular`, so FAT/exFAT virtual files (`$MBR`, `$FAT1`,
   `$FAT2`, `$ALLOC_BITMAP`, `$UPCASE_TABLE`; kind `Virtual`), symlinks,
   `Unknown` (deleted names without metadata), FIFOs and devices are never
   hashed. Fix: skip `is_unallocated_space() || is_dir()` only.
   `Origin::VirtualDir` rows have kind `Directory`, so they stay skipped.
2. **Hash read error handling.** Autopsy posts an error message titled
   `Read Error: {name}` with the details `Error encountered while
   calculating the hash value for {parentPath+name} ({Allocated File|Deleted
   File})`. The state is "Allocated File" when the ALLOC meta flag is set.
   Autopsy then carries on: lookups with a null MD5 find nothing, and the
   result is OK. Our module puts the details text in the title and returns
   `Err`.
3. **Lookup error.** Autopsy's title is `Hash Lookup Error: {name}`. Ours is
   `Hash set lookup error for {path}`.
4. **Summary format.** Autopsy's labels are "Notables found:", "No Change
   items found:" (check the `@Messages` text), "Total Calculation Time",
   "Total Lookup Time" and "Hash Sets Used:", followed by notable and
   no-change set names. It is posted only when some set is configured,
   which we already do. Our labels differ.
5. Already matching:
   - hashes are reused only when both are present;
   - the module bails out when there are no known or notable sets and
     calculation is off (no-change sets do not count);
   - "calculate hashes" defaults to on;
   - zero-length files get the empty-input hashes;
   - the ingest scheduler's filter matches (`.`/`..`, NTFS `$X:stream` in
     the root with inode < 32).

## Next steps (in order)
1. Hash module:
   - fix the skip rule (finding 1) and the messages (findings 2–4);
   - add SHA-1 in the same pass (`IngestFile::set_sha1` exists; the
     `files.sha1` column exists);
   - reuse existing hashes only when all three are present.
2. Tests in `crates/pm-modules/tests/`:
   - on `pm_testkit::demo::demo_disk()`, every eligible row (deleted,
     ADS, orphans, carved/derived, virtual, zero-length) has correct
     MD5/SHA-1/SHA-256, checked against an independent hash of
     `case.content()`;
   - unalloc, slack and directories have no hashes;
   - a read error produces the message text above.
3. `pm-casedb`:
   - migration `0002_hash_indexes.sql` with partial indexes
     `files_sha1`/`files_sha256` (`WHERE … IS NOT NULL`);
   - append it to `MIGRATIONS`, bump `SCHEMA_VERSION` to 2, and add both
     to `FILE_INDEXES`;
   - test v1 → v2 migration and `EXPLAIN QUERY PLAN`.
   - **Note:** PARITY.md WP-0 also plans a migration `0002`, so it must
     become `0003`.
   - `FileQuery.sha1/sha256`, plus a helper (new file
     `pm-casedb/src/hashes.rs`) that parses hex hashes and picks the column
     by length (32/40/64), with an `IN` lookup for many hashes.
4. UI (`pm-ui`; keep `app.rs` hooks minimal, because the search agent also
   edits it):
   - MD5/SHA-1/SHA-256 lines in `viewers/metadata.rs` ("Not calculated"
     when absent) and the known status;
   - MD5/SHA-1/SHA-256 and Known columns in `table.rs`. `FileRow` needs
     hash fields, filled in the listing's background task by one query per
     listing on `files(fs_id, inode)`. `views.rs::row_of` carries them
     from `FileRecord`;
   - refresh listings when analysis finishes (`was_analysing`);
   - a "Search by Hash" Tools entry;
   - kittest tests.
5. An `#[ignore]` throughput benchmark of MD5+SHA-1+SHA-256 over
   `ZeroSource`. Check the aarch64 hardware acceleration of `sha1`/`sha2`
   and try larger read buffers. Record the results in `docs/PERF.md`.

## Untested
Everything above "Done" except that the three crates compile.


## Update (coordinator, 2026-10-15)
Done in `pm-modules/src/hash_lookup.rs` (commit after this note):
- Skip rules now match Autopsy: only unallocated blocks/slack and
  directories are skipped (virtual files, symlinks, devices, deleted names
  without metadata are hashed).
- MD5 + **SHA-1** + SHA-256 in one pass; existing hashes reused only when
  all three are present.
- Autopsy's message texts: `Read Error: {name}` (details "Error encountered
  while calculating the hash value for … (Allocated File|Deleted File)."),
  processing continues; `Hash Lookup Error: {name}` per failing set with the
  per-kind text; summary labels Notables found / Total Calculation Time /
  Total Lookup Time / Hash Sets Used.
- Test: every eligible demo file's MD5/SHA-1/SHA-256 equals an independent
  computation; FAT virtual files hashed; directories not.
Still open: hash indexes migration (0002_hash_indexes), FileQuery
sha1/sha256 + any-hash helper, UI (table columns, metadata viewer,
Search by Hash), throughput benchmark.
