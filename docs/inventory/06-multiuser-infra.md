# 06 — Multi-user, automation & infrastructure: feature inventory (Autopsy → postmortem)

This inventory covers everything in Autopsy that exists to make several machines work on one case, to run Autopsy unattended, or to support it operationally. It is detailed enough to reimplement the behaviour in postmortem (pure Rust) without reading the Java. The Autopsy source (read-only) is at `../autopsy`. Unless stated otherwise, paths are relative to `Core/src/org/sleuthkit/autopsy/`. Experimental paths are relative to `Experimental/src/org/sleuthkit/autopsy/experimental/`.

Scope:

- `coordinationservice` (ZooKeeper locks and node data), `events` (ActiveMQ remote events), the multi-user parts of `casemodule` (`multiusercases`, `multiusercasesbrowser`, `CollaborationMonitor`, the lock and delete logic in `Case.java`), `core/ServicesMonitor`, `core/UserPreferences` (multi-user keys), `corecomponents/MultiUserSettingsPanel`.
- `appservices`, `machinesettings`, `featureaccess`, `diagnostics`, `threadutils`, `healthmonitor` (infrastructure aspects only), `commandlineingest`, `integrationtesting`, `mcp`, `testutils`, `test`.
- Experimental `autoingest` (auto ingest nodes, dashboard, manifests, job states, case deletion, file exporter, case importer) and `configuration` (auto ingest settings, shared configuration, the multi-user test tool, mode selection).
- Tools: `Tools/ZookeeperNodeMigration`, `Tools/ManifestTool`, `Tools/autopsy-mcp-stdio`.
- Build, test and release scaffolding (platform support and test strategy).

The final section, **§16, is a PROPOSAL** for the postmortem architecture. Everything before it describes Autopsy as it is.

## Contents

1. Multi-user topology at a glance
2. Multi-user settings: keys, defaults, storage, validation, service monitoring
3. Coordination service (ZooKeeper): API, semantics, lock and node inventory
4. Case node data (`CaseNodeData`) and node-path conventions
5. Multi-user case lifecycle: create, open, close, delete, data-source delete
6. Event bus: local and remote publishing (ActiveMQ), channels, remote event list
7. Collaboration monitor (remote progress indicators)
8. Multi-user case browser, Open Multi-User Case dialog, Cases dashboard
9. Small infrastructure packages: appservices, machinesettings, featureaccess, diagnostics, threadutils, health monitor (infra)
10. Command-line mode (`commandlineingest`)
11. MCP server (`mcp`) and the stdio bridge
12. Auto ingest (cluster, manifests, jobs, nodes, dashboards, admin, deletion, export, import)
13. Auto ingest configuration and shared configuration
14. Test infrastructure, CI, platforms, release
15. Quirks and bugs worth not reproducing
16. PROPOSAL: pure-Rust architecture for postmortem

---

## 1. Multi-user topology at a glance

A multi-user Autopsy deployment uses five external services plus shared storage:

| Role | Product | Default port | Used for |
|---|---|---|---|
| Case database | PostgreSQL (tested with 9.x and later) | 5432 | One PostgreSQL database per case. Also the Central Repository database and the `HealthMonitor` database. |
| Text index | Solr 8 (Solr 4 is still supported for reading old cases) | 8983 | One Solr core (collection) per case. |
| Coordination | ZooKeeper (by default the one embedded in Solr) | Solr port + 1000, so 9983 | Cluster-wide read/write locks and small binary "node data" blobs (case registry, auto ingest job registry). |
| Messaging | ActiveMQ 5.x (tested 5.14) | 61616 (OpenWire TCP) | Remote events (JMS topics) between all clients and auto ingest nodes. |
| Shared storage | SMB/UNC share | — | Case directories (the `.aut` file, logs, exports, reports, module output), auto ingest input folder, shared configuration folder. |

Every client ("examiner" machine) and every auto ingest node (AIN) talks to all of them directly. There is no Autopsy server process. All coordination is peer-to-peer through ZooKeeper and ActiveMQ. Case data lives in PostgreSQL and Solr, and files live on the share.

Two kinds of machines exist:

- **Examiner clients** run the normal UI. They create and open multi-user cases, see other users' changes through remote events, and can open the auto ingest dashboard. Admin features are gated by marker files.
- **Auto ingest nodes** are switched to "auto ingest mode" (`UserPreferences.SelectedMode.AUTOINGEST`). They show the `AutoIngestControlPanel` instead of the normal UI, scan an input folder for manifest files, and process jobs one at a time.

---

## 2. Multi-user settings

### 2.1 Storage

`core/UserPreferences.java` keeps settings in four `.properties` files (`ConfigProperties`):

| File | Location | Content |
|---|---|---|
| `ViewPreferences.properties` | `<userdir>/config/ModuleConfig/` (the "shared" module config dir) | View options |
| `MachineSpecificPreferences.properties` | `<userdir>/config/` | Ingest threads, process timeout, log count, Solr heap, temp folder, health monitor report path, map tile paths |
| `ModePreferences.properties` | `<userdir>/config/` | `AutoIngest` settings key, `AutopsyMode` (mode ordinal), `AppName` |
| `ExternalServicePreferences.properties` | `<userdir>/config/ModuleConfig/` | Everything multi-user (below), plus geolocation tile options, OCR translation and text translator name |

On first run, values are migrated from the legacy `config/Preferences/org/sleuthkit/autopsy/core.properties` if none of the new files exist (`updateConfig()`).

`<userdir>` is the NetBeans user directory (`%APPDATA%\autopsy` on Windows, `~/.autopsy/dev` and similar on Unix). The shared configuration feature (§13.3) copies these files between nodes.

### 2.2 Keys and defaults (ExternalServicePreferences)

| Key | Default | Meaning |
|---|---|---|
| `IsMultiUserModeEnabled` | `false` | Master switch. `getIsMultiUserModeEnabled()` also requires `isMultiUserSupported()`, which is true only if the `org.sleuthkit.autopsy.keywordsearch.SolrSearchService` `AutopsyService` is present (the KeywordSearch module was built in). |
| `ExternalDatabaseHostnameOrIp` | `""` | PostgreSQL host |
| `ExternalDatabasePortNumber` | `"5432"` | PostgreSQL port (stored as a string) |
| `ExternalDatabaseName` | — | Declared but unused for case DBs (each case has its own database) |
| `ExternalDatabaseUsername` | `""` | PostgreSQL user |
| `ExternalDatabasePassword` | `""` | Encrypted (see below) |
| `ExternalDatabaseType` | `"POSTGRESQL"` | `DbType` enum name. If it fails to parse, `SQLITE` is used. |
| `Solr8ServerHost` | `""` | "Indexing server" host (Solr 8) |
| `Solr8ServerPort` | `"8983"` | Solr 8 port (stored as int) |
| `IndexingServerHost` | `""` | **Solr 4** host (legacy key name) |
| `IndexingServerPort` | `""` | Solr 4 port |
| `ZookeeperServerHost` | `""` | ZooKeeper host. If empty, the Solr 8 host is used. |
| `ZookeeperServerPort` | `"9983"` | ZooKeeper port. If host or port is empty, Solr 8 port + 1000 is used. |
| `MessageServiceHost` | `""` | ActiveMQ host |
| `MessageServicePort` | `61616` | ActiveMQ port. An unparseable value falls back to 61616. |
| `MessageServiceUser` | `""` | Optional |
| `MessageServicePassword` | `""` | Optional, encrypted |

**Password "encryption"** (`coreutils/TextConverter`): PBEWithMD5AndDES with a hard-coded key `hgleri21auty84fwe`, salt `DE 33 10 12 DE 33 10 12`, 20 iterations, then Base64. This is obfuscation, not security. postmortem should use the OS keychain (see §16).

Other relevant keys: `ProcessTimeOutEnabled` / `ProcessTimeOutHours` (default 60 h), `NumberOfFileIngestThreads`, `MaximumNumberOfLogFiles`, `SolrMaxJVMSize` (2048 MB on 64-bit, 512 MB on 32-bit), `AutopsyMode` (ordinal of `STANDALONE`=0 or `AUTOINGEST`=1, stored in the `AutoIngest` module settings file).

### 2.3 Options panel (Tools → Options → Multi-User)

`corecomponents/MultiUserSettingsPanel.java`:

- Checkbox **Enable multi-user cases**. It is disabled when multi-user is unsupported or a case is open. When a case is open, all fields are disabled and the text "Close case to modfy settings" is shown.
- Four groups, each with a **Test** button and a status icon (green tick or red cross) plus a warning label:
  - **Database Server Settings**: host, port, user name, password. The test calls `SleuthkitCase.tryConnect(CaseDbConnectionInfo(..., POSTGRESQL))` and shows the exception message.
  - **Solr 8 Server Settings** (host, port), **Solr 4 Server Settings** (host, port), **ZooKeeper Server Settings** (host, port). Notes: "Enter Solr 8 and/or Solr 4 server settings." and "New text indexing can only be done with Solr 8." The ZooKeeper tooltip explains that embedded ZooKeeper uses the Solr port + 1000. Solr is tested via `KeywordSearchService.tryConnect(host, port)`. ZooKeeper is tested with the four-letter word `ruok` (expects `imok`), 15 s connect and read timeout.
  - **ActiveMQ Message Server Settings**: host, port, user name (optional), password (optional). The test is `MessageServiceConnectionInfo.tryConnect()`.
- Field pre-population: Solr fields are filled from saved values. ZooKeeper is filled from saved ZK values. Otherwise it is filled from the Solr 8 host (or the Solr 4 host if only Solr 4 is set) with the default port.
- **Validation** (`valid()`), shown in a red text line, in this order:
  1. All of these must be populated: DB host, port, user **and password**; message host and port; ZK host and port; at least one of Solr 8 or Solr 4 (host and port). Otherwise: "Fill in all values". Message service user and password must be both set or both empty.
  2. DB port must be an integer from 0 to 65535: "Invalid database port number".
  3. Solr 8 port, if given, must be valid: "Invalid Solr 8 server port number". The same applies to Solr 4. At least one Solr must be configured: "Either Solr 8 or/and Solr 4 server needs to be configured". ZK host must be non-empty: "ZooKeeper server host name not set". ZK port must be valid: "Invalid ZooKeeper server port number".
  4. Message port must be valid: "Invalid message service port number".
- **Store**: writes all values. If multi-user was already enabled and the ZooKeeper host or port changed, a dialog says a restart is required. The coordination service is a process-wide singleton created once. If multi-user was toggled or the DB connection changed, it notifies the Central Repository settings (`GlobalSettingsPanel.onMultiUserChange`).

### 2.4 ActiveMQ connection check (`events/MessageServiceConnectionInfo`)

- URI: `tcp://<host>:<port>?wireFormat.maxInactivityDuration=0` (inactivity monitoring disabled).
- `tryConnect()`: a missing host, user (null) or password (null) gives a specific message. It opens and closes a JMS connection. Errors are classified from the cause message:
  - starts with "connection timed out": bad host name;
  - starts with "connection refused": bad port;
  - starts with "user name [": bad credentials;
  - anything else: "host name or port".
  - With no cause, it tries `InetAddress.isReachable(1000 ms)`. Reachable means a port problem, otherwise a host problem.

### 2.5 Services monitor (`core/ServicesMonitor.java`)

- Monitors three services: `REMOTE_CASE_DATABASE`, `REMOTE_KEYWORD_SEARCH`, `MESSAGING`, each with status `UP` or `DOWN`. ZooKeeper is **not** monitored here.
- Checks run at construction and then every **15 minutes** (`CRASH_DETECTION_INTERVAL_MINUTES`), only when multi-user is enabled. `getServiceStatus(name)` re-checks core services synchronously on every call. Auto ingest relies on this before each job.
- Check methods:
  - DB: `SleuthkitCase.tryConnect`.
  - KWS: `tryConnect` on the Solr 8 host, then the Solr 4 host if one is configured. Status is UP if any configured server succeeded and no exception was thrown.
  - Messaging: `tryConnect`.
- On a status *change*, it logs, shows a tray notification ("Connection to {service} is up/down" style), and publishes a **local-only** `ServiceEvent(service, status, details)` whose event name is the service name. Subscribers include the auto ingest dashboard and control panel ("Services Status: Case databases up, keyword search up, coordination up, messaging up"). Other modules may register their own service names via `setServiceStatus`.

---

## 3. Coordination service (ZooKeeper)

Source: `coordinationservice/CoordinationService.java`, `coordinationservice/utils/CoordinationServiceUtils.java`, `CoordinationServiceNamespace.java`.

### 3.1 Connection

- Singleton, created lazily on first `getInstance()`. It cannot be re-pointed without restarting the app.
- Host and port come from `ZookeeperServerHost`/`ZookeeperServerPort`. If either is empty: Solr 8 host and Solr 8 port + 1000.
- Pre-flight check: `ruok`/`imok` over a raw socket (15 s timeouts). Failure means "Unable to access ZooKeeper".
- Apache Curator client: session timeout 300 000 ms, connection timeout 300 000 ms, `ExponentialBackoffRetry(1000 ms, 3 retries)`.
- **Namespace root**: from the first `CoordinationServiceNamespace` service provider, else `"autopsy"`. Paths are `/autopsy/<category>/...`.
- At startup it creates persistent nodes (open ACL) for every category:

| `CategoryNode` | Path segment |
|---|---|
| `CASES` | `cases` |
| `MANIFESTS` | `manifests` |
| `CONFIG` | `config` |
| `CENTRAL_REPO` | `centralRepository` |
| `HEALTH_MONITOR` | `healthMonitor` |

### 3.2 API and semantics

All node paths given by callers are **upper-cased** before use. A path starting with `/` is appended directly, otherwise a `/` is inserted. Trailing slashes are stripped. Intermediate nodes are created with `ZKPaths.mkdirs`.

Implication: a Windows path such as `\\SERVER\Cases\Foo_2020_01_01_10_00_00` becomes one znode name containing backslashes (ZooKeeper only splits on `/`). A Unix path `/mnt/cases/foo` becomes a nested node tree `/autopsy/cases/MNT/CASES/FOO`. On Unix, `getNodeList(CASES)` would then return `MNT` rather than the case paths. Multi-user only really worked with Windows UNC paths. postmortem must not copy this.

| Method | Semantics |
|---|---|
| `tryGetExclusiveLock(category, path)` | Curator `InterProcessReadWriteLock(path).writeLock().acquire(0 s)`. Returns `null` if not immediately available. |
| `tryGetExclusiveLock(category, path, timeout, unit)` | Same, but waits up to the timeout. Returns `null` on timeout. Throws `InterruptedException` if interrupted. |
| `tryGetSharedLock(category, path[, timeout, unit])` | Read lock of the same RW lock. Many shared holders, or one exclusive holder. |
| `Lock.release()` / `close()` | Releases. Curator locks are **re-entrant per thread and must be released by the acquiring thread**. This is why `Case` runs open and close on one dedicated single-thread executor. |
| `getNodeData(category, path)` | Creates the node if missing, then returns its bytes (`null` if there is no node). |
| `setNodeData(category, path, bytes)` | Sets data. Fails if the node does not exist. |
| `deleteNode(category, path)` | Deletes. Callers detect "NoNode" by searching the cause message for `KeeperErrorCode = NoNode`. |
| `getNodeList(category)` | Direct children names of the category node (already upper-cased). |

Locks are ephemeral-sequential children under the path node (the Curator recipe). A crashed holder's lock disappears when its ZooKeeper session expires (up to 300 s). **Node data is persistent.** Lock and data share the same znode path: the data lives on the parent node, the lock children below it.

### 3.3 Lock inventory (everything locked in Autopsy)

| Category | Node path (before upper-casing) | Mode | Timeout | Holder / purpose |
|---|---|---|---|---|
| CASES | case directory path, e.g. `\\srv\cases\Foo_2021_01_01_12_00_00` | **shared** while a case is open in a client, **exclusive** for data-source deletion | 1 min (`CASE_LOCK_TIMEOUT_MINS`) | `Case.acquireCaseLock`. It is held for the whole time the case is open. Failure gives "Failed to get a shared/exclusive lock on the case." |
| CASES | `<caseDir>_RESOURCES` | exclusive | 1 hour (`CASE_RESOURCES_LOCK_TIMEOUT_HOURS`) | Held only *during* a case open, create or close action. It serialises the creation and teardown of case resources (DB schema, Solr core, app services) across machines. |
| CASES | `<caseDir>` | exclusive, no wait | 0 | Multi-user case deletion (`Case.deleteMultiUserCase`). Failure gives "Cannot delete case because it is open for another user or host." |
| CASES | case **name** (directory name minus `_yyyy_MM_dd_HH_mm_ss` timestamp), e.g. `FOO` | exclusive | 30 min (AIM) / 0 (delete) | Auto ingest `openCase()`: serialises "find or create the case directory for this case name" across nodes. `DeleteCaseTask` also takes it. |
| CASES | `<caseDir>\AUTO_INGEST_LOG.TXT` | exclusive | 15 min | Every append to the per-case auto ingest log (`AutoIngestJobLogger`). |
| MANIFESTS | manifest file full path | exclusive, no wait | 0 | AIM: claim a job (`dequeueAndLockNextJob`), held for the whole job. Also taken briefly for crash recovery, adding new jobs and upgrading node data. |
| MANIFESTS | manifest file full path | exclusive | 5 min | `DeleteCaseTask` locks all manifests of a case. |
| CONFIG | shared configuration folder absolute path | exclusive (upload) / shared (download) | 30 min | `SharedConfiguration`. Returns `LOCKED` if not acquired. |
| CENTRAL_REPO | `<host>_<dbName>` | exclusive | 5 min | `PostgresCentralRepo.getExclusiveMultiUserDbLock()`, used during CR schema create and upgrade. If ZooKeeper is unreachable, it proceeds without a lock. |
| HEALTH_MONITOR | `HealthMonitor` | exclusive (create/upgrade DB) / shared (sample data) | 5 min | `HealthMonitor` database bootstrap. |

### 3.4 Node data inventory

| Category | Node | Payload |
|---|---|---|
| CASES | `<caseDir>` | `CaseNodeData` (§4). This is the multi-user **case registry**. |
| CASES | `<caseName>`, `<caseDir>_RESOURCES`, `<caseDir>\AUTO_INGEST_LOG.TXT` | none (lock-only nodes) |
| MANIFESTS | manifest path | `AutoIngestJobNodeData` (§12.4). This is the **auto ingest job registry and queue**. |
| CONFIG, CENTRAL_REPO, HEALTH_MONITOR | — | lock-only |

`Tools/ZookeeperNodeMigration` is a stand-alone CLI (`<inZkHost> <inZkPort> <outZkHost> <outZkPort>`). For every category, it copies each node's data to another ZooKeeper (taking an exclusive lock on the destination node). It is used when moving from the Solr-embedded ZooKeeper to a stand-alone one.

---

## 4. Case node data (`casemodule/multiusercases`)

### 4.1 `CaseNodeData` binary format (Java `DataOutputStream`, big-endian, "modified UTF-8" strings with u16 length prefix)

Current major version 2, minor version 0.

| Field | Encoding | Notes |
|---|---|---|
| `version` | i32 | 0 = legacy (only an errors flag), 1, 2 |
| `errorsOccurred` | v1: bool byte. v2: byte, negative (0x80) = true | Set when an auto ingest job for the case had errors. Shown as a red/yellow status in the AID and Cases dashboard. |
| `directory` | UTF | case directory path |
| `createDate` | i64 ms epoch | from case metadata |
| `lastAccessDate` | i64 ms epoch | updated on every multi-user open |
| `name` | UTF | unique case name (with timestamp) |
| `displayName` | UTF | updated when the user renames the case |
| `deletedItemFlags` | i16 bit set | see below |
| `minorVersion` | i32 (v2+) | |

`DeletedFlags`: `TEXT_INDEX=1`, `CASE_DB=2`, `CASE_DIR=4`, `DATA_SOURCES=8`, `MANIFEST_FILE_NODES=16`. They make deletion resumable and idempotent: each step is skipped if its flag is already set.

### 4.2 Behaviour

- `createCaseNodeData(metadata)`: written when a multi-user case is created.
- `readCaseNodeData(path)`: if the data is missing or corrupt, it is **rebuilt from the `.aut` metadata file** in the directory (this fails if the directory or `.aut` is gone). Older versions are upgraded (v0 → rebuilt from metadata, keeping the errors flag; v1 → minor version set) and written back.
- `writeCaseNodeData`: overwrites.
- `CaseNodeDataCollector.getNodeData()`: lists all CASES children and skips:
  - case-name nodes (no `\` and no `//` in the path; Windows-centric);
  - `_RESOURCES` nodes;
  - nodes whose file name is `AUTO_INGEST_LOG.TXT`.
  
  It reads each remaining node and silently skips (with a log entry) those that fail. This is the data source for the multi-user case browser.

### 4.3 Node path helpers (`multiusercases/CoordinationServiceUtils`)

- resources node = `caseDir + "_RESOURCES"`
- auto ingest log node = `caseDir/AUTO_INGEST_LOG.TXT`
- case directory node = `caseDir`
- case name node = directory name with a trailing `_yyyy_MM_dd_HH_mm_ss` (20 chars) removed (plus a trailing `_`).

Case directory naming convention (CLI and auto ingest): `<caseName>_<yyyy_MM_dd_HH_mm_ss>`. The internal case "name" (used in event channel names) is `displayNameToUniqueName`: non-ASCII, control characters and `` /?:'"\`` are replaced by `_`, the result is lower-cased, and `_yyyyMMdd_HHmmss` is appended.

---

## 5. Multi-user case lifecycle (`casemodule/Case.java`)

### 5.1 Open/create (`doOpenCaseAction`)

All case actions for one `Case` run on one dedicated single-thread executor (thread name contains the case name), because of the Curator same-thread release rule. In GUI mode a modal progress dialog with optional Cancel is shown. Without a GUI, a logging progress indicator is used.

For a multi-user case:

1. `acquireCaseLock(SHARED)`: the case directory lock, with a 1 min wait. It is kept until close.
2. `acquireCaseResourcesLock(caseDir)`: exclusive, up to 1 h.
3. Run the action (`create` or `open`). Create includes:
   - creating the PostgreSQL case DB via the TSK layer (DB name generated from the display name);
   - `createCaseNodeData`;
   - opening app services (`AutopsyService.openCaseResources`, e.g. creating or opening the Solr core and the Central Repository case row);
   - `openCommunicationChannels`: the remote event channel `"<caseName>-Case-Events"` plus a `CollaborationMonitor`.
   
   Open includes `updateCaseNodeData` (sets `lastAccessDate = now`).
4. Release the resources lock. If the action failed, release the case lock too.

Single-user cases skip all locking.

### 5.2 Close (`doCloseCaseAction`)

It runs on the same executor. It acquires the resources lock (1 h), closes the case (closes app services, the remote channel, the collaboration monitor, and the DB), then **always** releases the case directory lock and shuts the executor down.

### 5.3 Data source deletion

Close the current case (releasing the shared lock). Re-open with an **EXCLUSIVE** case lock, so it fails if anyone else has the case open. Delete the data source, close, and re-open normally with a SHARED lock. It is gated by `FeatureAccessUtils.canDeleteDataSources()` (§9.3).

### 5.4 Case deletion

- **Single-user**: delete the text index (every `KeywordSearchService.deleteTextIndex`), delete the case directory, and remove the case from Recent Cases. Errors are collected, and the user sees "Errors occured while deleting the case. See the application log for details."
- **Multi-user** (`deleteMultiUserCase(metadata, progress)`):
  1. Connect to coordination ("Connecting to coordination service...").
  2. Take an **exclusive, no-wait** lock on the case directory node. If that fails: "Cannot delete case because it is open for another user or host."
  3. Read `CaseNodeData`. Then, each step skipped if its flag is already set:
     - drop the DB: connect to `jdbc:postgresql://host:port/postgres`, `SELECT 1 from pg_database WHERE datname='<db>'`, then `DROP DATABASE "<db>"`, and set `CASE_DB`;
     - delete the text index and set `TEXT_INDEX`;
     - delete the case directory and set `CASE_DIR`;
     - remove from Recent Cases.
  4. Delete the `_RESOURCES` node (ignoring NoNode).
  5. After releasing the lock, if there were no errors, delete the case directory node.
- `Case.deleteMultiUserCase(CaseNodeData, metadata, progress, logger)` is a public `@Beta` entry point reused by auto ingest `DeleteCaseTask`.

### 5.5 Feature gates

See §9.3. "Delete case" and "Add data source" on multi-user cases can be blocked by a marker file.

---

## 6. Event bus (`events/`)

### 6.1 Model

- `AutopsyEvent extends PropertyChangeEvent implements Serializable`. The event *name* is the property name. It has `oldValue` and `newValue`, and `sourceType` ∈ {`LOCAL`, `REMOTE`} (`getSource()` returns the string).
- `LocalEventPublisher`: an in-process pub/sub keyed by event-name strings (`addSubscriber(Set<String>|String, PropertyChangeListener)`).
- `AutopsyEventPublisher` wraps one local publisher plus at most one `RemoteEventPublisher`:
  - `publish(e)` = `publishLocally(e)` + `publishRemotely(e)`.
  - `publishRemotely` is a no-op unless a channel is open. On failure it closes the remote publisher. The next publish tries to reopen it. `MAX_REMOTE_EVENT_PUBLISH_TRIES = 1`, so one attempt per publish. Failures are only logged, and the event is lost.
  - `openRemoteEventChannel(name)` / `closeRemoteEventChannel()`.

### 6.2 Remote transport (`RemoteEventPublisher`)

- ActiveMQ `ActiveMQConnectionFactory(user, password, uri)`, `setTrustAllPackages(true)`. The payload is a Java-serialised object, which is a deserialisation-of-untrusted-data risk.
- One JMS **topic per channel name**. Session `AUTO_ACKNOWLEDGE`, non-transacted. Producer `DeliveryMode.NON_PERSISTENT`, so offline subscribers miss events and there is no replay.
- Message: an `ObjectMessage` with string property `events = 'All'`. The consumer uses selector `events = 'All'` and `noLocal = true`, so a node never receives its own events.
- On receive: if the object is an `AutopsyEvent`, it sets `sourceType = REMOTE` and republishes **locally**. Subscribers cannot tell local and remote apart except via `getSourceType()`.

### 6.3 Channel (topic) names

| Channel | Opened by | Events |
|---|---|---|
| `<caseName>-Case-Events` | `Case` on multi-user open | All `Case.Events` published with `publish()` (below) |
| `<caseName>-Ingest-Job-Events` | `IngestManager.handleCaseOpened` (multi-user) | `IngestJobEvent`: `STARTED`, `COMPLETED`, `CANCELLED`, `DATA_SOURCE_ANALYSIS_STARTED`, `DATA_SOURCE_ANALYSIS_COMPLETED` (a cancelled data source is sent as `DATA_SOURCE_ANALYSIS_COMPLETED` with reason `ANALYSIS_CANCELLED`) |
| `<caseName>-Ingest-Module-Events` | same | `IngestModuleEvent`: `DATA_ADDED`, `CONTENT_CHANGED`, `FILE_DONE`. `FILE_DONE` is sent remotely **once per file**, which is very chatty. |
| `<caseName>-Collaboration-Monitor-Events` | `CollaborationMonitor` | `COLLABORATION_MONITOR_EVENT` |
| `Auto-Ingest-Manager-Events` | every AIN (`AutoIngestManager`) and every dashboard (`AutoIngestMonitor`) | the auto ingest events of §12.6 |

### 6.4 Case events that travel remotely

`Case.Events` published with `publish()` (local + remote):

- `NAME`, `NUMBER`, `EXAMINER`, `CASE_DETAILS`. The code mistakenly publishes the examiner change under `NUMBER`.
- `ADDING_DATA_SOURCE`, `ADDING_DATA_SOURCE_FAILED`, `DATA_SOURCE_ADDED`, `DATA_SOURCE_NAME_CHANGED`, `DATA_SOURCE_DELETED`.
- `REPORT_ADDED`, `REPORT_DELETED`.
- `CONTENT_TAG_ADDED`, `CONTENT_TAG_DELETED`, `BLACKBOARD_ARTIFACT_TAG_ADDED`, `BLACKBOARD_ARTIFACT_TAG_DELETED`, `TAG_DEFINITION_CHANGED`.
- `CR_COMMENT_CHANGED`, `TIMELINE_EVENT_ADDED`.
- `OS_ACCOUNTS_ADDED/UPDATED/DELETED`, `OS_ACCT_INSTANCES_ADDED`.
- `HOSTS_ADDED/UPDATED/DELETED`, `PERSONS_ADDED/UPDATED/DELETED`, `HOSTS_ADDED_TO_PERSON`, `HOSTS_REMOVED_FROM_PERSON`.
- `TAG_NAMES_ADDED/UPDATED/DELETED`, `TAG_SETS_ADDED/DELETED`.

`CURRENT_CASE` is **local only**.

**Lazy hydration pattern**: event classes hold the heavy objects in `transient` fields and serialise only IDs (object id, tag id, lists of ids for `TskDataModelChangedEvent`). On the receiving side, `getNewValue()` reloads the object from the case DB of the *current case* (`getContentById`, `getContentTagByID`, etc.). Because PostgreSQL is shared, every client sees the same rows. postmortem should keep this: events carry IDs, not data.

**Consumers that deliberately ignore remote events**:

- the Central Repository `CaseEventListener` (only LOCAL events update the CR, to avoid double inserts);
- the timeline controller for ingest module events;
- `IngestMonitor` for `CURRENT_CASE`;
- `CommandLineIngestManager`;
- `CollaborationMonitor.LocalTasksManager`.

Most UI tree and result refresh listeners react to both, which is how other users' tags, data sources and new results appear. There is no dedicated "case updated by someone else" banner. The visible collaboration UX is:

- tree and result nodes refresh on remote events;
- the collaboration monitor's progress bars in the status area (§7);
- the services-monitor tray notifications.

---

## 7. Collaboration monitor (`casemodule/CollaborationMonitor.java`)

Purpose: show, in each examiner's status bar, what *other* hosts are doing to the same case (adding a data source, analysing a data source).

- Created when a multi-user case opens. It uses its own channel `<caseName>-Collaboration-Monitor-Events`.
- **Local side** (`LocalTasksManager`) listens to **local** events only:
  - `ADDING_DATA_SOURCE` adds a task with status `"<host> adding data source"`, keyed by the event UUID hash;
  - `ADDING_DATA_SOURCE_FAILED` or `DATA_SOURCE_ADDED` removes that task;
  - `DATA_SOURCE_ANALYSIS_STARTED` adds a task `"<host> analyzing <dataSourceName>"`, keyed by the ingest job id;
  - `DATA_SOURCE_ANALYSIS_COMPLETED` removes it.
  
  After every change, it publishes `CollaborationEvent(hostName, Map<taskId, Task{id, status}>)` remotely. Task ids are a per-host counter.
- **Heartbeat**: every 1 min, it re-publishes the current task map (possibly empty).
- **Remote side** (`RemoteTasksManager`):
  - keeps `host → {lastUpdate, taskId → ProgressHandle}`;
  - for each event, it creates a NetBeans progress handle (titled with the host name, message = status) for each new task, updates existing ones, and finishes handles for tasks no longer listed;
  - **stale detection** every 2 min: if a host has not been heard from for ≥ 5 min (5 missed heartbeats), all its progress bars are finished and the host is dropped.
- Host name: `NetworkUtils.getLocalHostName()`.
- Shutdown: stop executors (wait in 30 s loops), unsubscribe, close the channel, finish all remote bars.

---

## 8. Multi-user case browser & dashboards

### 8.1 Shared browser component (`casemodule/multiusercasesbrowser`)

- `MultiUserCasesBrowserPanel` is an outline view of `MultiUserCaseNode`s. The children come from `CaseNodeDataCollector.getNodeData()` (ZooKeeper CASES nodes), loaded on a background thread.
- A `MultiUserCaseBrowserCustomizer` supplies the columns, sort order, context actions and the preferred (double-click) action.
- Columns (`Column` enum):
  - `DISPLAY_NAME` (always first);
  - `CREATE_DATE`, `DIRECTORY`, `LAST_ACCESS_DATE`;
  - `MANIFEST_FILE_ZNODES_DELETE_STATUS`, `DATA_SOURCES_DELETE_STATUS`, `TEXT_INDEX_DELETE_STATUS`, `CASE_DB_DELETE_STATUS`, `CASE_DIR_DELETE_STATUS`. These are rendered from the `DeletedFlags` bits.

### 8.2 Open Multi-User Case dialog (`OpenMultiUserCaseDialog`, `OpenMultiUserCasePanel`, `OpenMultiUserCaseDialogCustomizer`)

- File → Open Multi-User Case (`OpenMultiUserCaseAction`).
- Columns: Name, Create Date, Directory. Sorted by create date, descending.
- Action: open (`Case.openAsCurrentCase(<caseDir>/<name>.aut)`).

### 8.3 Cases dashboard (Experimental, `CasesDashboardTopComponent`, `CasesDashboardCustomizer`)

- An "administrative dashboard for multi-user cases".
- Columns: Name, Create Date, Last Access Date, Directory, manifest-znodes / text-index / case-DB / case-dir delete status. The data-sources delete status column appears only with extended admin.
- Sorted by display name, ascending.
- Actions: **Open** (preferred), **Open Auto Ingest Log File**. With the `adminext` marker file (§12.10): **Delete Input**, **Delete Output**, **Delete Input and Output**, **Delete Case(s)** (§12.11).
- Tools menu actions: cleanup of orphaned case znodes and orphaned manifest znodes (§12.11).

---

## 9. Small infrastructure packages

### 9.1 `appservices/AutopsyService`

- A service-provider interface for case-scoped application services: `getServiceName()`, `openCaseResources(CaseContext)`, `closeCaseResources(CaseContext)`. Both are default no-ops.
- `CaseContext` holds: the case, a progress indicator, a volatile `cancelRequested` flag (`requestCancel()`), and `isNewCase`.
- `AutopsyServiceException` is the error type.
- Implementations: `SolrSearchService` (KeywordSearch: create/open the Solr core), `CentralRepositoryService`, `ImageWriterService`, `ImageGalleryService`, and `test/TestAutopsyService` (commented out).
- `Case` opens them **sequentially** (service-provider order). Each runs on its own single-thread executor with its own modal, cancellable progress dialog ("<service>: opening case resources"). Exceptions are shown as notifications ("<service> failed to open case resources"), and the case still opens. Closing calls `closeCaseResources` on all of them.
- The presence of `SolrSearchService` is also how `UserPreferences.isMultiUserSupported()` decides whether multi-user is available.

### 9.2 `machinesettings/UserMachinePreferences`

- NbPreferences (per machine, not shared): `TempDirectory` (custom temp path) and `TempDirChoice` ∈ {`CUSTOM`, `SYSTEM`, `CASE`}, default `SYSTEM`.
- Validation of the custom dir: blank is OK (use the default). Otherwise create it if missing (error "There was an error creating the temp directory for path: {0}") and require read and write access ("...error reading or writing to temp directory path: {0}").
- Consumers: the Options → Application panel and `Case.getTempDirectory()`.

### 9.3 `featureaccess/FeatureAccessUtils`

Policy is driven by a marker file `<userdir>/config/mualimit` ("multi-user access limit"). If the file exists, the restricted features are off.

| Check | Rule |
|---|---|
| `canCreateMultiUserCases()` | multi-user enabled AND no `mualimit` |
| `canAddDataSources()` | current case is single-user OR no `mualimit` |
| `canDeleteDataSources()` | a case is open AND the case DB *creation* schema ≥ 8.4 AND (single-user OR no `mualimit`) |
| `canDeleteCurrentCase()` | single-user OR no `mualimit` |
| `canAddHashSetsToCentralRepo()` | no `mualimit` |

### 9.4 `diagnostics/PerformancePanel` (Tools → Performance Diagnostics)

A modal dialog: "This panel performs a series of tests to help identify bottlenecks in the system." Pressing Start runs, sequentially:

1. **CPU**: MD5 over 50 × 255 × 256 KiB of filled buffers. Reports "N MB hashed / sec".
2. **Image reading**: from the last `Image` data source of the current case, reads 4 KiB blocks from a random 512-aligned offset in the first half, until 1000 × 1024 × 1024 bytes are read or EOF. Reports "N MB read / sec (bytes)".
3. **File reading**: shows "Skipped", then still reads the first image's first path through a `FileReader` (chars, not bytes) up to the same limit. This is buggy.
4. **Database**: times `findAllFilesWhere("obj_id < 50000")`. Reports "N records / sec".

Messages: "Case Not Open", "No Images In Case", "Image Path Doesn't Exist". Integer division by elapsed seconds is a bug: anything under 1 s shows 0.

### 9.5 `threadutils/TaskRetryUtil`

`attemptTask(Callable<T>, List<TaskAttempt>, ScheduledThreadPoolExecutor, Terminator, Logger, desc)`:

- Each `TaskAttempt{delay, timeout, unit}` schedules the callable after `delay` and waits `delay + timeout`.
- A null result, an exception or a timeout (timeouts cancel the future with interrupt) moves on to the next attempt.
- A `Terminator.stopTaskAttempts()` check runs before each attempt.
- Returns the first non-null result, or null.
- Global counters: total tasks, retries, attempt timeouts, failed tasks (exposed as getters).
- Used by the embedded file extractor (`FileTaskExecutor`) for file I/O on flaky shares.

### 9.6 Health monitor (infrastructure aspect)

- Enterprise metrics for multi-user clusters. It requires multi-user mode, and the dashboard is covered elsewhere.
- **Storage**: its own PostgreSQL database `HealthMonitor` on the case DB server (created with `CREATE DATABASE "HealthMonitor" OWNER "<user>"`), schema version 1.2:
  - `timing_data(id serial, name, host, timestamp bigint, count bigint, average, max, min double)`;
  - `user_data(id serial, host, timestamp, event_type int, is_examiner bool, case_name, username)`;
  - `db_info(id, name, value)` with rows `SCHEMA_VERSION`, `SCHEMA_MINOR_VERSION`, `MONITOR_ENABLED`.
- **Global enable flag** lives in `db_info.MONITOR_ENABLED` (cluster-wide). Each node polls it.
- Bootstrap (create DB, init or upgrade schema) runs under the ZooKeeper exclusive lock `HEALTH_MONITOR/HealthMonitor` (5 min).
- Uses a DBCP pool of 10 with validation query `SELECT version()`.
- Each node aggregates timing metrics in memory (`TimingMetric` → count/avg/max/min in ms) and user events (`LOG_ON=0`, `LOG_OFF=1`, `CASE_OPEN=2`, `CASE_CLOSE=3`). Case open and close come from `CURRENT_CASE` events. It flushes every **60 minutes**. Host name comes from `InetAddress.getLocalHost()`, or a random UUID on failure.
- APIs: `getTimingMetric(name)` / `submitTimingMetric` / `submitNormalizedTimingMetric(metric, normalization)`. These are no-ops when disabled.

---

## 10. Command-line mode (`commandlineingest/`)

### 10.1 Invocation and detection

- Options are parsed by a NetBeans `OptionProcessor` (`CommandLineOptionProcessor`), so syntax is `--long=value`, `--long value` or `-x value`.
- In `StartupWindowProvider.init()`:
  - CLI mode is chosen if any command option was given, or if the main window is not visible ("headless").
  - If a **default (positional) argument** was given, it is treated as a case path (a `.aut` file, or a directory searched for the first `*.aut`). It is opened in the normal UI (`CommandLineOpenCaseManager`). Used for "Open with" / double-clicking `.aut` files.
  - Otherwise `CommandLineIngestManager.start()` runs commands on a new thread. A `CommandLineStartupWindow` ("Running in Command Line Mode") is shown only when not headless.
- Runtime property "running with GUI" is set to false, so all progress goes to logs.

### 10.2 Options

| Short | Long | Arg | Meaning |
|---|---|---|---|
| `-c` | `--createCase` | none | command |
| `-n` | `--caseName` | required | case base name. **Required for every command** except listing profiles. |
| `-t` | `--caseType` | required | `single` (default) or `multi`. `multi` requires `FeatureAccessUtils.canCreateMultiUserCases()`. |
| `-o` | `--caseBaseDir` | required | parent folder of case folders. **Required**; must exist and be a directory. |
| `-a` | `--addDataSource` | none | command |
| `-s` | `--dataSourcePath` | required | must exist |
| `-k` | `--key` | required | BitLocker key / password for the data source |
| `-i` | `--dataSourceObjectId` | required | existing data source object id (for `--runIngest` without `--addDataSource`) |
| `-r` | `--runIngest` | optional | command; optional value = ingest profile name |
| `-l` | `--listAllDataSources` | none | command |
| `-g` | `--generateReports` | optional | command; optional value = comma-separated report profile names |
| `-p` | `--listAllIngestProfiles` | none | command (needs no case) |

Validation errors throw `CommandException(-2, msg)`. NetBeans prints the message and exits with code `-2` (`CL_PROCESS_FAILURE`). Messages include:

- "Invalid command line, an input option must be supplied."
- "'caseName' argument is empty", "'caseType' argument is invalid"
- "Unable to create multi user case. Confirm that multi user settings are configured correctly."
- "'caseBaseDir' directory doesn't exist or is not a directory: …"
- "Input data source file … doesn't exist"
- "'dataSourceId' argument is empty" (for `runIngest` without `addDataSource` and without an id)
- "Empty report profile name"

### 10.3 Execution order and semantics

Commands are queued in this fixed order regardless of argument order: `CREATE_CASE`, `ADD_DATA_SOURCE`, `RUN_INGEST`, `LIST_ALL_DATA_SOURCES`, `GENERATE_REPORTS` (one command per report profile). `LIST_ALL_INGEST_PROFILES` is exclusive: if present, nothing else runs. `OPEN_CASE_IN_UI` exists in the enum but is unused here.

They run sequentially on one thread. The first failure stops processing. The case is always closed at the end, then the application exits with `0` (`CL_SUCCESS`) or `-1` (`CL_RUN_FAILURE`). Progress lines go to stdout ("Processing 'Create Case' command", ...) as well as the log.

- **CREATE_CASE**:
  - Fails if a case folder for that name already exists in `caseBaseDir`. The match is a folder named `<caseName>_<timestamp>` that contains a `.aut` file ("Case name must be unique").
  - Creates `<caseBaseDir>/<caseName>_<yyyy_MM_dd_HH_mm_ss>` via `Case.createCaseDirectory` + `Case.createAsCurrentCase(type, dir, CaseDetails(caseName))`.
  - Writes `<caseDir>/Command Output/createCase_<ts>.json` = `{"@caseName": ..., "@caseDir": ...}`.
- **Opening an existing case** (all other commands, when not just created): finds the most recently modified matching `<caseName>_<timestamp>` folder and opens `<dir>/<caseName>.aut`. Otherwise it fails with "Case X doesn't exist".
- **ADD_DATA_SOURCE**:
  - Builds `AutoIngestDataSource(randomUUID, path, password)` and asks `DataSourceProcessorUtility.getOrderedListOfDataSourceProcessors(path, password)` for the DSPs that can handle it, in confidence order.
  - Tries each DSP (`notifyAddingDataSource(uuid)` and `process(...)`, waiting on a lock for the callback) until one produces content without `CRITICAL_ERRORS`.
  - Writes `addDataSource_<ts>.json` with one `{"@dataSourceName","@dataSourceObjectId"}` object per content (the image display name from `tsk_image_info.display_name`, else the name).
- **RUN_INGEST**:
  - If no data source was added in this run, it loads `--dataSourceObjectId` via `getContentById`. A missing id returns **without** setting a failure code (bug).
  - Profile: if given, it must match an ingest profile name (case-insensitive) and that profile's file filter must exist among custom plus standard filters. Settings context is then `IngestProfilePaths.getIngestProfilePrefix() + profileName`. Otherwise the context is `"CommandLineModeContext"`.
  - Any settings warnings abort. It starts the ingest job and waits (`wait(60 s)` loop while ingest is running). Cancelled modules are logged. Cancellation for any reason other than user cancel is an error.
- **LIST_ALL_DATA_SOURCES**: writes `listAllDataSources_<ts>.json` with image data sources (display name) and then logical data sources.
- **GENERATE_REPORTS**: runs `ReportGenerator(profileName)` with a CLI progress indicator. The default profile name is `"CommandLineIngest"`.
- **LIST_ALL_INGEST_PROFILES**: prints each `IngestProfile` as JSON (Gson) to stdout between "Listing ingest profiles" and "Ingest profile list complete".

JSON output files go in `<caseDir>/Command Output/`. They use Jackson with a pretty printer that puts newlines between fields. Multiple root values are separated as Jackson does by default.

### 10.4 Settings panel (Tools → Options → Command Line Ingest)

- **Ingest profile**: a combo box, currently disabled, plus a "Configure" button that edits the `CommandLineModeContext` ingest settings in an `IngestJobSettingsPanel` and shows warnings.
- **Report profile**: a combo box with "Default" (maps to `CommandLineIngest`) plus all report configuration names. "Configure" opens the report wizard for the selected profile. Empty names are rejected.

---

## 11. MCP server (`mcp/`) — "AI: MCP Server"

**What it is**: a recent addition. It is a local **Model Context Protocol** server that lets an AI assistant (Claude Desktop or Claude Code) run **read-only queries against the currently open case**. It exposes no write tools.

### 11.1 Lifecycle and transport

- `@OnStart AutopsyMcpModule`: if the NbPreference `MCP_SERVER_ENABLED` (default `false`) is set, it starts the server at app start. It subscribes to `CURRENT_CASE`: a case open sets the query service, and a case close clears it while the server keeps running.
- Options panel (Tools → Options → "AI: MCP Server"):
  - Enable checkbox. **Windows only**; other OSes show "MCP server is only supported on Windows."
  - The location of `bin/autopsy-mcp-stdio.exe` (or "Not found").
  - A copyable Claude config snippet `{"mcpServers":{"autopsy":{"command":"<exe>"}}}`.
  - Enabling or disabling starts or stops the server immediately. A start failure shows "Failed to start the MCP server: {0}" and resets the preference.
- HTTP server: Javalin, bound to **127.0.0.1 only**.
  - Port from `%LOCALAPPDATA%\autopsy\mcp\mcp-config.properties` key `port` (default **8743**). The file is created with a comment if missing. Invalid values fall back to the default.
  - One route, `POST /mcp`. Every request needs `Authorization: Bearer <token>`, else 401 "Unauthorized".
- Auth token: 32 random bytes, Base64url without padding, generated per server start. Written to `%LOCALAPPDATA%\autopsy\mcp\mcp-token`, deleted on stop and on JVM exit. If writing fails, the server is stopped.
- The protocol is plain JSON-RPC 2.0 over a single POST (not MCP's streamable HTTP). Methods:
  - `initialize` returns `{protocolVersion:"2024-11-05", serverInfo:{name:"autopsy-mcp",version:"1.0.0"}, capabilities:{tools:{}}}`.
  - `tools/list` returns **a bare array** of tool definitions (not `{tools:[...]}`). Works without a case.
  - `tools/call` with `{name, arguments}`.
  - Others return `-32601`.
  - Errors: parse error `-32700` (id null); unknown method or tool `-32601`; internal or no-case `-32603`. An uncaught exception gives HTTP 500 with a JSON-RPC error envelope.
- Every tool result except `get_server_status` is wrapped as `{"caseId": <case display name or null>, "result": <payload>}`. Each tool description ends with: "Note: caseId in the response reflects the currently open case — if this changes between calls, alert the user."

### 11.2 Tools

Timestamps are emitted as ISO-8601 UTC strings (`null` for ≤ 0). Date inputs accept ISO-8601 instants or `YYYY-MM-DD`: start bounds expand to `T00:00:00Z`, end and `*Before` bounds to `T23:59:59Z`. SQL filters are built by string concatenation with `'` → `''` escaping.

| Tool | Params (all optional unless marked) | Behaviour / output |
|---|---|---|
| `get_server_status` | — | `{server:"autopsy-mcp", status:"running", caseOpen, caseName?}`. Works without a case and is not wrapped. |
| `get_case_summary` | — | `{caseName, dataSources, totalFiles (countFilesWhere 1=1), totalArtifacts (COUNT(*) blackboard_artifacts)}`. Without a case it returns `{message:"No case is currently open in Autopsy."}` instead of an error. |
| `query_files` | `nameContains`, `extension`, `mimeType` (supports `prefix/*`), `minSize`, `maxSize`, `modifiedAfter/Before`, `createdAfter/Before`, `pathContains` (parent path), `isDirectory`, `allocated`, `knownState` (UNKNOWN/KNOWN/NOTABLE), `md5`, `sha256`, `limit` (default 50, max 500; out of range → 50), `orderBy` (`size_desc/asc`, `name_asc/desc`, `modified_desc/asc`, `created_desc/asc`) | List of files: `objectId, name, path (unique path), parentPath, extension, dataSourceId, size, modifiedTime, accessedTime, changeTime, createdTime, fileType, dirType, metaType, dirFlag, metaFlags, modes, knownState, mimeType, md5Hash, sha256Hash, sha1Hash, uid, gid, ownerUid, metaAddr, metaSeq, attributes[{type,value}]` |
| `query_data_artifacts` | `artifactType` (e.g. TSK_WEB_HISTORY), `attributeType`, `attributeValue` (substring), `dataSourceId`, `limit` (default 50, max 500) | Data artifacts: `objectId, sourceFileId, artifactType, dataSourceId, attributes[{type,value}]`. The tool description lists ~50 data artifact types with their key attributes. Attribute filtering is a post-filter after the SQL `LIMIT`, so it can return fewer results than exist. |
| `query_analysis_results` | same as above | Same, plus `score{significance, priority}`. The description lists ~22 analysis result types. |
| `query_data_sources` | — | Per data source: `objectId, name, type (Java class), size, timezone, deviceId, addedDate`. For images also: `imageType, sectorSize, paths[], md5, sha1, sha256, acquisitionDetails, acquisitionToolName, acquisitionToolVersion`. |
| `get_hosts` | — | `[{id, name, dataSources[...]}]` |
| `get_data_source_tree` | `dataSourceId` | Image → `volumeSystems[{objectId,type,vsType,offset,blockSize, volumes[{objectId,addr,description,startSector,lengthSectors,size,flags, fileSystems[]}]}]` and/or direct `fileSystems[{objectId, fsType, imageOffset, blockSize, blockCount, rootInum, firstInum, lastInum}]`. With one id it returns a single object, else a list. |
| `query_tags` | `tagName` (display name) | `[{tagId, tagName, comment, itemType:"file"|"artifact", objectId, itemName}]` over tag names in use |
| `query_timeline` | `startTime`, `endTime`, `dataSourceId`, `textFilter` (description substring), `limit` (default 100, max 1000) | `[{time, eventType, description (HIGH detail), sourceFileId, artifactId, dataSourceId, tagged, hashHit}]`, all event types, sorted by time |
| `summarize_timeline` | `startTime`, `endTime`, `dataSourceId` | `{startTime, endTime, totalEvents, byCategory{<category display name>: count}}` at the CATEGORY hierarchy level |
| `get_os_accounts` | — | `[{objectId, addr (SID/UID), loginName, fullName, type, status, creationTime, extendedAttributes[{type,value}], instances[{dataSourceId, instanceType}]}]` |
| `get_communications_accounts` | `accountType` (CREDIT_CARD, DEVICE, EMAIL, FACEBOOK, IMO, INSTAGRAM, LINE, MESSAGING_APP, PHONE, SHAREIT, SKYPE, TANGO, TEXTNOW, THREEMA, TWITTER, VIBER, WEBSITE, WHATSAPP, XENDER, ZAPYA), `limit` (default 100, max 500) | `[{accountType, accountId, deviceId, relationshipCount}]` |
| `get_account_relationships` | **`accountType`**, **`accountId`** | `{account, accountType, deviceId, totalRelationships, relatedAccounts[{accountType, accountId, deviceId, relationshipCount}]}`, or `{error:"Account not found: ..."}` |
| `get_file_content` | **`objectId`**, `offset` (default 0), `maxBytes` (default 65536, max 1 048 576) | `{objectId, fileName, fileSize, offset, bytesRead, truncated, content}`. Bytes are decoded as UTF-8 with undecodable bytes replaced by `?`. The description says out-of-range `maxBytes` is rejected; the code silently resets it to 65536. |
| `get_object_children` | **`objectId`** | `{object, parent, childCount, children[]}`. Each entry is a type-specific summary: artifact (artifactType, sourceObjectId, dataSourceId, attributes), file (name, path, size, mimeType, fileType, dirType, isDirectory, modifiedTime, createdTime), image, volumeSystem, volume, fileSystem, else class name + name. |
| `list_reports` | — | `[{objectId, reportName, sourceModule, path, createdTime, size, contentType}]`. `contentType` is `text/html` for `.htm(l)`/`.xhtml`, else `text/plain`. |
| `get_report_content` | **`objectId`**, `offset`, `maxBytes` (as above) | `{objectId, reportName, path, fileSize, contentType, offset, bytesRead, truncated, eof, content}` |

### 11.3 STDIO bridge (`Tools/autopsy-mcp-stdio`)

A Node.js script using `@modelcontextprotocol/sdk`, shipped as a pre-built `autopsy-mcp-stdio.exe`.

- Reads the port from the same properties file and the token from `mcp-token`.
- Proxies `tools/list` (it wraps the bare array as `{tools}`; if Autopsy is unreachable it returns a single placeholder "unavailable" tool) and `tools/call`. The JSON result is returned as one text content item, pretty-printed. Failures become `isError` text ("Autopsy is not reachable: ...").
- 30 s call timeout. Errors are logged to `mcp-stdio.log`, rotated at 1 MiB.
- `--test` mode checks: token present, HTTP reachable, valid JSON-RPC, at least 15 tools, and spot-checks named tools.

---

## 12. Auto ingest (`Experimental/.../autoingest`)

### 12.1 Concepts

- **Input folder** (`AutoModeImageFolder`): a shared folder tree. Acquisition staff drop a data source plus a **manifest file** next to it.
- **Output folder** (`AutoModeResultsFolder`): the shared folder where case directories `<caseName>_<yyyy_MM_dd_HH_mm_ss>` are created. All auto ingest cases are **multi-user** cases.
- **Job** = one manifest = one data source to add to a named case. Many jobs (data sources) usually feed one case.
- **Auto ingest node (AIN)**: a machine in auto ingest mode that runs `AutoIngestManager` (AIM) behind `AutoIngestControlPanel`. It processes **one job at a time**; scale-out means more AINs.
- **Auto Ingest Dashboard (AID)**: an examiner-side view of the cluster (`AutoIngestMonitor` + `AutoIngestDashboard`), via Tools → Auto Ingest Dashboard. It is enabled only when multi-user is enabled.
- The **job registry** is the set of ZooKeeper MANIFESTS nodes (§12.4). There is no central scheduler. Every AIN scans the input folder, builds its own pending list, and claims jobs with an exclusive manifest lock.

### 12.2 Manifest files

- Pluggable via the `ManifestFileParser` service provider (`fileIsManifest(path)`, `parse(path) → Manifest`). The built-in implementation is `AutopsyManifestFileParser`.
- A file is a manifest if its name ends with **`_MANIFEST.XML`** (case-insensitive) and its root element is **`AutopsyManifest`**.
- XML (XPath):
  - `/AutopsyManifest/CaseName` (**required**);
  - `/AutopsyManifest/DataSource` (**required**; a path relative to the manifest's folder);
  - `/AutopsyManifest/DeviceId` (optional; random UUID if empty);
  - `/AutopsyManifest/Password` (optional; e.g. a BitLocker key).
- Malformed XML is retried after cleaning it with JTidy into a temp file.
- `Manifest` = `{filePath, dateFileCreated (file system creation time), caseName, deviceId, dataSourcePath, dataSourceFileName, password, manifestProperties map}`.
- `Tools/ManifestTool` (AutoIt `.au3`/`.exe`, Windows) is a helper GUI that generates manifests for folders of data sources.

Example:

```xml
<AutopsyManifest>
  <CaseName>Operation X</CaseName>
  <DeviceId>laptop-01</DeviceId>
  <DataSource>laptop01.E01</DataSource>
</AutopsyManifest>
```

### 12.3 Job model (`AutoIngestJob`, serialisable, sent over ActiveMQ)

- **Fields**:
  - manifest, node (host) name, case directory path, priority (int, default 0);
  - stage + stage start date, stage details;
  - transient: DSP, ingest job, cancelled and completed flags;
  - completed date, errorsOccurred, processingStatus, numberOfCrashes, dataSourceSize;
  - snapshots of ingest threads, ingest jobs and module run times (for the remote progress dialog);
  - ocrEnabled, password.
- **Equality** is by manifest file path.
- **`ProcessingStatus`**: `PENDING`, `PROCESSING`, `COMPLETED`, `DELETED` (legacy; now used as the "case deleted" marker).
- **`Stage`** (display text):
  - `PENDING` "Pending", `STARTING` "Starting";
  - `UPDATING_SHARED_CONFIG` "Updating shared configuration", `CHECKING_SERVICES` "Checking services";
  - `OPENING_CASE` "Opening case", `IDENTIFYING_DATA_SOURCE` "Identifying data source type";
  - `ADDING_DATA_SOURCE` "Adding data source", `ANALYZING_DATA_SOURCE` "Analyzing data source", `ANALYZING_FILES` "Analyzing files";
  - `EXPORTING_FILES` "Exporting files", `CANCELLING_MODULE` "Cancelling module", `CANCELLING` "Cancelling", `COMPLETED` "Completed".
- **Queue order** (`compareTo`):
  1. higher priority first;
  2. then older manifest creation date first;
  3. then case name, descending;
  4. then data source file name, descending.

### 12.4 `AutoIngestJobNodeData` (MANIFESTS node payload) — binary format

A `ByteBuffer` (big-endian), with a maximum size of 131 641 bytes. Current version **4**.

| Field | Type | Since |
|---|---|---|
| processingStatus | i32 (ordinal) | v0 |
| priority | i32 | v0 |
| numberOfCrashes | i32 | v0 |
| completedDate | i64 ms | v0 |
| errorsOccurred | i32 (0/1) | v0 |
| version | i32 | v1 |
| deviceId | string, u8 length | v1 |
| caseName | string, u8 length | v1 |
| caseDirectoryPath | string, i16 length | v1 |
| manifestFileDate | i64 ms | v1 |
| manifestFilePath | string, i16 length | v1 |
| dataSourcePath | string, i16 length | v1 |
| processingStage | u8 (ordinal) | v1 |
| processingStageStartDate | i64 ms | v1 |
| processingStageDetailsDescription | string, u8 length | v1 |
| processingStageDetailsStartDate | i64 ms | v1 |
| processingHostName | string, i16 length | v1 |
| dataSourceSize | i64 bytes | v2 |
| ocrEnabled | i32 (0/1) | v3 |
| password | string, i16 length | v4 |

Strings are written with the **platform default charset**, and the length prefix is the **character count**, not the byte count. Non-ASCII names corrupt the record (§15). Older-version records are upgraded by the AIM scanner (under the manifest lock) when encountered.

### 12.5 AutoIngestManager (AIM) — the node engine

**Threads**:

- input-scan scheduler (`AIM-input-scan-scheduler`), every `IntervalBetweenInputScan` minutes (default 60), starting immediately;
- input-scan worker (one thread);
- job-processing thread (one);
- job-status publisher, every **10 s**.

**Startup**:

1. Get the coordination service.
2. Open channel `Auto-Ingest-Manager-Events`.
3. Read the input and output roots.
4. Schedule the scans, start job processing and status publishing.
5. Subscribe to remote events, set state `RUNNING`, publish `AutoIngestNodeStateEvent(RUNNING, host)`.

States: `IDLE`, `RUNNING`, `SHUTTING_DOWN`.

**Input scan** (`InputDirScanner`):

- `Files.walkFileTree(inputRoot, FOLLOW_LINKS)`. For each file, the first `ManifestFileParser` that accepts it parses it. Then the MANIFESTS node data for the manifest path is read:
  - **no data** → new job: under the manifest lock (no wait), write fresh node data (`PENDING`), add to pending;
  - `PENDING` → add to pending (upgrade old versions under the lock);
  - `PROCESSING` → `doRecoveryIfCrashed` (below);
  - `COMPLETED` → add to completed, only if the case directory still exists;
  - `DELETED` → ignore.
- At the end, the pending and completed lists are **replaced atomically**, pending is sorted, and `scanMonitor` is notified, which wakes job processing.
- **Crash recovery**: under the manifest lock (no wait), so it is skipped if another node is actually processing:
  - if the node data says `PROCESSING`, the previous holder died. Set `errorsOccurred` (and the case's `CaseNodeData.errorsOccurred` if the case dir exists) and `numberOfCrashes++`;
  - if `numberOfCrashes < MaxNumTimesToAttemptToProcessImage`: back to `PENDING` and log "Detected crash while processing, reprocessing";
  - else: `COMPLETED` and log "...reached retry limit for processing".

**Job processing loop**:

1. `waitForInputDirScan`: honour a pause request, then block until the next scan completes.
2. `processJobs`: `dequeueAndLockNextJob` repeatedly.
3. Dequeue: first try with `enforceMaxJobsPerCase = true`, then without. For each pending job in order:
   - try the exclusive manifest lock (no wait), skipping if it is held;
   - re-read node data and drop the job if it is not `PENDING` anymore;
   - drop it if the manifest file is gone;
   - if enforcing, skip when the number of *remote* running jobs for the same case ≥ `MaxConcurrentNodesForOneCase` (default 3);
   - otherwise this becomes `currentJob` (rebuilt from fresh node data).
4. The manifest lock is held for the entire job.

**`processJob`**:

1. Set `PROCESSING`, stage `STARTING`, host = local. Write node data. Publish `AutoIngestJobStartedEvent`.
2. `attemptJob`:
   1. `CHECKING_SERVICES`: `ServicesMonitor` DB and KWS must be UP, otherwise `DatabaseServerDownException` / `KeywordSearchServerDownException`.
   2. `OPENING_CASE`: take the exclusive **case name** lock (30 min wait). Find the newest `<caseName>_<ts>` folder containing a `.aut`, and open it. If there is none:
      - create the case directory;
      - `Server.selectSolrServerForCase` (if `<outputRoot>/solrServerList.txt` exists, pick a random `host,port` line and write it to `<caseDir>/solrserver.txt`);
      - create the multi-user case;
      - sleep `SleepBetweenCasesTime` s (default 30) to let services settle.
      
      Record the case dir in node data. Append the manifest path to `<caseDir>/auto-ingest-job-manifests.txt`.
   3. `UPDATING_SHARED_CONFIG` if shared config is enabled: `SharedConfiguration.downloadConfiguration()`.
   4. Ingest:
      - `IDENTIFYING_DATA_SOURCE`: a missing data source sets errors and logs "Data source file not found"; the job completes.
      - Extensions `xry` and `dar` are skipped ("File type can not currently be processed").
      - `ADDING_DATA_SOURCE`: DSPs in confidence order. If the DSP supports **ingest streams**, ingest settings (`AutoModeContext`) are validated first and ingest starts while adding. Otherwise it is the plain `process` + wait. Fall through to the next DSP on critical errors or no content. If all fail, `AutoIngestDataSourceProcessorException` (which pauses the node).
      - `ANALYZING_DATA_SOURCE`: start the ingest job with the `AutoModeContext` settings. The job's `ocrEnabled` is pushed into the keyword search module's settings. Wait (1 min polling) until ingest stops. Cancelled modules mark errors. Non-user cancellation is an `AnalysisStartupException`. Then sleep `SleepBetweenCasesTime` again.
      - Metrics: sum the data source content sizes into `dataSourceSize` and write node data.
      - `EXPORTING_FILES`: run the File Exporter if enabled (§12.12). Export errors only mark errors; settings errors pause.
   5. Always close the case.
3. Finally:
   - completed or cancelled → `COMPLETED` + completedDate. Otherwise (exception) → `PENDING`, so the job is retried after the node resumes.
   - Clear the host and write node data. On cancel, log "Auto ingest job cancelled during processing" and set the case errors flag.
   - Publish `AutoIngestJobCompletedEvent(job, retry)`. Release the manifest lock.

**Error handling, "pause for system error"**: any exception out of `processJobs` maps to an `ErrorState`:

| ErrorState | Caused by |
|---|---|
| `COORDINATION_SERVICE_ERROR` | coordination service failure or invalid node data |
| `SHARED_CONFIGURATION_DOWNLOAD_ERROR` | shared config download |
| `SERVICES_MONITOR_COMMUNICATION_ERROR` | services monitor |
| `DATABASE_SERVER_ERROR` | DB down |
| `KEYWORD_SEARCH_SERVER_ERROR` | KWS down |
| `CASE_MANAGEMENT_ERROR` | case create, open or close |
| `ANALYSIS_STARTUP_ERROR` | ingest start |
| `FILE_EXPORT_ERROR` | file exporter |
| `JOB_LOGGER_ERROR` | case job log |
| `DATA_SOURCE_PROCESSOR_ERROR` | DSP |
| `JOB_METRICS_COLLECTION_ERROR` | metrics |
| `UNEXPECTED_EXCEPTION` | anything else |

The node then **pauses** (`PAUSED_FOR_SYSTEM_ERROR` published) until a user resumes it. The control panel shows the matching message, e.g. "Paused, unable to communicate with case database service.".

**Pause / resume**:

- `pause()` sets `pauseRequested` and publishes `PAUSE_REQUESTED`. The pause takes effect between jobs ("Pausing after current job completes..."), or immediately if idle (`PAUSED_BY_USER_REQUEST`).
- `resume()` publishes `RESUMED` and notifies.
- Scheduled pause windows (`ScheduledIngestPauseSettings`: enabled, day of week, start hour and minute, duration in minutes, default 60) pause the *ingest pipeline* (`IngestPipeline`) rather than the AIM.

**Cancellation**:

- `cancelCurrentJob()` sets the job cancelled flag and cancels the ingest job.
- `cancelCurrentDataSourceLevelIngestModule()` sets stage `CANCELLING_MODULE` and cancels the running data-source module only.

**Shutdown**: publish `SHUTTING_DOWN`, stop scans (30 s waits), cancel the current job, stop processing, publish `SHUTDOWN`, close the channel, clear lists.

**Local admin operations on the AIN** (control panel buttons). Each writes node data and publishes an event:

- `prioritizeCase` / `prioritizeJob`: new priority = max pending priority + 1; publishes `CASE_PRIORITIZED`/`JOB_PRIORITIZED`.
- `deprioritize*`: priority back to 0.
- `reprocessJob`: a completed job goes back to `PENDING`, errors cleared, completedDate 0, stage `PENDING`.

### 12.6 Auto ingest events (channel `Auto-Ingest-Manager-Events`)

`AutoIngestManager.Event` names:

`INPUT_SCAN_COMPLETED, JOB_STARTED, JOB_STATUS_UPDATED, JOB_COMPLETED, CASE_PRIORITIZED, CASE_DELETED, PAUSE_REQUESTED, PAUSED_BY_USER_REQUEST, PAUSED_FOR_SYSTEM_ERROR, RESUMED, STARTING_UP, RUNNING, SHUTTING_DOWN, SHUTDOWN, REPORT_STATE, CANCEL_JOB, REPROCESS_JOB, GENERATE_THREAD_DUMP_RESPONSE, OCR_STATE_CHANGE`

| Event class | Name | Payload | Sent by → handled by |
|---|---|---|---|
| `AutoIngestJobStartedEvent` | JOB_STARTED | job | AIN → AINs (remove from pending, record as running on that host) and AIDs |
| `AutoIngestJobStatusEvent` | JOB_STATUS_UPDATED | job incl. ingest snapshots | AIN every 10 s while busy → AINs/AIDs |
| `AutoIngestJobCompletedEvent` | JOB_COMPLETED | job, `retry` flag | AIN → AINs (add to completed unless retry) / AIDs |
| `AutoIngestCasePrioritizedEvent` | CASE_PRIORITIZED | host, caseName, user, `EventType{CASE_PRIORITIZED, CASE_DEPRIORITIZED, JOB_PRIORITIZED, JOB_DEPRIORITIZED}`, dataSource | AIN/AID → others trigger a rescan or refresh |
| `AutoIngestOcrStateChangeEvent` | OCR_STATE_CHANGE | host, caseName, user, `{OCR_ENABLED, OCR_DISABLED}` | AID → AINs rescan |
| `AutoIngestCaseDeletedEvent` | CASE_DELETED | caseName, host, user | (never sent, see §12.11) → handlers rescan/refresh |
| `AutoIngestNodeStateEvent` | one of the state names | eventType, nodeName | AIN on every state change → AIDs |
| `AutoIngestRequestNodeStateEvent` | REPORT_STATE | — | AID → all AINs re-publish their last state event |
| `AutoIngestNodeControlEvent` | PAUSE / RESUME / SHUTDOWN / GENERATE_THREAD_DUMP_REQUEST | targetNode, originatingNode, user | AID → target AIN (host name compared case-insensitively) |
| `ThreadDumpResponseEvent` | GENERATE_THREAD_DUMP_RESPONSE | originalNode, targetNode, dump text | AIN → requesting AID. It writes `<logdir>/ThreadDumpFromNode_<node>_<ts>.txt` and opens it. |
| `AutoIngestJobCancelEvent` | CANCEL_JOB | job, node, user | AID → AIN running it |
| `AutoIngestJobReprocessEvent` | REPROCESS_JOB | job, node, user | AID → AINs (move completed → pending) |

**Remote-node liveness in an AIN**: `hostNamesToLastMsgTime` is updated on any event from a host. A remote running job is dropped if no update arrives for 10 × 10 s = **100 s**.

### 12.7 AutoIngestMonitor (dashboard back end)

- Opens the same channel and subscribes to job, state, thread-dump and OCR events.
- **State refresh task**, every **5 minutes** and on demand:
  - rebuild the snapshot from ZooKeeper (all MANIFESTS nodes, v ≥ 1; `PENDING` → pending, `PROCESSING` → running, `COMPLETED` → completed, `DELETED` → ignored);
  - then publish `REPORT_STATE` to collect node states.
- Event handling: started → move to running; status → update stage, snapshots and status of the running entry; completed → move to completed; prioritized, deleted or OCR → schedule a refresh.
- **Node states**: `AutoIngestNodeState{name, State, lastSeen}` with `State` ∈ {`STARTING_UP`, `SHUTTING_DOWN`, `RUNNING`, `PAUSE_REQUESTED`, `PAUSED_BY_REQUEST`, `PAUSED_DUE_TO_SYSTEM_ERROR`, `UNKNOWN`}. `SHUTDOWN` removes the node. Only nodes seen in the last **15 minutes** are shown.
- **Admin operations from the AID**:
  - prioritize or deprioritize case or job, enable or disable OCR for a case: **read-modify-write of node data without a lock**, then a remote event;
  - cancel job (remote event to the owning AIN);
  - reprocess job (rewrite node data to `PENDING`, then event);
  - pause, resume, shutdown, thread dump of a node (control events).
  - Events are published from throwaway threads.

### 12.8 Dashboards and control panel UI

**Auto Ingest Dashboard** (`AutoIngestDashboardTopComponent`, opened from Tools menu position 106, enabled only when multi-user is on):

- Three tables: **Pending Jobs**, **Running Jobs**, **Completed Jobs**. Plus "Services Status: Case databases {up|down|unknown}, keyword search …, coordination …, messaging …".
- Refreshed every **30 s** after the first update, and on monitor notifications.
- Columns (`AutoIngestJobsNode`):
  - Pending: Case Name, Data Source, Job Created, Prioritized (Yes/No), OCR;
  - Running: Case Name, Data Source, Host Name, Stage, Time in Stage (dd:hh:mm:ss);
  - Completed: Case Name, Data Source, Job Created, Job Completed, Status (OK / error icon), OCR.
- Context actions, **admin only**:
  - pending: Prioritize Job, Prioritize Case, Deprioritize Job, Deprioritize Case, Enable OCR For This Case, Disable OCR For This Case;
  - running: Ingest Progress (a remote snapshot dialog of thread activity, jobs and module run times), Generate Thread Dump, Cancel Job (with confirmation);
  - completed: Reprocess Job, Show Case Log.

**Auto Ingest Nodes** (`AinStatusDashboardTopComponent`):

- Table of Host Name and Status: "Starting Up", "Running", "Pause Requested", "Paused By User", "Paused Due to System Error", "Shutting Down", "Unknown".
- Buttons: Refresh, Auto Ingest Metrics, Health Monitor.
- Admin actions: Pause Node / Resume Node, Shutdown Node (with consequences warning), Generate Thread Dump.

**Auto Ingest Metrics** (`AutoIngestMetricsDialog`): pick a date, then "Generate Metrics Report". Over all COMPLETED or DELETED manifest nodes with `completedDate >= date`, it shows "Since <date>: Number of Jobs Completed: N, Total Size of Data Sources: X GB". GB is 2^30 bytes here; postmortem should show SI GB per ADR-005.

**AutoIngestControlPanel** (the AIN's own window, used instead of the startup window in AUTOINGEST mode):

- The same three tables with columns: Case, Data Source, Host Name, Job Created, Stage Started, Job Completed, Stage, Time in Stage, Status, Case Folder, Local Job?, Manifest File Path, Prioritized, OCR.
- Buttons:
  - Start ("Waiting to start");
  - Pause/Resume, which confirms "Pause will occur after the current job completes processing. This could take a long time. Continue?";
  - Refresh, Options (opens the auto ingest options; only while paused);
  - Prioritize/Deprioritize Case/Job, Ingest Progress, Cancel Job, Cancel Module, Reprocess Job, Show Case Log;
  - Open System Logs Folder, Auto Ingest Metrics;
  - Exit, which warns "This will cancel any currently running job on this host...".
- Status line: Running / Paused / pause reason. Services status line includes coordination.
- On startup failure: "Failed to start automated ingest. Verify Multi-user Settings."

### 12.9 Logs

| Log | Location | Content |
|---|---|---|
| Case auto ingest log | `<caseDir>/auto_ingest_log.txt` | Line format `"%-8s: %s %s: %s: %s: %s"` = category (`INFO`/`WARNING`/`ERROR`), `yyyy/MM/dd HH:mm:ss`, host, manifest file name, data source file name, message. Each write holds the ZooKeeper lock `CASES/<caseDir>/AUTO_INGEST_LOG.TXT` (15 min). Messages include: "Data source file not found", "Failed to extract data source from archive", "Using data source processor: X", "Added data source to case", "Failed to add data source to case", "Data source failed to produce content", "Failed to analyze data source due to settings errors / ingest module startup errors / ingest job startup error", "Analysis of data source completed", "<module> analysis of data source cancelled", "Analysis of data source cancelled", "Automated file export completed", "Error exporting files", "Detected crash while processing, reprocessing", "…reached retry limit for processing", "Auto ingest job cancelled during processing", "File type can not currently be processed". |
| AIN system log | `<userdir>/var/log/auto_ingest.log` (rotating, about 50 MB × 10) | AIM internals |
| Dashboard log | `<userdir>/var/log/auto_ingest_dashboard.log` (about 50 MB × 10) | AID actions, deletions |
| Case manifests list | `<caseDir>/auto-ingest-job-manifests.txt` | One manifest path per line, appended on each job. Used by case deletion. |

### 12.10 Admin access gating (marker files in `<userdir>/config/`)

- `admin`: enables admin actions in the AID and the nodes dashboard.
- `adminext`: implies admin, and additionally enables the destructive Cases-dashboard deletes and the data-sources delete-status column.
- `mualimit`: see §9.3.

### 12.11 Case deletion with auto ingest (`DeleteCaseTask`)

| Option | Menu item | Deletes |
|---|---|---|
| `DELETE_INPUT` | Delete Input | manifest files + data source files |
| `DELETE_OUTPUT` | Delete Output | case DB, text index, case directory, case znodes, manifest znodes (only where the input was deleted) |
| `DELETE_INPUT_AND_OUTPUT` | Delete Input and Output | both |
| `DELETE_CASE` | Delete Case(s) | output, and marks manifest znodes `DELETED` (so the jobs are not re-queued while the input remains) |

Algorithm (each step checks for interruption):

1. Exclusive **case name** lock (no wait). If it fails, return.
2. Exclusive **case directory** lock (no wait). If it fails, return.
3. Manifest paths come from `auto-ingest-job-manifests.txt`, or, if that file is missing, by scanning all MANIFESTS nodes whose `caseName` matches. Lock each with a 5 min wait. If any lock fails, release all and abort. If there are no manifests, set the `MANIFEST_FILE_NODES` flag.
4. If the case dir exists and `.aut` metadata is readable:
   - for INPUT options: open the case DB directly and, for each manifest, delete the data source files (for an image, all segment paths of the image whose path equals the manifest's data source) and the manifest file (3 tries, 1 s apart). Set `DATA_SOURCES` if all succeed;
   - for `DELETE_CASE`: mark manifest node data `DELETED`;
   - for OUTPUT, BOTH and CASE: `Case.deleteMultiUserCase(...)` (DB, index, directory, with flags).
   
   If the case dir is missing, set the `CASE_DIR` flag.
5. Delete the `_RESOURCES` and `AUTO_INGEST_LOG.TXT` nodes (ignoring NoNode).
6. OUTPUT and BOTH: release each manifest lock and delete the manifest node if its input was deleted. Set the flag if all were deleted.
7. Delete the case-directory node only if all relevant flags are set (`DATA_SOURCES`, `CASE_DB`, `TEXT_INDEX`, `CASE_DIR`, `MANIFEST_FILE_NODES` for OUTPUT and BOTH; all except `DATA_SOURCES` for CASE).
8. Delete the case-name node (OUTPUT, BOTH, CASE).
9. (`AutoIngestCaseDeletedEvent` / `CASE_DELETED` exists and is handled by AINs and AIDs, but **no code constructs it**. After a deletion, other nodes only notice at their next scan or refresh.)

**Orphan cleanup** (Tools menu, admin):

- *Cleanup Case Znodes*: case-dir nodes whose directory no longer exists. It shows the list in a dialog ("Znodes found: N") and, on OK, deletes the name, resources, log and directory nodes.
- *Cleanup Manifest File Znodes*: MANIFESTS nodes whose manifest file no longer exists are deleted without a prompt.

### 12.12 Automated File Exporter (`FileExporter*`, `FileExportRuleSet`)

Optional post-ingest step that copies files matching user rules into an export area with JSON catalogs. Used for downstream triage pipelines.

- **Settings** (`<userdir>/config/Automated File Exporter/fileexporter.settings`, Java-serialised):
  - enabled flag;
  - files root dir and reports root dir;
  - master catalog name (default `interim`);
  - export-completed flag file name (`EXTRACTION_FINISHED`);
  - rules-evaluated flag file name (`SORTER_FINISHED`);
  - named rule sets.
- **Prerequisite**: the File Type Identification and Hash Lookup ingest modules must have been enabled for the data source (checked via ingest job info). Otherwise `FileExportException`, which pauses the node.
- **Rules**: a rule = name + optional MIME type condition (`files.mime_type = 'x'`) + size conditions (op ∈ `= <= < >= > !=`, units Bytes/KB/MB/GB/TB/PB as 1024 multiples) + artifact conditions (artifact type + attribute type + op + typed value: int32, int64, double, text, bytes as hex, datetime). Each rule compiles to one SQL query over `tsk_files` joined with `blackboard_artifacts`/`blackboard_attributes` aliases. Matching file ids are accumulated with the list of rule names.
- **Output layout per device id**:
  - `<filesRoot>/<deviceId>/<mime with / → ->/<md5[0:2]>/<md5[2:4]>/<md5[4:6]>/<md5[6:8]>/<MD5>` (deduplicated by MD5);
  - `<reportsRoot>/<deviceId>/<Catalog>/<catalog>.json` (master catalog) plus per-rule catalogs;
  - flag files written at the end.
  
  Both device folders are wiped first. NTFS/FAT metadata files (`$…:` with meta address < 32 in the root) are excluded.
- **Catalog entry**: `{ "<MD5>": { "Filename", "Type", "MD5", "File data": { "Modified":[], "Changed":[], "Accessed":[], "Created":[], "Extension":[], "Filename":[], "Size":[], "Source Path":[], "Flags (Dir)":[], "Flags (Meta)":[], "Mode":[], "User ID":[], "Group ID":[], "Meta Addr":[], "Attr Addr":[], "Dir Type":[], "Meta Type":[], "Known":[] }, "<ARTIFACT_TYPE>": { "<ATTR_TYPE>": [values] } … } }`. Root values are separated by CRLF.

### 12.13 Single-user case import (`SingleUserCaseImporter`, `CaseImportPanel`)

- Options panel "Case Import". It requires multi-user enabled and the auto ingest cluster joined.
- Walks a folder for `.aut` cases. For each case:
  - picks unique output names (adding `_N` before the timestamp);
  - optionally copies the source images;
  - converts the case with `SingleUserCaseConverter` (SQLite → PostgreSQL, rewriting paths);
  - copies `auto_ingest_log.txt`;
  - optionally deletes the source case.
- Writes an import log in the output folder.

---

## 13. Auto ingest configuration (`Experimental/.../configuration`)

### 13.1 Mode selection

- Options → "Auto Ingest" panel (`AutoIngestSettingsPanel`), with radio buttons for **Standalone / Review** mode and **Automated ingest** mode (`OptionsUiMode` `STANDALONE`, `AIM`, `DOWNLOADING_CONFIGURATION`).
- Saving sets `JoinAutoModeCluster` and `UserPreferences.setMode(AUTOINGEST|STANDALONE)`. Changing it needs a restart.
- `experimental/configuration/StartupWindow`: in AUTOINGEST mode the startup window *is* the `AutoIngestControlPanel` (closing it shuts AIM down). Otherwise it shows the normal welcome banner.

### 13.2 `AutoIngestUserPreferences`

Stored in the `AutoIngest` module settings file. Every key can be **overridden by a JVM system property** `AutoIngest.<key>`.

| Key | Default | UI (Advanced panel range) |
|---|---|---|
| `JoinAutoModeCluster` | false | mode radio |
| `AutoModeImageFolder` | "" | input folder |
| `AutoModeResultsFolder` | "" | output folder |
| `SharedSettingsFolder` | "" | shared config folder |
| `SharedSettingsEnabled` | false | "Use shared configuration" |
| `SharedSettingsMaster` | false | "Use this node as a master node that can upload settings" |
| `SleepBetweenCasesTime` | 30 s | "System synchronization wait time" 30–3600, step 10 |
| `ShowToolsWarning` | true | — |
| `MaxNumTimesToAttemptToProcessImage` | **0** in code; the spinner defaults to 2 | "Maximum job retries allowed" 0–9 999 999 |
| `MaxConcurrentNodesForOneCase` | 3 | "Target concurrent jobs per case" 1–100 |
| `IntervalBetweenInputScan` | 60 min | "Interval between input scans" 1–100 000 (restart required) |

The ingest settings context name is `"AutoModeContext"`.

The Advanced panel also edits:

- number of file ingest threads: choices `{1,2,4,6,8,12,16}`, capped by CPU count tiers (≥16 cores: up to 16; 12–15: 12; 8–11: 8; 6–7: 6; 4–5: 4; 2–3: 2; else 1);
- external process timeout (enable + hours, 1–100 000, default 60);
- the scheduled ingest pause (enable, day of week, start time hh:mm, duration in hours).

**Validation** (AIM mode):

- input and output folders must be non-empty, exist (or be symlinks or UNC), and be readable and writable by the current user ("Cannot access … check permissions for user X");
- the shared settings folder must be valid if shared config is enabled;
- shared config requires multi-user enabled ("Multi-user cases must be enabled").
- A **"Test"** button runs `MultiUserTestTool` against the output folder.

### 13.3 Shared configuration (`SharedConfiguration`)

Lets one **master** node publish its configuration to a shared folder, which every AIN downloads before **each job**.

- **Upload** (master only, "Save & Upload Config"):
  - exclusive lock `CONFIG/<sharedFolder>` (30 min). If it cannot be acquired, `LOCKED` ("Shared configuration folder is currently locked by another node - try again in a few minutes").
  - The local `AutoModeContext` ingest settings must exist ("Local configuration has not been initialized...").
  - Create the marker `uploadInProgress` in the shared folder, flush preferences, then copy:
    1. `IngestSettings/AutoModeContext/` folder + `AutoModeContext.properties`;
    2. enabled-modules settings;
    3. file type definitions (`UserFileTypeDefinitions.settings` / legacy `.xml`);
    4. interesting files sets (`InterestingFileSets.settings` / legacy `InterestingFilesSetDefs.xml`);
    5. keyword search (`keywords.settings`, legacy `keywords.xml`, `KeywordSearch*.properties`);
    6. extension mismatch (`mismatch_config.settings`/`.xml`);
    7. `AndroidTriage_Options.properties`;
    8. multi-user and general settings: `ViewPreferences.properties`, `MachineSpecificPreferences.properties`, `ModePreferences.properties`, `ExternalServicePreferences.properties`, `AutoIngest.properties`;
    9. hash DBs (below);
    10. the File Exporter folder;
    11. `CentralRepository.properties`;
    12. object detection classifiers folder;
    13. user Python modules folder;
    14. the YARA folder.
  - Delete the marker.
- **Hash DB sharing**:
  - `hashLookup.settings` (and legacy `hashsets.xml`) are copied.
  - Each referenced DB file (plus `-md5.idx` and `.idx2`) is copied to `<shared>/hashDb/<local path with ":" → "__colon__">` only if its **CRC32** differs from `SharedConfigVersions.txt`, which is rewritten both locally and remotely.
  - Download is the reverse: a DB is copied when it has no local versions entry, its CRC differs, or the local file is missing. It is copied back to the **same absolute path it had on the master** (identical layout is assumed). Hash DBs loaded at that path are closed before being overwritten, and the hash DB manager is reloaded afterwards.
- **Download** (every AIN, every job, and the "Download Config" button):
  - shared lock (30 min). Fails if `uploadInProgress` exists ("…is corrupt - re-upload configuration") or the folder is not populated.
  - Saves non-shared local values (master flag, shared folder, tools warning, local-time display, hide known/slack files, keep preferred viewer, **file ingest threads**), downloads everything in the same order, reloads preferences, restores those local values, then downloads hash DBs.
  - Afterwards it verifies KWS, DB and messaging are UP and that the input and output folders are read/write, else `SharedConfigurationException` (the node pauses).
- When copying, a file missing on the source side deletes the destination copy (for optional files). Folder copies wipe the destination first.

### 13.4 MultiUserTestTool ("Test" button)

Returns `"Success"` or the first error string. Steps:

1. ServicesMonitor DB, KWS and messaging are UP ("<service> is down" / "Unable to check service status").
2. Create the multi-user case `Test_Multi_User_Settings_<ts>` in the output folder.
3. `select * from tsk_db_info` must return a row.
4. Write `%TEMP%/AutopsyTempFile_<ts>.txt`, add it with `LocalFilesDSProcessor`, and check for content and no critical errors.
5. Find the file through `FileManager`.
6. Detect its MIME type and `KeywordSearchService.index(file)` (proves the Solr core is writable).
7. Run ingest with the `AutoModeContext` settings and report startup errors.
8. Always delete the test case.

---

## 14. Test infrastructure, CI, platforms, release

### 14.1 Integration testing (`integrationtesting/`)

YAML-snapshot tests that run Autopsy end to end, driven by JSON configuration:

1. For each test suite, create a case (single-user, multi-user or both).
2. Apply configuration modules.
3. Ingest the listed data sources.
4. Run every selected `IntegrationTestGroup`'s `@IntegrationTest` methods.
5. Serialise the return values to YAML (`<rootTestOutputPath>/<suite>-<caseType>.yml`, roughly).
6. Optionally diff against gold YAML (`DiffService`, unified diff to `diffOutputPath`).

- **Entry point**: `Core/test/qa-functional/.../integrationtesting/TestRunner` (NbJUnit). It is run with `ant -f Core integration-test -Dintegration-test.configFile=config.json`, or with every key as `-Dintegration-test.<key>`. Ant maps these to JVM system properties.
- **EnvConfig** (`envconfig.schema.json`):
  - required: `rootTestOutputPath`, `rootTestSuitesPath`, `rootCaseOutputPath`, `workingDirectory` (relative paths resolve against it; defaults to the config file's folder);
  - optional: `diffOutputPath`, `rootGoldPath`, and `dbConnection`, `mqConnection`, `solrConnection`, `zkConnection`, `crConnection`, each a `connectionconfig{hostName (required), userName, password, port}`.
- **Test suite JSON** (a file holds one object or an array):
  - required: `name`, `caseTypes` (`singleUser`/`multiUser`/`both`), `dataSources[]`, `integrationTests`;
  - optional: `description`, `configurationModules[]`.
  - `integrationTests` takes either `excludeAllExcept: parameterizedresource[]` or `includeAllExcept: string[]`.
  - A `parameterizedresource` is either a class-name string or `{resource, parameters}`. Parameters are deserialised with Jackson into the resource's parameter type.
- **`ConfigurationModule<T>`** (experimental): `IngestJobSettings configure(IngestJobSettings current, T params)` and `void revert()`. The built-in `IngestModuleSetupManager` takes `{modules: [factory class names]}` and enables exactly those ingest modules. With no modules, the default settings are used.
- **`IntegrationTestGroup`** (a service provider) has `setupClass`/`tearDownClass`/`setup`/`tearDown`. Its `@IntegrationTest` public methods return POJOs that are serialised (getters). Exceptions are serialised as `ExceptionObject`.
- Example group in the tree: `datasourcesummary.UserActivitySummaryTests`.

### 14.2 Test utilities (`testutils/`, `test/`)

- `CaseUtils.createAsCurrentCase(name)` (single-user in a temp directory) and `closeCurrentCase()`.
- `IngestUtils.addDataSource(dsp, path)`, `runIngestJob(dataSources, settings)`, `getIngestModuleTemplate(factory)`.
- `DataSourceProcessorRunner.runDataSourceProcessor(dsp, path)` blocks until the DSP callback fires.
- `IngestJobRunner.runIngestJob(...)` blocks until the ingest job completes and ignores remote events.
- `test/` contains dev-only ingest modules: `CustomArtifactsCreator*` creates one of every custom artifact and attribute type, `InterestingArtifactCreator*` creates interesting artifact hits, and `TestAutopsyService` is a disabled no-op case service. They are used to exercise viewers and reports.

### 14.3 Test tiers

| Tier | Where | Runner | Notes |
|---|---|---|---|
| Unit | `Core/test/unit/src` (about 15 classes: discovery domain search cache, data source summary, external viewer settings, translators, Tsk mocks) and `KeywordSearch/test/unit` | NbJUnit + Mockito, `ant -f Core/build.xml test-unit` | Runs in CI |
| Functional ("qa-functional") | `Core/test/qa-functional/src` (about 17 classes: Central Repository datamodel and personas/accounts, common-properties search, ingest file filters, embedded file extraction, encryption detection, integration `TestRunner`) | NbJUnit with a real NetBeans runtime, `ant test-qa-functional`. Test data is downloaded into `Core/test/qa-functional/data` (`getTestDataFiles`). | Needs a sleuthkit build. CR tests can use PostgreSQL. |
| UI regression | `Testing/test/qa-functional/.../RegressionTest.java` | Jemmy/Jelly UI automation: new case wizard, add image or logical files, configure ingest, hash and keyword settings, ingest, expand tree, generate reports | Driven by the Python harness below |
| Gold-standard regression | `test/script/regression.py` (+ `regression_utils.py`, `config.xml`, `Emailer.py`, `getcounts.py`) | Python 3 under Cygwin. `ant test-download-imgs` fetches images into `test/input`. Options: `-f file`, `-r rebuild gold`, `-b both`, `-l config`, `-u ignore unallocated`, `-k keep Solr index`, `-o diff output`, `-v`, `-e exception mode`. | Runs ingest per image (single- and multi-user), dumps the case DB, and diffs it with `tskdbdiff` (from `$TSK_HOME/db_diff`, supports PostgreSQL) against zipped gold DBs and error logs. Produces an HTML report. |

### 14.4 CI

- **GitHub Actions** `build-windows.yml`, on every push and PR, on `windows-2025`:
  - Zulu JDK 21, MSBuild, Chocolatey nuget and ant;
  - clone sleuthkit `develop-4.1x`, build native libtsk, build Java bindings and case-uco;
  - `ant build`, then `ant -f Core/build.xml test-unit` and `ant -f KeywordSearch/build.xml test-unit`.
- **AppVeyor** (legacy): VS2019 x64, JDK 17, PostgreSQL 9.6 service, NetBeans platform 15. Build plus Core unit tests.
- **Travis** (legacy): Ubuntu bionic and macOS (xcode12.2), JDK 17, apt or Homebrew dependencies (libewf, afflib, libvhdi, libvmdk, testdisk, libheif, gstreamer, `postgresql@15` on macOS), `travis_build.sh`.

### 14.5 Platforms and release

- **Windows x64** is the primary platform:
  - MSI installer built by `release_scripts/build_windows_installer.ps1` from an Advanced Installer project (`installer_autopsy/installer_autopsy.aip`), producing `dist/autopsy-X.Y.Z-64bit.msi`;
  - a bundled JRE;
  - Windows-only features: the MCP server and stdio exe, the ManifestTool (AutoIt), and several bundled Windows executables. Multi-user deployment documentation assumes Windows servers and UNC shares.
- **Linux (Debian/Ubuntu) and macOS**:
  - the release zip plus `linux_macos_install_scripts/`:
    - `install_prereqs_ubuntu.sh` (apt: openjdk-17, build tools, libpq, testdisk, libewf/afflib/libvhdi/libvmdk/libvslvm, libheif/libde265, gstreamer);
    - `install_prereqs_macos.sh` (Homebrew);
    - `install_tsk_from_src.sh -p <dir> -b <tag>`;
    - `install_application.sh -z zip -i dir -j java_home [-n name] [-v .asc]` (GPG verification against `sleuthkit.org/carrier.asc`, then runs `unix_setup.sh`);
    - `add_macos_jna.sh -i dir`;
  - on Linux, TSK is installed from the `sleuthkit-java` `.deb`.
  - `unix_setup.sh` (`TSK_VERSION=4.15.0`) checks for PhotoRec (testdisk), sets `jdkhome` in `etc/<app>.conf`, and checks the TSK jar.
  - A **Snap** package (`snap/snapcraft.yaml`).
  - `unix/` holds an old live-ISO builder (`live_iso.sh`, `launch_script_bootable.sh`, Java 8 era).
- Release helpers: `update_autopsy_version.pl`, `update_sleuthkit_version.pl`, `APIUpdate` (API change reports), `localization_scripts` (bundle export and import for translations).
- Takeaway for postmortem: Autopsy is effectively a **Windows-first** product. Multi-user is Windows/UNC-only in practice, and tests are mostly integration/regression against real images with gold DB diffs.

---

## 15. Quirks and bugs worth not reproducing

1. ZooKeeper node paths are upper-cased file paths. They only form flat names on Windows (backslashes). On Unix, case paths become nested znodes and the case browser breaks. Case names that differ only in case collide.
2. `AutoIngestJobNodeData` writes strings with the platform default charset and a **character-count** length prefix, so non-ASCII paths corrupt records. u8 length fields cap case name and device id at 127 characters (signed byte).
3. AID priority and OCR changes do read-modify-write on node data **without** the manifest lock, so they race with the AIN that is processing.
4. `MaxNumTimesToAttemptToProcessImage` defaults to 0 in code (no crash retries) but 2 in the UI spinner.
5. Remote events are Java-serialised objects with `trustAllPackages`. This is unsafe deserialisation. They are also non-persistent: a client that reconnects misses events and never resyncs, except where a periodic poll exists (AID 5 min, collaboration heartbeat).
6. `FILE_DONE` is published to the broker once per file per ingest.
7. Remote publish is fire-and-forget with one attempt. Failures are only logged.
8. Examiner changes are published under the `NUMBER` event name.
9. Passwords are "encrypted" with a hard-coded key.
10. The CLI `--runIngest` with an unknown data source id exits 0. `findCaseDirectory` compares `lastModified` of a *relative* `File`, so it is effectively 0 and the last listed folder wins.
11. Performance diagnostics: integer division gives 0 MB/s for fast tests, and the "skipped" file test still runs.
12. `AutoIngestCaseDeletedEvent` is never sent.
13. The MCP `tools/list` returns a bare array (non-standard). `get_file_content` silently clamps `maxBytes` despite its description. Attribute filters apply after `LIMIT`.
14. Many delete steps and remote operations run on throwaway threads with no error reporting to the user.

---

## 16. PROPOSAL: pure-Rust architecture for postmortem

> **PROPOSAL.** Everything in this section is a recommendation, not a description of Autopsy. It assumes the decisions in `docs/DECISIONS.md`: ADR-002 (own schema; SQLite single-user, PostgreSQL multi-user, behind one storage trait) and ADR-005 (SI units).

### 16.1 Goals

- Preserve every observable behaviour from §2–§13: shared cases, locking semantics, live updates between users, the auto ingest cluster with priorities, retries and crash recovery, the dashboards and admin actions, case deletion, CLI, MCP and shared configuration.
- **Minimise external services**. Autopsy needs PostgreSQL + Solr + ZooKeeper + ActiveMQ + an SMB share. postmortem should need **PostgreSQL + one `postmortem-server` binary** (+ shared storage for evidence). Tantivy replaces Solr inside the server, and PostgreSQL replaces ZooKeeper and ActiveMQ.
- Work identically on Windows, Linux and macOS (no path-mangling tricks), with typed, versioned wire formats (no language-native serialisation).

### 16.2 Deployment topology

```
 examiner (postmortem GUI, egui) ──┐  HTTPS/2 + WebSocket (rustls, token auth)
 examiner (postmortem GUI)  ───────┤
 postmortem-cli / MCP bridge ──────┤
                                   ▼
                       postmortem-server (1..N instances, stateless except index shards)
                         ├─ case registry, locks, job queue  ──► PostgreSQL (one cluster)
                         ├─ event fan-out (LISTEN/NOTIFY → WS)
                         ├─ Tantivy index service (per case)  ──► local/shared disk
                         └─ (optional) embedded auto-ingest workers
 postmortem-worker (auto-ingest node, headless) ─ same protocol, or direct PG + shared storage
```

- **Single-user** stays fully local: SQLite + in-process Tantivy + an in-process event bus. No server.
- **Multi-user**: the GUI talks either (a) to `postmortem-server` only (recommended: one auth point, no DB credentials on clients, works across networks), or (b) directly to PostgreSQL for reads (faster, Autopsy-like) with the server for index, events and locks. Start with (b) for read-heavy views and keep (a) as an option behind the storage trait.

### 16.3 Coordination: PostgreSQL instead of ZooKeeper

Recommendation: **PostgreSQL advisory locks plus ordinary tables**. etcd would be one more service to operate and secure, and every lock in §3.3 protects a PostgreSQL-backed resource anyway.

- **Lock keys**: `pg_advisory_lock(bigint)` / `pg_advisory_lock_shared(bigint)`. Map `(category, resource_id)` to a 64-bit key: the category in the high 8 bits plus 56 bits of a SipHash of a **case UUID or job UUID**, never a path. Keep a `lock_names(key, category, resource, holder, acquired_at)` table, written in the same session, for diagnostics ("who holds this case?").
- **Semantics map 1:1**:
  - shared case lock while open: `pg_advisory_lock_shared`;
  - exclusive for delete or data-source delete: `pg_try_advisory_lock`;
  - "try with timeout": loop `pg_try_advisory_lock` with backoff until the deadline, or `SET lock_timeout` + `pg_advisory_lock`.
- **Session-scoped locks die with the connection**. This is equivalent to ZooKeeper ephemeral nodes, and failure detection is TCP keepalive plus `tcp_keepalives_*` / `idle_session_timeout`. Use a **dedicated `tokio-postgres` connection per held long-lived lock** (or one "lock connection" per process holding all of them) so pool recycling never drops locks. This replaces Curator's same-thread rule with "same connection".
- **Node data → tables** (in a cluster-wide `postmortem_registry` database):
  - `cases(id uuid pk, name, display_name, directory, db_name, index_location, created_at, last_access_at, errors_occurred bool, deleted_flags smallint, schema_version)` replaces `CaseNodeData`;
  - `ingest_jobs(id uuid pk, manifest_path, manifest_ctime, case_name, case_id, device_id, data_source_path, password_ref, status enum, stage enum, stage_started_at, stage_details, host, priority int, crashes int, errors bool, completed_at, data_source_size bigint, ocr bool, lease_expires_at, version)` replaces `AutoIngestJobNodeData`;
  - `nodes(host, kind, state, last_seen, version)` replaces the node-state events and the 15-minute "last seen" filter;
  - `config_versions` for shared configuration.
- **Job claiming**: `SELECT … FROM ingest_jobs WHERE status='pending' AND <per-case concurrency predicate> ORDER BY priority DESC, manifest_ctime, case_name DESC, ds_name DESC FOR UPDATE SKIP LOCKED LIMIT 1`, then set `status='processing', host, lease_expires_at = now()+lease`.
  - A worker heartbeats the lease (for example every 10 s, matching the Autopsy status interval).
  - **Crash recovery** = a sweeper that finds `processing` rows with expired leases, increments `crashes`, and requeues or completes according to `max_retries`. This replaces "lock held by a dead session + PROCESSING status".
  - Priority and OCR edits become transactional `UPDATE`s guarded by `status='pending'`. This fixes quirk 3.
- Optional abstraction: a `Coordinator` trait (`lock_shared/lock_exclusive/try_lock(timeout)`, `claim_job`, `heartbeat`, `registry CRUD`) with `PgCoordinator` and an `InProcessCoordinator` (for single-user and tests). An `EtcdCoordinator` can be added later if someone needs PostgreSQL-less coordination. It is not needed for parity.

### 16.4 Events: LISTEN/NOTIFY (+ outbox) instead of ActiveMQ

- **Transport**: PostgreSQL `NOTIFY channel, payload` (payload ≤ 8000 bytes). Use one channel per case (`pm_case_<uuid-hex>`) and one cluster channel (`pm_cluster`). This replaces the five topics per case: the case, ingest job, ingest module and collaboration channels merge into one typed stream with an `event_kind` field.
- **Durability and resync** (Autopsy has none): write events to an `events(id bigserial, case_id, kind, payload jsonb/bytea, origin_host, origin_session, created_at)` **outbox table** in the same transaction as the change, then `NOTIFY` with just the `id`. Listeners fetch by id range, and a reconnecting client resumes from its last seen id. This fixes quirk 5 and gives a free audit trail. Prune with a retention policy (for example 7 days or N rows).
- **Payloads**: versioned, schema-checked types (`serde` + `postcard`/`bincode` or JSON). Keep Autopsy's **ID-only** principle: `TagAdded{tag_id}`, `DataSourceAdded{object_id, adding_event_uuid}`, `ObjectsChanged{kind, ids[]}`. Receivers reload from the shared DB. Include `origin_session_id` so a node ignores its own events (`noLocal`).
- **Rate control**: never publish per-file events remotely (quirk 6). Coalesce ingest progress into periodic `IngestProgress{job_id, files_done, …}` events (for example 1 Hz) and `ResultsAdded{artifact_type_counts}` batches. UI trees invalidate by type and debounce.
- **When to consider NATS**: if the server must fan out to many hundreds of GUI clients across WAN links, or event volume exceeds what NOTIFY handles comfortably (NOTIFY serialises on commit). Hide it behind an `EventBus` trait (`publish`, `subscribe(case|cluster)`) with `InProcessBus`, `PgNotifyBus` and an optional `NatsBus`. Clients connected to `postmortem-server` receive events over **WebSocket** regardless of the back end.
- **Collaboration monitor**: replaced by `nodes`/`sessions` rows (who has which case open, what they are doing: `activity text, progress, updated_at`), plus events on change. The GUI shows remote activity in the status bar, with the same "host adding data source" / "host analyzing X" texts and the 1-minute heartbeat / 5-minute stale rules (now `updated_at < now()-5min`). Add an explicit **"case changed by <user>" banner/toast** with a "Refresh" action for views that cannot live-update cheaply, which the task brief calls "case updated banners".

### 16.5 Text index: shared Tantivy service

- Tantivy indexes are single-writer. Run **one index service per case** inside `postmortem-server` (shardable across server instances by case UUID, with the assignment recorded in `cases.index_location`, which is the analogue of `solrServerList.txt` / `solrserver.txt`).
- Ingest workers send extracted text **batches** (gRPC or HTTP/2, `tonic` or `axum`) to the owning server. The server commits on a time or size policy and serves search requests (keyword, regex, highlight, snippets) to all clients.
- In single-user mode the same `IndexService` trait runs in-process against `<case>/index/`.
- Deletion = drop the index directory plus the registry flag (`TEXT_INDEX` bit). The MultiUserTestTool step "index a test document" becomes a server health-check endpoint.

### 16.6 `postmortem-server` binary (axum + tokio + sqlx/tokio-postgres)

Responsibilities:

1. auth (per-user tokens; optional OS/LDAP integration later);
2. case registry API;
3. lock API for clients that do not hold their own PostgreSQL connection;
4. event fan-out over WebSocket;
5. Tantivy index service;
6. auto ingest coordinator endpoints (job list, priority, OCR, cancel, reprocess, node control);
7. health and metrics (`/healthz`, `/metrics` Prometheus): this replaces the services monitor checks and the HealthMonitor DB (store timing metrics in a `metrics` table or export them);
8. optional embedded MCP endpoint.

- Stateless apart from index shards. Several instances can run against one PostgreSQL. Leader election for singleton tasks (job-lease sweeper, orphan cleanup, retention) uses `pg_try_advisory_lock(LEADER_KEY)`.
- Configuration via TOML (`postmortem-server.toml`) + environment variables, validated at start-up with clear errors (port ranges 1–65535, reachable PostgreSQL, writable index root, TLS material).

### 16.7 Auto ingest daemon (`postmortem-worker`)

- A headless binary (also selectable as a GUI "node mode" to keep Autopsy's control panel UX). The loop:
  1. scan the input roots (`walkdir`, or `notify` file watching plus a periodic full scan, default 60 min to keep parity);
  2. parse manifests (`_MANIFEST.XML`, root `AutopsyManifest`, same elements, `quick-xml`, tolerant mode for malformed XML);
  3. upsert `ingest_jobs` by manifest path. The unique key should be `(input_root_id, relative_manifest_path)`, not an absolute path;
  4. claim the next job (§16.3);
  5. run the same stages as `AutoIngestJob.Stage` (keep the enum and display strings);
  6. on infrastructure errors, set the node state `paused_for_system_error` with the same `ErrorState` taxonomy and wait for resume (configurable auto-resume backoff as an improvement).
- Case lookup or creation serialised by an advisory lock on the **case name**: the registry holds `case_name → case_id`, so there is no directory-name timestamp scanning. Keep the `<name>_<yyyy_MM_dd_HH_mm_ss>` directory convention for compatibility.
- Per-case concurrency target (default 3), retries (default 2: **fix** the 0/2 discrepancy), sync wait (default 30 s; probably unnecessary with PostgreSQL transactions, so keep it configurable with a default of 0), scheduled pause windows.
- Case log: keep `<caseDir>/auto_ingest_log.txt` with the same line format for examiners, but write it from a `job_log` table (no cross-node file locking). Render it on demand, or append under an advisory lock.
- Node control (pause, resume, shutdown, thread-dump equivalent = a tokio task dump or `tracing` span snapshot) goes through `nodes` rows plus events. Admin rights come from the server's role model instead of `admin`/`adminext`/`mualimit` marker files (keep marker-file support for parity if wanted).
- File exporter and case importer: port as optional post-processing stages with the same rule model and output layout (§12.12). Store rules as TOML or JSON, not Java serialisation.
- **Shared configuration**: the server stores a versioned **config bundle** (ingest profiles, module settings, file type defs, interesting sets, keyword lists, hash set manifests, YARA rules, plugin list) in `config_bundles(id, created_by, created_at, sha256, blob)`, with content-addressed hash DB blobs in shared storage. Workers fetch the active bundle at job start only if the version changed (cheap `SELECT`), instead of copying the whole folder before every job. A "master" is just a role permission. Machine-local values (threads, view preferences, folders) are never part of the bundle.

### 16.8 CLI (`postmortem` binary subcommands, `clap`)

Mirror the Autopsy CLI but as proper subcommands with non-zero exit codes and JSON on stdout:

```
postmortem case create --name X --base-dir D [--multi-user]
postmortem source add --case X --base-dir D --path P [--key K] [--ingest [PROFILE]]
postmortem ingest run --case X --base-dir D --source-id N [--profile P]
postmortem source list --case X --base-dir D
postmortem report generate --case X --base-dir D [--profile P,...]
postmortem profiles list
postmortem open <case.pmcase|dir>              # GUI open
postmortem-server ... / postmortem-worker ...  # services
```

- Keep the legacy flag spellings (`--createCase --caseName= --caseBaseDir= --caseType= --addDataSource --dataSourcePath= --key --dataSourceObjectId= --runIngest[=] --listAllDataSources --generateReports[=] --listAllIngestProfiles`) as hidden aliases for script compatibility. Keep the same fixed execution order and the `Command Output/*.json` files (`@caseName`, `@caseDir`, `@dataSourceName`, `@dataSourceObjectId`).
- Exit codes: 0 success, 1 run failure, 2 usage error. Fix quirk 10.
- Progress goes to stderr through `tracing`. The `--json` flag gives machine output.

### 16.9 MCP

- Provide a **native MCP server** in Rust (stdio transport in `postmortem mcp` and streamable HTTP in the server) using an MCP SDK crate, or a small hand-written JSON-RPC layer. Implement the 17 read-only tools of §11.2 with the same names, parameters and limits so existing prompts keep working. Return results in proper MCP `content` and `structuredContent`.
- Use parameterised SQL, never string concatenation. Apply attribute filters in SQL before `LIMIT`. Reject or clamp `maxBytes` consistently with the description.
- Keep: localhost-only binding, a per-start random bearer token in a user-only-readable file, the `caseId` wrapper and its warning text, and "no case open" handling. Make it cross-platform (token under the OS config dir from `directories`).

### 16.10 Settings, secrets, health

- Multi-user settings: a single TOML section `[multi_user] server_url | pg_url, …`. Keep Autopsy's validation messages and "Test" buttons (test PostgreSQL connect, test server `/healthz`, test index round-trip, which replaces the MultiUserTestTool steps).
- Secrets go in the OS keychain (`keyring` crate), with an encrypted file fallback. Never use a hard-coded key.
- The services monitor becomes a background task polling PostgreSQL and the server every 15 min (configurable) and on demand, emitting `ServiceStatusChanged` events with the same UP/DOWN semantics and tray or toast notifications.
- Health monitor parity: timing metrics via `metrics` + an exporter, plus user events (`log_on`, `log_off`, `case_open`, `case_close`) in a table. The dashboard reads from the server.

### 16.11 Testing strategy for this area

- `InProcessCoordinator` and `InProcessBus` for unit tests. Run `testcontainers` PostgreSQL for integration tests of locks, job claiming, lease expiry and recovery (kill a worker mid-job and assert requeue with `crashes+1`), NOTIFY ordering and resume-from-id.
- Golden-output tests in the spirit of `integrationtesting`: suites in TOML/JSON, YAML/JSON snapshots with `insta`, run single-user and multi-user (`caseTypes: both`) in CI on Linux, Windows and macOS.
- A protocol fuzz target for the manifest parser and the event decoder (ADR-004 panic-free).

### 16.12 Mapping summary

| Autopsy | postmortem (PROPOSAL) |
|---|---|
| ZooKeeper RW locks (CASES/MANIFESTS/CONFIG/CENTRAL_REPO/HEALTH_MONITOR) | PostgreSQL advisory locks keyed by UUIDs, plus a `lock_names` diagnostics table |
| ZooKeeper node data (`CaseNodeData`, `AutoIngestJobNodeData`) | `cases`, `ingest_jobs` tables with typed columns and migrations (`sqlx migrate`) |
| ActiveMQ topics, Java-serialised `AutopsyEvent` | LISTEN/NOTIFY + `events` outbox, typed versioned payloads, WebSocket to GUIs. `NatsBus` optional. |
| Solr cores per case (+ `solrServerList.txt`) | Tantivy per case inside `postmortem-server`, shard assignment in `cases.index_location` |
| `CollaborationMonitor` | `sessions` activity rows + events, plus a "changed by" banner |
| `ServicesMonitor` | health poller + `/healthz` |
| `HealthMonitor` DB | metrics table / Prometheus |
| AIM + AID + control panel | `postmortem-worker` + server job API + egui dashboards (same tables, columns and actions) |
| Shared configuration folder | versioned config bundles in PostgreSQL + content-addressed blobs |
| NetBeans CLI options | `clap` subcommands + legacy aliases |
| Javalin MCP + Node stdio bridge | native Rust MCP (stdio + HTTP) |
| Marker files `admin`/`adminext`/`mualimit` | server roles (marker files optional for parity) |
