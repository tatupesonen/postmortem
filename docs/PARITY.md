# postmortem ↔ Autopsy parity matrix

Audit date: 2026-10-15. Master is at `bbc96d8`. The 6 running track worktrees have no commits or changes yet, so every "In progress" row below is scope only, not code.

Method: I enumerated every NetBeans module and package in `/home/tatu/slop/autopsy`, including `@OptionsPanelController`, `IngestModuleFactory`, `DataSourceProcessor`, `DataContentViewer`/`DataResultViewer` and report-module registrations. I cross-checked against `docs/inventory/01–08`, then read the postmortem crates (`crates/*`), `PLAN.md`, `PROGRESS.md` and `agents/RULES.md`.

**Status legend**
- **Done**: implemented and tested on master.
- **Partial**: some of it exists; the Notes say what is missing. "Tests owed" means the code exists but has no test.
- **In progress (track)**: inside the scope of a running track. Not on master.
- **Missing**: nothing on master and no track owns it.
- **N/A?**: an Autopsy platform artefact that probably should not be ported. The coordinator should confirm each one.

**Path abbreviations**
- `A/` = `Core/src/org/sleuthkit/autopsy/`
- `CT/` = `Core/src/com/basistech/df/cybertriage/autopsy/`
- `RA/` = `RecentActivity/src/org/sleuthkit/autopsy/recentactivity/`
- `KWS/` = `KeywordSearch/src/org/sleuthkit/autopsy/keywordsearch/`
- `IG/` = `ImageGallery/src/org/sleuthkit/autopsy/imagegallery/`
- `EXP/` = `Experimental/src/org/sleuthkit/autopsy/experimental/`
- `TB/` = `thunderbirdparser/src/org/sleuthkit/autopsy/thunderbirdparser/`
- `IPM/` = `InternalPythonModules/`
- `pm-x/…` = `crates/pm-x/…`

## 0. Headline findings

1. **What is solid (Done):** evidence for raw/split/E01 + MBR/GPT + NTFS/FAT/exFAT/ext, all with TSK differential tests. Also the case create/open/load, the ingest core, Hash Lookup + all hash-set formats, the timeline first cut, and the shell/browse slice. There are about 346 tests in the workspace.
2. **Implemented but untested:** Recent Activity Chromium/IE (only a few unit tests), Picture Analyzer (no end-to-end test), Timeline Details view, the timeline chart, and artifact events. `PROGRESS.md` already records these as owed.
3. **Zero of the 126 items in the test-parity checklist (`docs/inventory/08-autopsy-tests.md §20`) are ticked.** Some are de facto covered: the hash formats and the ingest filters. No golden-dump harness exists.
4. **Schema blocker.** The case DB has no tables for:
   - OS accounts / realms / persons
   - communication accounts + relationships
   - timeline events
   - aggregate scores
   - data-artifact ingest pipeline support
   
   `RULES.md` only lets `derived-and-carving` add migrations. As a result:
   - `windows-artifacts` (SAM/ProfileList → OS accounts) and `email-mobile` (accounts/relationships) can only emit artifacts.
   - Communications, OS Accounts, CR and the Persons/Hosts UI have nothing to build on.
   - This must be resolved first (WP-0).
5. **Several views read an in-memory index or the evidence, not the case DB.** The Data Sources tree reads the evidence directly. Gallery, Discovery, Data Source Summary and the file timeline read the in-memory `pm-index` catalogue. This is ADR-014 scale debt.
6. **UI is excluded from every track** (`pm-ui` is coordinator-owned). So every In-progress engine row also has an untracked UI-integration part, gathered in WP-2/WP-3.

---

## 1. Case management (`A/casemodule`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Case folder layout + metadata file (`.aut`) | `A/casemodule/Case.java`, `CaseMetadata.java` | Done | `case.pmcase` TOML + `case.db`, folders Export/Reports/ModuleOutput/Temp/Log (`pm-case/src/case.rs`). Tests in `pm-case/tests/case.rs`. |
| New Case wizard (name, base dir, type; optional number/examiner name/phone/email/notes) | `NewCaseWizardAction`, `NewCaseVisualPanel1/2`, `OptionalCasePropertiesPanel` | Done | `pm-ui/src/case_ui.rs` with name/location validation and a kittest flow test. No case-type choice (no multi-user) and no CR organisation picker. |
| Open Case (file chooser) | `CaseOpenAction` | Done | `pick_case`. Multi-user case browser is missing (see §30). |
| Open Recent Case (menu + welcome list) | `RecentCases`, `OpenRecentCasePanel` | Partial | Present in the menu and the welcome screen. Not done: removing missing cases from the list, and the 2-column dialog. No test. |
| Welcome / startup window | `StartupWindowProvider`, `StartupWindow`, `CueBannerPanel` | Done | `case_ui::welcome`, with screenshot test. |
| Close Case (with ingest-running check) | `CaseCloseAction`, `actions/IngestRunningCheck` | Partial | Close exists. No confirmation when closing while ingest is running. |
| Delete Case | `CaseDeleteAction`, `Case.deleteCurrentCase` | Missing | No API in `pm-case` and no UI. |
| Case Details dialog (view/edit properties, data source list) | `CaseDetailsAction`, `CaseInformationPanel`, `CaseDetailsPanel`, `EditOptionalCasePropertiesPanel` | Partial | `Case::set_details` exists with a test. No dialog after creation. |
| Remove data source | `DeleteDataSourceAction`, `Case.deleteDataSourceFromCurrentCase` | Partial | `Case::delete_data_source` exists with a test. No UI action. |
| Missing image relocation on open | `MissingImageDialog` | Partial | `relocate_data_source` exists and is tested (`moved_evidence_is_reported_and_can_be_relocated`). No UI prompt. |
| Unpack and Open Portable Case | `UnpackagePortableCaseAction/Dialog` | Missing | Depends on the Portable Case report (§26). |
| Open Case Folder / Open Output Folder / Open Log Folder | `A/actions/OpenOutputFolderAction`, `OpenLogFolderAction`, Tools › Open Case Folder | Missing | — |
| Case events bus (`Case.Events`: DATA_SOURCE_ADDED, tags, reports, CR comment, hosts…) | `A/casemodule/events/*`, `A/events/*` | Missing | No event bus in `pm-case`. The UI polls and refreshes instead. |
| Examiner recording | `Case`/TSK `examiners` | Partial | There is a table and the examiner is typed into the wizard. The OS-login examiner is not used for tags (there are no tag writes from the UI). |
| AutopsyService case-open hooks | `A/appservices/AutopsyService` | Missing | Plugin lifecycle hooks; relevant to WP-24. |
| Multi-data-source "group by data source?" prompt (>5 sources) | `directorytree/GroupDataSourcesDialog` | Missing | Depends on person/host grouping (§7). |

## 2. Data source processors & Add Data Source wizard

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Add DS wizard: DSP selection page | `AddImageWizardSelectDspPanel` | Missing | The wizard (`pm-ui/src/add_source.rs`) only supports disk images. |
| Add DS wizard: host selection page (new host from DS name / named host / existing host) | `AddImageWizardSelectHostPanel` | Missing | The `hosts` table exists but nothing uses it. |
| Disk Image / VM file DSP (path, time zone, sector size, ignore FAT orphans, MD5/SHA-1/SHA-256 to verify, BitLocker password) | `A/casemodule/ImageDSProcessor`, `ImageFilePanel`, `AddImageTask` | Partial | Done: path, time zone and FAT-orphan option. Missing: sector size override, hash entry fields (input for Data Source Integrity), and password. Formats are limited (§3). |
| Ingest configuration page in wizard | `AddImageWizardIngestConfigPanel` | Partial | Module checkboxes, hash-set configuration and the unallocated-space filter radio, with a test. No custom file filters or profiles, and no settings panels for other modules. |
| Add progress page (+ adding warnings/errors) | `AddImageWizardAddingProgressPanel` | Partial | Progress is shown in the status bar and the Ingest Progress window. There is no wizard page with critical-error / warning lists. |
| Local Disk DSP (read-only physical device; optional VHD copy; FAT-orphan option) | `LocalDiskDSProcessor`, `LocalDiskPanel`, `LocalDiskSelectionDialog` | Missing | Planned in `pm-evidence`. The image-writer VHD copy is in §31. |
| Logical Files DSP (files/folders → LocalFilesDataSource, virtual dirs, timestamps) | `LocalFilesDSProcessor`, `LogicalFilesDspPanel`, `LocalFilesPanel`, `AddLocalFilesTask`, `services/FileManager` | Missing | The schema has `origin=3 local` and `local_path`, but no loader and no UI. |
| Logical Evidence File (L01) via Logical Files DSP | `LogicalEvidenceFilePanel` | Missing | Needs the EWF L01 `ltree` reader (`PLAN.md` M1 unchecked). |
| Unallocated Space Image File DSP (raw blob, optional chunking) | `A/datasourceprocessors/RawDSProcessor`, `RawDSInputPanel`, `AddRawImageTask` | Missing | — |
| XRY Text Export DSP (calls/contacts/messages/device info/web bookmarks reports) | `A/datasourceprocessors/xry/*` (15 files) | Missing | — |
| Autopsy Logical Imager Results DSP | `A/logicalimager/dsp/*` | Missing | — |
| Memory Image DSP (Volatility) | `EXP/volatilityDSP/*` | Missing | External-tool runner (§17). |
| Auto-ingest DSP confidence ordering (`AutoIngestDataSourceProcessor.canProcess`) | `A/datasourceprocessors/DataSourceProcessorUtility` | Missing | Needed for the CLI and auto-ingest. |
| Unallocated-space virtual files (`$Unalloc`, chunked) | TSK + `AddImageTask` | Done | `pm-case/src/load.rs` (`chunk_ranges`), with tests. |
| `$OrphanFiles` virtual dirs | TSK | Done | Tested for FAT/NTFS/exFAT. |
| `$CarvedFiles` virtual dir | TSK/PhotoRec | Done | — |
| Slack files (`-slack`) | TSK `SlackFile` | Missing | `PLAN.md` M1 item unchecked. Schema has `origin 6 slack`. |
| Data-source time zone (FAT local times) | `ImageFilePanel` | Done | — |

## 3. Image formats (`pm-image`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Raw / split raw (.dd .img .raw .bin .001 .aa) | `GeneralFilter.RAW_IMAGE_EXTS`, TSK | Done | Tests in `pm-image/tests/open.rs`. |
| EWF v1 E01/s01 (multi-segment, stored hashes, header metadata) | `GeneralFilter.ENCASE_IMAGE_EXTS`, TSK/libewf | Done | Verified against ewfacquire, with a fuzz target (`fuzz_targets/ewf_read.rs`). |
| EWF2 Ex01 / Lx01 | TSK/libewf | Missing | Sniffed only; returns `Unsupported`. |
| L01 logical evidence | `LogicalEvidenceFilePanel` | Missing | See §2. |
| VMDK (sparse, flat, descriptor, split) | `GeneralFilter.VIRTUAL_MACHINE_EXTS`, TSK/libvmdk | Done | In-house reader (pm-image), with parents; differential tests against qemu-img. |
| VHD (fixed/dynamic/differencing) | same | Done | In-house reader with parent chains. |
| VHDX | same | Done | In-house reader with parent chains. |
| QCOW2 | TSK (libqcow), not in Autopsy's file filter | Done | In-house reader with backing files. |
| DMG (UDIF) | Not in Autopsy | Missing | In the postmortem `PLAN.md` only. |
| AFF / AFF4 | TSK optional; not in Autopsy's file filter | Missing | Low priority. |
| Image metadata (acquisition details, device id, image hashes) | TSK `tsk_image_info` | Partial | EWF header metadata is parsed. The `data_sources` columns exist. No Image Details panel (§9). |

## 4. Volume systems, pools, encryption (`pm-volume`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| DOS/MBR + EBR | TSK | Done | Matches `mmls`. Tests and fuzz. |
| GPT (incl. backup header) | TSK | Done | Same as above. |
| Apple Partition Map | TSK | Missing | — |
| BSD disklabel | TSK | Missing | — |
| Sun VTOC | TSK | Missing | — |
| Pools: APFS container | TSK pools, `PoolNode` | Missing | Needs `pm-fs-apfs`. |
| Pools: LVM | TSK pools | Missing | — |
| BitLocker decryption (password / recovery key) | TSK, `ImageFilePanel` password, CLI `--key` | Missing | Encryption *detection* is in progress (core-file-modules). Unlock is untracked. |
| LUKS / FileVault / Core Storage detection | TSK (detect) | Done | Detection only (pm-encryption); unlock is not planned. |

## 5. File systems (`pm-fs-*`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| NTFS (MFT, ADS, compression, deleted, orphans, $I30 slack) | TSK | Done | TSK-exact against the NIST image. Differential tests and fuzz. |
| NTFS extras: owner SIDs ($Secure), reparse points, EFS flag, WOF compression, hard links, $UsnJrnl/$LogFile | TSK (partly) | Missing | `PLAN.md` M1 item unchecked. |
| FAT12/16/32 (incl. deleted LFN, orphans) | TSK | Done | Differential tests and fuzz. |
| exFAT | TSK | Done | Matches fls; fuzz. |
| ext2/3/4 | TSK | Done | fls parity. **No fuzz target** (`fuzz/fuzz_targets` has none for ext). |
| HFS+/HFSX (resource forks, compression, hard links) | TSK | Missing | — |
| APFS (incl. snapshots, encryption) | TSK | Missing | — |
| ISO9660 (+Joliet/Rock Ridge) | TSK | Missing | — |
| UDF | TSK | Missing | — |
| UFS1/2 (FFS) | TSK | Missing | Not in `PLAN.md`; TSK supports it. |
| XFS | TSK | Missing | Low priority in `PLAN.md`. |
| Btrfs | TSK | Missing | Low priority. |
| YAFFS2 | TSK | Missing | Low priority. |
| Unsupported content node | TSK `UnsupportedContent` | Partial | Partitions without a known FS are listed as volumes. No explicit node. |

## 6. Case database & data model (`pm-casedb`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Objects/files/VS/FS/data sources schema + bulk loader | TSK schema | Done | `migrations/0001_initial.sql`, about 205k rows/s. Tests in `pm-casedb/tests`. |
| Standard artifact/attribute type catalogue (incl. custom types) | TSK `BlackboardArtifact.Type` | Done | 226 TSK_ entries in `catalog.rs`, plus the `custom_types` test. |
| Artifacts with score/conclusion/justification/configuration, review status | TSK analysis results | Partial | Scores, conclusion, justification and configuration shown; review-status UI missing. |
| Aggregate score per object (incl. tags, artifacts as objects) | TSK `tsk_aggregate_score` | Done | Schema v2 `scores` table, TSK ordering, tags included. |
| Tags (tag names, tag sets, file/artifact tags, byte-range tags) | TSK tags | Partial | Files and results with sets and scores; byte-range tags missing. |
| Hosts | TSK `HostManager` | Done | Schema v2 + Manage Hosts UI. |
| Persons | TSK `PersonManager` | Done | Schema v2 + person UI on host nodes. |
| OS accounts, realms, instances, attributes | TSK `OsAccountManager` | Done | Schema v2 with TSK merge rules; SAM/ProfileList feed it. |
| Communication accounts + account_relationships | TSK `CommunicationsManager` | Done | Schema v2; written by the Email Parser via post_communication. |
| Timeline events table (event types, 3 description levels, tagged/hash_hit flags) | TSK `TimelineManager`, `tsk_events` | Partial | Schema v2 `events` + API; the Timeline UI still builds events in memory (WP-15). |
| Ingest job records | TSK `ingest_jobs` | Done | `start/finish_ingest_job`. |
| Reports table | TSK `reports` | Partial | Table only. |
| Derived files / file re-injection | TSK `DerivedFile` | Done | ADR-016; `pm_modules::derive`. |
| PostgreSQL back end | TSK | Missing | M11. |
| Autopsy case importer (TSK DB → pm) | — | Missing | Stretch goal in `PLAN.md`. |

## 7. Directory tree (`A/directorytree`, `A/datamodel`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Data Sources → content hierarchy (image → volumes → dirs; VS/FS/root inlined) | `datamodel/ContentChildren`, node classes | Partial | `pm-ui/src/model.rs` reads the evidence directly, not the DB. No host level. No derived/local/carved/artifact-child nodes. |
| Hosts level under Data Sources; "Unknown Host" | `DataSourcesNode`, `HostNode` | Done | — |
| Group by Person/Host layout (per-DS filtered subtrees) | `AutopsyTreeChildFactory`, `PersonNode`, `DataSourceGroupingNode` | Missing | — |
| File Views › By Extension (Images, Videos, Audio, Archives, Databases, Documents…, Executable) with counts | `FileTypesByExtension` | Done | `pm-ui/src/views.rs`, from the case DB. Test `views_list_files_by_type_from_the_case_database`. |
| File Views › By MIME Type | `FileTypesByMimeType` | Partial | Implemented; empty until File Type ID lands (in progress). |
| File Views › Deleted Files (FS / All) | `DeletedContent` | Done | Tested. |
| File Views › File Size (50–200 MB, 200 MB–1 GB, 1 GB+) | `FileSize` | Done | — |
| Hide known files in Views / in Data Sources; hide slack | `KnownAndSlackFilterBase`, View prefs | Partial | Only "hide known in Views", in the View menu. |
| Data Artifacts / Analysis Results type nodes with counts (refresh during ingest) | `DataArtifacts`, `AnalysisResults` | Done | `pm-ui/src/results.rs`. Test `analysis_results_are_listed_and_open_the_source_file`. |
| Keyword Hits subtree (list › term › hits) | `datamodel/KeywordHits` | Done | List › keyword › match, Autopsy grouping rules. |
| Hashset Hits by set name | `HashsetHits` | Missing | Currently a flat list. |
| Interesting Items / Interesting File/Artifact hits by set | `InterestingHits` | Missing | — |
| E-Mail Messages by account › folder | `EmailExtracted` | Missing | — |
| Communication Accounts by type; credit cards By File / By BIN (BIN DB) | `datamodel/accounts/Accounts`, `ranges.csv` | Missing | The search track does card validation; the tree is untracked. |
| TSK_MALWARE filtered to notable | `AnalysisResults` | Missing | — |
| OS Accounts node | `OsAccounts.java` | Done | Under Analysis Results (user choice). |
| Tags node (tag › File Tags / Result Tags; hide other users' tags) | `Tags.java` | Partial | "Hide other users' tags" missing. |
| Score node (Bad Items / Suspicious Items) | `ScoreContent.java` | Done | — |
| Reports node (open/delete report) | `Reports.java` | Done | Open, open folder, delete (from the case). |
| Cyber Triage data node (`ct_errors`) | `CyberTriageData.java` | N/A? | Only for Cyber Triage-written DBs. |
| Count throttled refresh, "counting…" threshold >1M | `RefreshThrottler`, `ViewsNode` | Partial | Counts refresh every 2 s during ingest. No 1M threshold. |
| View Options gear popup (tree settings) | `DirectoryTreeTopComponent` | Missing | — |
| Back/forward navigation | `DirectoryTreeTopComponent` | Done | Alt+←/→. |
| Collapse All | `directorytree/CollapseAction` | Missing | — |
| Hide rejected results (session) | View prefs | Missing | — |

## 8. Result viewers (`A/corecomponents`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Table tab: virtualised, sortable | `DataResultViewerTable` | Partial | Files: 10 fixed columns, single-key sort (`pm-ui/src/table.rs`), tested. Results table is capped at `MAX_ROWS = 20000` with no paging. |
| Column chooser, reorder, multi-key sort, per-node-type persistence | `ResultViewerPersistence` | Missing | — |
| File row columns parity: Known, Location, MD5, SHA-256, MIME type, Original Name (translated), hidden extras (Mode, UID, GID, Meta Addr…) | `AbstractAbstractFileNode` | Partial | Name, times, size, flags, extension and inode only. |
| Artifact row columns (Source Name, Score/Conclusion/Justification, per-attribute columns, Data Source, File Path…) | `BlackboardArtifactNode.createSheet` | Partial | Attribute columns plus source file. No score columns, Data Source column, or the email/mismatch special cases. |
| S / C / O columns (async) | `GetSCOTask`, `SCOData` | Partial | S and C done; O needs the Central Repository. |
| Paging (page size pref, go-to page) | `BaseChildFactory` paging | Missing | Virtualisation replaces it for files; results have a hard cap. |
| Save Table as CSV / Export Selected Rows to CSV | `ExportCSVAction` | Missing | — |
| Thumbnail tab (S/M/L, 200/page, sort chooser) | `DataResultViewerThumbnail`, `SortChooser` | Missing | Thumbnails exist only in the Gallery/Discovery windows. |
| Summary tab (Data Source Summary in result pane) | `resultviewers/summary` | Partial | See §23. |
| Review status: Approve / Reject accounts and results | `datamodel/accounts` actions | Missing | The column exists in the DB. |
| Result window pop-out / multiple result windows (Window › Result Viewers) | `DataResultTopComponent` | Partial | Only the content viewer pops out. |

## 9. Context-menu actions & navigation (`A/directorytree`, `A/actions`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| View in New Window | `NewWindowViewAction` | Done | "Open in New Window" plus pop-out viewports. |
| Extract File(s) / folders (recursive, with name collision handling) | `ExtractAction`, `actionhelpers/ExtractActionHelper` | Partial | Single file only. No folders, multi-select or progress. |
| Extract Unallocated Space to Single File(s) | `ExtractUnallocAction` | Missing | — |
| Open in External Viewer (Ctrl+E) + External Viewer rules | `ExternalViewerAction`, `ExternalViewerShortcutAction`, `ExternalViewerRulesManager` | Missing | 18 Autopsy UI tests to port (08 §12). |
| View File in Timeline / View Result in Timeline | `ViewFileInTimelineAction`, timeline actions | Partial | Done for files. Not done for artifacts. |
| View in Directory (from result rows / tags) | `ViewContextAction` | Partial | Result rows open the source file in the viewer. They do not navigate the tree. |
| View Source Result / View Associated Content / View Artifact / View OS Account | `ViewSourceArtifactAction`, `ViewAssociatedContentAction`, `actions/ViewArtifactAction`, `ViewOsAccountAction` | Missing | — |
| Copy Path | (NetBeans copy) | Done | — |
| Add File Tag / Add Result Tag / Replace / Remove (with comment), Bookmark Ctrl+B | `A/actions/*Tag*`, `GetTagNameAndCommentDialog` | Done | Includes Ctrl+B Bookmark and Tag and Comment. |
| Add File(s) to Hash Set | `hashdatabase/AddContentToHashDbAction` | Missing | `pm-hashdb` can write kdb (`add`). |
| Image Details / Volume Details / File System Details | `ImageDetailsPanel`, `VolumeDetailsPanel`, `FileSystemDetailsPanel` | Missing | — |
| Run Ingest Modules (on a data source) | `ingest/runIngestModuleWizard/*`, `RunIngestModulesAction` | Missing | The `pm-ingest` API supports it; no UI. |
| Remove Data Source | `DeleteDataSourceAction` | Missing (UI) | API done. |
| Open File Search by Attributes (on a data source) | `FileSearchTreeAction` | Done | — |
| View Summary Information (on a data source) | `ViewSummaryInformationAction` | Partial | Selecting a DS shows the summary. |
| Drag-and-drop image to add | — | Done | Postmortem extra. |

## 10. Content viewers (`A/contentviewers`, `A/corecomponents`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Automatic best-viewer selection + "keep preferred viewer" pref | `DataContentPanel`, `ViewerPriority` | Partial | Auto-select plus a user-pinned tab. No preference setting. |
| Hex viewer (paging, go-to offset, copy, launch external hex editor) | `DataContentViewerHex` | Partial | Virtualised with go-to (`viewers/hex.rs`). No selection/copy and no external editor. |
| Strings (scripts/encodings: Latin, CJK…, language selection) | `textcontentviewer/StringsTextViewer`, `coreutils/StringExtract` | Partial | ASCII and UTF-16LE only. No script selection. |
| Text (plain) | `textcontentviewer` | Done | BOM detection, with tests. |
| Extracted Text viewer (indexed text, hit highlighting, next/prev hit, pages) | `KWS/ExtractedTextViewer`, `HighlightedText` | Done | Indexed pages or on-the-fly extraction; hit highlighting and navigation. |
| Translation sub-viewer | `texttranslation/ui/TranslatedTextViewer` | Missing | — |
| Application › Picture (zoom, rotate, fit, region tags) | `MediaViewImagePanel`, `imagetagging/*` | Partial | Zoom/fit only. No rotate and no image-region tags (`contentviewertags`). |
| Application › Audio/Video player | `MediaPlayerPanel` (GStreamer) | Missing | — |
| Application › HTML viewer | `HtmlViewer` | Missing | — |
| Application › PDF viewer | `PDFViewer` | Missing | — |
| Application › SQLite viewer (tables, paging, epoch columns, CSV export) | `SQLiteViewer` | Missing | — |
| Application › Binary plist viewer | `PListViewer` | Missing | — |
| Application › Windows Registry viewer (rejview tree + values) | `WindowsRegistryViewer`, `A/rejview/*` | Missing | The hive parser is coming from windows-artifacts; the viewer is untracked. |
| File Metadata viewer (name, path, type, MIME, size, times, MD5/SHA-256, known, internal id, istat output) | `contentviewers/Metadata` | Partial | Missing: hashes, MIME, known status, data source, object id, and the istat-style block. |
| Data Artifacts viewer (artifacts of a file; per-type sub-viewers) | `DataArtifactContentViewer`, `artifactviewers/*` | Missing | Includes the default table, General Purpose (web), Message (email/SMS with attachments), Contact and Call Log viewers. |
| Analysis Results viewer (score, conclusion, justification per result) | `analysisresults/*` | Missing | — |
| Annotations viewer (tags, CR comments, hash/interesting hits) | `annotations/*` | Missing | — |
| Context viewer (source/usage: downloaded from, attached to, associated artifacts) | `contextviewer/*` | Missing | — |
| OS Account viewer | `osaccount/*` | Missing | — |
| Other Occurrences viewer (CR) | `centralrepository/contentviewer/*` | Missing | See §25. |
| Pop-out content viewer windows | `DataContentTopComponent` | Done | Deferred viewports. |

## 11. Tags (`A/casemodule/services/TagsManager` etc.)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Default tag names (Bookmark, Follow Up, Notable Item) | `TagNameDefinition` | Done | Seeded in the migration. |
| Tag file / artifact with comment; replace; delete | `TagsManager`, `A/actions/*` | Done | — |
| Custom tag names + notable flag (Options › Tags) | `TagOptionsPanel`, `TagNameDialog` | Done | Tools › Options › Tags. |
| Tag sets (Project VIC categories) | `TagSetDefinition` | Done | Created on case open; exclusive within a set. |
| Notable tag → item score / CR known-bad | `TagsManager`, CR listeners | Partial | Score done; CR known-bad needs the Central Repository. |
| Image region tags (content viewer tags) | `contentviewertags/ContentViewerTagManager` | Missing | — |
| Per-examiner tags, "show only current user's tags" | View prefs | Missing | — |

## 12. Ingest framework (`A/ingest`, `pm-ingest`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Module factory API, file + data-source modules, staged DS modules | `IngestModuleFactory`, `IngestJobExecutor` | Done | `pm-ingest`, 10 integration tests. |
| Priority scheduling (user folders first, unalloc last), N workers, cancellation | `IngestTasksScheduler` | Done | Tested. |
| Data-artifact ingest pipeline & analysis-result pipeline | `DataArtifactIngestModule`, `AnalysisResultIngestModule` (KWS indexing of artifacts, CR artifact module) | Missing | Not in any track; `pm-ingest` is owned by derived-and-carving. Needed by the search and CR work. |
| Derived-file re-injection into running jobs | `IngestJobExecutor.addFiles` | Done | — |
| Standard file filters (all / not unallocated) | `FilesSetsManager` standard filters | Done | `FileFilter`, tested. |
| Custom file ingest filters (FilesSet rules) | `FilesSetsManager`, `IngestOptionsPanel` › File Filters | Partial | The FilesSet engine is in progress (core-file-modules). Wiring into `pm-ingest`, the UI and 7 `IngestFileFiltersTest` ports are untracked. |
| Ingest profiles | `ingest/profile/IngestProfiles`, `ProfileSettingsPanel` | Missing | — |
| Ingest options (threads, process timeout) | `IngestOptionsPanel` › Settings | Partial | `JobSettings.threads` exists; no UI. |
| Run Ingest Modules wizard on existing data source | `runIngestModuleWizard/*` | Missing | — |
| Per-module settings panels (job + global) | `IngestModuleIngestJobSettingsPanel`, `IngestModuleGlobalSettingsPanel` | Partial | Hash Lookup only. |
| Ingest inbox (messages, unique-key grouping, details) | `IngestMessageTopComponent`, `IngestMessagePanel` | Partial | Inbox exists (capped errors) and is shown in the progress window. No toolbar inbox with unread count, no grouping and no "view content" link. |
| Ingest progress snapshot (per-thread activity, module times) | `IngestProgressSnapshotPanel` | Done | `analysis.rs` `snapshot_ui`. |
| Cancel ingest (whole job; per-module cancel) | `IngestJob.cancel`, progress cancel | Partial | Whole-job cancel only. |
| Ingest history (jobs + module versions) | `ingest_jobs`, DSS Ingest History tab | Partial | Recorded; not displayed. |
| Ingest job events (STARTED/COMPLETED/DATA_ADDED…) | `IngestManager.IngestJobEvent` | Partial | Progress polling only. |
| Warning: adding a DS / closing case while ingest running | `IngestRunningCheck` | Missing | — |

## 13. Core ingest modules (`A/modules/*`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Hash Lookup (known/notable sets, calculate MD5 w/o sets, inbox per hit, TSK_HASHSET_HIT) | `modules/hashdatabase/HashDbIngestModule` | Done | `pm-modules/src/hash_lookup.rs`, with 4 end-to-end tests. |
| Hash set formats: kdb, NSRL RDS v2/v3, md5sum, HashKeeper, EnCase, idx | `hashdatabase/*Parser`, TSK | Done | `pm-hashdb`, with tests. Port of the 08 `test/input` idx fixtures still owed. |
| Hash Sets options panel (create, import, index, delete, add hashes dialog, import into CR) | `HashLookupSettingsPanel`, `HashDbCreateDatabaseDialog`, `HashDbImportDatabaseDialog`, `AddHashValuesToDatabaseDialog` | Partial | "Add Hash Set…" exists in the wizard only. No global manager, create, add-hashes or CR import. |
| File Type Identification (Tika-equivalent) | `modules/filetypeid` | Done | Matches Tika 3.0.0 on 10 861 test files; ~23× faster. |
| User-defined file types (Options › File Types) | `filetypeid/FileTypeIdGlobalSettingsPanel`, `AddFileTypeSignatureDialog` | Partial | Engine done; Options › File Types UI missing. |
| Extension Mismatch Detector (+ options panel table) | `modules/fileextmismatch` | Partial | Module done; options table UI missing. |
| Embedded File Extractor (archives, Office, PDF images, zip bomb, password detection) | `modules/embeddedfileextractor` | Done | Password prompt UI missing. |
| Picture Analyzer: EXIF → TSK_METADATA_EXIF + USER_CONTENT_SUSPECTED | `modules/pictureanalyzer/impls/EXIFProcessor` | Partial | Done; tests owed (no end-to-end ingest test). |
| Picture Analyzer: HEIC/HEIF → JPEG derived files | `pictureanalyzer/impls/HEICProcessor` | Missing | Needs derived files and a HEIC decoder (external). |
| Encryption Detection (entropy, BitLocker/VeraCrypt/SQLCipher, protected Office/PDF/Access) | `modules/encryptiondetection` | Done | — |
| Interesting Files Identifier (+ rule-set editor, import/export XML) | `modules/interestingitems` | Partial | Module and FilesSet engine done; rule-set editor UI missing. |
| Data Source Integrity (verify/compute image hashes, TSK_VERIFICATION_FAILED) | `modules/dataSourceIntegrity` | Partial | Module done; DSP hash-entry UI missing (§2). |
| PhotoRec Carver | `modules/photoreccarver` | Done | 17 of PhotoRec's ~300 types (native carver). |
| Scalpel Carver (disabled in Autopsy) | `ScalpelCarver/` | N/A? | Not registered in Autopsy. |
| Central Repository ingest module | `centralrepository/ingestmodule` | Missing | §25. |
| Keyword Search ingest module | `KWS/KeywordSearchIngestModule` | Done | §15. |
| Email Parser | `TB/*` | Partial | MBOX, EML, vCard on pm-plugin-api (pm-email); PST/OST missing. |
| Recent Activity | `RA/*` | Partial / In progress | §14. |

## 14. Recent Activity (`RA/`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| RA framework (ordered extractors, temp copies, inbox lines, re-run idempotence) | `RAImageIngestModule`, `Extract` | Partial | `recent_activity/mod.rs`: 2 steps (Chromium, IE), idempotent. Not done: TSK_ASSOCIATED_OBJECT links and download TSK_PATH_ID. Tests owed. |
| Chromium family: history, downloads, cookies, bookmarks, logins, autofill | `Chromium.java` | Partial | Implemented; zero tests (chromium.rs). The browsers track owns the tests. |
| Chromium: favicons, web-form addresses, extensions (+ malicious list), cache | `Chromium.java`, `ChromeCacheExtractor`, `malicious_chrome_extensions.csv` | Done | — |
| Internet Explorer: index.dat history, Favorites, cookies | `ExtractIE.java` (pasco) | Partial | Native parser plus 3 unit tests. Not done: pasco2 test ports (08 §14) and a diff against pasco. Browsers track. |
| Firefox (places, cookies, downloads, form history, logins, bookmarks) | `Firefox.java` | Done | — |
| Safari (history, bookmarks, downloads plist, binary cookies) | `ExtractSafari.java`, `BinaryCookieReader` | Done | — |
| Edge legacy / IE10+ WebCache (ESE) | `ExtractEdge.java` | Missing | Needs an ESE reader (drafted, not merged). |
| Web search queries (SEUQA mappings) | `SearchEngineURLQueryAnalyzer`, `SEUQAMappings.xml` | Partial | Implemented with 1 unit test. |
| Public suffix list domain extraction | `Util.extractDomain` / CR normalizer | Done | — |
| Web account types (admin/mod/user roles) | `ExtractWebAccountType.java` | Missing | Not in the browsers scope as listed. |
| Domain categorisation (default CSV, priority categoriser, custom categories) | `DomainCategoryRunner`, `DefaultDomainCategorizer`, `default_domain_categories.csv` | Missing | Untracked. |
| Custom Web Categories options panel (import/export) | `A/url/analytics/domaincategorization/*` | Missing | Untracked. |
| Windows Registry analyses (OS info, USB + USB ID DB, installed programs, MRUs, shellbags, BAM, Bluetooth, WiFi, network drives, WSL, SAM accounts) | `ExtractRegistry.java`, `ShellBagParser`, `UsbDeviceIdMapper` | Partial | Merged and tested on generated data; real-image verification pending. |
| Recycle Bin ($I/$R, re-created deleted file) | `ExtractRecycleBin.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| Jump Lists (AutomaticDestinations → derived LNKs) | `ExtractJumpLists.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| LNK recent documents | `RecentDocumentsByLnk.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| Prefetch | `ExtractPrefetch.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| SRUM | `ExtractSru.java` | Missing | — |
| Zone.Identifier → downloads | `ExtractZoneIdentifier.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| Data source usage | `DataSourceUsageAnalyzer.java` | Partial | Merged and tested on generated data; real-image verification pending. |
| OS detection by marker files (macOS, Android, Linux distros) | `ExtractOs.java` | Missing | Not explicitly in any track; Data source usage depends on it. Confirm with windows-artifacts. |
| Thumbcache extraction (derived thumbnails) | `ExtractThumbcache.java` | Missing | Not in the windows-artifacts list. |

## 15. Keyword search & text (`KWS/`, `A/textextractors`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Text extraction: plain/HTML/PDF/Office/RTF/SQLite/strings fallback, artifact text, metadata → TSK_METADATA | `A/textextractors/*`, Tika | Done | pm-text. |
| Indexing (chunking, Tantivy), inline search during ingest | `Chunker`, `Ingester`, `InlineSearcher` | Done | Artifact text is indexed in the data-source stage. |
| Keyword lists (built-ins: phone, IP, email, URL, credit card), exact/substring/regex, TSK_KEYWORD_HIT | `KeywordSearchList`, `XmlKeywordSearchList` | Done | — |
| Keyword list import/export (XML, EnCase) | `EnCaseKeywordSearchList` | Partial | Engine done; Options › Keyword Search lists UI missing. |
| Credit cards (Luhn, IIN/BIN DB, TSK_ACCOUNT) | `CreditCardValidator`, `datamodel/ranges.csv` | Done | BIN tree view (§7) missing. |
| Language detection / Japanese (CJK) analysers | `LanguageDetector`, `LanguageSpecificContent*` | Missing | Not implemented. |
| File-name search index (Tantivy n-gram) | `PLAN.md` scale track | Done | Used by File Search by Attributes is pending. |
| Ad-hoc search UI: toolbar Keyword Search + Keyword Lists dropdowns, results node | `DropdownToolbar`, `DropdownSingleTermSearchPanel`, `DropdownListSearchPanel`, `AdHocSearch*` | Partial | Single-term panel, result tabs, save results done; Keyword Lists dropdown missing. |
| Keyword Search options (lists editor, string extraction scripts, general: NSRL skip, snippets, OCR) | `GlobalListSettingsPanel`, `KeywordSearchGlobalLanguageSettingsPanel`, `KeywordSearchGlobalSearchSettingsPanel` | Missing (UI) | — |
| Keyword search job settings panel | `KeywordSearchJobSettingsPanel` | Missing (UI) | — |
| OCR (Tesseract) for images/PDF | `textextractors/TikaTextExtractor` OCR, `textextractors/configs` | Missing | Not listed in the search scope; `PLAN.md` M5. |
| Extract Unique Words report | `KWS/ExtractAllTermsReport` | Missing | — |
| String extraction for unallocated/binary (script selection) | `coreutils/StringExtract`, `StringsConfig` | Done | — |

## 16. Email, mobile & specialised modules

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| MBOX / EML / PST/OST / vCard parsing, attachments as derived files | `TB/MboxParser`, `EMLParser`, `PstParser`, `VcardParser`, `MimeJ4MessageParser` | Partial | MBOX (streamed, size-capped), EML and vCard done with attachments/photos as derived files; PST/OST missing. |
| Email threading (JWZ) | `TB/EmailMessageThreader` | Done | Union of Message-ID / In-Reply-To / References; deterministic thread ids (Autopsy: random UUIDs). |
| Email accounts & relationships (CommunicationsManager) | `TB/ThunderbirdMboxFileIngestModule` | Done | EMAIL/PHONE/DEVICE accounts and MESSAGE/CONTACT relationships via post_communication. |
| Android Analyzer, 22 app parsers (contacts, call log, SMS, Tango, WWF, Google Maps, browser/cache location, IMO, Xender, Zapya, SHAREit, LINE, WhatsApp, TextNow, Skype, Viber, FB Messenger, Samsung Internet/Opera, OruxMaps, installed apps) | `IPM/android/*.py` | Missing | The email-mobile track was lost; to be redone on pm-plugin-api. |
| GPX Parser | `IPM/GPX_Module` | Done | On pm-plugin-api (pm-geo GPX parser): bookmarks, track segments, routes. |
| DJI Drone Analyzer | `A/modules/drones` | Missing | The email-mobile track was lost; to be redone on pm-plugin-api. |
| YARA Analyzer + YARA options panel (rule sets) | `A/modules/yara`, `yara/ui/*` | Missing | The email-mobile track was lost; to be redone on pm-plugin-api. |
| Virtual Machine Extractor (VMDK/VHD in DS → new data sources + ingest) | `A/modules/vmextractor` | Missing | The email-mobile track was lost; to be redone on pm-plugin-api. |

## 17. External-tool / experimental modules

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| aLEAPP / iLEAPP analyzers (TSV → artifacts + HTML report) | `A/modules/leappanalyzers` | Missing | Untracked. External runner or native ports. |
| Plaso (log2timeline/psort → TSK_TL_EVENT) | `A/modules/plaso` | Missing | Untracked. Optional external. |
| Object Detection (OpenCV Haar) | `EXP/objectdetection` | Missing | Untracked. |
| Volatility memory DSP | `EXP/volatilityDSP` | Missing | Untracked. |
| Malware Scan (Cyber Triage cloud), CT options/licence | `CT/malwarescan/*`, `CT/ctoptions/*`, `CT/ctapi/*` | N/A (decided) | Proprietary cloud service; out of scope (user decision 2026-10-17). |
| Sample/example modules | `A/examples/*`, `pythonExamples/*` | N/A? | Become WASM plugin templates (§32). |

## 18. Timeline (`A/timeline`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Timeline window, event-type hierarchy, time binning | `TimeLineTopComponent`, `zooming/*` | Done | `pm-timeline` with 15 tests (RangeDivision). |
| Counts view (stacked bars, log scale, zoom) | `ui/countsview/*` | Partial | Custom chart in `timeline_chart.rs`; tests owed (no chart widget tests). |
| Details view (clustered stripes, description LOD, expand/collapse, pin, hide descriptions) | `ui/detailview/*` | Partial | Stripes and LOD exist. Missing: expand/collapse sub-clusters, pinned lane, "Hidden Descriptions" pane, bundling. Tests owed. |
| List view | `ui/listvew/*` | Done | Test `timeline_builds_and_lists_events`. |
| Filters: event types, data sources, file types, text | `ui/filtering/*` | Partial | Done. The filter tree lacks Apply/Reset semantics and the "select only/others" menu. |
| Filters: Tags, Hash Hits | `ui/filtering` | Missing | — |
| Events from all artifact types (registry, LNK, prefetch, email, messages, GPS, …) | TSK `TimelineManager` event types | Partial | Web/EXIF events only. No persisted events table (§6). |
| Overview range slider | `ui/TimeLineResultView` | Partial | Present; tests owed. |
| History back/forward, zoom presets | `ViewFrame` | Done | — |
| Add Event (manual, TSK_TL_EVENT) | `actions/AddManualEvent` | Missing | — |
| Snapshot report (HTML + PNG) | `actions/SaveSnapshotAsReport`, `timeline/snapshot` | Missing | — |
| View in Timeline from files/artifacts and from DSS | `actions/ViewFileInTimelineAction`, `ViewArtifactInTimelineAction` | Partial | Files and DSS: yes. Artifacts: no. |

## 19. Communications (`A/communications`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| CVT window (Browse/Visualize tabs) | `CVTTopComponent` | Done | Tools › Communications. |
| Filters (account types, devices, date range, most recent N, limit) | `FiltersPanel` | Partial | Devices, account types, relationship kinds, date range; "most recent N" and result limit missing. |
| Accounts browser table | `AccountsBrowser` | Done | Account, device, type, item count; sortable. |
| Relationship graph (pin/unpin/reset, lock, layouts, zoom, snapshot report) | `VisualizationPanel`, `CommunicationsGraph`, `PinAccountsAction`, `communications/snapshot` | Partial | Radial graph of the selected account and its correspondents (click to follow); pin/lock/layouts/snapshot missing. |
| Relationship browser: Summary, Messages (threads), Call Logs, Contacts, Media | `relationships/*` | Partial | Messages, Call Logs, Contacts with details; Summary and Media tabs and message threads missing. |
| Persona integration in CVT | `CVTPersonaCache`, `PersonaPanel` | Missing | — |
| Data dependency: accounts/relationships tables | TSK `CommunicationsManager` | Done | — |

## 20. Geolocation (`A/geolocation`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Map window with waypoints (bookmark, last known, search, trackpoint, EXIF, custom), routes, tracks, areas | `GeolocationTopComponent`, `MapPanel`, `geolocation/datamodel/*` | Done | walkers + OSM tiles (disk-cached): all waypoint kinds, tracks, routes, areas. |
| Tile sources: online OSM, custom OSM server, offline zip, MBTiles (Options › Geolocation) | `GeolocationSettingsPanel`, `MBTilesFileConnector` | Partial | Online OSM only; custom server, offline zip and MBTiles missing. |
| Filters (data sources, artifact types, time window, untimed waypoints) | `GeoFilterPanel` | Partial | Data sources, most recent N days, untimed waypoints; per-artifact-type filter missing. |
| Waypoint details panel, KdTree selection | `WaypointDetailPanel`, `KdTree` | Done | Click picks the nearest marker (no KdTree needed at this scale; only visible markers are tested). |
| KML export from map / KML report | `report/modules/kml` | Partial | KML report done; export from the map window missing. |

## 21. Discovery (`A/discovery`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| File discovery: Images / Videos / Documents, size buckets, data source, parent folder filters, group by, orderings, paging, thumbnails | `discovery/search/*`, `discovery/ui/*` | Partial | `pm-discovery` (5 tests) plus a window. Types come from extensions, not MIME. Runs on `pm-index`, not the DB. Missing: "Possibly User Created", Hash Set, Interesting Item and Object Detected filters; Past Occurrences/frequency (CR); MD5 de-duplication. |
| Domain discovery (web artifacts → domains, page views, downloads, activity dates, account types, categories) | `DomainSearch*`, `DomainSearchCacheLoader` | Missing | 20 Autopsy tests to port (08 §11). |
| Details pane (file instances, artifacts tabs for domains, document text summary) | `discovery/ui/*DetailsPanel`, `A/textsummarizer` | Missing | — |
| Discovery CR frequency + "previously notable" | `discovery/search/DiscoveryAttributes` | Missing | Depends on CR. |

## 22. Image Gallery (`IG/`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Gallery window, folder (path) groups, thumbnail grid, deleted marking, viewer pop-out | `IG/gui/*` | Partial | `gallery.rs` with a test. Built on `pm-index`. Pictures only; no videos. |
| Group by Hashset / Category / Tags / Camera Make / Model / MIME; group sort (priority, hit density) | `datamodel/grouping/*` | Missing | — |
| Seen / next unseen group, seen by other examiners | `GroupManager`, `NextUnseenGroup` | Missing | — |
| Categories (Project VIC tag set), keyboard 0–5, category summary table, undo/redo | `CategorizeAction`, `UndoRedoManager` | Missing | — |
| Tag group / selected files, Follow Up flag on tiles | `TagGroupAction`, `TagSelectedFilesAction` | Missing | — |
| Hash-hit badges and "Only Hash Hits" list | `HashHitGroupList` | Missing | — |
| Slideshow with video player | `gui/drawableviews/SlideShowView`, `VideoPlayer` | Missing | — |
| Metadata "Details" pane | `MetaDataPane` | Missing | — |
| Persistent drawable DB / thumbnail cache, listening during ingest, Options › Image Gallery | `drawable.db`, `ImageGalleryOptionsPanel` | Missing | The thumbnail cache is in memory only (`thumbs.rs`). |

## 23. Data Source Summary (`A/datasourcesummary`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Types tab (pie, counts by MIME/category, OS, usage) | `ui/TypesPanel` | Partial | Pie by extension category, from `pm-index`. No OS/usage rows. |
| User Activity tab (top programs, recent domains, web searches, devices, accounts) | `ui/UserActivityPanel` | Missing | About 30 Autopsy unit tests to port (08 §10). |
| Analysis tab (hash set, keyword, interesting hits) | `ui/AnalysisPanel` | Missing | — |
| Recent Files tab (recently opened docs, downloads, attachments) | `ui/RecentFilesPanel` | Partial | Postmortem shows recently modified/accessed/created files instead. Autopsy's artifact-based lists are missing. |
| Past Cases tab (CR) | `ui/PastCasesPanel` | Missing | — |
| Geolocation tab (most common/recent cities) | `ui/GeolocationPanel` | Missing | Needs the bundled cities DB. |
| Timeline tab (30-day activity, View in Timeline) | `ui/TimelinePanel` | Done | — |
| Ingest History tab | `ui/IngestJobInfoPanel` | Missing | Data is recorded. |
| Container tab (image paths, sizes, hashes, acquisition, unallocated size) | `ui/ContainerPanel` | Partial | Implemented as `Tab::Container`; check it covers hashes and acquisition details. |
| Case › Data Source Summary dialog (data source list + tabs) | `ui/DataSourceSummaryDialog` | Missing | Only the result-pane variant exists. |
| "Ingest is running" banner | DSS panels | Missing | — |

## 24. File search

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| File Search by Attributes (name, size, dates M/A/C/Cr + zone, known status, MIME, MD5, data source, deleted only) | `A/filesearch/*` | Done | All filters; result tabs. |
| File Search by MD5 Hash (Tools) | `hashdatabase/HashDbSearchPanel`, `HashDbSearchAction` | Missing | Index `files_md5` exists. |

## 25. Central Repository & cross-case (`A/centralrepository`, `A/commonpropertiessearch`, `A/allcasessearch`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| CR database (SQLite/PostgreSQL schema v1.6, orgs, cases, data sources, instance tables per type) | `centralrepository/datamodel/*` | Missing | About 46 test items in 08 §7. |
| Correlation types + normalisers (MD5, domain, email, phone, USB, SSID, MAC, IMEI, IMSI, ICCID, installed program, OS account, account types) | `CorrelationAttributeNormalizer` | Missing | — |
| CR ingest module (save instances, previously notable/seen/unseen) | `ingestmodule/*` | Missing | Needs the data-artifact pipeline. |
| CR event listeners (case/DS added, notable tags → known_bad) | `eventlisteners/*` | Missing | — |
| CR options (enable, DB config, correlation properties, organisations, manage cases) | `optionspanel/*` | Missing | — |
| Other Occurrences content viewer (+ CSV export) | `contentviewer/*`, `application/OtherOccurrences` | Missing | — |
| O column / CR comments in C column / Annotations | `GetSCOTask` | Missing | — |
| Personas (window, create/edit/delete, aliases, metadata, accounts, cases) | `centralrepository/persona/*` | Missing | 9 test ports in 08. |
| CR reference sets as hash sets | `hashdatabase` CR import, `ReferenceSet*` | Missing | — |
| Common Properties search (intra-case + inter-case, file type filters, frequency threshold) | `A/commonpropertiessearch/*` | Missing | About 11 test ports in 08 §8. Intra-case can be done without CR. |
| All Cases search (Search Central Repository) | `A/allcasessearch/*` | Missing | — |

## 26. Reports (`A/report`)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Report wizard (module choice, data source selection, tags/artifact-type selection, file options, progress panel) | `report/infrastructure/*` | Partial | Module, data sources, all/tagged results, result types, file columns/delimiter; progress and cancel. Portable case options missing. |
| Report profiles (saved configurations, used by CLI) | `ReportingConfig`, `ReportingConfigLoader` | Missing | — |
| HTML report | `report/modules/html` | Partial | Autopsy's frameset layout, tables and columns; tagged-image thumbnails page missing. |
| Excel report | `report/modules/excel` | Done | xlsx (rust_xlsxwriter): Summary + one sheet per data type. |
| Files – Text (TSV/CSV of files) | `report/modules/file` | Done | All 13 columns, delimiter choice, UTF-8 with BOM. |
| TSK body file | `report/modules/bodyfile` | Done | For mactime; file-system files. |
| Google Earth KML | `report/modules/kml` | Done | ReportKML.kml + style.kml, Autopsy folders and colours. |
| CASE-UCO JSON-LD | `report/modules/caseuco` | Missing | — |
| Save Tagged Hashes to hash set | `report/modules/taggedhashes` | Done | Into a new or existing .kdb set. |
| Data Source Summary report (XLSX) | `report/modules/datasourcesummaryexport` | Missing | — |
| Portable Case (subset case, tags/interesting items, 7z/zip chunking) | `report/modules/portablecase` | Missing | — |
| Report branding (agency logo, title, footer) | `ReportBranding`, Application options | Missing | — |
| Reports node + DB registration | `datamodel/Reports` | Done | — |

## 27. Options dialog (Tools › Options)

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Options dialog shell | NetBeans options | Done | Tools › Options (View, Tags). |
| Application (logo, temp dir, max log files, heap dump path, JVM/Solr memory) | `AutopsyOptionsPanel` | Missing | JVM/Solr memory is N/A? for Rust. |
| View (keep preferred viewer, hide known/slack in both trees, time zone list, hide others' tags, hide SCO, translated names, page size, group by host) | `ViewPreferencesPanel` | Partial | Theme, local/UTC and hide known in Views only. |
| Ingest (settings, file filters, profiles) | `IngestOptionsPanel` | Missing | — |
| Multi-User | `MultiUserSettingsPanel` | Missing | — |
| Auto Ingest / Case Import | `EXP/configuration`, `EXP/autoingest/CaseImportPanel` | Missing | — |
| Keyword Search | KWS panels | Missing (UI) | — |
| Machine Translation | `texttranslation` | Missing | — |
| Hash Sets | `HashLookupSettingsPanel` | Partial | Wizard only. |
| File Extension Mismatch | `FileExtMismatchSettingsPanel` | Missing (UI) | Engine is in progress. |
| File Types | `FileTypeIdGlobalSettingsPanel` | Missing (UI) | — |
| Interesting Files | `InterestingItemsDefsPanel` | Missing (UI) | — |
| Tags | `TagOptionsPanel` | Done | — |
| External Viewer | `ExternalViewerGlobalSettingsPanel` | Missing | — |
| Central Repository | `EamOptionsPanel` | Missing | — |
| Image Gallery | `ImageGalleryOptionsPanel` | Missing | — |
| Command Line Ingest | `CommandLineIngestSettingsPanel` | Missing | — |
| Geolocation | `GeolocationSettingsPanel` | Missing | — |
| YARA | `YaraRuleSetOptionPanel` | Missing (UI) | — |
| Custom Web Categories | `WebCategoriesOptionsPanel` | Missing | — |
| AI: MCP Server | `A/mcp/McpOptionsPanel` | Missing | — |
| Cyber Triage | `CT/ctoptions` | N/A? | — |

## 28. Shell, menus, help, misc tools

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Main window: tree / results / content panes, menu bar, toolbar, status bar, themes | `core/layer.xml` | Done | Kittest flows. |
| Toolbar parity (Images/Videos, Communications, Geolocation, Timeline, Discovery, Generate Report, Close Case, Ingest Messages, Keyword Lists, Keyword Search) | `layer.xml` | Partial | New/Open/Add DS/back/forward/Timeline/Gallery/Discovery/Pop Out. Missing: Comms, Geo, Report, inbox, keyword dropdowns. |
| Tools menu items (Images/Videos, Communications, Geolocation, Timeline, Discovery, File Search, Search CR, Hash Search, Common Properties, Run Ingest Modules, Generate Report, Python Plugins, Personas, Live Triage, Open Case Folder, Create Logical Imager, Options) | `layer.xml`, `@ActionReference` | Partial | Ingest Progress, Timeline, Gallery and Discovery only. |
| Window › Reset Windows, Result/Content viewer lists | `layer.xml`, `ResetWindowsAction` | Partial | Pop-out list and close-all only. |
| Help: online/offline docs (F1/F2), Ingest Status Details, Performance Diagnostics, Thread Dump, Open Log Folder | `corecomponents/OnlineHelpAction`, `diagnostics/PerformancePanel`, `actions/ThreadDumpAction` | Partial | About only. Ingest status is under Tools. |
| About dialog (versions, system info, verbose logging toggle) | `AboutWindowPanel` | Partial | About exists. No verbose-logging toggle and no version details. |
| Keyboard shortcuts (Ctrl+N, Ctrl+O, Ctrl+B, Ctrl+E, Alt+←/→) | `layer.xml` | Partial | Ctrl/⌘+O adds a DS and Alt+arrows work. Ctrl+O means Open Case in Autopsy. No Ctrl+N/B/E. |
| Logging (rotating logs, max log files, per-case log) | `coreutils/Logger` | Partial | `tracing` to stderr. No case Log/ file or rotation. |
| Services monitor / notifications | `core/ServicesMonitor` | Missing | Multi-user only. |
| Machine/user preferences persistence | `core/UserPreferences`, `machinesettings` | Partial | eframe persistence for UI prefs. |

## 29. Translation, OCR, summaries, misc analysis services

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Machine translation service SPI + Google + Bing providers | `A/texttranslation/*`, `translators/*` | Missing | — |
| Translated file-name column | `AbstractAbstractFileNode` + translation | Missing | — |
| Text summarizer (Discovery document previews) | `A/textsummarizer/*` | Missing | — |
| OCR | KWS/Tika | Missing | See §15. |
| URL/domain categorisation SPI | `A/url/analytics/*` | Missing | See §14. |

## 30. Multi-user, automation, CLI, MCP, health

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| PostgreSQL case DB | TSK | Missing | M11. |
| Coordination service (locks) | `A/coordinationservice/*` | Missing | Planned as Postgres advisory locks. |
| Remote event bus | `A/events/RemoteEventPublisher` | Missing | — |
| Collaboration monitor (other users' ingest notifications) | `casemodule/CollaborationMonitor` | Missing | — |
| Multi-user case browser / open dialog / cases dashboard | `casemodule/multiusercasesbrowser/*`, `EXP/autoingest/CasesDashboard*` | Missing | — |
| Shared text index service (Solr replacement) | KWS `Server` | Missing | — |
| Auto ingest (manifests, job queue, nodes, dashboard, control panel, AIN status, file exporter, shared config, case import, delete case) | `EXP/autoingest/*`, `EXP/configuration/*` | Missing | — |
| Health Monitor (metrics DB, dashboard) | `A/healthmonitor/*` | Missing | — |
| Command-line ingest (create case, add DS, run ingest w/ profile, list DS, reports, list profiles, JSON outputs) | `A/commandlineingest/*` | Missing | The `postmortem` binary only accepts `[CASE] [IMAGE…]` to open in the GUI. |
| Open case from command line (positional `.aut`) | `CommandLineOpenCaseManager` | Done | `postmortem/src/main.rs`. |
| MCP server (17 read-only tools: case summary, files, artifacts, analysis results, data sources, hosts, DS tree, tags, timeline, OS accounts, comms accounts/relationships, file content, children, reports) + stdio bridge | `A/mcp/*`, `Tools/autopsy-mcp-stdio` | Done | pm-mcp: the 17 tools plus search/pivot/timeline tools, stdio and HTTP (ADR-021). |
| Feature access / admin gating | `A/featureaccess`, auto-ingest admin markers | Missing | — |

## 31. Acquisition & triage tools

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Image Writer (VHD copy of local disk while ingesting) | `A/imagewriter/*` | Missing | — |
| Make Live Triage Drive | `A/livetriage/*` | Missing | Probably N/A? in its Autopsy form (copies the Java install); a postmortem portable build would replace it. |
| Create Logical Imager (rule config wizard, JSON config) | `A/logicalimager/configuration/*` | Missing | — |
| Logical Imager results DSP | `A/logicalimager/dsp/*` | Missing | See §2. |

## 32. Plugins

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| Third-party ingest/report/DSP/content-viewer plugins (Jython, NBM) | `A/python/*`, `pythonExamples/*` | Partial | ADR-022: `pm-plugin-api` + `pm-plugin-wasm` + `pm-plugin-sdk`; file and data-source ingest plugins work end to end (example: Autopsy's Big and Round File Finder). Report, DSP and content-viewer plugin kinds are not yet in the WIT world. |
| Tools › Python Plugins folder, Plugins manager | `OpenPythonModulesFolderAction` | Done | Tools › Plugins: folder, open, reload, loaded plugins and load errors. |

## 33. Test & quality infrastructure parity

| Feature | Autopsy source | Status | Notes / what is missing |
|---|---|---|---|
| 126-item test parity checklist | `docs/inventory/08-autopsy-tests.md §20` | Missing | 0 items ticked. Some are effectively covered (hash formats, ingest filter basics) but unticked. |
| Golden DB-dump regression harness (tskdbdiff equivalent) | `test/script/tskdbdiff.py`, `regression.py` | Missing | Includes `postmortem dump`, normalisers, compare/rebuild. |
| Integration-test framework (YAML configs, summary outputs) | `A/integrationtesting/*` | Missing | — |
| Synthetic artifact generators (custom types, interesting artifacts) | `A/test/*` | Missing | — |
| Fuzz targets per binary parser | — | Partial | volume, FAT, NTFS, exFAT, EWF. Missing: ext, hashdb formats, IE `index.dat`, EXIF. |
| cargo-deny in gate | `PLAN.md` | Missing | — |

---

## Untracked work packages, ordered by value to an examiner

Each package fits one agent. Every UI package touches `pm-ui`, which `RULES.md` reserves for the coordinator. Either the coordinator does these, or it hands `pm-ui` ownership to one UI agent at a time.

**WP-0 (blocker, do first): case DB schema v2.**
- Scope: migration `0002` adding:
  - hosts ↔ persons
  - OS accounts / realms / instances / attributes
  - communication accounts + account_relationships (with device ids)
  - timeline events + event descriptions with tagged/hash-hit flags
  - persisted aggregate scores, maintained on artifact and tag writes
  - artifact tags API
  
  Also add typed query APIs, and data-artifact + analysis-result ingest pipelines in `pm-ingest`.
- Key sources: TSK schema notes in inventory 01 §11; `A/datamodel/OsAccounts`, `ScoreContent`; `A/ingest/DataArtifactIngestPipeline`.
- Dependencies: must coordinate with derived-and-carving, which owns migrations and `pm-ingest`. Unblocks windows-artifacts (SAM → OS accounts), email-mobile (relationships), search (artifact indexing), and WP-8/9/10/15.

**WP-1: Tagging end to end.**
- Scope:
  - Add/replace/remove file and result tags with comments; Bookmark (Ctrl+B).
  - Tags tree node (File/Result tags, per-examiner filter).
  - Tag name management (Options › Tags, notable flag, tag sets).
  - Notable tag → score.
  - Annotations content viewer.
- Key sources: `A/actions/*Tag*`, `A/casemodule/services/TagsManager`, `TagNameDefinition`, `TagOptionsPanel`, `A/datamodel/Tags`, `A/contentviewers/annotations`.
- Dependencies: WP-0 (artifact tags, score).

**WP-2: Result browsing parity.**
- Scope:
  - Analysis Results/Data Artifacts subtrees: keyword list/term, hash set, interesting set, email account/folder, accounts by type and credit-card BIN/file.
  - Data Artifacts content viewer with Message, Contact, Call Log and General Purpose sub-viewers; Analysis Results viewer; Context viewer.
  - S/C columns and the Score node.
  - Review status approve/reject plus hide rejected.
  - Full file and artifact column sets; column chooser and persisted layouts.
  - Save Table as CSV / export selected rows.
  - Thumbnail result tab.
  - Navigation: View in Directory, View Source Result, View Associated Content, View Artifact.
  - Removing the `MAX_ROWS` cap via keyset paging.
- Key sources: `A/datamodel/*` (KeywordHits, HashsetHits, InterestingHits, EmailExtracted, accounts/Accounts), `A/contentviewers/artifactviewers/*`, `analysisresults/*`, `contextviewer/*`, `A/corecomponents/DataResultViewerTable/Thumbnail`, `A/directorytree/ExportCSVAction`, `ViewContextAction`.
- Dependencies: WP-0. Must also take the UI for the outputs of the in-progress tracks.

**WP-3: UI integration of the six running tracks.**
- Scope:
  - Settings panels for File Type ID (user types), Extension Mismatch, Interesting Files rule editor, Encryption Detection, EFE, carving, YARA rule sets.
  - Keyword Search: toolbar search + lists dropdowns, options, job settings, Extracted Text viewer with hit navigation.
  - Email/Android result rendering.
  - Registry viewer (rejview).
  - MIME tree.
  - Ingest inbox parity (toolbar count, grouping).
- Key sources: the options panels listed in §27; `KWS/Dropdown*`, `ExtractedTextViewer`; `A/contentviewers/WindowsRegistryViewer`, `A/rejview/*`.
- Dependencies: each track merging.

**WP-4: Reports.**
- Scope:
  - Report wizard: modules, DS selection, tag/artifact selection, progress.
  - Report profiles and branding (logo/title/footer).
  - Modules: HTML, Excel, Files-Text, body file, KML, CASE-UCO, Save Tagged Hashes, DSS XLSX, Portable Case (+ Unpack and Open Portable Case).
  - Reports tree node (open/delete).
  - Timeline snapshot and CVT snapshot reports.
  - Extract Unique Words.
- Key sources: `A/report/infrastructure/*`, `A/report/modules/*`, `A/casemodule/UnpackagePortableCase*`, `KWS/ExtractAllTermsReport`, `A/datamodel/Reports`.
- Dependencies: WP-1 (tags). Insta snapshot tests.

**WP-5: Data source processors.**
- Scope:
  - Logical files/folders DSP (LocalFilesDataSource, virtual dirs).
  - L01/Lx01 + Ex01 (EWF2) reader.
  - Local disk DSP (read-only devices, cross-platform enumeration).
  - Unallocated-space image DSP.
  - DSP choice page, host selection page, DSP hash-entry fields, sector size.
  - Remove data source UI, missing-image relocate dialog, Run Ingest Modules on an existing data source.
  - Slack files.
- Key sources: `A/casemodule/LocalFilesDSProcessor`, `AddLocalFilesTask`, `services/FileManager`, `LocalDiskDSProcessor`, `A/datasourceprocessors/RawDSProcessor`, `AddImageWizardSelectDsp/SelectHost*`, `MissingImageDialog`, `A/ingest/runIngestModuleWizard/*`.
- Dependencies: WP-0 (hosts).

**WP-6: Virtual and container image formats.**
- Scope: VMDK (monolithic/split sparse, flat, descriptor), VHD (fixed/dynamic/differencing), VHDX, QCOW2, optional DMG/AFF4. Each with fuzz targets and differential tests (qemu-img/libvhdi as oracles).
- Key sources: `GeneralFilter.VIRTUAL_MACHINE_EXTS`; `docs/inventory/07-rust-ecosystem.md`.
- Dependencies: none. Unblocks the VM Extractor (email-mobile).

**WP-7: Apple and optical file systems.**
- Scope:
  - HFS+/HFSX: catalog/extents B-trees, resource forks, decmpfs compression, hard links.
  - APFS: container/pool nodes, volumes, snapshots, encrypted-volume detection.
  - ISO9660 (+Joliet/Rock Ridge) and UDF.
  - APM volume system.
  - Each with fls differentials and fuzz.
- Key sources: TSK behaviour; `A/datamodel/PoolNode`.
- Dependencies: none.

**WP-8: Ingest framework parity.**
- Scope:
  - Ingest profiles; custom file ingest filters on the FilesSet engine, with ports of the 7 `IngestFileFiltersTest` cases.
  - Ingest options (threads, module timeouts).
  - Run Ingest Modules wizard.
  - Per-module cancel, ingest job events, "ingest running" guards.
  - Ingest history display.
- Key sources: `A/ingest/profile/*`, `IngestOptionsPanel`, `ProfileSettingsPanel`, `FilesSetsManager`, `runIngestModuleWizard/*`, `actions/IngestRunningCheck`.
- Dependencies: the FilesSet engine (core-file-modules); `pm-ingest` ownership (derived-and-carving).

**WP-9: File search tools.**
- Scope:
  - File Search by Attributes dialog: name, size, dates + zone, known, MIME, MD5, data source, deleted.
  - File Search by MD5.
  - "Add file(s) to hash set" action.
  - Full Hash Sets manager: create kdb, import, index, delete, add-hashes dialog.
  - External viewer action + rules (with ports of the 18 Autopsy tests).
  - Extract folders/multi-select; Extract Unallocated Space.
  - Image/Volume/FS Details panels.
- Key sources: `A/filesearch/*`, `A/modules/hashdatabase/*Dialog*`, `HashDbSearchPanel`, `A/directorytree/ExternalViewer*`, `ExtractAction`, `ExtractUnallocAction`, `*DetailsPanel`.
- Dependencies: none (WP-0 optional).

**WP-10: OS Accounts, Hosts, Persons UI.**
- Scope:
  - OS Accounts node and OS Account content viewer.
  - Manage Hosts dialog, merge hosts, associate with person; persons add/edit/delete.
  - Group by Person/Host tree layout and the >5-DS grouping prompt.
- Key sources: `A/datamodel/OsAccounts`, `A/datamodel/hosts/*`, `A/datamodel/persons/*`, `A/contentviewers/osaccount/*`, `AutopsyTreeChildFactory`.
- Dependencies: WP-0, windows-artifacts.

**WP-11: Remaining content viewers.**
- Scope:
  - HTML viewer (sanitised), PDF viewer, SQLite viewer (tables/paging/epoch columns/CSV), binary plist viewer, audio/video playback.
  - Image viewer rotate + EXIF + region tags.
  - Hex selection/copy and external hex editor.
  - Strings script selection.
  - Metadata viewer parity: hashes, MIME, known, object id, istat-style block.
  - Keep-preferred-viewer pref.
- Key sources: `A/contentviewers/{HtmlViewer,PDFViewer,SQLiteViewer,PListViewer,MediaFileViewer,MediaPlayerPanel,Metadata}`, `imagetagging/*`, `A/corecomponents/DataContentViewerHex`, `A/coreutils/StringExtract`.
- Dependencies: none.

**WP-12: Central Repository and cross-case analysis.**
- Scope:
  - `pm-centralrepo` (SQLite + PostgreSQL) with correlation types, normalisers and the PSL.
  - CR ingest module (file + data-artifact) and event listeners.
  - Other Occurrences viewer, O column, CR comments.
  - CR options (orgs, cases, correlation properties).
  - Reference sets.
  - Personas window and personas in CVT.
  - Common Properties search (intra-case first), All Cases search.
  - DSS Past Cases tab; Discovery frequency.
  - About 60 test ports from 08 §7–8.
- Key sources: `A/centralrepository/**`, `A/commonpropertiessearch/*`, `A/allcasessearch/*`.
- Dependencies: WP-0 (data-artifact pipeline, OS accounts, comms accounts), the browsers PSL.

**WP-13: Communications and Geolocation.**
- Scope:
  - CVT: filters, accounts browser, relationship graph with pinning/layout, Summary/Messages/Calls/Contacts/Media browsers, snapshot.
  - Geolocation map: online OSM + offline MBTiles/zip tiles, all waypoint kinds, tracks/routes/areas, filters, details panel, KML export.
- Key sources: `A/communications/**`, `A/geolocation/**`.
- Dependencies: WP-0 (relationships), email-mobile (accounts, GPS artifacts), WP-4 (KML report).

**WP-14: Timeline completion.**
- Scope:
  - Persisted events table populated at load and by every module (all TSK event types incl. registry, LNK, prefetch, email, messages, GPS).
  - Tags/hash-hit filters; filter Apply/Reset semantics.
  - Details expand/collapse/pin/hide-descriptions.
  - Add Event, Snapshot report, View Artifact in Timeline.
  - Tests owed for the chart, Details and artifact events.
- Key sources: `A/timeline/**`, TSK `TimelineManager` event-type list.
- Dependencies: WP-0 (events table), WP-1 (tags).

**WP-15: Data Source Summary + Discovery + Image Gallery completion.**
- Scope:
  - DSS: User Activity, Analysis, Recent Files (artifact-based), Geolocation (cities DB), Ingest History tabs; Case › DSS dialog; ingest-running banner; port about 30 DSS unit tests.
  - Discovery: domain discovery (+20 tests), MIME-based types, hash/interesting/object/user-created filters, MD5 de-duplication, details pane, text summarizer.
  - Gallery: groupings (hash set, category, tags, camera, MIME), seen/next-unseen, Project VIC categories + shortcuts + undo, tag actions, hash badges, slideshow + video, details pane, persisted thumbnail cache, options.
  - Move all three off `pm-index` onto indexed DB queries (ADR-014).
- Key sources: `A/datasourcesummary/**`, `A/discovery/**`, `A/textsummarizer`, `IG/**`.
- Dependencies: WP-0, WP-1, most modules.

**WP-16: Options dialog and preferences.**
- Scope:
  - Tools › Options shell with all categories (§27).
  - Application: logo, temp dir, log retention, case log files.
  - View: hide known/slack in both areas, arbitrary display time zone, page size, keep preferred viewer, hide SCO, translated names, group by host.
  - Help menu: Open Log/Case/Output folders, diagnostics, verbose logging in About, docs links.
  - Keyboard shortcuts (Ctrl+N/O/B/E).
  - Reset Windows.
  - Delete Case, Case Details dialog, close-while-ingesting guard.
- Key sources: `A/corecomponents/AutopsyOptionsPanel`, `ViewPreferencesPanel`, `AboutWindowPanel`, `A/diagnostics/PerformancePanel`, `A/casemodule/CaseDeleteAction`, `CaseInformationPanel`.
- Dependencies: none.

**WP-17: Unassigned analysers.**
- Scope:
  - Web account types; domain categorisation (+ default CSV, priority categoriser, Custom Web Categories options).
  - OS detection by marker files (if not taken by windows-artifacts); Thumbcache extraction.
  - HEIC → JPEG in Picture Analyzer.
  - Machine translation provider interface (Google/Bing) + Translation viewer + translated names.
  - OCR (Tesseract/ocrs) if the search track does not take it.
- Key sources: `RA/ExtractWebAccountType`, `DomainCategoryRunner`, `default_domain_categories.csv`, `A/url/analytics/**`, `RA/ExtractOs`, `RA/ExtractThumbcache`, `A/modules/pictureanalyzer/impls/HEICProcessor`, `A/texttranslation/**`.
- Dependencies: browsers, derived files.

**WP-18: Command line, MCP and golden-dump harness.**
- Scope:
  - `pm-cli`: create case, add DS with DSP ordering, run ingest with a profile, list DS/profiles, generate reports with a profile, JSON outputs.
  - `pm-mcp` with the 17 tools (case summary, files, artifacts, results, data sources, hosts, DS tree, tags, timeline, OS/comms accounts, file content, children, reports) + stdio transport.
  - `postmortem dump` golden harness with normalisers, compare/rebuild, regression profile (08 §18).
- Key sources: `A/commandlineingest/*`, `A/mcp/*`, `Tools/autopsy-mcp-stdio`, `test/script/tskdbdiff.py`.
- Dependencies: WP-4 (reports), WP-8 (profiles).

**WP-19: Remaining volume systems and file systems.**
- Scope: BSD disklabel, Sun VTOC, LVM pools, BitLocker unlock (password/recovery key, CLI `--key`), NTFS extras (owner SIDs, reparse points, EFS flag, WOF, hard links, $UsnJrnl/$LogFile), UFS, XFS, Btrfs, YAFFS2, ext fuzz target.
- Key sources: TSK behaviour; `ImageFilePanel` password; `PLAN.md` M1.
- Dependencies: none.

**WP-20: External-tool modules and alternative DSPs.**
- Scope: aLEAPP/iLEAPP runner (TSV → artifact mapping table), Plaso runner, Volatility memory DSP, object detection, XRY DSP, Logical Imager config wizard + results DSP, image writer (VHD copy), live-triage equivalent.
- Key sources: `A/modules/leappanalyzers`, `A/modules/plaso`, `EXP/volatilityDSP`, `EXP/objectdetection`, `A/datasourceprocessors/xry`, `A/logicalimager/**`, `A/imagewriter`, `A/livetriage`.
- Dependencies: WP-5/6.

**WP-21: Multi-user and auto ingest.**
- Scope:
  - PostgreSQL back end for `pm-casedb` (parity tests on both).
  - `pm-server`: locks, event bus via LISTEN/NOTIFY, shared index service, collaboration monitor.
  - Multi-user case browser, Multi-User options.
  - Auto ingest: manifests, queue, workers, dashboards, file exporter, shared config, case import.
  - Health monitor.
- Key sources: inventory 06; `A/coordinationservice`, `A/events`, `A/casemodule/multiuser*`, `EXP/autoingest/**`, `EXP/configuration/**`, `A/healthmonitor`.
- Dependencies: most of the above.

**WP-22: WebAssembly plugin system.**
- Scope: `pm-plugin-api` (WIT world: ingest file/DS, report, content viewer), wasmtime host with limits, plugin manager UI, Rust template, examples ported from `pythonExamples/`.
- Key sources: `A/python/*`, `pythonExamples/*`, ADR-013, inventory 03 §4.
- Dependencies: a stable ingest/report API.

**Decided 2026-10-17:** Cyber Triage (`CT/**`) is out of scope. External-tool runners (aLEAPP/iLEAPP, Plaso, Volatility; part of WP-20) are out of scope; the valuable LEAPP parsers are ported natively in Rust instead.
