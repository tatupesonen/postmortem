# Beyond Autopsy — additional analyses

Autopsy parity is the baseline (see `docs/PARITY.md`). This document lists
analyses postmortem adds on top, mostly inspired by Eric Zimmerman's
EZ Tools (MFTECmd, EvtxECmd, RECmd batch files, AppCompatCacheParser,
AmcacheParser, PECmd, LECmd, JLECmd, SBECmd, RBCmd, SrumECmd, SumECmd,
WxTCmd, SQLECmd, RecentFileCacheParser, bstrings, VSCMount) and KAPE's
target list. Everything is native Rust (no external tools), feeds the
standard result tree and the Timeline, and follows ADR-014 (streaming,
bounded memory).

Results use standard TSK artifact types where one fits; otherwise
custom types named `PM_*` (Autopsy itself supports custom types, so
reports and the UI handle them generically).

Status: **[done]** implemented and tested · **[track]** assigned to a
running agent track · **[next]** coordinator queue · **[later]**.

## 1. System overview ("OS Info" on the Data Source Summary)

Autopsy's `TSK_OS_INFO` has product name, version, owner, organisation,
install date, computer name and a few more. Additionally **[track:
windows-artifacts]**:

| Item | Source |
|---|---|
| Time zone name, UTC offset, DST rules, dynamic DST disabled | `SYSTEM\…\Control\TimeZoneInformation` |
| Last shutdown time | `SYSTEM\…\Control\Windows\ShutdownTime` |
| Install date, upgrade history | `SOFTWARE\…\CurrentVersion` InstallDate/InstallTime, `SYSTEM\Setup\Source OS (…)` |
| Edition, display version, build + UBR, installation type | `SOFTWARE\…\CurrentVersion` |
| Product ID, decoded product key | `DigitalProductId` (legacy + Win8 algorithms) |
| Hostname, domain / workgroup | `Tcpip\Parameters`, `ComputerName` |
| Network interfaces (IP, DHCP, gateway, DNS), network profiles (first/last connect) | `Tcpip\Parameters\Interfaces`, `NetworkList\Profiles` + `Signatures` |
| Last logged-on user, auto-logon (+ stored password flag) | `Authentication\LogonUI`, `Winlogon` |
| Default browser, page file / hibernation / crash dump settings, Defender exclusions, BitLocker status | various |

Also derived summary facts **[next]**: first and last user activity per
account (from event logs, SAM, NTUSER timestamps), evidence of anti-forensics
(cleared event logs 1102/104, time changes 4616, USN journal deleted,
`fsutil`, CCleaner/BleachBit execution, VSS deletion).

## 2. Windows event logs (EvtxECmd) — **[done: first cut]**

Autopsy only reaches EVTX through Plaso. postmortem parses every
`*.evtx` natively (`evtx` crate) and applies maps in the spirit of
EvtxECmd's, producing:

- **Logons** (Security 4624/4625/4634/4647/4648/4672, 4778/4779 RDP
  reconnect/disconnect; TerminalServices 21/22/24/25; RDP 1149):
  user, domain, logon type (named), source IP / workstation, process,
  logon ID, elevated token → `PM_LOGON_EVENT`.
- **Account management** (4720 created, 4722/4725 enabled/disabled,
  4726 deleted, 4724 password reset, 4732/4728/4756 group membership) →
  `PM_ACCOUNT_CHANGE`.
- **Services and persistence** (System 7045, Security 4697 service
  installed; 4698 scheduled task created; 7034/7036 service crashes and
  state changes) → `PM_SERVICE_INSTALLED` / `PM_SCHEDULED_TASK`.
- **Power and boot** (Kernel-General 12/13 boot/shutdown, EventLog
  6005/6006/6008, Kernel-Power 41/42/1, Power-Troubleshooter 1) →
  `PM_SYSTEM_POWER`.
- **Anti-forensics** (1102 Security log cleared, 104 log cleared, 4616
  system time changed) → `TSK_INTERESTING_ITEM` (Likely Notable).
- **PowerShell** (4103/4104 script block, 400/403/600) →
  `PM_POWERSHELL`.
- **USB / devices** (DriverFrameworks-UserMode 2003/2100/2102,
  Partition/Diagnostic 1006 with serials) → `TSK_DEVICE_ATTACHED`
  events with times.
- **Windows Defender** detections (1116/1117) → `TSK_MALWARE`.
- **Wi-Fi** (WLAN-AutoConfig 8001/8003) → `TSK_WIFI_NETWORK` connections.
All of them become Timeline events.

## 3. NTFS internals (MFTECmd) — **[next]**

- **$UsnJrnl:$J** change journal: create/delete/rename/overwrite records
  with reasons → Timeline events and a "USN Journal" result view;
  resolve parent paths through the MFT (including deleted parents
  via sequence numbers). Recovers evidence of deleted files whose MFT
  entries are reused.
- **$LogFile** transaction records (create/rename/delete operations).
- **$I30 slack** — already recovered as deleted names (TSK parity).
- **$STANDARD_INFORMATION vs $FILE_NAME** comparison: timestomp
  indicators (SI earlier than FN, zero sub-second precision) →
  `TSK_INTERESTING_ITEM` "Possible timestomping".
- **$Secure:$SDS** owners/ACL summary per file (file owner SID).
- **Volume Shadow Copies** (VSCMount): enumerate and expose snapshots as
  additional file systems of the volume (read-only) **[later]**.

## 4. Program execution and presence — **[next, after pm-registry merges]**

- **ShimCache / AppCompatCache** (AppCompatCacheParser): path, last
  modified, executed flag (XP–Win11 formats) → `TSK_PROG_RUN`-like
  `PM_SHIMCACHE`.
- **Amcache.hve** (AmcacheParser): InventoryApplicationFile (path, SHA-1,
  link date, publisher), InventoryApplication, devices, drivers;
  hash lookups of Amcache SHA-1s against hash sets.
- **RecentFileCache.bcf** (Win7).
- **BAM/DAM** (`bam\State\UserSettings`): last execution per user.
- **Prefetch** **[track]**, **UserAssist** **[track]**, **SRUM**
  **[track]** (app resource usage and network bytes per app).
- **Windows Timeline** `ActivitiesCache.db` (WxTCmd) → executed apps
  and opened files with focus durations.
- **PowerShell history** `ConsoleHost_history.txt` per user.
- **Scheduled tasks** (`Windows\System32\Tasks\*` XML), **services**
  and **Run/RunOnce/Winlogon/IFEO** autostart locations (persistence
  overview, Autoruns-like).

## 5. User activity — **[next]**

- Office MRU, Office TrustRecords (macros enabled), WordWheelQuery
  (Explorer searches), TypedPaths, LastVisitedPidlMRU, ComDlg32 MRUs.
- **Notifications** database (`wpndatabase.db`), **clipboard history**
  (ActivitiesCache), **Sticky Notes** (`plum.sqlite`).
- **Thumbcache** / **IconCache** entries (viewed pictures).
- **OneDrive** sync logs and settings, **Teams/Slack/Discord** local
  caches (LevelDB/IndexedDB) **[later]**.
- **RDP bitmap cache** reconstruction (`bcache*.bmc`, `cache*.bin`) **[later]**.
- **User Access Logging** on servers (SumECmd, ESE) **[later]**.

## 6. Cross-artifact analysis — **[later]**

- Unified **USB device story**: first install (setupapi.dev.log),
  MountedDevices drive letter, EMDMgmt volume serial/label, per-user
  MountPoints2, last connected/removed (event logs) in one artifact.
- **Logon sessions**: pair 4624/4634 into sessions with durations,
  correlate with RDP sources.
- **Lateral movement** hints: RDP outbound (Terminal Server Client MRU),
  `PsExec` services (7045), admin share access (5140/5145).
- **Timestamp anomaly** report across sources.

## 7. Other platforms — **[later]**

macOS (KnowledgeC, FSEvents, unified log, quarantine events, Biome),
Linux (bash/zsh history, auth.log, wtmp/btmp, journald, cron, systemd
units), iOS beyond iLEAPP-equivalents.

## 8. MCP server for AI assistants — **[next]**

Autopsy 4.23 has an MCP server: 17 read-only query tools served over HTTP
on localhost with a token, plus a stdio bridge. That set is a parity item
(`docs/PARITY.md` §30, WP-18), and its tool names stay the same so existing
client setups keep working. postmortem goes further: it gives an assistant
the analyst's real search and pivot tools, lets it cite each claim back to
the evidence, and can optionally let it record findings.

### Transport and lifecycle

- **stdio** is the main transport: `postmortem mcp --case <dir>`. It opens
  the case read-only, using the case database's WAL mode, so it can run
  next to the GUI or after the GUI has closed. It needs no network.
- **Streamable HTTP**, the Autopsy-compatible mode, runs while the GUI has
  a case open.
  - It binds to `127.0.0.1` only. Clients send a bearer token, which is
    regenerated each session and stored in the user's settings folder with
    `0600` permissions.
  - Options › "AI: MCP Server" turns it on and off, sets the port, copies
    the token, and shows a ready-made client config snippet.
- It is a crate of its own, `pm-mcp`, built on the official Rust SDK
  (`rmcp`). It uses only `pm-case`, `pm-casedb`, `pm-search` and
  `pm-text`, and never the UI.

### Conventions for every tool

- **Stable references**: `file:123`, `artifact:45`, `result:46`,
  `account:7`, `os_account:3`, `event:900`, `ds:1`. Every row carries its
  reference, data source, path and, where it applies, a byte offset. An
  assistant can then cite "file:123 @ 0x4000", and the GUI's
  "Go to reference" box can open it.
- **Paging**: keyset cursors (`next_cursor`), a default page of 50 rows
  and a maximum of 500. Each response is capped at 256 KiB, and anything
  cut is marked with `truncated: true` and what to call for the rest. The
  server never runs an O(files) scan inside a request (ADR-014).
- **Times**: ISO 8601 in UTC plus Unix seconds. The case time zone comes
  with `get_case_summary`.
- **Evidence is untrusted input.** Text taken from the evidence (file
  contents, names, registry values, messages) is returned only inside
  `evidence` fields, and the server instructions tell the model that this
  data is never instructions to follow. This defends against prompt
  injection planted in a suspect's files.
- **Audit**: each call (tool, arguments, row count, client name) is
  appended to the case's `Logs/mcp-audit.jsonl`, so the examiner can see
  what the assistant looked at.

### Tools

**Autopsy-compatible**: `get_case_summary`, `query_data_sources`,
`get_hosts`, `get_data_source_tree`, `get_object_children`,
`query_files`, `get_file_content`, `query_data_artifacts`,
`query_analysis_results`, `query_tags`, `query_timeline`,
`summarize_timeline`, `get_os_accounts`, `get_communications_accounts`,
`get_account_relationships`, `list_reports`, `get_report_content`.

**Search**, the main addition:

| Tool | What it does |
|---|---|
| `search_text` | Content index search: exact, substring or regex, with Autopsy's keyword semantics. Optional keyword list, data source and MIME filters. Returns hits with snippets and chunk offsets. |
| `search_file_names` | File-name index: substring, prefix or exact on the name or full path. Filters: extension, size, deleted, data source. |
| `find_files` | Structured filters: File Search by Attributes, plus MIME, score, tag, hash-set hit, extension mismatch and an "inside this folder" option. |
| `lookup_hash` | MD5/SHA-1/SHA-256 against the case, the configured hash sets and NSRL. Reports known status and where the hash occurs. |
| `run_keyword_list` | Runs a saved keyword list (or the built-in phone/e-mail/IP/URL/card lists) over the index and returns grouped hit counts. |

**Content**:

| Tool | What it does |
|---|---|
| `get_file_metadata` | The full File Metadata tab: every timestamp (`$SI` and `$FN`), MFT entry and sequence, ADS list, hashes, MIME type, extension mismatch, score with its reasons, tags, comments, OS account owner, and the derived-from chain. |
| `read_file_text` | Extracted text by page (the Extracted Text viewer's pages), optionally starting at a search hit. |
| `read_file_bytes` | Hex or base64 at an offset, at most 64 KiB per call. |
| `get_file_strings` | Autopsy's strings extraction over a byte range, with a script choice. |
| `get_image_preview` | A downscaled PNG thumbnail of an image or a video frame, returned as MCP image content so vision models can look at it. Off by default (see below). |

**Artifacts and pivots**:

| Tool | What it does |
|---|---|
| `list_artifact_types` | Data artifact and analysis result types with their counts: the result tree, so the model can see what exists. |
| `get_artifact` | All attributes, the source file, linked artifacts and the score of one artifact. |
| `get_related` | From any reference: the same hash elsewhere, other files in the same folder, artifacts from the file, accounts on the artifact, and events from the artifact. |
| `list_notable` | The Score tree: bad and suspicious items with the reason each was scored. |

**Timeline and activity** (built on the analyses in §2–§6):

| Tool | What it does |
|---|---|
| `timeline_events` | Filters: event types, a time window, data source, text and "tagged/hash hits only". Paged, oldest first or newest first. |
| `timeline_histogram` | Bucketed counts per event super-type, to find the busy periods. |
| `events_around` | Events within ±N minutes of a time or a reference ("what happened around this download?"). |
| `user_activity` | For one OS account: logons and sessions, program execution, recent documents, LNK files and jump lists, browser history and USB use, merged in time order. |
| `program_execution` | One row per executable: first run, last run, run count, and the evidence for each (Prefetch, Amcache, ShimCache, UserAssist, BAM, SRUM). |
| `usb_devices` | The combined USB device story from §6. |
| `query_event_logs` | Channel, event ID, provider, time window and text. Uses the EvtxECmd-style field maps from §2. |
| `read_registry_key` | Hive plus key path, giving values and subkeys with last-write time. Reads hives in the image, read-only. |

**Communications and places**: `get_messages` (one thread between
accounts, paged), `get_contacts`, `geolocation_points` (bounding box and
time window).

**Coverage**: `list_ingest_jobs` reports the modules that ran per data
source, their settings, errors and skipped files. The assistant can then
say "no keyword search was run" instead of "no hits".

### Writing to the case (opt-in)

The server is read-only unless the examiner enables "Allow the assistant
to tag and comment" in the options, or starts it with `--allow-writes`.

| Tool | What it does |
|---|---|
| `add_tag` / `remove_tag` | Tag files or results with an existing tag name and a comment. The examiner is recorded as `mcp:<client name>` so these tags can be filtered and reviewed. |
| `set_comment` | Sets the central-repository-style comment on a file or result. |
| `save_search_results` | Stores a `search_text` result as keyword-hit analysis results, like Autopsy's "Save search results". |

Ingest, deleting data sources and generating reports stay with the
examiner and are never exposed.

### Privacy switches

These are in the Options panel and are all off by default:
- `get_image_preview`: sending pictures to a cloud model can be
  unacceptable, for example for CSAM cases.
- Showing the raw bytes and text of files tagged "Child Abuse Material"
  (Project VIC CAM). With this off, those files are withheld from every
  content tool, and their metadata is still returned.

### Prompts (MCP prompts)

Canned workflows the client can offer:
- "Triage this data source": OS info, users, notable items, top
  programs, USB devices, last activity.
- "Account activity for <user>".
- "What happened around <time / reference>".
- "Summarise this communication thread".

### Tests

- Golden JSON responses for each tool, run against the demo case.
- A conformance run with the MCP inspector.
- Tests that cursors return every row exactly once.
- A prompt-injection fixture: a file whose text says "ignore previous
  instructions…" must come back only inside `evidence`.
- Write tools must fail while writes are disabled.
