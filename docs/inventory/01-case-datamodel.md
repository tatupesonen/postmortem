# 01 — Case Management & Core Data Model: Feature Inventory

Source tree: `/home/tatu/slop/autopsy/Core/src/org/sleuthkit/autopsy/` (abbreviated `A/` below).
Scope: `casemodule`, `datasourceprocessors`, `datamodel`, `directorytree`, `events`, `tags`, `actions`, `menuactions`, `filesearch`, `core`, `coreutils` (+ `machinesettings`, `featureaccess`, `guiutils/RefreshThrottler` where they define behaviour of the above).

Conventions used in this document:

* **[seen]**: copied from Autopsy source. **[inferred]**: reconstructed from usage or from knowledge of The Sleuth Kit (TSK) 4.15 Java bindings, which are *not* in the repo. Verify [inferred] numeric IDs before relying on them for import/compat.
* "TSK" means the `org.sleuthkit.datamodel` library (SleuthkitCase, Blackboard, TaggingManager, and so on). Autopsy delegates almost all persistence to it. Postmortem must reimplement TSK's behaviour as well as Autopsy's.
* UI labels are given verbatim in quotes.
* Sizes are reported in bytes, or in the decimal/binary units Autopsy itself uses (the code is inconsistent, and both are noted).

---

## 1. Case lifecycle (`A/casemodule/Case.java`, `CaseMetadata.java`, `CaseDetails.java`)

### 1.1 Core concepts

* **CaseType** enum: `SINGLE_USER_CASE("Single-user case")` and `MULTI_USER_CASE("Multi-user case")`. The string value is persisted in the `.aut` file. `fromString` is case-insensitive.
* **Case name vs display name.**
  * The *display name* is what the user typed. It is validated by `Case.isValidName`, which rejects any of `\ / : * ? " < > |`.
  * The *unique name* (`Case.getName()`, shown as "Case UUID" in the details panel) is derived by `displayNameToUniqueName`:
    1. Replace non-ASCII characters with `_`.
    2. Replace control characters with `_`.
    3. Replace any of ` /?:'"\` with `_`.
    4. Lowercase the result.
    5. Append `_` followed by `yyyyMMdd_HHmmss` (the creation time).
  * The unique name is used as the remote-event channel prefix, the temp dir name and the PostgreSQL DB name basis.
* **Only one case can be open per process.** The static `currentCase` holds it. All open, close and delete operations are serialized by `caseActionSerializationLock`. Opening a case while another is open first closes the current one; a close error is shown but does not abort the open.
* **Case directory layout.** Created by `Case.createCaseDirectory(path, type)`, which fails if the path exists as a file or is not read/writable:
  ```
  <caseDir>/
    <DisplayName>.aut            case metadata (XML), §1.7
    autopsy.db                   single-user TSK case DB (SQLite), SINGLE_USER_CASE_DB_NAME
    [<hostname>/]                multi-user only: per-host output root (NetworkUtils.getLocalHostName())
      Export/                    default target for Extract/Export CSV
      Log/                       case logs (the app log is redirected here while the case is open)
      Cache/                     e.g. Cache/thumbnails/<objId>.png
      ModuleOutput/              per-module output (e.g. ModuleOutput/L01, ModuleOutput/Image Writer, ModuleOutput/LogicalImager)
      Reports/
      Config/                    case-scoped settings, e.g. CasePreferences.properties
      Temp/                      only when the temp-dir choice is CASE, see below
  ```
  * `getOutputDirectory()` returns `caseDir` (single-user) or `caseDir/<hostname>` (multi-user), creating it on demand.
  * `getCacheDirectory`, `getExportDirectory`, `getLogDirectoryPath`, `getReportDirectory`, `getConfigDirectory` and `getModuleDirectory` all resolve `<outputDir>/<Folder>` and `mkdirs` lazily.
  * `getModuleOutputDirectoryRelativePath()` returns `ModuleOutput` (single-user) or `<host>/ModuleOutput` (multi-user).
* **Temp directory.** `getTempDirectory()` depends on `UserMachinePreferences.TempDirChoice`, an enum with values `SYSTEM` (the default), `CASE` and `CUSTOM`. The key `TempDirChoice` holds the choice and `TempDirectory` holds the custom path; both live in NbPreferences for module `machinesettings`.
  * SYSTEM: `<java.io.tmpdir>/<AppName>/<caseUniqueName>/Temp`.
  * CASE: `<caseDir>/Temp`.
  * CUSTOM: `<custom>/<AppName>/<caseUniqueName>/Temp`. Falls back to SYSTEM if the custom path is blank.
  * Multi-user cases insert `<hostname>/` before `Temp`.
  * The temp dir is **wiped on open and on close** (`deleteTempfilesFromCaseDirectory`).
* **hasDataSource / hasData flags** (decide whether the tree/result windows open on case open):
  * `dbHasDataSource`: `SELECT count(*) AS count FROM (SELECT * FROM data_source_info LIMIT 1)t`.
  * `dbHasData`: `SELECT SUM(cnt) total FROM (SELECT COUNT(*) AS cnt FROM (SELECT * FROM tsk_objects LIMIT 1)t UNION ALL SELECT COUNT(*) AS cnt FROM (SELECT * FROM tsk_hosts LIMIT 1)r) s`.
  * Both flags are updated on adding a data source, on OS-account/host add or delete, and on report add or delete.
* **Time zones in case**: `getTimeZones()` = `SELECT time_zone FROM data_source_info` (distinct non-empty → `TimeZone`). Used by File Search date filter.

### 1.2 Create case (`NewCaseWizardAction`, `NewCaseWizardPanel1/2`, `NewCaseVisualPanel1/2`, `OptionalCasePropertiesPanel`)

Entry points: "Case > New Case" (shortcut **Ctrl+N**, layer `Shortcuts/D-N`), the startup window "New Case" button, and the toolbar.

The wizard first runs `IngestRunningCheck.checkAndConfirmProceed("Warning: This will close the current case", "Ingest is running. Are you sure you want to close the case?")`.

**Step 1, "Case Information"** (wizard title "New Case Information"):

* Fields:
  * "Case Name:".
  * "Base Directory:" with a [Browse] button (directory chooser, approve text "Select").
  * "Case Type:" radio buttons "Single-User" / "Multi-User".
  * Read-only "Case data will be stored in the following directory:" showing `<parentDir>/<caseName>`. `parentDir` always gets a trailing separator.
* Defaults:
  * The base directory is the last-used one (`ModuleSettings` file `Case`, key `LBL_BaseCase_PATH`).
  * Multi-User is enabled and selected iff `FeatureAccessUtils.canCreateMultiUserCases()` (multi-user mode enabled and the `<userConfig>/mualimit` file absent). Otherwise Single-User is selected and Multi-User is disabled.
* Live validation (red warning label):
  * Multi-user and path on `C:` gives `Warning: Path to multi-user case folder is on "C:" drive`. This is a warning only.
  * Single-user and a UNC path gives `Error: UNC paths are not allowed for Single-User cases`. This disables Next.
  * When running live on a target (`--liveAutopsy`): Windows paths on C: and Linux paths not under `/media|/mnt` produce warnings.
  * Next is enabled when both the name and the dir are non-empty.
* On Next (`validate`):
  * Invalid symbols give `The Case Name cannot contain any of the following symbols: \ / : * ? " < > |`.
  * If `<parent><name>` already exists: `Case directory '{0}' already exists.`
  * If the parent is absolute but missing: confirm dialog "Create directory" / `The base directory "{0}" does not exist. ... create it?`.
  * A non-absolute parent gives `ERROR: The Base Directory that you entered is not valid.`
  * The case directory tree is **created at this step** (`Case.createCaseDirectory`). On failure the partially created directory is deleted.
  * The startup window is closed.
  * Persists `Current_Case_Type` and `LBL_BaseCase_PATH` to `ModuleSettings` "Case".

**Step 2, "Optional Information"** (`OptionalCasePropertiesPanel`):

* Case group: "Number:".
* Examiner group: "Name:", "Phone:", "Email:", "Notes:".
* Organization group (only when the Central Repository is enabled): "Organization analysis is being done for:" combo plus [Manage Organizations], with "Point of Contact" Name/Phone/Email.
* Remembered across sessions in ModuleSettings "Case": `LBL_EXAMINER_NAME`, `LBL_EXAMINER_PHONE`, `LBL_EXAMINER_EMAIL`, `LBL_ORGANIZATION_NAME`. Number and notes are always cleared.

**Finish** (background worker):

1. `Case.createAsCurrentCase(type, dir, CaseDetails(name, number, examiner, phone, email, notes))`.
2. If the Central Repository is enabled: find or create the CR case and set its organization.
3. On success, **automatically launch the Add Data Source wizard**.
4. On failure: show "Cannot create case" with the message, delete the created directory and reopen the startup window.

Cancelling the wizard deletes the created directory.

**`Case.create()` steps.** These run in a single-thread executor named `<caseName>-case-action`, under a modal progress dialog titled "Creating Case" with a Cancel button. Cancellation is checked between steps.

1. "Creating case directory..." (only if the directory does not already exist).
2. "Switching log directory..." (Logger → `<output>/Log`).
3. "Saving case metadata to file..." (writes the `.aut` file).
4. "Creating coordination service node data..." (multi-user only, §1.16).
5. "Creating case database...":
   * Single-user: `SleuthkitCase.newCase(<caseDir>/autopsy.db, null, APP_NAME)` and store `DatabaseName=autopsy.db`.
   * Multi-user: `SleuthkitCase.newCase(displayName, dbConnInfo, caseDir)`. TSK picks a unique PostgreSQL DB name, which is then stored in the `.aut` file.
   * Detects `ConcurrentDbAccessException` (walks the cause chain up to 10 levels) and reports `The case is open in {0}. Please close it before attempting to open it in Autopsy.` (default app name "another application").
6. "Opening case-level services..." creates `Services(caseDb)` = {FileManager, TagsManager, KeywordSearchService, Blackboard} and registers a Guava-EventBus listener on TSK events (§6).
7. "Opening application service case resources...": for every `AutopsyService` plugin (e.g. the Solr text index), run `openCaseResources(ctx)` in its own thread (`<service>-manage-case-resources`) with its own cancellable modal "{0} Opening Case Resources". A failure produces a notification "{0} Error" but does not abort.
8. "Setting up network communications..." (multi-user only) opens the remote event channel `<caseUniqueName>-Case-Events` and starts the CollaborationMonitor.

Any `CaseActionException` triggers `close()` (rollback of whatever was opened), then a rethrow.

**After open (GUI):**

* If TSK upgraded the DB schema (`getBackupDatabasePath() != null`), show an info dialog with the backup path.
* Missing image check:
  * For each image path (`db.getImagePaths()`, first path per image), if neither `File.exists` nor `DriveUtils.driveExists` holds, ask `...images ... are missing. Would you like to search for them now? Previously, the image with host, {0}, was located at: {1} ...`.
  * Yes opens `MissingImageDialog`: browse with the image file filters, then `db.setImagePaths(objId, [newPath])`. The dialog has "Select Image" and "Cancel"; cancelling asks for confirmation.
* Enables the menu actions: Add Data Source, Hosts, Close, Case Details, Data Source Summary, Delete (if permitted), Timeline, Communications, Common Properties, Discovery. "Open Case Folder" is (oddly) disabled.
* Adds the case to Recent Cases.
* Window title: `<displayName> - <AppName> <version>`.
* If `hasData`, open the core windows (tree + result + content viewer); otherwise only the tree.
* Publishes the `CURRENT_CASE` event locally (old=null, new=Case).

### 1.3 Open case (`CaseOpenAction`, `StartupWindowProvider`, `CueBannerPanel`, `OpenRecentCasePanel`, `RecentCases`, `OpenMultiUserCase*`)

* "Case > Open Case" (**Ctrl+O**):
  * When multi-user mode is enabled, it opens the **Open Multi-User Case** dialog, which lists cases from the coordination service (§1.16). That dialog has an "Open Single-User Case..." button.
  * Otherwise a file chooser filtered to `"{AppName} Case File ( .aut)"` opens in the last base dir.
  * After choosing, `LBL_BaseCase_PATH` = the parent of the case directory.
  * Shows the ingest-running warning first. Errors are shown as "Error Opening Case" and the startup window is reopened.
* `Case.openAsCurrentCase(path)`:
  1. Parse the `.aut` (error `Failed to read case metadata:\n{0}.`).
  2. A multi-user case without multi-user settings gives `Multi-user settings are missing (see Tools, Options, Multi-user tab), cannot open a multi-user case.`
* **`Case.open()` steps** (progress title "Opening Case"):
  1. Switch log dir.
  2. Update the coordination node data `lastAccessDate` (multi-user).
  3. Clear temp.
  4. Open the case DB:
     * Content-provider checks, §1.12.
     * Single-user: `SleuthkitCase.openCase(<caseDir>/<DatabaseName>, provider, APP_NAME)`.
     * Multi-user: `openCase(dbName, connInfo, caseDir, provider)`.
     * `TskUnsupportedSchemaVersionException` gives `Unsupported case database schema version`.
  5. Case services.
  6. App services.
  7. Network.
  8. **checkImagePaths**: unless a content provider is set, any `Image` with zero paths fails the open with `The following images had no associated paths: {names}`.
  9. **openFileSystemsInBackground**: a single background thread reads 512 bytes from every `FileSystem` of every image to warm up TSK handles. Skipped if there are more than 100 images (`MAX_IMAGE_THRESHOLD`). Cancelled if another case opens.
* **Startup window** ("Welcome", `CueBannerPanel`): big buttons "New Case", "Open Recent Case", "Open Case", plus [Close]. Plugins can replace it via a `StartupWindowInterface` service.
  * Selection rule: if 2 implementations exist, use the non-default one; if more than 2, use the first not in package `org.sleuthkit.autopsy`.
  * Command-line mode uses `CommandLineStartupWindow` / `CommandLineIngestManager`.
  * On startup, if multi-user is enabled and the Solr host is empty, it shows "Solr 8 Server Not Configured".
  * **Reopen after window reset:** if the file `ResetWindowsAction.getCaseToReopenFilePath()` exists, read the `.aut` path from it, delete the file and open that case.
* **Recent cases** (`RecentCases`):
  * Keeps `LENGTH = 6` slots in ModuleSettings "Case", keys `LBL_RecentCase_Name<i>` / `LBL_RecentCase_Path<i>`.
  * The menu "Case > Open Recent Case" lists the entries (newest first). Only the first 5 are exposed: `getRecentCaseNames()` returns `LENGTH-1` entries.
  * Ends with "Clear Recent Cases"; the label "-Empty-" shows when there are no entries.
  * Entries whose path no longer exists are dropped (`RecentCase.exists`). Opening a missing one shows `Case {0} no longer exists.`
  * Also updated on rename (`updateRecentCase`) and on delete.
  * The "Open Recent Case" dialog (`OpenRecentCasePanel`) is a table with columns "Case Name", "Path" and buttons Open / Cancel.

### 1.4 Close case (`CaseCloseAction`, `Case.closeCurrentCase`)

* "Case > Close Case" and the toolbar button "Close Case" (icon `btn_icon_close_case.png`). Shows the ingest-running warning first.
* Sequence:
  1. Publish `CURRENT_CASE` (old=case, new=null) **before** closing.
  2. Close in the case-action thread (multi-user: under the resources lock). Close steps:
     1. Cancel all ingest jobs (reason `CASE_CLOSED`).
     2. Multi-user: stop the CollaborationMonitor and close the remote channel ("Shutting down network communications...").
     3. Each AutopsyService `closeCaseResources` (own modal "{0} Closing Case Resources"; errors notify "Could not close case resources for {0} service: {1}").
     4. Unregister the TSK listener and `caseDb.close()`.
     5. Delete temp.
     6. Switch the log back to the global log dir.
  3. GUI: close core windows, disable case actions, clear notifications, reset the title, reopen the startup window.

### 1.5 Delete case (`CaseDeleteAction`, `Case.deleteCurrentCase/deleteCase`)

* "Case > Delete Case" is enabled when a case is open and `FeatureAccessUtils.canDeleteCurrentCase()` holds.
* Confirmation: "Delete Current Case?" / "Are you sure you want to close and delete the current case?" (default No).
* Closes the case, then deletes it with the progress title "Deleting Case". Errors: "Failed to Delete Case" / "Error deleting case: {0}".
* **Single-user:**
  1. Delete the text index (every `KeywordSearchService.deleteTextIndex(metadata)`).
  2. Delete the case directory recursively.
  3. Remove it from recent cases.
  * Errors are accumulated, ending with `Errors occured while deleting the case. See the application log for details.`
* **Multi-user:**
  1. Connect to the coordination service.
  2. `tryGetExclusiveLock(CASES, caseDir)`. If unavailable: `Cannot delete case because it is open for another user or host.`
  3. Read CaseNodeData.
  4. Then, idempotently, using the `DeletedFlags` bitmask persisted in the node data after each step:
     * `CASE_DB`: connect to `jdbc:postgresql://host:port/postgres`, then `SELECT 1 from pg_database WHERE datname = '<db>'` and `DROP DATABASE "<db>"`.
     * `TEXT_INDEX`: delete the text index.
     * `CASE_DIR`: delete the directory.
     * Remove it from recent cases.
  5. Delete the coordination nodes `<caseDir>_RESOURCES` (ignoring "KeeperErrorCode = NoNode") and, if there were no errors, the case dir node.
* Cannot delete the current case directly (`Cannot delete current case, it must be closed first.`).

### 1.6 Remove data source (`DeleteDataSourceAction`, `Case.deleteDataSourceFromCurrentCase`)

* Context menu "Remove Data Source" on Image / LocalFilesDataSource nodes. It is enabled iff `FeatureAccessUtils.canDeleteDataSources()`: case DB **creation** schema ≥ 8.4 and (single-user, or no `mualimit` file).
* If ingest is running: `Data sources cannot be removed from a case when ingest is running.`
* Confirmation: `Are you sure you want to remove the selected data source from the case?\nNote that the case will be closed and re-opened during the removal.`
* Algorithm:
  1. Close the case.
  2. Open a fresh `Case` with an **EXCLUSIVE** lock and a non-cancellable progress dialog "Removing Data Source".
  3. Run `open()` fully.
  4. `SleuthkitCaseAdminUtil.deleteDataSource(caseDb, objId)`. TSK deletes the data source's objects, files, artifacts, tags, events and so on.
  5. `KeywordSearchService.deleteDataSource(objId)` (Solr delete-by-query).
  6. Publish `DATA_SOURCE_DELETED(objId)`.
  7. Close.
  8. Reopen the case normally (SHARED lock).
* Errors: `An error occurred while removing the data source:\n{0}...` and `Failed to re-open the case`.
* Reports are not deleted.

### 1.7 Case metadata file `.aut` (`CaseMetadata`)

XML file named `<DisplayName>.aut` in the case directory. The name is fixed at creation; renaming the display name later does **not** rename the file. It is located on open by scanning the directory for a file ending `.aut` (case-insensitive). The writer always writes schema **6.0**, indent 2.

```xml
<AutopsyCase>
  <SchemaVersion>6.0</SchemaVersion>
  <CreatedDate>2024/05/01 12:00:00 (CEST)</CreatedDate>   <!-- format "yyyy/MM/dd HH:mm:ss (z)", Locale.US -->
  <ModifiedDate>...</ModifiedDate>                       <!-- rewritten on every save -->
  <CreatedByAutopsyVersion>4.x.y</CreatedByAutopsyVersion>
  <SavedByAutopsyVersion>4.x.y</SavedByAutopsyVersion>
  <Case>
    <Name>uniquecasename_20240501_120000</Name>
    <DisplayName>My Case</DisplayName>
    <Number/><Examiner/><ExaminerPhone/><ExaminerEmail/><CaseNotes/>
    <CaseType>Single-user case</CaseType>     <!-- or "Multi-user case" -->
    <Database/>                               <!-- legacy absolute DB path; empty in v>=3 -->
    <CaseDatabase>autopsy.db</CaseDatabase>   <!-- relative file name (single) or PostgreSQL db name (multi) -->
    <TextIndex/>                              <!-- legacy Solr core name -->
  </Case>
  <ContentProvider><Name/></ContentProvider>  <!-- optional plugin providing image bytes (§1.12) -->
  <OriginalCase>                              <!-- only populated for portable cases -->
    <CreatedDate>...</CreatedDate>
    <Case>...same children as above, for the source case...</Case>
  </OriginalCase>
</AutopsyCase>
```

Read compatibility:

| Schema | Created-by element | Display name | Examiner phone/email/notes | DB name |
|---|---|---|---|---|
| 1.0 | `AutopsyCreatedVersion` | = `Name` | empty | `DatabaseName`; text index in `TextIndexName` |
| 2.0 | `CreatedByAutopsyVersion` | = `Name` | empty | `Database` (absolute path); `TextIndex` |
| 3.0 | same | `DisplayName` | empty | `CaseDatabase` (relative) + optional `Database` |
| 4.0 to 6.0 | same | `DisplayName` | read | same as 3.0 |

* If the DB name is an absolute path, it is converted to relative against the case directory.
* Required elements: SchemaVersion, CreatedDate, the created-by element, Name, DisplayName (v≥3), CaseType and the DB name. Missing or empty required elements raise "Case metadata file corrupted".
* Every setter (`setCaseDetails`, `setCaseDatabaseName`, `setCreatedDate`, `setCreatedByVersion`) rewrites the file and rolls back the in-memory value on failure.

### 1.8 Case Details dialog (`CaseDetailsAction`, `CaseInformationPanel`, `CaseDetailsPanel`, `EditOptionalCasePropertiesPanel`)

* Opened from "Case > Case Details". It is a modal resizable dialog titled "Case Details".
* Shows:
  * "Case Name:", "Case Number:", "Created Date:", "Case Directory:".
  * "Case UUID:" (the unique name).
  * "Case Type:" (localized "Single-user case" / "Multi-user case").
  * "Database Name:" (full path `<caseDir>/autopsy.db` for single-user, DB name for multi-user).
  * Examiner Name/Phone/Email/Notes.
  * Organization Name and Point of Contact Name/Phone/Email (from the Central Repository).
* Buttons: [Edit Details] opens "Edit Case Details" (same fields as wizard step 2, with "Name:" editable, Save/Cancel); [Close].
* Edit validation: an empty name gives `No case name entered.`; invalid characters give the same list as above.
* `Case.updateCaseDetails`:
  1. Write the `.aut` file.
  2. Multi-user display-name change: update CaseNodeData.displayName.
  3. Publish events. A number change publishes `NUMBER` (old, new). An examiner change is **also published as `NUMBER`** (a bug). A name change publishes `NAME`. `CASE_DETAILS(oldDetails, newDetails)` is always published.
  4. Update the window title and the recent-case entry.

### 1.9 Other casemodule features

* **IngestJobInfoPanel** (used in Data Source Summary "Ingest History"):
  * Jobs table: "Start Time", "End Time", "Ingest Status".
  * Modules table: "Module Name", "Module Version".
  * Data comes from `skCase.getIngestJobs()` and `IngestJobInfo.getIngestModuleInfo()`.
* **CasePreferences** (per case): `<case>/Config/CasePreferences.properties`, key `groupByDataSource=true|false`. The default is false; it is persisted whenever set.
* **Portable case**:
  * `Case.createPortableCase(name, folder)` creates a new single-user case folder with `.aut` metadata that has `OriginalCase` filled, plus an empty `autopsy.db`. The rest is done by the Portable Case report module (out of scope).
  * **Unpack**: "Case > Unpack and Open Portable Case".
    * Dialog fields: "Portable Case:" (`.zip` or `.zip.001`) and "Folder to extract to:".
    * Validation: `File extension must be .zip or .zip.001`, `Folder {0} already exists`, and others.
    * Uses bundled `7-Zip/7z.exe x <archive> -o<outputFolder>` (Windows only), then opens `<out>/<dir>/<dir>.aut`.
    * If the case was previously unpacked: "Case has been previously opened. Open it again?"
* **SingleUserCaseConverter** (used by auto-ingest tooling):
  * Copies a SQLite case into a new PostgreSQL DB table by table, with INSERTs for `blackboard_artifact_types`, `blackboard_attribute_types`, `tsk_objects`, `tsk_image_names`, `tsk_image_info`, `tsk_fs_info`, `tsk_files_path`, `tsk_files`, `tsk_file_layout`, `tsk_db_info`, `tag_names`, `reports`, `blackboard_artifacts`, `blackboard_attributes`, `tsk_vs_parts`, `tsk_vs_info`, `tsk_files_derived`, `tsk_files_derived_method`, `content_tags`, `blackboard_artifact_tags`.
  * DB name deconfliction: `SELECT datname FROM pg_catalog.pg_database WHERE LOWER(datname) LIKE LOWER('<name>%')`.
  * Optionally copies images and rewrites `tsk_image_names` paths.
  * This is an outdated column set (pre-8.x), useful only as a historic reference.
* **AutopsyContentProvider / ContentProviderUtils (§1.12):** a plugin can serve image bytes (e.g. from cloud storage) instead of local files.
* **GeneralFilter**: the file-chooser filters listed in §2.3.

### 1.10 Multi-user infrastructure (`multiusercases/*`, `CollaborationMonitor`, `OpenMultiUserCase*`)

* **Locks (ZooKeeper "coordination service", category `CASES`):**
  * Case directory lock: node name = the case directory path. Open or create take a **SHARED** lock (timeout 1 min); data-source deletion and case deletion take **EXCLUSIVE**. Failures give `Failed to get a shared lock on the case.` / `...exclusive lock...`.
  * Resources lock: node `<caseDir>_RESOURCES`, exclusive, timeout 1 hour. Held for the duration of each open/close action.
  * Other node names: `<caseDir>_AUTO_INGEST_LOG.TXT` and the case-name node (the directory name with the `_yyyy_MM_dd_HH_mm_ss` timestamp suffix removed).
* **CaseNodeData** is a binary blob stored in the case-directory node. Big-endian `DataOutputStream`, version 2.0:
  ```
  int    majorVersion (=2)
  byte   errorsOccurred (0x80 = true)
  UTF    caseDirectory
  long   createDate (epoch ms)
  long   lastAccessDate (epoch ms)
  UTF    caseUniqueName
  UTF    displayName
  short  deletedItemFlags  (TEXT_INDEX=1, CASE_DB=2, CASE_DIR=4, DATA_SOURCES=8, MANIFEST_FILE_NODES=16)
  int    minorVersion (=0)
  ```
  * v0 → v2 is upgraded by re-reading the `.aut`; v1 gets the minor version added.
  * If the data is missing, it is rebuilt from the `.aut` in the directory.
* **Open Multi-User Case dialog:**
  * Columns "Name", "Create Time", "Directory" (also "Last Access Time"), sorted by create time descending.
  * "Select any case and start typing to search by case name".
  * Buttons "Open Selected Case", "Open Single-User Case...", "Cancel".
  * The case list is `CaseNodeDataCollector.getNodeData()`: every `CASES` node except name, resources and log nodes.
  * The admin browser additionally has the columns "Case Database Deleted", "Case Directory Deleted", "Data Sources Deleted", "Manifest Znodes Deleted", "Text Index Deleted".
* **CollaborationMonitor:**
  * Remote channel `<caseName>-Collaboration-Monitor-Events`, event name `COLLABORATION_MONITOR_EVENT`.
  * Payload: `(hostName, Map<taskId, Task{id, status}>)`.
  * Local tasks:
    * On `ADDING_DATA_SOURCE`: status "`{host} adding data source`".
    * On `DATA_SOURCE_ANALYSIS_STARTED`: status "`{host} analyzing {dataSource}`".
    * Tasks are removed on `DATA_SOURCE_ADDED` / `ADDING_DATA_SOURCE_FAILED` / `DATA_SOURCE_ANALYSIS_COMPLETED`.
  * Heartbeat every 1 min (re-publishes current tasks).
  * Remote hosts' tasks are shown as NetBeans progress bars. A host is considered stale after 5 missed heartbeats; the stale check runs every 2 min.
  * Also an `IntervalErrorReport` for "Database Connection Error" notifications ("new issue(s)" / "total issue(s)").
* **Multi-user DB:** PostgreSQL connection from `UserPreferences.getDatabaseConnectionInfo()`, one database per case.
* **FeatureAccessUtils:** a file `<userConfig>/mualimit` (any content) restricts multi-user features: creating multi-user cases, adding or deleting data sources and deleting cases in multi-user cases, adding hash sets to the CR.

### 1.11 AutopsyService plugin lifecycle

`AutopsyService` (in `appservices`) has `getServiceName()`, `openCaseResources(CaseContext)` and `closeCaseResources(CaseContext)`. `CaseContext` carries the case, a progress indicator, an `isNewCase` flag and a cancel-requested flag. Postmortem needs an equivalent trait for the text index, image gallery DB, timeline and other services.

### 1.12 Content providers (`AutopsyContentProvider`, `ContentProviderUtils`)

* The `.aut` `ContentProvider/Name` holds `<baseName>_<major>.<minor>.<patch>`.
* On open, `ContentProviderUtils.getContentProvider(name)` looks for an exact match.
* If none is found:
  * If a provider with the same base name (case-insensitive) is installed, show "Content Provider Load Failed" and fail.
  * Otherwise show "Content Provider Not Found" (`This case requires a content provider plugin ('{0}') that is not installed.`) and fail.
* Compatibility: same base name, same major and minor, installed patch ≥ required patch (also "Content Provider Version Mismatch").
* With a provider, the "images must have paths" check is skipped.

---

## 2. Add Data Source wizard and Data Source Processors (DSPs)

### 2.1 Wizard (`AddImageAction`, `AddImageWizardIterator`, `AddImageWizard*Panel/Visual`)

Entry points: "Case > Add Data Source", the toolbar "Add Data Source" (icon `btn_icon_add_image.png`), automatically after New Case, and the tree context menu.

* Preconditions:
  * It is enabled iff a case is open and `canAddDataSources`.
  * If ingest is running, confirm "Ingest in progress": `Ingest is ongoing on another data source. Adding a new source now might slow down the current ingest. Do you want to proceed...?`
* The dialog is 875×550 px, titled "Add Data Source", with steps "Step {0} of {1}".

Panels in order:

1. **"Select Host"** ("Select Host To Add The Data Source To"). Text: "Hosts are used to organize data sources and other data." Radio buttons:
   * "Generate new host name based on data source name" (default). Returns a null host; TSK creates a host named after the data source plus an ID suffix.
   * "Specify new host name" plus a text field, validated by `HostNameValidator`:
     * empty: "Please provide some text for the host name."
     * duplicate (case-insensitive, sanitized): "Another host already has the same name. Please choose a different name."
   * "Use existing host" plus a list of all hosts. Requires a selection: "Please select an existing host."
2. **"Select Data Source Type"**: a vertical list of toggle buttons, one per DSP. The order is fixed first, then plugins:
   1. "Disk Image or VM File"
   2. "Local Disk"
   3. "Logical Files"
   4. "Unallocated Space Image File"
   5. "Autopsy Logical Imager Results"
   6. then other `DataSourceProcessor` services and Jython DSPs, deduplicated by type string (e.g. "XRY Text Export", "Memory Image File (Volatility)").
   * The last-used DSP is preselected.
   * In multi-user cases "Local Disk" is disabled with the suffix "- This type of Data Source Processor is not available in multi-user mode".
3. **Data source settings**: the selected DSP's `getPanel()`. Next is enabled by `isPanelValid()`; panels fire `DSP_PANEL_EVENT.UPDATE_UI`.
4. **(optional) Ingest profile selection**: only if saved ingest profiles exist. The last profile is stored under key `AIW_LASTPROFILE_NAME`.
5. **"Configure Ingest"**: the ingest module selection panel (owned by the ingest area).
6. **"Add Data Source"** progress panel:
   * Text: "Processing data source and adding it to a local database. File analysis will start when this finishes."
   * Shows the DSP progress text plus a "*This process may take some time for large data sources." progress bar.
   * On completion: "Data source has been added to the local database. Files are being analyzed." with "*Data Source added." (black) or "*Errors encountered in adding Data Source." (red).
   * Non-critical errors: "*Data Source added (non-critical errors encountered). Click below to view the log."
   * Critical errors: "*Failed to add data source (critical errors encountered)..."
   * [View Log] opens "Add Image Log" with Copy/Close.
   * Beeps on Windows.

Navigation:

* Back is allowed only up to the DSP settings panel.
* Processing starts when leaving the ingest-config panel:
  1. `startDataSourceProcessing(dsp, host)` generates a `UUID dataSourceId`.
  2. `Case.notifyAddingDataSource(id)` publishes `ADDING_DATA_SOURCE`.
  3. If `dsp.supportsIngestStream()`, call `runWithIngestStream(host, ingestJobSettings, monitor, callback)`: ingest begins while files are still being added. Otherwise call `run(host, monitor, callback)` and queue the ingest job afterwards: `IngestManager.queueIngestJob(newContents, settings)`.
* Callback `done(result, errors, contents)`:
  * Results are `NO_ERRORS`, `NONCRITICAL_ERRORS` or `CRITICAL_ERRORS`.
  * If `contents` is non-empty, publish `DATA_SOURCE_ADDED(contents[0], id)`; otherwise publish `ADDING_DATA_SOURCE_FAILED(id)`.
  * Errors are logged with the DatasourceID.
* Cancel during processing calls `dsp.cancel()` (via a CleanupTask fired when the wizard closes).

### 2.2 DSP contract (`corecomponentinterfaces.DataSourceProcessor`, `datasourceprocessors/AutoIngestDataSourceProcessor`)

* `getDataSourceType()`: the display string, also used as the identity.
* `getPanel()`, `isPanelValid()`, `reset()`, `cancel()`.
* `run(progress, cb)`, `run(host, progress, cb)`, `run(password, host, progress, cb)`.
* `supportsIngestStream()`, `runWithIngestStream(...)`.
* Callback: `DataSourceProcessorCallback.done(DataSourceProcessorResult, List<String> errors, List<Content> newDataSources)`. It is invoked on the GUI thread (`doneEDT`).
* `DataSourceProcessorProgressMonitor`: `setIndeterminate`, `setProgress(int)`, `setProgressMax`, `setProgressText`.
* **Auto-ingest / command-line extension:**
  * `int canProcess(Path)`: confidence 0 to 100; 0 means cannot. Image DSP returns 100 if the extension matches and TSK can open a file system.
  * `process(deviceId, path, [password], [host], progress, cb)`.
  * `processWithIngestStream(...)`.
  * `DataSourceProcessorUtility.getOrderedListOfDataSourceProcessors(path)` sorts by confidence.
* **Streaming ingest:**
  * `AddDataSourceCallbacks.onDataSourceAdded()`, `onFilesAdded(List<Long> fileObjIds)`.
  * `StreamingAddDataSourceCallbacks` forwards to `IngestStream.addFiles`.
  * `DefaultIngestStream` is a no-op stream used when not streaming.

### 2.3 "Disk Image or VM File" (`casemodule/ImageDSProcessor`, `ImageFilePanel`, `AddImageTask`, `GeneralFilter`)

* File-chooser filters:
  * "All Supported Types"
  * "Raw Images (*.img, *.dd, *.001, *.aa, *.raw, *.bin)"
  * "Encase Images (*.e01)"
  * "Virtual Machines (*.vmdk, *.vhd, *.vhdx)"
  * Matching is a case-insensitive `endsWith`. Physical drives (`\\.\PhysicalDriveN`, `/dev/...`) and partitions (`\\.\X:`) are also accepted.
* Panel fields:
  * "Path:" with [Browse].
  * "Ignore orphan files in FAT file systems" with the note "(faster results, although some data will not be searched)". Default unchecked.
  * "Time zone:": a list of `(GMT+h:mm) Zone/ID` sorted by raw offset then ID. Default is the local zone.
  * "Sector size:" one of `Auto Detect` (0), `512`, `1024`, `2048`, `4096`.
  * "Bitlocker Password (optional):".
  * "Hash Values (optional):" MD5 / SHA-1 / SHA-256 with "NOTE: These values will not be validated when the data source is added." The hash fields are disabled for `.e01`, since E01 carries its own hashes.
* Remembers the last path in ModuleSettings context `<DSP class name>`, key `LBL_LastImage_PATH`.
* `.001` handling: if a sibling `.000` exists, ask "Choose .001 file?" (`The selected file has extenson .001 but there is a .000 file in the sequence...`).
* **Validation** is debounced: 1000 ms after the last keystroke on a scheduled executor, with a "loading..." label while running.
  1. The path must be a file, physical drive or partition.
  2. Hashes must be valid hex of the right length ("Invalid MD5 hash", "Invalid SHA1 hash", "Invalid SHA256 hash").
  3. `SleuthkitJNI.testOpenImage(path, password)` must succeed, otherwise `An error occurred while opening the image:{msg}`.
  4. Multi-user plus C: drive gives a warning only.
* `run()`:
  1. `deviceId = UUID.randomUUID()`.
  2. `SleuthkitJNI.addImageToDatabase(caseDb, [path], sectorSize, tz, md5, sha1, sha256, deviceId, password, host)` creates the `Image` row first.
  3. Then `AddImageTask` runs on a new thread:
     * `caseDb.makeAddImageProcess(tz, addUnallocSpace=true, noFatFsOrphans, imageWriterPath, password)`, then `.run(deviceId, image, sectorSize, callbacks)`.
     * A progress poller every 500 ms shows "Adding: {currentDirectory}".
     * On finish: `image.verifyImageSize()` (warning appended to errors); set MD5/SHA1/SHA256 on the image (failure is critical); start the image writer if configured.
     * Critical errors come from `TskCoreException` during run; non-critical errors come from `TskDataException`s collected during the add.
  4. Cancel calls `AddImageProcess.stop()` and `ingestStream.stop()`.
* Supports ingest streaming.
* Auto-ingest `process()`: tz = local, sector 0, no FAT-orphan skip, no hashes.

### 2.4 "Local Disk" (`LocalDiskDSProcessor`, `LocalDiskPanel`, `LocalDiskSelectionDialog`)

* Fields:
  * "Local Disk:" with [Select Disk] opening "Select Local Disk". The dialog is a modeless table with columns "Disk Name" and "Disk Size", has [Refresh Local Disks], and loads asynchronously ("Loading local disks...", "No Accessible Drives").
  * Error messages: "Disks were not detected. On some systems it requires admin privileges..." and "Some disks were not detected...".
  * "Timezone:", "Sector Size:", "Ignore orphan files in FAT file systems".
  * "Make a VHD image of the drive while it is being analyzed", with a path and [Browse]. Default path: `<ModuleOutput>/Image Writer/<diskName sanitized to [0-9A-Za-z _()], or "localDisk" prefix> <epochMillis>.vhd`.
  * "Update case to use VHD file upon completion".
  * "Note that at least one ingest module must be run to create a complete copy".
  * VHD path errors: "Error - enter path for VHD", "Error - VHD path is a directory", "Error - VHD path already exists", "Error - directory does not exist".
* Disk enumeration (`PlatformUtil.getPhysicalDrives/getPartitions`):
  * Windows: probe `\\.\PhysicalDrive0..N`, giving up after 4 consecutive misses. Partitions come from `File.listRoots()` drive letters as `\\.\C:`.
  * Linux/mac: `/dev/*` names containing `hd`, `sd` or `disk`. Names of at most 5 characters are disks; at most 7 are partitions.
  * Each candidate must be readable (open and read 1 byte). Size comes from `SleuthkitJNI.findDeviceSize`.
* Uses the same `AddImageTask`, with `ImageWriterSettings(path, updateDbPath)`. **No ingest streaming** (uses `DefaultIngestStream`).

### 2.5 "Logical Files" (`LocalFilesDSProcessor`, `LogicalFilesDspPanel`, `LocalFilesPanel`, `LogicalEvidenceFilePanel`, `AddLocalFilesTask`, `services/FileManager`)

* Sub-type combo: "Local files and folders" or "Logical evidence file (L01)".
* Local files panel:
  * A list of selected paths, with [Add] (multi-select files and dirs; chooser title "Select Local Files or Folders"), [Delete] and [Clear].
  * "Logical File Set Display Name: Default" with [Change] to rename the set.
  * "Timestamps To Include:" checkboxes, all unchecked by default:
    * "Modified Time - Often not changed when a file is copied"
    * "Creation Time - Often changed when a file is copied"
    * "Access Time - Can be changed when the file is opened"
  * Note: "NOTE: Time stamps may have changed when the files were copied to the current location."
* `FileManager.addLocalFilesDataSource(deviceId, rootName, tz="", host, paths, crtime?, atime?, mtime?, progress)`:
  1. Validate that every path exists and is readable, otherwise `TskDataException("File at %s does not exist or cannot be read")`.
  2. Root name: if blank, `LogicalFileSet<N+1>`, where N is the count of existing virtual-directory roots starting with `LogicalFileSet`.
  3. One transaction: `caseDb.addLocalFilesDataSource(deviceId, rootName, tz, host, trans)` (creates the `data_source_info` row and a VIRTUAL_DIR root).
  4. Then recursively:
     * Directories: `addLocalDirectory(parentId, name, trans)` (type LOCAL_DIR).
     * Files: `addLocalFile(name, absPath, size, ctime=0, crtime, atime, mtime, isFile, EncodingType.NONE, parent, trans)`. Timestamps are epoch seconds from `BasicFileAttributes`, only for the checked boxes; otherwise 0.
  5. Commit, then fire `ModuleContentEvent` for each top-level added file.
  6. Progress shows "Adding: {parentPath}/{name}" every 10 files.
* L01:
  * Windows only. Runs bundled `ewfexport_exec/{32-bit|64-bit}/ewfexport.exe -f files -t <base>_<epochMillis> <file.l01>` in `<ModuleOutput>/L01`, logging to `<dir>.txt`.
  * The extracted folder is then added as local files.
  * Error: "Only files with the .l01 file extension are supported here."
* Result is `CRITICAL_ERRORS` if any exception occurs; there is no partial success.
* No streaming.

### 2.6 "Unallocated Space Image File" (`datasourceprocessors/RawDSProcessor`, `RawDSInputPanel`, `AddRawImageTask`)

* Fields: "Browse for an unallocated space image file:", "Please select the input timezone:", and "Break image up into:" with "Do not break up" or "2GB chunks" (default chunk size 2,000,000,000 bytes).
* Algorithm:
  1. `addImageToDatabase([path], sectorSize=0, tz, no hashes, deviceId)`. There is no file-system analysis.
  2. Create layout files covering the whole image: `caseDb.addLayoutFiles(image, ranges)`.
  3. Ranges are `TskFileRange(start, len, seq)` in 2 GB pieces if chunking is on and size ≥ 2 GB, plus a leftover; otherwise one range. These become `UNALLOC_BLOCKS`-style layout files that ingest/carving can process in parallel.
* Errors: `Critical error adding <path> for device <id> is not existing.`

### 2.7 "XRY Text Export" (`datasourceprocessors/xry/*`)

* Input: a folder containing XRY `.txt` reports, at depth 1 only.
* Each report file must:
  * be UTF-16LE with BOM `FF FE`;
  * have first line `xry export`, with the report type on line 3 and a 5-line header;
  * contain entities separated by blank lines;
  * use `Key:\tValue` pairs.
* Validation messages: "Please select a folder containing exported XRY text files", "The selected path is not a folder", "Selected path is not readable", "Selected folder did not contain any XRY text files".
* Flow:
  1. All files in the folder are added as a **LocalFilesDataSource** (via FileManager).
  2. Each report is parsed by type (lowercase report-type line):
     * `calls` gives TSK_CALLLOG via `CommunicationArtifactsHelper.addCalllog`, account type PHONE.
     * `contacts/contacts` or `contacts` gives TSK_CONTACT (helper `addContact`, account DEVICE).
     * `device/general information` gives TSK_DEVICE_INFO.
     * `messages/sms` gives TSK_MESSAGE (helper `addMessage`).
     * `web/bookmarks` gives TSK_WEB_BOOKMARK.
  3. Key-to-attribute maps, for example:
     * General info: "device name"→TSK_DEVICE_NAME, "device type"→TSK_DEVICE_MAKE, "mobile id (imei)"/"imei/meid"→TSK_IMEI, "security code"/"unlock code"→TSK_PASSWORD, "model"→TSK_DEVICE_MODEL, "wifi address"→TSK_MAC_ADDRESS, "subscriber id (imsi)"→TSK_IMSI.
     * Bookmarks: "web address"→TSK_URL, "domain"→TSK_DOMAIN, "application"→TSK_PROG_NAME.
     * Generic: "deleted"→TSK_ISDELETED, "name (matched)"→TSK_NAME / TSK_NAME_PERSON, "related application"→TSK_PROG_NAME, "address home"→TSK_LOCATION.
  * Parser name (module attribution) is `"XRY DSP"`.

### 2.8 "Autopsy Logical Imager Results" (`logicalimager/dsp/*`)

* Select a drive (auto-scan external drives for `Logical_Imager_HOSTNAME_yyyymmdd_HH_MM_SS` folders) or "Manually Choose Folder".
* The table has columns "Hostname", "Extracted Date (GMT)", "Path".
* The folder is copied to `<ModuleOutput>/LogicalImager/<name>`. If it already exists, the user is asked to add it with a new name.
* Adds the sparse `.vhd` images (`AddMultipleImagesTask`; an image without a file system is added as a 2 GB-chunked unallocated file) or the extracted files as a LocalFilesDataSource.
* Adds `SearchResults.txt` and `*_users.txt` as case **reports**.
* Creates `TSK_INTERESTING_ITEM` analysis results for search hits. The rule/set name comes from SearchResults.txt.

### 2.9 "Memory Image File (Volatility)" (`Experimental/.../volatilityDSP`)

* Fields: "Browse for a memory image file:", "Timezone:", "Profile:", "Plugins to run:" (checkbox list with Select All / Deselect All).
* Adds the memory file as an image without a file system, runs `volatility.exe` per plugin, adds the outputs as derived/local files, indexes them in the keyword search, and flags the files they mention with `TSK_INTERESTING_ITEM` (set name `Volatility Plugin {plugin}`).

### 2.10 FileManager service (`casemodule/services/FileManager.java`)

* `findFilesByMimeType(types)` uses `WHERE mime_type IN ('a', 'b')`; a variant is restricted to `data_source_obj_id = X`.
* `findFilesByParentPath(dsId, parentPath)`: `data_source_obj_id = X AND parent_path LIKE '<path>%'`.
* `findFiles(name)`, `findFiles(name, parentSubstring)`, `findFiles(dataSource, name[, parentSubstring])`, `findFiles(name, parentFile)` and `openFiles(dataSource, path)` all delegate to TSK. TSK name matching is a `LIKE` on `name` (with `%` wildcards allowed) plus a parent-path substring match [inferred].
* `findFilesExactName(parentId, name)`.
* `addDerivedFile(name, localPath, size, ctime, crtime, atime, mtime, isFile, parent, rederiveDetails, toolName, toolVersion, otherDetails, encodingType)` and `updateDerivedFile(...)`.
* `addCarvedFiles(CarvingResult)` creates CARVED layout files under the parent's `$CarvedFiles` virtual dir [inferred].
* Encodings: `EncodingType.NONE` and `XOR` (XOR1, used for local files stored encoded on disk) [seen enum names].

---

## 3. Data model and tree (`A/datamodel/**`)

Autopsy builds a NetBeans `Node` tree. `DisplayableItemNode` subclasses wrap TSK objects or "virtual" groupings. The left **Directory Tree** shows only container-like nodes (§5.1); the selected node's children are shown in the **Result Viewer** table, and the selected row in the **Content Viewer**.

### 3.1 Root layouts (`AutopsyTreeChildFactory`, `RootContentChildren`)

The layout is chosen by `CasePreferences.groupItemsInTreeByDataSource` (per-case). The UI radio buttons are "Group by Data Type" and "Group by Person/Host" in the View Options popup.

**A. Group by Data Type (default):**

```
<case root, hidden>
├── Data Sources                     (DataSourcesNode, icon image.png)
│   └── <Host name> | "Unknown Host" (HostNode, host.png), one per host, sorted
│       └── <data source>            (ImageNode / LocalFilesDataSourceNode), sorted case-insensitively by name
│           └── content hierarchy…  (§3.2)
├── File Views                       (ViewsNode, views.png), pre-expanded
│   ├── File Types
│   │   ├── By Extension
│   │   └── By MIME Type
│   ├── Deleted Files
│   └── File Size
├── Data Artifacts                   (DataArtifacts.RootNode), pre-expanded
├── Analysis Results                 (AnalysisResults.RootNode), pre-expanded
├── OS Accounts                      (OsAccounts.OsAccountListNode)
├── Tags                             (Tags.RootNode)
├── Score                            (ScoreContent.ScoreContentsNode)
├── Reports                          (Reports.ReportsListNode)
└── Addl. Cyber Triage Data          only if the case DB has a `ct_errors` table (sqlite_master check)
```

**B. Group by Person/Host:**

```
<root>
├── <Person name>  (PersonNode, person.png), sorted; "Unknown Persons" node added if any host has no person
│   └── <Host>   (HostNode)
│       └── <Data source>  (DataSourceGroupingNode: image.png / fileset icon; name = data source name)
│           ├── Data Source Files   (DataSourceFilesNode → the data source's content hierarchy)
│           ├── File Views          (filtered to this data source)
│           ├── Data Artifacts      (filtered)
│           ├── Analysis Results    (filtered)
│           ├── OS Accounts         (filtered: getOsAccountsByDataSourceObjId)
│           ├── Tags                (filtered)
│           └── Score               (filtered)
│   (if no persons exist at all: the hosts are the top level instead of persons)
└── Reports
```

* Every grouped sub-node carries `filteringDataSourceObjId` (0 means no filter) and adds `AND data_source_obj_id = X` to its queries.
* Refresh triggers:
  * The root refreshes on `DATA_SOURCE_ADDED`, `HOSTS_ADDED/DELETED`, `PERSONS_ADDED/DELETED`, `HOSTS_ADDED_TO_PERSON`, `HOSTS_REMOVED_FROM_PERSON`, but only when grouping is on.
  * The Data Sources node refreshes on `DATA_SOURCE_ADDED`, `HOSTS_ADDED/DELETED/UPDATED`.
  * A host node refreshes on `DATA_SOURCE_ADDED`, `HOSTS_DELETED`, and renames itself on `HOSTS_UPDATED`.
  * A person node refreshes on host events and renames itself on `PERSONS_UPDATED`.
* Pre-expansion on open: in mode A the nodes named Analysis Results, Data Artifacts and File Views are expanded; in mode B every Person and Host node is expanded. After opening, the first root child is selected.
* **Grouping prompt.** On tree open, if the case has more than *N* data sources (N = ModuleSettings "Case" key `GroupDataSourceThreshold`, default **5**), show the "Group by Data Source?" dialog: "This case contains {0} data sources. Would you like to group by data source for faster loading?" with Yes/No. The answer is stored in CasePreferences.

### 3.2 Data-source content hierarchy (`ContentChildren`, `CreateSleuthkitNodeVisitor`, content node classes)

Children of a content node are `content.getChildren()`, flattened as follows:

* `VolumeSystem` → skipped (its children are inlined).
* `FileSystem` → skipped (inlined).
* The root `Directory` of a file system (`isRoot()` and not `.`/`..`) → skipped (its children are inlined). The user therefore sees `Image → vol2 (…) → files`, not `Image → VS → Vol → FS → root`.
* Root `LocalDirectory` → inlined.
* Everything else is a child. This includes `BlackboardArtifact` children, such as email attachments and message attachments, which hang off artifacts.

Node classes, display names and icons:

| TSK class | Node class | Display name | Icon |
|---|---|---|---|
| Image | ImageNode | `image.getName()` | hard-drive-icon.jpg |
| LocalFilesDataSource | LocalFilesDataSourceNode | name (e.g. LogicalFileSet1) | fileset-icon-16.png |
| Pool | PoolNode | pool name | pool-icon.png |
| Volume | VolumeNode | `vol<addr> (<description>: <start>-<end>)`; pool volumes: `vol<addr> (<desc>: <start>)` | vol-icon.png |
| Directory | DirectoryNode | name; `.` → "[current folder]", `..` → "[parent folder]" | Folder-icon.png, or folder-icon-deleted.png if the name is unallocated |
| File (fs) | FileNode | name | by extension (below); file-icon-deleted.png if the name is unallocated; carved-file-x-icon-16.png if CARVED |
| LayoutFile | LayoutFileNode | name | carved icon if CARVED; deleted icon if LAYOUT_FILE and the name is unallocated; else by extension |
| LocalFile, DerivedFile | LocalFileNode | name | Folder-icon if dir; else by extension |
| VirtualDirectory | VirtualDirectoryNode | name (e.g. `$Unalloc`, `$CarvedFiles`, `$OrphanFiles`) | folder-icon-virtual.png |
| LocalDirectory | LocalDirectoryNode | name | folder icon |
| SlackFile | SlackFileNode | name (TSK appends `-slack`) | by extension / deleted |
| BlackboardArtifact | BlackboardArtifactNode | §3.10 | IconsUtil per type |
| UnsupportedContent | UnsupportedContentNode | "Unsupported Content" | — |

**File icon by extension** (`FileNode.getIconForFileType`, using the lists in §3.3.1):

| Extension group | Icon |
|---|---|
| image | image-file.png |
| video | video-file.png |
| audio | audio-file.png |
| doc (`.doc .docx .odt .xls .xlsx .ppt .pptx`) | doc-file.png |
| executable | exe-file.png |
| text | text-file.png |
| web | web-file.png |
| pdf | pdf-file.png |
| archive | archive-file.png |
| anything else | file-icon.png |

### 3.3 File Views (`ViewsNode`, `FileTypes`, `FileTypesByExtension`, `FileTypesByMimeType`, `DeletedContent`, `FileSize`, `RecentFiles`)

"File Views" contains "File Types", "Deleted Files" and "File Size". "Recent Files" exists in the code but was removed from the tree in June 2015 because it was "not useful w/out filtering".

**Count display.** Nodes show `Name (N)`. Counting is skipped once `countFilesWhere("1=1") > 1,000,000` (`NODE_COUNT_FILE_TABLE_THRESHOLD`); the check happens once per refresh and stops after the threshold is reached. While counting runs in the background the label reads `Name (counting...)`.

#### 3.3.1 By Extension

Base predicate:

```sql
(dir_type = 5 /*TSK_FS_NAME_TYPE_ENUM.REG*/)
 [AND (known IS NULL OR known != 1)]          -- if "Hide known files in Views" (default ON)
 [AND data_source_obj_id = X]
 AND (extension IN ('jpg','jpeg',...))        -- extensions lower-cased, leading dot stripped
```

The `extension` column is TSK-computed: lowercase, after the last dot.

Groups (`FileTypesByExtension.RootFilter`, with lists from `FileTypeExtensions`):

* **Images**: `.jpg .jpeg .png .psd .nef .tiff .bmp .tec .tif .webp`
* **Videos**: `.aaf .3gp .asf .avi .m1v .m2v .m4v .mp4 .mov .mpeg .mpg .mpe .mp4 .rm .wmv .mpv .flv .swf`
* **Audio**: `.aiff .aif .flac .wav .m4a .ape .wma .mp2 .mp1 .mp3 .aac .mp4 .m4p .m1a .m2a .m4r .mpa .m3u .mid .midi .ogg`
* **Archives**: `.zip .rar .7zip .7z .arj .tar .gzip .bzip .bzip2 .cab .jar .cpio .ar .gz .tgz .bz2`
* **Databases**: `.db .db3 .sqlite .sqlite3`
* **Documents** (`.htm .html .doc .docx .odt .xls .xlsx .ppt .pptx .pdf .txt .rtf`), with sub-nodes:
  * **HTML**: `.htm .html`
  * **Office**: `.doc .docx .odt .xls .xlsx .ppt .pptx`
  * **PDF**: `.pdf`
  * **Plain Text**: `.txt`
  * **Rich Text**: `.rtf`
* **Executable** (`.exe .msi .cmd .com .bat .reg .scr .dll .ini`), with sub-nodes `.exe`, `.dll`, `.bat`, `.cmd`, `.com`.

Other extension lists used by icons and helpers:

* TEXT: `.txt .rtf .log .text .xml`
* WEB: `.html .htm .css .js .php .aspx`
* PDF: `.pdf`

Table columns for group nodes: "File Type", "File Extensions", "Name".

#### 3.3.2 By MIME Type

Base predicate:

```sql
(dir_type = 5) AND (type IN (0 FS,1 CARVED,2 DERIVED,9 LAYOUT_FILE,3 LOCAL [,7 SLACK unless hide-slack-in-views])) 
 [AND data_source_obj_id = X] [AND (known IS NULL OR known != 1)]
```

* Tree build: `SELECT mime_type, count(*) AS count FROM tsk_files WHERE mime_type IS NOT null AND <base> GROUP BY mime_type`. The result is split on `/` into a two-level tree: media type (e.g. `image`), then subtype node `jpeg (N)` with the full `image/jpeg` as key.
* Leaves: `findAllFilesWhere(<base> AND mime_type = 'image/jpeg')`.
* If no MIME types exist (file type ID not run), selecting the node shows "Data not available. Run file type identification module."
* Columns: "Type" / "Subtype".

#### 3.3.3 Deleted Files

Filters:

* **"File System"**: `dir_flags = 2 /*UNALLOC*/ AND meta_flags != 32 /*ORPHAN*/ AND type = 0 /*FS*/`
* **"All"**:
  ```sql
  ((dir_flags = 2 OR meta_flags = 32) AND type = 0) OR type = 1 /*CARVED*/ OR (dir_flags = 2 AND type = 9 /*LAYOUT_FILE*/)
  ```

Each filter gets an optional `AND data_source_obj_id = X`. The parent icon is file-icon-deleted.png. Columns are "Type" and "Name".

Refresh happens on ingest `CONTENT_CHANGED` and job completion; carving adds files.

#### 3.3.4 File Size

| Node label | Predicate |
|---|---|
| "50 - 200MB" (SIZE_50_200) | `size >= 50000000 AND size < 200000000` |
| "200MB - 1GB" (SIZE_200_1GB) | `size >= 200000000 AND size < 1000000000` |
| "1GB+" (SIZE_1000+) | `size >= 1000000000` |

Every row also gets `AND (type != 4 /*UNALLOC_BLOCKS*/)` and an optional data-source filter. Thresholds are decimal. The column is "Size Range"; the icon is file-size-16.png.

#### 3.3.5 Recent Files (disabled)

Filters "Final Day", "Final Day - 1" … "Final Day - 6", relative to the latest timestamp found.

* Latest timestamp: `SELECT MAX(<attr>) FROM tsk_files WHERE <attr> < now` for `crtime`, `ctime` and `mtime`.
* Per-day predicate:
  ```sql
  (dir_type=5) AND (known IS NULL OR known != 1)
   AND ((crtime BETWEEN lo AND hi) OR (ctime BETWEEN lo AND hi) OR (mtime BETWEEN lo AND hi))
  ```

### 3.4 Data Artifacts / Analysis Results (`DataArtifacts`, `AnalysisResults`, `Artifacts`)

* Root labels: "Data Artifacts" and "Analysis Results". Children are type nodes for every artifact type **in use**:
  * `skCase.getArtifactTypesInUse()`, or `blackboard.getArtifactTypesInUse(dsObjId)` when filtered;
  * filtered to the category (`DATA_ARTIFACT` or `ANALYSIS_RESULT`);
  * excluding `TSK_DATA_SOURCE_USAGE`, `TSK_GEN_INFO`, `TSK_DOWNLOAD_SOURCE`, `TSK_TL_EVENT`, `TSK_ASSOCIATED_OBJECT`;
  * sorted case-insensitively by display name.
* Type node label: `<type display name> (N)`, where N is `getBlackboardArtifactsTypeCount(typeId)` or `getArtifactsCount(typeId, dsObjId)`. The icon comes from IconsUtil (§4.5).
* Leaves:
  * Analysis results: `blackboard.getAnalysisResultsByType(typeId[, dsId])`.
  * Data artifacts: `getDataArtifacts(typeId[, dsId])`.
  * All attributes are pre-loaded per artifact (`art.getAttributes()`), an N+1 pattern.
* Refresh:
  * Throttled (§3.12) on ingest `DATA_ADDED` where the `ModuleDataEvent` type category matches.
  * Also on ingest job `COMPLETED` / `CANCELLED`.

**Special type subtrees** replace the flat type node:

| Type | Subtree |
|---|---|
| TSK_KEYWORD_HIT, "Keyword Hits" (keyword_hits.png) | List level, keyed by TSK_SET_NAME. Single-term searches go under "Single Literal Keyword Search" or "Single Regular Expression Search" depending on whether TSK_KEYWORD_REGEXP is present. Next is a **term** level: the regex for regex searches, the literal term otherwise (TSK_KEYWORD_REGEXP holds the original term if present). Regex terms add an **instance** level per matched TSK_KEYWORD value. Literal hits (`TSK_KEYWORD_SEARCH_TYPE == 1` substring, or no regexp) collapse the instance level. Leaves are artifacts. Table columns: "List Name", "Files with Hits", "Name", "Number of Children". Data comes from one query (below). |
| TSK_HASHSET_HIT, "Hashset Hits" | Set name (TSK_SET_NAME) `(count)`, then artifacts. |
| TSK_INTERESTING_ITEM, TSK_INTERESTING_FILE_HIT, TSK_INTERESTING_ARTIFACT_HIT | One root per type, then set name, then artifacts. |
| TSK_EMAIL_MSG, "E-Mail Messages" (mail-icon-16.png) | Account, then folder, then messages. Account and folder come from TSK_PATH split on `/`: `[2]` is the account and `[3]` the folder. Fewer than 4 parts gives "Default"/"Default". |
| TSK_ACCOUNT, "Communication Accounts" / "Accounts" | Account type (TSK_ACCOUNT_TYPE value) `(count)`. Credit cards add a level "By File (N)" / "By BIN (N)". By BIN: BIN groups keyed by the first 8 digits, shown as a range `start-endSuffix` when a ranges.csv entry spans a range. By File: one node per source file, columns "File", "Accounts", "Status"; unallocated chunks are named `{0}_chunk_{1}`. Card leaves follow. Other types list artifacts directly. Rejected artifacts are hidden unless "Show Rejected Results" is toggled (filter `review_status_id != 2`). |
| TSK_MALWARE | Only results with significance NOTABLE or LIKELY_NOTABLE (joins `tsk_analysis_results`). |

Keyword tree query:

```sql
SELECT blackboard_attributes.value_text, blackboard_attributes.value_int32,
       blackboard_artifacts.artifact_obj_id, blackboard_attributes.attribute_type_id
FROM blackboard_attributes, blackboard_artifacts
WHERE blackboard_attributes.artifact_id = blackboard_artifacts.artifact_id
  AND blackboard_artifacts.artifact_type_id = <TSK_KEYWORD_HIT>
  AND (attribute_type_id = <TSK_SET_NAME> OR attribute_type_id = <TSK_KEYWORD>
       OR attribute_type_id = <TSK_KEYWORD_SEARCH_TYPE> OR attribute_type_id = <TSK_KEYWORD_REGEXP>)
  [AND blackboard_artifacts.data_source_obj_id = X]
```

Set-name grouping (hash and interesting):

```sql
SELECT value_text, blackboard_artifacts.artifact_obj_id
FROM blackboard_attributes, blackboard_artifacts
WHERE attribute_type_id=<TSK_SET_NAME> AND blackboard_attributes.artifact_id=blackboard_artifacts.artifact_id
  AND blackboard_artifacts.artifact_type_id=<type> [AND blackboard_artifacts.data_source_obj_id = X]
```

Email grouping:

```sql
SELECT art.artifact_obj_id AS artifact_obj_id,
  (SELECT value_text FROM blackboard_attributes attr
   WHERE attr.artifact_id = art.artifact_id AND attr.attribute_type_id = <TSK_PATH> LIMIT 1) AS value_text
FROM blackboard_artifacts art WHERE art.artifact_type_id = <TSK_EMAIL_MSG> [AND art.data_source_obj_id = X]
```

Account types with counts:

```sql
SELECT res.account_type, COUNT(*) AS count FROM (
  SELECT MIN(blackboard_attributes.value_text) AS account_type FROM blackboard_artifacts
  LEFT JOIN blackboard_attributes ON blackboard_artifacts.artifact_id = blackboard_attributes.artifact_id
  WHERE blackboard_artifacts.artifact_type_id = <TSK_ACCOUNT>
    AND blackboard_attributes.attribute_type_id = <TSK_ACCOUNT_TYPE> [ds filter]
  GROUP BY blackboard_artifacts.artifact_id) res
GROUP BY res.account_type
```

* Files with credit cards: grouped by `obj_id` and `solr_document_id` (TSK_KEYWORD_SEARCH_DOCUMENT_ID), with review statuses aggregated via `GROUP_CONCAT(review_status_id)` (SQLite) or `string_agg(review_status_id::character varying, ',')` (PostgreSQL).
* BINs: `SELECT SUBSTR(blackboard_attributes.value_text,1,8) AS BIN, COUNT(...) … GROUP BY BIN` over TSK_CARD_NUMBER.
* BIN metadata comes from `datamodel/ranges.csv` (columns `iin_start, iin_end, number_length, scheme, brand, type, country, bank_name, bank_url, bank_phone, bank_city`). Start is right-padded with 0 to 8 digits. BIN node columns: "Bank Identifier Number", "Accounts", "Payment Card Type", "Credit Card Scheme", "Brand", "Bank", "Bank City", "Bank Country", "Bank Phone #", "Bank URL".
* Account context actions: "Approve Accounts" (sets `review_status` APPROVED), "Reject Accounts" (REJECTED). Rejecting with the rejected view hidden moves the selection to the previous sibling. Also "Show Rejected Results" (toggle).

### 3.5 OS Accounts (`OsAccounts.java`)

* Node label "OS Accounts" (icon os-account.png). Children:
  * `osAccountManager.getOsAccounts()`, or `getOsAccountsByDataSourceObjId(dsId)` when filtered.
  * Refreshes on `OS_ACCOUNTS_ADDED/DELETED`; rows update in place on `OS_ACCOUNTS_UPDATED`.
* Row display name is the login name or address. Columns:
  * "Name"
  * "Login Name"
  * "Host": the hosts the account was seen on, comma-joined
  * "Scope": realm scope (e.g. Local/Domain)
  * "Realm Name": first realm name
  * "Creation Time"
  * S / C / O
* Realm and host values load asynchronously: `getRealmByRealmId`, `getHosts(osAccount)`.
* Navigation: "View OS Account" (`DirectoryTreeTopComponent.viewOsAccount`) finds the OS Accounts list node (under the matching host when grouped) and selects the row.

### 3.6 Tags (`Tags.java`, `ContentTagNode`, `BlackboardArtifactTagNode`, `tags/TagUtils`)

```
Tags
└── <decorated tag name> (N)          one per tag name IN USE (getTagNamesInUse[ForUser][(dsId)])
    ├── File Tags (n)                 ContentTagTypeNode → ContentTagNode per content tag
    └── Result Tags (n)               BlackboardArtifactTagTypeNode → BlackboardArtifactTagNode per artifact tag
```

* Decorated name: `<TagSet name>: <tag name>` if the tag belongs to a set, plus `" (Notable)"` if the tag type is BAD.
* Bookmark has its own icon.
* With "Hide other users' tags in the: Tags area in the tree" (`ShowOnlyCurrentUserTags`), counts and lists filter on `tag.getUserName()` equal to the current examiner (the OS login).
* Refreshes on `BLACKBOARD_ARTIFACT_TAG_ADDED/DELETED`, `CONTENT_TAG_ADDED/DELETED` and the preference change.

Columns:

* **File tag row**: "File", "File Path" ("Unavailable" if not found), "Comment", "Modified Time", "Changed Time", "Accessed Time", "Created Time", "Size", "MD5 Hash", "User Name", ["Original Name" when translation is on].
* **Result tag row**: "Source Name", "Source File Path", "Result Type", "Comment", "User Name". The context action "View Source Result" navigates to the artifact.

### 3.7 Score (`ScoreContent.java`)

* "Score" (icon red-circle-exclamation.png) has children "Bad Items (N)" and "Suspicious Items (N)". Each lists **files and data artifacts** whose aggregate score matches:
  * Bad: `tsk_aggregate_score.significance = NOTABLE AND (priority = NORMAL OR priority = OVERRIDE)`.
  * Suspicious: same with `LIKELY_NOTABLE`.
* Files: `findAllFilesWhere(" obj_id IN (SELECT tsk_aggregate_score.obj_id FROM tsk_aggregate_score WHERE <filter>) [AND data_source_obj_id = X]")`.
* Data artifacts: `blackboard.getDataArtifactsWhere(" artifacts.artifact_obj_id IN (…) [AND artifacts.data_source_obj_id = X]")`.
* Count:
  ```sql
  SELECT COUNT(tsk_aggregate_score.obj_id) AS count FROM tsk_aggregate_score
  WHERE <filter> [AND tsk_aggregate_score.data_source_obj_id = X]
    AND tsk_aggregate_score.obj_id IN (SELECT tsk_files.obj_id AS obj_id FROM tsk_files
       UNION SELECT blackboard_artifacts.artifact_obj_id FROM blackboard_artifacts
             WHERE artifact_type_id IN (SELECT artifact_type_id FROM blackboard_artifact_types WHERE category_type = <DATA_ARTIFACT>))
  ```
* Columns: "Name", "Type" (e.g. "File").
* Refresh is throttled on analysis-result `DATA_ADDED`, on `CONTENT_TAG_*` and on ingest completion.

### 3.8 Reports (`Reports.java`)

* "Reports" node, children `Case.getAllReports()`. Refreshes on `REPORT_ADDED/REPORT_DELETED`.
* Columns: "Source Module Name", "Report Name", "Created Time", "Report File Path".
* Actions:
  * **"Open Report"** (default/double-click) opens the path with the OS handler (URLs open in the browser). Errors: "There is no associated editor for reports of this type…", "The report file no longer exists.", "Permission to open the report file was denied."
  * **"Delete Report"** / "Delete Reports" with confirmation "Do you want to delete {0} reports from the case?" and the note "The report(s) will remain on disk." It only removes the DB rows.

### 3.9 Cyber Triage data (`CyberTriageData.java`)

Shown as "Addl. Cyber Triage Data" with an "Errors" child when the case DB contains `ct_errors`. That rows come from:

```sql
SELECT id, title, description, stack_trace, time_stamp, severity FROM ct_errors ORDER BY time_stamp DESC
```

* Columns: Severity, Title, Description, Timestamp, Stack Trace.
* Artifacts with the JSON attribute type `CT_JSON_DATA_ARTIFACT`/`CT_JSON_DATA_ATTRIBUTE` have their top-level JSON keys expanded into columns.

### 3.10 Result-table rows (property sheets)

**AbstractFile rows** (`AbstractAbstractFileNode.AbstractFilePropertyType`), in order:

1. "Name" (`.` → "[current folder]", `..` → "[parent folder]")
2. "Original Name": only when a translator is configured and "Add column … for file name translation" is on. Translation runs async and replaces the display name.
3. "S", "C", "O": unless "Do not add columns for S(core), C(omments), and O(ccurences)". "O" only when the Central Repository is enabled. Async (§3.11).
4. "Modified Time", "Change Time", "Access Time", "Created Time": `TimeZoneUtils.getFormattedTime(epochSecs)`.
5. "Size": bytes.
6. "Flags(Dir)": `getDirFlagAsString` ("Allocated" / "Unallocated").
7. "Flags(Meta)": `getMetaFlagsAsString`, e.g. "Allocated, Used".
8. "Known": `FileKnown.getName()`: "unknown" / "known" / "notable".
9. "Location": `getUniquePath()`, e.g. `/img_x.E01/vol_vol2/Windows/…`.
10. "MD5 Hash", "SHA-256 Hash", "MIME Type", "Extension".

Defined but not shown by default: "Mode", "UserID", "GroupID", "Meta Addr.", "Attr. Addr.", "Type(Dir)", "Type(Meta)", "Object ID". The table viewer can show hidden columns.

**Image row:**

* "Name", "Type" ("Image"), "Size (Bytes)", "Sector Size (Bytes)", "Timezone", "Device ID".
* Actions:
  * "Image Details" and "Extract Unallocated Space to Single Files" (ExplorerNodeActionVisitor)
  * "Open File Search by Attributes"
  * "View Summary Information"
  * "Run Ingest Modules"
  * "View in New Window"
  * "Remove Data Source"
  * "Collapse All" (tree only)

**LocalFilesDataSource row:** "Name", "Type" ("Logical File Set"), "Size (Bytes)", "Timezone", "Device ID".

**Volume row:** "Name", "ID", "Starting Sector", "Length in Sectors", "Description", "Flags". Actions:

* "Volume Details"
* "Extract Unallocated Space to Single File"
* "File System Details" (if it contains a file system)
* "View in New Window"

**Pool row:** "Name", "Type".

**BlackboardArtifact rows** (`BlackboardArtifactNode.createSheet`):

1. "Source Name": the display name of the source content. For an artifact source this is its short description; for an OS account, its address or name.
2. For analysis results other than hash or keyword hits:
   * "Score" (significance display name), "Conclusion", "Configuration", "Justification", "Source Type".
   * Otherwise "S", "C", "O" (async).
3. ["Original Name" when translation is on.]
4. Interesting item / interesting artifact hit with TSK_ASSOCIATED_ARTIFACT: "Result Type" and "Result Details" of the associated artifact.
5. **One column per attribute**, keyed by the attribute type display name, in attribute order. Rules:
   * Skipped: TSK_PATH_ID, TSK_TAGGED_ARTIFACT, TSK_ASSOCIATED_ARTIFACT, TSK_SET_NAME, TSK_KEYWORD_SEARCH_TYPE and any JSON-valued attribute (except Cyber Triage JSON, which expands).
   * DATETIME values are formatted.
   * TSK_TOOL_OUTPUT / TSK_TEXT is truncated to 512 characters.
   * For TSK_EMAIL_MSG: TSK_DATETIME_SENT, CONTENT_HTML, CONTENT_RTF, BCC, CC and HEADERS are hidden; CONTENT_PLAIN is truncated to 160 characters plus "...".
   * INTEGER, LONG and DOUBLE stay numeric (sortable); others use `getDisplayString()`.
6. TSK_EXT_MISMATCH_DETECTED: "Extension" and "MIME Type" of the source file.
7. Analysis results: "File Path" (source unique path). For TSK_INTERESTING_FILE_HIT and TSK_INTERESTING_ITEM, also file "Modified Time", "Changed Time", "Accessed Time", "Created Time", "Size", "MD5 Hash".
8. Data artifacts: "Data Source" (the data source name).
9. TSK_METADATA_EXIF: "Size" and "Path" of the file.
10. Plugin columns from `ArtifactPropertyEnricher` services (an extension point).

**Artifact text viewer** (`ArtifactStringContent`): an HTML table with headers "Type", "Value", "Source(s)", plus "Source File Path" and "Artifact ID".

### 3.11 S / C / O columns (`GetSCOTask`, `SCOData`)

The task runs on a 10-thread pool (`content-node-background-task-%d`) per node, and only for visible pages. Each column starts as the description "value loading".

* **S (Score):** `content.getAggregateScore()` gives a `Score(significance, priority)`. The renderer shows an icon by significance. Tooltip: "Has an {significance} analysis result score".
* **C (Comment):** `HasCommentStatus` is one of:
  * `NO_COMMENT`
  * `TAG_NO_COMMENT`: the item has tags but none has a comment
  * `TAG_COMMENT`: a tag has a non-blank comment
  * `CR_COMMENT`: the Central Repository correlation attribute has a comment
  * `CR_AND_TAG_COMMENTS`
* **O (Occurrences):**
  * Number of *other cases* (CR `getCountCasesWithOtherInstances`) sharing the correlation value. MD5 is used for files.
  * -1 when unavailable; the renderer hides negatives.
  * Tooltips: "There were {0} datasource(s) found with occurrences of the MD5 correlation value", "Hash lookup had not been run on this file when the column was populated", "No correlation properties found", "Multiple different correlation properties exist for this result".
* Live updates: file rows re-compute S and C on `CONTENT_TAG_ADDED/DELETED` for their object, and C on `CR_COMMENT_CHANGED`.

### 3.12 Paging, filtering, refresh (`BaseChildFactory`, `KnownAndSlackFilterBase`, `guiutils/RefreshThrottler`)

* **Paging:** result children are produced with `makeKeys()`, which loads **all** keys. They are filtered with a `Predicate` and partitioned in memory into pages of `ResultsTablePageSize` (default **10,000**; 0 means unpaged).
  * Paging is driven by Guava EventBus messages keyed by node name: `PageChangeEvent(n)`, `PageSizeChangeEvent(size)`, `PageCountChangeEvent(count)` and `RefreshKeysEvent`. The result viewer shows page n / count with prev/next.
  * A page size change resets to page 1.
* **Known / slack filters** (in-memory predicates on result children):
  * Views uses `HideKnownFilesInViewsTree` (default **true**) and `HideSlackFilesInViewsTree` (default **true**).
  * Data Sources uses `HideKnownFilesInDataSourcesTree` (default **false**) and `HideSlackFilesInDataSourcesTree` (default **true**).
  * Known means `known == KNOWN(1)`; slack means `type == SLACK(7)`.
  * The Views SQL also applies hide-known.
  * **Bug:** `filterKnown` / `filterSlack` are static in the shared base class, so the Views and Data Sources settings overwrite each other.
* **RefreshThrottler:** listens to ingest module events `DATA_ADDED`, `CONTENT_CHANGED` and `FILE_DONE`. If `isRefreshRequired(evt)`, it schedules one refresh after **5 s** (`MIN_SECONDS_BETWEEN_REFRESH`) on a single "Node Refresh Thread". Further events are coalesced until that refresh runs.
* **Tree preference changes that rebuild the whole tree:** display time zone / local time, hide known or slack (both areas), hide SCO, display translated names, keep preferred viewer, translator name. `ShowOnlyCurrentUserTags` refreshes only the Tags nodes.

### 3.13 Hosts and Persons management (`datamodel/hosts/*`, `datamodel/persons/*`)

* **"Case > Manage Hosts"** (`OpenHostsAction`, dialog "Manage Hosts"):
  * Text: "Hosts represent individual devices that may have multiple data sources."
  * A host list plus "Host Details" (Host Name, and the data sources of the host).
  * Buttons New / Edit / Delete / Close. Delete is enabled only when the host has **no data sources**.
  * Name validation via `HostNameValidator` (empty, duplicate, or same as the original name).
* **Host node context menu:**
  * If the host has a person: "Remove from Person ({0})".
  * Otherwise "Associate with Person" submenu: the existing persons, "Unknown Person", "New Person...".
  * "Merge Into Other Host" submenu: the other hosts, with confirmation "Are you sure you want to merge {0} into {1}?\nThis may include merging OS Accounts and cannot be undone." and progress "Merging {0} into {1}...". Uses `HostManager.mergeHosts`.
* **Person node:** "Rename Person..." (`AddEditPersonDialog` with title "Edit Person" and validator) and "Delete Person". Deleting a person orphans its hosts to "Unknown Persons".

---

## 4. Blackboard catalog (artifact types, attribute types, enums)

TSK predefines the types. Modules may add custom ones via `Blackboard.getOrAddArtifactType(typeName, displayName[, category])` and `getOrAddAttributeType(typeName, valueType, displayName)`. Autopsy's `casemodule/services/Blackboard` wraps these.

* Each artifact has a **category**: `DATA_ARTIFACT` (extracted facts) or `ANALYSIS_RESULT` (judgements with a score).
* Analysis results additionally have `score(significance, priority)`, `conclusion`, `configuration`, `justification` and `ignore_score`.
* Data artifacts may link an `os_account_obj_id`.

### 4.1 Artifact types

IDs are [inferred] from TSK; ID 9 = KEYWORD_HIT is confirmed by hard-coded SQL in `discovery/search/SearchFiltering.java`. Category: D = data artifact, A = analysis result. "Producers" lists the Autopsy modules seen creating or using the type. Many Python and LEAPP modules create types through TSK helpers (`CommunicationArtifactsHelper`, `GeoArtifactsHelper`, `WebBrowserArtifactsHelper`) or through config-driven names (`modules/leappanalyzers/*.xml`), so the list is a lower bound.

| ID | Type name | Display name [inferred] | Cat | Meaning | Producers [seen] |
|---|---|---|---|---|---|
| 1 | TSK_GEN_INFO | General Info | D | Per-object generic holder (hidden from tree) | TSK internal |
| 2 | TSK_WEB_BOOKMARK | Web Bookmarks | D | Browser bookmark | RecentActivity, XRY |
| 3 | TSK_WEB_COOKIE | Web Cookies | D | Browser cookie | RecentActivity |
| 4 | TSK_WEB_HISTORY | Web History | D | Visited URL | RecentActivity |
| 5 | TSK_WEB_DOWNLOAD | Web Downloads | D | Downloaded file record (links to file via TSK_PATH_ID / ASSOCIATED_OBJECT) | RecentActivity |
| 6 | TSK_RECENT_OBJECT | Recent Documents | D | Recently used files (LNK, jump lists, registry MRU) | RecentActivity |
| 7 | TSK_GPS_TRACKPOINT | GPS Trackpoints | D | Deprecated single trackpoint | — |
| 8 | TSK_INSTALLED_PROG | Installed Programs | D | Installed software | RecentActivity |
| 9 | TSK_KEYWORD_HIT | Keyword Hits | A | Keyword/regex search hit | KeywordSearch |
| 10 | TSK_HASHSET_HIT | Hashset Hits | A | File matched a notable or known hash set | hashdatabase, ImageGallery |
| 11 | TSK_DEVICE_ATTACHED | USB Device Attached | D | USB device history | RecentActivity |
| 12 | TSK_INTERESTING_FILE_HIT | Interesting Files | A | (Legacy) file matched an interesting-files rule | python examples |
| 13 | TSK_EMAIL_MSG | E-Mail Messages | D | Email (MBOX/PST/EML/Thunderbird) | thunderbirdparser |
| 14 | TSK_EXTRACTED_TEXT | Extracted Text | D | Extracted text blob | — |
| 15 | TSK_WEB_SEARCH_QUERY | Web Search | D | Search engine query | RecentActivity |
| 16 | TSK_METADATA_EXIF | EXIF Metadata | A | Camera EXIF (date, make, model, GPS) | pictureanalyzer, ImageGallery |
| 17 | TSK_TAG_FILE | Tagged Files | A | Deprecated | — |
| 18 | TSK_TAG_ARTIFACT | Tagged Results | A | Deprecated | — |
| 19 | TSK_OS_INFO | Operating System Information | D | OS name, version, owner, product ID… | RecentActivity |
| 20 | TSK_OS_ACCOUNT | Operating System User Account | D | Deprecated, replaced by `tsk_os_accounts` | — |
| 21 | TSK_SERVICE_ACCOUNT | Web Accounts | D | Online service account | RecentActivity |
| 22 | TSK_TOOL_OUTPUT | Raw Tool Output | D | Deprecated raw text | — |
| 23 | TSK_CONTACT | Contacts | D | Address-book entry | android, XRY, vCard (thunderbirdparser) |
| 24 | TSK_MESSAGE | Messages | D | SMS/IM message | android/LEAPP via helper, XRY |
| 25 | TSK_CALLLOG | Call Logs | D | Phone call | android/LEAPP, XRY |
| 26 | TSK_CALENDAR_ENTRY | Calendar Entries | D | Calendar event | android |
| 27 | TSK_SPEED_DIAL_ENTRY | Speed Dial Entries | D | Speed dial | android |
| 28 | TSK_BLUETOOTH_PAIRING | Bluetooth Pairings | D | Paired BT device | RecentActivity, android |
| 29 | TSK_GPS_BOOKMARK | GPS Bookmarks | D | Saved location | GPX module, android |
| 30 | TSK_GPS_LAST_KNOWN_LOCATION | GPS Last Known Location | D | Last location | android |
| 31 | TSK_GPS_SEARCH | GPS Searches | D | Map search | android |
| 32 | TSK_PROG_RUN | Run Programs | D | Program execution evidence (prefetch, UserAssist…) | RecentActivity |
| 33 | TSK_ENCRYPTION_DETECTED | Encryption Detected | A | Password-protected or encrypted file/volume | encryptiondetection, embeddedfileextractor, RA, thunderbird |
| 34 | TSK_EXT_MISMATCH_DETECTED | Extension Mismatch Detected | A | Extension does not match MIME | fileextmismatch |
| 35 | TSK_INTERESTING_ARTIFACT_HIT | Interesting Results | A | (Legacy) artifact matched a rule | — |
| 36 | TSK_GPS_ROUTE | GPS Route | D | Route with waypoints (TSK_GEO_WAYPOINTS JSON) | drone/GPX/android |
| 37 | TSK_REMOTE_DRIVE | Remote Drive | D | Mapped network drive | RecentActivity |
| 38 | TSK_FACE_DETECTED | Face Detected | A | Face in image | — |
| 39 | TSK_ACCOUNT | Accounts | D | Communication account (incl. credit cards) | KeywordSearch (CC), comms helpers |
| 40 | TSK_ENCRYPTION_SUSPECTED | Encryption Suspected | A | High-entropy file | encryptiondetection |
| 41 | TSK_OBJECT_DETECTED | Object Detected | A | OpenCV classifier hit | Experimental/objectdetection |
| 42 | TSK_WIFI_NETWORK | Wireless Networks | D | Known Wi-Fi SSIDs | RecentActivity |
| 43 | TSK_DEVICE_INFO | Device Info | D | Phone device info (IMEI…) | XRY, android |
| 44 | TSK_SIM_ATTACHED | SIM Attached | D | SIM card (ICCID/IMSI) | android |
| 45 | TSK_BLUETOOTH_ADAPTER | Bluetooth Adapter | D | Local BT adapter | android |
| 46 | TSK_WIFI_NETWORK_ADAPTER | Wireless Network Adapters | D | Local Wi-Fi adapter | android |
| 47 | TSK_VERIFICATION_FAILED | Verification Failure | A | Image hash verification failed | dataSourceIntegrity |
| 48 | TSK_DATA_SOURCE_USAGE | Data Source Usage | A | Guess of what the data source was (e.g. "Android device") — hidden in tree | RecentActivity |
| 49 | TSK_WEB_FORM_AUTOFILL | Web Form Autofill | D | Saved form field | RecentActivity |
| 50 | TSK_WEB_FORM_ADDRESS | Web Form Addresses | D | Saved address | RecentActivity (Chromium) |
| 51 | TSK_DOWNLOAD_SOURCE | Download Source | D | Deprecated (hidden) | — |
| 52 | TSK_WEB_CACHE | Web Cache | D | Browser cache entry (links to cached file) | RecentActivity |
| 53 | TSK_TL_EVENT | TL Events | D | Timeline event from Plaso (hidden) | plaso |
| 54 | TSK_CLIPBOARD_CONTENT | Clipboard Content | D | Clipboard text | — |
| 55 | TSK_ASSOCIATED_OBJECT | Associated Object | D | Links a file to an artifact (TSK_ASSOCIATED_ARTIFACT) (hidden) | RecentActivity, KeywordSearch |
| 56 | TSK_USER_CONTENT_SUSPECTED | User Content Suspected | A | Image likely user-created (camera photo) | pictureanalyzer |
| 57 | TSK_METADATA | Metadata | D | Document metadata (author, last printed…) | KeywordSearch (Tika) |
| 58 | TSK_GPS_TRACK | GPS Track | D | Track (TSK_GEO_TRACKPOINTS JSON) | GPX/drone |
| 59 | TSK_WEB_ACCOUNT_TYPE | Web Account Type | A | Role on a domain (admin/user) | RecentActivity |
| 60 | TSK_SCREEN_SHOTS | Screenshots | D | Screenshot records | LEAPP |
| 61 | TSK_PROG_NOTIFICATIONS | Program Notifications | D | App notifications | LEAPP |
| 62 | TSK_BACKUP_EVENT | Backup Events | D | Backup activity | LEAPP |
| 63 | TSK_DELETED_PROG | Deleted Programs | D | Uninstalled app | LEAPP |
| 64 | TSK_USER_DEVICE_EVENT | User Device Events | D | Device usage event | LEAPP |
| 65 | TSK_YARA_HIT | YARA Hit | A | YARA rule matched | modules/yara |
| 66 | TSK_GPS_AREA | GPS Area | D | Polygon (TSK_GEO_AREAPOINTS JSON) | drone/android |
| 67 | TSK_WEB_CATEGORIZATION | Web Categorization | A | Domain category (e.g. "Web Email") | RecentActivity |
| 68 | TSK_PREVIOUSLY_SEEN | Previously Seen | A | Correlation value seen in other cases | centralrepository ingest |
| 69 | TSK_PREVIOUSLY_UNSEEN | Previously Unseen | A | Value never seen before | centralrepository ingest |
| 70 | TSK_PREVIOUSLY_NOTABLE | Previously Notable | A | Value tagged notable in another case | centralrepository ingest |
| 71 | TSK_INTERESTING_ITEM | Interesting Items | A | Generic interesting file/artifact (replaces 12/35) | interestingitems, filetypeid, embeddedfileextractor, logical imager, volatility |
| 72 | TSK_MALWARE | Malware | A | Malware scan result | `com.basistech…cybertriage…malwarescan` (CT Malware Scanner) |

**Custom artifact types seen:**

| Type name | Display name | Creator |
|---|---|---|
| TSK_RECYCLE_BIN | "Recycle Bin" | RecentActivity/ExtractRecycleBin |
| RA_SHELL_BAG | "Shell Bags" | ExtractRegistry |
| WSL_NAME | "Windows Subsystem For Linux" | ExtractRegistry |
| TSK_FAVICON, TSK_LOCAL_STATE, TSK_CHROME_EXTENSIONS | — | Chromium |
| `<atName.toUpperCase()>` | per config | LEAPP XML config |

**Custom attributes seen:**

| Type name | Notes | Creator |
|---|---|---|
| RA_SHELL_BAG_LAST_WRITE | DATETIME | ExtractRegistry |
| RA_SHELL_BAG_KEY | STRING | ExtractRegistry |
| WSL_KERNAL_COMMAND | — | ExtractRegistry |
| RA_SAM_QUESTION_1..3, RA_SAM_ANSWER_1..3 | — | ExtractRegistry |
| EMAIL_HAS_ATTACHMENT | STRING, "Has Attachments" | thunderbird |
| vCard-derived names, e.g. `TSK_PHONE_NUMBER_<TYPE>`, `TSK_EMAIL_<TYPE>` | — | VcardParser |
| TSK_IP_DHCP | "DHCP Information" | LEAPP |
| TSK_REG_RUN_KEY_NAME/VALUE, TSK_REG_KEY_LOCATION | — | python example |

**Hidden from the tree** (`Artifacts.TypeFactory.IGNORED_TYPES`): TSK_DATA_SOURCE_USAGE, TSK_GEN_INFO, TSK_DOWNLOAD_SOURCE, TSK_TL_EVENT, TSK_ASSOCIATED_OBJECT.

### 4.2 Attribute types (all referenced in the repo)

Value type enum `TSK_BLACKBOARD_ATTRIBUTE_VALUE_TYPE` [inferred IDs]: STRING(0), INTEGER(1), LONG(2), DOUBLE(3), BYTE(4), DATETIME(5, epoch seconds stored in value_int64), JSON(6, stored in value_text). Value types below are [inferred] unless stated. Only TSK_SET_NAME's numeric ID is confirmed (**37**).

* **Time** (DATETIME): TSK_DATETIME, TSK_DATETIME_ACCESSED, TSK_DATETIME_CREATED, TSK_DATETIME_DELETED, TSK_DATETIME_MODIFIED, TSK_DATETIME_START, TSK_DATETIME_END, TSK_DATETIME_SENT, TSK_DATETIME_RCVD, TSK_DATETIME_PASSWORD_RESET, TSK_DATETIME_PASSWORD_FAIL, TSK_LAST_ACCESSED, TSK_LAST_PRINTED_DATETIME.
* **Web** (STRING): TSK_URL, TSK_URL_DECODED, TSK_DOMAIN, TSK_REFERRER, TSK_TITLE, TSK_HOST, TSK_SERVER_NAME.
* **Naming / identity** (STRING):
  * Basic: TSK_NAME, TSK_NAME_PERSON, TSK_USER_NAME, TSK_USER_ID, TSK_ORGANIZATION, TSK_OWNER, TSK_REALM, TSK_GROUPS, TSK_HOME_DIR.
  * Accounts and passwords: TSK_PASSWORD, TSK_PASSWORD_HINT, TSK_PASSWORD_SETTINGS, TSK_ACCOUNT_SETTINGS.
  * Flags: TSK_IS_ADMIN (INTEGER 0/1), TSK_FLAG (account flags, STRING), TSK_PERMISSIONS.
* **Communications** (STRING): TSK_PHONE_NUMBER, TSK_PHONE_NUMBER_FROM, TSK_PHONE_NUMBER_TO, TSK_PHONE_NUMBER_HOME, TSK_PHONE_NUMBER_MOBILE, TSK_PHONE_NUMBER_OFFICE, TSK_EMAIL, TSK_EMAIL_FROM, TSK_EMAIL_TO, TSK_EMAIL_CC, TSK_EMAIL_BCC, TSK_EMAIL_REPLYTO, TSK_EMAIL_HOME, TSK_EMAIL_CONTENT_PLAIN, TSK_EMAIL_CONTENT_HTML, TSK_EMAIL_CONTENT_RTF, TSK_HEADERS, TSK_SUBJECT, TSK_MSG_ID, TSK_THREAD_ID, TSK_MESSAGE_TYPE, TSK_DIRECTION ("Incoming"/"Outgoing"/"Unknown"), TSK_READ_STATUS (INTEGER), TSK_ATTACHMENTS (JSON list of file/URL attachments), TSK_TEXT, TSK_ID.
* **Accounts**: TSK_ACCOUNT_TYPE (STRING; `Account.Type` name, e.g. "CREDIT_CARD"), TSK_ID.
* **Credit cards**: TSK_CARD_NUMBER, TSK_CARD_EXPIRATION, TSK_CARD_SERVICE_CODE, TSK_CARD_DISCRETIONARY, TSK_CARD_LRC, TSK_CARD_SCHEME, TSK_CARD_TYPE, TSK_BRAND_NAME, TSK_BANK_NAME, TSK_CITY, TSK_COUNTRY (all STRING); TSK_KEYWORD_SEARCH_DOCUMENT_ID (STRING, Solr doc id).
* **Keyword search**:
  * TSK_KEYWORD, TSK_KEYWORD_REGEXP, TSK_KEYWORD_PREVIEW (all STRING).
  * TSK_KEYWORD_SEARCH_TYPE (INTEGER [seen]: 0 LITERAL/exact, 1 SUBSTRING, 2 REGEX, from `KeywordSearch.QueryType` ordinal).
  * TSK_SET_NAME (STRING, list/set/rule set name; ID 37 [seen]).
* **Interesting / rules**: TSK_SET_NAME, TSK_RULE, TSK_CATEGORY, TSK_COMMENT, TSK_DESCRIPTION, TSK_ASSOCIATED_ARTIFACT (LONG artifact id), TSK_TAGGED_ARTIFACT (LONG, deprecated).
* **Files / paths**: TSK_PATH (STRING), TSK_PATH_ID (LONG obj_id of the linked file; -1 means none), TSK_LOCAL_PATH, TSK_REMOTE_PATH, TSK_TEMP_DIR, TSK_SHORTCUT, TSK_HASH_MD5, TSK_ISDELETED (STRING), TSK_COUNT (INTEGER), TSK_VALUE, TSK_BYTES_SENT / TSK_BYTES_RECEIVED (LONG).
* **Programs / OS**: TSK_PROG_NAME, TSK_VERSION, TSK_PRODUCT_ID, TSK_PROCESSOR_ARCHITECTURE.
* **Devices**: TSK_DEVICE_ID, TSK_DEVICE_MAKE, TSK_DEVICE_MODEL, TSK_DEVICE_NAME, TSK_MAC_ADDRESS, TSK_IMEI, TSK_IMSI, TSK_ICCID, TSK_SSID, TSK_IP_ADDRESS.
* **Geo** (DOUBLE): TSK_GEO_LATITUDE, TSK_GEO_LONGITUDE, TSK_GEO_ALTITUDE, TSK_GEO_VELOCITY, TSK_GEO_LATITUDE_START/END, TSK_GEO_LONGITUDE_START/END. JSON: TSK_GEO_TRACKPOINTS, TSK_GEO_WAYPOINTS, TSK_GEO_AREAPOINTS. TSK_LOCATION (STRING).
* **Calendar**: TSK_CALENDAR_ENTRY_TYPE.
* **Correlation** (STRING): TSK_CORRELATION_TYPE, TSK_CORRELATION_VALUE, TSK_OTHER_CASES (list of case names).
* **Timeline**: TSK_TL_EVENT_TYPE (LONG event type id).

### 4.3 Score model [inferred from TSK + usage]

* `Score(Significance, Priority)`. Constants: SCORE_NOTABLE, SCORE_LIKELY_NOTABLE, SCORE_NONE, SCORE_UNKNOWN.
* **Significance:** UNKNOWN, LIKELY_NONE, LIKELY_NOTABLE, NONE, NOTABLE. Ordered, with definitive outranking likely. Display names in the order "Unknown", "Likely Not Notable", "Likely Notable", "Not Notable", "Notable". Numeric IDs are TSK-internal (verify).
* **Priority:** NORMAL and OVERRIDE. An override result supersedes the normal max.
* **Aggregate score** (`tsk_aggregate_score(obj_id, data_source_obj_id, significance, priority)`): maintained by TSK `ScoringManager` as the max over the object's analysis results. Tags with a BAD tag type also affect it; Autopsy re-reads S on `CONTENT_TAG_*`.
* Score is used by the Score tree (§3.7), the S column, Discovery and others.

### 4.4 Other enums used by the data model

* **TskData.TSK_DB_FILES_TYPE_ENUM** (`tsk_files.type`) [inferred values]:

  | Value | Name | Value | Name |
  |---|---|---|---|
  | 0 | FS | 5 | UNUSED_BLOCKS |
  | 1 | CARVED | 6 | VIRTUAL_DIR |
  | 2 | DERIVED | 7 | SLACK |
  | 3 | LOCAL | 8 | LOCAL_DIR |
  | 4 | UNALLOC_BLOCKS | 9 | LAYOUT_FILE |

* **TSK_FS_NAME_TYPE_ENUM** (`dir_type`): UNDEF 0 "-", FIFO 1 "p", CHR 2 "c", **DIR 3 "d"**, BLK 4 "b", **REG 5 "r"**, LNK 6 "l", SOCK 7 "s", SHAD 8 "h", WHT 9 "w", VIRT 10 "v", VIRT_DIR 11 "V".
* **TSK_FS_META_TYPE_ENUM** (`meta_type`): UNDEF 0, REG 1, DIR 2, FIFO 3, CHR 4, BLK 5, LNK 6, SHAD 7, SOCK 8, WHT 9, VIRT 10, VIRT_DIR 11.
* **TSK_FS_NAME_FLAG_ENUM** (`dir_flags`): ALLOC 1 "Allocated", UNALLOC 2 "Unallocated".
* **TSK_FS_META_FLAG_ENUM** (`meta_flags`, a bitmask): ALLOC 1, UNALLOC 2, USED 4, UNUSED 8, COMP 16, ORPHAN 32.
* **FileKnown** (`known`): UNKNOWN 0 "unknown", KNOWN 1 "known", BAD 2 "notable".
* **ObjectType** (`tsk_objects.type`): IMG 0, VS 1, VOL 2, FS 3, ABSTRACTFILE 4, ARTIFACT 5, REPORT 6, POOL 7, OS_ACCOUNT 8, HOST_ADDRESS 9, UNSUPPORTED 10.
* **ReviewStatus** (`review_status_id`): APPROVED 1, REJECTED 2, UNDECIDED 3. Only REJECTED is filtered in the UI.
* **TagType** (formerly FileKnown on `tag_names.knownStatus`): BAD and SUSPICIOUS [seen]. Conversion: FileKnown.BAD ↔ BAD, others → SUSPICIOUS.
* **TagName.HTML_COLOR** [seen subset]: NONE, RED, YELLOW, GREEN, BLUE, FUCHSIA. Full TSK set [inferred]: NONE, WHITE, SILVER, GRAY, BLACK, RED, MAROON, YELLOW, OLIVE, LIME, GREEN, AQUA, TEAL, BLUE, NAVY, FUCHSIA, PURPLE.
* **EncodingType**: NONE, XOR1 (`XOR`).
* **DbType**: SQLITE, POSTGRESQL.
* **IngestJobStatusType**: STARTED, CANCELLED, COMPLETED.
* **Account.Type** (seen): PHONE, DEVICE, CREDIT_CARD, FACEBOOK, EMAIL, WHATSAPP, SKYPE, IMO, XENDER, VIBER, LINE, TEXTNOW, ZAPYA, TANGO, SHAREIT, WEBSITE, TWITTER, MESSAGING_APP, INSTAGRAM, THREEMA. Plus runtime-added types (e.g. `addAccountType("WWF","Words with Friends")`).
* **OS account realms**: `RealmScope.LOCAL|DOMAIN` (plus UNKNOWN [inferred]). `ScopeConfidence` and `OsAccountStatus`/`OsAccountType` come from TSK [inferred: status UNKNOWN/ACTIVE/DISABLED/DELETED; type UNKNOWN/SERVICE/INTERACTIVE].
* **CaseNodeData.DeletedFlags** (§1.10). **DataSourceProcessorResult**: NO_ERRORS, CRITICAL_ERRORS, NONCRITICAL_ERRORS.

### 4.5 Icons per artifact type (`datamodel/utils/IconsUtil.getIconFilePath`)

All icons are under `/org/sleuthkit/autopsy/images/`:

| Artifact type | Icon | Artifact type | Icon |
|---|---|---|---|
| WEB_BOOKMARK | bookmarks.png | WEB_COOKIE | cookies.png |
| WEB_HISTORY | history.png | WEB_DOWNLOAD | downloads.png |
| RECENT_OBJECT | recent_docs.png | GPS_TRACKPOINT | gps_trackpoint.png |
| INSTALLED_PROG | programs.png | DEVICE_ATTACHED | usb_devices.png |
| EMAIL_MSG | mail-icon-16.png | EXTRACTED_TEXT | text-file.png |
| WEB_SEARCH_QUERY | searchquery.png | METADATA_EXIF | camera-icon-16.png |
| OS_INFO | computer.png | SERVICE_ACCOUNT | account-icon-16.png |
| CONTACT | contact.png | MESSAGE | message.png |
| CALLLOG | calllog.png | CALENDAR_ENTRY | calendar.png |
| SPEED_DIAL_ENTRY | speeddialentry.png | BLUETOOTH_PAIRING | Bluetooth.png |
| GPS_BOOKMARK | gpsfav.png | GPS_LAST_KNOWN_LOCATION | gps-lastlocation.png |
| GPS_SEARCH | gps-search.png | PROG_RUN | installed.png |
| ENCRYPTION_DETECTED / SUSPECTED | encrypted-file.png | EXT_MISMATCH_DETECTED | mismatch-16.png |
| GPS_ROUTE / GPS_TRACK | gps_trackpoint.png | REMOTE_DRIVE | drive_network.png |
| OS_ACCOUNT | os-account.png | OBJECT_DETECTED | objects.png |
| WEB_FORM_AUTOFILL | web-form.png | WEB_CACHE | cache.png |
| USER_CONTENT_SUSPECTED | user-content.png | METADATA | metadata.png |
| CLIPBOARD_CONTENT | clipboard.png | FACE_DETECTED | face.png |
| WIFI_NETWORK / WIFI_NETWORK_ADAPTER | network-wifi.png | SIM_ATTACHED | sim_card.png |
| BLUETOOTH_ADAPTER | Bluetooth.png | DEVICE_INFO | devices.png |
| VERIFICATION_FAILED | validationFailed.png | WEB_ACCOUNT_TYPE | web-account-type.png |
| WEB_FORM_ADDRESS | web-form-address.png | WEB_CATEGORIZATION | domain-16.png |
| GPS_AREA | gps-area.png | YARA_HIT | yara_16.png |
| PREVIOUSLY_SEEN | previously-seen.png | PREVIOUSLY_UNSEEN | previously-unseen.png |
| PREVIOUSLY_NOTABLE | red-circle-exclamation.png | default | artifact-icon.png |

Special nodes use: keyword_hits.png, hashset_hits (in the hashset node), interesting_item.png, accounts icons and others.

---

## 5. Directory Tree component (`A/directorytree/**`)

### 5.1 `DirectoryTreeTopComponent` ("Directory Tree" window, left pane)

* Single-selection tree; the root is hidden. The window cannot be closed while the case has data.
* Header buttons:
  * Back / Forward (icons `btn_step_back*.png`, `btn_step_forward*.png`).
  * View Options (`view-preferences-23.png`): a popup containing `ViewPreferencesPanel` (§10.2).
* **What appears in the tree** (`DirectoryTreeFilterChildren`): only `DisplayableItemNode`s that pass `ShowItemVisitor`:
  * Directories, except `.` and `..`.
  * Files (FileNode, LocalFileNode, LayoutFileNode, SlackFileNode) **only if they have visible children**: derived files from archives, attachments, or email/message artifacts beneath them. The check is `contentHasVisibleContentChildren`:
    ```sql
    SELECT COUNT(obj_id) AS count FROM ( SELECT obj_id FROM tsk_objects WHERE par_obj_id = <id> AND type = 5 /*ARTIFACT*/
      INTERSECT SELECT artifact_obj_id FROM blackboard_artifacts WHERE obj_id = <id>
        AND (artifact_type_id = <TSK_EMAIL_MSG> OR artifact_type_id = <TSK_MESSAGE>)
      UNION SELECT obj_id FROM tsk_objects WHERE par_obj_id = <id> AND type = 4 /*ABSTRACTFILE*/) AS OBJECT_IDS
    ```
  * Virtual and local directories: always.
  * BlackboardArtifactNode: only EMAIL_MSG / MESSAGE artifacts that have children (attachments).
  * Every non-content grouping node.
* **Leaf decision** (`IsLeafItemVisitor`):
  * A directory is a leaf if none of its children is a non-dot `Directory` and none has visible content children.
  * A volume is a leaf if all its children are `LayoutFile`s.
  * A file-like node is a leaf unless it has a directory child or a child with visible children.
  * Category nodes use `isLeafTypeNode()`.
* **Child counts in tree labels** (`DirectoryTreeFilterNode.getDisplayName`):
  * For file-backed nodes: `name (N)`, where N is `file.getChildren().size()` minus children hidden by hide-known / hide-slack in the Data Sources area, minus artifact children that are not email/message.
  * For email/message artifact nodes: `(number of attachments)`.
  * Computed synchronously on paint.
* Tree-node context menu: the node's own actions (only if its lookup has Content/Host/Person), plus "Collapse All" (collapses everything below the root).
* **Selection → result view** (`respondSelection`, on the AWT queue):
  * Wraps the selected node's original in a `DataResultFilterNode`, then a `TableFilterNode` whose column-order key is `getItemType()`, and sets it on the "Listing" `DataResultTopComponent`.
  * The path label shows `content.getUniquePath()` or the node's String lookup.
  * Focus moves to the result view when the selection changes to a different node.
* **History:**
  * `backList` and `forwardList` hold node paths (`String[]` from `NodeOp.createPath`).
  * Each new selection whose last path element differs from the current one pushes onto back and clears forward.
  * Back pops the current entry onto forward and selects the new last entry. Forward pops from forward and pushes onto back.
  * Path lookup is tolerant: if `NodeOp.findPath` fails, it trims trailing segments until a node is found.
  * History resets on case open and on tree rebuild.
* **Case events:**
  * `CURRENT_CASE` with new=null sets an empty root.
  * New case: rename the root to the case name.
  * `DATA_SOURCE_ADDED` opens the core windows if they are not open.

### 5.2 "View in Directory" (`ViewContextAction`)

Labels used for this action:

* "View File in Directory"
* "View Source File in Directory"
* "View in Directory" (default action for artifact rows)
* "View {Associated File|Cached File|Downloaded File} in Directory" (artifact with TSK_PATH_ID)
* "View Source {File|Data Artifact|OS Account|Item} in Directory"

The action is disabled for known files when the Data Sources area hides known files, and likewise for slack files.

Algorithm:

1. Let `parent` be the content itself if it is a DataSource, otherwise `content.getParent()`. If the parent is `UnsupportedContent`, show "Unable to navigate to content not supported in this release."
2. Locate the data-source-level tree nodes:
   * **Grouped mode** (Person/Host): collect every node under persons and hosts whose name equals the data source's name. Duplicates are handled by checking each candidate. Then descend into its "Data Source Files" child.
   * **Ungrouped mode**: under "Data Sources" → hosts → data source nodes.
3. Build the ancestor chain of `parent` with an `AncestorVisitor`. It walks `getParent()` upward and skips VolumeSystem and FileSystem (mirroring the tree flattening). Reverse it and wrap it as a dummy filtered tree.
4. Walk down the real tree, matching node **names** level by level (node name = `content_<objId>` for content nodes). The deepest match is the parent tree node.
5. On the parent node's undecorated original, set `ChildNodeSelectionInfo = ContentNodeSelectionInfo(content)`. For an artifact, use its source object (`artifact.getObjectID()`).
6. Expand the parent node and select it. The result viewer then selects the matching child row and switches page if needed.
7. If the parent node is already selected, re-run `respondSelection` and focus the listing.

Error messages: "Failed to locate directory.", "Failed to select directory in tree.", "Failed to locate data source node in tree."

### 5.3 "View Artifact" navigation (`DirectoryTreeTopComponent.viewArtifact`)

Used by "View Source Result", by Discovery, Timeline and others.

1. Find the category node (Data Artifacts or Analysis Results) at the root, or under the matching data source in grouped mode.
2. Choose the child by type:
   * HASHSET_HIT: type node, then set-name child.
   * KEYWORD_HIT: list (or "Single Literal/Regular Expression Search"), then regex node (named by list or `list_regex`), then keyword child.
   * INTERESTING_*: type root, then the child whose String lookup equals the set name.
   * EMAIL_MSG: account, then folder from the parsed TSK_PATH.
   * ACCOUNT: type node by `art.getDisplayName()`, then the account-type child. For credit cards: "Credit Card", then "By BIN", then the BIN node (range-formatted).
   * Otherwise: the type node found by type name.
3. Set `ArtifactNodeSelectionInfo(art)`, expand and select.

`viewArtifactContent(art)` runs ViewContextAction with the label "View Artifact Content".

### 5.4 Result-table behaviour (`DataResultFilterNode`)

* Context menu is the node's own `getActions(true)`. The Reports list node has none.
* **Default action** (double-click / Enter):
  * Directory: `..` selects the tree parent; `.` does nothing; otherwise select that child in the tree (drill down).
  * File or local file with children: drill down. Otherwise none (the content viewer shows it).
  * Artifact: its preferred action. EMAIL_MSG / MESSAGE with children drill down; otherwise "View in Directory".
  * Report: "Open Report".
  * Grouping nodes: drill down.
  * Common-properties nodes: none.

### 5.5 Context-menu actions on file rows (order as built)

FileNode (LayoutFileNode, LocalFileNode and SlackFileNode are similar):

1. "View File in Directory" (only when not in directory-browse mode, i.e. shown from Views/Results/Search).
2. "View File in Timeline...".
3. —
4. "View Item in New Window" (`NewWindowViewAction`): an undocked content viewer named after the item.
5. "Open in External Viewer  Ctrl+E": only when exactly one file is selected; plus the global Ctrl+E shortcut.
6. —
7. "Extract File(s)".
8. "Export Selected Rows to CSV".
9. —
10. "Add File Tag" / "Add File Tags" (submenu, §7.3).
11. "Remove File Tag" (only when one file is selected).
12. Plugin actions via `ContextMenuExtensionPoint.getActions()` (the `ContextMenuActionsProvider` services), e.g. "Add to hash set", "Search for MD5 in Central Repository".
13. For archives (by extension) with a TSK_ENCRYPTION_DETECTED artifact: "Extract Archive with Password".
14. —
15. NetBeans default actions (Properties).

Labels declared but not wired in this version: "Search for files with the same MD5 hash" (FileNode, AttachmentNode, LocalFileNode, DataResultFilterNode) and `HashSearchProvider`. Treat this as a feature to restore.

DirectoryNode adds "Run Ingest Modules" after Export CSV. Image, Volume and data-source nodes are covered in §3.10.

Artifact rows (`BlackboardArtifactNode.getActions`), in groups separated by separators:

1. "View Selected Item in Timeline..."
2. "View {Associated File} in Directory", "View {Associated File} in Timeline..." (when TSK_PATH_ID points to a file)
3. "View Source {File/Data Artifact/OS Account} in Directory", "View Source {X} in Timeline..."
4. "Extract Archive with Password" (if applicable)
5. Report actions (if the source is a report)
6. "View Item in New Window", "Open in External Viewer  Ctrl+E" (single file)
7. "Extract File(s)", "Export Selected Rows to CSV" (if there is a source file)
8. "Add File Tag" (if there is a source file and the artifact is not a data artifact), "Add Result Tag", "Remove File Tag" (one file), "Remove Result Tag" (one artifact)
9. Plugin actions
10. Defaults

### 5.6 Extract (`ExtractAction`, `actionhelpers/ExtractActionHelper`, `ContentUtils.ExtractFscContentVisitor`)

* **One file:**
  * Save dialog defaults to the case `Export` dir, or the last user-chosen dir if it still exists (session memory).
  * Default name: `FileUtil.escapeFileName(name)`, which replaces `[\p{Cntrl}/:"*?<>|]+` with `_` and trims.
* **Many files:**
  * Pick a folder (created if missing). Each file goes to `<folder>/<objId>-<escapedName>`.
  * Duplicate selections are deduplicated by object.
* **Overwrite:** if the destination exists, confirm. If deleting the old file fails, show "cannot overwrite" and skip it.
* `.` and `..` entries are skipped. If nothing is left: "No file(s) to extract."
* Runs as a background task with a cancellable progress bar ("Extracting {destName}", per-file "Extracting {name}").
* Recursion:
  * Directories become directories; children are extracted recursively.
  * If a child file already exists, it is written as `<objId>-<name>`.
  * The buffer size is 8192 bytes.
  * Slack files, local files, derived files and layout files are all supported.
* Done message: "File(s) extracted." or "Error extracting files: {0}".

### 5.7 Extract Unallocated Space (`ExtractUnallocAction`)

* Menu items:
  * "Extract Unallocated Space to Single File" on a Volume.
  * "Extract Unallocated Space to Single Files" on an Image: one file per volume, or per image if there is no volume system.
* Output file name: `<ImgName>-Unalloc-<ImgObjectID>-<VolumeID>.dat` in a user-chosen directory ("Select directory to save to"; default is the case Export dir).
* Gathers `LayoutFile`s from the `$Unalloc` virtual directory under each file system (or directly under a volume or image). They are sorted by obj_id and their bytes concatenated.
* Progress: "processing {0} of {1} MBs" (MB = 1024²).
* Prevents concurrent runs on the same image or volume: "Unallocated Space is already being extracted on this Image..." and "Already extracting unallocated space into {0} - will skip this volume".
* Existing output: "The Unalloc File for this volume, {0} already exists, do you want to replace it?"
* No files: "No unallocated files found on volume".
* Done: "Completed extraction of unallocated space." / "Files were extracted to {0}".

### 5.8 External viewer (`ExternalViewerAction`, `ExternalViewerShortcutAction`, `ExternalViewerRulesManager`, Options "External Viewer")

* Disabled if: size is 0, there is no extension, the item is a slack file, or the extension is executable (`.exe .dll .com .bat .msi .reg .scr .cmd`). Executables show "The file is an executable and will not be opened."
* Algorithm:
  1. Write the content to `<caseTemp>/<name><random><.ext>` (deleted on exit).
  2. Look up a rule by **MIME type** first, then by extension (rule name `.ext` or `ext` exactly as stored).
  3. If a rule matches, run `exePath file`.
  4. Otherwise use the OS default handler (`Desktop.open`). Paths containing "http" are opened as URLs.
* Errors:
  * "There is no associated editor for files of this type or the associated application failed to launch."
  * "This platform (operating system) does not support opening a file in an editor this way."
  * "The file no longer exists."
  * "Permission to open the file was denied."
* **Rules storage:** ModuleSettings `ExternalViewerRules.properties`, key `Rules`, format `name>exePath>MIME|name>exePath>EXT|…` (`RuleType` is MIME or EXT).
* Options panel:
  * "Add your custom rules for external viewers:", a table of rules, and New Rule / Edit Rule / Delete Rule.
  * The rule dialog has a "MIME type" or "Extension" radio button, a name field and the program path.
  * Validation: "The MIME type is invalid. Add your custom types in the File Types options panel.", "The extension is invalid.", "The path to the program executable is invalid", "A rule already exists with this MIME type or extension. Please edit that one."
  * Also "HxD Editor Path:" (`ExternalHexEditorPath`, default `C:\Program Files\HxD\HxD.exe`) for "Open in hex editor".

### 5.9 Export CSV (`ExportCSVAction`)

* "Export Selected Rows to CSV" on the selected result rows.
* Save dialog with a "csv file" filter; `.csv` is appended if missing. If the file exists: "File {0} already exists" (no overwrite).
* Columns are the union of property display names over the **first 100** rows (sampling), excluding S, C and O.
* Each row is `"v1","v2",…\n` with `"` escaped as `\"`. Note: this is not RFC 4180.
* Background task with progress "Exporting to CSV file". Done: "Wrote to {0}".

### 5.10 Details dialogs

* **Image Details** ("Image Information"): Name, Type, Sector Size, Total Size, Hash Value.
* **Volume Details** ("General Volume Information"): Volume ID, Starting Sector, Length in Sectors, Description, Flags.
* **File System Details**: FileSystem Type, Image Offset (bytes), Volume ID, Block Size (bytes), Block Count, Root Metadata Entry, First Metadata Entry, Last Metadata Entry.

---

## 6. Events (`A/events/**`, `A/casemodule/events/**`, `Case.Events`)

### 6.1 Infrastructure

* `AutopsyEvent extends PropertyChangeEvent implements Serializable`: (eventName, oldValue, newValue) plus `SourceType` LOCAL or REMOTE. `getSource()` returns the source type string.
* `AutopsyEventPublisher` (one static instance in `Case`):
  * **Local**: `LocalEventPublisher` holds `Map<eventName, Set<listener>>` (concurrent). It delivers **synchronously on the publishing thread**, one listener after another, and logs listener exceptions.
  * **Remote** (multi-user only): `RemoteEventPublisher` uses ActiveMQ/JMS.
    * Topic name = channel name (`<caseUniqueName>-Case-Events`, plus a separate `<caseName>-Collaboration-Monitor-Events` publisher).
    * Messages are `ObjectMessage`s carrying the Java-serialized `AutopsyEvent`, with string property `events='All'` and consumer selector `events = 'All'`.
    * `noLocal=true`, so a host does not receive its own messages. `NON_PERSISTENT` delivery, `AUTO_ACKNOWLEDGE`.
    * Received events get `sourceType=REMOTE` and are republished locally.
  * `publish()` = publishLocally + publishRemotely. `publishRemotely` tries once (`MAX_REMOTE_EVENT_PUBLISH_TRIES=1`) and reopens the channel if it was closed after an error.
* Connection: `MessageServiceConnectionInfo(host, port default 61616, user, password)`, URI `tcp://%s:%s?wireFormat.maxInactivityDuration=0`.
  * `tryConnect()` diagnoses errors by message prefix: "connection timed out" means a bad host, "connection refused" a bad port, "user name [" bad credentials; otherwise an `InetAddress.isReachable(1000 ms)` check decides.
* **Lazy payloads:** most data-model events (`TskDataModelChangedEvent`) serialize only **IDs**. Objects are `transient` and re-fetched from the case DB on first `getOldValue()`/`getNewValue()` (`getNewValueObjects(caseDb, ids)`). Remote listeners therefore get fresh DB objects.

### 6.2 `Case.Events` catalog (event name = enum name)

| Event | old value | new value | Published by / notes |
|---|---|---|---|
| NAME (deprecated) | old display name | new | `updateCaseDetails` |
| NUMBER (deprecated) | old number | new | also (bug) used for examiner change |
| EXAMINER (deprecated) | — | — | never published |
| ADDING_DATA_SOURCE | null | null; `getEventId()` UUID | wizard start (`AddingDataSourceEvent`) |
| ADDING_DATA_SOURCE_FAILED | null | null; `getAddingDataSourceEventId()` | DSP returned no content |
| DATA_SOURCE_ADDED | null | Content (lazy, by obj id); `getAddingDataSourceEventId()` | DSP finished |
| DATA_SOURCE_DELETED | data source obj id | null | data source removal |
| DATA_SOURCE_NAME_CHANGED | null | Content; `getNewName()` | `notifyDataSourceNameChanged` (API only) |
| CURRENT_CASE | closed Case / null | opened Case / null | **local only** |
| REPORT_ADDED | null | List<Report> (lazy) | `Case.addReport` |
| REPORT_DELETED | Report | null | `Case.deleteReports` |
| BLACKBOARD_ARTIFACT_TAG_ADDED | List<DeletedBlackboardArtifactTagInfo> (tags removed by the same tag set) or null | BlackboardArtifactTag (lazy by tag id) | TagsManager |
| BLACKBOARD_ARTIFACT_TAG_DELETED | DeletedBlackboardArtifactTagInfo{tagID, name(TagName), comment, contentID, artifactID} | null | TagsManager |
| CONTENT_TAG_ADDED | List<DeletedContentTagInfo> or null | ContentTag (lazy) | TagsManager |
| CONTENT_TAG_DELETED | DeletedContentTagInfo{tagID, name, comment, contentID, beginByteOffset, endByteOffset} | null | TagsManager |
| CASE_DETAILS | old CaseDetails | new CaseDetails | `updateCaseDetails` |
| TAG_DEFINITION_CHANGED | changed tag display name | null | Tags options when the notable flag changes |
| TIMELINE_EVENT_ADDED | — | TimelineEvent | TSK TimelineManager |
| CR_COMMENT_CHANGED | null | new comment; `getContentID()`, `getCaseName()` | Central Repository comment edit |
| OS_ACCOUNTS_ADDED / UPDATED | — | List<OsAccount> (lazy) | TSK EventBus |
| OS_ACCOUNTS_DELETED | List<Long> obj ids | — | TSK |
| OS_ACCT_INSTANCES_ADDED | — | List<OsAccountInstance> | TSK |
| HOSTS_ADDED / UPDATED | — | List<Host> | TSK |
| HOSTS_DELETED | List<Long> host ids | — | TSK |
| PERSONS_ADDED / UPDATED | — | List<Person> | TSK |
| PERSONS_DELETED | List<Long> person ids | — | TSK |
| HOSTS_ADDED_TO_PERSON | [Person] | List<Host> | TSK |
| HOSTS_REMOVED_FROM_PERSON | [Person] | List<Long> host ids | TSK |
| TAG_NAMES_ADDED / UPDATED | — | List<TagName> | TSK TaggingManager |
| TAG_NAMES_DELETED | List<Long> ids | — | TSK |
| TAG_SETS_ADDED | — | List<TagSet> | TSK |
| TAG_SETS_DELETED | List<Long> ids | — | TSK |

* Subscription API: `Case.addEventTypeSubscriber(Set<Events>, listener)` / `removeEventTypeSubscriber`. `addPropertyChangeListener` subscribes to all events.
* TSK-originated events arrive on TSK's Guava EventBus (`caseDb.registerForEvents`) and are re-published as Autopsy events, local and remote.
* Ingest events are separate: `IngestManager.IngestModuleEvent` (DATA_ADDED(ModuleDataEvent{moduleName, artifactType, artifacts}), CONTENT_CHANGED(ModuleContentEvent{source content}), FILE_DONE) and `IngestJobEvent` (STARTED, COMPLETED, CANCELLED, DATA_SOURCE_ANALYSIS_STARTED/COMPLETED). These are the main tree-refresh drivers and are owned by the ingest inventory.
* `core/events/ServiceEvent`: services-monitor status events (§10.4).

---

## 7. Tags (`casemodule/services/TagsManager`, `TagNameDefinition`, `TagSetDefinition`, `TagOptionsPanel`, `TagNameDialog`, `contentviewertags`, `actions/*Tag*`, `tags/TagUtils`)

### 7.1 Model

* **TagName** (TSK table `tag_names`): id, displayName (unique), description, color (`HTML_COLOR`), tagType (`BAD` = notable, `SUSPICIOUS` = normal), tagSetId, rank.
* **TagSet** (`tsk_tag_sets`): id, name, ordered TagNames. **Tags in one set are mutually exclusive on an item**: TSK `TaggingManager.addContentTag/addArtifactTag` removes the other tags of the same set from the item and returns them as `removedTags`. They appear in the event's old value.
* **ContentTag**: id, content (obj), tagName, comment, beginByteOffset/endByteOffset (−1 for whole-file; byte ranges are used by text highlighting), examiner (userName = OS login).
* **BlackboardArtifactTag**: id, artifact, content (source), tagName, comment, examiner.
* **Standard tags** (created in every case):

  | Name | Type | Color |
  |---|---|---|
  | "Bookmark" | SUSPICIOUS | NONE |
  | "Follow Up" | SUSPICIOUS | NONE |
  | "Notable Item" | BAD | NONE |

* **Project VIC tag set "Project VIC"** (created by the ImageGallery service on case open):

  | Name | Color | Type |
  |---|---|---|
  | "Non-Pertinent" | GREEN | SUSPICIOUS |
  | "Child Abuse Material - (CAM)" | RED | BAD |
  | "Child Exploitive (Non-CAM) Age Difficult" | YELLOW | BAD |
  | "CGI/Animation - Child Exploitive" | FUCHSIA | BAD |
  | "Comparison Images" | BLUE | SUSPICIOUS |

  Legacy "CAT-0: Uncategorized" … "CAT-5: Non-pertinent" names are purged from user settings on upgrade (`DhsImageCategory` keeps legacy colors: CAT-1 red, 2 orange, 3 yellow, 4 bisque, 5 green, 0 light grey).
* **User-defined tag names** persist globally in ModuleSettings `Tags.properties`:
  * key `TagNames` = `name,description,COLOR,TAGTYPE;name,…`
  * key `CustomTagNameVersion=2`
  * v1 stored FileKnown in the 4th field. Upgrade maps it, using the Central Repository `db.badTags` list for 3-field entries, and drops standard and Project VIC names.
* **User-defined tag sets**: `<userConfig>/tags/<name>-tag-set.json`, Gson-serialized `{name, tagNameDefinitionList:[{displayName, description, color, tagType}]}`.
* **Case init** (`TagsManager` constructor):
  1. If the case has no tag sets yet: add the standard tag names, then every user JSON tag set (`addOrUpdateTagName` + `addTagSet`).
  2. Cache all tag names by display name.
  3. Then `saveToCase` every user-defined TagNameDefinition.
  4. Keep the cache in sync via `TAG_NAMES_ADDED/UPDATED/DELETED`.
* **Name validation** (`containsIllegalCharacters`): `\ : * ? " < > | , ;` are not allowed. Descriptions may not contain `,` or `;` (they break the settings format). Names must be non-empty and unique ("Tag name must be unique. A tag with this name already exists.").
* **Notable semantics:**
  * Tag type BAD shows " (Notable)" in menus and tree labels.
  * The Central Repository listener marks correlation attributes of items tagged BAD as notable (outside this area).
  * Data source summary counts tags; the S column reflects tags.
  * Changing a tag's notable flag in options fires `TAG_DEFINITION_CHANGED(name)`. The CR then re-flags existing tagged items.

### 7.2 TagsManager API (all via TSK)

* `getAllTagNames()` (cached), `getTagNamesInUse()`, `getTagNamesInUse(dsId)`, `getTagNamesInUseForUser([dsId,] user)`. The ForUser variants load **all tags** into memory and filter in Java.
* `getDisplayNamesToTagNamesMap()`.
* `addTagName(name[, desc[, color[, tagType]]])`: defaults are "", NONE, SUSPICIOUS. It also persists the definition to user settings. Throws `TagNameAlreadyExistsException`.
* `addContentTag(content, tagName[, comment[, begin, end]])` publishes `CONTENT_TAG_ADDED(newTag, removedTags)`. `deleteContentTag` publishes `CONTENT_TAG_DELETED`.
* `addBlackboardArtifactTag(artifact, tagName[, comment])` and `deleteBlackboardArtifactTag` work the same way.
* Getters:
  * `getContentTagsByContent`, `getContentTagsByTagName(tagName[, dsId])`, `getContentTagsCountByTagName(tagName[, dsId])`.
  * `getBlackboardArtifactTagsByArtifact`, `…ByTagName`, `…CountByTagName`.
  * ForUser variants.
  * `getAllContentTags`, `getAllBlackboardArtifactTags`.
  * `getContentTagByTagID`, `getBlackboardArtifactTagByTagID`.
* Tag sets: `getAllTagSets`, `getTagSet(tagName)`, `addTagSet(name, list)`, static `addTagSetDefinition(def)` (writes the JSON).

### 7.3 UI

* **"Add File Tag" / "Add File Tags"** (plural when more than one file is selected) and **"Add Result Tag(s)"**. Submenu layout:
  1. Non-standard, non-set tag names sorted by name (TreeMap). Each item label is `name` + `" (Notable)"` if BAD.
  2. —
  3. Standard tags; "Bookmark" shows accelerator **Ctrl+B**.
  4. One submenu per tag set (e.g. "Project VIC").
  5. —
  6. "Tag and Comment...": dialog with "Tag:" combo, "Comment:" field, [New Tag], OK/Cancel; Esc cancels.
  7. "New Tag...": `GetTagNameDialog` with a "Pre-existing Tag Names:" list and a "New Tag" panel ("Tag Name:", "Description:", "Tag indicates item is notable."). OK creates the tag and applies it.
* Tagging runs on a background thread per action.
  * Tagging `.` tags the parent directory instead. Tagging `..` tags the grandparent.
  * If tagging is impossible: "Unable to tag {0}, not a regular file." or "Unable to tag {0}."
  * Re-applying an existing tag: "{0} has been tagged as {1}. Cannot reapply the same tag." (enforced by TSK).
* **Ctrl+B "Bookmark"** (`AddBookmarkTagAction`): if any artifacts are selected, tag the artifacts; otherwise tag the files.
* **"Remove File Tag"** submenu lists the tags on the selected file (`getContentTagsByContent`). **"Remove Result Tag"** does the same for artifacts. Both are disabled when there are no tags.
* In the Tags tree: "Remove Selected Tag(s)" and **"Replace Selected Tag(s) With"**. The latter is the same menu as Add, with the tag names already on the item disabled, plus New Tag... and Tag and Comment...; it deletes the old tag and adds the new one with the same comment.
* **Options > "Custom Tags"** (`TagOptionsPanel`):
  * "Tag Names:" list showing only user-defined, non-standard names.
  * New Tag / Edit Tag / Delete Tag.
  * Shows "Tag Description:" and "Tag indicates item is notable:".
  * Delete removes only the definition from settings; existing case tag names remain.
  * Editing existing tags is blocked while ingest runs: "Cannot make changes to existing tags when ingest is running!"
  * The New/Edit dialog (`TagNameDialog`) has "Name:", "Description:" and "Tag indicates item is notable."; the color is always NONE.

### 7.4 Content viewer tags (image region tags) (`contentviewertags/ContentViewerTagManager`)

* Per-case table created on case open if missing:
  ```sql
  CREATE TABLE beta_tag_app_data (app_data_id INTEGER PRIMARY KEY /* BIGSERIAL on PostgreSQL */,
    content_tag_id INTEGER NOT NULL, app_data TEXT NOT NULL,
    FOREIGN KEY(content_tag_id) REFERENCES content_tags(tag_id))
  ```
* `app_data` is JSON (Gson) of a viewer-specific object, e.g. `ImageTagRegion{x, y, width, height, scale, rotation, …}` used by the image viewer to draw tag rectangles.
* CRUD via `CaseDbAccessManager` insert/update/select/delete:
  * Insert: `INSERT (content_tag_id, app_data) VALUES (%d, '%s')`.
  * Select: `* FROM beta_tag_app_data WHERE content_tag_id = %d`.

---

## 8. Menus, toolbar, shortcuts, actions (`A/core/layer.xml`, `@ActionReference`s, `A/actions/**`, `A/menuactions/**`)

### 8.1 Main menu bar

**Case**:

| Pos | Label | Action / notes |
|---|---|---|
| 100 | New Case… | `CaseNewAction` → NewCaseWizardAction, **Ctrl+N** |
| 101 | Open Recent Case ▸ | up to 5 recent entries, "Clear Recent Cases"; "-Empty-" when none |
| 102 | Open Case… | `CaseOpenAction`, **Ctrl+O** |
| 103 | Unpack and Open Portable Case | |
| 104 | Close Case | |
| 105 | Delete Case | |
| 150 | — | |
| 151 | Manage Hosts | `OpenHostsAction`, case open |
| 201 | Add Data Source | |
| 202 / 300 | — | |
| 301 | Case Details | |
| 302 | Data Source Summary | owned by the datasourcesummary area |
| 1000 | Exit | `ExitAction`; if ingest is running, confirm "Ingest is Running" / "Ingest is running, are you sure you want to exit?". Closes the case, then exits. |

**View**: NetBeans Toolbars list.

**Tools**:

* Image/Video Gallery (101)
* Communications (102)
* Geolocation (103)
* Timeline (104)
* Discovery (105)
* [Auto Ingest Dashboard (106), experimental]
* "File Search by Attributes" (200)
* Search All Cases (201)
* Common Properties Search (202)
* —
* Run Ingest Modules ▸ (300)
* Generate Report (301)
* Python Plugins (1400): opens `<userdir>/python_modules`
* Personas (1825)
* Create Live Triage Drive (1850)
* "Open Case Folder" (1900): opens the case output dir in the OS file manager; without a case: "No open case, therefore no current case folder available."
* Create Logical Imager (2000)
* Plugins
* Options

**Window**:

* "DataResult Windows" ▸ (`DataResultMenu`/`SearchResultMenu`: "Data Results" lists the open result tabs).
* "Data Content Windows" ▸ (`DataContentDynamicMenu`: "Main" plus every "View in New Window" viewer).
* Reset Windows (205): restarts the UI and re-opens the case via the reopen file.

**Help**:

* Online Help (0)
* Offline Help (1)
* "Ingest Status Details" (1125, `ShowIngestProgressSnapshotAction`, title "Get Ingest Progress Snapshot")
* Performance (1437)
* "Thread Dump" (1750): writes a JVM thread dump to `<logDir>/ThreadDump_<yyyy_MM_dd_HH_mm_ss>.txt` (case Log dir if a case is open) and opens it
* "Open Log Folder" (2000): the case Log dir if a case is open, else the global log dir
* About (3000)

### 8.2 Toolbar ("Case" toolbar)

In order:

1. "Add Data Source"
2. Image Gallery
3. Communications
4. Geolocation
5. Timeline
6. Discovery
7. Generate Report
8. "Close Case"
9. spacer

The separate "Ingest" toolbar holds the ingest inbox / messages button.

### 8.3 Shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+N | New case |
| Ctrl+O | Open case |
| Ctrl+E | Open in External Viewer (selected file) |
| Ctrl+B | Bookmark selected files or results |

### 8.4 Misc

* `IngestRunningCheck.checkAndConfirmProceed(title, msg)`: Yes/No confirm when ingest is running. Used before new, open and close case, Add Data Source and exit.
* `ViewArtifactAction` ("View Source Result", `viewArtifact`); `ViewOsAccountAction` ("View Source OS Account").
* Startup also launches `ServicesMonitor` notifications (§10.4).

---

## 9. File Search by Attributes (`A/filesearch/**`)

* Opened from "Tools > File Search by Attributes" or "Open File Search by Attributes" on a data source (which preselects that data source).
* Non-modal dialog. Header: "Search for files that match the following criteria:".
* Each filter area has an enable checkbox; at least one filter must be enabled ("At least one filter must be selected.").
* Buttons: [Search], [Close]. Validation errors appear inline: "Validation Error: {0}".

| Filter | UI | SQL predicate (on `tsk_files`) |
|---|---|---|
| Name | "Name:" text. *Note: case-insensitive substring of the file name only (not the parent path); regex not supported* | `LOWER(name) LIKE LOWER('%<kw>%')`. **Bug:** the quote escaping result is discarded, so `'` breaks the query. |
| Size | "Size:" with operator {"equal to", "greater than", "less than"}, a number, and units {"Byte(s)","KB","MB","GB","TB"} | `size <op> <n * 2^(10*unitIndex)>`. The number must be a non-negative integer (US locale, grouping allowed). |
| Date | "Date:" from/to (`MM/dd/yyyy`, empty = no limit), "Timezone:" (case data-source zones first, then a separator, then all zones), checkboxes "Modified", "Changed", "Accessed", "Created" | `NULL OR (mtime BETWEEN f AND t) OR (ctime …) OR (atime …) OR (crtime …)`. Dates are parsed in the selected TZ; empty means 0. **Bug:** the end date is the start of day and an empty "to" means 0. Errors: "The end date should be after the start date.", "At least one date type checkbox must be selected." |
| Known Status | "Known Status:" checkboxes "Unknown", "Known (NSRL or other)", "Notable" | `NULL OR known = 0 OR known = 1 OR known = 2` (only the checked ones) |
| MD5 / SHA-256 | "MD5:" / "SHA-256:" hex | `md5 = '<lower>'`, `sha256 = '<lower>'`; both combine with AND. Length must be 32 or 64 hex digits. |
| MIME Type | "MIME Type:" multi-select list of `FileTypeDetector.getDetectedTypes()` (Tika plus custom) | `mime_type = 'a' OR mime_type = 'b'` |
| Data Source | "Data Source:" multi-select (*Multiple data sources can be selected*) | `data_source_obj_id = 'X' OR …` |
| Deleted | "Deleted" checkbox | the Deleted-Files "All" predicate (§3.3.3) |

* Final query: `(p1) AND (p2) …` via `findAllFilesWhere`.
* Results open in a **new result window** titled "File Search Results {n}", path "Filename Search Results:", initially "Performing file search by attributes. Please wait."
* A new search cancels a previous one still running.
* More than 10,000 matches triggers the notification "File Search: {0} matches found" / "Large number of matches may impact performance on some operations".
* No matches: "No results found." Cancelled: "Search Was Cancelled".
* Rows are `SearchNode` children (file nodes in non-directory-browse mode, so "View File in Directory" is present).

---

## 10. Core settings and utilities (`A/core/**`, `A/coreutils/**`, `A/machinesettings/**`)

### 10.1 Storage locations

* User dir: NetBeans `Places.getUserDirectory()`, e.g. `%APPDATA%/autopsy` or `~/.autopsy/dev`.
  * `getUserConfigDirectory()` = `<userdir>/config`
  * `getModuleConfigDirectory()` = `<userdir>/config/ModuleConfig`
  * `getLogDirectory()` = `<userdir>/var/log/`
  * Python plugins: `<userdir>/python_modules`
  * Object-detection classifiers: `<userdir>/object_detection_classifiers`
  * OCR language packs: `<install>/ocr_language_packs/*.traineddata`
* **ModuleSettings:**
  * One Java `.properties` file per "module name" at `<userConfig>/<module>.properties`, e.g. `Case.properties` (MAIN_SETTINGS = "Case"), `Tags.properties`, `ExternalViewerRules.properties`, `AutoIngest.properties`.
  * API: `getConfigSetting(module, key)`, `setConfigSetting`, `setConfigSettings(map)`, `settingExists`, `removeProperty`. Each write rewrites the file with a comment header ("Set <key>").
  * `DEFAULT_CONTEXT = "GeneralContext"`. Key `Current_Case_Type` stores the last case type.
* **UserPreferences** are stored in four `ConfigProperties` files (Java Preferences-like API with change listeners):

  | File | Contents |
  |---|---|
  | `<ModuleConfig>/ViewPreferences.properties` | Shared view settings |
  | `<userConfig>/MachineSpecificPreferences.properties` | Machine-specific settings |
  | `<userConfig>/ModePreferences.properties` | App mode |
  | `<ModuleConfig>/ExternalServicePreferences.properties` | Multi-user and external services |

  * Legacy `<userConfig>/Preferences/org/sleuthkit/autopsy/core.properties` is migrated on first run (`updateConfig`).
  * `addChangeListener` notifies on any key; the tree and nodes listen.

### 10.2 UserPreferences: keys, defaults, UI labels

| Key | Type / default | Meaning (UI label in Options > View or the tree View Options popup unless noted) |
|---|---|---|
| `KeepPreferredViewer` | bool false | "When selecting a file:" "Stay on the same file viewer" (true) vs "Change to the most specific file viewer" |
| `HideKnownFilesInDataSourcesTree` | bool **false** | "Hide known files (i.e. those in the NIST NSRL) in the:" "Data Sources area (the directory hierarchy)" |
| `HideKnownFilesInViewsTree` | bool **true** | same, "Views area" |
| `HideSlackFilesInDataSourcesTree` | bool **true** | "Hide slack files in the:" "Data Sources area" |
| `HideSlackFilesInViewsTree` | bool **true** | "Views area" |
| `DisplayTimesInLocalTime` | bool **true** | "When displaying times:" "Use local time zone" vs "Use another time zone" |
| `TimeZoneForDisplays` | string, default `GMT` id | the other zone (stored as an ID; the UI shows `(GMT±h:mm) ID`) |
| `GroupItemsInTreeByDataSource` | bool false | legacy global; superseded by the per-case CasePreferences (radio buttons "Group by Data Type" / "Group by Person/Host" under "Current Case Settings") |
| `ShowOnlyCurrentUserTags` | bool false | "Hide other users' tags in the:" "Tags area in the tree" |
| `HideCentralRepoCommentsAndOccurrences` | bool false | "Do not add columns for:" "S(core), C(omments), and O(ccurences)" "to reduce loading times" |
| `DisplayTranslatedNames` | bool **true** | "Add column in result viewer for file name translation" (effective only if a translator is configured) |
| `ExternalHexEditorPath` | string `C:\Program Files\HxD\HxD.exe` | External Viewer options "HxD Editor Path:" |
| `ResultsTablePageSize` | int **10000** | "Maximum number of Results to show in table:" (per page) |
| `NumberOfFileIngestThreads` | int **2** | Options > Application/Ingest |
| `ProcessTimeOutEnabled` / `ProcessTimeOutHours` | bool false / int **60** | external process timeout (ExecUtil terminators) |
| `MaximumNumberOfLogFiles` | int **10** | rotated log count |
| `SolrMaxJVMSize` | int MB, 2048 (64-bit) / 512 (32-bit) | local Solr heap |
| `HealthMonitorReportPath` | string "" | |
| `GeolocationOsmZipPath`, `GeolcoationMBTilesFilePath` (sic), `GeolocationOsmServerAddress`, `GeolocationTileOption` (int 0) | | geolocation tiles |
| `AppName` (Mode prefs) | default `Version.getName()` ("Autopsy") | branding; used in titles and temp paths |
| `AutopsyMode` (ModuleSettings `AutoIngest`) | ordinal of `SelectedMode{STANDALONE, AUTOINGEST}` | |
| `IsMultiUserModeEnabled` | bool false (and `isMultiUserSupported()`) | Options > Multi-User "Enable multi-user cases" |
| `ExternalDatabaseHostnameOrIp`, `ExternalDatabasePortNumber` (**5432**), `ExternalDatabaseUsername`, `ExternalDatabasePassword` (encrypted), `ExternalDatabaseType` (`POSTGRESQL`), `ExternalDatabaseName` | | PostgreSQL connection for multi-user case DBs |
| `Solr8ServerHost`, `Solr8ServerPort` (**8983**); `IndexingServerHost`, `IndexingServerPort` (Solr 4 legacy); `ZookeeperServerHost`, `ZookeeperServerPort` (**9983**) | | multi-user Solr + coordination service |
| `MessageServiceHost`, `MessageServicePort` (**61616**), `MessageServiceUser`, `MessageServicePassword` (encrypted) | | ActiveMQ |
| `TextTranslatorName` | null | machine translation provider; "Translate text:" |
| `OcrTranslationEnabled` | bool true | |

* Password "encryption" (`coreutils/TextConverter`):
  * `PBEWithMD5AndDES`, key chars `"hgleri21auty84fwe"`, salt `DE 33 10 12 DE 33 10 12`, 20 iterations, Base64 output.
  * The key is hard-coded, so this is obfuscation only.
* `UserMachinePreferences` (NbPreferences): `TempDirChoice` (SYSTEM / CASE / CUSTOM, default SYSTEM) and `TempDirectory`. A custom dir must exist and be read/writable.

### 10.3 Logging (`coreutils/Logger`)

* `java.util.logging` wrapper.
* File handler: `<logDir>/autopsy.log` (rotated `autopsy.log.0..N-1`, N = `MaximumNumberOfLogFiles`). Size limit 0 = unlimited per file; rotation happens at startup. Encoding is the platform log encoding.
* While a case is open, the handler is switched to `<case>/[host/]Log/autopsy.log`. It is switched back on close.
* Record format:
  ```
  <java.sql.Timestamp> <sourceClass> <sourceMethod>
  <LEVEL>: <message>
  <throwable.toString()>
  	<stack frame>…   (for each cause; note: prints the *root* thrown's frames for every cause — bug)
  ```
* A console handler is also attached.

### 10.4 Services monitor (`core/ServicesMonitor`)

* Services: `REMOTE_CASE_DATABASE`, `REMOTE_KEYWORD_SEARCH`, `MESSAGING`. Statuses: `UP`, `DOWN`.
* Checks run every **15 min** in multi-user mode:
  * DB: open a JDBC connection to PostgreSQL.
  * Solr: ping.
  * Messaging: `MessageServiceConnectionInfo.tryConnect`.
* Status changes publish a `ServiceEvent` and show notifications: "restored", "failed", or a generic status-change message.
* Other code can call `setServiceStatus(service, status, details)` / `getServiceStatus(service)`.

### 10.5 Startup (`core/Installer`, `datamodel/Installer`, `AutopsyOptionProcessor`, `RuntimeProperties`)

* Loads native libraries: on Windows the MSVC/UCRT `api-ms-win-*` DLLs, zlib, libewf, libvmdk, libvhdi, libtsk_jni and others. Sets the temp dir `<tmp>/<AppName sanitized>_tmp`-style suffix.
* TSK library lock (`LibraryLock`): if held by an older app, show "Another forensics application is running that uses The Sleuth Kit. You must close that application before launching Autopsy." and exit.
* Command-line option `-l/--liveAutopsy[=path]` sets `RuntimeProperties.runningInTarget` (live triage mode) and the base case dir.
* `RuntimeProperties.runningWithGUI` is false for command-line ingest and headless runs. All GUI code is guarded by it; there is a `LoggingProgressIndicator` for headless mode.

### 10.6 Time handling (`coreutils/TimeZoneUtils`, `TimeStampUtils`, `ContentUtils`)

* All DB times are **epoch seconds UTC**. 0 means "not set".
* Display: `TimeZoneUtils.getFormattedTime(epoch)` uses TSK `TimeUtilities.epochToTime(epoch, tz)` with format `yyyy-MM-dd HH:mm:ss z`; epoch ≤ 0 shows `0000-00-00 00:00:00` [inferred from TSK]. The zone is the local zone if `DisplayTimesInLocalTime`, else `TimeZoneForDisplays`. ISO variant: `yyyy-MM-dd'T'HH:mm:ss'Z'`.
* Zone list strings: `String.format("(GMT%+d:%02d) %s", hours, minutes, id)` using the raw offset, sorted by offset then ID, over all Java zone IDs. Parse back by taking the substring after the first space.
* `convertToAlphaNumericFormat(id)`: e.g. `EST5EDT`-style strings, passed to TSK as the image time zone for FAT.
* `TimeStampUtils`: suffix `_yyyy_MM_dd_HH_mm_ss` (20 characters; regex `\d{4}_\d{2}_\d{2}_\d{2}_\d{2}_\d{2}$`), with create, remove and get helpers. Used for auto-ingest case names and dump files.
* `ContentUtils.getStringTime(epoch, content)` uses the format `yyyy-MM-dd HH:mm:ss z`.

### 10.7 Platform and process utilities

* **PlatformUtil:**
  * OS name, version and arch; `isWindowsOS/isLinuxOS/isMacOS`; `is64BitOS/JVM`.
  * Physical drive and partition enumeration (§2.4).
  * PID lookup and `killProcess` (via Sigar/OS).
  * Memory info strings (JVM max/total/free, physical).
  * `extractResourceToUserConfigDir`.
  * `getOSFilePath` (Wine/Windows path translation).
  * Default charset and log encoding.
* **ExecUtil:**
  * Runs a `ProcessBuilder` and polls `waitFor(5 s)`. Each interval it asks a `ProcessTerminator`; terminators include a timed one (`TimedProcessTerminator`, using `ProcessTimeOutHours` when enabled), a cancellation one, and hybrid.
  * On terminate: destroy the process tree (`destroyForcibly` children), then wait up to 1 min.
  * Stream redirection to strings or files.
* **ThreadUtils:** `shutDownTaskExecutor` with a 5 s timeout.
* **NetworkUtils:**
  * `getLocalHostName()`: `InetAddress.getLocalHost().getHostName()`, falling back to the `COMPUTERNAME` env var.
  * `extractDomain(url)`: host, then registrable domain via `DomainTokenizer` (bundled `public_suffix_list.dat`, wildcard `*` and exception `!` rules), validated by Apache `DomainValidator` or an IPv4 regex.
* **DriveUtils:**
  * `isPhysicalDrive(path)`: starts with `\\.\physicaldrive` or `/dev/`.
  * `isPartition(path)`: `\\.\X:`.
  * `driveExists`: reads 1 byte.
* **PathValidator:**
  * `isValidForCaseType`: multi-user rejects `C:` and UNC paths.
  * `isValidForRunningOnTarget`: live mode rejects C: on Windows and non-`/media|/mnt` on Linux.
* **UNCPathUtilities:** maps drive letters to UNC (`net use` parsing) for multi-user path normalization.
* **FileUtil:** `deleteDir` (recursive), `deleteFileDir`, `copyFile`, `copyFolder`, `escapeFileName`, `hasReadWriteAccess` (creates and deletes a temp file "Autopsy*").
* **DataSourceUtils.imageHasFileSystem(path[, password]):** `SleuthkitJNI.isImageSupported` / `testOpenImage`.
* **MessageNotifyUtil:**
  * `Message.info/warn/error`: modal dialog.
  * `Notify.info/warn/error(title, msg)`: NetBeans notification balloon, timestamped `MM/dd/yy HH:mm:ss z`; `clear()` on case close.
* **XMLUtil:** DOM create/load/validate against an XSD/save. **EscapeUtil:** URL encode/decode and HTML escape. **TextUtil:** RTL detection, Solr-valid UTF-8 char check.
* **StringExtract:**
  * Unicode-script-aware string extraction (scripts table from `StringExtract.properties`).
  * Min string length `MIN_CHARS_STRING = 4`. Also "Basic Latin only" mode. Used by the strings content viewer and keyword search.
* **textutils/EncodingUtils:**
  * Charset detection: Tika `CharsetDetector` if confidence ≥ **41**, else `Decodetect` if confidence ≥ **0.4**, else `UNKNOWN_CHARSET`.
* **ImageUtils / VideoUtils** (shared with the content viewers):
  * Thumbnail sizes: `ICON_SIZE_SMALL=50`, `ICON_SIZE_MEDIUM=100`, `ICON_SIZE_LARGE=200`.
  * Cache: `<case>/Cache/thumbnails/<objId>.png`.
  * Supported image types: ImageIO readers plus `tec`, minus `db`, plus extra MIME types; GIF handled specially.
  * Image header sniffing.
  * Video thumbnails via OpenCV at frame positions 50%, 25%, 75%, 1%.
  * The default thumbnail is `file-icon.png`.
* **FileTypeUtils.FileTypeCategory:** IMAGE, VIDEO, AUDIO, DOCUMENTS, EXECUTABLE, plus composites MULTI_MEDIA and VISUAL. Each has MIME-type sets and extension sets, used by Discovery and the gallery. `application/octet-stream` is explicitly excluded from images.
* **JLNK / JLnkParser / LnkEnums:** a Windows `.lnk` parser (header flags, file attributes, times, target size, IDList, LinkInfo with volume/network info, string data: name, relative path, working dir, arguments, icon). Used by RecentActivity and the content viewer.
* **SQLiteTableReader / AppSQLiteDB / SQLiteDBConnect:** read SQLite files found in evidence (copies them to temp along with `-wal`/`-journal`; streaming row callbacks).
* **History<T>:** generic back/forward stack (JavaFX observable), used by the Image Gallery and Timeline.
* **Version:** `getVersion()`, `getName()`, `getBuildType()` (DEVELOPMENT / RELEASE), Java and NetBeans versions.
* **ColorUtilities, StopWatch, Desktop** (AWT Desktop wrapper with a Linux xdg fallback), **Win32Process** (process tree via JNA), **ThreadConfined** (annotation), **ContextMenuExtensionPoint** (collects `ContextMenuActionsProvider` services, §5.5).

---

## 11. Case database schema (TSK), inferred from usage

TSK 4.15 creates and migrates the schema itself (version **9.x**; TSK stores it in `tsk_db_info.schema_ver/schema_minor_ver` and `tsk_db_info_extended`). The same DDL is used for SQLite (single-user `autopsy.db`) and PostgreSQL (multi-user, one DB per case). Columns marked * appear verbatim in Autopsy SQL; the others are [inferred] from TSK knowledge.

### 11.1 Object model

* **`tsk_objects`**(`obj_id`* PK, `par_obj_id`* → parent object, `type`* `ObjectType`).
  * Every Content (image, volume system, volume, pool, file system, file, artifact, report, OS account, host address) has a row here.
  * `par_obj_id` defines the tree; data sources have NULL parents.
* **`data_source_info`**(`obj_id`* PK = image or local-files root, `device_id`, `time_zone`*, `acquisition_details`, `added_date_time`, `acquisition_tool_settings`, `acquisition_tool_name`, `acquisition_tool_version`, `host_id` → tsk_hosts). Membership in this table is what makes something a data source.
* **`tsk_image_info`**(`obj_id`*, `type`* `TSK_IMG_TYPE_ENUM` (raw, split, EWF, VMDK, VHD, …), `ssize`* sector size, `tzone`*, `size`*, `md5`*, `sha1`, `sha256`, `display_name`*).
* **`tsk_image_names`**(`obj_id`*, `name`* path of each segment, `sequence`*).
* **`tsk_vs_info`**(`obj_id`*, `vs_type`* (DOS/GPT/MAC/BSD/SUN), `img_offset`*, `block_size`*).
* **`tsk_vs_parts`**(`obj_id`*, `addr`*, `start`* sectors, `length`* sectors, `descr`*, `flags`* alloc/unalloc/meta).
* **`tsk_pool_info`**(`obj_id`, `pool_type`) for APFS/LVM pools.
* **`tsk_fs_info`**(`obj_id`*, `data_source_obj_id`, `img_offset`*, `fs_type`*, `block_size`*, `block_count`*, `root_inum`*, `first_inum`*, `last_inum`*, `display_name`*).
* **`tsk_files`**:
  * Identity and structure: `obj_id`*, `fs_obj_id`*, `data_source_obj_id`*, `attr_type`*, `attr_id`*, `name`*, `meta_addr`*, `meta_seq`*.
  * Type and flags: `type`* `TSK_DB_FILES_TYPE_ENUM`, `has_layout`*, `has_path`*, `dir_type`*, `meta_type`*, `dir_flags`*, `meta_flags`*.
  * Metadata: `size`*, `ctime`*, `crtime`*, `atime`*, `mtime`* (epoch secs), `mode`*, `uid`*, `gid`*.
  * Hashes and classification: `md5`*, `sha256`*, `sha1`, `known`* `FileKnown`, `mime_type`*, `extension`*.
  * Paths: `parent_path`* (e.g. `/Windows/System32/`, always with a trailing slash).
  * Ownership and collection: `owner_uid`, `os_account_obj_id`, `collected` (CollectedStatus).
  * Indexes [inferred] exist on `(data_source_obj_id)`, `(mime_type)`, `(md5)`, `(parent_path)`, `(extension)`, `(known)` and others.
* **`tsk_files_path`**(`obj_id`*, `path`*, `encoding_type`*): the on-disk path for LOCAL/DERIVED files.
* **`tsk_file_layout`**(`obj_id`*, `byte_start`*, `byte_len`*, `sequence`*): byte runs for LAYOUT/CARVED/UNALLOC files within the image.
* **`tsk_files_derived`**(`obj_id`*, `derived_id`*, `rederive`*) and **`tsk_files_derived_method`**(`derived_id`*, `tool_name`*, `tool_version`*, `other`*).
* **`tsk_file_attributes`**(id, obj_id, attribute_type_id, value_type, value_byte, value_text, value_int32, value_int64, value_double): file-level attributes (TSK ≥ 4.11).
* **`file_encoding_types`**(encoding_type, name).
* **`reports`**(`obj_id`* (earlier `report_id`), `path`*, `crtime`*, `src_module_name`*, `report_name`*).

### 11.2 Blackboard

* **`blackboard_artifact_types`**(`artifact_type_id`*, `type_name`*, `display_name`*, `category_type`* 0=DATA_ARTIFACT / 1=ANALYSIS_RESULT [inferred]).
* **`blackboard_attribute_types`**(`attribute_type_id`*, `type_name`*, `display_name`*, `value_type`).
* **`blackboard_artifacts`**(`artifact_id`* PK, `obj_id`* source content, `artifact_obj_id`* this artifact's own `tsk_objects` id, `data_source_obj_id`*, `artifact_type_id`*, `review_status_id`*).
* **`blackboard_attributes`**(`artifact_id`*, `artifact_type_id`*, `source`* module name(s), `context`*, `attribute_type_id`*, `value_type`*, `value_byte`*, `value_text`*, `value_int32`*, `value_int64`*, `value_double`*). There are multiple rows per artifact and no PK; indexes are on `artifact_id`, `attribute_type_id` and (`attribute_type_id`, `value_text`) [inferred].
* **`tsk_analysis_results`**(`artifact_obj_id`*, `significance`*, `priority`, `conclusion`, `configuration`, `justification`, `ignore_score`).
* **`tsk_data_artifacts`**(`artifact_obj_id`, `os_account_obj_id`).
* **`tsk_aggregate_score`**(`obj_id`*, `data_source_obj_id`*, `significance`*, `priority`*).
* **`review_statuses`**(review_status_id, review_status_name, display_name).

### 11.3 Tags, examiners

* **`tag_names`**(`tag_name_id`*, `display_name`* UNIQUE, `description`*, `color`*, `knownStatus`* (holds the TagType), `tag_set_id`, `rank`).
* **`tsk_tag_sets`**(`tag_set_id`, `name`).
* **`tsk_examiners`**(`examiner_id`*, `login_name`, `display_name`).
* **`content_tags`**(`tag_id`*, `obj_id`*, `tag_name_id`*, `comment`*, `begin_byte_offset`*, `end_byte_offset`*, `examiner_id`*).
* **`blackboard_artifact_tags`**(`tag_id`*, `artifact_id`*, `tag_name_id`*, `comment`*, `examiner_id`).
* Autopsy-created: **`beta_tag_app_data`** (§7.4).

### 11.4 Hosts, persons, OS accounts (TSK managers; no direct SQL in Autopsy)

* **`tsk_persons`**(id, name).
* **`tsk_hosts`**(id, name, db_status ACTIVE/MERGED/DELETED, person_id, merged_into).
* `tsk_host_addresses`, `tsk_host_address_map`, `tsk_host_address_dns_ip_map`, `tsk_host_address_usage`.
* **`tsk_os_account_realms`**(id, realm_name, realm_addr (SID prefix), realm_signature, scope_host_id, scope_confidence, db_status, merged_into).
* **`tsk_os_accounts`**(os_account_obj_id, login_name, full_name, realm_id, addr (SID/UID), signature, status, type, created_date, db_status, merged_into).
* **`tsk_os_account_attributes`**(id, os_account_obj_id, host_id, source_obj_id, attribute_type_id, value_*).
* **`tsk_os_account_instances`**(id, os_account_obj_id, data_source_obj_id, instance_type LAUNCHED/ACCESSED/REFERENCED).

### 11.5 Ingest, communications, timeline

* **`ingest_jobs`**(`ingest_job_id`, `obj_id`* data source, `host_name`, `start_date_time`, `end_date_time`, `status_id`*, `settings_dir`).
* **`ingest_modules`**(ingest_module_id, display_name, unique_name, type_id, version).
* **`ingest_job_modules`**(ingest_job_id, ingest_module_id, pipeline_position).
* `ingest_module_types` and `ingest_job_status_types`.
* **`account_types`**(account_type_id, type_name, display_name).
* **`accounts`**(account_id, account_type_id, account_unique_identifier).
* **`account_relationships`**(relationship_id, account1_id, account2_id, relationship_source_obj_id, date_time, relationship_type, `data_source_obj_id`*).
* **`tsk_event_types`**(event_type_id, display_name, super_type_id).
* **`tsk_event_descriptions`**(event_description_id, full_description, med_description, short_description, data_source_obj_id, content_obj_id, artifact_id, hash_hit, tagged).
* **`tsk_events`**(event_id, event_type_id, event_description_id, time). The `known_status` column was removed in recent schemas (see the comment in `timeline/ui/filtering/FilterTreeItem.java`).
* **`tsk_db_info`**(`schema_ver`*, `tsk_ver`*, `schema_minor_ver`) and **`tsk_db_info_extended`**(name, value), e.g. `CREATION_SCHEMA_MAJOR_VERSION`/`MINOR` (used by `canDeleteDataSources` ≥ 8.4).

### 11.6 Key query patterns the UI depends on

1. **Tree child listing:**
   * Children of an object: `SELECT … FROM tsk_objects WHERE par_obj_id = ?` joined to `tsk_files` / `tsk_vs_parts` / … by type.
   * "Has children" checks.
   * The visible-children query in §5.1.
   * Postmortem should precompute child counts per parent, both visible and including hidden known/slack.
2. **Data-source lists** (`DataSourceLoader`):
   ```sql
   select obj_id, name from tsk_files where obj_id in (SELECT obj_id FROM tsk_objects WHERE obj_id in (select obj_id from data_source_info))
   select obj_id, name from tsk_image_names where obj_id in (SELECT obj_id FROM tsk_objects WHERE obj_id in (select obj_id from data_source_info))
   ```
3. **View counts and lists:**
   * Extension IN lists, MIME `GROUP BY mime_type`, deleted-flags predicates, size buckets (§3.3), all with optional `data_source_obj_id` and `known` filters.
   * Paged in memory today. Postmortem should use `ORDER BY … LIMIT/OFFSET` or keyset pagination.
4. **Artifact counts per type** (with or without a data source):
   * `SELECT artifact_type_id, COUNT(*) FROM blackboard_artifacts [WHERE data_source_obj_id=?] GROUP BY artifact_type_id`.
   * "Types in use": `SELECT DISTINCT artifact_type_id …`.
5. **Attribute-pivot grouping:** keyword lists/terms, hash and interesting set names, email account/folder (from TSK_PATH), account types, credit-card BIN/file.
   * All are done as a full scan of `blackboard_attributes` joined to `blackboard_artifacts` and filtered by `attribute_type_id`, then grouped in Java.
6. **Score:** `tsk_aggregate_score` filtered by significance and priority, joined to files and data artifacts (§3.7).
7. **Tags:** tag names in use, counts by tag name (optionally per data source or per user), tags by content, tags by artifact.
   * Data source summary counts tags per data source by joining `content_tags`→`tsk_files` and `blackboard_artifact_tags`→`blackboard_artifacts`.
8. **Data source summary counts:**
   ```sql
   SELECT data_source_obj_id, COUNT(*) AS value FROM tsk_files WHERE meta_type=1 AND type<>6 AND dir_type<>11 AND name<>'' GROUP BY data_source_obj_id
   SELECT data_source_obj_id, COUNT(*) AS value FROM blackboard_artifacts WHERE review_status_id != 2 GROUP BY data_source_obj_id
   ```
9. **File search:** an arbitrary `WHERE` over `tsk_files` (§9).
10. **Navigation:** `getContentById`, `getAbstractFileById`, `getUniquePath()` (walks parents), `getParent`, and finding a tree path by object ancestry (§5.2). Path-to-object lookup: `openFiles(dataSource, path)` uses `parent_path` + `name`.
11. **Has-data checks:** `LIMIT 1` probes on `data_source_info`, `tsk_objects` and `tsk_hosts` (§1.1).
12. **Time zones:** `SELECT time_zone FROM data_source_info`.

### 11.7 SQLite vs PostgreSQL differences seen in Autopsy code

| Concern | SQLite | PostgreSQL |
|---|---|---|
| String aggregation | `GROUP_CONCAT(x)` | `string_agg(x::character varying, ',')` (Accounts, Timeline `TimelineDBUtils`, Discovery `DomainSearchCacheLoader`) |
| Auto-increment for app tables | `INTEGER PRIMARY KEY` | `BIGSERIAL PRIMARY KEY` (`beta_tag_app_data`) |
| Feature probing | `SELECT name FROM sqlite_master WHERE type='table' AND name='ct_errors'` (Cyber Triage; SQLite only) | — |
| DB lifecycle | file `autopsy.db` | `CREATE`/`DROP DATABASE` via the `postgres` maintenance DB |

* `CaseDbAccessManager.select(q, cb)` prepends `SELECT `, so many Autopsy query strings omit it. `createTable(name, schema)` and `tableExists` are also used.
* SQLite concurrency: TSK holds a **file lock** and detects another app via `ConcurrentDbAccessException`.

### 11.8 Other per-case stores (outside the TSK DB)

* `<case>/Cache/thumbnails/*.png`, `<case>/Config/CasePreferences.properties`, `<case>/ModuleOutput/**`, `<case>/Reports/**`, `<case>/Log/autopsy.log*`, `<case>/Export/`.
* Image Gallery `drawable.db` (under ModuleOutput, with its own tables referencing `tsk_examiners`); Timeline/Discovery caches; the Solr text index (`ModuleOutput/keywordsearch/data/…`). These are owned by other inventories.
* Global `<userConfig>/*.properties` (ModuleSettings), `<userConfig>/tags/*-tag-set.json`, `<userConfig>/mualimit`.

---

## 12. Performance and usability problems in Autopsy, and ideas for postmortem

**Data loading and paging**

1. **"Paging" loads everything.** `BaseChildFactory.makeKeys()` materializes every row object (with attributes, for artifacts) and then partitions in memory. A 10,000-row page size does not reduce DB or heap load.
   * *Postmortem:* server-side keyset pagination (`WHERE (sort_key, obj_id) > ? ORDER BY … LIMIT ?`), virtualized egui tables, and lazy column fetch.
2. **N+1 queries everywhere.**
   * `art.getAttributes()` per artifact.
   * `getUniquePath()` per row (walks parents).
   * `getChildren().size()` per tree label.
   * `contentHasVisibleContentChildren` per file in a directory.
   * Async S/C/O tasks per row (aggregate score and Central Repository count per row).
   * *Postmortem:* batch-fetch attributes with `WHERE artifact_id IN (…)`, store `unique_path`/`parent_path` with the file, keep a materialized `child_count`/`visible_child_count` per object, batch score and occurrence lookups per page.
3. **Grouping by full scans.** Keyword, hash and interesting-set, email and account trees scan `blackboard_attributes` and group in Java on every refresh.
   * *Postmortem:* denormalize common grouping keys (set_name, keyword, list, account_type, email account/folder) into indexed columns of an artifact table, or into summary tables updated at insert time.
4. **Counting throttles.** View counts are disabled after 1 M files; counts are recomputed with `COUNT(*)` on each refresh.
   * *Postmortem:* maintain incremental counters per (data source, view bucket, type) updated in the ingest write path; cheap `SELECT` of the counters.

**Refresh and events**

5. **Event-driven refresh storms.** Every ingest `DATA_ADDED`/`CONTENT_CHANGED`/`FILE_DONE` hits every listener; throttled to 5 s, but each refresh re-runs the full query and rebuilds nodes, losing scroll and selection.
   * *Postmortem:* a single channel of typed change events (e.g. `crossbeam`/`tokio::broadcast`) with diffs (new row ids per bucket) applied incrementally to visible models.
6. **Synchronous local event delivery.** Listeners run on the publisher's thread, including DB threads; a slow listener stalls ingest. Remote events use Java serialization (ObjectMessage) with `trustAllPackages`, which is a security and compatibility hazard.
   * *Postmortem:* async dispatch, versioned schema'd payloads (JSON/CBOR/protobuf), IDs-only with lazy hydration (as Autopsy already does), and authenticated transport (e.g. PostgreSQL `LISTEN/NOTIFY` or NATS instead of ActiveMQ).

**Correctness bugs to avoid reproducing**

7. **Static filter state bug.** `KnownAndSlackFilterBase.filterKnown/filterSlack` are static and shared, so the Views and Data Sources hide settings clobber each other.
8. **SQL injection and fragility.** SQL is built by string concatenation throughout:
   * File search name filter: the escaping result is discarded.
   * MIME and data-source filters quote IDs as strings.
   * `pg_database` lookups.
   * *Postmortem:* always use bound parameters and a typed query builder.
9. **File-search date bugs.** The "to" date is midnight at the start of the day; an empty "to" means epoch 0 (so only "from" with an empty "to" returns nothing).
   * *Postmortem:* use an inclusive end-of-day, open-ended ranges, and a time picker.
10. **CSV export is not RFC 4180** (escapes `"` as `\"`). Its columns come from sampling the first 100 rows.
    * *Postmortem:* use the `csv` crate, export the full column set of the visible model, and offer an "all rows, not just selected" option.
11. **Case metadata quirks.**
    * The examiner change event is mislabelled as `NUMBER`.
    * The `.aut` filename is fixed to the original display name.
    * Recent cases shows 5 of 6 slots.
    * *Postmortem:* store case metadata in the case DB itself, keeping a small TOML/JSON manifest for discovery, with a stable case UUID.

**Lifecycle and latency**

12. **Data source removal requires closing and reopening the whole case** under an exclusive lock.
    * *Postmortem:* `ON DELETE CASCADE` foreign keys plus a background delete job with progress; other users see a "deleting" status.
13. **Case open does heavy work on the critical path:**
    * per-service modal dialogs;
    * a synchronous missing-image prompt per image;
    * a background "read 512 bytes of every file system" warm-up (capped at 100 images).
    * *Postmortem:* open the DB immediately, warm up lazily, show the missing-evidence state per data source in the tree (badge), and allow relinking from a context menu.
14. **Startup.** NetBeans module loading plus native DLL loading. Postmortem as a single static binary avoids this; keep the "another instance holds the case" check via SQLite `locking_mode`/advisory lock files and PostgreSQL advisory locks.

**Multi-user**

15. **ZooKeeper, ActiveMQ, Solr and PostgreSQL are four services**; health is checked every 15 min.
    * *Postmortem:* consider PostgreSQL alone for locks (`pg_advisory_lock`), events (`LISTEN/NOTIFY`), coordination metadata (a cases table) and full text (`tsvector` or tantivy per case); expose live health in the status bar.

**UX**

16. **Tree mixes concerns.** Two grouping modes are implemented as two node hierarchies, with expensive "View in Directory" path matching by node names.
    * *Postmortem:* one normalized navigation model keyed by object id; an explicit `object_id → tree path` resolver using the `par_obj_id` chain (skip VS/FS levels as Autopsy does); breadcrumbs plus back/forward that survive refresh.
17. **Long synchronous UI work on the EDT:**
    * child-count labels;
    * `isLeafDirectory` checks iterating children;
    * `getChildren()` in `DirectoryTreeFilterNode.getDisplayName`.
    * *Postmortem:* all DB access off the UI thread, with placeholder "(…)" counts.
18. **No unified filter.** Hide known and hide slack are global toggles applied inconsistently (SQL in Views, in-memory predicates in Data Sources).
    * *Postmortem:* a per-view filter bar (known, slack, deleted, MIME, date, size) shared by all tables and compiled to SQL.
19. **Tag UX.**
    * Tag sets are mutually exclusive but this is invisible in menus.
    * The "ForUser" counts scan all tags.
    * Notable-tag semantics are spread across modules.
    * *Postmortem:* a tag palette with set grouping and color chips, per-examiner filtering in SQL, explicit "notable" badge plus score integration, and keyboard shortcuts for the first N tags (Autopsy only has Ctrl+B).
20. **External viewer.** It always copies the whole file to temp, synchronously. MIME-then-extension rule lookup has no wildcards.
    * *Postmortem:* stream to temp in the background, support wildcard rules (`image/*`), and use `open`/`xdg-open`/ShellExecute.
21. **Dead features to restore:**
    * "Search for files with the same MD5 hash" (label exists, not wired).
    * "Recent Files" view (disabled for lack of filtering; restore it with data-source and date-range filters).
22. **Password storage.** The PBE with a hard-coded key is not a secret store.
    * *Postmortem:* use the OS keychain (`keyring` crate) for DB and messaging credentials.
23. **Extraction naming.** Multi-file extract prefixes `objId-`; recursive collisions append `objId-` as well. Timestamps and metadata are not preserved.
    * *Postmortem:* optionally preserve the directory structure, set mtime/atime, write a sidecar hash manifest (MD5/SHA-256) for chain of custody, and stream with progress per byte.
24. **Wizard friction.**
    * Data source type is chosen by a vertical list of toggle buttons.
    * Ingest modules must be configured before files appear.
    * Host selection comes first.
    * *Postmortem:* drag-and-drop evidence onto the window with auto-detection by `canProcess` confidence (Autopsy already has this for auto-ingest); default host from the data source; streaming ingest for all DSPs (Local Disk, Logical Files and XRY currently do not stream).

---

## 13. Source file index (quick reference)

* Case lifecycle:
  * `A/casemodule/Case.java`, `CaseMetadata.java`, `CaseDetails.java`
  * `CaseOpenAction.java`, `CaseCloseAction.java`, `CaseDeleteAction.java`, `CaseNewAction.java`, `NewCaseWizard*.java`, `NewCaseVisualPanel*.java`
  * `OptionalCasePropertiesPanel.java`, `CaseDetails*.java`, `CaseInformationPanel.java`
  * `RecentCases.java`, `RecentItems.java`, `UpdateRecentCases.java`
  * `StartupWindow*.java`, `CueBannerPanel.java`, `OpenRecentCasePanel.java`
  * `MissingImageDialog.java`, `DeleteDataSourceAction.java`, `CasePreferences.java`, `CollaborationMonitor.java`
  * `multiusercases/*`, `multiusercasesbrowser/*`, `OpenMultiUserCase*.java`
  * `SingleUserCaseConverter.java`, `UnpackagePortableCase*.java`, `ContentProviderUtils.java`, `AutopsyContentProvider.java`
  * `A/featureaccess/FeatureAccessUtils.java`, `A/machinesettings/UserMachinePreferences.java`
* Add data source:
  * `A/casemodule/AddImage*.java`, `ImageDSProcessor.java`, `ImageFilePanel.java`, `AddImageTask.java`
  * `LocalDisk*.java`, `LocalFiles*.java`, `LogicalFilesDspPanel.java`, `LogicalEvidenceFilePanel.java`, `AddLocalFilesTask.java`
  * `GeneralFilter.java`, `StreamingAdd*.java`, `DefaultIngestStream.java`
  * `A/datasourceprocessors/*`, `A/datasourceprocessors/xry/*`
  * `A/logicalimager/dsp/*`, `Experimental/src/org/sleuthkit/autopsy/experimental/volatilityDSP/*`
  * `A/casemodule/services/FileManager.java`
* Tree / data model:
  * `A/datamodel/AutopsyTreeChildFactory.java`, `RootContentChildren.java`, `ContentChildren.java`, `CreateSleuthkitNodeVisitor.java`
  * `*Node.java` (Image, Volume, Pool, Directory, File, LayoutFile, LocalFile, SlackFile, VirtualDirectory, LocalDirectory, LocalFilesDataSource, Host, Person, DataSourceGrouping, DataSources, Views)
  * `AbstractAbstractFileNode.java`, `AbstractContentNode.java`, `BlackboardArtifactNode.java`, `GetSCOTask.java`
  * `FileTypes*.java`, `FileTypeExtensions.java`, `DeletedContent.java`, `FileSize.java`, `RecentFiles*.java`
  * `Artifacts.java`, `DataArtifacts.java`, `AnalysisResults.java`, `KeywordHits.java`, `HashsetHits.java`, `InterestingHits.java`, `EmailExtracted.java`, `MalwareHits.java`, `accounts/Accounts.java`, `CreditCards.java`, `accounts/BINRange.java`
  * `OsAccounts.java`, `Tags.java`, `ContentTagNode.java`, `BlackboardArtifactTagNode.java`, `ScoreContent.java`, `Reports.java`, `CyberTriageData.java`
  * `BaseChildFactory.java`, `*KnownAndSlackFilter*.java`, `utils/IconsUtil.java`, `hosts/*`, `persons/*`
  * `A/guiutils/RefreshThrottler.java`
* Directory tree:
  * `A/directorytree/DirectoryTreeTopComponent.java`, `DirectoryTreeFilterNode.java`, `DirectoryTreeFilterChildren.java`, `DataResultFilterNode.java`
  * `ViewContextAction.java`, `ExtractAction.java`, `actionhelpers/ExtractActionHelper.java`, `ExtractUnallocAction.java`
  * `ExternalViewer*.java`, `ExportCSVAction.java`, `NewWindowViewAction.java`
  * `*DetailsPanel.java`, `ExplorerNodeActionVisitor.java`, `GroupDataSourcesDialog.java`
* Events: `A/events/*`, `A/casemodule/events/*`.
* Tags: `A/casemodule/services/Tag*.java`, `contentviewertags/*`, `A/actions/*Tag*.java`, `GetTagName*Dialog.java`, `A/tags/TagUtils.java`, `ImageGallery/.../ImageGalleryService.java` (Project VIC).
* Menus: `A/core/layer.xml`, `A/actions/*`, `A/menuactions/*`.
* File search: `A/filesearch/*`.
* Core:
  * `A/core/UserPreferences.java`, `ConfigProperties.java`, `ServicesMonitor.java`, `Installer.java`, `RuntimeProperties.java`, `AutopsyOptionProcessor.java`
  * `A/coreutils/*`
  * `A/corecomponents/ViewPreferencesPanel.java` (labels)
