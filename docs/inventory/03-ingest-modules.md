# 03 — Ingest framework & ingest modules (feature inventory)

Source of truth: Autopsy 4.x Java tree at `/home/tatu/slop/autopsy` (read-only). This document describes
*behaviour*, so the Java does not have to be re-read. Class names are given only so that a reader can find
the original if needed.

**Conventions**

* "Artifact" means a TSK blackboard artifact. *DataArtifact* = extracted fact (`TSK_GPS_TRACKPOINT`, ...),
  *AnalysisResult* = judgement with a `Score` (`TSK_HASHSET_HIT`, `TSK_INTERESTING_ITEM`, ...). Attribute
  names are the TSK `ATTRIBUTE_TYPE` enum names (`TSK_SET_NAME`, `TSK_COMMENT`, ...).
* Scores: `SCORE_NONE`, `SCORE_UNKNOWN`, `SCORE_LIKELY_NOTABLE`, `SCORE_NOTABLE` (+ `LIKELY_NONE`).
* "Rust status" for external dependencies: **crate** = a mature pure-Rust crate exists;
  **partial** = crates exist but they lack features we need; **implement** = we must write it ourselves;
  **external** = keep shelling out to a separate binary (an optional feature, never required for core flow).
* Units: sizes in bytes (KB = 1024 where Autopsy uses binary units; decimal where Autopsy uses
  `1000*1000`, which is called out). Times in seconds unless stated.

**Contents**

1. Ingest framework (API, discovery and ordering, settings, profiles, filters, scheduler, pipelines,
   threading, cancellation, progress, inbox, events, UI)
2. Core file and data-source modules (Hash Lookup, File Type ID, Extension Mismatch, Embedded File
   Extractor, Picture Analyzer, Encryption Detection, Interesting Files, Data Source Integrity)
3. External-tool and specialised modules (aLEAPP/iLEAPP, Plaso, PhotoRec, Scalpel, VM Extractor, DJI
   Drones, YARA, Object Detection, Volatility DSP)
4. Python/Jython modules: Android parsers, GPX, Jython loader, third-party plugin API
5. Module matrix (one table)
6. Performance problems in Autopsy and improvement ideas for postmortem

---

## 1. Ingest framework

### 1.1 Object model at a glance

```
IngestManager (singleton)
 ├─ thread pools: 1× data-source, N× file, 1× data-artifact, 1× analysis-result, 1× job-starter, 1× event publisher
 ├─ IngestTasksScheduler (singleton; shared queues for ALL jobs)
 └─ ingestJobsById: Map<jobId, IngestJob>
        IngestJob (one per data source; id from a process-global AtomicLong starting at 0)
         ├─ mode: BATCH | STREAMING
         ├─ settings: IngestJobSettings (enabled module templates + file filter + execution context)
         ├─ files: optional explicit subset (the "run ingest on the children of this file" case)
         └─ IngestJobExecutor
               ├─ tiers[0]: DS pipeline (stage 1) + N file pipelines + data-artifact pipeline + analysis-result pipeline
               └─ tiers[1]: DS pipeline (stage 2) only
```

An **ingest module** is created by an **IngestModuleFactory**. A factory can produce any combination of
four module kinds. Each kind has its own pipeline type:

| Kind | Interface method | Pipeline | Instances per job |
|---|---|---|---|
| Data-source | `process(Content dataSource, DataSourceIngestModuleProgress)` | `DataSourceIngestPipeline` | 1 per tier where it is placed |
| File | `process(AbstractFile)` | `FileIngestPipeline` | **one per file-ingest thread** (N copies of the pipeline) |
| Data artifact | `process(DataArtifact)` | `DataArtifactIngestPipeline` | 1 |
| Analysis result | `process(AnalysisResult)` | `AnalysisResultIngestPipeline` | 1 |

### 1.2 Module API contract (what a plugin implements)

**`IngestModule`** (base of all four kinds)

* `startUp(IngestJobContext ctx) throws IngestModuleException`: called once per module *instance*
  before any data. If it throws, the error is collected and the **whole job fails to start**. The other
  pipelines are shut down, a dialog lists `module: message`, and every other job in the same
  `beginIngestJob` batch is cancelled with `INGEST_MODULES_STARTUP_FAILED`.
* `process(...) -> ProcessResult {OK, ERROR}`: ERROR only causes logging. The pipeline continues with the
  next module and the next item. A `Throwable` thrown from `process` is caught by the pipeline and turned
  into an `IngestModuleError(moduleDisplayName, throwable)`, which is logged.
* `shutDown()`: called once per instance when its tier finishes, or after cancellation. Exceptions are
  caught, logged, and shown as a desktop notification.
* Threading guarantee: **an instance is only ever called from one thread.** Instances of the same module
  run concurrently on different threads, and several jobs can run at once. Shared per-job state uses the
  `IngestModuleReferenceCounter` idiom: `incrementAndGet(jobId) == 1` in startUp means "first instance,
  initialise the shared state for this job", and `decrementAndGet(jobId) == 0` in shutDown means "last
  instance, post the summary and free the state". Almost every core module uses this idiom to post one
  summary inbox message per job.

**`IngestJobContext`** (passed to startUp)

| Method | Meaning |
|---|---|
| `getDataSource()` | The data source `Content` |
| `getJobId()` | Ingest job id (process-local, not the DB id) |
| `getExecutionContext()` | Settings context string (for example `org.sleuthkit.autopsy.casemodule.AddImageWizardIngestConfigPanel` or `IngestProfiles.<name>`) |
| `dataSourceIngestIsCancelled()` | True if the job is cancelled **or** only the currently running DS module was cancelled |
| `fileIngestIsCancelled()` / `dataArtifactIngestIsCancelled()` | Job cancelled |
| `processingUnallocatedSpace()` | From the file filter: `!filter.ignoresUnallocatedSpace` (true when there is no filter) |
| `addFilesToJob(List<AbstractFile>)` (deprecated alias `scheduleFiles`) | Schedule newly derived or carved files into **this** job as *high-priority* file tasks (pushed to the front of the queue). Ignored if the job is cancelled, if the current tier has no file modules, or outside the `ANALYZING`/`ACCEPTING_STREAMED_CONTENT` states |

**`DataSourceIngestModuleProgress`**: `switchToDeterminate(totalUnits)`, `switchToIndeterminate()`,
`progress(units)`, `progress(String msg)`, `progress(String msg, units)`.

**`IngestModuleFactory`** (all methods except the first three have defaults)

| Method | Default | Notes |
|---|---|---|
| `getModuleDisplayName()` | (none) | **Identity key.** Used in settings files, enable/disable lists and duplicate detection. Must be globally unique |
| `getModuleDescription()`, `getModuleVersionNumber()` | (none) | The version is stored in the case DB (`ingest_modules`) and drives the "already run" warnings |
| `hasGlobalSettingsPanel()/getGlobalSettingsPanel()` | false | The panel has `saveSettings()`. Shown under Options and from the job settings dialog |
| `getDefaultIngestJobSettings()` | `NoIngestModuleIngestJobSettings` | Serializable POJO with `getVersionNumber()` |
| `hasIngestJobSettingsPanel()/getIngestJobSettingsPanel(settings)` | false | The panel returns `getSettings()` |
| `isDataSourceIngestModuleFactory()/createDataSourceIngestModule(settings)` | false | |
| `isFileIngestModuleFactory()/createFileIngestModule(settings)` | false | |
| `isDataArtifactIngestModuleFactory()/createDataArtifactIngestModule(settings)` | false | |
| `isAnalysisResultIngestModuleFactory()/createAnalysisResultIngestModule(settings)` | false | |

Adapters (`IngestModuleFactoryAdapter`, `FileIngestModuleAdapter`, `DataSourceIngestModuleAdapter`) only
provide no-op defaults. `GenericIngestModuleJobSettings` is a `Map<String,String>` settings bag that
Python modules can use.

**External processes from modules**: `DataSourceIngestModuleProcessTerminator(ctx[, maxSeconds | useGlobalTimeout])`
and `FileIngestModuleProcessTerminator`. They are polled by `ExecUtil.execute(ProcessBuilder, terminator)`
and kill the child when the job (or DS module) is cancelled or the timeout is exceeded.
`getTerminationCode() -> NONE|TIME_OUT|CANCELATION|CUSTOM`. The global timeout comes from the user
preferences `ProcessTimeOutEnabled` (default **false**) and `ProcessTimeOutHours` (default
**60 h**), editable in Options → Ingest → Settings.

**postmortem**: model this as Rust traits `DataSourceModule`, `FileModule`, `ArtifactModule`,
`ResultModule` plus a `ModuleFactory` trait. Settings are `serde` structs with an explicit
`schema_version` (do not use Java serialization). Keep the "one instance per worker" contract, but make
sharing explicit: the factory gets `fn start_job(&self, ctx) -> Arc<JobShared>`, which replaces the
static reference-counter maps.

### 1.3 Module discovery and ordering

1. **Discovery** (`IngestModuleFactoryLoader`): every `IngestModuleFactory` registered through the
   NetBeans Lookup (`@ServiceProvider`), plus legacy `IngestModuleFactoryAdapter` services, plus Jython
   factories (§4). `SampleIngestModuleFactory` is skipped. A **duplicate display name** is dropped with a
   warning. A Jython duplicate also shows an error dialog.
2. **List order shown in the UI** (and the default when nothing else applies): the core order is
   Recent Activity, Hash Lookup, File Type Identification, Extension Mismatch Detector, Embedded File
   Extractor, Picture Analyzer, Keyword Search, Email Parser, Encryption Detection, Interesting Files
   Identifier, Central Repository, PhotoRec Carver, Virtual Machine Extractor, Data Source Integrity.
   Other Java factories follow, sorted by display name, then Jython factories in load order.
3. **Pipeline order** (`IngestModuleTierBuilder` + `PipelineConfig.xml`, which is copied to the user
   config directory on first run and so can be edited by the user). The configured classes go first, in
   file order:
   * `ImageAnalysisStageOne`: `RecentActivityExtracterModuleFactory`, `module.AndroidModuleFactory` (Jython)
   * `FileAnalysis`: HashLookup, FileTypeId, EmbeddedFileExtractor, ExifParser (legacy name, no longer
     present), KeywordSearch, EmailParser, FileExtMismatch, InterestingItems, PhotoRecCarver,
     `GPX_Parser_Module.GPXParserFileIngestModuleFactory`
   * `ImageAnalysisStageTwo`: DataSourceIntegrity

   The remaining enabled modules of each kind are appended: modules whose factory class starts with
   `org.sleuthkit.autopsy` first, then third-party modules. Each group keeps its insertion order (the order
   from the settings template). Jython factory class names (`org.python.proxies.<module>$<Class>$N`) are
   normalised to `<module>.<Class>` for matching. Data-artifact and analysis-result pipelines are not
   configurable: the order is Autopsy modules, then third-party.
   **Rule:** Hash Lookup must run before everything that checks `file.getKnown()`, and File Type ID must
   run before every MIME consumer. Those consumers also call `FileTypeDetector.getMIMEType` lazily, so a
   wrong order still works, only slower.
4. **Tiers**: tier 0 = {stage-1 DS pipeline + all unlisted DS modules, file pipelines × N, artifact
   pipeline, result pipeline}. Tier 1 = {stage-2 DS pipeline}. **Tier 1 starts only after every task of
   tier 0 (including files added by modules and artifacts posted by modules) has finished.** Tier 1 has no
   artifact or result pipeline, so artifacts posted by stage-2 DS modules are never fed to artifact
   modules. That is a gap in Autopsy; postmortem should allow N stages for every module kind.

### 1.4 Ingest job settings (`IngestJobSettings`)

* **Execution context**: a string that namespaces the stored settings. Known contexts:
  `org.sleuthkit.autopsy.casemodule.AddImageWizardIngestConfigPanel` (Add Data Source wizard),
  `org.sleuthkit.autopsy.ingest.RunIngestModulesDialog` (Tools → Run Ingest Modules or the context menu),
  `IngestProfiles.<ProfileName>` (profiles), the command-line ingest context, and the auto-ingest
  context. When `IngestType != ALL_MODULES` the context gets the suffix `.DATA_SOURCE_ONLY` or
  `.FILES_ONLY`.
* **IngestType**: `ALL_MODULES`, `DATA_SOURCE_ONLY` (only DS-capable factories, plus data-artifact
  factories), `FILES_ONLY` (only file-capable factories, plus data-artifact factories). "Run ingest
  modules" on a directory or file uses `FILES_ONLY` and passes the file's **direct children** as the
  explicit file list. The action is only enabled when the file has children.
* **Storage layout** (in `<userConfigDir>/<moduleConfigDir>/IngestSettings/`):
  * `<context>.properties`: keys `Enabled_Ingest_Modules` and `Disabled_Ingest_Modules` (lists of display
    names joined with `", "`) and `Last_File_Ingest_Filter` (filter name). Profiles add `Profile_Name`,
    `Profile_Description` and `Profile_Filter`.
  * `<context>/<normalizedFactoryClass>.settings`: the module's serialized per-job settings. If loading
    fails, the module falls back to `factory.getDefaultIngestJobSettings()` and adds a warning.
* **Default-disabled modules** (on first sight): `Plaso`, `Keyword Search`, and the Malware Scan module.
  Every other newly discovered module defaults to **enabled**.
* **Legacy rename map** (applied when the name lists are read): Thunderbird Parser or MBox Parser →
  Email Parser; File Extension Mismatch Detection → Extension Mismatch Detector; EWF Verify, E01 Verify or
  E01 Verifier → Data Source Integrity; Archive Extractor → Embedded File Extractor; Correlation Engine →
  Central Repository; Exif Parser → Picture Analyzer; Drone Analyzer → DJI Drone Analyzer.
* A module that is enabled in the settings but no longer installed produces a warning ("missing module").
  `getWarnings()` returns the list and clears it, and the UI shows it in a dialog before the job starts.
* **File filter**: selected by name from the standard filters plus the custom filters. If the name is
  unknown or the lookup fails, the default filter is used. `getProcessUnallocatedSpace()` is
  `!filter.ignoreUnallocatedSpace`.

### 1.5 Ingest profiles

* A profile = name + description + file filter name + a complete `IngestJobSettings` under the context
  `IngestProfiles.<name>`. Profiles are listed by scanning `IngestSettings/` for files that start with
  `IngestProfiles.`.
* A profile name may contain only letters, digits and `_`. The new-profile placeholder is
  `NewEmptyProfile`. Rename moves both the `.properties` file and the settings folder. Delete removes both.
* A file filter that is in use by a profile cannot be deleted. The error names the profile.
* The Add Data Source wizard and the Run Ingest Modules wizard first show a profile selection page
  ("Custom settings" or a profile). Choosing a profile skips the module configuration page.
* UI: Options → Ingest → tabs **File Filter** (the FilesSet editor), **Settings**, **Profiles**.

### 1.6 File ingest filters (shared rule engine with Interesting Files)

A **file filter** is a `FilesSet` (see §2.7 for the full rule syntax). Standard filters:

| Name | ignoreKnown | ignoreUnalloc | Rule |
|---|---|---|---|
| `All Files, Directories, and Unallocated Space` (**default**) | false | false | MetaType ALL |
| `All Files and Directories (Not Unallocated Space)` | false | true | MetaType ALL |

Custom filters are stored in `<moduleConfigDir>/FileIngestFilterDefs.settings` (serialized map).
Semantics when used as an ingest filter (`IngestTasksScheduler.shouldEnqueueFileTask`):

1. Files named `.` or `..` are dropped.
2. **Directories are never filtered**: they are always enqueued, so their children get visited and file
   modules also receive directories. Non-directories pass if `shouldBeCarved` (job processes unallocated
   space **and** file type is `UNALLOC_BLOCKS`) **or** `filter.fileIsMemberOf(file) != null`.
3. FAT and NTFS only: files whose parent is the root directory, `meta_addr < 32`, and a name that starts
   with `$` and contains `:` are dropped (NTFS system-file alternate data streams such as `$Secure:$SDS`).
4. When `ignoreUnallocatedSpace` is set, `UNALLOC_BLOCKS`, `SLACK` and `UNUSED_BLOCKS` files are rejected
   by `fileIsMemberOf`. When `ignoreKnown` is set, files with `known == KNOWN` are rejected. Note that the
   known status is only set later by Hash Lookup, so in practice this only affects re-ingest.

### 1.7 IngestManager: threads, starting jobs, multi-user

* **Threads** (created once, live for the whole process):
  * `IM-data-source-ingest-0`: **1 thread** for all DS tasks of all jobs (DS modules of different jobs run
    one after another).
  * `IM-file-ingest-%d`: **N threads**, where N = user preference `NumberOfFileIngestThreads` (default
    **2**). The UI offers {1,2,4,6,8} depending on the CPU count (≥8 cores: max 8, recommended 4; ≥6: max 6,
    recommended 4; ≥4: max 4, recommended 2; ≥2: max 2, recommended 1). Changing it requires a **restart**.
  * `IM-data-artifact-ingest`: 1 thread. `IM-analysis-result-ingest`: 1 thread.
  * `IM-start-ingest-jobs`: 1 thread. Jobs queued with `queueIngestJob` start here asynchronously, and
    module startUp runs on this thread with a cancellable "Starting ingest" progress handle.
  * `IM-ingest-events`: 1 thread for publishing events.
  * Each worker loops `task = queue.takeFirst(); task.execute(threadId)`.
* **APIs**: `queueIngestJob(dataSources, settings)` (async, one job per data source),
  `queueIngestJob(ds, files, settings)` (explicit file subset), `beginIngestJob(dataSources, settings) ->
  IngestJobStartResult{job, startupException, moduleErrors}` (synchronous), and `openIngestStream(ds,
  settings) -> IngestStream` (streaming mode, used by the image DSP while TSK is still adding files:
  `addFiles(objIds)`, `close()` (data source fully added, so DS modules can start), `stop()` (cancel)).
  If the settings have no enabled modules, no job is created.
* **Start checks**: a case must be open. For multi-user cases the remote case DB service must be UP. The
  `IngestMonitor` is started. The job is registered, its module pipelines are started, and the
  `ingest_job` row is recorded. Then `STARTED` and `DATA_SOURCE_ANALYSIS_STARTED` are fired.
* **Service monitoring** (multi-user only): when `REMOTE_CASE_DATABASE` or `REMOTE_KEYWORD_SEARCH` goes
  DOWN, all jobs are cancelled with reason `SERVICES_DOWN` and an error dialog is shown.
  **Case closed** → all jobs are cancelled with `CASE_CLOSED`, and the inbox is cleared.
* `IngestManager` subscribes to the TSK `Blackboard.ArtifactsPostedEvent`. Each posted artifact is routed
  to the job with the posted `ingestJobId`, or, when there is none, to the first running job whose data
  source matches the artifact's data source. It becomes a data-artifact or analysis-result task in that
  job. A legacy `DATA_ADDED` (`ModuleDataEvent`) event is also published for each artifact type.

### 1.8 Job lifecycle (`IngestJobExecutor`)

States: `PIPELINES_STARTING_UP → (STREAMING: ACCEPTING_STREAMED_CONTENT_AND_ANALYZING →) ANALYZING →
PIPELINES_SHUTTING_DOWN → (next tier: ANALYZING ...)`.

1. **startUp**: build the tiers and start every pipeline: for each tier, the DS pipeline, then the file
   pipelines (stop at the first file pipeline that fails), then the artifact and result pipelines. A
   pipeline whose module fails startUp is shut down immediately.
2. **Record in the case DB**: `addIngestModule(displayName, normalizedFactoryClass, type, version)` for every
   enabled template. The type is one of `DATA_SOURCE_LEVEL | FILE_LEVEL | DATA_ARTIFACT | MULTIPLE`; the
   analysis-result kind is not represented. The DB row's unique name is
   `<factoryClass>-<displayName>-<version>`. Then `addIngestJob(ds, hostname, modules, start, end=0,
   STARTED, settingsDir="")`.
3. **Batch mode**: schedule one DS task (if the tier has DS modules), the file tasks (the explicit list
   or the top-level files of the data source), and artifact and result tasks for **all existing** data
   artifacts and analysis results of the data source (when there are artifact or result modules).
   Then check whether the tier is already done.
4. **Streaming mode**: start the file progress bar as indeterminate and schedule existing artifacts and
   results. Files arrive through `addStreamedFiles`. On `close()`
   (`addStreamedDataSource`) the state changes to ANALYZING, the file estimate is computed, and the DS task
   is scheduled.
5. **Tier completion** (`checkForTierCompleted`, called in a `finally` after every task): if the
   scheduler has no queued **or running** tasks for this job in any queue, shut down the current tier's
   pipelines (`shutDown` on every module instance; the file pipeline first flushes its batched file
   saves), advance to the next tier and schedule it. Repeat while the next tier is also empty. After the
   last tier: set the `ingest_job` status to `COMPLETED` or `CANCELLED` with an end time, then fire
   `DATA_SOURCE_ANALYSIS_COMPLETED` (reason ANALYSIS_COMPLETED or ANALYSIS_CANCELLED) and
   `COMPLETED` or `CANCELLED`, and remove the job.
6. **File estimate** (for the determinate progress bar only): with an explicit file list, its size;
   otherwise a count over every file system of the data source:
   `fs_obj_id = X AND (meta_type IN (REG, DIR, VIRT_DIR) OR (meta_type = VIRT AND name NOT IN ('.','..')))`.
   Layout files count as 1, and other content counts its leaf children recursively. `processedFiles`
   can exceed the estimate (derived files, for example). The progress bar is clamped.

### 1.9 Task scheduler (`IngestTasksScheduler`, one instance shared by all jobs)

Queues:

| Queue | Type | Purpose |
|---|---|---|
| `dataSourceIngestTasksQueue` | tracking blocking deque | DS tasks |
| `topLevelFileIngestTasksQueue` | `TreeSet` ordered by root-directory priority | children of the root directories of each data source |
| `batchedFileIngestTasksQueue` | deque | directories (and any file **with children**) waiting to be expanded |
| `streamedFileIngestTasksQueue` | queue | object ids pushed by streaming add-image |
| `fileIngestTasksQueue` | tracking blocking deque | **what file threads actually take from** |
| `artifactIngestTasksQueue`, `resultIngestTasksQueue` | tracking blocking deque | |

"Tracking" queues keep `queuedTasks` and `tasksInProgress` lists so that `hasTasksForJob` also counts
running tasks.

* **Top-level files**: for each root object from `GetRootDirectoryVisitor` (file systems → root dir;
  `VirtualDirectory`, `LocalDirectory`, `LayoutFile` and `Directory` are themselves roots), take its children.
  A root with no children is itself a task. A data source that is a single file is itself the task.
* **Root priority** (applies to the *name* of a top-level entry; case-insensitive prefix regex):
  HIGH = `^Users`, `^Documents and Settings`, `^home`, `^ProgramData`; MEDIUM = `^Program Files`
  and anything not matched; LOW = `^$OrphanFiles`, `^Windows`; LAST = `^pagefile`, `^hiberfil`, **and
  every non-FS-type file** (unallocated blocks, carved, derived, local). The order is priority descending,
  then object id descending.
* **Refill algorithm** (runs under the scheduler lock after every file task completes and after
  scheduling). Only when `fileIngestTasksQueue` is empty:
  1. Streaming first: move up to N (thread count) streamed tasks that pass the filter.
  2. Batch: if `batched` is empty, move the next top-level task into it. Pop the head of `batched`,
     enqueue it if it passes the filter, then for each child: a child **with children** goes to the
     **back** of `batched` (breadth-first inside one top-level subtree), and any other child that passes
     the filter goes straight into `fileIngestTasksQueue`. Loop until the file queue is non-empty or
     nothing is left.
  The effect: work arrives in **one directory's worth of files at a time**, and the expensive
  `getChildren()` DB queries happen lazily under a global lock.
* **High-priority** tasks (`addFilesToJob`) are `putFirst` into `fileIngestTasksQueue` after the filter
  check.
* **Cancellation** removes the job's tasks from the top-level, batched and streamed queues only. Tasks
  already in the file, DS and artifact queues are still dequeued, but `execute` skips them because the
  executor is cancelled.

### 1.10 Pipelines

`IngestPipeline.performTask(task)`:

```
if !running -> error "Pipeline not started or shut down"
if !cancelled:
   pauseIfScheduled()                 # see below
   prepareForTask(task)
   for module in modules:
       pauseIfScheduled(); if cancelled break
       currentModule = module; module.startTime = now
       try module.process(task) catch Throwable -> errors += (module, ex)
       if cancelled break
cleanUpAfterTask(task)
```

* **Scheduled pause** (`ScheduledIngestPauseSettings`; preferences `IngestPauseEnabled` (false),
  `IngestPauseDayOfWeek` (SUNDAY), `IngestPauseTimeHour`/`Minutes` (0:00), `IngestPauseDurationMinutes`
  (60)): if the current time is inside the weekly window, the worker thread sleeps until the window ends.
  It registers itself so that cancellation can interrupt the sleep.
* **File pipeline cleanup**: sets the thread activity to "Saving Results", `file.close()`, and adds the
  `AbstractFile` to a per-pipeline batch. When the batch reaches **500** files, it opens a
  transaction, calls `file.save(tx)` on each file (this persists the `known`, MIME type, MD5 and SHA-256
  values that modules set in memory), commits, and fires `FILE_DONE` for each file. The batch is flushed
  in `shutDown`. **Modules mutate the `AbstractFile` object, and the pipeline persists it.** On
  cancellation the batch is **not** saved.
* **DS pipeline**: sets the progress bar title to `"<module> (<data source>)"` (indeterminate) before
  each module and resets it afterwards. If only this module was cancelled, it records the name in
  `cancelledDataSourceIngestModules`, clears the flag, and restarts the progress bar.
* Per-module wall time is accumulated in `IngestManager.ingestModuleRunTimes[displayName]`: the time
  between activity-snapshot changes on a thread, so it includes "Saving Results" as a pseudo-module.

### 1.11 Cancellation

* Job-level reasons: `NOT_CANCELLED`, `USER_CANCELLED`, `INGEST_MODULES_STARTUP_FAILED`,
  `OUT_OF_DISK_SPACE`, `SERVICES_DOWN`, `CASE_CLOSED`.
* `IngestJob.cancel(reason)` returns immediately; the work runs on a new thread. The progress bars show
  "Cancelling...", pending file tasks are purged, paused threads are interrupted, and the tier completion
  check is run.
* **DS progress-bar cancel button** opens a dialog: "cancel the current data-source module only" or
  "cancel the whole job". File, artifact and result progress-bar cancel buttons cancel the whole job.
* Modules must poll `ctx.*IsCancelled()` in long loops. Core modules check every 10 000 bytes (entropy),
  per archive item, and so on. External processes are killed by the process terminators.

### 1.12 Progress and diagnostics

* Up to 4 NetBeans progress handles per job: "Data source ingest of <ds>" (DS modules update it),
  "File ingest of <ds>" (determinate on the estimate, showing the first in-progress file name),
  "Analyzing data artifacts from <ds>" (indeterminate), and "Analyzing analysis results from <ds>"
  (indeterminate).
* **Ingest Progress Snapshot dialog** (Help → Ingest progress snapshot), with a Refresh button:
  * Thread table: Thread ID, Activity (module name or `IDLE`), Data Source, File, Start Time,
    Elapsed (H:M:S), Job ID.
  * Job table: Job ID, Data Source, Start, Tier ("Tier N [shut down]"), Num Processed, **Files/Sec**,
    In Progress, Files Queued, Dir Queued, Root Queued, Streaming Queued, DS Queued, Artifacts Queued,
    Results Queued.
  * Module table: module name → accumulated run time (and its share of the total).
* `IngestJob.getSnapshot()` (public API, used by auto-ingest and others) exposes the running DS module
  handle (name, start time, cancel()), whether file ingest is running and its start time, the cancelled
  flag and reason, and the list of cancelled DS modules.

### 1.13 Ingest inbox (`IngestMessage`, `IngestMessageTopComponent`)

* `IngestMessage{id, type ∈ DATA|INFO|WARNING|ERROR, source (module name, or null for the manager),
  subject, detailsHtml, uniqueKey, data: BlackboardArtifact?, datePosted}`. Factories:
  `createMessage(type, src, subj[, html])`, `createErrorMessage`, `createWarningMessage`, and
  `createDataMessage(src, subj, html, uniqueKey, artifact)`, which requires every argument to be non-null.
* Only shown when the GUI is running. **ERROR and WARNING messages are capped at 200 per case**. Message
  201 is replaced by a single "error message limit reached" message.
* **Grouping** (DATA messages only, keyed by `(source, uniqueKey)`): the first 4 messages with the same
  key stay as separate rows. When more than 3 groups exist and one more arrives, they are merged into one
  row that moves to the bottom. Later messages join that merged row. The row colour depends on the group
  size: 1 → `#A4A4CA`, 2–4 → `#B4B4D3`, 5–14 → `#C7C7DE`, ≥15 → `#DDDDEB`. ERROR rows use
  `#FF5A5A`. Unread rows are bold.
* Sort by time (default) or by priority (re-sorted on every insert). The counters "total" and "unique"
  appear in the toolbar. Clicking a row shows the merged details HTML, separated by `<hr/>`, with "go to
  artifact" and "go to directory" buttons when `data` is set. The inbox is cleared on case open and close.

### 1.14 IngestMonitor and IngestServices

* `IngestMonitor`: a Swing timer that fires every **60 s** while ingest runs. It writes memory usage to
  `var/log/monitor.log` (3 rotating files). It is supposed to cancel all jobs (`OUT_OF_DISK_SPACE`) when
  the case volume has less than **100 MiB** free, **but `getFreeSpace()` is hard-coded to return UNKNOWN
  (-1)**, so the check is disabled. postmortem should implement it properly (statvfs, with UNC and
  network-drive caveats).
* `IngestServices` (the module-facing service locator): `getCase()`, `getCaseDatabase()`,
  `getLogger(moduleName)`, `postMessage(IngestMessage)`, `fireModuleContentEvent(ModuleContentEvent(content))`
  (signals that new child content exists, which makes the tree refresh), deprecated
  `fireModuleDataEvent` (now `blackboard.postArtifacts`), `getFreeDiskSpace()`, and
  `get/setConfigSetting(moduleName, key[, value])` and `get/setConfigSettings(moduleName, Map)`
  (`<module>.properties` key-value storage).

### 1.15 Events

| Channel | Event name | Payload |
|---|---|---|
| job | `STARTED`, `COMPLETED`, `CANCELLED` | oldValue = job id |
| job | `DATA_SOURCE_ANALYSIS_STARTED` | `DataSourceAnalysisStartedEvent(jobId, dataSource)` |
| job | `DATA_SOURCE_ANALYSIS_COMPLETED` | `DataSourceAnalysisCompletedEvent(jobId, ds, reason ANALYSIS_COMPLETED|ANALYSIS_CANCELLED)` |
| module | `DATA_ADDED` | `BlackboardPostEvent(ModuleDataEvent{moduleName, artifactType, artifacts})`; the remote form serializes artifact ids and resolves them lazily |
| module | `CONTENT_CHANGED` | `ContentChangedEvent(ModuleContentEvent{moduleName, content})` |
| module | `FILE_DONE` | `FileAnalyzedEvent(file)`, fired after the batch commit |

In multi-user cases both channels are also published on the remote message bus (topic
`<caseName>-Ingest-Job-Events` / `-Ingest-Module-Events`). Listeners subscribe to all events or to a set.

### 1.16 UI surfaces to reproduce

* **Ingest job settings panel**: a module table (checkbox, name, and a history icon: ⚠ "this module has
  been run before on this data source" when a **completed** past job has the same unique name and version;
  ℹ "a previous version has been run before" when only the name matches). It also has the module
  description, the per-run settings panel ("The selected module has no per-run settings." otherwise),
  a Global Settings button, Select All and Deselect All, a **History** button (the past ingest jobs table
  from the case DB), and a "Run ingest modules on:" filter combo with standard filters, custom filters and
  "Create/edit file ingest filters...".
* **Run Ingest Modules**: Tools menu → a sub-menu listing each data source. Context menu on a data
  source. Context menu on a directory or file with children (FILES_ONLY, direct children).
* **Options → Ingest → Settings**: number of file threads, process timeout (off by default, 60 h),
  weekly pause. While ingest runs, the settings are partly disabled ("Ingest is ongoing, some settings will
  be unavailable until it finishes").
* The inbox toolbar button with the unread count, and the progress snapshot dialog.

### 1.17 Postmortem design notes for the framework

* Keep the externally visible semantics: module kinds, per-thread instances, the order rules, the tiers
  (generalise to N stages), filters, high-priority re-injection of derived files, the batched
  `known`/MIME/hash persistence, the event types, the job DB records and the "already run" detection.
* Replace the global-lock scheduler with a work-stealing executor (see §6). Directory expansion should
  read children through one indexed query per directory, or better, stream the file table ordered by
  `(data_source, priority, parent_path)`.
* Persist settings as TOML or JSON (`serde`) keyed by module id (a stable string, not the display name)
  with a `schema_version`, and provide migration functions in place of `readObject` upgrades.


---

## 2. Core modules (Java, `Core/src/org/sleuthkit/autopsy/modules/*`)

Common skip rule used by most file modules ("standard skip"): skip when `type ∈ {UNALLOC_BLOCKS, SLACK}`
(sometimes also `UNUSED_BLOCKS` and `VIRTUAL_DIR`), when `!isFile()`, and often when `known == KNOWN`.
Every artifact is created only when `blackboard.artifactExists(file, type, attrs)` is false, which makes
re-ingest idempotent. It is then posted with `blackboard.postArtifact(art, moduleName, jobId)`. That post
indexes the artifact for keyword search, fires DATA_ADDED, and routes the artifact into the job's
artifact or result pipeline.

### 2.1 Hash Lookup (`hashdatabase`, file module, "Hash Lookup")

**Settings**
* Per job (`HashLookupModuleSettings`): `shouldCalculateHashes` (default **true**, UI text "Calculate MD5
  even if no hash set is selected") and a list of `HashDbInfo` entries, each with a
  `searchDuringIngest` flag. A set missing from the list uses the set's own global flag.
* Global (`HashLookupSettings`, file `<moduleConfigDir>/HashLookup/hashLookup.settings`; legacy
  `hashsets.xml` with elements `hash_set{name,type,use_for_ingest,show_inbox_messages}` and
  `hash_set_path`, where the placeholder `[UserConfigFolder]` is substituted). Each set has: name,
  `KnownFilesType`, `searchDuringIngest`, `sendIngestMessages`, path (file sets) **or**
  `referenceSetID` + version + readOnly (Central Repository sets), and an `isCentralRepoDatabaseType` flag.
* `KnownFilesType`: `KNOWN` ("Known", file status KNOWN, score NONE; inbox messages off by default),
  `KNOWN_BAD` ("Notable", status BAD, score **NOTABLE**; inbox messages on by default), `NO_CHANGE`
  ("NoChange", status unchanged/UNKNOWN, score UNKNOWN; inbox messages on by default).
* **Official hash sets**: every `*.kdb` file in the installed `OfficialHashSets/` folder, named
  `<name>.<Known|Notable|NoChange>.kdb`. They are read-only, searched during ingest, and send no inbox
  messages. A user set with the same name is renamed to `<name> (Custom)` (repeatedly if needed) and the
  settings are rewritten.

**Algorithm (`process(file)`)**
1. Skip `UNALLOC_BLOCKS`, `SLACK` and directories. Also skip when there are no known sets, no notable
   sets, and `shouldCalculateHashes` is false. (NoChange sets are ignored in this test; that is a quirk.)
2. **Hash**: if MD5 **and** SHA-256 are both already set, reuse them. Otherwise compute **MD5 and SHA-256
   in one pass** over the content (`HashUtility.calculateHashes`), set them on the file object (persisted
   by the pipeline batch save), and report timing to the health monitor. A read error produces an ERROR
   inbox message ("Error encountered while calculating the hash value for <path> (Allocated File|Deleted
   File)").
3. **Notable sets** (all enabled `KNOWN_BAD` sets): `lookupMD5(file)` → `HashHitInfo` (hash, names,
   comments). On a hit: `file.setKnown(BAD)`, increment the counter, and build a comment by
   concatenating the hit comments with spaces, truncated to **500 characters + "..."**. Create the
   artifact if it does not exist yet.
4. **No-change sets**: the same as step 3, but the status set is `UNKNOWN`.
5. **Known sets**, only when no notable hit: `lookupMD5Quick` (boolean). The first hit sets
   `known = KNOWN` and stops. **No artifact.**
6. Lookup errors produce an ERROR inbox message and `ProcessResult.ERROR`, and processing continues.

**Artifact**: `TSK_HASHSET_HIT` (AnalysisResult) on the file, with score by set type (above),
`configuration = set display name`, and attributes `TSK_SET_NAME` (set display name), `TSK_HASH_MD5`
and `TSK_COMMENT` (the merged comments). The existence check keys on `TSK_SET_NAME` only. If the set has
`sendIngestMessages`: a DATA inbox message "Notable: <file>" with an HTML table (File Name, MD5 Hash,
Hash Set Name), `uniqueKey = fileName + md5`.

**Summary** (last instance per job, posted only if any set is configured): an INFO message "Hash Lookup
Results" with a table (notable files found, no-change items found, total calculation time in ms, total
lookup time in ms) and a list of the notable and no-change sets used. On the first instance per job, a
WARNING message is posted for each missing kind of set ("No notable hash set." / "No known hash set.").

**Hash set formats and engine** (in Autopsy they are implemented in TSK C code through `SleuthkitJNI`;
postmortem must **implement** them all):

| Format | Detection / extension | Layout |
|---|---|---|
| **TSK SQLite DB** (`.kdb`, the native and updateable format) | SQLite | tables `hashes(id INTEGER PK, md5 BLOB(16) UNIQUE, sha1 BLOB, sha2_256 BLOB)`, `file_names(name, hash_id)`, `comments(comment, hash_id)`, `properties(name, value)` (schema version, db name). Lookup by md5 blob. `addToHashDatabase(md5, sha1, sha256, name, comment)` |
| NSRL RDS v2 (`NSRLFile.txt`) | CSV header `"SHA-1","MD5","CRC32","FileName","FileSize","ProductCode","OpSystemCode","SpecialCode"` | text, needs an index |
| md5sum | `<32 hex><space(s)><filename>` | text, needs an index |
| HashKeeper (`.hsh`) | CSV with a `hash` column (32 hex) | text, needs an index |
| EnCase (`.hash`) | binary: magic `48 41 53 48 0D 0A FF 00 02 00 00 00 01 00 00 00` (16 B), u32 LE count, 0x3F4 bytes of filler, 0x50 bytes of name, 0x28 bytes of type, then records of 16-byte MD5 + 2-byte divider | needs an index |
| TSK index (`<db>-md5.idx`, can be used "index only") | sorted fixed-width text lines `<md5 hex>|<offset into source db>` (binary search by line) | |

Operations: open, create (`.kdb` only; the name must end in `.kdb`), import (existing file; the dialog
can copy it into `<moduleConfigDir>/HashLookup/HashDatabases/`), "index" (build the `.idx` for text
formats with a progress bar), add hashes (right-click a file → "Add file to hash set"; this requires that
the MD5 was already computed; also a paste-a-list dialog that validates `^[0-9a-f]{32}$`), delete, and
"search case by MD5" (lists files whose `md5` matches).

**Central Repository hash sets**: reference sets in the CR DB (`reference_sets`, `reference_file`
tables, organisation, version, read-only flag, known status). Lookup uses
`isFileHashInReferenceSet(md5, setId)` and `lookupHash(md5, setId)`. Import into the CR accepts
`.idx`/`.txt` (md5sum/idx parser: first token before `|` or space, must match `^[0-9a-f]{32}$`), `.hash`
(EnCase), `.kdb` (reads `hashes.md5` and the `group_concat` of comments) and `.hsh` (HashKeeper), in
batches of 10 000.

**postmortem**:
* Load each enabled set into an in-memory **sorted `Vec<[u8;16]>`**, or an mmapped sorted file, with an
  optional Bloom or xor filter in front: about 50 M NSRL MD5s ≈ 800 MB raw, or about 60 MB for a
  binary-fuse filter. Do the lookup by binary search or Eytzinger layout.
* Keep the `.kdb` schema for read/write compatibility (`rusqlite`).
* Support NSRL **RDSv3** (SQLite `FILE` table) as well. NSRL v2 is no longer published.
* Hashing: `md-5` + `sha2` (with the `asm` feature) in one read pass. Optionally add SHA-1 and BLAKE3.

### 2.2 File Type Identification (`filetypeid`, file module, "File Type Identification")

**Detector order** (`FileTypeDetector.getMIMEType(file)`; the result is cached on the file and persisted):
1. If `file.getMIMEType()` is already set, return it without the `;params` suffix.
2. Non-files, size ≤ 0, `UNALLOC_BLOCKS`, `UNUSED_BLOCKS`, `VIRTUAL_DIR`, and `SLACK` files under
   4096 bytes get `application/octet-stream`. The code still reads the buffer, but this result wins.
3. Read the first **600 bytes**. Try the **user-defined types** first (in list order), then the
   **Autopsy-defined types**.
4. **Apache Tika** `detect(stream)` (magic only). If the result is not `octet-stream`, run a second
   pass `detect(stream, fileName)` that lets name globs refine it. Then strip `tika-` from the type
   (`application/x-tika-ooxml` → `application/x-ooxml`, `x-tika-msoffice` → `x-msoffice`) and strip the
   parameters.
5. If the result is octet-stream and the extension is `txt`, run charset detection; if a charset is
   found, return `text/plain`.
6. `audio/mpeg` whose first 2 bytes are `FF FF` is changed to `octet-stream` (a false-positive fix).
7. Any exception returns `octet-stream`.

**Custom signature model**: `FileType{mimeType, signatures[], createInterestingFileHit, interestingFilesSetName}`.
**All** signatures must match. `Signature{bytes, offset, type RAW|ASCII, relativeToStart}`.
Matching: fail if `offset ≥ size`. The actual offset is `offset` from the start, or `size - 1 - offset`
when relative to the end (so the *offset counts back to the first signature byte*). Fail if the
signature runs past the end. Compare from the cached buffer when it fits, otherwise read from the file.

**Autopsy-defined types** (checked after the user types; offsets from the start unless noted):
`text/xml` "<?xml"@0 · `application/x-gzip` 1F8B@0 · `application/x-123` 00000200060406000800000000 00@0 ·
`image/vnd.radiance` "#?RADIANCE\n"@0 · `image/x-dcx` B168DE3A@0 · `image/x-icns` "icns"@0 ·
`image/x-pict` 001102FF@522 · `image/x-portable-arbitrarymap` "P7"@0 · `image/x-portable-floatmap`
"PF"@0 or "Pf"@0 · `image/x-tga` "TRUEVISION-XFILE.\0"@17 from the **end** · `image/x-ilbm`
"FORM"@0 + "ILBM"@8 or "PBM"@8 · `image/webp` "RIFF"@0 + "WEBP"@8 · `audio/aiff` "FORM"@0 +
"AIFF"/"AIFC"/"8SVX"@8 · `application/x-iff` "FORM"@0 · `image/jpeg` FFD9FFD8@0 (a JPEG that starts with
an EOI) · `application/x.windows-registry` "regf"@0 · `application/x.android-hdb` 24×CC@8 ·
`application/x-vhd` "conectix"@511 from the end.

**User-defined types**: in `<userConfigDir>/UserFileTypeDefinitions.settings` (serialized), with legacy
`UserFileTypeDefinitions.xml` (`<FileTypes><FileType><MimeType/><Signature type="RAW|ASCII"><Bytes>hex</Bytes><Offset RelativeToStart="bool">n</Offset></Signature>`).
The UI can add or edit a type with multiple signatures, each signature having hex or ASCII bytes, an
offset, and start or end. The optional "alert as interesting file" option takes a set name.

**Module behaviour**: for each file it calls `getMIMEType` and sets it. If any user types exist, it reads
1024 bytes and re-matches the user types. A matching type with `createInterestingFileHit` produces a
`TSK_INTERESTING_ITEM` (score LIKELY_NOTABLE, configuration = set name) with attributes
`TSK_SET_NAME` = the configured set name and `TSK_CATEGORY` = the MIME type. Summary INFO message: total
processing time and number of files.

**postmortem**: Tika's database (`tika-mimetypes.xml`, about 1600 types, with magic priorities, globs and
sub-class relationships) is the parity target. **Implement**: generate a Rust matcher at build time from
`tika-mimetypes.xml` (Apache-2.0), plus **container detectors** that Tika has in code:
* OLE2/CFB stream names (`cfb` crate) → doc/xls/ppt/msg/msi, and encrypted OOXML
  (`EncryptionInfo` + `EncryptedPackage`) → `application/x-tika-ooxml-protected` → `x-ooxml-protected`.
* ZIP entries (`zip` crate): `[Content_Types].xml` → OOXML subtypes; `mimetype` → ODF; `META-INF/MANIFEST.MF` → jar; also apk, epub, kmz, xpi.
* Text and charset sniffing (`chardetng`).

The crates `infer`, `tree_magic_mini` and `file-format` are useful references but are not
parity-complete. MIME strings must match Tika's exactly, because rules, mismatch tables and user filters
compare strings.

### 2.3 Extension Mismatch Detector (`fileextmismatch`, file module)

**Settings** (per job): `skipFilesWithNoExtension` (true), `skipKnownFiles` (true), and
`checkType ∈ {ALL, NO_TEXT_FILES, ONLY_MEDIA_AND_EXE}` (default **ONLY_MEDIA_AND_EXE**). Global: a map
MIME → allowed extensions in `<userConfigDir>/mismatch_config.settings`, seeded from the bundled
`mismatch_config.xml` (`<mismatch_config><signature mimetype=".."><ext>..</ext>...`). The editor UI has
add/remove for MIME types and extensions. A context menu on a file offers "Add extension <ext> to
<mime>".

**Algorithm**: skip known files (if set), non-files, `UNALLOC_BLOCKS`, `UNUSED_BLOCKS`, `SLACK`, and
**deleted** files (meta or name flag UNALLOC). Get the extension (lower-case). Return no mismatch when the
extension is empty and `skipFilesWithNoExtension` is set. Get the MIME type. `NO_TEXT_FILES`: skip
`text/plain` when there is an extension. `ONLY_MEDIA_AND_EXE`: only check the MIME types in the list
below. **Mismatch = the MIME is in the map and the extension is not in its allowed set.** A MIME type not
in the map is never a mismatch.

`ONLY_MEDIA_AND_EXE` MIME list: image/{bmp,gif,jpeg,png,tiff,x-ms-bmp}; application/{dos-exe,exe,
x-dosexec,x-exe,x-msdownload,msword,pdf,rtf,vnd.ms-excel,vnd.ms-powerpoint,
vnd.oasis.opendocument.{presentation,spreadsheet,text},x-msoffice,x-ooxml}, and the 7 OOXML
`vnd.openxmlformats-officedocument.*` (docx, dotx, xlsx, xltx, pptx, potx, ppsx).

Default map (excerpt; 65 MIME entries in total): `application/x-dosexec: exe`; `application/pdf: pdf`;
`image/jpeg: gif,jfi,jfif,jif,jpe,jpeg,jpg,"jpg:ms-thumbnail",png,tile`; `image/png:
avatar,gif,jpeg,jpg,png`; `application/zip: amo,docx,dotx,jar,kmz,potm,potx,ppam,ppsm,pptm,pptx,vs,wmz,
xlam,xlsb,xlsm,xlsx,xltm,xpi,zip`; `application/x-msoffice:` 33 legacy Office and OLE extensions
(including `automaticdestinations-ms, db, msi, mst, feed-ms`); `text/plain:` about 190 extensions;
plus audio/video/archive types. **Copy the XML verbatim** into postmortem's defaults.

**Artifact**: `TSK_EXT_MISMATCH_DETECTED` (AnalysisResult, score LIKELY_NOTABLE, justification "File has
MIME type of <mime>", **no attributes**). Summary INFO: processing time and file count.

### 2.4 Embedded File Extractor (`embeddedfileextractor`, file module)

Output folder: `<case>/ModuleOutput/EmbeddedFileExtractor/<fileObjId>/...`. All extracted bytes are
written **XOR1-encoded** (TSK `EncodingType.XOR1`, which keeps antivirus software from quarantining
case files: a fixed ASCII header `TSK_CONTAINER_XOR1_xxxxxxxxxxxxx` followed by each byte XOR 0xCA. Verify
this against the TSK `EncodedFileOutputStream` before relying on it). Derived files are added with that encoding, and `context.addFilesToJob`
re-injects them as high-priority tasks, so they run through the whole file pipeline, recursively.
Directory and file creation goes through `FileTaskExecutor` (a thread pool of at least 4 threads; each
`exists`/`mkdirs`/`createNewFile` gets 4 attempts, after 0, 5, 10 and 15 min, each with a 10-minute
timeout; this guards against slow network shares), and the retry statistics go to the `task_retry_stats`
log.

**Skip**: `UNALLOC_BLOCKS`, `SLACK`, `known == KNOWN`, non-files. Dispatch on the MIME type:

**A. Archives (7-Zip-JBinding, native 7-Zip)**
* Supported MIME types: `application/zip`, `application/x-7z-compressed`, `application/gzip`,
  `application/x-gzip`, `application/x-bzip2`, `application/x-tar`, `application/x-gtar`,
  `application/x-rar-compressed`. RAR is opened with an explicit RAR format (so a ZIP inside a RAR is not
  mis-detected); everything else uses auto-detect.
* **Re-extraction**: if the archive already has children and its output directory exists, the existing
  derived files (`findFilesByParentPath(rootDataSourceId, archivePath)`) are loaded into a status map
  `{EXISTS, UPDATE, SKIP}`. An existing file is only re-written when the new size is larger.
* **Zip-bomb defences**:
  * **Depth**: `Archive{depth, rootArchiveId}` is tracked per job. At depth **4** (`MAX_DEPTH`) the root
    archive is flagged.
  * **Ratio**: for items with uncompressed size ≥ **500 000 000 bytes** (decimal), a ratio
    `size / packedSize ≥ 600` flags the root archive.
  * The ratio check is skipped for deleted (UNALLOC) archives and for `x-gzip`.
  * Once the root archive is flagged, every nested archive under it is skipped.
  * Flagging creates `TSK_INTERESTING_ITEM` on the **root** archive (score LIKELY_NOTABLE, set "Possible
    Zip Bomb") with `TSK_SET_NAME`="Possible Zip Bomb", `TSK_DESCRIPTION`="Zip Bomb Detected <name>" and
    `TSK_COMMENT` (details: the ratio or depth and the path), and posts a WARNING inbox message.
* **Disk space**: skip an item if free space minus the item size would fall below **1 GB** (decimal).
  This is dead code in practice, because free space is reported as unknown.
* **Item loop**:
  * Path: the 7-Zip `PATH` property. When the path is empty: `.gz` → base name, `.tgz` → `base.tar`,
    `.bz2` → base name, otherwise `/<archiveName>/<index>`. ADS colons are stripped.
  * Build an `UnpackedTree` from the path tokens (split on `/` and `\`), keeping the raw `PATH_BYTES`
    per token.
  * Encrypted item with no password: count it and skip it.
  * Local path: `<moduleDir>/<archiveObjId>/<index/1000>/<index>` (1000 files per sub-folder).
  * Folders are created with mkdirs; files are created empty first.
  * Then `extract(indices[], test=false, callback)` runs in index order, reusing one output stream (to
    work around a JBinding leak).
  * MIME is sniffed with Tika on the first write chunk.
  * Times: create, access and modify times come from the archive (seconds); ctime = 0.
  * A non-OK result marks the unpack as unsuccessful.
* **DB insert**: after extraction, walk the tree depth-first. `addDerivedFile(name, localRelPath, size,
  ctime, crtime, atime, mtime, isFile, parent, rederive="", tool=moduleName, toolVersion="", other="",
  XOR1, tx)`. Transactions are committed every **1000** inserts. Filename **charset fix**: for each
  directory, concatenate the raw name bytes of its children (up to about 1000 bytes), run ICU
  `CharsetDetector`, and if the confidence is ≥ 90 re-decode the names. This fixes CP437/Shift-JIS ZIP
  names. A child whose MIME type is a supported archive gets an `Archive{depth+1}` entry for the bomb
  check.
* **Encryption**: if any item was encrypted, create `TSK_ENCRYPTION_DETECTED` (score NOTABLE) on the
  archive. The comment / `TSK_COMMENT` is "Password protection detected." when **every** item is encrypted,
  and "Content-only Encryption (Archive File)" otherwise. A WARNING inbox message is posted.
* A failed open or extract posts an ERROR inbox message, but only when the archive is allocated.
  `ModuleContentEvent(archive)` is fired once, followed by `addFilesToJob(allExtracted)`.
* **"Unzip contents with password"** (context menu on an archive): prompts for a password, runs `unpack`
  with no job context, re-prompts on failure, and on success opens the Run Ingest Modules wizard for the
  archive.

**B. Office and PDF embedded content** (Apache Tika / POI):
* OOXML (docx, pptx, xlsx): Tika `AutoDetectParser` with an `EmbeddedDocumentExtractor` that keeps
  every embedded item whose detected top-level type is image, video, application or audio. Name: the
  resource name from the metadata (directory parts stripped, sanitised), or `image_<n>`. When the name
  has no extension, the extension is appended from the MIME type. Items are de-duplicated by name.
  Bytes are XOR1-written.
* DOC: POI `HWPFDocument.getPicturesTable().getAllPictures()` → `image_<n>.<suggestedExt>`.
* PPT: `HSLFSlideShow.getPictureData()`. Only JPEG, PNG, WMF, EMF and PICT are kept; others are skipped.
* XLS: `HSSFWorkbook.getAllPictures()`.
* PDF: Tika PDF parser with `extractInlineImages = true` and `uniqueInlineImagesOnly = true`. Every
  embedded resource (attachments and images) is written as `extract_<n>.<ext>`. The derived file name is
  the original resource name.
* Skipped when the file already has children and the output folder exists. No timestamps; atime is
  passed as mtime.

**Rust status**:
* zip: `zip` crate (supports ZipCrypto and AES, deflate64 via a feature).
* 7z: `sevenz-rust2` crate (pure Rust, supports AES).
* gzip/bzip2/xz: `flate2` (miniz_oxide), `bzip2` (libbz2-rs-sys, pure), `lzma-rs`/`xz2`.
* tar: `tar` crate.
* **RAR**: there is no complete pure-Rust RAR decoder (`unrar` is a C++ binding). **Implement** RAR
  4/5 decompression or ship an external `unrar`/`7z` as an optional feature.
* Office: `cfb` crate + **implement** the DOC picture table (Data stream + PICF/BLIP records), the PPT
  `Pictures` stream BLIP records and the XLS `MsoDrawingGroup` BStore. OOXML = zip + `word/media`,
  `ppt/media`, `xl/media`, `*/embeddings`.
* PDF: `lopdf` (EmbeddedFiles name tree, FileAttachment annotations, image XObjects with
  DCTDecode/JPXDecode passthrough).
* Charset detection: `chardetng` + `encoding_rs`.

### 2.5 Picture Analyzer (`pictureanalyzer`, file module; formerly "Exif Parser")

A plugin registry of `PictureProcessor{process(ctx, file), mimeTypes()}` (looked up through the service
loader). Skip non-files, known files, `UNALLOC_BLOCKS` and `SLACK`. For each processor whose MIME set
contains the file's MIME type (case-insensitive), call `process`.

**EXIFProcessor** (MIME: `image/jpeg`, `image/tiff`, `audio/x-wav`; drew metadata-extractor):
* `TSK_DATETIME_CREATED` ← ExifSubIFD `DateTimeOriginal` (0x9003), interpreted in the **data source's
  time zone** (Image data sources only, otherwise the JVM default).
* `TSK_GEO_LATITUDE` and `TSK_GEO_LONGITUDE` ← the GPS directory GeoLocation (decimal degrees).
* `TSK_GEO_ALTITUDE` ← GPS `Altitude` (0x0006) rational; infinity becomes 0. The altitude **ref** (below
  sea level) is ignored.
* `TSK_DEVICE_MODEL` (0x0110) and `TSK_DEVICE_MAKE` (0x010F) from IFD0, when not blank.
* If any attribute was found and no identical artifact exists: create `TSK_METADATA_EXIF` (AnalysisResult,
  score NONE) **and** `TSK_USER_CONTENT_SUSPECTED` (score UNKNOWN, `TSK_COMMENT` = "EXIF metadata data
  exists for this file."). Both are posted together.

**HEICProcessor** (MIME: `image/heif`, `image/heic`; JNI to libheif + libde265 + libjpeg):
* Reads the **whole file into memory** and converts it to JPEG at
  `ModuleOutput/HEIC/<objId>/<baseName>.jpg` (multi-image HEIF can produce several JPEGs).
* Every `*.jpg`/`*.jpeg` in that folder is added as a derived file (size and times from the local file;
  encoding NONE), re-queued with `addFilesToJob`, and announced with a content-changed event. That gives
  the JPEG EXIF parsing, hashing and so on.
* Silently disabled when the native library fails to load.

**Rust status**: EXIF uses `kamadak-exif` (TIFF, JPEG, HEIF, PNG, WebP) or `nom-exif` (which also reads
MOV/MP4 metadata); WAV EXIF (the RIFF `LIST/exif` chunk) must be **implemented**. HEIC → JPEG needs an
HEVC decoder. There is **no production pure-Rust HEVC decoder**, so the options are: (a) an optional
`libheif-rs` feature (C dependency), (b) only extract the embedded EXIF and thumbnails (the HEIF `thmb`
item is often HEVC too), or (c) a long-term pure-Rust HEVC intra decoder (HEIC stills are intra-only,
which makes it tractable). Recommendation: do (b) now and keep (a) optional.

### 2.6 Encryption Detection (`encryptiondetection`, file module + data-source module)

**Settings** (shared): `minimumEntropy` 7.5 (valid range 6.0–8.0), `minimumFileSize` 5 242 880
(5 MiB; must be ≥ 1), `fileSizeMultipleEnforced` true (size % 512 == 0), `slackFilesAllowed` true. The
range is validated in startUp, and an invalid value fails the job.

**Entropy**: a byte histogram over the whole content, `H = -Σ p·log2 p`, where p = count / **(size − 1)**
(an off-by-one; postmortem should use size but keep the thresholds). Cancellation is checked every
10 000 bytes. On cancellation it returns 0.

**File module**:
* Eligible: not `UNALLOC_BLOCKS`, `UNUSED_BLOCKS`, `VIRTUAL_DIR` or `LOCAL_DIR`; `SLACK` only if allowed;
  not KNOWN; not deleted (meta UNALLOC); not `/hiberfile.sys` or `/pagefile.sys` in the root
  (case-insensitive; note the misspelt "hiberfile").
* **Suspected**: MIME `application/octet-stream`, and the size qualifies: files with extension `db` need
  ≥ 65 536 bytes (SQLCipher), other files need ≥ minimum size and, if the multiple rule is enforced, a
  size that is a multiple of 512. Then entropy ≥ minimum. → `TSK_ENCRYPTION_SUSPECTED` (score
  LIKELY_NOTABLE, `TSK_COMMENT` = "Suspected encryption due to high entropy (%f).").
* **Detected (password-protected)**:
  * `application/x-ooxml-protected` → yes.
  * msword, ms-excel, ms-powerpoint or pdf → a full Tika parse; `EncryptedDocumentException` → yes.
  * `application/x-msaccess` → open with Jackcess and the crypt codec provider;
    `InvalidCredentialsException` or a non-null DB password → yes.
  * Result: `TSK_ENCRYPTION_DETECTED` (score NOTABLE, comment "Password protection detected.").
* Each artifact also gets a DATA inbox message "<type> Match: <name>", with the entropy for suspected
  files.

**Data-source module** (Image data sources only; it errors if the image has no paths): iterate over the
volume systems and their volumes.
* **BitLocker**: read the first 0x54 bytes. Match `-FVE-FS-` at offset 3, sectors-per-cluster
  (offset 0x0D) ∈ {1,2,4,...,128}, and reserved sectors (0x0E), FAT count (0x10), root entries (0x11),
  sectors16 (0x13), sectors-per-FAT (0x16) and sectors32 (0x20) all zero. → `TSK_ENCRYPTION_DETECTED` on the
  **Volume** (NOTABLE, "Bitlocker encryption detected.").
* Otherwise, a **volume with no recognised file system** with entropy ≥ minimum →
  `TSK_ENCRYPTION_SUSPECTED` on the volume.
* **Bug**: it returns after flagging the first volume. postmortem should flag all of them.

**Rust**:
* Entropy: a SIMD histogram (4 interleaved u32 count tables to avoid store-to-load stalls) over mmapped
  reads.
* Office encryption, **implement**:
  * OLE `EncryptionInfo` stream.
  * DOC FIB `fEncrypted` (bit 8 of `FibBase` flags at offset 0x0A).
  * XLS `FILEPASS` (0x002F) record.
  * PPT `CryptSession10Container` / `EncryptedSummary` stream.
  * PDF `/Encrypt` in the trailer (`lopdf`).
  * Access: Jet3/Jet4/ACE header at 0x42, and the database password mask XOR (Jackcess logic); ACCDB
    `0x299` encryption flags.
* BitLocker: trivial. Also add LUKS (`LUKS\xBA\xBE`), VeraCrypt (entropy only), FileVault/APFS crypto
  flags, and BitLocker To Go (`MSWIN4.1` + FVE metadata). These are new.

### 2.7 Interesting Files Identifier (`interestingitems`, file module) and the FilesSet rule language

**FilesSet** = `{name (required), description, ignoreKnownFiles, ignoreUnallocatedSpace, standardSet,
versionNumber ≥ 0, rules: Map<uuid, Rule>}`. **Rule** = `{optional name, uuid, exclusive?,
MetaTypeCondition (required), optional FileName (FullName | Extension), ParentPath, MimeType,
FileSize, Date}`. A rule needs the meta-type condition **plus at least one** of name, path, MIME, size or
date. A rule matches when **all** of its conditions pass.

`fileIsMemberOf(file) -> Option<ruleName>`:
1. `ignoreKnown && known == KNOWN` → no match. `ignoreUnalloc && type ∈ {UNALLOC_BLOCKS, SLACK, UNUSED_BLOCKS}` → no match.
2. If there are no inclusive rules: with no exclusive rules either → no match. With only exclusive rules →
   candidate name "Not Excluded".
3. Otherwise → the name of the first satisfied inclusive rule (HashMap iteration order, so effectively
   arbitrary), or no match.
4. Any satisfied **exclusive** rule → no match.

Conditions:

| Condition | Semantics |
|---|---|
| MetaType | `FILES` → `file.isFile()`; `DIRECTORIES` → meta type DIR or VIRT_DIR; `FILES_AND_DIRECTORIES` → REG/DIR/VIRT_DIR; `ALL` → true. XML: `typeFilter="file|dir|all|files_and_dirs"` (`files_and_dirs` maps to ALL); a missing attribute means FILES |
| FullName | exact, case-insensitive `equalsIgnoreCase(name)`; or regex `find()` (a substring match unless anchored) |
| Extension | a list of values (split on `,`, leading `.` stripped) compared case-insensitively to `getNameExtension()`; or regex `find()` |
| ParentPath | `textMatches(parentPath + "/")`. Plain text = case-insensitive **substring** (the UI forces a leading and trailing `/`, e.g. `/Windows/Temp/`); or regex `find()` |
| MimeType | exact, case-sensitive equality with `file.getMIMEType()` |
| FileSize | `size <cmp> value × unit`. cmp ∈ `< ≤ = > ≥` (XML also accepts `<= == >=`), units Bytes, Kilobytes, Megabytes or Gigabytes (1024-based). The UI rejects 0 unless the comparator is `=` |
| Date | `crtime > now − days·86400 OR mtime > now − days·86400` |

Regex flavour: Java `Pattern`. UI-created regexes are **CASE_INSENSITIVE**. Regexes loaded from XML are
**case-sensitive**. Names that contain `*` in XML are treated as **regex** (so `*.exe` fails to compile and
the rule is rejected; it is not a glob). Illegal characters in a plain name: `\ / : * ? " < >`; in a
plain path: `\ : * ? " < >`.

**XML format** (import, export and the standard sets):

```xml
<INTERESTING_FILE_SETS>
 <INTERESTING_FILE_SET name="Cloud Storage" description="..." ignoreKnown="false"
      ingoreUnallocated="false" standardSet="true" versionNumber="2">
  <NAME name="Dropbox" regex="false" typeFilter="file">dropbox.exe</NAME>
  <EXTENSION name="Docs" regex="false" typeFilter="file" pathFilter="/Users/"
      mimeType="..." comparatorSymbol=">" sizeValue="10" sizeUnits="Megabytes"
      daysIncluded="7" isExclusive="false">doc,docx</EXTENSION>
 </INTERESTING_FILE_SET>
</INTERESTING_FILE_SETS>
```

(The attribute is spelt `ingoreUnallocated` in the code. The export writer never writes it, so the flag
is lost on round-trip. A rule with no name condition is written as `<EXTENSION>` with empty text.
`pathRegex` is used instead of `pathFilter` for regex paths.)

**Storage**: `<moduleConfigDir>/InterestingFileSets.settings` (serialized map); legacy
`InterestingFilesSetDefs.xml` in the user config directory is migrated.
**Standard sets** ship in `InterestingFileSetRules/*.xml`: *Cloud Storage* (31 rules), *Cryptocurrency
Wallets* (39), *Encryption Programs* (19), *Privacy Programs* (45) and *Remote Monitoring and Management
Programs* (793). They are merged at startup (`copyOnNewer`): a missing standard set is added; one with a
higher `versionNumber` replaces the older copy; a user set with the same name is renamed
`<name> (Custom)` (repeated if needed). Standard sets are read-only in the UI; "Copy Set" makes an
editable copy.

**Module**:
* Per-job settings: enabled and disabled set names. A set **not** in the disabled list is enabled, so
  new sets default to on.
* On the first instance per job, it takes a **snapshot** of the enabled sets, so editing the rules
  during ingest does not affect the running job.
* Skips `SLACK` files.
* For **each** matching set (not only the first): create `TSK_INTERESTING_ITEM` (AnalysisResult, score
  **LIKELY_NOTABLE**, configuration = set name) with `TSK_SET_NAME` = the set name and
  `TSK_CATEGORY` = the matched rule name (or "Not Excluded").
* Also posts a DATA inbox message "Interesting File Match: <set>(<file>)", with details "File: <path>"
  and "Rule Set: <set>", and `uniqueKey` = the file name.

**Rust**: use the `regex` crate (no look-around or back-references) with `fancy-regex` as a fallback
when a pattern fails to compile. Pre-compile every set into a `RegexSet` for names and paths, plus a
hash set for exact names and extensions, so one call per file evaluates every rule.

### 2.8 Data Source Integrity (`dataSourceIntegrity`, data-source module, **stage 2**)

* Settings: `computeHashes` (true) and `verifyHashes` (true). startUp fails if both are false.
* Only for `Image` data sources. Other data sources get an INFO message ("Skipping non-disk image data source <ds>").
* **Mode**: if the image has any stored MD5, SHA-1 or SHA-256 (from E01 metadata or entered by the user
  in the Add Data Source wizard), the mode is **VERIFY** over exactly those algorithms. Otherwise the mode
  is **COMPUTE** over all three. A disabled mode produces an INFO "skipping" message.
* Reads in chunks of **64 × sector size** (32 KiB if the sector size is 0, matching libewf's chunk size),
  feeds every digest, updates the determinate progress per chunk, and checks cancellation per chunk.
* **VERIFY**: compare the lowercase hex with the stored value. The comparison is case-sensitive, so a
  stored upper-case hash fails; normalise it. On any mismatch: `TSK_VERIFICATION_FAILED`
  (AnalysisResult on the image, score NOTABLE, `TSK_COMMENT` = "<alg> hash verification failed:
  Calculated: x, Stored: y ..."), and a WARNING inbox message listing every algorithm's match status.
  On success: an INFO message.
* **COMPUTE**: `img.setMD5/setSha1/setSha256` (persisted to `tsk_image_info`) and an INFO message with
  the values.
* **Rust**: hash with three parallel digests in separate threads, fed from one reader through a
  double-buffered channel. MD5 is the bottleneck at about 700 MB/s per core, SHA-1 and SHA-256 with SHA-NI
  reach 2 GB/s and more. Read E01 natively with an `ewf` reader. Add an option to use the E01-internal
  per-chunk checksums.

### 2.9 Other `modules/` items

* `modules/exif/` contains only a leftover `Bundle_ja.properties`. The EXIF functionality lives in the
  Picture Analyzer.
* Default **file pipeline order** for the core modules: Hash Lookup → File Type ID → Embedded File
  Extractor → (Picture Analyzer) → Keyword Search → Email Parser → Extension Mismatch → Interesting Files →
  PhotoRec → GPX. Encryption Detection, Central Repository and others are appended in the order of
  their templates.


---

## 3. External-tool and specialised modules

### Shared infrastructure for external-tool modules

Several modules below shell out to a bundled native executable. They all use the same plumbing, which postmortem must replicate (or make unnecessary):

- **Locating the binary:** `InstalledFileLocator.locate("<subdir>/<exe>", <module package>, false)` resolves the binary inside the NetBeans install tree (`thirdparty/<tool>/...` copied into `release/modules/...`). The module fails `startUp()` with an `IngestModuleException` if the file is missing or `!canExecute()`. Most of these modules are **Windows-only**: their `startUp` checks `PlatformUtil.isWindowsOS()`, and several also check `is64BitOS()`.
- **`__COMPAT_LAYER=RunAsInvoker`:** this environment variable is set on every child process (aLEAPP, iLEAPP, Plaso, PhotoRec, Volatility) so that Windows does not raise a UAC elevation prompt. It is Windows-specific and has no meaning elsewhere.
- **`ExecUtil.execute(ProcessBuilder, ProcessTerminator)`:** polls `process.waitFor(5 s)` in a loop. On each wake-up it calls `terminator.shouldTerminateProcess()`, and if that returns true it kills the process tree (`terminateProcess`). `ExecUtil.execute(pb)` with no terminator uses a 30-day ceiling.
- **Terminators:**
  - `DataSourceIngestModuleProcessTerminator(context[, useGlobalTimeOut])` returns true when `context.dataSourceIngestIsCancelled()`. When `useGlobalTimeOut=true`, it also returns true after the global "process timeout" (`UserPreferences.getIsTimeOutEnabled()`, default **false**; `getProcessTimeOutHrs()`, default **60 h**).
  - `FileIngestModuleProcessTerminator` behaves the same, but checks `fileIngestIsCancelled()`.
  - `getTerminationCode()` returns `NONE`, `CANCELATION` or `TIME_OUT`.
- **Output locations:**
  - `Case.getModuleDirectory()/<Tool>/<timestamp>` holds persistent output (reports, logs).
  - `Case.getTempDirectory()/<Tool>/...` holds scratch copies.
- **Exporting content to disk:** `ContentUtils.writeToFile(AbstractFile, File, cancelCheck)` copies an image file to the local disk before the external tool can see it. This full copy is the dominant cost for carving and VM extraction.

**Postmortem recommendation:**
- Provide one `ExternalTool` service with these parts: a binary resolver (bundled path, then `PATH`, then a user override), a `std::process::Command` wrapper with a cancellation token, an optional wall-clock timeout and process-group kill (`command-group` crate / Job Objects on Windows), and stdout/stderr tee to log files in the module output directory.
- Make "Windows-only" a non-issue wherever a pure-Rust implementation exists. Keep external tools as optional *plugins* for parity.

---

### aLEAPP / iLEAPP Analyzers (`modules/leappanalyzers`)

#### Factories and modules
- `ALeappAnalyzerModuleFactory` / `ILeappAnalyzerModuleFactory` are **data-source-level** modules with no settings (the `IngestModuleFactoryAdapter` defaults). The version is the Autopsy version.
- `ALeappAnalyzerIngestModule` / `ILeappAnalyzerIngestModule` are near-identical code. The differences are:

| | aLEAPP (Android) | iLEAPP (iOS) |
|---|---|---|
| exe | `aLeapp/aleapp.exe` | `iLeapp/ileapp.exe` |
| XML mapping | `aleapp-artifact-attribute-reference.xml` | `ileapp-artifact-attribute-reference.xml` |
| extra arg | `-w` (wrap text in HTML report) | none |
| platform checks | 64-bit **and** Windows; otherwise `startUp` throws | same |
| Detection: FS types (images only) | EXT3/EXT4/YAFFS2(_DETECT) mean **Android**. NTFS/HFS/APFS(_DETECT) mean **not Android**. Anything else, or FAT only, falls through to the file check. | EXT2/3/4/EXT_DETECT/YAFFS2/NTFS mean **not iOS**. HFS/APFS/FAT/unknown fall through. |
| Detection: indicator files (`findFiles(ds, name, parentPathSubstring)`, first hit wins) | `build.prop` in `/system/`; `packages.xml` in `/data/system/`; `settings.db` in `/data/com.android.providers.settings/databases/` | `sms.db` in `/private/var/mobile/Library/SMS/` or `/var/mobile/Library/SMS/`; `AddressBook.sqlitedb` in `.../Library/AddressBook/`; `com.apple.mobilephone.plist` in `.../Library/Preferences/` (both `/private/var/mobile` and `/var/mobile`) |

If the device detection fails, the module posts an inbox DATA message ("aLeapp skipped: data source does not appear to be an Android device.") and returns OK. A `TskCoreException` during the indicator search **fails open**, treating the data source as a match.

#### Algorithm (`process(dataSource, progress)`)
1. Create `temp/<aLeapp|iLeapp>/fs_<dataSourceObjId>/`.
2. **Path-list pass:** run `<exe> -p` with cwd set to that temp directory. Stdout goes to `<x>Leapp_paths.txt` and stderr to `<x>Leapp_paths_error.txt`. The run uses the global-timeout terminator.
   - A non-zero exit, or an empty path list, posts an inbox ERROR ("Error running aLeapp, see log file.") and the module returns ERROR.
   - When reading the paths file, lines containing `path list generation` or shorter than 2 characters are skipped. Each remaining line is trimmed and kept as a glob.
3. **Logical-file data sources only** (`LocalFilesDataSource`): `findLeappFilesToProcess` looks at children of `/` (`findFiles(ds, "%", "/")`) that have a `localAbsPath`, are not virtual, and have an extension in {`zip`,`tar`,`tgz`}. For each one:
   - Create `ModuleOutput/<x>Leapp/<yyyy-MM-dd HH-mm-ss z>/`.
   - Run `<exe> -t <ext> -i <localAbsPath> -o <outdir> [-w]` with cwd set to the output directory. Logs go to `<x>Leapp_out.txt` / `_err.txt`.
   - Register the HTML report.
   - Call `LeappFileProcessor.processFiles(...)`, which attaches artifacts to the **archive file**.

   The progress bar is determinate over the archives.
4. **Extract the paths LEAPP asked for:**
   - Each glob from step 2 has `*` replaced by SQL `%`, then is normalised to `/`.
   - The glob is split into name and path, then `findFiles(ds, name[, path])` runs. These are DB LIKE queries.
   - Each hit is written to `temp/.../fs_<id>/<parentPath>/<name>`, preserving the directory tree; directories are just created.
   - When writing: `:` in names becomes `-`; names `.`/`..` (regex `.` and `..`, which in effect only matches 1–2-character names) are skipped; names ending in `-slack` are skipped.
5. **Run on the reconstructed tree:** `<exe> -t fs -i temp/.../fs_<id> -o ModuleOutput/<x>Leapp/<ts>/`, then register the report and call `processFileSystem`, which attaches artifacts to the **data source**.
6. Post an inbox DATA message ("aLeapp Processing Completed").

**Report registration:** walk the output directory for the first `*index.html`. It is only added via `Case.addReport(path, moduleName, "aLeapp Html Report")` if a sibling directory `_TSV Exports` exists; otherwise LEAPP produced no data.

#### LeappFileProcessor: TSV to blackboard
**Construction:**
- `loadCustomArtifactsAttributes`:
  - Always creates the custom artifact type `TSK_IP_DHCP` ("DHCP Information") via `getOrAddArtifactType`.
  - If `<userConfigDir>/<aLeapp|iLeapp>-custom-artifact-attribute-list.csv` exists, it is read as comma-separated values with a header row. Columns are `kind,name,description[,valueType]`.
  - `kind=artifact` creates a custom artifact type with the upper-cased name.
  - Otherwise a custom attribute type is created. Value types map as follows: string/json/integer/long/double all become a **STRING** attribute (a bug: numeric types are stored as strings), byte becomes BYTE, and datetime becomes DATETIME.
- `configExtractor()` copies the bundled XML into the user config directory, **overwriting** any existing copy.
- `loadConfigFile()` parses that XML, then the optional user override `<userConfigDir>/<aLeapp|iLeapp>-artifact-attribute-reference-user.xml`. On a key collision, the user file replaces the artifact type and comment for that TSV file, but its `<AttributeName>` columns are **appended** to the bundled column list, not substituted for it.

**XML schema** (root `<aLeap_Files_To_Process>` / `<iLeap_Files_To_Process>`):
```xml
<FileName filename="Call Logs.tsv" description="...">           <!-- key: trimmed, lower-cased file name -->
  <ArtifactName artifactname="TSK_CALLLOG" comment="null|text">  <!-- comment != "null" becomes a TSK_COMMENT attr on every row -->
    <AttributeName attributename="TSK_DATETIME_START|null" columnName="Call Date" required="yes|no"/>
  </ArtifactName>
</FileName>
```
- `attributename="null"` means the column is ignored.
- Column names are matched to the TSV header case-insensitively after trimming. Leading or trailing whitespace in `columnName` in the XML makes the entry be skipped, with an error logged.
- Unknown artifact or attribute names are logged as SEVERE and that mapping is dropped.

**Per-TSV processing:**
- The output directory is walked recursively for `*.tsv` files whose lower-cased name is a key in the map.
- Each file is parsed with Jackson CSV (tab separator, header row first).
- For each row:
  - Each mapped column takes the cell value, with all `\p{C}` (control or non-printable) characters removed.
  - **Type coercion:** STRING/JSON values are used as-is. For INTEGER, LONG, DOUBLE and BYTE, the value is trimmed, a blank value becomes null, and the value is parsed via `Double.valueOf` and then truncated. DATETIME uses `SimpleDateFormat("yyyy-MM-d HH:mm:ss")` in the **JVM default time zone** (a bug: it should be UTC), divided by 1000. A value that is blank or matches `^\s*[0\.]*\s*$` becomes null.
  - For any type, if the attribute is `TSK_DOMAIN`, the value is replaced by `NetworkUtils.extractDomain(value)`.
  - If a required column is missing from the row, or its value is unparseable, **the whole row is dropped**. A missing optional value only drops that attribute.
  - If the XML gives a comment, `TSK_COMMENT=<comment>` is appended.
- **Routing**, based on a hard-coded `ACCOUNT_RELATIONSHIPS` map from lower-cased file name:
  - `message` calls `CommunicationArtifactsHelper.addMessage(...)`, which creates a TSK_MESSAGE plus accounts and relationships. The fields are:
    - `TSK_DIRECTION` "incoming"/"outgoing" sets direction.
    - `TSK_PHONE_NUMBER_FROM` sets the sender.
    - `TSK_PHONE_NUMBER_TO` is split on commas, **but the list is never passed** (a bug: the recipient is always null).
    - `TSK_COMMENT` becomes messageType.
    - `TSK_ATTACHMENTS` becomes a `FileAttachment` by path.
    - `TSK_READ_STATUS` 1 means READ, anything else UNREAD.
    - `TSK_TEXT`, `TSK_SUBJECT`.
    - `TSK_ID` becomes the self-account alternate ID.
    - `TSK_TEXT_FILE` is a source path, resolved via `findFiles` and parent-path suffix match. The artifact attaches to that file, or to the data source if not found.
  - `contact` calls `addContact(name, phone, home, mobile, email, others)`, but only if an account type is resolved.
  - `calllog` calls `addCalllog(...)`. Two quirks:
    - Both `TSK_DATETIME_START` and `TSK_DATETIME_END` write the start time (a bug: the end time is lost).
    - For outgoing calls with no callee, the caller is moved into the callee list.
  - `route` (Google Map Locations) calls `GeoArtifactsHelper.addRoute`, producing **TSK_GPS_ROUTE**. Start and end are both written into the start variables (a bug), so the route is effectively (start, 0/0).
  - `trackpoint` (Oruxmaps Tracks) accumulates `TrackPoint`s. Each segment-name change triggers `addTrack`, producing **TSK_GPS_TRACK** with the JSON attribute `TSK_GEO_TRACKPOINTS`. A final `addTrack` runs after the file.
  - Anything else is a plain `newDataArtifact` or `newAnalysisResult(score UNKNOWN)`, chosen by the artifact type's category, created on the data source or archive file. The batch is posted with `postArtifacts` (one post per TSV file).
- **Account type by file name** (`getAccountType`):
  - `zapya.tsv` → ZAPYA
  - imo files → IMO
  - textnow files → TEXTNOW
  - viber files → VIBER
  - xender files → XENDER
  - whatsapp files → WHATSAPP
  - `tangomessages messages.tsv` → TANGO
  - `shareit file transfer.tsv` → SHAREIT
  - line files → LINE
  - skype files → SKYPE
  - facebook messenger files → FACEBOOK
  - everything else → PHONE

  The relationship map also lists: `sms messages`, `mms messages`, `contacts`, `call logs`, `call logs2`, `oruxmaps tracks` (trackpoint), `google map locations` (route), `sms - imessage`, `call history`.

**Active aLEAPP mappings** (36 files; prefix `TSK_` omitted; `~~col~~` = ignored column; `*` = required):
| TSV file | Artifact type | Comment | Columns -> attributes (`*` = required) |
|---|---|---|---|
| accounts ce 0.tsv | SERVICE_ACCOUNT | accounts ce 0 | Name->USER_ID*; Type->PROG_NAME*; Password->PASSWORD* |
| accounts ce 10.tsv | SERVICE_ACCOUNT | accounts ce 0 | Name->USER_ID*; Type->PROG_NAME*; Password->PASSWORD* |
| authtokens 0.tsv | SERVICE_ACCOUNT | Authtokens | ~~ID~~; Name->USER_ID*; Account Type->PROG_NAME*; ~~Authtoken Type~~; Authtoken->PASSWORD* |
| accounts de 0.tsv | SERVICE_ACCOUNT | accounts de 0 | ~~Last password entry~~; Name->USER_ID*; Type->PROG_NAME* |
| accounts de 10.tsv | SERVICE_ACCOUNT | accounts de 0 | ~~Last password entry~~; Name->USER_ID*; Type->PROG_NAME* |
| Browser - Bookmarks.tsv | WEB_BOOKMARK | Browser Bookmarks | Added Date->DATETIME_CREATED*; URL->URL*; Name->TITLE*; ~~Parent~~; ~~Type~~ |
| Browser - Cookies.tsv | WEB_COOKIE | Browser Cookies | Last Access Date->DATETIME_ACCESSED*; Host->URL*; Name->NAME*; Value->VALUE*; Created Date->DATETIME_CREATED*; Expiration Date->DATETIME_END*; ~~Path~~ |
| Browser - Web History.tsv | WEB_HISTORY | Browser History | Last Visit Time->DATETIME_ACCESSED*; URL->URL*; Title->TITLE*; ~~Visit Count~~; ~~Hidden~~ |
| Browser - Keyword Search Terms.tsv | WEB_SEARCH_QUERY | Browser Keyword Search Terms | Last Visit Time->DATETIME_ACCESSED*; Term->TEXT*; URL->DOMAIN* |
| Browser - Login Data.tsv | SERVICE_ACCOUNT | Browser Login | Created Time->DATETIME_CREATED*; Username->USER_ID*; Password->PASSWORD*; Origin URL->URL; ~~Blacklisted by User~~ |
| Browser - Offline Pages.tsv | WEB_HISTORY | Browser Offline Pages | Creation Time->DATETIME_CREATED*; Last Access Time->DATETIME_ACCESSED*; Online URL->URL*; ~~File Path~~; Title->TITLE; ~~Access Count~~; ~~File Size~~ |
| Browser - Search Terms.tsv | WEB_SEARCH_QUERY | Browser Search Terms | Last Visit Time->DATETIME_ACCESSED*; Search Term->TEXT*; URL->DOMAIN*; ~~Title~~; ~~Visit Count~~ |
| Browser - Top Sites.tsv | WEB_HISTORY | Browser Top Sites | URL->URL*; ~~Rank~~; Title->TITLE; ~~Redirects~~ |
| Call Logs.tsv | CALLLOG |  | Call Date->DATETIME_START*; Phone Account Address->PHONE_NUMBER_FROM*; Partner->PHONE_NUMBER_TO*; Type->DIRECTION*; ~~Duration in Secs~~; ~~Partner Location~~; ~~Country ISO~~; ~~Data~~; ~~Mime Type~~; ~~Transcription~~; ~~Deleted~~ |
| Chrome - Bookmarks.tsv | WEB_BOOKMARK | Chrome Bookmarks | Added Date->DATETIME_CREATED*; URL->URL*; Name->TITLE*; ~~Parent~~; ~~Type~~ |
| Chrome - cookies.tsv | WEB_COOKIE | Chrome Cookies | Last Access Date->DATETIME_ACCESSED*; Host->URL*; Name->NAME*; Value->VALUE*; Created Date->DATETIME_CREATED*; Expiration Date->DATETIME_END*; ~~Path~~ |
| Chrome - Web History.tsv | WEB_HISTORY | Chrome History | Last Visit Time->DATETIME_ACCESSED*; URL->URL*; Title->TITLE*; ~~Visit Count~~; ~~Hidden~~ |
| Chrome - login data.tsv | SERVICE_ACCOUNT | Chrome Login | Created Time->DATETIME_CREATED; Username->USER_ID*; Password->PASSWORD; Origin URL->URL; ~~Blacklisted by User~~; ~~Browser Name~~ |
| Chrome - offline pages.tsv | WEB_HISTORY | Chrome Offline Pages | Creation Time->DATETIME_CREATED*; Last Access Time->DATETIME_ACCESSED*; Online URL->URL*; ~~File Path~~; Title->TITLE; ~~Access Count~~; ~~File Size~~ |
| Chrome - search terms.tsv | WEB_SEARCH_QUERY | Chrome Search Terms | Last Visit Time->DATETIME_ACCESSED*; Search Term->TEXT*; URL->DOMAIN*; ~~Title~~; ~~Visit Count~~ |
| Chrome - top sites.tsv | WEB_HISTORY | Chrome Top Sites | URL->URL*; ~~Rank~~; Title->TITLE; ~~Redirects~~ |
| Edge - Bookmarks.tsv | WEB_BOOKMARK | Edge Bookmarks | Added Date->DATETIME_CREATED*; URL->URL*; Name->TITLE*; ~~Parent~~; ~~Type~~ |
| Edge - cookies.tsv | WEB_COOKIE | Edge Cookies | Last Access Date->DATETIME_ACCESSED; Host->URL*; Name->NAME*; Value->VALUE*; Created Date->DATETIME_CREATED; Expiration Date->DATETIME_END; ~~Path~~ |
| Edge - Web History.tsv | WEB_HISTORY | Edge History | Last Visit Time->DATETIME_ACCESSED*; URL->URL*; Title->TITLE*; ~~Visit Count~~; ~~Hidden~~ |
| Edge - login data.tsv | SERVICE_ACCOUNT | Edge Login | Created Time->DATETIME_CREATED*; Username->USER_ID*; Password->PASSWORD*; Origin URL->URL; ~~Blacklisted by User~~ |
| Edge - offline pages.tsv | WEB_HISTORY | Edge Offline Pages | Creation Time->DATETIME_CREATED*; Last Access Time->DATETIME_ACCESSED*; Online URL->URL*; ~~File Path~~; Title->TITLE; ~~Access Count~~; ~~File Size~~ |
| Edge - search terms.tsv | WEB_SEARCH_QUERY | Chrome Search Terms | Last Visit Time->DATETIME_ACCESSED*; Search Term->TEXT*; URL->DOMAIN*; ~~Title~~; ~~Visit Count~~ |
| Edge - top sites.tsv | WEB_HISTORY | Edge Top Sites | URL->URL*; ~~Rank~~; Title->TITLE; ~~Redirects~~ |
| google play searches.tsv | WEB_SEARCH_QUERY | Google Play Search | Timestamp->DATETIME_ACCESSED*; Display->PROG_NAME*; query->TEXT* |
| google quick search box.tsv | WEB_SEARCH_QUERY | Google Quick Search | File Timestamp->DATETIME_ACCESSED*; ~~Type~~; Queries->TEXT*; ~~Response~~; ~~Source File~~ |
| installed apps library.tsv | INSTALLED_PROG | Installed Apps (Library) | Purchase Time->DATETIME; ~~Account~~; Doc ID->PROG_NAME* |
| installed apps - GMS.tsv | INSTALLED_PROG | Installed Apps GSM | Bundle ID->PROG_NAME* |
| installed apps - GMS_0.tsv | INSTALLED_PROG | Installed Apps GSM | Bundle ID->PROG_NAME* |
| installed apps vending.tsv | INSTALLED_PROG | Installed Apps (Vending) | First Download->DATETIME; Package Name->PROG_NAME*; ~~Title~~; ~~Install Reason~~; ~~Last Updated~~; ~~Auto Update?~~; ~~Account~~ |
| mms messages.tsv | MESSAGE | MMS messages | Date->DATETIME*; ~~MSG ID~~; Thread ID->THREAD_ID*; ~~Date sent~~; Read->READ_STATUS*; From->PHONE_NUMBER_FROM*; To->PHONE_NUMBER_TO*; ~~Cc~~; ~~Bcc~~; Body->TEXT*; source file->TEXT_FILE* |
| sms messages.tsv | MESSAGE | SMS messages | ~~Date~~; ~~MSG ID~~; Thread ID->THREAD_ID*; Address->PHONE_NUMBER_FROM*; Contact ID->PHONE_NUMBER_TO*; Date sent->DATETIME*; Read->READ_STATUS*; Body->TEXT*; ~~Service Center~~; ~~Error Code~~; source file->TEXT_FILE* |

**Commented-out aLEAPP mappings.** These are disabled because they duplicate the Android Jython module, but they exist in the file, and a user override can re-enable them. All are routed through `ACCOUNT_RELATIONSHIPS`:
- Viber: Messages/Contacts/Call Logs (MESSAGE, CONTACT, CALLLOG)
- Zapya (MESSAGE)
- Xender: Messages/contacts
- Whatsapp: Single Call Logs, Group Call Logs, Contacts, Messages Logs
- Line: Calllogs, messages, Contacts
- Contacts (CONTACT)
- Textnow: Contacts/Messages
- Shareit file transfer
- IMO: AccountId/Messages
- tangomessages messages
- Skype: Contacts/Call Logs/Messages Logs
- Facebook Messenger: Contacts/Calls/Chats
- Call Logs2 (CALLLOG)
- Cache Locations, Browser Locations, Oruxmaps POI (GPS_BOOKMARK)
- Oruxmaps Tracks (GPS_TRACK)
- Google Map Locations (GPS_ROUTE)

**Active iLEAPP mappings** (55 files):
| TSV file | Artifact type | Comment | Columns -> attributes (`*` = required) |
|---|---|---|---|
| Application State.tsv | INSTALLED_PROG | Application State | Bundle ID->PROG_NAME; Bundle Path->PATH*; Sandbox Path->PATH_SOURCE* |
| App Snapshots.tsv | SCREEN_SHOTS |  | App Name->PROG_NAME*; Source Path->PATH*; Date Modified->DATETIME*; ~~Png Path~~ |
| Bluetooth Other.tsv | BLUETOOTH_ADAPTER | Bluetooth Other | Name->NAME*; Address->MAC_ADDRESS*; Last Seen Time->DATETIME*; UUID->DEVICE_ID* |
| Bluetooth paired.tsv | BLUETOOTH_PAIRING | Bluetooth Paired | Last Seen Time->DATETIME; MAC Address->MAC_ADDRESS; Name Key->DEVICE_NAME*; ~~Name~~; ~~Device Product ID~~; ~~Default Name~~ |
| Bluetooth paired LE.tsv | BLUETOOTH_PAIRING | Bluetooth Paired | UUID->DEVICE_ID*; Name->DEVICE_NAME*; ~~Name Origin~~; ~~Address~~; ~~Resolved Address~~; Last Connection Time->DATETIME_ACCESSED |
| Calendar Items.tsv | CALENDAR_ENTRY | Calendar Items | Start Date->DATETIME_START*; ~~Start Timezone~~; End Date->DATETIME_END*; ~~End Timezone~~; ~~All Day?~~; Summary->CALENDAR_ENTRY_TYPE*; ~~Calendar ID~~; ~~Last Modified~~ |
| Call History.tsv | CALLLOG | Call Logs | Starting Timestamp->DATETIME_START*; Ending Timestamp->DATETIME_END; Phone Number->PHONE_NUMBER_FROM*; ~~Name~~; ~~Answered~~; ~~Call Type~~; ~~Call Direction~~; ~~Call Duration~~; ~~ISO County Code~~; ~~Location~~; ~~Service Provider~~ |
| Data Usage.tsv | PROG_RUN | Data Usage | Process Timestamp->DATETIME*; ~~Process First Timestamp~~; ~~Live Usage Timestamp~~; Bundle ID->PROG_NAME*; ~~Process Name~~; ~~WIFI In~~; ~~WIFI Out~~; WWAN IN->BYTES_RECEIVED*; WWAN Out->BYTES_SENT*; ~~Table ID~~ |
| Data Usage Process.tsv | PROG_RUN | Data Usage Process | Timestamp->DATETIME*; ~~Process First Timestamp~~; Process Name->PROG_NAME*; ~~Bundle ID~~; ~~Table ID~~ |
| DHCP Received List.tsv | IP_DHCP |  | Key->NAME*; Value->VALUE* |
| KnowledgeC App Activity.tsv | PROG_RUN | KnowledgeC App Activity | Entry Creation->DATETIME*; ~~Day of the Week~~; ~~Start~~; ~~End~~; ~~ZSTREAMNAME~~; Value String->PROG_NAME; ~~Activity Type~~; ~~Title~~; ~~Expiration Date~~; ~~Content URL~~; ~~Calendar Date~~; ~~Calendar End Date~~ |
| KnowledgeC Application Calendar.tsv | CALENDAR_ENTRY | InteractionC Application Activty Calendar | Start->DATETIME_START*; End->DATETIME_END*; ~~Bundle ID~~; Activity Type->CALENDAR_ENTRY_TYPE*; User Activity Required String->DESCRIPTION*; ~~Title~~; ~~Calendar Date~~; ~~Calendar End Date~~; Source ID->LOCATION*; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~Expiration Date~~; ~~UUID~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Application Activity Safari.tsv | WEB_HISTORY | KnowledgeC Application Activty Safari | Start->DATETIME_ACCESSED*; ~~End~~; Bundle ID->PROG_NAME*; ~~Activity Type~~; Content URL->URL*; ~~ID~~; ~~Unique ID~~; ~~Source ID~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~Expiration Date~~; ~~UUID~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Application Relevant Shortcuts.tsv | PROG_RUN | KnowledgeC Application Relevant Shortcuts | Start->DATETIME*; ~~End~~; Bundle ID->PROG_NAME*; ~~Day of the Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Device is Backlit.tsv | USER_DEVICE_EVENT | Device Backlit | Start->DATETIME_START*; End->DATETIME_END*; ~~Screen is Backlit~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Start~~; ~~End~~; ~~Entry Creation~~; ~~UUID~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Battery Level.tsv | USER_DEVICE_EVENT | Battery Level | Start->DATETIME_START*; End->DATETIME_END*; ~~Battery Level~~; ~~Usage in Seconds~~; ~~Day of the Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Bluetooth Connections.tsv | BLUETOOTH_PAIRING | KnowledgeC Bluetooth Connections | Start->DATETIME*; ~~End~~; Bluetooth Address->MAC_ADDRESS*; Bluetooth Name->DEVICE_NAME*; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of the week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Car Play Connections.tsv | USER_DEVICE_EVENT | KnowledgeC Car Play Connections | Start->DATETIME_START*; End->DATETIME_END*; ~~Car Play Connected~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~ |
| KnowledgeC Disk Subsystem Access.tsv | USER_DEVICE_EVENT | disk Subsystem | Start->DATETIME_START*; End->DATETIME_END*; Bundle ID->PROG_NAME*; ~~Value String~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Do Not Disturb.tsv | USER_DEVICE_EVENT | Do Not Disturb | Start->DATETIME_START*; End->DATETIME_END*; ~~Value~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Inferred Motion.tsv | USER_DEVICE_EVENT | Inferred Motion | Start->DATETIME_START*; End->DATETIME_END*; ~~Value~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Application in Focus.tsv | PROG_RUN | KnowledgeC Application In Focus | Start->DATETIME*; ~~End~~; Bundle ID->PROG_NAME*; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Installed Apps.tsv | INSTALLED_PROG | KnowledgeC Installed Apps | Start->DATETIME*; ~~End~~; Bundle ID->PATH*; ~~App Category~~; App Name->PROG_NAME*; ~~Day of Week~~; ~~GMT Offset~~; Entry Creation->DATETIME_CREATED*; ~~ZOBJECT Table ID~~ |
| KnowledgeC Device Locked.tsv | USER_DEVICE_EVENT | Device Locked | Start->DATETIME_START*; End->DATETIME_END*; ~~Is Locked?~~; ~~Usage in Seconds~~; ~~Day of the Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~ZOBJECT Table ID~~ |
| Media Playing.tsv | RECENT_OBJ | KnowledgeC Media Playing | Start->DATETIME_ACCESSED*; ~~End~~; Bundle ID->PATH*; ~~Now Playing Album~~; ~~Now Playing Artists~~; ~~Playing Genre~~; Playing Title->NAME*; ~~Now Playing Duration~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Notes Activity.tsv | RECENT_OBJ | KnowledgeC Notes - Activity | Start->DATETIME_ACCESSED*; ~~End~~; Bundle ID->PATH*; ~~Activity Type~~; ~~User Activity Required String~~; ~~ID~~; Unique ID->PATH*; ~~Activity UUID~~; ~~Source ID~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~Expiration Date~~; ~~UUID~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Screen Orientation.tsv | USER_DEVICE_EVENT | Screen Orientation | Start->DATETIME_START*; End->DATETIME_END*; ~~Orientation~~; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Plugged In.tsv | USER_DEVICE_EVENT | Plugged In | Start->DATETIME_START*; End->DATETIME_END*; ~~Is Plugged In?~~; ~~Usage in Seconds~~; ~~Day of the Week~~; ~~GMT Offset~~; ~~Start~~; ~~End~~; ~~Entry Creation~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Safari Browsing.tsv | WEB_HISTORY | KnowledgeC Safari Browsing | Start->DATETIME_ACCESSED*; ~~End~~; Title->TITLE*; URL->URL*; Bundle ID->PROG_NAME*; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC Siri Usage.tsv | USER_DEVICE_EVENT | Siri Usage | Start->DATETIME_START*; App Name->PROG_NAME*; ~~Weekday~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~ZOBJECT Table ID~~ |
| KnowledgeC App Usage.tsv | PROG_RUN | KnowledgeC App Usage | Start->DATETIME*; ~~End~~; Bundle ID->PROG_NAME*; ~~Usage in Seconds~~; ~~Usage in Minutes~~; ~~Device ID~~; ~~Day of the week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC User Waking Events.tsv | USER_DEVICE_EVENT | User Waking | Start->DATETIME_START*; End->DATETIME_END*; ~~Day of Week~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| KnowledgeC Web Usage.tsv | WEB_HISTORY | KnowledgeC Web Usage | Start->DATETIME_ACCESSED*; ~~End~~; App Name->PROG_NAME*; ~~Usage in Seconds~~; ~~Usage in Minutes~~; Domain->DOMAIN*; URL->URL*; ~~Device ID~~; ~~Day of the Wekk~~; ~~GMT Offset~~; ~~Entry Creation~~; ~~UUID~~; ~~Zobject Table ID~~ |
| LocationD LTE Location.tsv | GPS_LAST_KNOWN_LOCATION | LocationD LTE Location | Timestamp->DATETIME*; ~~Coordinates~~; ~~MCC~~; ~~MNC~~; ~~CI~~; ~~UARFCN~~; ~~PID~~; ~~Altitude~~; ~~Speed~~; ~~Course~~; ~~Confidence~~; ~~Horizontal Accuracy~~; ~~Vertical Accuracy~~; Latitude->GEO_LATITUDE*; Longitude->GEO_LONGITUDE* |
| RoutineD Vehicle Location.tsv | GPS_LAST_KNOWN_LOCATION | RoutineD Vehicle Location | Timestamp->DATETIME*; ~~Location Date~~; ~~Coordinates~~; ~~Vehicle Identifier~~; ~~Location Identifier~~; ~~Identifier~~; ~~Location Quality~~; ~~User Set Location~~; ~~Usual Location~~; ~~Notes~~; ~~Geo Map Item~~; Latitude->GEO_LATITUDE; Longitude->GEO_LONGITUDE*; ~~Table ID~~ |
| RoutineD Parked Vehicle Historical.tsv | GPS_LAST_KNOWN_LOCATION | RoutineD Parked Vehicle Historical | Timestamp->DATETIME*; ~~Location Date~~; ~~Location Uncertainty~~; ~~Identifier~~; Latitude->GEO_LATITUDE*; Longitude->GEO_LONGITUDE* |
| LocationD WiFi Location.tsv | GPS_LAST_KNOWN_LOCATION | LocationD WiFi Location | Timestamp->DATETIME*; ~~Coordinates~~; ~~MAC~~; ~~Channel~~; ~~Infomask~~; ~~Speed~~; ~~Course~~; ~~Confidence~~; ~~Score~~; ~~Reach~~; ~~Horizontal Accuracy~~; ~~Vertical Accuracy~~; Latitude->GEO_LATITUDE*; Longitude->GEO_LONGITUDE* |
| Notifications.tsv | PROG_NOTIFICATIONS | iOS Notificatons | Creation Time->DATETIME*; Bundle->PROG_NAME*; Title[Subtitle]->TITLE*; Message->VALUE*; ~~Other Details~~ |
| Powerlog Agg Bulletins.tsv | PROG_NOTIFICATIONS | Powerlog Aggregate Bulletins | Timestamp->DATETIME*; Bulletin Bundle ID->PROG_NAME*; ~~Time Interval in Seconds~~; ~~Count~~; ~~Post Type~~; ~~Aggregate Table ID~~ |
| Powerlog Agg Notifications.tsv | PROG_NOTIFICATIONS | Powerlog Aggregate Notifications | Timestamp->DATETIME*; Notification Bundle ID->PROG_NAME*; ~~Time Interval in Seconds~~; ~~Count~~; ~~Notification Type~~; ~~Aggregate Table ID~~ |
| Powerlog Backup Info.tsv | BACKUP_EVENT |  | Timestamp->DATETIME*; Start->DATETIME_START*; End->DATETIME_END*; ~~State~~; ~~Finished~~; ~~Has error~~; ~~Table ID~~ |
| Powerlog Deleted Apps.tsv | DELETED_PROG | Powerlog Deleted Apps | App Deleted Date->DATETIME_DELETED*; Timestamp->DATETIME*; App Name->PROG_NAME*; ~~App Executable Name~~; Bundle ID->PATH*; ~~Table ID~~ |
| Powerlog Lightning Connector.tsv | USER_DEVICE_EVENT | Powerlog Lightning Connector Status | Adjusted Timestamp->DATETIME*; ~~Accesory Power Mode~~; ~~Original Lightnint Connector Timestamp~~; ~~Offset Timestamp~~; ~~Table ID~~ |
| Powerlog Push Message Received.tsv | USER_DEVICE_EVENT | Powerlog Push Message Received | Adjusted Timestamp->DATETIME*; Bundle ID->PROG_NAME*; Connection Type->VALUE*; ~~Is Dropped~~; ~~Link Quality~~; ~~Priority~~; ~~Topic~~; ~~Server Hostname~~; ~~Server IP~~; ~~Original Timestamp~~; ~~Offset Timestamp~~; ~~Time Offset~~; ~~Aggregate Table ID~~ |
| Powerlog Torch.tsv | USER_DEVICE_EVENT | Powerlog Torch | Adjusted Timestamp->DATETIME*; ~~Bundle ID~~; ~~Status~~; ~~Original Torch Timestamp~~; ~~Offset Timestamp~~; ~~Time Offset~~; ~~Torch ID~~ |
| Powerlog Wifi Network Connections.tsv | WIFI_NETWORK | Powerlog WiFi Network Connections | Adjusted Timestamp->DATETIME*; Current SSID->SSID*; ~~Current Channel~~; ~~Offset Timestamp~~; ~~Time Offset~~; ~~Cummilative Prop. Table ID~~ |
| Powerlog App Info.tsv | INSTALLED_PROG | Powerlog App Info | Timestamp->DATETIME*; App Name->PROG_NAME*; ~~App Executable Name~~; Bundle ID->PATH*; ~~App Build Version~~; App Bundle Version->VERSION*; ~~App TYpe~~; ~~App Deleted Date~~; ~~Table ID~~ |
| Powerlog Paired Device Conf.tsv | DEVICE_ATTACHED | Powerlog Paired Device Configuration | Timestamp->DATETIME*; Build->DEVICE_ID*; Device->DEVICE_MAKE*; HW Model->DEVICE_MODEL*; ~~Pairing ID~~ |
| Recent WebSearches.tsv | WEB_SEARCH_QUERY |  | Date->DATETIME_ACCESSED*; Search Term->TEXT* |
| Safari Browser - History.tsv | WEB_HISTORY |  | Visit Timestamp->DATETIME_ACCESSED; Title->TITLE; URL->URL*; ~~Visit Count~~; Redirect Source->REFERRER; ~~Redirect Destination~~; ~~Visit ID~~; ~~Origin~~ |
| Safari Web Search.tsv | WEB_SEARCH_QUERY |  | Visit Time->DATETIME_ACCESSED*; Search Term->TEXT*; ~~URL~~; ~~Visit Count~~; ~~Title~~; ~~iCloud Sync~~; ~~Load Successful~~; ~~Visit ID~~; ~~Redirect Source~~; ~~Redirect Destination~~; ~~History Item ID~~ |
| SMS & iMessage - Messages.tsv | MESSAGE | SMS - iMessage | Message Timestamp->DATETIME*; ~~Read Timestamp~~; Message->TEXT*; ~~Service~~; ~~Message Direction~~; ~~Message Sent~~; ~~Message Delivered~~; ~~Message Read~~; ~~Account~~; ~~Account Login~~; ~~Chat~~; ~~Contact ID~~; ~~Attachment Name~~; ~~Attachment Path~~; ~~Attachment Timestamp~~; ~~Attachment Mimetype~~; ~~Attachment Size (Bytes)~~; ~~Message Row ID~~; Chat ID->THREAD_ID; ~~From Me~~ |
| Wifi.tsv | WIFI_NETWORK | Wifi | SSID->SSID*; BSSID->MAC_ADDRESS*; ~~Network usage~~; ~~Country code~~; Device name->DEVICE_ID*; ~~Manufacturer~~; ~~Serial number~~; Model name->DEVICE_MODEL; Last joined->DATETIME*; ~~Last autojoined~~; ~~Enabled~~ |
| Wifi Known Networks.tsv | WIFI_NETWORK | Wifi | SSID->SSID*; BSSID->MAC_ADDRESS*; ~~Network usage~~; ~~Country code~~; Device name->DEVICE_ID*; ~~Manufacturer~~; ~~Serial number~~; Model name->DEVICE_MODEL; Last joined->DATETIME*; ~~Last autojoined~~; ~~Enabled~~ |
| Wifi Network Store Model - Networks.tsv | WIFI_NETWORK | Wifi | Last Connected Timestamp->DATETIME; ~~PK~~; SSID->SSID*; Latitude->GEO_LATITUDE; Longitude->GEO_LONGITUDE; BSSID->MAC_ADDRESS; ~~5 GHz Network~~; ~~2.4 GHz Network~~ |

The disabled iLEAPP entries are:
- `Account Data.tsv` (TSK_ACCOUNT).
- `iOS Mail.tsv` (TSK_EMAIL_MSG). It is disabled because the multi-line `TSK_EMAIL_CONTENT_PLAIN` breaks line-based TSV reading.

Note: `SMS & iMessage - Messages.tsv` is **not** in `ACCOUNT_RELATIONSHIPS`; only `sms - imessage.tsv` is. It therefore becomes a plain TSK_MESSAGE with no account or relationship.

#### Edge cases and quirks to preserve or fix
- The temp extraction tree is never deleted.
- Every LEAPP run writes a new timestamped output directory. Re-running ingest therefore duplicates artifacts; there is no dedupe.
- Glob-to-LIKE conversion is approximate: `findFiles` uses a parent-path LIKE `%path%`, so it over-matches.
- Errors in relationship creation throw an `IngestModuleException`, which aborts that TSV file only. Remaining files continue.
- Cancellation is checked between files and rows (`dataSourceIngestIsCancelled`). The external process is killed by the terminator.

#### Rust port notes
- **External tool:** aLEAPP/iLEAPP are Python (PyInstaller exe on Windows). No Rust port exists.
- **Option A (parity, fast to build):** support the LEAPP CLI as an optional cross-platform external tool (`aleapp.py`/`ileapp.py` or a user-supplied binary), extract the requested paths with a streaming exporter, and keep this exact XML/TSV mapping engine. Notes for that engine:
  - Use the `csv` crate with `delimiter(b'\t')` and `flexible(true)`.
  - Use `quick-xml` + serde for the mapping file.
  - Use `chrono` for dates, interpreting them as UTC.
- **Option B (long term):** native parsers for the high-value artifacts. Inputs are mostly SQLite (`rusqlite`), binary plist (`plist` crate), protobuf (`prost`), Android ABX XML (custom, small) and LevelDB (`rusty-leveldb`). LEAPP's own TSV column names make a good regression oracle.
- Fix the start/end, recipient and time-zone bugs listed above, keeping a compatibility flag only if needed for result-diffing.

---

### Plaso (`modules/plaso`)

- **Factory:** `PlasoModuleFactory` is data-source-level only and has per-job settings (`PlasoModuleSettings`, `serialVersionUID 1`). It is **Windows-only**: `startUp` throws on other platforms and needs both `Log2timeline.exe` and `psort.exe`.
- **Bundled version:** `plaso/plaso-20180818-amd64/` on 64-bit, otherwise `plaso/plaso-20180818-win32/`.
- **Settings:** `Map<String parserName, Boolean enabled>`. The defaults are all **false** (disabled):
  - `winreg`, `pe`
  - `chrome_preferences`, `chrome_cache`, `chrome_27_history`, `chrome_8_history`, `chrome_cookies`, `chrome_extension_activity`
  - `firefox_cache`, `firefox_cache2`, `firefox_cookies`, `firefox_downloads`, `firefox_history`
  - `msiecf`, `msie_webcache`

  The UI exposes only two checkboxes: `winreg` and `pe`. The browser parsers are disabled because they duplicate Autopsy's Recent Activity module. The panel text reads "All modules except chrome_cache* and the below are enabled. Enabling these will cause Plaso to run slower."
- **Input:** only `Image` data sources are processed. Any other data source gets an inbox DATA message "Skipping non-disk image datasource" and the module returns OK. The tool is given **`image.getPaths()[0]`**, the first segment of the original image path on disk. Plaso reads the image itself, not via TSK.

**Algorithm:**
1. Create `ModuleOutput/plaso/<yyyy-MM-dd HH-mm-ss Z>/`. Progress is determinate over 0..100.
2. Run log2timeline:
   ```
   "Log2timeline.exe" --vss-stores all -z <image.getTimeZone()> --partitions all
     --hasher_file_size_limit 1 --hashers none --parsers "!p1,!p2,..."   (every disabled parser prefixed with '!')
     --no_dependencies_check --workers 2 <out>/plaso <imagePath>
   ```
   - Stderr goes to `log2timeline_err.txt`.
   - Stdout is read by a background thread (`L2TStatusProcessor`). Each line is pushed to the progress text and copied to `log2timeline_output.txt`.
   - The wait uses `ExecUtil.waitForTermination(..., 5 s, DataSourceIngestModuleProcessTerminator(context))`: cancellation only, no global timeout.
   - If ingest was cancelled, return OK. If `<out>/plaso` (the storage file) does not exist, return ERROR.
3. Progress 33: run `psort.exe -o 4n6time_sqlite -w <out>/plasodb.db3 <out>/plaso` (stdout to `psort_output.txt`, stderr to `psort_err.txt`).
   - A non-zero exit gives a notification "Plaso returned an error when sorting events. Results are not complete." and the module returns ERROR.
   - If `plasodb.db3` is missing, return ERROR.
4. Progress 66: read the 4n6time SQLite database:
   ```sql
   SELECT substr(filename,1) AS filename, strftime('%s', datetime) AS epoch_date, description, source, type, sourcetype
   FROM log2timeline
   WHERE source NOT IN ('FILE','WEBHIST') AND sourcetype NOT IN ('UNKNOWN','PE Import Time');
   ```
   - Each row's `filename` (a Windows-style path) is resolved to an `AbstractFile` via `findFiles(name, parentPath)`, with an exact `parentPath` equality check. A one-entry cache holds the previous file. If unresolved, the event attaches to the **Image**.
   - **Event type mapping** (`findEventSubtype`):
     - `WEBHIST` maps to `WEB_COOKIE` if the file name or `type` contains "cookie", otherwise `WEB_HISTORY`. This branch is unreachable because of the WHERE filter.
     - `EVT`/`LOG` map to `LOG_ENTRY`.
     - `REG` maps to `DEVICES_ATTACHED` if `sourcetype` is "unknown : usb entries" or "unknown : usbstor entries", otherwise `REGISTRY`.
     - Anything else maps to `STANDARD_ARTIFACT_CATCH_ALL`.
   - An empty description is replaced by the event type's display name. If the type is CATCH_ALL, the row is **skipped** instead.
   - Each row creates **TSK_TL_EVENT** (a data artifact) with these attributes:
     - `TSK_DATETIME` = epoch seconds
     - `TSK_DESCRIPTION`
     - `TSK_TL_EVENT_TYPE` = `TimelineEventType.getTypeID()` (long)

     Each artifact is posted **individually** via `blackboard.postArtifact`, which is slow.
   - An empty database gives the notification "Plaso database was empty."
5. Post an inbox DATA message "Plaso Processing Completed".

**Rust port notes:**
- No Rust Plaso exists.
- `4n6time_sqlite` output was removed from modern Plaso. A port that keeps Plaso as an optional external tool should invoke `log2timeline.py --storage-file X.plaso` and `psort.py -o json_line -w out.jsonl`, then stream-parse with serde. The filename, timestamp, `data_type`/`parser` → event-type mapping should be updated (e.g. `parser` starts with `winevt` → LOG_ENTRY, `winreg` → REGISTRY, usb → DEVICES_ATTACHED).
- **Native alternative** for timeline coverage, with crates:
  - `evtx` (EVTX, pure Rust)
  - `notatin` or `nt-hive` (registry)
  - `frnsc-prefetch` / custom (prefetch)
  - `lnk` crate (LNK)
  - custom $UsnJrnl/$LogFile parsers
- Batch inserts (see the perf section).
- Pass image paths as all segments, not `[0]`.

---

### PhotoRec Carver (`modules/photoreccarver`)

- **Factory:** `PhotoRecCarverIngestModuleFactory` ("PhotoRec Carver") is a **file-level** module with per-job settings and a settings panel.
- **Binary:**
  - Windows: `photorec_exec/bin/photorec_win.exe` (TestDisk/PhotoRec, GPL, bundled with cygwin DLLs and libewf).
  - Elsewhere: the first `photorec` found on `$PATH`.
  - A missing binary gives "Unable to locate PhotoRec executable."; a non-executable one gives "Unable to execute PhotoRec."

**Settings** (`PhotoRecCarverIngestJobSettings`):
| field | default | meaning |
|---|---|---|
| `keepCorruptedFiles` | `false` | adds `options,keep_corrupted_file` to the cmd string |
| `extensionFilterOption` | `NO_FILTER` | `INCLUDE` / `EXCLUDE` / `NO_FILTER` |
| `extensions` | `[]` | UI: comma-separated, trimmed, empty dropped, sorted case-insensitively |

**startUp validation:**
- INCLUDE with an empty list raises "No extensions provided for PhotoRec to carve."
- Any extension not in `PhotoRecCarverFileOptExtensions.KNOWN_EXTENSIONS` (about 330 PhotoRec "fileopt" names such as `jpg`, `png`, `zip`, `doc`, `pdf`, `sqlite`, `evtx`, `pst`, `mov`, `mkv`, `e01`, `luks`, `kdbx`, …; generated from the PhotoRec UI) raises "The following extensions are invalid: …". The check is case-sensitive.
- If `context.processingUnallocatedSpace()` is false (the file filter skips unallocated space), it throws "The selected file ingest filter ignores unallocated space…".

**Option string** (the single last CLI argument, comma-joined): `[options,keep_corrupted_file,][fileopt,everything,<enable|disable>,<ext>,<enable|disable>,...,]search`.
- INCLUDE gives `everything,disable` followed by each `ext,enable`.
- EXCLUDE gives `everything,enable` followed by each `ext,disable`.

**Per-job shared state:**
- `IngestModuleReferenceCounter` is keyed by job ID. The first instance creates `ModuleOutput/PhotoRec Carver/<dsObjId>_<MM-dd-yyyy-HH-mm-ss-SSSS>/` and `Temp/PhotoRec Carver/<same>/`. The space in the name is required, per JIRA-6878.
- UNC output paths with an IP host are converted to a host name. If that fails: "PhotoRec cannot operate with a UNC path containing IP addresses." A read/write access check is also made.
- Static totals: items recovered, items with errors, write time and parse time.

**process(file):**
1. Skip unless `file.getType() == UNALLOC_BLOCKS`. These are the `Unalloc_<objid>_<start>_<end>` layout files TSK creates per volume or FS.
2. Disk-space check: if `freeSpace != UNKNOWN` and `size*1.2 > freeSpace`, raise an error notification ("Not enough disk space to save unallocated file. Carving will be skipped.") and return ERROR.
3. Copy the unallocated file to `Temp/.../<file.getName()>`. This is a full copy, which is a big cost.
4. Create `Output/.../<file.getName()>/`, with log `run_log.txt` (stdout and stderr appended).
5. Run `photorec_win.exe /d <outdir>/results /cmd <tempFile> <optionsString>`:
   - Terminator: `FileIngestModuleProcessTerminator(context, true)` (cancellation plus global timeout).
   - On cancellation: clean up and return OK.
   - On `TIME_OUT`: clean up, notify ("...terminated due to exceeding max allowable run time when scanning <name>") and return ERROR.
   - On a non-zero exit: clean up, increment the error counter and return ERROR.
6. PhotoRec writes into `results.1/`. Move `results.1/report.xml` to `<outdir>/report.xml`, then **delete every subdirectory**, which removes the carved file data. Only byte ranges are kept; the carved files are virtual.
7. Parse with `PhotoRecCarverOutputParser`, a DFXML reader:
   - For each `<fileobject>`, read `filename` (the basename is taken if it is under the output path), `filesize`, and each `<byte_run img_offset="" len="">`. Here `img_offset` is relative to the unallocated file.
   - Convert each range via `af.convertToImgRanges(offset, len)`. This maps a range inside a multi-range unallocated layout file to absolute image ranges and may split one range into several. Each resulting range is added as a `TskFileRange(start, len, seq)`.
   - Files with no valid ranges are dropped.
   - Insert all of them with `FileManager.addCarvedFiles(new CarvingResult(af, carvedFiles))`. This creates **LayoutFiles** of type `CARVED` under the `$CarvedFiles` virtual directory of the parent FS or volume (subfolders are created per batch of about 2000 files).
8. Call `context.addFilesToJob(carved)`, which **re-queues the carved files** through the file pipeline. Then fire a `ModuleContentEvent` for each newly created virtual parent directory, walking up until a non-virtual directory or an already-seen ID, plus one for the first carved file.
9. **shutDown (last instance of the job):** delete the temp directory and post an inbox INFO message "PhotoRec Results". Its HTML table holds: number carved, number of errors, total write time (ms) and total parse time (ms).

**Artifacts:** none. The module only creates files (LayoutFile, `TSK_DB_FILES_TYPE_ENUM.CARVED`).

**Rust port notes:**
- No pure-Rust PhotoRec equivalent exists (PhotoRec's 300+ format validators are C, GPL).
- **Option 1:** keep `photorec` as an optional external tool on all OSes. Feed it a FIFO or loop file instead of a full copy where possible: PhotoRec needs a seekable file, so a sparse temp file or a FUSE/virtual block device are alternatives.
- **Option 2:** a native carver. Use `aho-corasick`/`memchr` for multi-pattern header search over mmapped or streamed unallocated runs (see Scalpel below), plus per-format structure validators/length finders for the top formats: JPEG (SOI/EOI with marker walk), PNG (chunk walk to IEND), GIF, PDF (`%%EOF`), ZIP/OOXML (EOCD), SQLite (header page size × page count), EVTX chunks, MP4/MOV (box walk), RIFF (size field), BMP, OLE2 (sector math).
- Emit **virtual carved files as byte-range lists** (no data copy), exactly like Autopsy's LayoutFiles.
- Keep the DFXML `report.xml` parsing for the external-tool mode. The `quick-xml` streaming reader suits it, and the reports can be very large.

---

### Scalpel Carver (`ScalpelCarver/`, **disabled**)

- **Status:** the factory's `@ServiceProvider` annotation is commented out, so the module is **not registered** in shipping Autopsy. It is a file-level module with no settings.
- **Platform:** Windows-only, using JNI `libscalpel_jni` plus `libtre-4` and `pthreadGC2`.
- **Latent bug:** `startUp` checks `carver.isInitialized()` before `ScalpelCarver.init()` is ever called; `init()` only happens in `process()`. Unless another instance has already initialised the library, `startUp` always fails.
- **Config:** `scalpel.conf` is copied (without overwrite) to `<userConfigDir>/scalpel.conf`. Standard Scalpel format: `ext  case_sensitive(y/n)  max_size  header  [footer]  [REVERSE|NEXT]`, using `\xNN` escapes and `?` wildcards. The active rules are:

  | ext | header → footer | max size |
  |---|---|---|
  | gif | `GIF87a` → `\x00\x3b`; `GIF89a` → `\x00\x00\x3b` | 5 MB |
  | jpg | `FF D8 FF E0 00 10` → `FF D9` | 200 MB |
  | png | `\x50\x4e\x47?` → `FF FC FD FE` | 20 MB |
  | tif | `II*\0`, `MM\0*` (no footer) | 200 MB |
  | avi | `RIFF????AVI` | 50 MB |
  | mpg | `00 00 01 BA` → `00 00 01 B9`; `00 00 01 B3` → `00 00 01 B7` | 50 MB |
  | fws | `FWS` | 4 MB |
  | wav | `RIFF????WAVE` | 200 KB |
  | doc | OLE2 `D0 CF 11 E0 A1 B1 1A E1 00 00` → same, `NEXT`; and the 6-byte OLE2 prefix | 10 MB |
  | pst | `21 42 4E A5 6F B5 A6` | 500 MB |
  | ost | `21 42 44 4E` | 500 MB |
  | dbx | `CF AD 12 FE C5 FD 74 6F` | 10 MB |
  | idx | `JMF9` | 10 MB |
  | mbx | `JMF6` | 10 MB |
  | htm | `<html` → `</html>` (case-insensitive) | 50 KB |
  | pdf | `%PDF` → `%EOF\r` / `%EOF\n`, `REVERSE` | 5 MB |
  | zip | `PK\x03\x04` → `\x3c\xac` | 10 MB |
  | java | `CA FE BA BE` | 1 MB |
  | tgz | `1F 8B 08 08` | 2 MB |
  | ogg | `OggS\0\x02` → same, `NEXT` (the `x4f...` escaping in the file is broken: it lacks `\`) | 15 MB |

  About 100 more rules are present but commented out.
- **Algorithm:**
  - Only `UNALLOC_BLOCKS` files are processed.
  - The output directory is `ModuleOutput/ScalpelCarver/<fileObjId>/`.
  - The module walks up parents to find the containing FileSystem, Volume or Image ID.
  - It calls the native `carveNat(id, ReadContentInputStream, conf, outDir)`, which streams the data with no full copy.
  - It then parses `audit.txt`:
    - Skip to the localised "start" line, then skip the header line.
    - Each subsequent non-empty line is split on whitespace into `name start chop length` (fields 0, 1, 3).
  - Offsets are converted with `convertToImgOffset(start)`, which assumes **one contiguous range**. This is wrong for fragmented unallocated layout files; PhotoRec uses `convertToImgRanges`.
  - Results are added with `db.addCarvedFile(name, size, parentId, [range])`, followed by a ModuleContentEvent on the parent and `addFilesToJob`.
- **Errors:** they are swallowed; the module returns OK. There is no cancellation check during carving.
- **Rust:** reimplement natively; this is the easiest carver to build.
  - Parse the conf format (keeping compatibility for users' own rule files).
  - Compile all headers into one `aho-corasick` automaton. The `?` wildcard needs an expansion or a post-verify step: match on the longest literal prefix, then verify.
  - For each header hit, search for the footer (FORWARD/REVERSE/NEXT semantics) within `max_size` using `memchr::memmem`.
  - Emit byte-range carved files. Run over mmapped or chunked data with overlap equal to the longest header.

---

### Virtual Machine Extractor (`modules/vmextractor`)

- **Factory:** `VMExtractorIngestModuleFactory` ("Virtual Machine Extractor": "Extracts virtual machine files and adds them to a case as data sources.") is data-source-level with no settings.
- **startUp:**
  - Reads the parent data source's `deviceId` and `timeZone`; they are reused for the child data sources.
  - Creates `ModuleOutput/Virtual Machine Extractor/<dsName>_<dsId>_<yyyy_MM_dd_HH_mm_ss>/`, with `:` replaced by `_`.
- **Finding candidates:** for each extension in `GeneralFilter.VIRTUAL_MACHINE_EXTS = [.vmdk, .vhd, .vhdx]`, run `findFiles(ds, "%<ext>")`.
  - For `.vhd`: if the MIME type is null, run `FileTypeDetector` and **persist** the MIME type to the DB (`setMIMEType` + `save`). The file is kept only if the MIME type is `application/x-vhd`. If the detector fails to initialise, the file is kept.
  - `.vmdk` and `.vhdx` files are kept without a check.
- **Export:**
  - Each VM file is written with `ContentUtils.writeToFile` to `<jobOut>/<N>/<name>`.
  - `N` is a counter per distinct **parent path inside the image**. This keeps descriptor and extent files together.
  - There is no disk-space check (a TODO in the code).
  - Read errors give a notification "Failed to extract virtual machine file" and processing continues.
  - Progress is determinate over the files.
- **Identify the top-level images per output folder** (`VirtualMachineFinder.identifyVirtualMachines`):
  - List regular files with VM extensions.
  - Any `.vmdk` **smaller than 10 000 bytes** is treated as a text descriptor. In it, lines are split on a single space, and lines with at least 4 fields whose first field is `RW`, `RDONLY` or `NOACCESS` are extent lines. Field 3, with quotes stripped, is an extent file name. Those names are removed from the candidate list.
  - **Bug:** `continue` on a short line skips `readLine()`, which gives an **infinite loop** on any descriptor line with fewer than 4 space-separated fields. In practice such descriptor files hit this immediately (blank lines, `# Disk DescriptorFile`). Do not replicate.
  - Extent names containing spaces are split incorrectly.
- **Ingest:**
  - For each remaining file, run `ImageDSProcessor.run(parentDeviceId, path, parentTimeZone, ignoreFatOrphans=false, ...)` synchronously (wait/notify).
  - On success, call `Case.notifyDataSourceAdded`, build `new IngestJobSettings(context.getExecutionContext())` (the **same profile/context** as the parent job), post an inbox INFO "Added virtual machine image <path>", and call `IngestManager.beginIngestJob([newDs], settings)`. This starts a separate, **recursive** ingest job.
  - On failure, call `notifyFailedAddingDataSource`.
- **Artifacts:** none. The result is new data sources (Image objects backed by local copies under ModuleOutput).
- **Rust port notes:**
  - Avoid the copy entirely. Postmortem's image layer should open nested images through a `Read + Seek` adapter over an in-case file (an `AbstractFile` reader).
  - VMDK (sparse/flat/streamOptimized), VHD (fixed/dynamic/differencing) and VHDX readers are needed; implement them natively (small formats). Crates such as `vhdx` or `vmdk` exist but are immature, so plan to own the code. libyal (`libvmdk`/`libvhdi`) is the reference.
  - Multi-extent VMDK requires resolving extents relative to the descriptor's directory inside the parent image.
  - Add depth limiting to stop recursion (a VM inside a VM).
  - Parse descriptors with a real tokenizer that handles quoted names with spaces.

---

### DJI Drone Analyzer (`modules/drones`)

- **Factory:** `DroneIngestModuleFactory` ("DJI Drone Analyzer": "Analyzes files generated by some DJI drones.") is data-source-level with no settings and is cross-platform (pure Java).
- **Module:** `DroneIngestModule` runs a list of `DroneExtractor`s. Currently there is only `DATExtractor` ("DAT File Extractor").
  - It posts an inbox INFO "Started <dsName>".
  - Extractor exceptions are logged and the module still returns OK.
- **Inputs:** `findFiles(ds, "FLY___.DAT")`. In SQL LIKE, `_` matches any single character, so this matches `FLY` followed by exactly 3 characters and `.DAT`. Case sensitivity depends on the DB collation; DJI's on-aircraft flight logs are `FLY001.DAT` etc.
- **Algorithm per file:**
  1. Copy it to `Temp/DroneExtractor/org.sleuthkit.autopsy.modules.drones.DATExtractor/<name><objId><ext>`.
  2. `DATDumper.isDATFile`: `DatFile.isDatFile(path) || DJIAssistantFile.isDJIDat(file)` from the bundled **DatCon.jar** (a third-party Java library, `src.Files.*`). A non-DAT file is logged and skipped.
  3. `dumpDATFile(dat, csv, overwrite=true)`:
     - `DatFile.createDatFile` builds a version-specific parser.
     - `reset`, then `preAnalyze()`, which finds `gpsLockTick` and `lastMotorStopTick`.
     - `ConvertDat` with `sampleRate=1`. `tickRangeLower=gpsLockTick` and `tickRangeUpper=lastMotorStopTick` when they are not -1; this trims noise before GPS lock and after motor stop.
     - Then `createRecordParsers()`, `reset`, `analyze(true)`, writing the CSV.
  4. Read the CSV: the header is split on `,`, giving a name-to-index map. Columns used:
     - `IMU_ATTI(0):Latitude`, `IMU_ATTI(0):Longitude`: both required, otherwise the row is skipped.
     - `GPS(0):heightMSL` gives altitude.
     - `IMU_ATTI(0):velComposite` gives velocity.
     - `IMU_ATTI(0):distanceHP` gives distance from home point.
     - `IMU_ATTI(0):distanceTravelled` gives distance traveled.
     - `GPS:dateTimeStamp` is parsed as ISO date-time to epoch seconds, as UTC from the local date-time.

     Unparseable numbers become null. The index guard uses `index > values.length` (an off-by-one; it should be `>=`).
  5. If any points were found, call `GeoArtifactsHelper(case, "DAT File Extractor", programName="DatCon", DATFile, jobId).addTrack(DATFile.getName(), points, null)`. This creates **TSK_GPS_TRACK** (a data artifact) on the DAT file, with these attributes:
     - `TSK_NAME` = file name
     - `TSK_GEO_TRACKPOINTS` = JSON list of `{latitude, longitude, altitude, name=null, velocity, distanceFromHomePoint, distanceTraveled, timestamp}`
     - `TSK_PROG_NAME` = "DatCon"

     The helper posts the artifact. Note: these are GPS_TRACK artifacts, not per-point TSK_GPS_TRACKPOINT.
  6. Delete the temp DAT and CSV. Finally, delete the extractor temp directory.
- **Rust port notes:**
  - No Rust or C DJI DAT parser exists, so it must be implemented.
  - DatCon (open source, Java, by Bud Walker) is the reference for:
    - the record framing: 0x55 start byte, length, type ID, tick number, CRC;
    - per-firmware record payload XOR "encryption" keyed by tick number and record type;
    - the record layouts for IMU_ATTI, GPS, motor and so on.
  - Also support the `DJIAssistantFile` container variant.
  - Skip the CSV hop: decode records straight into track points.
  - The format is moderately complex. Scope only the fields listed above for parity.

---

### YARA Analyzer (`modules/yara`)

- **Factory:** `YaraIngestModuleFactory` is a **file-level** module with per-job settings and a global options panel ("Yara Rule Sets"). It is **Windows 64-bit only**: `startUp` throws "The YARA ingest module is only available on 64bit Windows."
- **Rule-set storage** (`RuleSetManager`, a singleton):
  - Each rule set is a directory `<userDir>/yara/ruleSets/<setName>/`, created on demand. Every file inside that directory is a rule source file, with no extension filter.
  - The UI offers New Set, Delete Set (recursive delete), Open Folder and Refresh File List. Panel text: "Place rule files in the set's folder. They will be compiled before use." and "A file need only match one rule in the set to be found."
  - Changes fire the property-change events `YARARuleSetAdded` / `YARARuleSetDeleted` on the EDT.
- **Job settings** (`YaraIngestJobSettings`, `serialVersionUID 1`):
  - `selectedRuleSetNames: List<String>`, default empty.
  - `onlyExecutableFiles: boolean`, default **true**. The UI radio buttons are "All Files" / "Only Executable Files".
- **startUp (first instance per job, via the reference counter):**
  - Create `Temp/yara/<8 random letters>_<jobId>/RuleSets/`.
  - If any sets are selected, compile **each rule file separately** by running the external **`yara/yarac64.exe "<rule>" "<RuleSets>/<set>/compiled_<rulefile>"`** (via `ExecUtil.execute`, with the default 30-day limit).
  - A non-zero exit throws: the job fails to start on any rule syntax error.
  - If no sets are selected, log INFO and do nothing.
- **process(file):**
  1. With no sets selected, return OK.
  2. If `onlyExecutableFiles`, skip unless `file.getNameExtension().equals("exe")`. The comparison is case-sensitive and checks the extension only, not the MIME type. `.EXE`, `.dll` and `.sys` are skipped.
  3. Skip zero-size files, directories and `UNALLOC_BLOCKS`.
  4. Files under 100 MiB (`FILE_SIZE_THRESHOLD_MB = 100`, although the comment says 15 MB) are read fully into a `byte[]`. Each rule set directory, then each compiled file in it, is passed to the JNI call `YaraJNIWrapper.findRuleMatch(compiledPath, bytes, len, timeout)`.
  5. Files of 100 MiB or more are first copied to `Temp/yara/<job>/<15 random letters><objId>.temp`, then `findRuleMatchFile(compiledPath, localPath, timeout)` runs, then the copy is deleted.
  6. The timeout constant is `30*60*60` = **108 000 s (30 hours)**; the comment says 30 minutes.
  7. Results: one artifact per matched rule identifier per set.
     - Type: `newAnalysisResult(TSK_YARA_HIT, Score.SCORE_NOTABLE, conclusion=null, configuration=<setName>, justification=<ruleName>, attrs)`.
     - Attributes: `TSK_SET_NAME`=set name and `TSK_RULE`=rule identifier.
     - The artifacts are posted in a batch per file via `postArtifacts`.
- **JNI bridge** (`thirdparty/yara/yarabridge/YaraJNIWrapper.cpp`, libyara 4.x):
  - `yr_initialize` runs once.
  - For **every scan call**, the bridge calls `yr_rules_load(compiled)`, then `yr_rules_scan_mem` / `yr_rules_scan_file` with flags 0 and the timeout, then `yr_rules_destroy`. Compiled rules are reloaded **for every file × every rule file**, which is a big performance waste.
  - The callback collects `rule->identifier` on `CALLBACK_MSG_RULE_MATCHING`. Tags, metadata, namespaces and string offsets are discarded.
  - A scan timeout raises a `YaraWrapperException` ("Yara file scan timed out"), and the file gets ERROR.
- **shutDown (last instance):** deletes the job temp directory, but with `File.delete()`, which fails on a non-empty directory; the directory leaks.
- **Rust port notes:**
  - Use **`yara-x`** (VirusTotal's pure-Rust YARA, highly compatible with YARA 4 syntax). A few legacy modules or features differ, so surface compile warnings.
  - Compile each rule set once at job start into `yara_x::Rules`, with all files of a set in one compiler and the set name as namespace. Share the rules via `Arc` across worker threads, with one `Scanner` per thread and `scanner.set_timeout(..)`.
  - Scan with `scanner.scan(&mmap_or_bytes)`. For huge files, scan a memory map of the content when the file is contiguous, or a streamed buffer; this avoids the temp copy.
  - Keep the matched rule identifier. Optionally also persist the namespace, tags and metadata, which are an improvement.
  - Fix the executable filter: use a case-insensitive extension check, or better the MIME types `application/x-dosexec`, `application/x-executable`, `application/x-mach-binary`, `application/x-sharedlib`, gated behind a setting.
  - Errors in user rules should be reported in the settings UI before ingest. Do not fail the job; skip the bad set with a warning.

---

### Object Detection (Experimental: `experimental/objectdetection`)

- **Factory:** `ObjectDetectionModuleFactory` ("Object Detection": "Use object classifiers to identify objects in pictures.") is file-level with no settings. It ships in the Experimental NBM, so it is enabled only if that plugin is active.
- **startUp:**
  - Windows-only: otherwise it throws "This module is only available on Windows."
  - Requires `OpenCvLoader.openCvIsLoaded()`, which is OpenCV 2.4.x via JNI from `thirdparty/opencv`.
  - Loads **every `*.xml` file** in `<userDir>/object_detection_classifiers/` (`PlatformUtil.getObjectDetectionClassifierPath()`) as an OpenCV `CascadeClassifier` (Haar/LBP cascades). The map is keyed by file name.
  - A missing directory throws. If the directory has no classifiers, the first instance for the job posts the inbox WARNING "No classifiers found." / "No classifiers were found in {dir}, object detection will not be executed."
- **process(file):**
  - Proceeds only if there are classifiers and `ImageUtils.isImageThumbnailSupported(file)`, which is a MIME/extension check for supported images.
  - Skips files larger than 100 000 000 bytes, with an INFO log.
  - Reads the whole file into memory and decodes it with `Highgui.imdecode(bytes, IMREAD_GRAYSCALE)`. A decode failure returns ERROR.
  - For each classifier, runs `detectMultiScale(image, rects)` with default parameters (scaleFactor 1.1, minNeighbors 3, no size limits).
  - When any rects are found, creates `newAnalysisResult(TSK_OBJECT_DETECTED, Score.SCORE_UNKNOWN, null, null, null, attrs)` with:
    - `TSK_DESCRIPTION` = classifier file name
    - `TSK_COMMENT` = "Classifier detected N object(s)", where N = `rects.size().height`

    The result is posted with `postArtifact`, one per classifier per file.
  - Bug: the same `MatOfRect` is reused across classifiers, but `detectMultiScale` overwrites it, so this is fine.
- **Rust port notes:**
  - The `opencv` crate is bindings to C++ OpenCV, which is not pure Rust.
  - **Pure-Rust options:**
    - (a) Implement a Viola-Jones Haar/LBP cascade evaluator. The OpenCV XML cascade format, integral images and sliding windows are about 600 lines of Rust; decode images with the `image` crate.
    - (b) Run ONNX detection models with `tract-onnx` (pure Rust) for modern detectors such as YOLO or face detection.
    - (c) `rustface` (SeetaFace) for faces only.
  - Keep the "classifier directory" plugin contract: drop XML cascades in a folder. Optionally also accept `.onnx` files.

---

### Volatility Memory Image DSP (Experimental: `experimental/volatilityDSP`)

This is a **DataSourceProcessor**, not an ingest module ("Memory Image File (Volatility)"). It is part of the Experimental NBM.

**Input panel** (`MemoryDSInputPanel`):
- **Memory image path:** a single file.
- **Time zone:** defaults to the local time zone.
- **Profile:** a combo box. The default is "Auto Detect", which means empty. The fixed list holds Volatility 2.6 Windows profiles: VistaSP0–2 (x86/x64), Win10x64 (+_10586/_14393), Win10x86 (+_10586/_14393), Win2003SP0–2, Win2008R2SP0/SP1 (+_23418), Win2008SP1/SP2, Win2012/R2 (+_18340), Win2016x64_14393, Win7SP0/SP1 (+_23418 variants), Win8/8.1 variants, WinXPSP1x64/SP2/SP3.
- **Plugin checklist:**
  - `amcache, apihooks, cmdline, cmdscan, connscan, consoles, devicetree, dlldump, dlllist, dumpfiles, dumpregistry, envars, filescan, gahti, getservicesids, getsids, handles, hashdump, hivelist, hivescan, impscan, ldrmodules, lsadump, malfind, moddump, modules, mutantscan, netscan, notepad, privs, procdump, pslist, psscan, pstree, psxview, shellbags, shimcache, shutdown, sockets, svcscan, timeliner, unloadedmodules, userassist, userhandles, vadinfo, verinfo`. `shimcache` is listed twice in the source array.
  - Selections persist in `ModuleSettings` under the context name. If no config exists yet, **all plugins are enabled** by default.

**AddMemoryImageTask** (background thread):
1. Call `SleuthkitJNI.addImageToDatabase(caseDb, [path], sectorSize 0, tz, md5 null, sha1 null, sha256 null, deviceId = random UUID)`. This adds the dump as a raw **Image** data source; TSK normally finds no file system in it.
2. Run `VolatilityProcessor`. Critical exceptions give `CRITICAL_ERRORS`; collected non-fatal messages give `NONCRITICAL_ERRORS`.
3. Call `callback.done(result, errors, [image])`. The normal add-data-source flow then offers ingest. Note: a crash before `volatilityProcessor` is assigned causes an NPE on `getErrorMessages()`.

**VolatilityProcessor:**
- **Binary:** `Volatility/volatility_2.6_win64_standalone.exe` (Windows only; otherwise it fails with "Volatility executable not found").
- **Virtual directory:** `addVirtualDirectory(image.id, "ModuleOutput")` holds the reports. The output directory is `ModuleOutput/Volatility/<dsObjId>/`.
- **Profile auto-detection:** if the profile is empty, run the `imageinfo` plugin first. The profile is the first line of `imageinfo.txt`: take the text after `:`, split on `,` or `(`, take the first token and strip whitespace. This is the first of the "Suggested Profile(s)" entries.
- **Per plugin**, progress is determinate over the plugins.
  - Command: `"vol.exe" --filename=<imageBaseName> [--profile=<p>] <plugin> [--dump-dir=<out>/<plugin>]`.
    - The cwd is the image's parent directory.
    - `--dump-dir` is added for `dlldump`, `moddump`, `procdump`, `dumpregistry` and `dumpfiles`.
    - Stdout goes to `<out>/<plugin>.txt` and stderr to `<out>/Volatility_err.txt`, which is overwritten by each plugin.
    - The run uses `ExecUtil.execute`, with no cancellation terminator. A `cancel()` flag is checked between plugins only.
  - A non-zero exit adds the non-fatal "Volatility error running X plugin" and skips the rest of that plugin.
  - The text output is added as a **DerivedFile** named `<plugin>` under the `ModuleOutput` virtual directory. Its local path is relative to the case directory, and the file is indexable and visible in the tree. **Files in dump directories are not added to the case.**
  - **Parsing:** fixed-column text parsers extract path sets for 9 plugins:

    | plugin | rule |
    |---|---|
    | `dlllist` | lines starting `0x` with length > 33 → `substring(33)` |
    | `handles` | skip 2 header lines; `0x` lines containing ` File ` with length > 57 → `substring(57)`, cut at `"`; `\Device\` paths without `HardDiskVolume` are skipped |
    | `cmdline` | `Command line : ` → quoted path, or the first token |
    | `shimcache` | skip 2 header lines; length > 62 → `substring(62)`, cut at `"` |
    | `psscan` | skip 2 header lines; `0x` lines → `substring(19,37)` (process name) |
    | `pslist` | `0x` lines → `substring(10,34)` |
    | `psxview` | skip 2 header lines; `0x` lines → `substring(11,34)` |
    | `pstree` | lines containing `:` → `substring(indexOf(':')+1, 52)` |
    | `svcscan` | `Binary Path: ` → quoted path or first token; skip `-`, `\Driver\`, `\FileSystem\` |

  - **Path normalisation:**
    1. Trim; convert `\` to `/`; lowercase.
    2. Strip a `/??/` prefix and the drive letter (everything up to and including `:`).
    3. Replace `/systemroot/` and `%systemroot%` with `/windows/`; remove `/device/`.
    4. Remove `.../harddiskvolumeN` (a fixed 16-character skip).
    5. Discard `/namedpipe/...`.

    Names equal to `system` are ignored for the process-list plugins.
  - **Flagging (`flagFiles`):** for each path:
    - Look it up with `fileManager.findFiles(name[, parentPath])`. This searches **all data sources in the case**, not just the image being added; this is a latent bug or feature.
    - If nothing is found and the name has no `.`, retry with `name.%`, unless `<path>.exe` is already in the set.
    - If still nothing is found, add the non-fatal message "File {path}/{name} not found for output of {plugin} plugin".
    - Each resolved non-SLACK file gets **TSK_INTERESTING_ITEM** (an analysis result, `Score.SCORE_LIKELY_NOTABLE`, configuration = set name) with `TSK_SET_NAME` = "Volatility Plugin <plugin>". The artifact is skipped if an identical artifact already exists (`blackboard.artifactExists`). It is posted individually with a null job ID.
- **Rust port notes:**
  - There is no pure-Rust Volatility. Volatility 2.6 is EOL (Python 2), and Volatility 3 is Python. MemProcFS is C with Rust bindings, not pure.
  - **Parity path:** an optional external `vol` (Volatility 3) plugin using `-r json` output. Volatility 3 has no profiles; it uses symbol tables. Parse with serde instead of fixed columns, map plugins to the v3 names (`windows.pslist`, `windows.dlllist`, `windows.handles`, `windows.cmdline`, `windows.svcscan`, `windows.shimcachemem`, `windows.psscan`, `windows.pstree`, `windows.psxview` if available), and add dump outputs as derived files.
  - **Native path (large):** a Windows memory analyzer would need:
    - kernel DTB discovery (scan for `_EPROCESS` / KDBG, or use crash-dump headers);
    - PDB symbol download and parsing (`pdb` crate, pure Rust);
    - x64 page-table translation;
    - EPROCESS list walking.

    That covers pslist, dlllist and cmdline as an MVP. The parts are feasible with `pdb` + `object` + custom code, but it is a large project; do this only after parity via the external tool.
  - Also support crash dumps, hiberfil and raw dumps as image formats.


---

## 4. Python (Jython) modules, Android Analyzer, GPX Parser and the third-party plugin API

Autopsy ships some of its analysis as **Jython 2.7** scripts. These scripts call Autopsy and TSK Java classes directly. Three groups matter for postmortem:

1. `InternalPythonModules/`: bundled modules. These are the **Android Analyzer**, a data-source ingest module with 22 app parsers, and the **GPX Parser**, a file ingest module.
2. `Core/src/org/sleuthkit/autopsy/python/`: the loader that finds `.py` files and turns their classes into ingest factories, report modules and data source processors (DSPs).
3. `pythonExamples/`: public-domain samples. They define the de facto plugin API that third-party modules are written against.

Postmortem must reimplement every Android/GPX parser natively in Rust. It must also offer a plugin mechanism that covers the same use cases. A proposal for that mechanism is at the end of this section.

### Shared helper semantics (TSK `blackboardutils`, used by almost every parser)

The helpers live in the TSK Java bindings (`org.sleuthkit.datamodel.blackboardutils`), not in the Autopsy repo. The behaviour below follows those bindings. Verify the exact attribute sets against the TSK version you target.

**`AppSQLiteDB.findAppDatabases(dataSource, dbName, matchExactName, parentPathSubstr)`** is in `Core/.../coreutils/AppSQLiteDB.java`.

* It runs a case DB query of the form `findAllFilesWhere(...)`:
  * exact match: `LOWER(name)=LOWER(dbName)`
  * otherwise: `LOWER(name) LIKE '%dbName%' AND LOWER(name) NOT LIKE '%journal%'`
  * plus `LOWER(parent_path) LIKE '%<parentPathSubstr>%'` (the path is always a substring match from this entry point)
  * plus `data_source_obj_id = <ds>`
* Each hit is copied to `<case>/Temp/<objId><name>`. Sibling `<name>-wal` and `<name>-shm` files are copied as `<case>/Temp/<dbObjId><metaName>` (looked up by exact name in the same parent dir), so the WAL is replayed when the copy opens.
* Each copy is opened with `org.sqlite.JDBC`. Errors are logged and the file is skipped.
* `attachDatabase(ds, name, exactParentPath, alias)` finds a sibling DB by exact name **and** exact parent path, copies it, and runs `ATTACH DATABASE '<copy>' AS '<alias>'`. Line (`naver_line` as `naver`) and WhatsApp (`wa.db` as `wadb`) use it.
* `columnExists(table,col)` uses `PRAGMA table_info`. `tableExists` queries `sqlite_master`.
* Temp copies are **never deleted** by the analyzers. They are only removed with the case Temp dir.
* Postmortem: open the SQLite image in place through a read-only VFS backed by the content stream. rusqlite supports a custom VFS, or use `sqlite3_deserialize` for small DBs. WAL must still be applied, so either implement WAL replay or materialise the db, wal and shm trio into a tmpfs dir.

**`CommunicationArtifactsHelper(caseDb, moduleName, srcFile, accountsType, [selfAccountType, selfAccountId,] jobId)`**

* Without an explicit self account, the "self" account is the device account: `Account.Type.DEVICE`, using the data source's device id.
* `addContact(name, phone, homePhone, mobilePhone, email, [extraAttrs])`:
  * Creates `TSK_CONTACT` with `TSK_NAME`, `TSK_PHONE_NUMBER`, `TSK_PHONE_NUMBER_HOME`, `TSK_PHONE_NUMBER_MOBILE` and `TSK_EMAIL`. Empty values are skipped, and the extra attributes are appended (often `TSK_ID`).
  * Creates account instances: phone numbers become `PHONE` accounts, emails become `EMAIL` accounts. For an app account type, `TSK_ID` becomes an account of the helper's `accountsType`.
  * Adds a `CONTACT` relationship from self to each account and posts the artifact.
  * Throws `IllegalArgumentException` if no phone, email or ID is present.
* `addMessage(msgType, direction, fromId, toId|List<toId>, dateTimeSec, readStatus, subject, text, threadId, [extraAttrs])`:
  * Creates `TSK_MESSAGE` with these attributes:
    * `TSK_MESSAGE_TYPE`
    * `TSK_DIRECTION` (Incoming/Outgoing/Unknown)
    * `TSK_DATETIME` (when > 0)
    * `TSK_READ_STATUS` (when known)
    * `TSK_SUBJECT`, `TSK_TEXT`, `TSK_THREAD_ID`
  * Sender and recipients go into `TSK_PHONE_NUMBER_FROM/TO` for `PHONE`, `TSK_EMAIL_FROM/TO` for `EMAIL`, otherwise `TSK_ID_FROM/TO`. A list of recipients is comma-joined.
  * Creates account instances for every id and a `MESSAGE` relationship. The self account is the implied counter-party.
* `addCalllog(direction, callerId, calleeId|List, startSec, endSec, CallMediaType)`:
  * Creates `TSK_CALLLOG` with `TSK_DIRECTION`, the from/to attributes (same rules as messages), `TSK_DATETIME_START` and `TSK_DATETIME_END`.
  * Media type is AUDIO, VIDEO or UNKNOWN.
  * Adds a `CALL_LOG` relationship.
* `addAttachments(msgArtifact, MessageAttachments(fileAtts, urlAtts))` adds a JSON `TSK_ATTACHMENTS` attribute to the message.
  * `FileAttachment(caseDb, dataSource, path)` resolves `path` to a file object id inside the data source by path lookup. If the lookup fails, it stores the path only.
  * `URLAttachment(url)` stores the URL.
* Direction, read-status and media enums: `CommunicationDirection{INCOMING,OUTGOING,UNKNOWN}`, `MessageReadStatus{READ,UNREAD,UNKNOWN}`, `CallMediaType{AUDIO,VIDEO,UNKNOWN}`.

**`GeoArtifactsHelper(caseDb, moduleName, programName, srcFile, jobId)`**

* `addTrack(name, GeoTrackPoints, extra)` creates `TSK_GPS_TRACK` with `TSK_NAME`, `TSK_GEO_TRACKPOINTS` (JSON list of `{lat, lon, alt, name, velocity, distFromPrev, distTraveled, time}`) and `TSK_PROG_NAME`.
* `addRoute(name, creationTime, GeoWaypoints, extra)` creates `TSK_GPS_ROUTE` with `TSK_GEO_WAYPOINTS` (JSON list of `{lat, lon, alt, label}`), `TSK_NAME`, `TSK_DATETIME` and `TSK_PROG_NAME`.

**`WebBrowserArtifactsHelper(caseDb, moduleName, srcFile, jobId)`**

All web artifacts also get `TSK_DOMAIN`, which the helper derives from the URL.

| Method | Artifact | Attributes |
|---|---|---|
| `addWebBookmark(url,title,created,prog)` | `TSK_WEB_BOOKMARK` | `TSK_URL, TSK_TITLE, TSK_DATETIME_CREATED, TSK_PROG_NAME, TSK_DOMAIN` |
| `addWebCookie(url,created,name,value,prog)` | `TSK_WEB_COOKIE` | `TSK_URL, TSK_DATETIME_CREATED, TSK_NAME, TSK_VALUE, TSK_PROG_NAME, TSK_DOMAIN` |
| `addWebHistory(url,accessed,referrer,title,prog)` | `TSK_WEB_HISTORY` | `TSK_URL, TSK_DATETIME_ACCESSED, TSK_REFERRER, TSK_TITLE, TSK_PROG_NAME, TSK_DOMAIN` |
| `addWebDownload(url,start,path,prog)` | `TSK_WEB_DOWNLOAD` | `TSK_URL, TSK_DATETIME_ACCESSED, TSK_PATH, TSK_PROG_NAME, TSK_DOMAIN` |
| `addWebFormAutofill(name,value,created,accessed,count)` | `TSK_WEB_FORM_AUTOFILL` | `TSK_NAME, TSK_VALUE, TSK_DATETIME_CREATED, TSK_DATETIME_ACCESSED, TSK_COUNT` |
| `addWebFormAddress(person,email,phone,addr,created,accessed,count)` | `TSK_WEB_FORM_ADDRESS` | `TSK_NAME, TSK_EMAIL, TSK_PHONE_NUMBER, TSK_LOCATION, TSK_DATETIME_CREATED/ACCESSED, TSK_COUNT`, plus EMAIL/PHONE accounts |

**`ArtifactsHelper.addInstalledProgram(name, dateSec)`** creates `TSK_INSTALLED_PROG` with `TSK_PROG_NAME` and `TSK_DATETIME`.

**`general.py`** holds shared Android code:

* `MODULE_NAME = "Android Analyzer"`.
* `AndroidComponentAnalyzer` is the abstract base class. Its only method is `analyze(dataSource, fileManager, context)`.
* `appendAttachmentList(body, list)` appends `"\n\n------------Attachments------------\n"` plus newline-joined names. It is unused by the current parsers.
* `isValidPhoneNumber` and `isValidEmailAddress` delegate to `CommunicationsUtils`.
* It also contains a duplicate copy of `ResultSetIterator`.

**`ResultSetIterator.py`** wraps a JDBC `ResultSet`. `next()` returns False when the result set is None, and `close()` closes it.

**`Tsk*Parser.py`** are template base classes. Subclasses override getters, and the defaults below apply when a field is missing:

| Base class | Getter | Default |
|---|---|---|
| `TskContactsParser` | `get_contact_name/phone/home_phone/mobile_phone/email()` | `""` |
| `TskContactsParser` | `get_other_attributes()` | `None` |
| `TskMessagesParser` | `get_message_type`, `get_phone_number_from`, `get_phone_number_to`, `get_message_subject`, `get_message_text`, `get_thread_id` | `""` |
| `TskMessagesParser` | `get_message_direction` | `UNKNOWN` |
| `TskMessagesParser` | `get_message_date_time` | `-1` |
| `TskMessagesParser` | `get_message_read_status` | `UNKNOWN` |
| `TskCallLogsParser` | `get_call_direction` | `UNKNOWN` |
| `TskCallLogsParser` | `get_phone_number_from/to` | `""` |
| `TskCallLogsParser` | `get_call_start/end_date_time` | `-1` |
| `TskCallLogsParser` | `get_call_type` | `UNKNOWN` |

`TskMessagesParser` and `TskCallLogsParser` also expose constants: `INCOMING`, `OUTGOING`, `READ`, `UNREAD`, `INCOMING_CALL`, `OUTGOING_CALL`, `AUDIO_CALL` and `VIDEO_CALL`.

Postmortem: model these as a Rust trait `CommRecordSource` with default methods, or better as declarative mappings (see the proposal at the end).

### Android Analyzer orchestrator (`InternalPythonModules/android/module.py`)

* **Factory:** `AndroidModuleFactory(IngestModuleFactoryAdapter)`.
  * Display name "Android Analyzer". Description "Extracts Android system and third-party app data." Version is `Version.getVersion()`, the Autopsy version.
  * It is a data-source module only. It has no settings and no panels.
* **startUp** stores the context.
* **process(dataSource, progressBar):**
  * Gets the `FileManager` and builds the analyzer list in this **fixed order**: Contact, CallLog, TextMessage, Tango, WWF, GoogleMapLocation, BrowserLocation, CacheLocation, IMO, Xender, Zapya, ShareIt, Line, WhatsApp, TextNow, Skype, Viber, FBMessenger, SBrowser, Opera, OruxMaps, InstalledApplications.
  * Calls `switchToDeterminate(22)` and runs the analyzers sequentially on one thread.
  * Before each analyzer it checks `context.dataSourceIngestIsCancelled()` and returns OK if set.
  * Any exception adds `"Error running <Class>"` to an error list and logs the traceback. `progress(n)` is called after each success.
* **Bug: no ingest message.** The error list is built into an HTML string but **never posted**, so no inbox message is ever shown. The module always returns `OK`.
* **No file filter:** each analyzer locates its own DBs by name and path query. It does not use the file pipeline or MIME types.
* **Cancellation granularity** is per analyzer only. Inside an analyzer, long SQL loops never check cancellation, except that `ContentUtils.writeToFile` takes a cancel check in the location analyzers.
* **Timestamps:** almost every parser divides by 1000, so ms becomes s. Python 2 `/` on longs is integer division.

### Android app parsers (one entry per file)

The path filter is always `parent_path LIKE '%<package>%'`, case-insensitive. The name match is exact (case-insensitive) unless noted otherwise. "Helper" means `CommunicationArtifactsHelper` with the stated account type.

#### contact.py: Android Contacts provider
* **Package and files:** `com.android.providers.contacts`, files `contacts.db` and `contacts2.db`. Parser name "Android Contacts Parser". Helper account type `PHONE`, self account is the device.
* **Query:**
  * If `contacts.name_raw_contact_id` exists, it joins `raw_contacts` → `contacts` → `raw_contacts AS name_raw_contact` (for `display_name`) → `data` → `mimetypes`.
  * Otherwise it uses `raw_contacts.display_name`.
  * Filter: `mimetype IN ('vnd.android.cursor.item/phone_v2','vnd.android.cursor.item/email_v2')`, `ORDER BY display_name`.
* **Algorithm:** a control break on `display_name`. Rows are grouped per name, keeping the **last** phone (`data1`) and the **last** email seen. One `addContact(name, phone, None, None, email)` is emitted per name that has a phone or email.
* **Edge cases:**
  * Different people with the same display name are merged into one contact.
  * Multiple numbers for one person are lost; only the last is kept.
  * The `_VERSION` value "53.1.0.1" is informational only.

#### calllog.py: Samsung/AOSP call log
* **Package and files:** `com.sec.android.provider.logsprovider`, files `logs.db`, `contacts.db` and `contacts2.db`. Tables `calls` and `logs`, if present (`tableExists`).
* **Query:** `SELECT number,date,duration,type,name FROM <t> ORDER BY date DESC`.
* **Direction from `type`:**

  | `type` | Direction | Party set |
  |---|---|---|
  | 1 or 3 | INCOMING | `callerId = number` |
  | 2 or 5 | OUTGOING | `calleeId = number` |
  | other | UNKNOWN | none, so no artifact (the helper needs an id) |

* **Artifact:** `addCalllog(dir, caller, callee, date/1000, date/1000 + duration*1000, AUDIO)`. Account type `PHONE`.
* **Bug:** the end time mixes units. Start is in seconds, but duration (in seconds) is multiplied by 1000. Postmortem should compute `end = start + duration`.
* The `name` column is read but unused. `selfAccountId` is always None (dead code).

#### textmessage.py: SMS
* **Package and file:** `com.android.providers.telephony`, file `mmssms.db`. Parser name "Android Message Parser". Message type "Android Message".
* **Query:** `SELECT address,date,read,type,subject,body,thread_id FROM sms`. MMS tables are **not** parsed.
* **Mapping:**
  * `type=="1"` means INCOMING with `from=address`. Anything else is OUTGOING with `to=address`. So drafts, outbox and failed messages are all labelled outgoing.
  * `read` 1 is READ, 0 is UNREAD.
  * Timestamp is `date/1000`.
  * The thread id is `"<random UUID per DB>-<thread_id>"`, which makes threads unique per DB file.
  * Rows with a null `address` are skipped.
* Account type `PHONE`. A dead branch would have used `Account.Type.IMO` as the self type, which is a copy-paste bug.

#### tangomessage.py: Tango
* **Package and file:** `com.sgiggle.production`, file `tc.db`. Account `TANGO`. Message type "Tango Message".
* **Query:** `SELECT conv_id,create_time,direction,payload FROM messages ORDER BY create_time DESC`.
* **Mapping:** direction "1" is INCOMING, otherwise OUTGOING.
* **Body:** Base64-decode `payload`, interpret it as UTF-8, split on the `conv_id` string, and take element [1]. Use "" on any error.
* **Edge case:** `from` and `to` are always None. The helper still creates the message, but no accounts are created.

#### wwfmessage.py: Words With Friends
* **Package and file:** `com.zynga.words`, file `WordsFramework` (no extension). It creates a **custom account type** with `addAccountType("WWF","Words with Friends")`.
* **Query:** `SELECT message, strftime('%s',created_at) AS datetime, user_id, game_id FROM chat_messages ORDER BY game_id DESC, created_at DESC`.
* **Artifact:** `addMessage("WWF Message", UNKNOWN, from=user_id, to=None, datetime, UNKNOWN, "", message, "<uuid>-<user_id>")`.
* `game_id` is not stored.

#### googlemaplocation.py: Google Maps destination history
* **Files:** `fileManager.findFiles(ds, "da_destination_history")`. This is a name match with **no package filter**.
* Each file is copied to `Temp/<id><name>` with a cancellation check and opened via JDBC.
* **Query:** `SELECT time,dest_lat,dest_lng,dest_title,dest_address,source_lat,source_lng FROM destination_history`.
* **Coordinate conversion (`convertGeo(s)`):** the value is an integer string in micro-degrees. Insert a "." 6 characters from the end, i.e. divide by 1e6. If the length is 6 or less, parse the string as is.
* **Artifact:** `GeoArtifactsHelper(moduleName="Android Analyzer", program="Google Maps History")`.
  * `addRoute(dest_title, time/1000, [Waypoint(src_lat, src_lng, None, None), Waypoint(dst_lat, dst_lng, None, dest_address)], None)`, producing `TSK_GPS_ROUTE`.

#### browserlocation.py: WebView cached geoposition
* **Files:** `findFiles(ds, "CachedGeoposition%.db")`, a LIKE pattern. Zero-size files are skipped. The file is copied to Temp.
* **Query:** `SELECT timestamp,latitude,longitude,accuracy FROM CachedPosition`.
* **Artifact:** `TSK_GPS_BOOKMARK` (data artifact) with `TSK_GEO_LATITUDE`, `TSK_GEO_LONGITUDE`, `TSK_DATETIME` (=ts/1000) and `TSK_PROG_NAME="Browser Location History"`. Posted with source "Android Analyzer".
* `accuracy` is read but ignored.

#### cachelocation.py: Android network location cache
* **Files:** `findFiles("cache.cell")` + `findFiles("cache.wifi")`, non-empty only. These are binary, not SQLite.
* **Format (big-endian):**
  * Header: `i16 version, i16 entries`.
  * Each entry: `i16 keyLen`, `keyLen` bytes key (cell id or BSSID), then 32 bytes `i32 accuracy, i32 confidence, f64 lat, f64 lon, u64 readtime_ms`.
  * Parsing only proceeds if `entries*32 < fileSize`. Otherwise it logs a warning.
* **Artifact:** one `TSK_GPS_BOOKMARK` per entry with lat, lon, `TSK_DATETIME=readtime/1000` and `TSK_PROG_NAME="<filename> Location History"`.
* The key (MAC or cell id) is **discarded**. Postmortem could store it as `TSK_MAC_ADDRESS` or `TSK_DEVICE_ID` as an improvement.

#### imo.py: IMO messenger (v9.8.0)
* **Package:** `com.imo.android.imous`. Account `IMO`.
* **Self account:** read from `accountdb.db`: `SELECT uid,name FROM account`.
  * **Bug:** `getString("uid")` is called without `next()`, so the self account is effectively never set. Postmortem should call `next()`.
* **Contacts (`imofriends.db`):** `SELECT buid,name FROM friends` produces `addContact(name,"","","","", [TSK_ID=buid])`.
* **Messages (`imofriends.db`):**
  * Query: `SELECT messages.buid, imdata, last_message, timestamp, message_type, message_read, name FROM messages INNER JOIN friends ON friends.buid=messages.buid`.
  * `message_type==1` is INCOMING (`from=buid`), otherwise OUTGOING (`to=buid`).
  * `message_read` 1 is READ, 0 is UNREAD.
  * Timestamp is `timestamp/1e9` (nanoseconds).
  * The body is `last_message`. Thread id is empty.
* **Attachments:** parse `imdata` as JSON. If `original_path` is present, use it, otherwise `local_path`. The result becomes a `FileAttachment`.

#### xender.py: Xender file transfer (v4.6.5)
* **Package and file:** `cn.xender`, file `trans-history-db`. Note the hyphen: there is no ".db". Account `XENDER`.
* **Self account:** the first `device_id` from `SELECT device_id,nick_name FROM profile WHERE connect_times=0`.
* **Transfers:** `SELECT f_path,f_display_name,f_size_str,c_start_time,c_direction,c_session_id,s_name,s_device_id,r_name,r_device_id FROM new_history`.
  * `c_direction==1` is OUTGOING (`to=r_device_id`), otherwise INCOMING (`from=s_device_id`).
  * Timestamp is `c_start_time/1000`.
  * The message has a null text and thread=`c_session_id`.
  * `FileAttachment(f_path)` is attached.

#### zapya.py: Zapya file transfer (v5.8.3)
* **Package and file:** `com.dewmobile.kuaiya.play`, file `transfer20.db`. Account `ZAPYA`.
* **Query:** `SELECT device,name,direction,createtime,path,title FROM transfer`.
* **Mapping:** `direction==1` is OUTGOING (`to=device`), otherwise INCOMING (`from=device`). Timestamp is `createtime/1000`. The message has a null text and no thread. `FileAttachment(path)` is attached.

#### shareit.py: SHAREit (v5.0.28_ww)
* **Package and file:** `com.lenovo.anyshare.gps`, file `history.db`. Account `SHAREIT`.
* **Query:** `SELECT history_type,device_id,device_name,description,timestamp,file_path FROM history JOIN item WHERE history.content_id=item.item_id`.
* **Mapping:** `history_type==1` is INCOMING (`from=device_id`), otherwise OUTGOING. Timestamp is `timestamp/1000`. `FileAttachment(file_path)` is attached.

#### line.py: LINE (v9.15.1)
* **Package and files:** `jp.naver.line.android`, files `naver_line` (contacts and messages) and `call_history` (calls). Account `LINE`. Parser name "Line Parser".
* **Contacts:** `SELECT m_id, server_name FROM contacts` produces `addContact(server_name, "", "", "", "", [TSK_ID=m_id])`.
* **Calls:**
  * `call_history` ATTACHes the sibling `naver_line` as `naver`.
  * Query: a union of `membership` group ids (with `group_concat(m_id)` as members) and `naver.contacts` m_ids, joined to `call_history ON caller_mid=id`. Columns: `substr(call_type,-1)` as direction, `start_time`, `end_time`, `voip_type`, `voip_gc_media_type`.
  * Direction: "O" is OUTGOING. Everything else is INCOMING.
  * Outgoing calls go to the group member list, or else to `caller_mid`. Incoming calls come from `caller_mid`.
  * Media: `voip_type` "V" is VIDEO and "A" is AUDIO. For "G", `voip_gc_media_type` is used ("VIDEO" or "AUDIO").
  * Start and end times are ms converted to s.
* **Messages:**
  * The same union with `contacts`, joined to `chat_history ON chat_id=id WHERE attachement_type != 6`. The misspelling is the real column name.
  * `status==1` is INCOMING (`from=from_mid`). Otherwise OUTGOING, to the member list or the `id`.
  * Thread id is the group id only for groups.
  * Text is `content`. Time is `created_time/1000`.
  * A `FileAttachment(attachement_local_uri)` is added unless the value contains "content:".

#### whatsapp.py: WhatsApp (v2.19.244)
* **Package and files:** `com.whatsapp`, files `wa.db` (contacts) and `msgstore.db` (calls and messages). Account `WHATSAPP`.
* **Contacts (`wa_contacts`):**
  * `number` = the `number` column, falling back to `jid` when it is null or empty.
  * `name` = given + family name, falling back to display_name, then jid.
  * The phone is set only if `isValidPhoneNumber(number)`. The email is set only if `isValidEmailAddress`.
  * `TSK_ID=jid` is always added. A contact is emitted if it has any phone, email or extra attribute.
* **Single calls:**
  * Query: `call_log JOIN jid ON jid._id=call_log.jid_row_id WHERE _id NOT IN (SELECT call_log_row_id FROM call_log_participant_v2)`.
  * `from_me==0` is INCOMING, otherwise OUTGOING.
  * Start is `timestamp/1000`, end is start + `duration` (seconds).
  * `video_call==1` is VIDEO, otherwise AUDIO.
* **Group calls:**
  * Query: `call_log_participant_v2 JOIN call_log JOIN jid (participants) JOIN jid J1 (call owner) GROUP BY call_log._id`.
  * `group_concat(raw_string)` gives the members. Outgoing calls go to the member list. Incoming calls come from `J1.raw_string`.
* **Messages:**
  * ATTACH `wa.db` as `wadb`.
  * Build `contact_book_w_groups` = `wadb.wa_contacts` LEFT JOIN (`group_participants` grouped by gjid, with group_concat of non-empty jids) to get recipients. Join that to `messages ON key_remote_jid = jid`.
  * Columns: `key_from_me` as direction, `data` as text, `timestamp`, `received_timestamp`, `remote_resource` (the group sender) and `media_url`.
  * `key_from_me==0` is INCOMING. Outgoing messages go to the recipients list or `key_remote_jid`. Incoming messages come from `remote_resource` for groups, otherwise `key_remote_jid`.
  * Time is `timestamp/1000` for outgoing and `received_timestamp/1000` for incoming.
  * Thread id is the gjid for groups only.
  * A `URLAttachment(media_url)` is added only if it starts with `http:` or `https:`.
* **Edge cases:**
  * Messages whose `key_remote_jid` is not in `wa_contacts` are dropped by the inner join.
  * The newer WhatsApp schema (`message` table, 2021+) is unsupported. Postmortem should add the newer schema.

#### textnow.py: TextNow (v6.41.0.2)
* **Package and file:** `com.enflick.android.TextNow`, file `textnow_data.db`. Account `TEXTNOW`.
* **Contacts:** `SELECT contact_value AS number, coalesce-ish(contact_name, contact_value) AS name FROM contacts`. Phone and email are validated the same way as WhatsApp. A contact is emitted only if it has a phone or email.
* **Calls:**
  * Rows from `messages` where `message_type IN (100,102)`.
  * `message_direction==1` is INCOMING (`from=contact_value`), otherwise OUTGOING (`to=contact_value`).
  * Start is `date/1000`. End is start + `long(message_text)`, because the duration is stored in the text column.
  * The media type is always UNKNOWN, even though 100 and 102 are audio and video.
* **Messages:**
  * A union of `group_members`/`groups` (with `group_concat(member_contact_value)` as to_addresses, and the group's `contact_value` as thread_id) and `contacts`, joined to `messages`, `WHERE message_type NOT IN (100,102)`.
  * `from_address` and `to_address` are derived in SQL from the direction (1 is incoming, 2 is outgoing). `to` is split on ",".
  * Read status only applies to incoming messages (`read` boolean).
  * Attachment: `attach`, as a `FileAttachment`.
* **Bug:** `len(get_file_attachment())` raises `TypeError` when `attach` is NULL. The resulting exception aborts the TextNow message loop at the first row without an attachment. Postmortem: guard against None.

#### skype.py: Skype (v8.15.0.428)
* **Package and files:** `com.skype.raider`. The DB name is matched **by substring** `"live:"` (`matchExactName=False`), because the DB file is named after the user id. Files whose names contain "journal" are excluded. Account `SKYPE`.
* **Self account:** `SELECT entry_id, <name expr> FROM user`. The first row's `entry_id` is used.
* **Contacts:** from `person`. Name = first + last with commas removed, or `entry_id`. Extra attribute `TSK_ID=entry_id`, emitted via `addContact(name, "", "", "", "", [TSK_ID])`.
* **Calls:**
  * `contact_book_w_groups` = (`particiapnt` grouped by conversation_id with `group_concat(person_id)`) UNION (`person.entry_id`). It is joined to `chatitem ON conversation_link`, `WHERE message_type == 3`.
  * `is_sender_me` 0 is INCOMING (`from=person_id`), 1 is OUTGOING (to the participant list or the conversation_id).
  * Start is `time/1000`. End is start + `duration/1000`.
  * The media type is UNKNOWN; video and audio are not distinguished.
* **Messages:**
  * The same join, `WHERE message_type != 3`.
  * Text is `content`. Thread is the conversation_id for groups.
  * Attachment: `device_gallery_path`, as a `FileAttachment`.
* Note the real table name typo `particiapnt`.

#### viber.py: Viber (v11.5.0)
* **Package and files:** `com.viber.voip`, files `viber_data` (contacts and calls) and `viber_messages`. Account `VIBER`.
* **Contacts:**
  * Query: `phonebookcontact C JOIN phonebookdata D ON C._id=D.contact_id`, selecting `display_name` and `coalesce(D.data2, D.data1, D.data3)`.
  * Rows that have a number use `addContact`.
  * Rows with **no number but a name** get a raw `TSK_CONTACT` with only `TSK_NAME`. It is created via `newDataArtifact` and posted directly, bypassing the helper, so no account is created.
* **Calls:**
  * Query: `SELECT canonized_number, type, duration, date, viber_call_type FROM calls`.
  * `type` 1 (incoming) or 3 (missed) is INCOMING. Everything else is OUTGOING.
  * Start is `date/1000`, end is start + duration.
  * `viber_call_type` 1 is AUDIO, 4 is VIDEO.
* **Messages:**
  * A self-join of `participants` × `participants_info` builds, per sender, the comma list of other participants in the conversation. That is joined to `messages` on (participant_id, conversation_id).
  * `send_type` 0 is INCOMING, otherwise OUTGOING.
  * `to` = recipients split on ",". Time is `msg_date/1000`.
  * Read status: for incoming messages, `unread==0` means READ and anything else means UNREAD.
  * Thread is `conversation_id`.
  * Attachment: `extra_uri`. If it contains "content:", the body with `file://` stripped is used instead. Otherwise `extra_uri` with `file://` stripped.

#### fbmessenger.py: Facebook Messenger (FB v239.0.0.41)
* **Packages and accounts:** contacts come from the Facebook app package `com.facebook.katana`, file `contacts_db2`. Threads come from `com.facebook.orca`, file `threads_db2`. Account `FACEBOOK`. Module name "FB Messenger Analyzer". Message type "Facebook Messenger".
* **Self account:** `SELECT fbid,display_name FROM contacts WHERE added_time_ms = 0`. This has the same missing-`next()` bug as IMO.
* **Contacts:** `WHERE added_time_ms <> 0` produces `addContact(display_name, "", "", "", "", [TSK_ID=fbid, TSK_DATETIME_CREATED=added_time_ms/1000])`.
* **Messages:**
  * Query: `messages JOIN thread_participants ON thread_key JOIN thread_users ON user_key WHERE msg_type=0 ORDER BY msg_id`.
  * A control break on `msg_id` collects the recipient list. Recipients are all participant `user_key`s with the "FACEBOOK:" prefix stripped, excluding the sender.
  * The sender comes from the JSON `sender.user_key`.
  * Direction is OUTGOING if the sender equals the self account id, otherwise INCOMING. It is UNKNOWN if there is no sender.
  * Text is `text`, falling back to `snippet`. Thread is `thread_key`. Time is `timestamp_ms/1000`.
* **Message attachments:**
  * The JSON `attachments[0]` is examined by `mime_type`:
    * `image/jpeg`: the `urls` value is a Python-literal dict of preview to dict-string, giving `URLAttachment(src)` for each entry.
    * `video/mp4`: the urls plus `video_data_url` and `video_data_thumbnail_url`.
    * `audio/mpeg`: `FileAttachment(audio_uri without file://)`.
  * `pending_send_media_attachment[0].uri` gives a `FileAttachment`.
  * **Bug:** an audio attachment with an empty `audio_uri` hits `continue`, which skips writing the *previous* message. That is a control-break bug.
* **Calls:**
  * Rows with `msg_type=9` (1:1 call events) or `msg_type=203 AND admin_text_thread_rtc_event='group_call_ended'`, with the same control break.
  * End is `timestamp_ms/1000`.
  * The JSON `generic_admin_message_extensible_data` gives `call_duration` (seconds) and `video` (bool). Start = end − duration.
  * **Bug:** `mediaType` and `duration` are not reset per call, so values leak across rows.

#### sbrowser.py: Samsung Internet (v10.1.00.27) and operabrowser.py: Opera (v53.1.2569)
* **Packages and program names:** `com.sec.android.app.sbrowser` and `com.opera.browser`. Programs "SBrowser" and "Opera". Both use `WebBrowserArtifactsHelper`.
* **Chromium time conversion:** `t/1_000_000 − 11644473600` (WebKit µs to Unix seconds).

| Source | Query | Output |
|---|---|---|
| `sbrowser.db` (SBrowser only) | `SELECT url,title,created FROM bookmarks WHERE url IS NOT NULL` | `addWebBookmark`, with created in ms/1000 |
| `Cookies` | `SELECT host_key,name,value,creation_utc FROM cookies` | `addWebCookie(host_key, …)` |
| `History` | `SELECT url,title,last_visit_time FROM urls` | `addWebHistory` with an empty referrer |
| `History` | `SELECT target_path,start_time,url FROM downloads INNER JOIN downloads_url_chains ON downloads.id = downloads_url_chains.id` | `addWebDownload` (never works, see below) |
| `Web Data` | SBrowser: `autofill JOIN autofill_dates ON pair_id`. Opera: plain `autofill`. | `addWebFormAutofill(name, value, date_created, 0, count)` |
| `Web Data` | `autofill_profiles JOIN autofill_profile_names/phones/emails ON guid` | `addWebFormAddress` with the name as "first last" and the address as street, city, "state zip", country_code joined by newlines (never works, see below) |

* **Bugs:**
  * Downloads reference the undefined `historyResultSet`, so **no download artifacts are ever produced**.
  * Web form addresses call the undefined `autofillDbHelper`, so **no address artifacts are produced**.
  * The address name concatenation fails on NULL values.

  Postmortem should implement the intended behaviour. It should also reuse the generic Chromium parser from the Recent Activity inventory.

#### oruxmaps.py: OruxMaps (v7.5.7)
* **Package and file:** `oruxmaps` (path substring), file `oruxmapstracks.db`. Program "OruxMaps". Uses `GeoArtifactsHelper`.
* **POIs:** `SELECT poilat,poilon,poialt,poitime,poiname FROM pois` produces `TSK_GPS_BOOKMARK` with `TSK_DATETIME` (ms/1000), `TSK_GEO_LATITUDE`, `TSK_GEO_LONGITUDE`, `TSK_GEO_ALTITUDE`, `TSK_NAME` and `TSK_PROG_NAME`.
* **Tracks:**
  * Read `tracks(_id, trackname, trackciudad)`. For each track, read `segments WHERE segtrack=<id>`. For each segment, read `trackpoints WHERE trkptseg=<segId>` (`trkptlat`, `trkptlon`, `trkptalt`, `trkpttime` in ms).
  * Each segment becomes one `addTrack("<trackname> - <trackciudad> - <segname>", points)`. Each `TrackPoint` carries `name=segmentName` and velocity, distance and distance-traveled set to 0.
  * This is N+1 querying. Postmortem should use a single ordered join.

#### installedapps.py: Google Play library
* **Package and file:** `com.android.vending`, file `library.db`.
* **Query:** `SELECT doc_id, purchase_time FROM ownership`.
* **Artifact:** `ArtifactsHelper.addInstalledProgram(doc_id, purchase_time/1000)`, producing `TSK_INSTALLED_PROG`.
* It lists purchased or owned apps, not necessarily installed ones.

#### Account types created (summary)
* Built-in types: `PHONE`, `IMO`, `TANGO`, `XENDER`, `ZAPYA`, `SHAREIT`, `LINE`, `WHATSAPP`, `TEXTNOW`, `SKYPE`, `VIBER`, `FACEBOOK`, plus `DEVICE` as the implicit self account.
* Custom type: `WWF` ("Words with Friends").
* Postmortem's account-type table must be seeded with all of these and must allow runtime additions.

### GPX Parser (`InternalPythonModules/GPX_Module/GPX_Parser_Module.py`)

* **Factory:** "GPX Parser", version "1.2". It is a **file** ingest module with no settings.
* **Bundled library:** `gpxpy` 0.8.8 (Apache-2.0, pure Python). Postmortem replacement: the `gpx` crate, or `quick-xml` with a custom schema. Handle GPX 1.0 and 1.1.
* **Construction:** creates `<case>/Temp/GPX_Parser_Module/`.
* **File selection:** `file.getName().lower().endswith(".gpx")`. There is no MIME check and no exclusion of unallocated or virtual files.
* **Processing:**
  * The file is copied to `Temp/GPX_Parser_Module/<uuid4>.gpx`. The copy is **never deleted**.
  * On a parse exception the module logs and returns `ProcessResult.ERROR`.
  * **Tracks:** each `track.segment` becomes `GeoTrackPoints`. Each point is `TrackPoint(lat, lon, elevation or 0, None, 0, 0, 0, ts)`, where `ts = time.mktime(point.time.timetuple())`.
    * **Bug:** `mktime` interprets UTC as *local* time, so timestamps are skewed by the analyst machine's UTC offset. Postmortem: use the UTC epoch.
    * Each non-empty segment becomes `addTrack("Track", points, None)`, producing `TSK_GPS_TRACK`. The track name is always the literal "Track"; `track.name` is ignored.
  * **Waypoints:** each `gpx.waypoint` becomes a direct `newDataArtifact(TSK_GPS_BOOKMARK)` with `TSK_GEO_LATITUDE`, `TSK_GEO_LONGITUDE`, `TSK_FLAG="Waypoint"`, `TSK_NAME=waypoint.name` and `TSK_PROG_NAME="GPXParser"`, then `postArtifact`. Elevation and time are dropped.
  * **Routes:** each route becomes `GeoWaypoints` of `Waypoint(lat, lon, elevation, name)`, then `addRoute(None, None, waypoints, None)`, producing `TSK_GPS_ROUTE` with no name and no time.
  * The `GeoArtifactsHelper` is created with programName=None.
* **shutDown:** posts `IngestMessage DATA "<count> files found"`.
  * **Bug:** `fileCount` is per module *instance*, and there is one instance per file-ingest thread. With N threads the inbox gets N messages.

### Jython module loading (`Core/src/org/sleuthkit/autopsy/python/`)

**`JythonModuleLoader`**

* **Entry points:** `getIngestModuleFactories()`, `getGeneralReportModules()` and `getDataSourceProcessorModules()`. All are `static synchronized`.
* **Startup preload:** `core/Installer.preloadJython()` calls all three on a background thread so the first real load is faster. `IngestModuleFactoryLoader` calls the ingest one on **every** request; nothing is cached, so editing scripts takes effect without a restart.
* **Directories scanned:**
  1. `<userdir>/InternalPythonModules/*/`. On first use, `copyInternalInstallToUserDir()` copies the install's `InternalPythonModules` there, but only if the user dir copy does not exist yet. So **upgrades do not refresh it unless that folder is deleted**, which is a known staleness issue. The copy exists so that `$py.class` compilation does not write into Program Files.
  2. `<userdir>/python_modules/*/`, from `PlatformUtil.getUserPythonModulesPath()`. On Windows this is `%APPDATA%\autopsy\python_modules`. `core/Installer.ensurePythonModulesFolderExists()` creates it, and **Tools → Python Plugins** (`OpenPythonModulesFolderAction`) opens it.
* **Search depth:** only **immediate subdirectories** are scanned, and only `*.py` files directly inside them. Files in the root of `python_modules` are ignored, and nested deeper folders are ignored.
* **Discovery is textual, not by introspection:** each `.py` file is read line by line. A line qualifies if it **starts with `class `** (column 0) and contains the filter token:

  | Kind | Token |
  |---|---|
  | Ingest | `IngestModuleFactoryAdapter` or `IngestModuleFactory` |
  | Report | `GeneralReportModuleAdapter` or `GeneralReportModule` |
  | DSP | `DataSourceProcessorAdapter` or `DataSourceProcessor` |

  The class name is `line.substring(6, line.indexOf("("))`.
  * Consequences: indented classes, a class whose base list is on another line, and classes inheriting through an intermediate Python base are all missed.
  * Any class whose *name* contains the token also matches. For example, `class FooDataSourceProcessorSettings(JPanel)` would match the DSP filter and fail to load.
* **Instantiation (`createObjectFromScript`), all in **one shared `PythonInterpreter`**:**
  1. `import sys; sys.path.append('<script dir>')`
  2. `import <module>; reload(<module>)`
  3. `from <module> import <Class>; obj = <Class>()`
  4. `interpreter.get("obj", Interface.class)`
  5. `sys.path.remove(dir)`

  Sibling modules (e.g. `general.py` and `gpxpy/`) import because the script dir is on the path during import. Modules are cached in `sys.modules` across plugins, so **two plugins with a same-named helper module (e.g. `utils.py`) collide**.
* **Failures:** a load failure logs and shows a modal error dialog naming the class. An interpreter creation failure (e.g. non-Latin characters in the path) shows a notification and loads no Python modules.
* **Ordering:** results are sorted case-insensitively by Python class simple name. `IngestModuleFactoryLoader` then appends the Jython factories **after** the core-ordered Java modules and the alphabetical third-party Java modules. A duplicate display name is rejected with a dialog.
* **Sandboxing: none.** Scripts run in-process with full JVM, filesystem and network access. They can call any Autopsy or TSK internal API, spawn processes, and crash or hang ingest. Jython is Python 2.7 only, so no C extensions (numpy etc.) are available.

**`FactoryClassNameNormalizer.normalize(name)`**

* Jython proxy classes are named like `org.python.proxies.<module>$<Class>$<N>`, where N changes per load. If a name contains `org.python.proxies`, the trailing `$\d*` is stripped.
* The stable name is used for three things:
  1. The per-module settings filename: `<normalized>.settings` under the ingest profile/context dir, from `IngestJobSettings`.
  2. The `ingest_modules` registration in the case DB (`IngestJobExecutor` → `caseDb.addIngestModule(name, normalizedFactoryClass, type, version)`).
  3. Unique names in `IngestJobSettingsPanel` history.
* Postmortem needs an equivalent **stable plugin identity**, e.g. `"<plugin-id>/<factory-id>"`, for settings persistence and ingest-job provenance.

### Third-party plugin API surface (what `pythonExamples/` and real plugins use)

#### Factory contract (`IngestModuleFactory` / `IngestModuleFactoryAdapter`)

| Method | Purpose |
|---|---|
| `getModuleDisplayName()` | Must be globally unique. It is the module identity in the UI, in attributes and in the DB. |
| `getModuleDescription()`, `getModuleVersionNumber()` | Shown in the UI and stored in the `ingest_modules` table. |
| `hasGlobalSettingsPanel()`, `getGlobalSettingsPanel()` | Returns an `IngestModuleGlobalSettingsPanel`, a Swing panel with `saveSettings()`, shown under Tools → Options. |
| `getDefaultIngestJobSettings()`, `hasIngestJobSettingsPanel()`, `getIngestJobSettingsPanel(settings)` | Per-job settings. The panel class is `IngestModuleIngestJobSettingsPanel`, whose `getSettings()` returns the settings object. |
| `isDataSourceIngestModuleFactory()` / `createDataSourceIngestModule(settings)` | Data-source module type. |
| `isFileIngestModuleFactory()` / `createFileIngestModule(settings)` | File module type. |
| `isDataArtifactIngestModuleFactory()` / `createDataArtifactIngestModule(settings)` | Data-artifact module type. |
| `isAnalysisResultIngestModuleFactory()` / `createAnalysisResultIngestModule(settings)` | Analysis-result module type. |

* **Settings objects:**
  * Python modules use `GenericIngestModuleJobSettings`, a `HashMap<String,String>` with `getSetting`, `setSetting` and `removeSetting`.
  * Java modules use any `Serializable` `IngestModuleIngestJobSettings`, which is Java-serialized to `<normalizedFactoryClass>.settings`.
* **Python factory pitfall:** the sample stores the settings on the factory (`self.settings`) and passes them to `createFileIngestModule`. That breaks with concurrent jobs.
* **Module instances:** `create*Module` is called once per pipeline copy. With N file-ingest threads you get N `FileIngestModule` instances per job; data-source modules get one. Modules must therefore be thread-safe across instances, e.g. via shared reference counting such as `IngestModuleReferenceCounter`.

#### Module lifecycle
* `startUp(IngestJobContext)` may throw `IngestModuleException(msg[, cause])`. If any module's startUp fails, the **whole job fails to start** and errors are shown.
* The per-type process methods:
  * `DataSourceIngestModule.process(Content dataSource, DataSourceIngestModuleProgress)`
  * `FileIngestModule.process(AbstractFile)`
  * `DataArtifactIngestModule.process(DataArtifact)`
  * `AnalysisResultIngestModule.process(AnalysisResult)`

  All return `ProcessResult.OK` or `ERROR`. An `ERROR` from a file module is recorded as an `IngestModuleError` but does not stop the pipeline.
* `shutDown()` is called once per instance at job end, including after cancellation. Modules typically post summary inbox messages here.

#### `IngestJobContext`

| Method | Purpose |
|---|---|
| `getJobId()` | Required on every `postArtifact` and helper constructor call; links artifacts to the ingest job. |
| `getDataSource()` | The data source being ingested. |
| `getExecutionContext()` | The profile/context name. |
| `isJobCancelled()`, `dataSourceIngestIsCancelled()`, `fileIngestIsCancelled()`, `dataArtifactIngestIsCancelled()` | Cancellation checks. |
| `processingUnallocatedSpace()` | Job setting for whether unallocated space is processed. |
| `addFilesToJob(List<AbstractFile>)` | Schedules derived or carved files into the current job. `scheduleFiles` is deprecated. |

#### `DataSourceIngestModuleProgress`
`switchToDeterminate(workUnits)`, `switchToIndeterminate()`, `progress(int)`, `progress(String msg)`, `progress(String task, int units)`.

#### `IngestServices.getInstance()`

| Method | Purpose |
|---|---|
| `postMessage(IngestMessage)` | Posts to the Ingest Inbox. |
| `fireModuleDataEvent(ModuleDataEvent)` | Legacy; `postArtifact` does this now. |
| `fireModuleContentEvent(ModuleContentEvent)` | Signals new derived content so the tree refreshes. |
| `getCase()`, `getCaseDatabase()`, `getLogger(moduleName)`, `getFreeDiskSpace()` | Case, DB, logging and disk-space access. |
| `getConfigSetting(module,key)`, `setConfigSetting`, `getConfigSettings`, `setConfigSettings` | A per-module properties file in the user config dir (`ModuleSettings`). |

#### `IngestMessage`

| Method | Message type |
|---|---|
| `createMessage(type, source, subject[, detailsHtml])` | DATA, INFO, WARNING or ERROR |
| `createErrorMessage(src, subject, html)` | ERROR |
| `createWarningMessage(src, subject, html)` | WARNING |
| `createDataMessage(src, subject, html, uniqueKey, BlackboardArtifact)` | DATA, linked to an artifact |

The inbox groups messages by source and subject, and dedupes by `uniqueKey`.

#### Content access
* **Finding files:**
  * `Case.getCurrentCase().getServices().getFileManager()`.
  * `findFiles(dataSource, namePattern)`: a SQL LIKE pattern with `%` and `_`, case-insensitive.
  * `findFiles(dataSource, name, parentSubstring)`, `findFiles(ds, name, parentFile)`, `findFilesExactName(parentId, name)`.
  * `addLocalFilesDataSource(deviceId, rootName, tz, [paths], FileAddProgressUpdater)`, used by DSPs.
* **Reading content:**
  * `ReadContentInputStream(file)`, a Java InputStream.
  * `file.read(byte[], offset, len)`.
  * `ContentUtils.writeToFile(content, java.io.File[, cancelCheck])` extracts to disk.
* **Raw case DB:** `SleuthkitCase.findAllFilesWhere("<SQL where on tsk_files>")`. Plugins depend on the raw `tsk_files` column names: `crtime`, `meta_type`, `name`, `parent_path`, `data_source_obj_id`, `md5`, etc.
* **AbstractFile getters:** `getName`, `getParentPath`, `getUniquePath`, `getSize`, `getType()` (a `TSK_DB_FILES_TYPE_ENUM`: FS, CARVED, DERIVED, LOCAL, UNALLOC_BLOCKS, UNUSED_BLOCKS, LAYOUT_FILE, SLACK…), `isFile`, `isDir`, `getMd5Hash`, `getMIMEType`, `getArtifacts(type)`, `getId`, `getDataSource`.
* **Data-source and case info:** `Image.getPaths()` for data-source image paths. `Case.getTempDirectory()`, `getModuleDirectory()`, `getCaseDirectory()`, and `addReport(path, srcModule, name)`.

#### Blackboard
* **Creating artifacts:**
  * `file.newDataArtifact(BlackboardArtifact.Type, Collection<BlackboardAttribute>)`.
  * `file.newAnalysisResult(Type, Score, conclusion, configuration, justification, attrs).getAnalysisResult()`. Score values include `SCORE_LIKELY_NOTABLE` and `SCORE_NOTABLE`.
* **Attributes:** `BlackboardAttribute(Type|ATTRIBUTE_TYPE|typeId, moduleName, value)`, where the value is a string, int, long, double or byte[].
* **Posting:** `skCase.getBlackboard().postArtifact(art, moduleName, jobId)`. This indexes the artifact for keyword search and fires events. `postArtifacts(list, …)` is the batch form.
* **Custom types:**
  * `skCase.addBlackboardArtifactType(name, displayName)`, or `blackboard.getOrAddArtifactType(name, display, category)`.
  * `skCase.addArtifactAttributeType(name, valueType, display)`, or `blackboard.getOrAddAttributeType`.
  * `getArtifactType(name)`, `getAttributeType(name)`.
  * Registry_Example.py shows the try/except-on-duplicate pattern.
* **Helpers:** `CommunicationArtifactsHelper`, `GeoArtifactsHelper`, `WebBrowserArtifactsHelper` and `ArtifactsHelper` (semantics above). `CommunicationsManager.addAccountType(name, display)` adds account types.
* **App DB helper:** `AppSQLiteDB` (find, copy, open, attach, query).
* **Bundled third-party Java libraries plugins rely on:**
  * Rejistry (`com.williballenthin.rejistry.RegistryHiveFile`) for registry hives.
  * The `org.sqlite.JDBC` driver.
  * Apache commons. Tango uses `Base64`.

#### External processes
* `ExecUtil.execute(ProcessBuilder, ProcessTerminator)` blocks. It polls the terminator every **5 s** (`DEFAULT_TERMINATION_CHECK_INTERVAL`) and kills the process tree when `shouldTerminateProcess()` returns true.
* **Terminators:**
  * `DataSourceIngestModuleProcessTerminator(context[, maxRunTimeSec | useGlobalTimeOut])` terminates on `dataSourceIngestIsCancelled()`, giving `ProcTerminationCode.CANCELATION`. It also terminates on timeout, giving `TIME_OUT`.
  * `FileIngestModuleProcessTerminator` is the same but uses `fileIngestIsCancelled`.
  * Global timeout: `UserPreferences.ProcessTimeOutEnabled`, default **false**, with `ProcessTimeOutHours` default **60 h**.
* The RunExe.py sample runs a bundled `img_stat.exe` on `Image.getPaths()[0]`, redirects stdout to `<case>/Reports/img_stat-<dsId>.txt`, and calls `Case.addReport`. It deletes partial output when cancelled.
* `PlatformUtil.isWindowsOS()` gates Windows-only executables.

#### Report modules (`GeneralReportModuleAdapter`)
* Methods: `getName()`, `getDescription()`, `getRelativeFilePath()`, optional `getConfigurationPanel()`/`get/setConfiguration`, and `supportsDataSourceSelection()`.
* `generateReport(GeneralReportSettings settings, ReportProgressPanel progress)`. The settings provide `getReportDirectoryPath()` and the selected data source ids.
* Progress calls: `setIndeterminate`, `start`, `setMaximumProgress`, `increment`, `updateStatusLabel`, and `complete(ReportStatus.COMPLETE|ERROR)`.
* The module must call `Case.addReport(path, moduleName, displayName)`.
* Samples: a file-count report (crtime in the last 14 days) and a CSV of `getUniquePath(), md5` for all non-directories.

#### Data source processors (`DataSourceProcessorAdapter`)
* Methods: `getDataSourceType()` (the wizard label), `getPanel()` (a JPanel that fires `DSP_PANEL_EVENT.UPDATE_UI` property changes), `isPanelValid()`, `run(host, progressMonitor, callback)` (must be asynchronous), `cancel()`, `reset()`.
* Optional:
  * `run(password, host, …)`.
  * `runWithIngestStream(…)` with `supportsIngestStream()` for streaming ingest.
  * `DataSourceProcessorProgressMonitor`: `setIndeterminate`, `setProgress`, `setProgressText`.
* **Callback:** `callback.done(DataSourceProcessorResult{NO_ERRORS,NONCRITICAL_ERRORS,CRITICAL_ERRORS}, errorList, contentList)`. `Case.notifyAddingDataSource(uuid)` and `notifyDataSourceAdded(content, uuid)` fire events.
* The sample (`DataSourceProcessorModule.py`) extracts a .tar/.tgz into the module dir and adds it as a logical-files data source.

### Proposal: postmortem plugin architecture

**Goals:**
* Keep parity with the Autopsy plugin surface: ingest (4 kinds), report and DSP modules, with settings.
* Allow writing small parsers fast, as the Python modules do today.
* Make plugins safe enough that a buggy third-party module cannot corrupt the case or hang ingest.
* Keep hot loops fast.

**Tier 0: built-in modules (Rust crates, compiled in).**
* Every module in this inventory, including all 22 Android analyzers and GPX, implements the same traits that external plugins see through the host ABI.
* Registration uses `inventory`/`linkme` distributed slices with an explicit `order` key, reproducing Autopsy's core ordering.

**Tier 1: declarative "App DB recipes" (TOML or YAML, no code).**
* About 80% of the Android parsers are "find DB by name and package, run SQL, map columns, convert units, call a helper". A recipe captures exactly that:

  ```toml
  [recipe]
  id = "android.viber.calls"
  account_type = "VIBER"
  [[recipe.db]]
  name = "viber_data"
  path_contains = "com.viber.voip"
  exact_name = true
  attach = [{ name = "viber_messages", alias = "m" }]
  [[recipe.emit]]
  kind = "calllog"
  sql = "SELECT canonized_number n, type d, duration s, date t, viber_call_type c FROM calls"
  direction = { col = "d", map = { 1 = "in", 3 = "in", _ = "out" } }
  from = { when = "in", col = "n" }
  to   = { when = "out", col = "n" }
  start = { col = "t", unit = "ms" }
  end   = { expr = "start + s" }
  media = { col = "c", map = { 1 = "audio", 4 = "video" } }
  ```

* Supported emit kinds: `contact`, `message`, `calllog`, `web_*`, `gps_bookmark`, `gps_track`, `gps_route`, `installed_prog`, and `generic` (any artifact type plus an attribute map).
* Supported transforms: unit conversion (`s`, `ms`, `us`, `ns`, `webkit_us`, `cocoa`), `split`, `json_path`, `base64`, `regex`, `coalesce`, phone and email validation, and a control-break `group_by` (as in contacts and FB Messenger).
* Recipes are hot-reloadable, schema-validated, and testable against fixture DBs. They also cover most iLEAPP/aLEAPP-style needs.

**Tier 2: WASM component plugins (wasmtime + WIT). This is the recommended third-party ABI.**
* **Isolation:** runs in-process with memory isolation. CPU time is limited by epoch interruption (cancellation and timeouts map directly onto `ProcTerminationCode`), and memory is capped per instance.
* **Capabilities:** there is no ambient filesystem or network. Everything goes through host functions, granted per manifest: `read-content`, `write-derived`, `exec` (off by default), `net` (off by default).
* **Portability:** one `.wasm` file works on every OS. Plugins can be written in Rust, C, Go, Python (componentize-py) or JS.
* **Packaging:** `plugin.toml` manifest (id, version, kinds, capabilities, settings schema) + `.wasm` + assets. The user plugins dir is `<config>/postmortem/plugins/<id>/`, scanned recursively. The manifest provides the stable identity, replacing class-name regex discovery and `FactoryClassNameNormalizer`.
* **Instancing:** one instance per worker thread, mirroring Autopsy's per-pipeline-copy semantics. Use a pooling allocator, so instantiation takes microseconds.

**Tier 3 (optional, trusted): native dylibs via `abi_stable` or `stabby`.**
* For performance-critical closed-source modules, such as vendor decoders.
* Not sandboxed; requires explicit user trust and a signature check.

**Python compatibility (optional feature `python`):** embed CPython via `pyo3`, not RustPython, because real plugins need the stdlib and C extensions.
* Ship a `postmortem` Python package that mirrors the Autopsy names, for easy porting:
  * `IngestModuleFactoryAdapter`, `FileIngestModule`, `DataSourceIngestModule`, `IngestModule.ProcessResult`, `IngestMessage`.
  * `Case.getCurrentCase().getServices().getFileManager().findFiles(ds, pattern)`, `ReadContentInputStream`.
  * `BlackboardArtifact.Type.*`, `BlackboardAttribute.Type.*`.
  * `CommunicationArtifactsHelper` and the other helpers, plus `AppSQLiteDB`. Use the Python `sqlite3` module on a host-materialized copy.
* Jython-specific `from java.* import` lines cannot be supported. Provide a `java.util.ArrayList` / `Arrays.asList` shim and a migration guide.
* Run Python plugins in a **separate worker process** (one per plugin, IPC via shared-memory ring + bincode). This bounds crashes, the GIL and memory, and gives each plugin its own `sys.modules`, which fixes Autopsy's shared-interpreter module collisions.

**Host API surface to expose.** Express it as WIT interfaces; the same Rust traits back Tier 0.

```
interface types      { obj-id, job-id, score{unknown,likely-notable,notable,…}, process-result{ok,error},
                       file-kind{fs,carved,derived,local,unalloc-blocks,unused-blocks,layout,slack,virtual-dir},
                       attr-value{text,int32,int64,double,bytes,datetime,json} }
interface plugin     { describe() -> manifest; global-settings-schema(); default-job-settings() -> settings(json);
                       validate-job-settings(settings) -> result<_, list<string>> }
interface ingest-ds  { start-up(ctx) -> result; process(ds: obj-id, progress) -> process-result; shut-down() }
interface ingest-file{ start-up(ctx) -> result; process(file: file-info) -> process-result; shut-down() }
interface ingest-art { start-up(ctx); process(artifact: artifact-info) -> process-result; shut-down() }   // data-artifact & analysis-result kinds
interface report     { generate(settings, out-dir, progress) -> result<list<report-entry>> }
interface dsp        { settings-schema(); run(settings, host-id, progress) -> stream<new-content>; cancel() }

// host-provided
interface job-context { job-id(); data-source(); execution-context(); is-cancelled(kind);
                        processing-unallocated(); add-files-to-job(list<obj-id>) }
interface progress    { determinate(units); indeterminate(); advance(units); message(text) }
interface content     { file-info(obj-id); read-at(obj-id, offset, len) -> bytes; size(obj-id);
                        open-stream(obj-id) -> stream; hashes(obj-id) -> {md5?, sha1?, sha256?};
                        mime-type(obj-id); children(obj-id); parent(obj-id); path(obj-id) }
interface files       { find(ds, name-like, parent-like?) -> list<file-info>; find-exact(parent, name);
                        query(filter: structured-filter)   // NO raw SQL; typed filter AST
                        materialize(obj-id) -> temp-path  // only with fs capability (for sqlite etc.)
                        open-sqlite(obj-id, with-wal: bool) -> db-handle; db-query(handle, sql, params) -> rows; attach(handle, obj-id, alias) }
interface derived     { add-derived-file(parent, name, bytes|temp-path, derivation-details) -> obj-id;
                        add-carved-files(parent, list<range+name>) -> list<obj-id>; add-local-files-ds(...) }
interface blackboard  { get-or-add-artifact-type(name, display, category); get-or-add-attribute-type(name, value-type, display);
                        new-data-artifact(src, type, attrs) -> art-id; new-analysis-result(src, type, score, conclusion, config, justification, attrs);
                        post(list<art-id>)          // batched; host coalesces into one txn + one event
                        artifacts-of(obj-id, type?) ; attributes(art-id) }
interface comms       { add-account-type(name, display); contact(...); message(...); calllog(...); attachments(art-id, files, urls) }
interface geo         { track(name, points, extra); route(name, time, waypoints, extra); bookmark(...) }
interface web         { bookmark/cookie/history/download/form-autofill/form-address(...) }
interface inbox       { post(level{data,info,warning,error}, subject, details-html?, unique-key?, artifact?) }
interface config      { get(key) / set(key, value) per-plugin KV (replaces IngestServices.get/setConfigSetting) }
interface log         { log(level, msg) }
interface exec        { run(program: bundled-asset|path, args, stdin?, stdout-to: temp|report, timeout) -> exit-code  // capability-gated; host polls cancel every 1 s, not 5 s }
interface case        { temp-dir(); module-dir(); add-report(path, module, display) }
```

**Settings UI.**
* Plugins **cannot** ship egui code across the WASM boundary. Instead they declare a settings schema: a JSON-Schema subset with bool, int range, enum, string, path-picker, list-of-strings and table. The host renders it in egui, both per job and globally.
* This covers every Autopsy Python example (checkboxes, text fields, file choosers). Tier 0 built-ins may additionally register native egui panels.
* Settings persist as JSON keyed by stable plugin id, with a per-profile override (replaces Java-serialized `.settings`).

**Behavioural fixes to carry over from this audit:**
* Clean up temp copies, or avoid them entirely via an in-place SQLite VFS.
* Post one summary message per *job*, not per thread instance.
* Interpret GPX time as UTC.
* Implement the Android parser bugs listed above as their intended behaviour: call-log end time, IMO/FB self account, SBrowser/Opera downloads and addresses, TextNow null attachment, FB control-break `continue`.
* Check cancellation inside long per-row loops. The host can do this cheaply: `post()` returns `Cancelled` once the job is cancelled.
* Batch artifact posting: one transaction per DB file rather than per row. The Android analyzers currently do one insert, one event and one index request per artifact.


---

## 5. Module matrix

| Module (display name) | Kind | Default | Main inputs | Artifacts / effects | External dependency → Rust status |
|---|---|---|---|---|---|
| Hash Lookup | File | on | every file (not dirs/unalloc/slack) | sets `md5`, `sha256`, `known`; `TSK_HASHSET_HIT` | TSK hash DB (C) → **implement** (rusqlite, sorted u128 arrays); hashing → **crate** md-5/sha2 |
| File Type Identification | File | on | every file | sets `mime_type`; optional `TSK_INTERESTING_ITEM` | Apache Tika → **implement** (generate from tika-mimetypes.xml + container detectors) |
| Extension Mismatch Detector | File | on | allocated files with an extension | `TSK_EXT_MISMATCH_DETECTED` | none |
| Embedded File Extractor | File | on | archives, Office, PDF | derived files (XOR1), `TSK_ENCRYPTION_DETECTED`, zip-bomb `TSK_INTERESTING_ITEM` | 7-Zip-JBinding → **crate** zip/sevenz-rust2/tar/flate2/bzip2, **implement** RAR, ARJ; POI/Tika → **implement** on `cfb` + `lopdf` |
| Picture Analyzer | File | on | jpeg/tiff/wav (EXIF), heic/heif | `TSK_METADATA_EXIF` + `TSK_USER_CONTENT_SUSPECTED`; derived JPEGs | metadata-extractor → **crate** kamadak-exif; libheif → **external/optional** (no pure-Rust HEVC) |
| Encryption Detection | File + DS | on | octet-stream files ≥5 MiB; Office/PDF/Access; volumes | `TSK_ENCRYPTION_SUSPECTED`, `TSK_ENCRYPTION_DETECTED` | Tika/Jackcess → **implement** header checks |
| Interesting Files Identifier | File | on | every file except slack | `TSK_INTERESTING_ITEM` | Java regex → **crate** regex (+fancy-regex) |
| Data Source Integrity | DS (stage 2) | on | Image data sources | sets image md5/sha1/sha256; `TSK_VERIFICATION_FAILED` | **crate** md-5/sha1/sha2 |
| PhotoRec Carver | File | on | `UNALLOC_BLOCKS` files | carved `LayoutFile`s under `$CarvedFiles` | photorec_win.exe → **implement** carver (or external) |
| Scalpel Carver | File | not registered | unallocated | carved files | libscalpel JNI → superseded |
| Virtual Machine Extractor | DS | on | `*.vmdk/.vhd/.vhdx` | **new data sources** + recursive ingest jobs | **implement** VMDK/VHD/VHDX readers |
| DJI Drone Analyzer | DS | on | `FLY???.DAT` | `TSK_GPS_TRACK` | DatCon.jar → **implement** |
| YARA Analyzer | File | on (needs rule sets) | `.exe` or all files | `TSK_YARA_HIT` | yarac/libyara JNI → **crate** yara-x |
| Android Analyzer (aLEAPP) / iOS Analyzer (iLEAPP) | DS | on | logical acquisitions, zip/tar | many data artifacts (TSV mapping) + HTML report | Python exe → **external** (or port parsers natively) |
| Plaso | DS | **off** | Image data sources | `TSK_TL_EVENT` | log2timeline/psort → **external**; native evtx/registry crates for parity |
| Android Analyzer (Jython) | DS (stage 1) | on | app SQLite DBs | contacts, call logs, messages, web, GPS, installed programs, accounts | Jython → **built-in Rust** parsers (rusqlite) |
| GPX Parser | File | on | `*.gpx` | `TSK_GPS_TRACK`, `TSK_GPS_ROUTE`, `TSK_GPS_BOOKMARK` | gpxpy → **crate** `gpx` or quick-xml |
| Object Detection (experimental) | File | on if installed | jpeg/png/bmp/gif | `TSK_OBJECT_DETECTED` | OpenCV Haar → **partial** (rustface/tract) |
| Volatility (experimental DSP) | DSP + ingest | n/a | memory images | files + `TSK_INTERESTING_ITEM` | volatility 2.6 exe → **external** |
| Keyword Search, Email Parser, Recent Activity, Central Repository, Malware Scan | (see other inventory files) | | | | |

## 6. Performance problems in Autopsy and improvement ideas for postmortem

### 6.1 Where Autopsy loses time (observed in the code)

1. **One global lock for scheduling.** `IngestTasksScheduler` is `synchronized` for every enqueue,
   refill and completion. Refilling runs `getChildren()` (a DB round trip per directory) **inside** the
   lock, and `hasTasksForJob` linearly scans `LinkedList`s of every queued task (O(n) per completed task
   → O(n²) on big queues). File threads starve while another thread expands a directory.
2. **Small fixed thread pool**: default 2 file threads, maximum 8, restart required; a single DS thread
   shared by every job; a single artifact thread. CPU-heavy modules (hashing, entropy, decompression)
   leave most cores idle, and slow modules (external tools) block their whole thread.
3. **Repeated full reads of the same file**: hash (MD5 + SHA-256), Tika detection (twice: magic, then
   magic + name), entropy (byte at a time through `BufferedInputStream`), YARA (whole file into a
   `byte[]`, or a temp copy above 100 MiB), the Picture Analyzer (HEIC read fully into memory), Keyword
   Search text extraction, and so on. Each read goes JNI → TSK → image decompression (E01, zlib)
   **again**.
4. **Per-artifact transactions**: `postArtifact` is called per hit (Plaso posts one artifact per timeline
   row). Each post does an `artifactExists` query, an insert, keyword indexing and an event. Only file
   property updates are batched (500 per transaction).
5. **Java serialization and XML** for settings. Reflection-heavy and slow to start; harmless at runtime.
6. **Temp copies to disk** before external tools (VM extractor, PhotoRec unallocated, YARA > 100 MiB,
   Volatility, LEAPP), and the XOR1 re-encoding of every extracted byte.
7. **Native-library reloads**: YARA reloads compiled rules for every file × rule file. Tika
   `AutoDetectParser` objects are rebuilt per call in the encryption checks.
8. **Event fan-out on one thread**: every `FILE_DONE` and `DATA_ADDED` is published individually on a
   single executor, and in multi-user cases over ActiveMQ.
9. **Unbounded memory**: `IOUtils.toByteArray` for Office embedded objects, a HEIC `byte[]` the size of
   the file, ingest message details accumulating HTML, `processedFiles` with no backpressure on
   `addFilesToJob` (archive explosions enqueue everything at once at the head of the queue).

### 6.2 Design for postmortem

**Scheduling and parallelism**
* **Work-stealing pipeline** (`rayon` scope or a custom crossbeam-deque executor) with *per-file tasks
  that run the whole file pipeline* (keeps Autopsy's per-file module order and lets one worker own the
  file's bytes). Default the worker count to `available_parallelism()`, adjustable live, no restart.
  Give each module a declared cost class (CPU, IO, External), and run External modules on a separate
  bounded pool so that they never block CPU workers.
* **Lock-free bookkeeping**: per-job `AtomicU64` counters (queued, running, done) replace list scans.
  "Tier complete" = `queued == 0 && running == 0`, signalled through a `Notify`/condvar.
* **Directory-order streaming instead of `getChildren()`**: one cursor over the case DB file table
  ordered by `(priority_class, parent_path, name)`, read in pages of 10 000 rows by a single producer
  thread into a bounded channel (backpressure). The root-priority classes (Users/home first, pagefile
  last) become a computed column.
* **Derived-file re-injection** goes through the same bounded channel with a high-priority lane. Add a
  per-job cap on the extraction depth and on total derived bytes (generalises the zip-bomb rules).

**I/O**
* **Read once, fan out.** For each file, one reader produces chunks (e.g. 1 MiB) that are fed to every
  streaming consumer at once: MD5, SHA-256, SHA-1 (optional), the entropy histogram, the YARA streaming
  scanner, and magic detection (first 8 KiB). Modules declare `wants_stream()` versus `wants_random_access()`.
  Random-access consumers (zip, OLE, SQLite, EXIF) get a `Read + Seek` handle with a shared block cache.
* **mmap where possible**: logical and local files, and raw/dd images, through `memmap2`. For
  E01/AFF4/VMDK, use a decompressed-chunk LRU cache shared across threads (sharded by chunk index). Sort
  work by on-disk offset where cheap (the TSK meta address roughly correlates with the location).
* **No temp copies**: give external tools a FUSE-less path where possible (named pipe or `-` stdin for
  tools that support it). Otherwise copy once to a content-addressed cache keyed by SHA-256 so that
  every tool shares one copy.
* Store extracted files **once**, content-addressed (`blake3` or sha256 path), compressed with zstd;
  make the XOR1 obfuscation optional (it only exists to dodge antivirus software).

**Hashing and matching**
* **SIMD hashing**: `sha2` with the `asm` feature uses SHA-NI and ARMv8 SHA extensions (≥2 GB/s per
  core). MD5 cannot be vectorised within one stream, so use **multi-buffer MD5** across 4–16 files at once
  (AVX2/AVX-512 lanes, as in Intel's isa-l_crypto / `md5-simd` in Go) for small-file-heavy images.
  `blake3` for internal dedup keys.
* **Hash sets**: mmapped sorted 16-byte arrays + an Eytzinger layout or a binary-fuse filter. Lookups are
  O(1)-ish and fit in memory for NSRL scale. Build them once at import and memoise per set version.
* **Rule matching**: compile all interesting-file sets and file filters into a single `RegexSet` +
  `aho-corasick` for literals, so one evaluation per file covers every set. File-type magic as a
  generated decision tree (first-byte dispatch table) instead of a linear list.
* **YARA**: yara-x `Rules` compiled once per job, shared through `Arc`; one `Scanner` per worker.
* **Entropy**: 4-way split histograms (u32×256×4) to avoid dependency stalls; this reaches memory
  bandwidth. Compute it from the shared chunk stream, not from a second read.

**Database**
* **Batched writes**: per-worker write buffers flushed to a single writer thread (SQLite WAL, one
  connection, `BEGIN IMMEDIATE` batches of 1–10k rows, prepared statements). For PostgreSQL use
  `COPY ... FROM STDIN BINARY` for artifacts and attributes. Commit on a size or time threshold (e.g.
  5000 rows or 250 ms).
* **Idempotency without a SELECT per artifact**: replace `artifactExists` with a unique index on
  `(obj_id, artifact_type, attrs_fingerprint)` and `INSERT ... ON CONFLICT DO NOTHING`. The fingerprint is
  a stable hash of the sorted attribute tuples.
* Defer keyword indexing and event publication: batch `DATA_ADDED` events per (module, type) every
  N ms. The UI subscribes to coalesced change notifications (tokio broadcast or crossbeam channel).
* Persist `known`, `mime_type`, `md5`, `sha256`, `sha1` through the same batched writer. Avoid one UPDATE
  per file by using a staging table + `UPDATE ... FROM` at commit (PostgreSQL), or a large batch of
  `UPDATE` statements in one transaction (SQLite).

**Robustness**
* Bounded memory everywhere: streaming extractors (`zip` streaming, `sevenz-rust2` block iteration),
  caps on embedded-object sizes, and an explicit per-module memory budget.
* Per-file **timeouts and panic isolation**: run each module's `process` under `catch_unwind`; for
  parsers of hostile input (Office, PDF, archives) consider a subprocess sandbox pool (or WASM for
  plugins) with a wall-clock timeout that marks the file and moves on, instead of hanging a worker.
* Real **disk-space monitoring** (Autopsy's is disabled): poll `statvfs` on the case and temp volumes
  every 10 s, pause extraction under a soft threshold, cancel under a hard threshold.
* Progress: lock-free per-worker activity slots (module, file id, start instant) read by the UI at
  10 Hz. Per-module time histograms (hdrhistogram) instead of a single summed counter. Throughput in
  files/s and MB/s.
