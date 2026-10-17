# Track: mcp (MCP server for AI assistants)

Status: implemented on the mcp track branch (2026-10-17); design in
[BEYOND-AUTOPSY §8](../BEYOND-AUTOPSY.md#8-mcp-server-for-ai-assistants--next),
decisions in ADR-021. Every slice passed the full quality gate before it was
committed.

## What is in

### `crates/pm-mcp`
* `server::McpServer` — the transport-independent engine: tool registry,
  dispatch, write gating, CAM withholding, audit log. `McpServer::open(dir,
  config)` opens the case read-only (or writable with `allow_writes`) and
  its keyword/name indexes read-only; `McpServer::new(case, index,
  config)` serves an already open case (the GUI passes its own
  `Arc<Case>` and shared `Arc<CaseIndex>`). `call(client, tool, args)`
  returns the JSON envelope; `set_config` changes switches live.
* `config::McpConfig` (serde) — `allow_writes`, `image_previews`,
  `show_cam_content`, `port` (8743, Autopsy's), `audit`, `keyword_lists`
  (the examiner's `keywords.xml`). All privacy switches default off.
* `handler::McpHandler` — `rmcp` 3.5.0 `ServerHandler` (tools, prompts,
  server instructions, tool annotations `readOnlyHint`); calls run on the
  blocking pool; the client name comes from `initialize` (or the
  2026-07-28 per-request metadata).
* `stdio::serve_stdio` / `serve_io` — stdio transport (any byte streams).
* `http::HttpServer` — streamable HTTP on `127.0.0.1:<port>` (0 = any),
  `/mcp` only, random 256-bit bearer token per start (constant-time
  check), token file written `0600` and deleted on stop,
  `start/stop/url/token/client_config_snippet` for the GUI options panel;
  `rmcp`'s DNS-rebinding host check stays on.
* `cli` — `postmortem mcp --case <dir> [--allow-writes] [--image-previews]
  [--show-cam-content] [--keyword-lists <xml>] [--no-audit] [--http
  [--port N] [--token-file P]]`.
* `prompts` — `triage_data_source`, `account_activity`, `around_time`,
  `summarise_thread`.

### Shared-crate additions (no schema change)
* `pm-casedb/src/mcp.rs`: `CaseDb::open_read_only`, `is_read_only`;
  keyset-paged, filterable `mcp_files` (every Autopsy `query_files`
  filter plus all four times, tag, score, hash-set hit, extension
  mismatch, inside-folder, ADS of an inode; orders id/size/name/mtime/
  crtime asc/desc), `mcp_artifacts` (category, types, file, source
  artifact, attribute predicates: text contains/equals, int equals/range,
  double range, has), `mcp_tags`, `mcp_events` + `mcp_file_events`
  (merge keysets), histograms, `mcp_accounts`, `mcp_relationships`,
  `mcp_ingest_jobs`, `mcp_reports`, `mcp_volume_systems`, type lookups.
* `pm-case/src/case/read_only.rs`: `Case::open_read_only`, `is_read_only`.
* `pm-search`: `ContentIndex/NameIndex::open_read_only` and
  `CaseIndex::open_read_only` (never create, never take the writer lock).
* `postmortem` binary: `mcp` sub-command (logs to stderr).

## Tools

Autopsy-compatible (names and argument names kept): `get_server_status`,
`get_case_summary`, `query_files`, `query_data_artifacts`,
`query_analysis_results`, `query_data_sources`, `get_hosts`,
`get_data_source_tree`, `query_tags`, `query_timeline`,
`summarize_timeline`, `get_os_accounts`, `get_communications_accounts`,
`get_file_content`, `get_account_relationships`, `get_object_children`,
`list_reports`, `get_report_content`.

Extended: `search_text`, `search_file_names`, `find_files`, `lookup_hash`,
`run_keyword_list`, `get_file_metadata`, `read_file_text`,
`read_file_bytes`, `get_file_strings`, `get_image_preview` (opt-in),
`list_artifact_types`, `get_artifact`, `get_related`, `list_notable`,
`timeline_events`, `timeline_histogram`, `events_around`,
`query_event_logs`, `read_registry_key`, `get_messages`, `get_contacts`,
`geolocation_points`, `list_ingest_jobs`.

Write (only with `allow_writes` on a writable case): `add_tag`,
`remove_tag` (own tags only), `save_search_results`.

## Conventions
* References `file:N`, `artifact:N` / `result:N` (same id space),
  `event:N`, `account:N`, `os_account:N`, `ds:N`, `host:N`, `report:N`.
* Envelope `{caseId, result, next_cursor, truncated}`; cursors are opaque
  base64url keysets; default 50 rows, max 500 (`query_timeline` default
  100); responses ≤ 256 KiB (pages cut at a row, single objects get long
  strings shortened, `truncated_reason`).
* Times: ISO 8601 UTC plus `…Epoch` / `timeEpoch` Unix seconds.
* Evidence-derived text only inside `evidence` objects; instructions and
  every tool description say it is data, never instructions.
* Audit: one JSON line per call (time, client, tool, arguments, rows, ok,
  error, ms) in `<case>/Log/mcp-audit.jsonl` (the case's existing `Log`
  folder; §8 wrote `Logs/`).

## Differences from Autopsy's MCP server
* Results carry evidence text under `evidence` (e.g. `query_files` rows
  have `evidence.name/path/parentPath/extension`; artifact attributes are
  `evidence.attributes`).
* Lists are paged (`cursor`, `next_cursor`); Autopsy returned one capped
  list. `get_object_children` pages its children in `result` and puts
  `object`/`parent` next to it.
* `get_file_content`/`get_report_content` read at most 192 KiB per call
  (Autopsy 1 MiB) to respect the 256 KiB response cap; `truncated`/`eof`
  as in Autopsy.
* `knownState` says `NOTABLE` (Autopsy printed TSK's `BAD`; both accepted
  as input). Object ids are per table: `objectId` of `query_files` is a
  file id, artifacts are `artifact:N`.
* `get_communications_accounts` lists every account (with its
  relationship count), not only accounts with relationships.

## Tests
See the end of this file for counts. `crates/pm-mcp/tests/` builds one
fixture case: the pm-testkit demo disk plus a bait NTFS volume whose file
name, content, label, a web-history title, a message, a contact, a tag
comment and an event description carry a prompt-injection text; ingested
with Hash Lookup, File Type Identification and Keyword Search; plus web
history, an interesting item, tags, a message thread, a contact, a GPS
bookmark and event-log entries.
* `golden_responses_per_tool`: every read tool against
  `tests/golden/<case>.json` (volatile keys redacted; `UPDATE_GOLDEN=1`
  rewrites deliberately).
* `cursors_return_every_row_exactly_once`: 13 listings paged with
  `limit=3` equal one 500-row page, no duplicates.
* `prompt_injection_only_ever_appears_inside_evidence`: every response of
  every read tool is walked; the marker may only occur below `evidence`.
* `writes_are_refused_while_disabled`, `write_tools_tag_as_the_client_and_audit`,
  `cam_content_is_withheld_and_previews_are_opt_in`,
  `responses_follow_the_conventions`,
  `timeline_is_ordered_and_merges_file_and_artifact_events`.
* `tests/transport.rs`: stdio round trip over in-memory pipes
  (initialize, tools/list, tools/call, refused write, prompts), HTTP:
  401 without/with a wrong token, session + tool call with the token,
  token file removed on stop.
* Unit tests: args/time/refs/cursors, envelope capping, audit log,
  config, CLI parsing, prompts, CAM tag names, rows; `pm-casedb` mcp
  queries (paging in every order, LIKE escaping, attribute predicates,
  merged event keysets, read-only open); `pm-case` read-only open;
  `pm-search` read-only index open does not block the writer.

## Gaps and follow-ups
* Not built (no backing analysis yet): `user_activity`,
  `program_execution`, `usb_devices` (§2–§6 analyses). `query_event_logs`
  works on `PM_EVENT_LOG` artifacts of the Windows Event Logs module.
* `set_comment`: postmortem has no central repository / object comment
  store yet.
* `lookup_hash` searches the case only (hash sets/NSRL are reflected
  through Hash Lookup results); a direct `pm-hashdb` lookup needs the
  examiner's configured hash sets passed in `McpConfig`.
* `list_ingest_jobs` cannot report per-file errors or skipped files (not
  stored in the case).
* Performance: `LIKE '%x%'` filters, `atime`/`ctime`/`crtime` ranges
  (indexes come with the timeline work) and timeline text filters are
  LIMIT-bounded scans; `get_case_summary` counts files with `COUNT(*)`.
  `get_account_relationships` reads all related accounts of one account
  (shows 500).
* `read_file_text` for files that are not indexed extracts on the fly up
  to the requested page.
* GUI: Options › "AI: MCP Server" panel (start/stop `HttpServer`, port,
  copy token / `client_config_snippet`, the `McpConfig` switches) and a
  "Go to reference" box — coordinator.
* Conformance run with the MCP inspector not automated (needs Node).
