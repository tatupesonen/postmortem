# 02 — UI shell & viewers: feature inventory (Autopsy → postmortem)

This inventory covers Autopsy's main UI shell, result viewers, content viewers and the supporting UI subsystems. It is detailed enough to reimplement them in postmortem (Rust, egui with multi-viewport popouts) without reading the Java. The Autopsy source (read-only) is at `/Users/tatu/slop/autopsy`. Unless stated otherwise, paths are relative to `Core/src/org/sleuthkit/autopsy/`.

Scope: `corecomponents`, `corecomponentinterfaces`, `contentviewers` (all sub-packages), `resultviewers`, `rejview`, `guiutils`, `guicomponentutils`, `textextractors`, `texttranslation`, `textsummarizer`, `imagewriter`, `logicalimager`, `livetriage`, `url`. It also covers the NetBeans window layout, menus, toolbar and shortcuts (`core/layer.xml`, `*.wsmode`, `*.wstcref`, `@ActionReference`) and `/branding`. A few closely coupled pieces that live elsewhere are included: the KeywordSearch Extracted Text viewer, the Central Repository Other Occurrences viewer, and the startup window in `casemodule`.

## Contents

1. Application shell: main window, window modes, branding, startup (menus, toolbar, shortcuts, About, Welcome)
2. Options dialog (all categories; Application / View / Multi-User in full, with defaults)
3. Result viewers & content-panel plumbing (Table / Thumbnail / Summary, paging, column persistence, the "best viewer" algorithm, popouts, interfaces, guiutils)
4. Content viewer tab registry, plus the metadata, artifact, analysis-result, annotation, OS account, context and Other Occurrences viewers
5. File-content viewers (Hex, Text: Strings / Extracted / Translation, Application viewers: media / HTML / PDF / SQLite / plist / registry)
6. Supporting subsystems (text extractors, machine translation, summarizer, image writer, logical imager, live triage, web categories)
7. Cross-cutting performance/usability problems and the postmortem (egui) architecture recommendations

Each section ends with its own "problems & egui ideas" list. Section 7 consolidates the architecture-level ones.

---

## Application shell: main window, window modes, branding, startup

### 1.1 Branding and main frame

- Product name/version comes from branding bundles: `currentVersion=Autopsy 4.23.0`, main window title `CTL_MainWindow_Title=Autopsy 4.23.0` (same title when no case is open).
- When a case opens, the frame title becomes `"<case display name> - <AppName> <version>"`. If `AppName` (ModePreferences key `AppName`) was changed from the default, only the app name is shown (no version). On case close it reverts to `"<AppName> <version>"`. Title is also updated when case details (display name) are edited.
- Splash screen: `splash.gif`, 538 x 314 px, progress bar at (0,308,538,6), running status text at (0,289,538,18), black, 19 pt font, window title "Starting Autopsy".
- Window icons: `frame.gif` (16 px), `frame32.gif`, `frame48.gif`.
- Look and feel (corecomponents `Installer`): on macOS and Linux the cross-platform Metal L&F is used, with bold fonts turned off and every UI font set to "DejaVu Sans Condensed" 11 pt (Linux only, when the L&F succeeds). Custom 32 px option-pane icons are used for question, warning, information and error. View-mode tab strips are replaced with `NoTabsTabDisplayerUI`, so docked side windows show no tab header when they hold a single component. Tabbed content borders are zeroed.
- Sources: `branding/core/core.jar/org/netbeans/core/startup/Bundle.properties`, `branding/modules/org-netbeans-core-windows.jar/.../Bundle.properties`, `Core/src/org/sleuthkit/autopsy/corecomponents/Installer.java`, `casemodule/Case.java` (`getNameForTitle`, `mainFrame.setTitle`).

### 1.2 Window layout (NetBeans window system "modes")

The main frame is a NetBeans docking layout. Window positions persist per user in `<userdir>/config/Windows2Local`. **Window > Reset Windows** deletes this folder in a shutdown hook and restarts the app (see 1.5).

| Mode | Kind / state | Default placement | What lives there |
|---|---|---|---|
| `explorer` | view, joined | Left column, horizontal weight 0.3, vertical weight 0.5 | `DirectoryTreeTopComponent` (the "Directory Tree": Data Sources, Views, Results, Tags, Reports, Hosts/Persons tree). It has a small top toolbar with Back, Forward (navigation history of tree selections, held as two linked lists) and an "Open View Options" gear that opens `ViewPreferencesPanel` inline with immediate updates. |
| `editor` | editor (document area), joined | Centre/top-right | The main `DataResultTopComponent` ("Listing" result viewer) plus any extra result tabs, for example search results, keyword hits and common-properties results. Each one is a closable document tab. `@RetainLocation("editor")`. |
| `output` | NetBeans default output mode, joined | Bottom right, under the editor area | `DataContentTopComponent` (the main content viewer, "Main"), redocked here on every case open by `CoreComponentControl.openCoreWindows()`. |
| `floatingLeftBottom` | separated (floating), permanent | Floating, bounds (7,909,700x400) | `IngestMessageTopComponent` ("Messages": the ingest inbox). Opened from the toolbar mail icon. Persistence ALWAYS, opened=true in wstcref. |
| `outputFloat` | separated, permanent | Floating, bounds (300,495), 640x480 | Popout content viewers created by **"View in New Window"** (`NewWindowViewAction`). |
| `dockedBottom` | sliding, bottom | Minimised slide-in strip | Available slot for minimised windows. |
| `timeline` | editor, separated | Own window, (76,68,996x672) | Timeline tool (`TimeLineTopComponent`). |
| `cvt` | editor, separated | Own window, 996x672 | Communications Visualization Tool. |
| `discovery` | editor, separated | Own window, 1100x700 | Discovery tool. |
| `geolocation` | editor, separated | Own window, 800x500 | Geolocation map. |
| `personas` | editor, separated | Own window, 1200x800 | Personas manager. |
| `personadetails` | (registered) | dialog-like | Persona details. |
| `ImageGallery` | separated (ImageGallery module) | own window | Image Gallery (JavaFX). |
| `dashboard`, `nodeStatus` | Experimental module | own window | Auto Ingest dashboards. |

Behaviour notes:
- All tool windows (timeline, cvt, discovery, geolocation, personas, image gallery) and the result and content TopComponents (except the main result viewer) use `PERSISTENCE_NEVER`. They are not restored after restart and are recreated on demand.
- On **case open**, `CoreComponentControl.openCoreWindows()` docks every `DataExplorer` (the directory tree) into `explorer` and opens it, then docks `DataContentTopComponent` into `output` and opens it. This forcibly undoes any user re-docking of these two windows.
- On **case close**, `closeCoreWindows()` first calls `setNode(null)` on every open `DataContent` and `DataResult` so they do not query the closed DB. It then closes all opened TopComponents, closing DataContent before the Directory Tree and Favorites (to avoid a JavaFX NPE).
- Every NetBeans window can be dragged, docked to another mode, floated (undocked into its own OS window), minimised to a side strip, or maximised (double-click the tab or Shift+Escape). This is the platform "Configure Window" behaviour. Separated modes (`timeline`, `cvt`, ...) open as standalone OS windows by default but can be docked into the main frame.
- **Popout ("View in New Window")**: creates an independent, frozen content-viewer window in the `outputFloat` mode. It is listed under **Window > Content Viewers**. Full details are under "Result viewers & content-panel plumbing → DataContentTopComponent and popout".
- Extra result viewers (each `DataResultTopComponent` created by a search) are listed under **Window > Result Viewers** for re-focusing.
- Sources: `Core/src/org/sleuthkit/autopsy/core/layer.xml`, `core/*Wsmode.xml`, `core/*Wstcref.xml`, `core/*Settings.xml`, `corecomponentinterfaces/CoreComponentControl.java`, `directorytree/NewWindowViewAction.java`, `menuactions/*`, `apputils/ResetWindowsAction.java`.

### 1.3 Main menu tree (complete, in display order)

Positions come from `layer.xml` and `@ActionReference`. Hidden NetBeans defaults are listed for completeness, because postmortem should *not* reproduce them: File, Edit, GoTo and Reports menus, Print and Page Setup, Save/Save As/Save All, Favorites, Output window, Templates, Check for Updates, NetBeans About, Toolbars list, HTML view, Log, Options import/export, Keymaps/Appearance/Java/Miscellaneous option categories, and QuickSearch toolbar.

**Case**
1. New Case (Ctrl+N). Opens the New Case wizard.
2. Open Recent Case ▸ dynamic submenu of up to 5 recent cases (6 are tracked, newest first). Entries are disabled or missing if the files are gone.
3. Open Case… (Ctrl+O). Opens a file chooser for an `.aut` file (or, in multi-user mode, the multi-user case browser).
4. Unpack and Open Portable Case
5. Close Case
6. Delete Case
7. ──────────
8. Manage Hosts
9. Add Data Source
10. ──────────
11. Case Details (case properties dialog)
12. Data Source Summary
13. ──────────
14. Exit

**View**: only NetBeans built-ins remain (for example Full Screen). Autopsy hides Toolbars, Editors, HTML View and Log.

**Tools** (positions in brackets)
1. Images/Videos (101). Image Gallery module.
2. Communications (102). Opens CVT.
3. Geolocation (103)
4. Timeline (104)
5. Discovery (105)
6. Auto Ingest Dashboard (106, Experimental module only)
7. File Search by Attributes (200)
8. Search Central Repository (201)
9. Hash Search / "File Search by MD5 Hash" (201, hashdatabase layer)
10. Find Common Properties (202). If it is unavailable, a dialog explains why: no case open, fewer than 2 data sources, CR disabled, case not in CR, fewer than 2 CR cases, or invalid CR config.
11. ──────────
12. Run Ingest Modules ▸ (300) submenu with one entry per data source in the case, which opens the Run Ingest Modules wizard.
13. Generate Report (301), followed by a separator
14. Python Plugins (1400). Opens the python modules folder in the OS file browser.
15. ──────────
16. Personas (1825)
17. ──────────
18. Make Live Triage Drive (1850)
19. ──────────
20. Open Case Folder (1900)
21. ──────────
22. Create Logical Imager (2000)
23. Plugins (NetBeans plugin manager, used to install `.nbm` modules)
24. Options (NetBeans options dialog, see "Options dialog" below)

**Window**
1. Result Viewers ▸ (dynamic, lists the ids of all open `DataResultTopComponent`s, disabled if there are none or no case)
2. Content Viewers ▸ (dynamic: "Main" (enabled only if the case has data), followed by each popout content window by name)
3. ──────────
4. Reset Windows (205). Shows the confirmation "...the software will close and restart. If a case is currently open, it will be closed...". On yes it writes the current case `.aut` path to `<userconfig>/caseToOpen.txt`, closes the case, marks for restart and exits. A shutdown hook deletes `Windows2Local`. On the next start `StartupWindowProvider` reads `caseToOpen.txt`, deletes it, and reopens that case automatically. If that fails it shows a warning and the Welcome window.
5. NetBeans window-management items that remain: Configure Window ▸ (Maximize, Float, Float Group, Minimize, Minimize Group, Dock, Dock Group, New Document Tab Group, Collapse Document Tab Group) and Close Window (Ctrl+W). Hidden: Output, Favorites, Documents, Close All Documents, Close Other Documents, Clone, Switch to Recent Document, Progress List, IO window, Groups.

**Help**
1. Online Autopsy Documentation (F1). Opens `http://sleuthkit.org/autopsy/docs/user-docs/<version>/` in the system browser (or the NetBeans URL displayer as a fallback).
2. Offline Autopsy Documentation (F2). Opens the bundled HTML docs (`HELP_REL_PATH`, under the install dir) in the browser and logs an error if they are missing.
3. Ingest Status Details (1125). Opens the ingest progress snapshot dialog (Esc closes it).
4. Performance Diagnostics (1437)
5. Thread Dump (1750). Writes a JVM thread dump to the log dir and opens it.
6. Open Log Folder (2000)
7. ──────────
8. About (3000). See 1.6.

### 1.4 Toolbar

A single toolbar row, enabled only while a case is open (otherwise buttons are greyed out). Buttons are icon plus text, roughly 24 to 32 px icons.

| Order | Button | Action |
|---|---|---|
| 1 | Add Data Source (`btn_icon_add_image.png`) | Add Data Source wizard |
| 2 | Images/Videos | Image Gallery |
| 3 | Communications | CVT |
| 4 | Geolocation | Geolocation window |
| 5 | Timeline | Timeline window |
| 6 | Discovery | Discovery window |
| 7 | Generate Report (`btn_icon_generate_report.png`) | Report wizard |
| 8 | Close Case | Close case |
| — | Spacer (`SpacerAction`, position 10000) | pushes the following groups to the right edge |
| 9 | Ingest Messages (mail icon `mail24.png` with rollover) | Toggles the floating "Messages" inbox. The button is disabled until ingest messages exist and shows the count of unread messages. |
| 10 | Keyword Lists ▾ (dropdown) | Popup panel listing keyword lists with checkboxes, a "Search" button and "Manage Lists" |
| 11 | Keyword Search ▾ (dropdown) | Popup with a text field, Exact Match / Substring Match / Regular Expression radios, a Search button, and Cut/Copy/Paste/Select All context menu |

### 1.5 Keyboard shortcuts (global and window-scoped)

| Key | Scope | Action |
|---|---|---|
| Ctrl+N | global | New Case |
| Ctrl+O | global | Open Case |
| F1 | global | Online documentation |
| F2 | global | Offline documentation |
| Ctrl+B | Result viewer window (ancestor-of-focus), Timeline, tag menus | Add "Bookmark" tag to the selected item(s). It also appears as the accelerator on the Bookmark entry in Add Tag / Replace Tag submenus. |
| Ctrl+E | Result viewer, Timeline, Image Gallery | Open the selected file in the external viewer (`ExternalViewerShortcutAction`) |
| Esc | Tag-name dialogs, recent-cases dialog, ingest snapshot dialog | Cancel/close |
| Alt+Left / Alt+Right | Timeline, Image Gallery | Back/forward in view history |
| Ctrl+Z / Ctrl+Y | Image Gallery | Undo/redo categorisation |
| 0–5 | Image Gallery | Assign category |
| Shift+Esc, Ctrl+W, Alt+Shift+Enter | NetBeans platform | Maximise window, close window, full screen |
| Ctrl+C | tables/text | Copy (Swing default) |

### 1.6 About dialog

- Header logo `images/splash.png`. Clicking it opens `URL_ON_IMG` (the Autopsy web site) in the browser.
- An HTML description block (12 pt Verdana) shows: **Product Version** (`currentVersion` + build number + build type, e.g. RELEASE/DEVELOPMENT), **Sleuth Kit Version** (`SleuthkitJNI.getVersion()`), **Netbeans RCP Build**, **Java** (`java.version`; VM name + version), **System** (`"<os.name> version <os.version> running on <os.arch>"`; file encoding; system locale), and **Userdir**.
- Copyright block: links to autopsy.com, training, community support, "Copyright © 2003-2025 Sleuth Kit Labs", "Maintained by Sleuth Kit Labs", "Sponsored by Cyber Triage". Links open in the system browser.
- **"Activate verbose logging"** button. It turns on verbose logging for the session, after which the button text changes to say that verbose logging is enabled and the button is disabled. If verbose logging is already on, the button starts disabled.
- Close button.
- Source: `corecomponents/AboutWindowAction.java`, `AboutWindowPanel.java`, `corecomponents/Bundle.properties` (`LBL_Description`, `LBL_Copyright`).

### 1.7 Startup ("Welcome") window

- `StartupWindowProvider` decides what to show after the UI is ready, and after command-line option processing has completed:
  1. When running from the command line or headless, it runs `CommandLineOpenCaseManager` (if a case path is the default argument) or `CommandLineIngestManager`. No welcome window is shown in headless mode, and a `CommandLineStartupWindow` is shown otherwise.
  2. If multi-user mode is enabled but no Solr host is configured, it shows an error notification and the `SolrNotConfiguredDialog`.
  3. Otherwise it picks a `StartupWindowInterface` service. The default is `StartupWindow`. If a second implementation is registered, the non-default one wins. With three or more, the first whose class is not under `org.sleuthkit.autopsy` wins (the extension point for custom startup windows, e.g. Experimental auto-ingest mode chooser).
  4. If `caseToOpen.txt` exists (from Reset Windows), that case is opened directly on a background thread.
  5. Before the startup window, it shows `CentralRepositoryNotificationDialog` once, when the default CR (SQLite) was set up for the first time.
- `StartupWindow` is a modal, non-resizable dialog of 750 x 400, titled "Welcome" and centred on the main window. It contains `CueBannerPanel` with the welcome logo and three big 64 x 64 icon buttons with labels: **New Case**, **Open Recent Case** (disabled when there are no recent cases), and **Open Case**. A **Close** button sits bottom-right. Closing the dialog leaves an empty main window.
- "Open Recent Case" opens a sub-dialog (`OpenRecentCasePanel`) with a 2-column table, **Case Name** (30% width) and **Path** (70%), plus Open and Cancel buttons. Double-click opens a case. Esc closes the dialog. Missing cases give an error and are removed from the list.
- Recent cases are kept by `RecentCases` with LENGTH = 6 (5 displayed), newest first, deduplicated by path, stored in preferences.
- Sources: `casemodule/StartupWindowProvider.java`, `StartupWindow.java`, `CueBannerPanel.java`, `OpenRecentCasePanel.java`, `RecentCases.java`, `corecomponents/Installer.java`.

## Options dialog (Tools > Options)

The NetBeans options dialog has a row of category icons along the top, a keyword search box, and OK/Cancel (plus Apply and Help in some versions). Import/Export is hidden. Each category is an `OptionsPanelController`. A category panel's `valid()` gates OK, and `load()`/`store()` are called on open and OK. Many settings are greyed out while a case is open or ingest is running.

### 2.1 All categories (by position)

| Pos | Category (title) | Owner | Notes |
|---|---|---|---|
| 1 | Application | corecomponents `AutopsyOptionsPanel` | see 2.2 |
| 2 | View | corecomponents `ViewPreferencesPanel` | see 2.3 |
| 3 | Ingest | ingest `IngestOptionsPanel` | tabs: Settings (file ingest threads, default 2, max suggested = cores; process timeout enabled = false, 60 h), File Filters, Profiles |
| 4 | Multi-User | corecomponents `MultiUserSettingsPanel` | see 2.4 |
| 5 | Auto Ingest | Experimental | |
| 6 | Keyword Search | KeywordSearch module | |
| 7 | Machine Translation | texttranslation | see the translation section |
| 8 | Hash Sets | hashdatabase | |
| 9 | File Extension Mismatch | fileextmismatch | |
| 10 | File Types | filetypeid (user-defined signatures) | |
| 11 | Interesting Files | interestingitems | |
| 12 | Tags | casemodule/services (custom tag names, notable flag) | |
| 13 | External Viewer | directorytree (MIME/extension → external program rules) | |
| 15 | Central Repository | centralrepository | |
| 16 | Image Gallery | ImageGallery | |
| 17 | Command Line Ingest | commandlineingest | |
| 18 | Geolocation | geolocation (tile source: online OSM default, OSM server, offline OSM zip, MBTiles) | |
| 20 | YARA | yara rule sets | |
| 21 | Custom Web Categories | url/analytics | |
| 800 | AI: MCP Server | mcp (new in this fork) | |
| 999999 | Cyber Triage | com.basistech... | vendor upsell |

### 2.2 Application (`AutopsyOptionsPanel`)

All in a scroll pane, grouped in titled panels:

- **Logo** (used in reports): radio buttons *Use default* / *Specify a logo*, a path field and Browse (image file chooser), and a 64 x 64 preview ("No logo selected" when empty). Validation messages: "Agency logo path must be set.", "Path is not valid.", "Invalid image file specified.". Stored via `ReportBranding.setAgencyLogoPath` (empty means default).
- **Runtime** (disabled on 32-bit JVM or developer builds, with "Memory settings are not available while running in developer mode"):
  - *Total System Memory*: read-only, in GB (bytes / 1e9).
  - *Maximum JVM Memory*: integer GB text field. The initial value comes from `-J-Xmx` in `<userdir>/etc/<appname>.conf`, or else the install `etc/<appname>.conf` (`default_options="..."`). Units `g`/`G`, or `m`/`M` converted to GB. Validation: positive integer, at least **2 GB**, below total system memory. On store, `<userdir>/etc/<appname>.conf` is rewritten.
  - *Maximum Solr JVM Memory*: spinner in MB with min 512, step 512, max = system GB × 1024. Default **2048 MB** on a 64-bit JVM (512 MB on 32-bit). Key `SolrMaxJVMSize` (machine-specific).
  - *Maximum Log Files*: positive integer, default **10** (`MaximumNumberOfLogFiles`).
  - *Custom Heap Dump Location*: directory field plus Browse, written as `-J-XX:HeapDumpPath=` in the conf. Must exist, contain no quotes, and must not already exist as a file.
  - Notes: "A restart is necessary for any memory changes to take effect." and "NOTE: Setting this too large may impact overall performance." (Solr).
- **Root Temp Directory**: radios *Local temp directory* (default `SYSTEM` = `${java.io.tmpdir}/<AppName>/Temp`), *Temp folder in case directory* (`CASE`), and *Custom* with path field plus Browse (`<custom>/<AppName>/Temp`). The choice must have read/write access. All are disabled while a case is open ("Close the current case to change the temporary directory."). Keys: `TempDirChoice`, `TempDirectory` (UserMachinePreferences).
- **Windows High DPI Scaling** (Windows only): static help text explaining how to set the compatibility "Override high DPI scaling" setting on the shortcut.
- Source: `corecomponents/AutopsyOptionsPanel.java`, `AutopsyOptionsPanelController.java`, `core/UserPreferences.java`, `machinesettings/UserMachinePreferences.java`.

### 2.3 View (`ViewPreferencesPanel`), also reachable from the directory-tree gear button

Global Settings (stored in shared `ViewPreferences.properties`):

| Setting | UI | Key | Default |
|---|---|---|---|
| When selecting a file | radios: "Change to the most specific file viewer" / "Stay on the same file viewer" | `KeepPreferredViewer` | false (switch to best viewer) |
| Hide known files (NSRL) in Data Sources area | checkbox | `HideKnownFilesInDataSourcesTree` | false |
| Hide known files in Views area | checkbox | `HideKnownFilesInViewsTree` | **true** |
| Hide slack files in Data Sources area | checkbox | `HideSlackFilesInDataSourcesTree` | **true** |
| Hide slack files in Views area | checkbox | `HideSlackFilesInViewsTree` | **true** |
| When displaying times | radios: "Use local time zone" / "Use another time zone" plus a list of every Java time zone formatted `"(GMT+hh:mm) Area/City"`, sorted by raw offset then ID (150 x 130 list) | `DisplayTimesInLocalTime`, `TimeZoneForDisplays` | local = true; zone = GMT |
| Hide other users' tags in the Tags area of the tree | checkbox | `ShowOnlyCurrentUserTags` | false |
| Do not add columns for S(core), C(omments), and O(ccurrences) to reduce loading times | checkbox | `HideCentralRepoCommentsAndOccurrences` | false |
| Translate text: Add column in result viewer for file name translation | checkbox (enabled only if a translation provider is configured) | `DisplayTranslatedNames` | true |
| Maximum number of results to show in table | spinner 0–50 000, step 10 000, not typeable; 0 = all (tooltip warns of poor responsiveness) | `ResultsTablePageSize` | **10 000** |

Current Case Settings (enabled only with a case open; stored in case preferences): radios **Group by Data Type** / **Group by Person/Host** for the tree (`GroupItemsInTreeByDataSource`, default false).

Current Session Settings (not persisted): **Hide rejected results** checkbox (applies to the directory tree, default unchecked, i.e. rejected results are shown).

When opened from the tree gear (`immediateUpdates = true`), each change is applied immediately without OK. Other settings: `ExternalHexEditorPath` (default `C:\Program Files\HxD\HxD.exe`, set from the hex viewer).

### 2.4 Multi-User (`MultiUserSettingsPanel`)

- "Enable multi-user cases" checkbox (only on Windows: "Multi-user cases are only available on Windows platforms"). All fields are disabled while a case is open ("Close case to modify settings"). "Application restart required to take effect."
- **Database Server Settings** (PostgreSQL): Hostname/IP, Port (default 5432), User Name, Password (stored hex-obfuscated), and a *Test Connection* button with a status icon.
- **Solr 8 Server Settings**: host, port (default 8983), Test Connection. **Solr 4 Server Settings** (legacy): host, port, Test. Notes: "Enter Solr 8 and/or Solr 4 server settings." and "New text indexing can only be done with Solr 8." At least one must be filled in.
- **ZooKeeper Server Settings**: host, port (default 9983, i.e. Solr port + 1000), Test.
- **ActiveMQ Message Server Settings**: host, port (default 61616), optional user and password, Test.
- Validation messages: "Fill in all values", invalid port messages per service, "ZooKeeper server host name not set", "Either Solr 8 or/and Solr 4 server needs to be configured".
- Text fields show grey prompt text (`TextPrompt`) such as "Hostname or IP Address".

### 2.5 Misc corecomponents dialogs and utilities

- `AdvancedConfigurationDialog` / `AdvancedConfigurationCleanDialog`: generic modal wrappers that host an ingest-module "Advanced" settings panel, titled with the panel name. The first has Apply/Cancel buttons. The "Clean" variant has no extra chrome.
- `OptionsPanel` interface: `load()`, `store()`, `valid()`, used by all options panels.
- `TextPrompt`: grey placeholder text in a text field, shown while it is empty.
- `AutoWrappingJTextPane`: a word-wrapping, read-only text pane used in viewers.
- Sources: `corecomponents/*.java`.

### 2.6 egui notes for the shell

- egui has no docking manager. Use `egui_dock` (or `egui_tiles`) for the main area: a left tree panel, a centre tab group of result viewers, and a bottom tab group of content viewers. Use egui multi-viewports (`ctx.show_viewport_deferred`) for "popout" windows: content viewer popouts, timeline, CVT, geolocation, discovery, personas, gallery and messages. Persist the dock tree plus viewport rects in the app config (replacing `Windows2Local`), and provide "Reset layout" *without* restarting the app.
- Do not force-redock the tree and content panels on case open (an Autopsy annoyance). Remember the user's layout per profile.
- Popout content windows should offer an optional "follow selection" toggle (Autopsy popouts are always frozen) and a pin icon.
- Command palette (Ctrl+K / Ctrl+Shift+P) listing every menu action. Make shortcuts rebindable (Autopsy hides the NetBeans Keymaps panel).
- Startup: a welcome screen embedded in the main window (not modal) with a recent-cases list that shows last-opened time, size and an existence check done asynchronously.
- Options: a single searchable settings window with left-side categories. Validate inline. Apply memory and thread settings without a JVM restart where possible (Rust needs no `-Xmx`, so drop the JVM/Solr memory fields and replace them with cache-size limits, for example thumbnail cache in MB and DB page cache in MB).

---

## Result viewers & content-panel plumbing

This section covers the area at the top right of the main window (the "result view") and the area at the bottom right (the "content view"). It describes how they are wired together, the extension interfaces, paging, column persistence, the popout windows, and the shared GUI utilities. All paths are relative to `Core/src/org/sleuthkit/autopsy/` unless stated otherwise.

### Mental model (three-pane wiring)

```
DirectoryTreeTopComponent (explorer mode, left)
   | selection of a tree node -> wrapped in DataResultFilterNode -> TableFilterNode
   v
DataResultTopComponent "main" (editor mode, top right)  ── owns ExplorerManager
   └─ DataResultPanel: header [description/path label] ......... [N] [Results]
        └─ JTabbedPane of DataResultViewer tabs:  Table | Thumbnail | Summary
              | single-row selection (ExplorerManager PROP_SELECTED_NODES)
              v
DataContentTopComponent "Data Content" (output mode, bottom right)
   └─ DataContentPanel: JTabbedPane of DataContentViewer tabs (10 built in)
```

* Selection flows **one way**: tree → result panel (`setNode(rootNode)`), then result panel → content panel (`setNode(selectedChild)`). The content panel gets a node only when **exactly one** row is selected. With 0 rows or more than 1 row selected it gets `setNode(null)`, which clears every content viewer.
* When a result TopComponent is activated (focused), it pushes its current single selection to the main `DataContentTopComponent` again (`componentActivated`). Switching between several result windows therefore re-targets the shared content viewer.
* `CoreComponentControl.openCoreWindows()` runs on case open. It docks every `DataExplorer` TopComponent into the `explorer` mode and the `DataContentTopComponent` into the `output` mode. `closeCoreWindows()` runs on case close. It first calls `setNode(null)` on every open `DataContent`/`DataResult`, then closes all TopComponents. Directory Tree and Favorites close last, because closing them before the content viewer causes a JavaFX NPE.

### Extension interfaces (`corecomponentinterfaces/`)

| Interface | Contract (what postmortem needs as a trait) |
|---|---|
| `DataExplorer` | A "top-left" explorer window: `getTopComponent()`, `hasMenuOpenAction()`. It is also a PropertyChangeListener. Implemented by the directory tree (and the old Favorites). |
| `DataResult` | A result-view container: `setNode(root)` (null = reset), `getPreferredID()`, `setTitle()`, `setPath(descriptionText)`, `isMain()`, `getViewers()`. |
| `DataResultViewer` | A tab inside a DataResult. `createInstance()` is a factory used for non-main windows. `isSupported(root)` enables or disables the tab. `setNode(root)`, `setSelectedNodes(Node[])`, `getTitle()`, `getComponent()`, `resetComponent()`, `clearComponent()` (dispose). `expandNode`/`setContentViewer` are deprecated no-ops. Discovered via `@ServiceProvider` lookup. |
| `DataContent` | The "bottom-right" content container: `setNode(selected)`. Extends PropertyChangeListener. |
| `DataContentViewer` | A tab inside DataContent. `setNode(node)` (null must drop all references), `getTitle()`, `getTitle(node)` (dynamic tab title, default = `getTitle()`), `getToolTip()`, `createInstance()`, `getComponent()`, `resetComponent()`, `isSupported(node)` (enables the tab), `isPreferred(node) -> int` (priority 0-8, see below). The viewer handles its own vertical scrolling. The container wraps it in a JScrollPane with the vertical bar set to NEVER, so it supplies horizontal scrolling only. |
| `TextViewer` | Same shape as DataContentViewer (minus `getTitle(node)`). These are sub-tabs inside the **Text** content viewer. Implementations: `StringsTextViewer` (position 1), KeywordSearch `ExtractedTextViewer` (position 2, isPreferred = 4), `TranslatedTextViewer` (position 4, isPreferred = 0). |
| `ContextMenuActionsProvider` | `getActions() -> List<Action>`. Contributes extra right-click actions based on the selected objects in the global action context. |
| `CoreComponentControl` | Static open/close of the core windows, described above. |
| `BlackboardResultViewer` | Deprecated and unused (`viewArtifact`, `viewArtifactContent`, `FINISHED_DISPLAY_EVT`). It can be skipped. |
| `DataSourceProcessor` | An add-data-source plugin: `getDataSourceType()` (combo label), `getPanel()` (must be <544×173 px), `isPanelValid()`, `run(progress, callback)` plus overloads with `Host` and with `password`, `runWithIngestStream(...)` variants, `supportsIngestStream()` (default false), `cancel()`, `reset()`. It fires `DSP_PANEL_EVENT.UPDATE_UI` (and the deprecated `FOCUS_NEXT`) to the wizard. |
| `DataSourceProcessorCallback` | `done(result, errList, newDataSources)` marshals to the EDT and calls `doneEDT`. Result enum: `NO_ERRORS`, `CRITICAL_ERRORS`, `NONCRITICAL_ERRORS`. |
| `DataSourceProcessorProgressMonitor` | `setIndeterminate(bool)`, `setProgress(int)`, `setProgressMax(int)` (default no-op), `setProgressText(String)`. |

### DataResultTopComponent (result window)

File: `corecomponents/DataResultTopComponent.java`

* **Main instance**: created once by `DirectoryTreeTopComponent` with the title from `DirectoryTreeTopComponent_resultsView_title` (the "Listing" window). `isMain = true`. It **cannot be closed** while the case has data (`PROP_CLOSING_DISABLED`, `canClose()` returns `!isMain || !case.hasData()`). Main viewers are the lookup singletons themselves, not copies.
* **Secondary instances** use `createInstance(title, description, node, childCount[, viewers])`. Each gets fresh viewer copies (`viewer.createInstance()`), can be closed, and has `PERSISTENCE_NEVER` unless a custom mode name is given. In that case the type is `PERSISTENCE_ALWAYS` and `open()` docks the window into that mode, falling back to the default mode with a warning. Callers that open new result tabs in the editor area:
  * File Search (`filesearch/FileSearchPanel`)
  * Keyword ad-hoc search (`KeywordSearch/.../AdHocSearchDelegator`): one tab per search
  * Hash DB search (`modules/hashdatabase/HashDbSearchManager`)
  * Common Properties search (`commonpropertiessearch/CommonAttributePanel`), which passes a custom viewer list
  * All-Cases (CR) search (`allcasessearch/AllCasesSearchDialog`)
* `initInstance` order: set the child count label, `open()`, `setNode(node)`, `setPath(description)`, `requestActive()`.
* Window flags on every result window: maximization disabled, dragging disabled (`PROP_DRAGGING_DISABLED`), `@RetainLocation("editor")`. Tooltip: "This is a DataResult window". Name and preferred ID equal the title. A static `activeComponentIds` list tracks open result window titles.
* Keyboard shortcuts bound while focus is inside a result window (`WHEN_ANCESTOR_OF_FOCUSED_COMPONENT`):
  * **Ctrl+B**: Add Bookmark tag to the selection (`AddBookmarkTagAction`)
  * **Ctrl+E**: open the selection in the external viewer (`ExternalViewerShortcutAction`)

### DataResultPanel (header + viewer tabs)

File: `corecomponents/DataResultPanel.java`

* **Layout**:
  * Top row: a left `descriptionLabel` showing the path or description text (for example the tree path or the search criteria). At the right, `numberOfChildNodesLabel` followed by the literal "Results".
  * Below that, a JTabbedPane of result viewers with zero gap.
* **Tabs**: when no explicit viewer list is given, all `DataResultViewer` service providers are used, in this order: **Table**, **Thumbnail**, **Summary**. Main windows use the lookup singletons. Others use `createInstance()`. `addResultViewer()` lets callers append a custom viewer.
* **`setNode(root)`**:
  1. Detach the listener from the old root and attach a `RootNodeListener` to the new one.
  2. Call `resetComponent()` on **every** viewer.
  3. `setupTabs(root)`: set each tab enabled or disabled from `viewer.isSupported(root)`. Pick the tab to select:
     * If the root is a `TableFilterNode` with a pending `NodeSelectionInfo` (meaning "select this child after load", used by "Go to file" and "View source file in directory"), force the **Table** tab.
     * Otherwise select the **first enabled tab**.
     * Only the selected viewer gets `setNode(root)`.
  4. Update the count label from `root.getChildren().getNodesCount()`.
* **Changing tabs** (`stateChanged`): the newly selected viewer gets `setNode(currentRootNode)` under a wait cursor. Viewers are lazy: hidden tabs are reset but never populated.
* **Asynchronous children**: NetBeans `ChildFactory` (created with async=true) first produces a placeholder child named **"Please Wait..."**. `RootNodeListener.childrenAdded` updates the count label on every add or remove. When the first *real* (non-"Please Wait...") child appears, it re-runs `setupTabs` once per root (`waitingForData` flag). This is needed because `isSupported` and the Table column setup depend on real children.
* **Selection to content**: an `ExplorerManager` listener on `PROP_SELECTED_NODES` calls `contentView.setNode(sel[0])` when exactly one row is selected, and `null` otherwise. `contentView` defaults to the main `DataContentTopComponent`. Custom `DataContent` targets are supported: the Timeline uses its own content panel, and the Message viewer's attachment table uses `createInstanceUninitialized(..., null)`, which means no content push.
* **`close()`**: removes the listener and calls `setNode(null)` on all viewers. On non-main windows it also calls `clearComponent()` on all viewers and tears down the Swing tree.
* `DataResultPanel` can also be embedded without a TopComponent, through the static `createInstance(...)` variants (Timeline, Message viewer attachments, and others).

### DataResultViewerTable ("Table" tab)

File: `corecomponents/DataResultViewerTable.java` (1533 lines), backed by NetBeans `OutlineView`/`ETable`.

**Layout**
* Toolbar row, left to right: `Page:` `[cur of total]` `Pages:` `[◀]` `[▶]` `Go to Page: [__]` … `[Save Table as CSV]` (right-aligned).
* The table fills the remaining area.
* The paging controls stay hidden until there is more than one page (`togglePageControls(true)` only when `totalPages > 1`).
* The CSV button is disabled for the Common Properties table subclass.

**Rows and columns**
* Each child node is one row. The first column is the node **display name** with its icon (tree column). It is labelled from the first property's display name, defaulting to "Name". Root is not visible. Drag-and-drop is disabled. Selection is `MULTIPLE_INTERVAL_SELECTION`.
* **Column discovery** (`ResultViewerPersistence.getAllChildProperties(root, 100)`): the union of all `PropertySet` properties of the **first 100 children**, recursing into grandchildren (each level capped at 100), in insertion order. Properties that only appear after row 100 never get a column.
* Every column is sortable (`ComparableColumnTTV`). Column 0 is the tree column and the initial sort column.
* **Auto column width** (`setColumnWidths`):
  * width = max(header preferred width, preferred width of the rendered cell for the **first 100 rows**) + 15 px padding
  * minimum 30 px, maximum 300 px
  * The **last column** stretches to fill the remaining viewport width, minus the scrollbar width when the rows overflow vertically.
  * Auto-resize mode is OFF when there are rows, which gives a horizontal scrollbar. With an empty table it is `AUTO_RESIZE_ALL_COLUMNS`.
* **Empty root**: `setNode` with null, or a root with 0 children, shows an empty LEAF root with no columns.
* `setNode` clears any ETable quick filter (`outline.unsetQuickFilter()`). NetBeans ETable offers a right-click quick filter on cells.
* NetBeans `OutlineView` provides a **column chooser** button in the top-right header corner (the "..." popup with checkboxes to show or hide columns). Header drag reorders columns. Header click sorts. Shift+click adds a secondary sort key.

**Special S / C / O columns** (installed when a column's header text is exactly "S", "C" or "O")
| Header | Tooltip | Renderer |
|---|---|---|
| **S** (Score) | "S(core) indicates whether the item is interesting or notable" | Centered icon. `Score.significance` NOTABLE → red circle-exclamation icon. LIKELY_NOTABLE → yellow circle-yield icon. NONE, LIKELY_NONE and UNKNOWN → blank. Cell tooltip = the property short description, i.e. the reason. |
| **C** (Comment) | "C(omments) indicates whether the item has a comment" | Centered notepad icon for `CR_COMMENT` ("Comment exists in Central Repository"), `TAG_COMMENT` ("Comment exists on associated tag(s)") and `CR_AND_TAG_COMMENTS`. Blank for `NO_COMMENT` and `TAG_NO_COMMENT` (tooltip "No comments found"). |
| **O** (Occurrences) | "O(ccurrences) indicates the number of data sources containing the item in the Central Repository" | Left-aligned number. Blank if the value is < 0 or not a Long. Tooltip = description ("No correlation properties found" / "Multiple different correlation properties exist for this result"). |
* S/C/O values are **lazy**:
  * File and artifact nodes create placeholder properties with the description "value loading".
  * A `GetSCOTask` is submitted to a shared 10-thread pool (`content-node-background-task-%d`, `AbstractContentNode.MAX_POOL_SIZE = 10`). The task computes score, tags and CR comments, and the CR occurrence count. The **O column exists only when the Central Repository is enabled**.
  * When the task finishes it fires `SCO_AVAILABLE`, and the node updates its sheet.
  * The columns are hidden entirely when the user preference "Hide S/C/O columns" is set (`UserPreferences.getHideSCOColumns()`).
* The CSV export skips the S, C and O columns.

**Persistence of column order, visibility and sort** (`ResultViewerPersistence`, NbPreferences for the module `DataResultViewerTable`)
* State persists only when the root is a `TableFilterNode`. The key base is `stripNonAlnum(tfn.getColumnOrderKey()) + "." + stripNonAlnum(propName)`. `columnOrderKey` defaults to `"NONE"`, so every table that never sets a key shares one layout. Tree nodes pass a per-node-type key, for example the artifact type or "file types by extension".
* Keys used:
  * `<base>.column`: position index (int)
  * `<base>.hidden`: boolean. Stored only when true; removed when shown.
  * `<base>.sortOrder`: true means ascending
  * `<base>.sortRank`: int. 0 or absent means unsorted.
* **Load order**:
  1. `loadColumnOrder`: saved positions win. Properties with no saved position, an out-of-range position or a clashing position are appended. The map is then compacted to remove gaps.
  2. `assignColumns`
  3. `loadColumnSorting`: applied in rank order
  4. `populateColumnMap`
  5. `loadColumnVisibility`
  6. `setColumnWidths`
* **Store triggers**:
  * Column moved → `storeColumnOrder`. Moving a column into or out of position 0 is reverted on mouse release, so the Name column is pinned first.
  * Header mouse click → `storeColumnSorting`. Hidden columns lose their sort.
  * Column added or removed (show/hide through the chooser) → `storeColumnVisibility`. This is posted to the EDT and suppressed while the table is being rebuilt (`listenToVisibilityChanges(false)`).
* **Column widths are not persisted.** They are recomputed on every `setNode`.

**Auto-selection after load**
* If the root has a `NodeSelectionInfo`, the table scans `getNodes(true)` for the matching child and selects it via `setExploredContextAndSelection` (invokeLater). It then clears the info so the selection happens only once.

**Paging**
* Implemented in `datamodel/BaseChildFactory.java` together with an inner `PagingSupport` in the table.
* Page size preference: `UserPreferences.RESULTS_TABLE_PAGE_SIZE` ("ResultsTablePageSize"), default **10 000**. It is edited in Options → View with a spinner: range 0..50 000, step 10 000. **0 means no paging**: all keys go into one page.
* `BaseChildFactory.createKeys()` calls `makeKeys()`, which fetches **all** keys from the database every time. It applies the optional filter predicate, then `Lists.partition(keys, pageSize)`. Only the current page's keys become nodes. Paging therefore limits *node/Swing* cost, not database cost.
* Communication goes over a Guava `EventBus` per **node name**, kept in a static map `nodeNameToEventBusMap`:
  * `PageCountChangeEvent(count)`: factory → table, posted whenever the page count changes
  * `PageChangeEvent(n)`: table → factory. The factory calls `refresh(true)` without re-querying (`isPageChangeEvent`).
  * `PageSizeChangeEvent(size)`: preference listener → all factories. Re-partitions the existing keys and resets to page 1.
  * `RefreshKeysEvent`: forces a re-query.
* The table keeps one `PagingSupport` per root node *name* (a map), so the current page is remembered per tree node while the viewer lives.
* **Controls**:
  * The label shows "`{cur} of {total}`".
  * Prev is disabled on page 1. Next is disabled on the last page.
  * Go-to-page accepts decimal, hex `0x..` or octal input (`Integer.decode`). Non-numeric input is silently ignored. An out-of-range page shows a warning dialog: "Please enter a valid page number between 1 and N" / "Invalid page number".
  * During a page change the cursor becomes a wait cursor until the root's `childrenAdded`/`childrenRemoved` fires.
* Edge cases:
  * Two different tree nodes with the same `getName()` share one event bus.
  * `setNode` must run on the EDT. Otherwise it logs SEVERE and returns.

**Save Table as CSV** (`directorytree/ExportCSVAction.saveNodesToCSV`)
* Exports **all rows of the current page** (`currentRoot.getChildren().getNodes()`), not only the selection. With no rows it shows the info message "No data to export".
* File chooser:
  * Default directory: the case Export folder, or the last user-chosen directory if it still exists.
  * Default name: `<first property value of the parent node, with "(123)" count suffix stripped and \/:*?"<>| replaced by _> <yyyyMMdd(e)hhmmss>`. Note `%te` gives a day without padding and `%tI` a 12-hour clock. The fallback name is `Results <date>`.
  * `.csv` is appended if missing. An info message appears if the file already exists.
* Header = union of property display names across the first 100 rows, minus S/C/O. Rows come from a Jackson CSV writer (null → ""). The export runs in a cancellable SwingWorker with a progress handle "Exporting to CSV file". Messages are shown on success and error.
* The same action, "Export Selected Rows to CSV", appears in context menus for selected rows.

### DataResultViewerThumbnail ("Thumbnail" tab)

Files: `corecomponents/DataResultViewerThumbnail.java`, `ThumbnailViewChildren.java`, `SortChooser.java`, `CriterionChooser.java`

* `isSupported(root)` returns `root != null`, so the tab is always enabled when something is selected. Nodes that are not images or videos are simply skipped in the thumbnail grid.
* **Layout**:
  * Wrapping toolbar (`WrapLayout`): `Page: [cur of total]` · `Pages: [◀][▶]` · `Go to Page: [____]` · `Images: [from-to]` · `[Small|Medium|Large Thumbnails ▼]` · `[Sort]  Sorted by: …`.
  * Center: a NetBeans `IconView` grid.
  * Bottom: `filePathLabel` shows the selected file's **unique path**, also used as its tooltip, when exactly one item is selected.
  * Multi-selection is allowed (multiple interval).
* **Thumbnail sizes** (`coreutils/ImageUtils`): Small = **50 px**, Medium = **100 px** (default, combo index 1), Large = **200 px**. Changing the size re-sizes every node, invalidates the cached images and cancels any in-flight tasks, then re-shows the current page.
* **Which files are shown**: a child whose lookup contains an `AbstractFile` and passes `ImageUtils.thumbnailSupported` is shown. The check uses the extension first: supported image extensions plus video extensions from `VideoUtils`. Then it tries a video MIME check, then an image MIME check (MIME detection can be expensive).
* **Thumbnail paging** is independent of the table paging. **`IMAGES_PER_PAGE = 200`**:
  * All supported children of the *current table page* are sorted and partitioned into pages of 200.
  * Each page is a `ThumbnailPageNode` named "1", "2", … with the display name "from-to".
  * Moving between pages switches the explored context. A SwingWorker shows the progress "Generating Thumbnails..." and disables the page buttons while it runs.
  * An invalid Go-to page shows the same warning dialog as the table.
* **Image loading**:
  * Each `ThumbnailViewNode.getIcon()` returns the cached image from a `SoftReference` when present.
  * Otherwise it submits a `ThumbnailLoadTask` to a **3-thread pool** ("Thumbnail-Loader-%d") per children object and returns an animated spinner GIF (`working_spinner.gif`). A 1 ms Swing timer repaints the spinner by firing icon changes.
  * Each task creates a system progress handle "Generating thumbnail for {name}" with a 500 ms initial delay.
  * `ImageUtils.getThumbnail(content, size)` produces the thumbnail. It uses a disk cache under the case folder, outside this area.
  * Display names are abbreviated to **18 chars** with "...".
* **On a new `setNode`**: all pending tasks are cancelled and the executor is shut down (`cancelLoadingThumbnails`). With an empty root the background turns black.
* **Sort** (the thumbnail view has no column headers):
  * The `Sort` button opens the "Choose Sort Criteria" dialog (`SortChooser`). It holds a list of `CriterionChooser` rows: `Sort By: [property ▼] (•▲ Ascending | ▼ Descending) [Remove]` plus an "Add Sort Criteria" button. The property list is the union of child properties over the first 100 rows, shown by property *name*.
  * On OK, the criteria are written to the **same preference keys** as the table's sort (`sortOrder`/`sortRank`, rank + 1) under the `DataResultViewerThumbnail` module node. If a property is chosen twice, the first criterion is kept. The node is then re-set to apply the sort.
  * Sorting uses a chained comparator over property values (`Comparable`, otherwise `toString`), with nulls first. DESCENDING and UNSORTED are both reversed.
  * The label shows "Sorted by: 1. Name ▲ 2. Size ▼", or "Sorted by: ---" when no criteria are set.
  * Quirk: the persistence reads through `NbPreferences.forModule(DataResultViewerTable.class)`. In practice both classes are in the same module, so the table sort and the thumbnail sort are **shared**.
* Controls are disabled (size combo, sort button, paging) when `totalPages == 0`.

### DataSourceSummaryResultViewer ("Summary" tab)

File: `resultviewers/summary/DataSourceSummaryResultViewer.java`

* Enabled only when the selected tree node's lookup contains a `DataSource` (an image, local disk or logical file set node).
* It embeds `datasourcesummary/ui/DataSourceSummaryTabbedPane`, whose sub-tabs are: **Types, User Activity, Analysis, Recent Files, Past Cases, Geolocation, Timeline, Ingest History, Container**.
* `setNode` → `summaryPanel.setDataSource(ds)`. Loading inside each panel is asynchronous (data source summary area, documented elsewhere). `clearComponent` closes the panel's listeners.

### TableFilterNode family

Files: `corecomponents/TableFilterNode.java`, `TableFilterChildren.java`, `TableFilterChildrenWithDescendants.java`

* `TableFilterNode(node, createChildren)` is a FilterNode wrapper placed around whatever the result viewers display.
  * With `createChildren = true`, children are wrapped as `TableFilterNode(child, false)`, which makes them leaves: the table is flat and never shows expanders.
  * When `createChildren` is true, the display name is forced to "Name" (the header of column 0).
* `TableFilterNode(node, createChildren, columnOrderKey)` sets the key used for column and sort persistence.
* `TableFilterNode(node, childLayerDepth)` keeps N levels of descendants as an expandable tree-table, and uses the wrapped display name. It is used by Common Properties (instance → files grouping).
* `get/setChildNodeSelectionInfo` delegates to the wrapped `DataResultFilterNode`. This drives "select this child after display".

### DataContentTopComponent and popout ("View in New Window")

Files: `corecomponents/DataContentTopComponent.java`, `directorytree/NewWindowViewAction.java`, `menuactions/DataContentDynamicMenu.java`, `core/outputFloatWsmode.xml`

* **Default instance**: preferred ID `DataContentTopComponent`, name "Data Content", tooltip "This is a DataContent window". It is docked in the `output` mode (bottom right), has `PERSISTENCE_NEVER`, and cannot be closed while the case has data. `findInstance()` looks it up in the WindowManager and falls back to a lazily created singleton.
* **"View in New Window"**: a context-menu action on file, directory, layout file, slack file, local file, image, volume, report, tag, keyword hit and generic artifact nodes, and the Message viewer attachment table.
  1. Title = the node's lookup `String` if present. Otherwise `Content.getUniquePath()`, for example `/img_x.E01/vol_vol2/Users/bob/a.jpg`. The fallback is "DataContent".
  2. `DataContentTopComponent.createUndocked(title, null)` creates a non-default instance with its **own copies of every content viewer** (`factory.createInstance()`).
  3. The window is docked into the mode **`outputFloat`**: kind = editor, state = **separated** (a floating OS window), bounds x=300 y=495 **640×480**, `permanent` empty-behavior.
  4. It opens. Then, via invokeLater, `setNode(node)`, `toFront()`, `requestActive()`.
  * Each popout is a fully independent content viewer for one fixed node. It does **not** follow table selection. Many can be open at once, and they are tracked in the static `newWindowList`. Closing one calls `setNode(null)` and removes it from the list.
* **Window → Content Viewers submenu** (`DataContentDynamicMenu`, which is dynamic): it is enabled only with a case open.
  * The "Main" item re-opens or focuses the default content window. It is enabled only if the case has data.
  * One item follows per open popout, labelled with the popout's title (the unique path).
* Global tab-header UI hack (`corecomponents/Installer.java`): `UIManager.put("ViewTabDisplayerUI", NoTabsTabDisplayerUI)`. Every NetBeans *view-mode* window container is rendered **without its tab strip**: zero-size tab displayer, no tab hit-testing, no drop indication, no shortcuts. The explorer/output windows therefore look like plain panels. Also: empty view content border and `TabbedPane.contentBorderInsets = 0`.

### DataContentPanel: building tabs and choosing the "best" viewer

File: `corecomponents/DataContentPanel.java`

* **Construction**: one tab per `DataContentViewer` service provider, ordered by `@ServiceProvider(position)`. The main window uses the singletons. Popouts and embedded panels use `createInstance()`. Each viewer component is wrapped in a JScrollPane (vertical bar NEVER). The tab tooltip is `getToolTip()`. **All tabs start disabled.**
* **Built-in tab order and `isPreferred` scores**: see the "Content viewer tab registry" section below. In short, Hex(1), Text(max of sub-viewers), Application(5), Metadata(1), OS Account(1), Data Artifacts(7/3), Analysis Results(3), Context(1), Annotations(1), Other Occurrences(1).
* **`setNode(node)` algorithm**:
  1. Cancel any running `DataContentPanelWorker`. Set `currentNode = null`.
  2. Synchronously, for each tab: disable it, update its title from `getTitle(node)` (title changes are applied only if different), and call `resetComponent()` (marks it *outdated*).
  3. If the node is non-null, start a SwingWorker. **In the background** it calls `isSupported(node)` for every viewer and records supported indices. For supported viewers it calls `isPreferred(node)` and keeps the **first** index with the strictly greatest score. Its initial values are `maxPreferred = 0` and `preferredIndex = 0`. Tie-break: the lowest tab position wins, so Hex (1) wins over Metadata (1) when nothing scores higher. The worker checks `isCancelled()` between viewers.
  4. On the EDT (`done`), unless cancelled: `currentNode = node`. Enable all supported tabs. Choose `tabIndex`:
     * If the preference **"Keep preferred viewer"** is set (`UserPreferences.keepPreferredContentViewer()`, key `KeepPreferredViewer`, default **false**), keep the currently selected tab index (stick with the user's tab).
     * Otherwise use the computed preferred index.
     * If that tab is disabled, reset it (blank). Otherwise call `setNode(node)` on that viewer only. Then select the tab and repaint.
* **Tab switch** (`stateChanged`): if the new tab's viewer is *outdated*, or is the Data Artifacts viewer (always refreshed), call `setNode(currentNode)` under a wait cursor. Other tabs are populated lazily the first time they are viewed for each node.
* Implications:
  * `isSupported`/`isPreferred` run off the EDT, but `setNode` of the chosen viewer runs on the EDT. Heavy viewers spawn their own workers.
  * The "selected tab" is not remembered per node type. With Keep-preferred off, the panel jumps to the computed best tab on every selection.
* `DataContentViewerUtility.getDefaultContent(node)`: returns the first non-artifact `Content` in the node's lookup, or the artifact if that is the only one. Hex, Strings and others use it so that selecting an artifact row shows its **source file's** bytes.

### guiutils / guicomponentutils (shared widgets)

All under `guiutils/` unless stated otherwise.

| Util | Behavior |
|---|---|
| `GrayableCellRenderer` | Left-aligned `toString()` cell. Uses the normal selected or unselected colors when the table is enabled. **When the table is disabled the cell is lightGray background with darkGray text.** Base class for the renderers below. |
| `CenteredGrayableCellRenderer` | Same as above, center-aligned. |
| `LongDateCellRenderer` | Center-aligned date in the format **`yyyy/MM/dd HH:mm:ss`** (local JVM time zone, SimpleDateFormat). null → "". |
| `ShortDateCellRenderer` | Center-aligned date in the format **`MM/dd HH:mm`**. |
| `DurationCellRenderer` | Left-aligned, ms → `DD:HH:MM:SS`. Days are zero-padded to 2 digits unless ≥ 99. Negative durations show as 00:00:00:00. Separator ':' (`getUnitSeperator`). Used by the ingest and auto-ingest job tables. |
| `StatusIconCellRenderer` | Centered status icon: OK → tick.png ("OK"), WARNING → warning16.png ("A warning occurred"), ERROR → cross-script.png ("An error occurred"). Unwraps NodeProperty values. |
| `SimpleTableCellRenderer` / `SimpleListCellRenderer` | Default renderer with tooltip = the cell text (for truncated values). |
| `CheckBoxJList<T: CheckboxListItem>` | A JList whose rows are checkbox + optional icon + label. A mouse press anywhere on a row toggles it. Single selection. Item interface: `isChecked`, `setChecked`, `getDisplayName`, `hasIcon`, `getIcon`. `guicomponentutils/AbstractCheckboxListItem` gives defaults: unchecked, no icon. |
| `CheckBoxListPanel<T>` | Title label (optional icon) + scrollable CheckBoxJList + "Check All"/"Uncheck All" buttons. API: `addElement(name, icon, obj)` (deduplicated, checked by default), `getSelectedElements`, `setSelectedElements`, `setSetAllSelected`, `clearList`, `isEmpty`, `setEnabled`. Used for data source and type filters (Timeline, Discovery, etc.). |
| `DataSourceComboBoxModel` | Simple String-array combo model for data source pickers. |
| `ContactCache` | A singleton Guava LoadingCache, **expiring 10 minutes after last access**. It maps an account identifier to the contact artifacts that mention it. Built by scanning **all TSK_CONTACT artifacts** and indexing every attribute whose type name starts with TSK_EMAIL, TSK_PHONE, TSK_NAME or TSK_ID. Invalidated on an ingest DATA_ADDED of TSK_CONTACT. `getContactNameList(id)` returns the unique TSK_NAME values. Used by the message, call log and communications viewers to show contact names. |
| `RefreshThrottler` | Coalesces ingest events DATA_ADDED, CONTENT_CHANGED and FILE_DONE into **at most one refresh per 5 seconds** (`MIN_SECONDS_BETWEEN_REFRESH = 5`). It uses a single scheduled thread ("Node Refresh Thread"). A `Refresher` supplies `isRefreshRequired(evt)` and `refresh()`. Used by tree and child factories during ingest. |
| `JFileChooserFactory` | Pre-builds a JFileChooser on a background thread because it is slow on Windows. `getChooser()` blocks with a wait cursor if it is not ready yet. |
| `WrapLayout` | Flow layout that wraps to new rows and grows its height. Supports a set of right-aligned ("opposite") components and baseline alignment. Default gaps 5/5. Used by the thumbnail toolbar. |
| `guicomponentutils/AutoCompletion` | Editable-JComboBox type-ahead (public-domain Bierhance code). Typing completes to the first model entry with a matching prefix and highlights the completed suffix. Backspace handling. Popup opens on key press. Optionally hides the popup on focus loss. |

### Performance/usability problems (this part) and egui ideas for postmortem

**Problems in Autopsy**
* **Full key load on every listing.** `BaseChildFactory.makeKeys()` fetches *every* row (for example all 2 million files of a type) and pages in memory. Page changes avoid re-querying, but any refresh re-fetches everything. The default page size of 10 000 builds 10 000 Swing nodes with property sheets.
* **Columns are guessed from the first 100 rows.** Columns that appear only in later rows (artifact attributes vary per row) are silently missing.
* **Column widths are computed by rendering 100 rows** on every `setNode` and never persisted. Sorting is done by ETable *within the current page only*: sorting does not reorder across pages, which misleads users.
* **S/C/O lazy columns** start a background task *per row* (10-thread pool, Central Repository queries per row). Sorting by S/C/O while loading is meaningless, and rows flicker when values arrive.
* **Persistence keyed by node name and type strings**: column layouts collide (`"NONE"` key) and event buses collide (same node name). There is no reset-to-default layout.
* **"Please Wait..." placeholder rows** make tab setup run twice. Selection is lost on refresh. Wait cursors are used instead of inline progress.
* **Thumbnails**: only 3 loader threads, thumbnail pages are nested inside table pages (200 inside 10 000), sorting happens through a modal dialog, and sizes are fixed at 50/100/200 px.
* **Content panel**:
  * `isSupported` on Data Artifacts and Analysis Results does a DB round-trip per selection.
  * Keyboard scrolling through rows triggers a full viewer reset and worker for every intermediate row. There is no debounce.
  * Without "keep preferred viewer" the tab jumps around as the user scrolls through rows.
  * There is no per-type memory of the user's last tab.
* **Popouts** show a frozen node with a cryptic unique-path title. There is no "follow selection" option, no pinning or comparing of two items side by side in the main window, and only one fixed default size (640×480 at 300,495).
* CSV export covers only the current page, uses the local date format quirks (`%te`, `%tI`), and skips S/C/O.

**egui ideas**
* **Virtualized table** (`egui_extras::TableBuilder` with `body.rows(row_height, total_rows, |row| ..)`) backed by a **DB-side windowed query**: `SELECT … ORDER BY <sort cols> LIMIT ? OFFSET ?`, or keyset pagination on `(sort_key, obj_id)`. Fetch the total count separately (with an estimate first if needed). This removes UI paging: infinite scroll with row-range prefetch (for example ±500 rows), an LRU row cache, and paging kept only as an optional mode.
* **Server-side sort and filter** across the whole result set. Multi-column sort via shift-click. A per-column filter row (text contains, ranges, enum chips) that compiles to SQL.
* **Schema-driven columns**: derive columns from the result *type* (file listing schema, or artifact type → attribute type list from the DB) rather than sampling rows. Show a column-chooser popover, "reset layout", and named layouts.
* Persist layout (order, visibility, **widths**, sort) per stable `ListingKind` id in the app config DB (serde → SQLite), not per display name.
* **S/C/O**: compute in bulk per visible window with one batched query (score from `tsk_aggregate_score`-like tables, tag/comment existence, CR counts) on a background tokio/rayon task. Show a small spinner glyph per cell until the result arrives, and allow sorting only after the data has loaded, or precompute during ingest.
* **Thumbnail grid**: a virtualized grid (`ScrollArea::show_rows` over rows of N tiles). Use a slider for continuous size instead of 3 presets, a decode pool sized to the CPU count, cancellation tied to the visible range, and a GPU texture LRU keyed by (obj_id, size bucket). The disk thumbnail cache stays. Sort uses the same server-side sort as the table, not a modal dialog.
* **Content panel**: run `is_supported`/`preferred` scoring as one async "capabilities" query per node (for example `has_data_artifacts`, `has_analysis_results`, mime, size in a single SQL round-trip). **Debounce selection** by about 100-150 ms while key-repeat navigation is active. Load only the visible viewer tab and cancel stale loads by generation counter.
  * Keep the "best viewer" rule and its tie-break, but add a "remember last tab per item kind" preference, a "lock tab" pin, and a visible badge on tabs that have data. The badge saves users from clicking through disabled or empty tabs.
* **Multi-viewport popouts**: an "Open in new window" action creates an `egui::ViewportId` holding its own `ContentPanelState` (node ref + viewer states).
  * Options: "follow main selection" (live) vs "pinned"; a readable title (file name — data source); saved size and position.
  * A Window menu lists open popouts (like Autopsy's Content Viewers submenu). Popouts close with the case.
  * Allow popping out *result tables* too, not only content. Autopsy's secondary result windows (search results) only dock as editor tabs.
* Keyboard: keep Ctrl+B (bookmark) and Ctrl+E (external viewer). Add Ctrl+C to copy the cell or row, Ctrl+Shift+C to copy the row as TSV, Enter to open, Ctrl+F for a quick filter, Alt+←/→ for listing history.
* CSV/XLSX export streams the **entire** result set from SQL (all pages, optional S/C/O), runs as a cancellable background job with a progress bar, and uses an ISO-8601 timestamp in the default filename.
* Refresh during ingest: keep the 5 s coalescing idea, but make refresh incremental (re-query the visible window plus the count) and **preserve selection and scroll position** by stable `obj_id`.

---

## Content viewer tab registry: order, support rules and "preferred" scoring

All lower-pane content viewers are NetBeans `@ServiceProvider(service = DataContentViewer.class, position = N)` singletons. `DataContentPanel` creates one tab per viewer, ordered by `position`. The tab set is always the same; only the enabled state changes.

**Selection algorithm** (`corecomponents/DataContentPanel.java`, `DataContentPanelWorker`):
1. On node change, cancel any running worker, disable every tab, refresh each tab title via `getTitle(node)` (titles can change per node), and call `resetComponent()` on every viewer.
2. A background worker calls `isSupported(node)` on every viewer. These calls can hit the DB, e.g. `hasDataArtifacts` or `hasAnalysisResults`. For each supported viewer it also calls `isPreferred(node)`. The winner is the first viewer with the strictly highest score. `maxPreferred` starts at 0, so a tie goes to the lower position. If nothing scores above 0, index 0 (Hex) is the default.
3. On the EDT, supported tabs are enabled. The selected tab is either the preferred one or, when the user preference `keepPreferredContentViewer` is true, the tab that is already selected. Only that viewer gets `setNode(node)`. Other tabs are populated lazily when the user clicks them.
4. The priority scale is `contentviewers/utils/ViewerPriority.java`: LevelZero=0 … LevelSeven=7, plus `Level`=8, which is unused.

| Pos | Tab title (`getTitle`) | Class | `isSupported(node)` | `isPreferred(node)` |
|---|---|---|---|---|
| 1 | **Hex** | `corecomponents/DataContentViewerHex` | `getDefaultContent(node)` is non-null, is not a `BlackboardArtifact`, and has size > 0. `getDefaultContent` takes the first non-artifact `Content` in the lookup and falls back to the artifact. | 1 |
| 2 | **Text** | `contentviewers/textcontentviewer/TextContentViewer` | False for a directory or a 0-byte file. Otherwise true if any `TextViewer` sub-tab supports the node (see below). | The highest score among its supported sub-viewers |
| 3 | **Application** | `contentviewers/FileViewer` | The node has an `AbstractFile` that is not a directory. The MIME type is taken from the DB; if it is empty, `FileTypeDetector` runs synchronously. The MIME type must not be `application/octet-stream`, and some `FileTypeViewer` must support it. | 5 if a sub-viewer supports the MIME type, else 0 |
| 4 | **File Metadata**. When the lookup contains a `DataArtifact`, the title is **Source File Metadata**. | `contentviewers/Metadata` | The lookup contains an `AbstractFile` or an `Image` | 1 |
| 5 | **OS Account** | `contentviewers/osaccount/OsAccountViewer` | The lookup has an `OsAccount`, or a file with `getOsAccountObjectId()` present, or a DataArtifact with an OS account id | 1 |
| 6 | **Data Artifacts** | `corecomponents/DataArtifactContentViewer` | Some `Content` in the lookup is neither a DataArtifact nor an AnalysisResult, and `blackboard.hasDataArtifacts(content.id)` is true | 7 if the node itself is a DataArtifact whose category is DATA_ARTIFACT, except TSK_WEB_DOWNLOAD and TSK_WEB_CACHE, which get 3. Otherwise 3. |
| 7 | **Analysis Results** | `contentviewers/analysisresults/AnalysisResultsContentViewer` | The lookup has an `AnalysisResultItem`, or a `TskContentItem` whose content passes `blackboard.hasAnalysisResults(id)` | 3 |
| 8 | **Context** | `contentviewers/contextviewer/ContextViewer` | The node has an `AbstractFile` with at least one `TSK_ASSOCIATED_OBJECT` artifact | 1 |
| 9 | **Annotations** | `contentviewers/annotations/AnnotationsContentViewer` | `AnnotationUtils.getDisplayContent(node).getContent() != null`. The content is the artifact's source content, or the node's `AbstractFile`. | 1 |
| 10 | **Other Occurrences** | `centralrepository/contentviewer/DataContentViewerOtherCases` | Central Repository (CR) is enabled, and the node has one of: an `OsAccount`, a `BlackboardArtifactItem`, a `BlackboardArtifactTag`, or an `AbstractFile` with a non-blank MD5 | 1 |

There is also a commented-out sample at `examples/SampleContentViewer.java`.

**Text tab sub-viewers** (`TextViewer` service, ordered by position, shown as an inner tab strip):

| Pos | Sub-tab | Class | Supported | Preferred |
|---|---|---|---|---|
| 1 | Strings | `contentviewers/textcontentviewer/StringsTextViewer` | Same rule as Hex | 1 |
| 2 | Indexed Text | `KeywordSearch/.../ExtractedTextViewer` | An ad-hoc keyword search result, a TSK_KEYWORD_HIT, a credit-card TSK_ACCOUNT, or a file that has indexed text | 4 |
| 4 | Translation | `texttranslation/ui/TranslatedTextViewer` | A translation provider is configured and the node has an `AbstractFile` | 0 |

**Effective defaults** that follow from this table:
- A file with an application viewer opens on **Application** (5).
- A keyword hit opens on **Text → Indexed Text** (4).
- A data artifact (web history, message, contact…) opens on **Data Artifacts** (7).
- An analysis result such as a hash hit opens on **Analysis Results** (3). It beats Data Artifacts because that tab is unsupported for analysis-result nodes.
- Everything else opens on **Hex**, which wins ties at 1.

---

## File Metadata viewer (`contentviewers/Metadata.java`)

**Rendering.** A read-only HTML `JTextPane` styled by `layout/ContentViewerHtmlStyles` with h1 headers, a two-column key/value table and monospace rows. The content is built in a `SwingWorker`. While it loads, the pane shows "Metadata loading..." and a wait cursor. A new node cancels the previous worker.

**Section "Metadata" for an `AbstractFile`**, in this order:
1. Name: `file.getUniquePath()`, falling back to `parentPath + "/" + name`.
2. Type: TSK_DB_FILES_TYPE name, e.g. FS, CARVED, DERIVED, LOCAL, UNALLOC_BLOCKS, SLACK, LAYOUT_FILE.
3. MIME Type: may be null or blank before File Type ID has run.
4. Size: raw bytes, unformatted.
5. File Name Allocation: `getDirFlagAsString()` (Allocated/Unallocated).
6. Metadata Allocation: `getMetaFlagsAsString()`, e.g. "Allocated, Used".
7. Modified, Accessed, Created, Changed. Each is formatted by `TimeZoneUtils.getFormattedTime`, which honours the global timezone and display-format preferences.
8. MD5: shows "Not calculated" when null.
9. SHA-256: shows "Not calculated" when null. Files get no SHA-1 row; only images do.
10. Hash Lookup Results: `file.getKnown()` enum toString (UNKNOWN / KNOWN / BAD).
11. Acquisition Details: from the `DataSource` in the node lookup, if there is one. A multi-line value is split into one row per line, with blank keys on the continuation rows. Shows "Unknown" when empty.
12. Internal ID: the object id.
13. Local Path: only for type LOCAL (logical files), from `getLocalAbsPath()`.
14. Downloaded From: shown when the file has a `TSK_ASSOCIATED_OBJECT` artifact whose `TSK_ASSOCIATED_ARTIFACT` points to a TSK_WEB_DOWNLOAD or TSK_WEB_CACHE artifact. The value is that artifact's TSK_URL. Only the first associated object is used.

**Section "From The Sleuth Kit istat Tool:"**, only for `FsContent`. This is the output of `fsFile.getMetaDataText()`, one monospace row per line: inode details, MFT attributes, $STANDARD_INFORMATION/$FILE_NAME times, data runs and so on.
- Output is truncated once the HTML exceeds 50 000 chars, with a "(results truncated)" row.
- Empty output shows "No Data".

**Section "Metadata" for an `Image` data source** (no AbstractFile in the lookup), in this order:
- Name (unique path)
- Type (image type name: Raw/E01/VHD/VMDK…)
- Size
- MD5, SHA1, SHA-256. Each shows "Not calculated" when null or empty.
- Sector Size
- Time Zone
- Acquisition Details
- Device ID
- Internal ID
- Local Path: one row per image segment path, or "None"

**Not shown** (gaps postmortem could fill):
- Owner, UID/GID and mode bits, except inside the istat text
- The OS account, which has its own tab
- Extended attributes and alternate data streams as structured data
- File system details (offset, block size)
- Volume/partition info
- SHA-1 for files
- Formatted sizes

---

## Data Artifacts viewer (`corecomponents/DataArtifactContentViewer.java`)

**Purpose.** Shows the data artifacts (category DATA_ARTIFACT) attached to the source content of the selected node, one artifact per "page".

**Layout.**
- A 24 px top bar: `Result: <n> of <total>   Result  [◀][▶]` on the left (the labelling is clumsy) and the artifact display name, e.g. "Web History", right-aligned.
- Below the bar, an `OverlayLayout` panel that hosts the chosen per-artifact sub-viewer.

**Loading** (`SelectedNodeChangedTask`, SwingWorker):
1. Takes the first `Content` in the lookup that is not a DataArtifact or AnalysisResult, i.e. the file or source content.
2. Loads `content.getAllDataArtifacts()`, all of them, with no paging at the DB level.
3. Picks the start page as follows. If the node itself is a DataArtifact, use its index in the list. If that artifact has `TSK_ASSOCIATED_ARTIFACT`, use the referenced artifact's index instead; this redirects e.g. an "associated object" to the real message. Otherwise start on page 1.
4. Prev/Next start a `SelectedArtifactChangedTask(page)`. Starting a new task cancels the previous one.
5. On error, the right label shows "Error retrieving result".

**Sub-viewer dispatch.** `ArtifactContentViewer` services come from a `HashSet`, so iteration order is undefined; the supported sets are disjoint in practice. The first viewer whose `isSupported(artifact)` returns true is used. The fallback is `DefaultTableArtifactContentViewer`.

| Artifact types | Sub-viewer |
|---|---|
| TSK_EMAIL_MSG, TSK_MESSAGE, and TSK_KEYWORD_HIT whose TSK_ASSOCIATED_ARTIFACT is a message or email | `MessageArtifactViewer` |
| TSK_CONTACT | `ContactArtifactViewer` |
| TSK_CALLLOG | `CallLogArtifactViewer` |
| TSK_WEB_HISTORY, WEB_SEARCH_QUERY, WEB_COOKIE, WEB_BOOKMARK, WEB_DOWNLOAD, WEB_CACHE, WEB_ACCOUNT_TYPE, WEB_FORM_ADDRESS, WEB_FORM_AUTOFILL | `GeneralPurposeArtifactViewer` |
| everything else | `DefaultTableArtifactContentViewer` |

A new viewer instance is created each time for the default table. The others are reused singletons from Lookup.

### DefaultTableArtifactContentViewer (`contentviewers/artifactviewers/`)

**Table.** An ETable with three columns: **Type** (preferred width 100), **Value** (800) and **Source(s)** (100).
- Not editable.
- Cell selection is enabled; column selection is a single interval.
- Column reorder and hiding are disabled.
- Auto-resize mode is NEXT_COLUMN.

**Rows**, in this order:
1. One row per attribute: the attribute type display name, the value, and `attr.getSources()` joined with ", ".
2. "Source File Path": the unique path of the artifact's parent content.
3. "Artifact ID".

**Value formatting:**
- DATETIME values use `TimeZoneUtils.getFormattedTime`.
- JSON values are pretty-printed into an indented multi-line string:
  - Objects become `key = value` lines.
  - Arrays become numbered entries (1, 2, …) indented four spaces.
  - Any key whose name contains "DATETIME" is treated as epoch seconds and formatted.
  - JSON null becomes `key = null`.
- All other types use `getDisplayString()`.

**Cells.** Multi-line cells render as a wrapping `JTextArea`. Row heights are recomputed for the Value column whenever a column is resized; this is O(rows) per resize.

**Context menu:**
- **Copy**: copies the selected cells, tab-separated, with a newline between rows.
- **Select All**.

### GeneralPurposeArtifactViewer (web artifacts)

**Layout.** A scrollable `GridBagLayout` form: bold header rows, then `Key: ` labels with read-only `JTextPane` values. Right-clicking a value offers **Copy**.

**Sections:**
1. **Details header**, titled by type:
   - Visit Details (history)
   - Bookmark Details
   - Cached File
   - Cookie Details
   - Downloaded File
   - Web Search
   - "Details" for every other type
2. Attributes in a per-type order. Each attribute is removed from the pool once it is shown.

   | Type | Attribute order |
   |---|---|
   | Bookmark | NAME, TITLE, DATETIME_ACCESSED, CREATED, START, END, DOMAIN, URL, PROG_NAME |
   | Cache | DOMAIN, URL, ACCESSED, CREATED, START, END, HEADERS, PATH, PROG_NAME |
   | Download | DOMAIN, URL, ACCESSED, CREATED, START, END, PATH, PROG_NAME |
   | Search | TEXT, ACCESSED, CREATED, START, END, DOMAIN, PROG_NAME |
   | History | TITLE, USER_NAME, ACCESSED, CREATED, START, END, DOMAIN, URL, REFERRER, PROG_NAME |
   | Cookie | DOMAIN, URL, NAME, VALUE, PROG_NAME |
   | Default | TITLE, NAME, ACCESSED, CREATED, START, END, DOMAIN, URL, REFERRER, PROG_NAME, VALUE, TEXT, PATH, PATH_ID, HEADERS |

   Special cases:
   - In a web search, TSK_TEXT is labelled **Term** and every datetime is labelled **Time**.
   - TSK_PATH gets the suffix " (no longer exists)" when the artifact has no TSK_PATH_ID.
   - Any attribute named TSK_DATETIME* is formatted with the user's timezone.
3. **Dates**: remaining DATETIME_CREATED, START, END and DATETIME, labelled Created/Start/End/Date Time. A bug makes the condition always true, so this section appears for every type and not just cookies.
4. **Other**: all remaining attributes, in hash-map order.
5. **Source**: Host (the host of the data source, may be blank), Data Source (name), and File (the parent's unique path).

### MessageArtifactViewer (email, SMS/chat)

**Envelope.** A grey translucent band showing:
- From, To, CC
- Subject
- Date/time, right-aligned
- Direction (messages only)

Values carry tooltips with the full text, for long recipient lists.
- For email the fields are TSK_EMAIL_FROM, TSK_EMAIL_TO, TSK_EMAIL_CC, TSK_SUBJECT and TSK_DATETIME_RCVD.
- For messages the fields are TSK_PHONE_NUMBER_FROM, TSK_PHONE_NUMBER_TO, TSK_DIRECTION, TSK_SUBJECT and TSK_DATETIME. CC is disabled for messages.

**Body tab strip.** Each tab is enabled only if it has content. The viewer auto-selects the last tab that received non-empty text, so HTML or RTF wins over plain text.
1. **Headers**: TSK_HEADERS in a text area. Email only.
2. **Text**: TSK_EMAIL_CONTENT_PLAIN or TSK_TEXT. This tab is a `TranslatablePanel` with an "Original Text / Translated Text" combo, enabled when a translation service is configured. Text orientation follows the language direction (RTL support).
3. **HTML**: TSK_EMAIL_CONTENT_HTML in `HtmlPanel`. Remote images are blocked by default; a **Download Images** / **Hide Images** toggle controls them. Email only.
4. **RTF**: TSK_EMAIL_CONTENT_RTF in a JTextPane configured with the RTF editor kit. Email only.
5. **Attachments (N)**: an embedded `DataResultPanel` table of `AttachmentNode`s.
   - Columns: Name, Location, Size, Mime type, Known. Each is shown only when the attachment is a file.
   - Node actions: View File in Directory, View in New Window, Open in External Viewer (Ctrl+E), and Search for files with the same MD5 hash.
   - A **View in New Window** button is enabled when exactly one attachment is selected.
   - Attachments come from the JSON attribute TSK_ATTACHMENTS (`MessageAttachments`: file attachments and URL attachments). For legacy data the fallback is the AbstractFile children of the artifact.
6. **Accounts**: `MessageAccountPanel`. Enabled after load; the constructor sets it from whether CR is enabled.

**Loading.** Done by `MessageArtifactWorker` (SwingWorker), which is cancelled when the selection changes. If the artifact is a keyword hit with TSK_ASSOCIATED_ARTIFACT, the associated message is resolved and shown instead.

**MessageAccountPanel**
- Calls `CommunicationsManager.getAccountsRelatedToArtifact(artifact)` and skips the device's own account (account id == data source device id).
- Each account row shows:
  - The account identifier, with a right-click Copy.
  - **Persona:** the persona name or "Unknown". There is one row per persona when an account belongs to several.
  - A **View** button when a persona exists, which opens `PersonaDetailsDialog` in VIEW mode.
  - A **Create** button when none exists. It opens the dialog in CREATE mode, pre-filled with the account id as the name and the account added with justification "Account found in Message artifact" and confidence HIGH. If the account is not in the CR, the dialog opens with a warning popup.
  - **Contact:** the name from the first TSK_CONTACT that matches the account, looked up through `guiutils/ContactCache`, or "Unknown".
- Empty result: "No matches found."

### ContactArtifactViewer (TSK_CONTACT)

**Layout**, top to bottom in a vertical scroll:
1. **Photo**. The first child `AbstractFile` of the artifact is read with `ImageIO.read(new File(file.getLocalAbsPath()))`, so it only works for files with a local path. The fallback is `images/defaultContact.png`.
2. **Name header**: the first non-empty TSK_NAME*, or "Unknown".
3. **Phone**: every TSK_PHONE* attribute, labelled with its display name.
4. **Email**: every TSK_EMAIL* attribute.
5. **Other**: all other attributes.
6. **Source**: Host and Data Source.
7. **Persona**:
   - With CR disabled, the section shows "Unknown" greyed out and the message "Enable Central Repository to view, create and edit personas."
   - With CR enabled, it first shows "Searching..." while `ContactPersonaSearcherTask` runs in the background:
     - It collects the CR accounts for all non-DEVICE accounts related to the contact.
     - It gathers every persona linked to any of those accounts.
   - With no persona found, it shows "No matches found" and a **Create** button. The create dialog is pre-filled with the contact name and every CR account, using justification "Account found in Contact artifact".
   - Otherwise there is a "Match N" row per persona: the persona name and a **View** button, followed by either "All accounts found." or one "Missing contact account: <id>" row per contact account not linked to that persona.
   - After a Create, the button turns into View.

### CallLogArtifactViewer (TSK_CALLLOG)

**Deriving the parties:**
- **Direction "Incoming"**:
  - From = the first non-null of PHONE_NUMBER_FROM, PHONE_NUMBER and TSK_ID.
  - To = PHONE_NUMBER_TO.
  - Local = TO.
- **Direction "Outgoing"**: the reverse. To = the first non-null of PHONE_NUMBER_TO, PHONE_NUMBER and TSK_ID; From = PHONE_NUMBER_FROM; Local = FROM.
- **No direction**: From = the first non-null of FROM, TO, PHONE_NUMBER and ID.
- A party equal to the device id is dropped.
- A comma-separated To becomes the first recipient plus "other parties", each rendered as an extra "To" row.

**Sections:**
1. **Parties**. Each From/To row shows:
   - The identifier, plus " (Local)" when it is the local account.
   - "Contact: <name>" rows taken from ContactCache.
   - A persona row: "Persona: Searching..." with a disabled button, later replaced by "Persona: <name> (1 of N)" or "Unknown", and a **View** or **Create** button. This data is filled in by `PersonaAccountFetcher`, a SwingWorker that maps account id to personas. Create uses the justification "Account found in Call Log artifact".
2. **Metadata**:
   - Direction (or Unknown).
   - Date: DATETIME_START, falling back to DATETIME.
   - Duration: "`N` seconds" when END > START.
3. **Other Attributes**: any attribute not in the handled set {PHONE_NUMBER, _TO, _FROM, ID, DIRECTION, DATETIME, _START, _END}.
4. **Source**: Host and Data Source.
5. With CR disabled, the message "Enable Central Repository to view, create and edit personas."

---

## Analysis Results viewer (`contentviewers/analysisresults/`)

**Which content is analysed.** If the node is an `AnalysisResultItem`, the analysed content is the result's parent, and that result is "selected". Otherwise the analysed content is the node's `TskContentItem` content.

**Data loaded** (`AnalysisResultsViewModel`, via `DataFetchWorker`):
- `analyzedContent.getAggregateScore()`
- `analyzedContent.getAllAnalysisResults()`, every result on the item and not only the selected one
- The item name. For an artifact this is `getShortDescription()`; otherwise it is the content name.

**Header:**
- `Item: <name>`
- `Aggregate Score: <significance display name>`, e.g. Notable, Likely Notable, Likely None, None, Unknown

**One section per result**, titled `Analysis Result N`. Results are sorted by Score descending.
- Fixed rows first: **Score** (the significance), **Type** (the artifact type display name, e.g. "Hashset Hit", "Interesting Items", "Keyword Hits", "Encryption Detected"), **Configuration** (e.g. the set name), **Conclusion**, **Justification**.
- Then all blackboard attributes, sorted case-insensitively by display name, each as `Display Name: value`.
- All values are trimmed. Null becomes "".

**Behaviour:**
- Each section carries an HTML anchor `AnalysisResult_<id>`. When the node is a specific result, the pane scrolls to that anchor; otherwise it shows the top.
- While loading, the pane shows "Loading...". On error it shows "There was an error loading results."

---

## Annotations viewer (`contentviewers/annotations/`)

**What is displayed.** For an artifact node, the artifact plus its source content. For a file node, the file.

**HTML built with jsoup**, as a sequence of sections:
- **For an artifact:**
  1. **Tags**: the artifact tags. Each tag is a small table with Tag (tag-name display name), Examiner (user name) and Comment.
  2. **Central Repository Comments**: only when CR is enabled. Correlation attributes are built from the artifact (`makeCorrAttrsForSearch`). The CR instances with the same type/value and a non-blank comment are fetched; each shows Case (display name), Comment and Path.
  3. **Artifact Comment**: only for a HASHSET_HIT, INTERESTING_FILE_HIT, INTERESTING_ARTIFACT_HIT or INTERESTING_ITEM that has a non-blank TSK_COMMENT. Shows Set Name and Comment.
  4. **Source File**: an indented block that repeats the file rendering below, using h2 sub-headers. It is removed when empty.
- **For a file**, or the source file of an artifact:
  - Tags, from the file's content tags.
  - Central Repository Comments: CR instances of the FILES type with the file's MD5 that have a comment.
  - Hash Set Hit Comments, Interesting File Hit Comments and Interesting Item Comments. Each is built from the artifacts of that type on the file that have a set name or comment, and shows Set Name and Comment.
- When nothing is rendered, the pane shows the italic message "No annotations were found for this particular item."

**Live refresh.** The viewer listens for these case events:
- BLACKBOARD_ARTIFACT_TAG_ADDED/DELETED
- CONTENT_TAG_ADDED/DELETED
- CR_COMMENT_CHANGED

It also listens for the ingest DATA_ADDED event for HASHSET_HIT, INTERESTING_FILE_HIT and INTERESTING_ITEM. When the event concerns the current artifact or content id and the panel is visible, it rebuilds without moving the caret.

**Quirks:**
- A refresh is dropped if a worker is already running.
- `unregisterListeners` mistakenly calls *add*.

---

## OS Account viewer (`contentviewers/osaccount/`)

**Account resolution.** The node's `OsAccount`. Otherwise the file's `getOsAccountObjectId()`. Otherwise the DataArtifact's OS account id.

**Loading.** A `PanelDataFetcher` SwingWorker runs, showing the plain label "Loading OsAccount Data..." meanwhile. It loads:
- The realm
- The hosts of the account
- The extended attributes, grouped first by host id and then by the attribute source's data source
- The account instances

**Rendering.** A GridBag form. The sections are:
1. **Basic Properties**:
   - Login (login name)
   - Full Name
   - Address: `account.getName()`, effectively the SID or unique address
   - Type (OsAccountType name, e.g. Regular/Service/Interactive)
   - Creation Date (formatted)
   - Object ID
   - The "Administrator" label is defined but not used here.
2. **`<HostName>` Details**: one section per host that has attributes.
   - When several data sources contributed, there is one bold sub-title per data source.
   - Each row is `attribute display name: display string`, with these renames:
     - TSK_COUNT is shown as **Login Count**.
     - TSK_IS_ADMIN is shown as **Administrator**, with the value True/False.
     - TSK_DATETIME_ACCESSED is shown as **Last Login**.
   - Other attributes appear as-is, e.g. password settings, flags, home directory, password hint.
   - Attributes with no host ("Global Host Details") are collected but never rendered, because only non-null host keys are shown.
3. **Realm Properties**:
   - Name (the first realm name, or "Unknown")
   - Address (realm address, e.g. a domain SID prefix)
   - Scope (Local/Domain/Unknown)
   - Confidence (Known/Inferred)

---

## Context viewer (`contentviewers/contextviewer/`)

**Purpose.** Explains where a file came from and how it was used. It is driven entirely by `TSK_ASSOCIATED_OBJECT` artifacts on the file. Each such artifact points via `TSK_ASSOCIATED_ARTIFACT` to the real artifact.

**Two groups, each sorted by date ascending.** Items with no date come first.

**Source** ("how the file arrived"):
- TSK_MESSAGE or TSK_EMAIL_MSG: "Attached to:".
  - For a message the text is "Message From <x> To <y> On <date>", using PHONE_NUMBER_FROM, PHONE_NUMBER_TO and DATETIME.
  - For an email it is "Email From/To/On", using EMAIL_FROM, EMAIL_TO and DATETIME_SENT.
- TSK_WEB_DOWNLOAD or TSK_WEB_CACHE: "Downloaded from:" "URL <url> On <DATETIME_CREATED>".

**Usage** ("how it was used"):
- TSK_RECENT_OBJECT: "Recent Documents:" "Opened at <DATETIME>", or "Opened at unknown time".
- TSK_PROG_RUN: "Program Execution:" "Program Run On <DATETIME>", or " Program Run at unknown time".

**Each entry row** is a `ContextSourcePanel` or `ContextUsagePanel` showing the bold name label, the text, and a **Go to Result** button. The button calls `DirectoryTreeTopComponent.viewArtifact(artifact)`, which navigates the tree and result table to that artifact.

**Text limits.** Each attribute value is abbreviated to 200 chars. The builder capacity is 1024.

**Empty groups** show "Unknown". Because the same `jUnknownPanel` instance is added twice, only one "Unknown" actually renders when both groups are empty.

**Label bug.** The bundle has the header labels swapped (`jSourceLabel=Usage`, `jUsageLabel=Source`). Postmortem should label them correctly as Source, then Usage.

**Threading.** Unlike most viewers, the DB work runs on the EDT; there is no worker.

---

## Other Occurrences viewer (`centralrepository/contentviewer/*`, `centralrepository/application/OtherOccurrences.java`)

**Correlation attributes searched:**

| Node holds | Attributes used |
|---|---|
| OsAccount | `getCorrelationAttributeFromOsAccount`: the account address (SID), when present, as an OS-account correlation attribute |
| Content item | `CorrelationAttributeUtil.makeCorrAttrsForSearch`, applied to the AbstractFile (MD5 and other file attributes), AnalysisResult or DataArtifact (e.g. email, phone, domain, USB id, MAC, IMEI, IMSI, ICCID, wifi SSID, accounts) |
| Fallback | The content of a ContentTag or BlackboardArtifactTag |

**Instance query.** For each attribute, the viewer fetches every CR instance with the same type and value (`getArtifactInstancesByTypeValue`). It excludes the exact same object, meaning the same case, data source name, device id and file object id. The results are keyed by `UniquePathKey`, which is case name + data source + device + file path + case UUID. Totals count instances, unique data sources and cases.

**Layout.** A horizontal split:
- **Left (75% weight)** holds three tables in nested split panes, with dividers at 150 px and 300 px:
  - **Case**: a single column of case display names. It is sorted ascending and its first row is auto-selected.
  - **Data Source Name**: filled from the selected cases. First row auto-selected.
  - **File Name**: the base name of the path, filled from the selected data sources. First row auto-selected.
- **Under the tables**: "Central Repository Starting Date: <earliest case creation date>" (or "Not Available.") and "Found %d instances in %d cases and %d data sources."
- **Right**: the details panel (`OccurrencePanel`).

**Details panel** content depends on what is selected:
- **A file row** shows:
  - **Common Properties**, repeated for each matching instance: Type, Value, Known Status (red when Notable/BAD), Comment (a wrapped read-only text area), and the case date.
  - **File Details**: File Path. Blank when several distinct paths match.
  - **Data Source Details**: Name.
  - **Case Details**: Name and Created Date.
- **A data source** shows case name/date and the data source name.
- **A case** shows case name and created date.

**Placeholder rows** in the Case table:
- "Item has no attributes with which to search." when there are no correlation attributes. This row also appears while loading.
- "No results found."

**Right-click menu** on the Files table:
- **Export all Other Occurrences to CSV**:
  - Opens a save dialog defaulting to `<case>/Export/<yyyyMMddhhmmss>_other_data_sources.csv`.
  - Header: `"Case","Data Source","Matched Attribute","Attribute Value","Known","Path","Comment"`.
  - Every value is quoted with `","` separators and nothing is escaped.
  - Written by a background worker; failure shows the dialog "Error Creating CSV".
- **Show Case Details**: a dialog with the CR case's details (org, examiner, contact, notes).
- **Show Frequency**: a dialog listing "X% of data sources have <value> (type: <type>)" for each correlation attribute, computed with `getFrequencyPercentage`.

The last two items are visible only when CR is enabled and one file row is selected.

**Threading:**
- A `SwingWorker` loads on node change.
- Each case or data-source selection change re-queries *all* correlated instances (`SelectionWorker`) and filters them client-side.
- `populateTableForOneType` lets other UIs, such as Discovery, reuse the panel for one type/value.

**Bug.** Excluding the current case compares `Case.getName()` with the case UUID, which works only because Autopsy case names include the UUID timestamp.

---

## Shared conventions (for consistent egui styling)

- `contentviewers/layout/ContentViewerDefaults` defines:
  - Header font: Label font, bold, +2 pt.
  - Sub-header font: bold.
  - Message font: italic.
  - A monospaced font at the label size.
  - Section indent and section spacing: 1 em in px.
  - Column spacing: em/3.
  - Line spacing: em/5.
  - Panel insets: the `TextPane.margin` default.
- `ContentViewerHtmlStyles` supplies the equivalent CSS classes (header, text, key column, monospace, indented, spaced-section, message) for the HTML-rendered viewers: Metadata, Annotations and Analysis Results.
- Every key/value viewer offers a right-click **Copy** on values. Empty or unavailable states use an italic message line.

---

## Performance/usability problems in this area and egui ideas for postmortem

- **Ruinous `isSupported` cost.** Every node selection runs a DB query per viewer: `hasDataArtifacts`, `hasAnalysisResults`, `getArtifacts(TSK_ASSOCIATED_OBJECT)`, a CR check, and MIME detection that can scan the file synchronously. Postmortem should compute one `ItemCapabilities` struct per selection in a single query. That query would return: has_data_artifacts, has_analysis_results, has_context_links, os_account_id, md5, mime, size, is_dir and has_indexed_text. It runs on a background task, and every viewer decides from it in O(1).
- **HTML-string rendering.** Metadata, Annotations and Analysis Results build HTML that a Swing JTextPane then parses, and istat output is truncated at 50 KB to avoid hangs. In egui, render structured `Vec<Section{title, rows: Vec<(key, value)>}>` with `egui_extras::TableBuilder` or `Grid`. For long istat output, use a virtualized monospace `ScrollArea::show_rows` with no truncation.
- **One-artifact-at-a-time paging in Data Artifacts.** With hundreds of artifacts per file (e.g. a browser DB), Prev/Next is painful, and `getAllDataArtifacts()` loads everything up front. Postmortem should show a left list of the file's artifacts, virtualized and grouped by type, with the detail pane on the right, plus keyboard up/down and a type filter. Attributes should load lazily per artifact.
- **Wasted UI work.**
  - DefaultTable recomputes every row height on each column resize. Use egui wrapped labels with a cached layout (`Galley` cache keyed by width).
  - The Contact photo uses blocking `ImageIO.read` from `getLocalAbsPath` on the EDT, and fails for images inside a disk image. Decode off-thread from the content stream and cache the texture.
- **Unhelpful tie-breaking and opaque preferred-tab logic.** Expose the scores in a debug tooltip. Let users pin a default tab per item kind (file, data artifact, analysis result, keyword hit), not just the global "keep current tab" flag. When the preferred tab is not chosen, still mark which tabs have data, e.g. a dot or count badge on the tab: Data Artifacts (12), Analysis Results (3), Annotations (2).
- **Annotations refresh drops events** while a worker runs. Use a generation counter so the latest request always wins, and subscribe through the app event bus rather than global listeners.
- **Other Occurrences re-queries the CR** on every case or data-source click. Fetch once into an in-memory `Vec<Occurrence>` and filter client-side. Make the three tables multi-select with live counts, and offer a flat "all occurrences" table (case, data source, path, known, comment) with sort and filter.
- **CSV export problems.** The export is not escaped (quotes in paths break it) and it re-queries. Use the `csv` crate with proper quoting and export from the cached set.
- **Context viewer** runs DB work on the UI thread, swaps its Source/Usage labels, and cannot handle more than a few links. Run it async, list entries as clickable rows ("Go to Result" means navigate in the main tree), and add other link types: USB-device-to-file, LNK and jump-list targets, prefetch.
- **OS Account viewer:**
  - It hides host-less (global) attributes. Show them.
  - Show all realm names, and the account's instances per data source (Instance type: Launched/Accessed/Referenced).
  - Add "files owned by this account" and "artifacts referencing this account" quick links.
- **Message viewer:**
  - The auto-selected body tab is "last non-empty", which can hide the plain text. Prefer HTML, then Text, then RTF, and remember the user's last choice.
  - HTML must stay sandboxed with no remote fetch. A pure-Rust HTML-to-text or limited renderer is enough for egui; keep an "Open externally" escape hatch.
  - Show attachment thumbnails inline.
- **Persona lookups** are CR round-trips per account, run on every selection. Batch them and cache per account id for the session, invalidating on persona edit events.
- **Inconsistent widgets.** Message, Contact, CallLog and OS Account are each hand-rolled GridBag layouts. Postmortem should provide one reusable "property sheet" widget (sections, key/value rows, copy-on-right-click, copy-section-as-text, monospace option, link values that navigate) and build every artifact viewer from declarative per-type field specs, like the ordering tables above.
- **Popout and comparison.** Every viewer should be renderable in a separate egui viewport. "View in New Window" should open a viewport bound to a fixed item id, so two artifacts or files can be compared side by side.

---

## File-content viewers (Hex, Text, Application/File viewers, Registry, Translation)

This section covers the content-viewer tabs that render the *bytes or text of a file*: Hex, Text (with its Strings / Extracted Text / Translation sub-tabs) and Application (the MIME-dispatched per-format viewers). Artifact/metadata/annotation/context/other-occurrence viewers are covered elsewhere.

### Registered viewers (complete list in this Autopsy tree)

Top-level `DataContentViewer`s (NetBeans `@ServiceProvider`, the `position` sets tab order left to right):

| pos | Tab title | Class | Notes |
|---|---|---|---|
| 1 | **Hex** | `corecomponents/DataContentViewerHex` | priority always 1 |
| 2 | **Text** | `contentviewers/textcontentviewer/TextContentViewer` | container for `TextViewer` sub-tabs |
| 3 | **Application** | `contentviewers/FileViewer` | MIME → `FileTypeViewer` dispatch; priority 5 when supported |
| 4 | File Metadata | `contentviewers/Metadata` | (other section) |
| 5 | OS Accounts | `contentviewers/osaccount/OsAccountViewer` | (other section) |
| 6 | Data Artifacts | `corecomponents/DataArtifactContentViewer` | (other section) |
| 7 | Analysis Results | `contentviewers/analysisresults/AnalysisResultsContentViewer` | (other section) |
| 8 | Context | `contentviewers/contextviewer/ContextViewer` | (other section) |
| 9 | Annotations | `contentviewers/annotations/AnnotationsContentViewer` | (other section) |
| 10 | Other Occurrences | `centralrepository/contentviewer/DataContentViewerOtherCases` | (other section) |
| – | (Sample) | `examples/SampleContentViewer` | registration commented out; example only |

`TextViewer` sub-tabs inside the Text tab (`corecomponentinterfaces/TextViewer`, ordered by position):

| pos | Sub-tab | Class | Priority |
|---|---|---|---|
| 1 | **Strings** | `contentviewers/textcontentviewer/StringsTextViewer` | 1 |
| 2 | **Extracted Text** | `KeywordSearch/.../keywordsearch/ExtractedTextViewer` | 4 (constant) |
| 4 | **Translation** | `texttranslation/ui/TranslatedTextViewer` | 0 |

`FileTypeViewer`s inside the Application tab (`contentviewers/FileTypeViewer`, package-private interface: `getSupportedMIMETypes()`, `setFile(AbstractFile)`, `getComponent()`, `resetComponent()`, `isSupported(AbstractFile)`). Registration order in `FileViewer.KNOWN_VIEWERS` (first registration of a MIME type wins; duplicates are logged):

1. `SQLiteViewer` 2. `PListViewer` 3. `MediaFileViewer` (images + audio/video) 4. `HtmlViewer` 5. `WindowsRegistryViewer` 6. `PDFViewer`

**Important gap / non-existent viewers:** there is *no* JSON viewer, Windows Event Log (.evtx) viewer, .lnk viewer, plain-text/syntax-highlighted text viewer, Office document viewer, archive browser or EXIF panel among content viewers in this source tree (grep of all modules for `DataContentViewer`, `TextViewer`, `FileTypeViewer` implementations finds only the classes above). Plain text files are seen only through Strings / Extracted Text. postmortem should treat JSON, EVTX, LNK, text/code, Office and archive viewers as *new* features, not parity items.

### Viewer priority scale (`contentviewers/utils/ViewerPriority`)

Enum `LevelZero(0) … LevelSeven(7), Level(8)`. Used values: Hex 1, Strings 1, Translation 0, Extracted Text 4, Text tab = max(1, max of its supported sub-viewers), Application 5 if a `FileTypeViewer` supports the file else 0 (also 0 for `application/octet-stream`). The panel switches to the highest-priority enabled tab unless the user preference **"Keep current viewer"** (`UserPreferences.keepPreferredContentViewer()`, key `KeepPreferredViewer`, default `false`) is set, in which case the currently selected tab index is retained. The same rule is applied independently to the Text tab's sub-tabs.

Consequences: a JPEG opens on Application (5); a Word document with indexed text opens on Text → Extracted Text (4); an unknown binary opens on Hex (first tab among priority-1 ties, since only strictly greater priority replaces the current pick).

---

## Hex viewer (`corecomponents/DataContentViewerHex.java`)

**Supported:** node's default content (see below) is non-null, *not* a `BlackboardArtifact`, and `size > 0`. `DataContentViewerUtility.getDefaultContent(node)` returns the first non-artifact `Content` in the node lookup, else the artifact — so selecting an artifact row shows the hex of its *source file*.

**Layout:** top control strip (inside a non-scrolling scroll pane), then a read-only monospaced `JTextArea` (font "Monospaced" 11 pt, tab size 0) filling the rest.
Control strip, left to right: `Page: <cur> of <total>` · `Page` ◀ ▶ (icon buttons with hover/disabled variants) · `Go to Page: [____]` (79 px) · `Jump to Offset [____]` (79 px) · `[Launch in HxD]`. All controls hidden until the first page loads.

**Paging:** `PAGE_LENGTH = 16384` bytes (16 KiB). `totalPages = ceil(size / 16384)` (computed as `round((size-1)/16384)+1` with integer division). Page N reads `[ (N-1)*16384, +16384 )`.

**Line format** (`datamodel/DataConversion.byteArrayToHex`): 16 bytes per line, xxd-like:
`0x%08x: ` + hex pairs separated by one space, an extra space after every 4th byte and another after the 8th byte (so groups of 4 with a double gap in the middle), padding with spaces for a short last line, two spaces, then 16 ASCII chars where bytes outside 32..126 render as `.`. Hex digits are upper-case. Offsets are absolute file offsets, lower-case hex, zero-padded to 8 digits (wider for files ≥ 4 GiB).

**Navigation behaviour**
- Prev/Next: enabled only when no read error and page in range; after paging the caret is reset to the top and the "Go to page" field is updated.
- Go to Page: Enter in the field; non-numeric or out-of-range → warning dialog *"Please enter a valid page number between 1 and {total}"* (title *Invalid page number*).
- Jump to Offset: parsed with Java `Long.decode` → accepts decimal, `0x`/`0X`/`#` hex, and **leading-zero octal** (quirk). A leading `+`/`-` makes it **relative to the offset of the line the caret is on** (parses the `0x…:` prefix of the caret line). Invalid → dialog *"Invalid Offset: {text}"*. Negative result → text area shows *"Cannot jump to the resultant offset"*.
  - Quirk: the jump reads 16 KiB **starting exactly at the requested offset** (not page-aligned) and labels it page `offset/16384+1`; subsequent Prev/Next snap back to page-aligned boundaries.
- Read error or 0 bytes read → text area shows *"(offset X-Y could not be read)"* and both nav buttons disabled.
- Loading: background `SwingWorker`; text area shows *"Loading hex from file..."*, wait cursor. A new selection cancels the in-flight worker.
- Right-click menu on text: **Copy**, **Select All** (copies the formatted hex dump text, not raw bytes).
- `currentPage` is a `static` field → shared between the main viewer and popped-out instances (bug).

**Launch in HxD:** enabled only on Windows. Executable path from Options (`UserPreferences.getExternalHexEditorPath()`, default `C:\Program Files\HxD\HxD.exe`). Copies the whole file to `<case>/Temp/<objId><escapedName>` with a cancellable progress handle *"Copying file to open in HxD..."*, then launches `"<exe>" "<tempfile>"`. Missing/non-executable path → *"Unable to launch HxD Editor. Please specify the HxD install location in Tools -> Options -> External Viewer"*. Temp file deleted on cancel/launch failure.

---

## Text tab container (`contentviewers/textcontentviewer/TextContentViewer.java`, `TextContentViewerPanel.java`)

- Title *Text*, tooltip *"Displays text associated with the selected item"*.
- Not supported for directories or zero-size files; otherwise supported if any sub-viewer supports the node.
- Inner `JTabbedPane` with one tab per `TextViewer` (each wrapped in a scroll pane with vertical scrollbar disabled — the sub-viewer scrolls itself). Unsupported sub-tabs are *disabled* (greyed), not removed.
- Lazy loading: each sub-viewer is wrapped in `UpdateWrapper` with an `outdated` flag; on node change all are reset, only the selected one is populated; switching to an outdated tab populates it then (wait cursor).
- Panel name is set to the content's unique path (used as window title when popped out).
- The main instance uses the singleton sub-viewers; `createInstance()` (pop-out) creates fresh ones.

---

## Strings sub-viewer (`textcontentviewer/StringsTextViewer.java`, `StringsContentPanel.java`; engine `coreutils/StringExtract.java`)

- Title *Strings*, tooltip *"Displays ASCII and Unicode strings extracted from the file."* Supported for any non-artifact content with size > 0. Also renders `StringContent` nodes (e.g. some virtual nodes) by showing `getString()` as-is, unpaged.
- **Layout:** wrapping control strip (`WrapLayout`, script selector right-aligned): `Page: x of N` · `Page` ◀ ▶ · `Go to Page: [__]` · … · `Script: [combo]`. Below: read-only text pane with Copy / Select All context menu.
- **Paging:** 16384-byte pages of *raw bytes* (same as hex); total pages = `ceil(size/16384)`. Each page is extracted independently, so strings spanning a page boundary are split/lost (edge case). Messages: *"(offset X-Y contains no text)"* for a page with no strings, *"(offset X-Y could not be read)"* on read failure; *"Loading text..."* while working. Go-to-page validation identical to Hex.
- **Script combo** (tooltip *"Language to attempt when interpreting (extracting and decoding) strings from binary data"*). Items in order, default = first (**Latin - Basic**):
  `Latin - Basic` (English), `Latin - Extended` (European), Arabic, Cyrillic, Han, Hiragana, Katakana, Hangul, Armenian, Bengali, Khmer, Ethiopic, Georgian, Hebrew, Lao, Mongolian, Thai, Tibetan. Only *one* script is enabled at a time; changing it re-extracts the current page. Selecting Latin-Extended implicitly also enables Latin-Basic.
- **Extraction algorithm** (needed for parity):
  - Minimum string length `MIN_CHARS_STRING = 4` characters.
  - Both UTF-8 and UTF-16 decoding always on. At each even offset, UTF-16 is tried in **both byte orders** and the longer run wins; UTF-8 is tried at every offset; the encoding producing more characters wins. If neither yields ≥4 chars, advance 1 byte (2 if UTF-8 off).
  - Pairs of `0x00 0x00` are skipped fast.
  - A run "locks" into the script of its first non-generic character; characters must be either generic (COMMON / punctuation etc. per `StringExtractUnicodeTable`, a 65536-entry BMP → script table loaded from `StringExtract.properties`) or the same locked script; any other character ends the string.
  - UTF-8 validation follows Unicode 5.0 well-formed byte sequences (rejects overlongs 0xC0/0xC1, surrogates, etc.).
  - Output: one string per line (`\n` separated); no offsets are shown.
- Static `currentOffset`/`currentPage` (shared across instances – bug).

---

## Extracted Text sub-viewer (KeywordSearch module: `ExtractedTextViewer.java`, `ExtractedContentPanel.java`, `HighlightedText.java`, `SolrIndexedText.java`, `AccountsText.java`, `FileReaderExtractedText.java`, `TextZoomPanel.java`)

Title *Extracted Text*; tooltip *"Displays extracted text from files and keyword-search results. Requires Keyword Search ingest to be run on a file to activate this viewer."* Priority constant 4.

**Supported when any of:**
- node carries an ad-hoc keyword search result (`AdHocQueryResult`);
- node is a `TSK_KEYWORD_HIT` artifact, or a `TSK_ACCOUNT` artifact whose account type is `CREDIT_CARD`;
- node's file is fully indexed in Solr (`Server.queryIsFullyIndexed(objId)`, cached in a 10-entry LRU cleared on case change);
- node's file can be extracted on the fly: not virtual dir / unalloc / unused blocks / carved, not a dir, size > 0, MIME not an archive type and not `application/octet-stream`, and a `TextExtractor` exists for it;
- node's artifact or `Report` is fully indexed.

**"Text Source" combo** — the list of `ExtractedText` sources built per node, default selection = first:
1. **Search Results** (`HighlightedText`) for keyword hits / ad-hoc results; or credit-card **Credit Card Number(s)** (`AccountsText`).
2. **File Text** — `SolrIndexedText` (indexed chunks) if fully indexed, else `FileReaderExtractedText` (live Tika/text extraction, forward-only).
3. **Result Text** — `SolrIndexedText` of the artifact (or the hit's associated artifact via `TSK_ASSOCIATED_ARTIFACT`).
The "current source" for control state prefers highlighted > artifact text > file text.

**Layout:** wrapping control strip: `Text Source: [combo]` · `Page: x of N` · `Page` ◀ ▶ · `Matches on page: x of N` · `Match` ◀ ▶ · zoom panel (`−` `[100%]` `+` `Reset`). Below: HTML `JTextPane` (custom `HTMLEditorKit` that allows breaking long inline runs → text wraps), right-click Copy / Select All.

**Pages:** one page = one Solr chunk. Chunker (`Chunker.java`): base chunk read to ≥ 30 KiB, extended up to 31 KiB to the next whitespace, hard cap 32 760 bytes (plus overlap "window" text used for cross-chunk hits). `SolrIndexedText` pages through all chunks; `HighlightedText` navigates **only pages that contain hits** (page set = chunk IDs with hits; "N" shows total chunks); `FileReaderExtractedText` has unknown page count (`-1`, shown as "-"), forward-only (no previous page). Extracted metadata (Tika metadata map) is appended after the file text.

**Highlighting:** query re-run against Solr per page with highlighting (`hl.fragsize=0` – whole chunk), FastVectorHighlighter unless index is old; hits wrapped in `<span style='background:yellow'>` and each hit given an anchor `<prefix>_<n>`; manual case-insensitive substring highlighting fallback if Solr returns no highlight. Language-specific (schema ≥ 2.2) fields handled; regex hits highlight the literal matched terms. Text wrapped in `<html><pre>…</pre></html>`.

**Match navigation:** ▶ moves to next hit on page; if none left, advances to the next page with hits and scrolls to its first anchor; ◀ symmetrical. Buttons enable based on `hasNext/PreviousItem || hasNext/PreviousPage`. After loading a page the view scrolls to the current hit anchor.

**Text direction:** first ≤1024 chars after `<pre>` are un-escaped and analysed (`TextUtil.getTextDirection`) → RTL orientation applied for Arabic/Hebrew text.

**Loading:** `SetMarkupWorker` with an indeterminate progress handle *"Loading text for {name}"*; placeholder *"Loading text... Please wait"* (italic). Errors: *"Error retrieving text."*; *"No text available for this file."*; *"This file is a known file (based on MD5 hash) and does not have indexed text."* (when skip-known enabled and file is KNOWN).

**Zoom (`TextZoomPanel`):** steps 6.25, 12.5, 25, 37.5, 50, 75, **100** (default), 150, 200, 250, 300, 400, 500, 600, 800, 1000 %. Sets body/pre font-size = default label font size × step; re-scrolls to last anchor after resize.

---

## Translation sub-viewer (`texttranslation/ui/TranslatedTextViewer.java`, `TranslationContentPanel.java`, `TranslateTextTask.java`)

- Title *Translation*, tooltip *"Displays translated file text."* Priority 0 (never auto-selected). Supported only when a translation provider is configured (`TextTranslationService.hasProvider()`) and the node has an `AbstractFile`.
- **Layout:** top strip: `Show: [Translated Text | Original Text]` (default *Translated Text*, first item), `OCR: [Autodetect language | <installed Tesseract packs>]` (enabled only if *Use OCR in translation* option is on — `UserPreferences.getUseOcrInTranslation()`, default `true`), warning label *"Up to the first {N}KB of text will be translated"* (N = provider max chars/1000; Google and Bing both 5000 chars → "5KB"). Body: auto-wrapping read-only text pane.
- OCR language list: Tesseract 3-letter codes mapped to names (≈100 entries, e.g. `eng`→English, `chi_sim`→Chinese - Simplified, `deu`→German); only installed packs (`PlatformUtil.getOcrLanguagePacks()`) shown, sorted by code, last selection remembered for the session.
- Pipeline (background single-thread executor, previous task cancelled on any combo change):
  1. *"Extracting text, please wait..."* (italic). Text extraction via `TextExtractorFactory.getExtractor(file, ctx)`; falls back to the strings extractor if no extractor. If OCR enabled, an `ImageConfig` with OCR on and the chosen language is passed. Images with OCR disabled → message *"OCR is not enabled. To change, go to Tools->Options->Machine Translation"*.
  2. Text truncated to **25 600 bytes** of UTF-8 (`MAX_EXTRACT_SIZE_BYTES`) (and further limited by the provider).
  3. If "Translated" selected: *"Translating text, please wait..."* then provider call.
  4. Display with auto-detected direction (first 1024 chars). Errors (italic): *"File has no text."*, *"The machine translation software was not found."*, *"An error occurred while translating the text ({msg})."*, *"The machine translation software did not return any text."*, *"An error occurred while extracting the text ({msg})."*

### TranslatablePanel (`contentviewers/TranslatablePanel.java`)

Reusable wrapper (used by the Message artifact viewer) that adds a combo `Original Text | Translated Text` (default Original) plus a status label with warning icon above any `ContentComponent`. Combo disabled if no provider. Translation is run once per content and cached; switching back and forth uses the cache. Status shows progress/error text. Same `TranslateTextTask` messages as above.

---

## Application tab dispatcher (`contentviewers/FileViewer.java`)

- Title *Application*, tooltip *"Displays file contents."*
- Supported if node has an `AbstractFile` that is not a directory, its MIME type (from DB; if empty, detected on the fly with `FileTypeDetector`) is not `application/octet-stream`, and the MIME→viewer map has an entry whose `isSupported(file)` is true. **Note:** `getSupportingViewer` uses `file.getMIMEType()` (DB value) even when the fallback detection was used, so files with no stored MIME type never get an Application view.
- On `setNode` the previous `FileTypeViewer` is reset, the new viewer's component is placed in the panel (only one at a time). Priority 5 when supported.

### MIME types per FileTypeViewer

| Viewer | MIME types | Extra `isSupported` rule |
|---|---|---|
| SQLiteViewer | `application/x-sqlite3` | always true |
| PListViewer | `application/x-bplist` (binary plist only; XML plists are not routed here) | always true |
| MediaFileViewer – images | all `ImageIO.getReaderMIMETypes()` (JRE + JAI ImageIO 1.1 plugins: typically jpeg, png, gif, bmp, wbmp, tiff, jpeg2000, pnm/raw) + `image/x-rgb`, `image/x-ms-bmp`, `image/x-portable-graymap`, `image/x-portable-bitmap`, `image/webp`; `application/octet-stream` removed | `ImageUtils.isImageThumbnailSupported`: is a file with size>0 AND (extension in ImageIO suffixes + `tec`, minus `db`; OR detected MIME starts with `image/`; OR in set; OR header is JPEG `FF D8` (size ≥100) or PNG) |
| MediaFileViewer – audio/video | `video/3gpp`, `video/3gpp2`, `audio/aiff`, `audio/amr-wb`, `audio/basic`, `audio/mp4`, `video/mp4`, `audio/mpeg`, `video/mpeg`, `audio/mpeg3`, `application/mxf`, `application/ogg`, `video/quicktime`, `audio/vorbis`, `audio/vnd.wave`, `video/webm`, `video/x-3ivx`, `audio/x-aac`, `audio/x-adpcm`, `audio/x-alaw`, `audio/x-cinepak`, `video/x-divx`, `audio/x-dv`, `video/x-dv`, `video/x-ffv`, `audio/x-flac`, `video/x-flv`, `audio/x-gsm`, `video/x-h263`, `video/x-h264`, `video/x-huffyuv`, `video/x-indeo`, `video/x-intel-h263`, `audio/x-ircam`, `video/x-jpeg`, `audio/x-m4a`, `video/x-m4v`, `audio/x-mace`, `audio/x-matroska`, `video/x-matroska`, `audio/x-mpeg`, `video/x-mpeg`, `audio/x-mpeg-3`, `video/x-ms-asf`, `audio/x-ms-wma`, `video/x-ms-wmv`, `video/x-msmpeg`, `video/x-msvideo`, `video/x-msvideocodec`, `audio/x-mulaw`, `audio/x-nist`, `audio/x-oggflac`, `audio/x-paris`, `audio/x-qdm2`, `audio/x-raw`, `video/x-raw`, `video/x-rle`, `audio/x-speex`, `video/x-svq`, `audio/x-svx`, `video/x-tarkin`, `video/x-theora`, `audio/x-voc`, `audio/x-vorbis`, `video/x-vp3`, `audio/x-w64`, `audio/x-wav`, `audio/x-wma`, `video/x-wmv`, `video/x-xvid` | extension must be one of `.3g2 .3gp .3gpp .aac .aif .aiff .amr .asf .au .avi .flac .flv .m4a .m4v .mka .mkv .mov .mp2 .mp3 .mp4 .mpeg .mpg .mxf .ogg .wav .webm .wma .wmv` **and** detected MIME in the list; disabled entirely if GStreamer failed to load |
| HtmlViewer | `text/html`, `application/xhtml+xml` | always true |
| WindowsRegistryViewer | `application/x.windows-registry` | size>0, extension not starting with `log` (excludes `.LOG1/.LOG2` transaction logs), and first 16 KiB parse as a valid REGF header |
| PDFViewer | `application/pdf` | MIME equals `application/pdf` |

---

## Media viewer (`contentviewers/MediaFileViewer.java`)

Card layout with two cards: IMAGE (`MediaViewImagePanel`) and AUDIO_VIDEO (`MediaPlayerPanel`). Audio/video check runs first, then image. Re-selecting the same file is a no-op. If GStreamer init fails at construction a notification *"Error initializing gstreamer for audio/video viewing and frame extraction capabilities. Video and audio viewing will be disabled."* is shown and only images work.

### Image panel (`contentviewers/MediaViewImagePanel.java` + `imagetagging/*`)

- JavaFX `ImageView` inside a `ScrollPane` (scrollbars as needed), smooth scaling; requires JavaFX initialised.
- **Toolbar:** Rotate Left, Rotate Right, rotation read-out (e.g. `90°`), Zoom Out, Zoom In, zoom read-out (e.g. `37%`), **Reset**, and a **Tags Menu** dropdown.
- **Loading:** background `ReadImageTask` with a masker pane + progress bar bound to ImageIO read progress and message *"Reading image: {name}"*; previous load cancelled. GIFs are read with JavaFX to keep animation; `.tec` files are scanned for the first JPEG SOI `FF D8` in the first 1 KiB and decoded from there; everything else through ImageIO (first image only). Failure → centred message *"Could not load file into Media View."* (or *"…: insufficent memory."* on OOM) plus a button **"Open in External Viewer  Ctrl+E"** (exports to temp and opens with the OS/user-configured external viewer).
- **Fit:** on load and on Reset, zoom = min(panelW/imgW, panelH/imgH) (fit whole image, *can upscale small images*), rotation reset to 0, scroll to top-left, "auto-resize" mode on — window resizes re-fit. After any manual zoom/rotate, auto-resize is off and resizes keep zoom.
- **Zoom steps:** 6.25, 12.5, 25, 37.5, 50, 75, 100, 150, 200, 250, 300, 400, 500, 600, 800, 1000 %. Zoom In jumps to the next step strictly greater than the current ratio (so from a fit ratio of 0.43 → 0.5); Zoom Out to the next lower. Buttons disabled at 6.25 % / 1000 %. No mouse-wheel zoom, no drag-to-pan (scrollbars only).
- **Rotate:** ±90° steps, mod 360; pivot at image centre; viewport swaps width/height for 90/270.
- **Image region tagging** (only enabled on **Windows with OpenCV loaded**; menu greyed otherwise):
  - Tags Menu items: **Create**, **Delete**, **Hide/Show**, **Export** (popup 300×150 px), enabled per state machine (EMPTY/NONEMPTY/SELECTED/HIDDEN/VISIBLE/CREATE/DISABLE).
  - Create: user drags a red rectangle on the image (left button). Stroke width = 1.5 % of the image's smaller dimension; rectangles smaller than the stroke are discarded; clamped to the image bounds. On release the standard *Tag Name & Comment* dialog appears; on OK a **content tag** on the file is created plus a row in table `beta_tag_app_data` (`content_tag_id`, JSON app data = `{x,y,width,height,strokeThickness}` in image pixel coordinates).
  - Existing region tags are drawn as red rectangles with a tooltip of the tag name. Click selects the smallest (min width+height) tag under the cursor; selected tag goes 50 % opacity and shows 8 circular drag handles (corners + edge midpoints) to resize within image bounds; on mouse release the new region is persisted.
  - Delete removes the selected region tag *and* its content tag.
  - Hide/Show toggles visibility of all region tags.
  - Export: choose a directory (defaults to case Export folder) → writes `<basename>-with_tags.png` with all regions burned in (OpenCV rectangle, colour BGR(0,0,255)=red, stroke rounded); success/failure message dialog.
  - Thumbnails with tags: `ImageTagsUtil.getThumbnailWithTags` (used by the thumbnail result viewer) – square resize to icon size.

### Audio/Video player (`contentviewers/MediaPlayerPanel.java`, `JavaFxAppSink.java`, `utils/GstLoader.java`)

- Backend: GStreamer (gst1-java) `PlayBin` with a custom `AppSink` → JavaFX `ImageView` (caps `video/x-raw, format=BGRx` on little-endian; aspect preserved, fit to panel). `GstLoader.tryLoad()` initialises once per session; on failure playback disabled for the session.
- **Not supported:** files whose name flag is UNALLOC (deleted) → info label *"Playback of deleted videos is not supported, use an external player."*
- **Buffering:** the whole file is first copied to `<case>/Temp/videos/<objId>.<ext>` (skipped if already present with full size) with a cancellable progress *"Buffering {name}"*; label shows *"Buffering..."*. Then the pipeline is created in PAUSED state (first frame shown), controls enabled.
- **Controls (bottom bar):** video area; progress slider (0–2000 resolution; drag seeks with FLUSH|ACCURATE, pauses while dragging then resumes if it was playing; click-to-position track); time label `HH:MM:SS/HH:MM:SS` (or `Unknown`); Rewind (−30 s, clamped at 0); Play/Pause toggle (icon swaps); Fast-forward (+30 s; if <30 s left jumps halfway to end; no-op in last 2 s); Volume slider 0–50 (default 25 → volume = value×2/100, i.e. 0.0–1.0, default 0.5); `Speed:` combo `0.25x 0.50x 0.75x 1x 1.25x 1.50x 1.75x 2x` (default **1x**), applied via seek with rate.
- Position refresh timer every 16 ms. End-of-stream → seek to 0 and pause. Pipeline error → controls disabled, red text *"The media player cannot process this file."* Reset on node change stops/disposes the pipeline and cancels buffering. Default info label *"File not supported."* after reset.

## HTML viewer (`contentviewers/HtmlViewer.java`, `HtmlPanel.java`)

- Reads the **entire file** into memory (`int` size), detects charset with ICU4J `CharsetDetector` (fallback platform default). Errors: *"This file is missing or unreadable."*, *"This file has unsupported encoding"*.
- Renders in JavaFX `WebView` with **JavaScript disabled**, context menu disabled, and all `<a>` click events cancelled after load (links not followable).
- **Sanitising (default, "images off"):** via Jericho parser removes: any tag whose text contains `background-image`; all `img`, `frame`, `iframe`, `picture`, `svg`, `audio`, `video`, `track`, `embed`, `link` elements; and **all URI-bearing attributes** (`href`, `src`, `action`, etc.). Prevents any network fetch.
- Toggle button **"Download Images"** / **"Hide Images"**: when pressed, loads the *raw* HTML (remote resources will be fetched from the internet). Resets to off on each new file. If rendering throws → plain-text *"The HTML text cannot be displayed, it may not be correctly formed HTML."*
- `HtmlPanel` is reused by artifact viewers (e.g. email HTML body).

## PDF viewer (`contentviewers/PDFViewer.java`)

- IcePDF `SwingController` + `SwingViewBuilder` full viewer UI (toolbar: page navigation, zoom, fit, rotate, search, text selection; utility pane with outline/thumbnails). Disabled: Save, Open, Print, annotation toolbar and annotation utility pane, local-storage dialogs. Keyboard bindings installed.
- Document loaded in background from a streaming `ReadContentInputStream`; UI disabled until loaded. Opens in **one-column (continuous) view**, text-selection tool.
- Encrypted PDFs: password prompt (IcePDF dialog, run on EDT); if security exception → *"This document is password protected."* Other failures → *"An error occurred while opening this PDF document. Check the logs for more information. You may continue to use this feature on other PDF documents."*

## SQLite viewer (`contentviewers/SQLiteViewer.java`, `SQLiteTableView.java`, `SQLiteTableRowFactory.java`, `EpochTimeCellRenderer.java`; reader `coreutils/SQLiteTableReader.java`)

- On open: copies the DB to `<case>/Temp/<objId><name>` and also copies sibling `<name>-wal` and `<name>-shm` from the same parent directory (not for files under `/$CarvedFiles/`), then opens via JDBC (so the WAL is replayed — note: this means uncheckpointed WAL rows *are* shown; there's no option to view DB without WAL). Worker runs in background; main window wait cursor.
- **Layout:** top strip `Table [combo]` · `<N> entries` · `Page x of N` ◀ ▶ · **[Export to CSV]**; below an Outline table.
- Table list: `SELECT name FROM sqlite_master WHERE type='table'` (views not listed; includes `sqlite_sequence` etc.). Empty → combo shows *"No tables found"* and disabled.
- Selecting a table: `SELECT count(*)`, then page 1. **Page size 100 rows**; pages = `rows/100 + 1` (quirk: shows an extra empty page when rows is an exact multiple of 100). Rows read with `SELECT * FROM "<t>" LIMIT 100 OFFSET n` — no ORDER BY.
- Table display: columns from the result set, auto-sized from first 20 rows (min 50 px, max 500 px, +16 px padding), horizontal and vertical scrollbars always on, auto-resize off, **cell** selection (single), BLOBs shown as *"BLOB Data not shown"*, NULL as empty. Sorting by header click is available (Outline) but only within the current 100-row page. Empty table → header row with blank cells.
- Right-click on a row → **Display as ▸ Date / Raw Data** for the selected column: Date renders numeric values as `yyyy/MM/dd HH:mm:ss` in *local* time, **interpreting value as milliseconds since epoch** (seconds-based timestamps display as 1970), italic; non-numbers or ≤0 stay raw.
- **Export to CSV:** save dialog defaulting to case Export folder, filename = table name, `*.csv` filter; overwrite confirmation *"Do you want to overwrite the existing file?"*; writes all rows of the table (not just the page), every field double-quoted, comma separated, `\n` line end, **embedded quotes not escaped** (bug), platform default charset, BLOBs as placeholder; always appends `.csv` to the chosen name (so `x.csv` → `x.csv.csv`). Errors → *"Failed to export table content to csv file."*
- Other errors: *"Error getting row count for table: {t}"*, *"Error getting rows for table: {t}"*.

## Binary plist viewer (`contentviewers/PListViewer.java`, `PListRowFactory.java`)

- Reads whole file, parses with dd-plist `PropertyListParser` (handles binary; XML/ASCII too if routed). Tree-table (Outline) with columns **Key | Type | Value**; root not shown; top level = root dict keys or root array items (array items have empty key).
- Types: STRING, NUMBER (integer → long, real → float), BOOLEAN, DATE (`NSDate.toString()`), DATA (value *"Binary Data value not shown"*), ARRAY, DICTIONARY (containers have children and empty Value). Icons: key-16 (leaf), keychain-16 (array), keys-dict-16 (dict).
- Column widths from first 20 rows (cap 2000 px). Row selection off.
- **Export** button → save as XML plist (`.xml` appended if missing, default dir = case Export folder); success *"Plist file exported successfully to {name}"* / failure dialogs. Parse errors → modal dialog *"Error while parsing/displaying plist file."* with the exception message.

## Windows Registry viewer (`contentviewers/WindowsRegistryViewer.java` + `rejview/*`)

- Reads the **entire hive into a byte array** and parses with `com.williballenthin.rejistry` (`RegistryHiveBuffer`).
- **Layout (`RejView`):** horizontal split pane (one-touch expandable, continuous layout; left tree preferred width 350 px). Left: `JTree` of the hive, root expanded; children loaded lazily on expand — **subkeys first, then values** as leaf nodes. Key node label = key name; value node label = value name or `(Default)`; parse failures shown as `PARSE FAILED`. Root parse failure → single `PARSE FAILED` node. Right: view for the selected node (divider position preserved).
- **Key view (`RejTreeKeyView`):** "Metadata" titled box: *Name:* **name**, *Number of subkeys:* n, *Number of values:* n, *Modification Time:* `yyyy-MM-dd HH:mm:ss z` in **GMT**. Below, "Values" titled table **Name | Type | Value** (sortable by click, row selection, auto column widths) with values formatted by `RegeditExeValueFormatter`:
  - REG_SZ / REG_EXPAND_SZ: string, empty → `(value not set)`, truncated to 48 chars with `...`;
  - REG_MULTI_SZ: items joined with `, `, truncated to 48;
  - REG_DWORD / REG_BIG_ENDIAN: `0x%08x (%d)`; REG_QWORD: `0x%016x (%d)`;
  - others (REG_BINARY, REG_NONE, …): first 16 bytes as hex pairs separated by spaces, `...` if longer.
- **Value view (`RejTreeValueView`):** "Metadata" box (*Name:*, *Type:*) and "Value" box: SZ/EXPAND_SZ as text, MULTI_SZ one per line, DWORD/QWORD/BIG_ENDIAN as `0x%x`; everything else in the mini **HexView**.
- **HexView (`rejview/HexView.java`):** Monospaced 12 pt; three synchronised columns: offsets (`0x%x`, every 16 bytes), hex (upper-case pairs + space) and ASCII (printable 0x20–0x7E else `.`); hex/ASCII in a split pane. Selecting in either column highlights the corresponding bytes in the other and the status bar shows *"Selection: {start} to {end} (len: {n}) [0x… to 0x… (len: 0x…)"* or *"Position: {n} [0x…]"*.
- No search, no export, no deleted-key/slack recovery, no transaction-log replay (`.LOG*` files excluded from viewer).

## Shared helpers

- `contentviewers/Utilities.configureTextPaneAsHtml/Rtf` – HTML editor kit with 14 pt Arial/CJK-capable font stack for body/p/li/td/th.
- `contentviewers/layout/ContentViewerDefaults` – global metrics: default font = LAF label font; header font bold +2 pt; sub-header bold; message font italic; monospaced at default size; section indent & spacing = default font size in px; column spacing = 1/3 font px; line spacing = 1/5 font px; panel background = LAF panel bg; insets = LAF TextPane margin.
- `contentviewers/layout/ContentViewerHtmlStyles` – CSS classes (`header`, `subHeader`, `message` (italic), `text`, `monospaced`, `indent`, `spacedSection`, `keyKolumn` (nowrap, right padding)) applied to HTML JTextPanes used by metadata/annotation/etc. viewers. postmortem should define equivalent egui text styles (Heading, SubHeading, Message/italic-weak, Body, Monospace) and spacing constants once.

---

## Performance / usability problems (file viewers) and egui improvement ideas

- **Whole-file reads into RAM:** HTML, binary plist, registry hive (can be hundreds of MB for SOFTWARE/NTUSER), and the full temp copy of every video/audio/SQLite file before display. *postmortem:* memory-map or stream via the Rust FS layer; registry parser (e.g. `nt-hive`/custom) working on a `Read+Seek` over the image with lazy cell access; render SQLite by opening read-only through a VFS over the in-image file (no temp copy), with WAL applied optionally (toggle "with WAL / without WAL" — forensically valuable).
- **Fixed 16 KiB paging in Hex/Strings** with text-area rendering: no continuous scrolling, jump-to-offset not page-aligned, page-boundary strings lost, copy gives formatted text. *postmortem:* virtualised hex view (`egui::ScrollArea::show_rows`) over the whole file with O(1) row → offset mapping, byte-level selection with synchronised hex/ASCII highlight, status bar (offset dec/hex, selection length), data inspector panel (int8..64 LE/BE, float, FILETIME, Unix time, GUID, UTF-16), go-to offset with hex/dec explicit syntax (no octal surprise), search for hex/ASCII/UTF-16 patterns, bookmarks, "copy as hex / C array / raw bytes", "export selection".
- **Strings:** per-page extraction with no offsets shown and single-script selection. *postmortem:* background streaming string scan of the whole file (rayon, cancellable) producing `(offset, encoding, text)` rows in a virtualised table, min-length option (default 4), encoding filters (ASCII/UTF-8/UTF-16LE/BE), script filter multi-select, filter-as-you-type, click → jump hex view to offset.
- **Extracted Text:** HTML `JTextPane` re-layout of 32 KiB chunks is slow; RTL detection only on first 1 KiB; forward-only live extraction. *postmortem:* our own index (tantivy) storing chunk text; render with `egui` `LayoutJob` and highlight ranges (no HTML); global hit list sidebar ("hit 17 of 230" across all chunks, not per page); cached live extraction with random page access; font zoom via Ctrl+wheel; per-chunk lazy loading.
- **Viewer selection depends on DB MIME only** (Application tab empty when MIME missing even though detection ran); no manual "View as…" override. *postmortem:* allow user to force any viewer (View as Image/SQLite/Registry/Text/Hex) from a dropdown, and auto-detect by signature when MIME is absent.
- **Stale state & static fields** (Hex/Strings `static currentPage`) break pop-out windows. *postmortem:* each viewer instance owns its state struct keyed by viewport; selection → viewer via message passing; all loading through an async job system with cancellation tokens and per-viewer generation counters to drop stale results.
- **SQLite:** 100-row pages, sorting only within page, page count off-by-one, no `ORDER BY`, views/schemas not shown, CSV quoting broken, epoch-as-ms only. *postmortem:* virtualised table using `LIMIT/OFFSET` window fetching (or rowid keyset) for millions of rows, server-side ORDER BY on header click, SQL query box (read-only connection), schema tab (CREATE statements, indices, views, triggers), freelist/unallocated-page and WAL-frame inspection (forensic plus), proper RFC 4180 CSV (UTF-8), per-column "interpret as" (Unix s/ms/µs, Mac absolute (2001), WebKit/Chrome (1601 µs), FILETIME, hex, UTF-8/UTF-16 BLOB, BLOB→hex/image preview), and timezone respecting the global display-timezone option.
- **Plist:** binary only routed (XML plists not), NSData hidden, no NSKeyedArchiver decoding, dates via `toString()`. *postmortem:* route `application/x-bplist` + XML plist + NSKeyedArchiver; show data length + hex preview/expand; decode keyed-archiver object graph; JSON/XML export.
- **Registry:** no search, no path bar, no export, GMT-only timestamps, values truncated to 48 chars in table, lazy tree via Swing. *postmortem:* key-path breadcrumb + copy path, search (key/value name/data, regex), show class name/security/last-written in display TZ, deleted keys/values from unallocated cells, transaction-log (`.LOG1/.LOG2`) replay toggle, export key as `.reg`/JSON, decoders for common binary values (FILETIME, UserAssist ROT13, ShellBags) as a later enhancement.
- **Images:** no wheel zoom / drag pan, upscaling of tiny images on fit, decoding entire image on UI-adjacent threads, image tagging Windows+OpenCV only. *postmortem:* `image` crate (+ `libwebp`, `heic` via optional libheif, RAW via `rawloader`) decode on worker thread, downscaled texture for display with full-res tiles on zoom; wheel zoom around cursor, drag pan, fit/1:1/fill buttons, rotate/flip, EXIF orientation honouring, animated GIF/APNG/WebP playback; cross-platform region tags (draw rect overlay in egui `Painter`), export-with-overlays via `image`/`imageproc`.
- **Audio/Video:** must copy whole file to temp before playback; deleted files not playable; GStreamer dependency. *postmortem:* feed demuxer from an in-image `Read+Seek` (ffmpeg via `ffmpeg-next` or gstreamer `appsrc`), allow playback of deleted/carved media when data is recoverable, frame-step, snapshot-frame export, keyboard shortcuts (Space play/pause, ←/→ ±5 s, Shift ±30 s), scrub thumbnails.
- **HTML:** JavaFX WebView heavy; sanitiser removes all URI attributes so even internal anchors die; "Download Images" fetches from the internet (OPSEC risk). *postmortem:* safest default = render sanitised text/HTML-to-rich-text (e.g. `html2text`/own layout) plus "Source" tab with syntax highlighting; never perform network fetches — resolve images only from the same evidence (e.g. browser cache) when requested; optional embedded webview (`wry`) in a sandboxed popout with network blocked.
- **PDF:** IcePDF slow/fragile on large docs. *postmortem:* `pdfium-render` (or mupdf bindings) page rasterisation on worker threads, lazy page rendering in a virtualised scroll, text-layer search/copy, metadata/attachments/JavaScript-presence panel (forensic).
- **Translation:** limited to first 25.6 KB / provider 5000 chars, re-extracts on every combo change. *postmortem:* cache extracted text per file in the case DB; chunked translation with progress; side-by-side original/translation view.
- **General:** every viewer blocks the main window with a wait cursor for some steps (SQLite table switch, HTML load). In egui keep the UI thread free — show inline spinners per pane, keep last good render until new content is ready, and debounce rapid selection changes (e.g. 100 ms) before starting heavy loads.

---

## Text extractors (`textextractors/`)

Source: `Core/src/org/sleuthkit/autopsy/textextractors/` (`TextExtractor.java`, `TextExtractorFactory.java`, `TikaTextExtractor.java`, `HtmlTextExtractor.java`, `SqliteTextExtractor.java`, `StringsTextExtractor.java`, `TextFileExtractor.java`, `ArtifactTextExtractor.java`, `configs/ImageConfig.java`, `configs/StringsConfig.java`). Helpers: `coreutils/StringExtract.java`, `coreutils/textutils/EncodingUtils.java`, `coreutils/PlatformUtil.java`.

These extractors are shared. The keyword-search indexer, the Indexed Text and Translation content viewers, Discovery document summaries and the "Strings" fallback all use them.

### Interface contract (`TextExtractor`)
- `isSupported() -> bool`. It is cheap and usually depends only on the MIME type and size.
- `getReader() -> Reader`. It is a streaming character reader and throws `InitReaderException` on failure.
- `setExtractionSettings(Lookup ctx)`. Configuration comes in as a bag of typed config objects (`ImageConfig`, `StringsConfig`, `ProcessTerminator`). The default is a no-op.
- `getMetadata() -> Map<String,String>`. It returns an empty map by default. Tika and HTML provide metadata.
- `willUseOCR() -> bool`. The caller uses it to warn that extraction will be slow.

### Selection order (`TextExtractorFactory.getExtractor(content, ctx)`)
1. If the content is an `AbstractFile`, the first extractor whose `isSupported()` returns true wins, tried in this fixed order:
   1. `TextFileExtractor`: MIME `text/plain` only.
   2. `HtmlTextExtractor`: web MIME types, file size ≤ 50,000,000 bytes.
   3. `SqliteTextExtractor`: MIME `application/x-sqlite3`.
   4. `TikaTextExtractor`: any type Tika supports, minus the exclusions listed below. It is deliberately last.
2. If the content is a `BlackboardArtifact`, `ArtifactTextExtractor` is used (always supported).
3. If the content is a `Report`, `TikaTextExtractor` is used.
4. Otherwise `NoTextExtractorFound` is thrown. Callers then fall back to `getStringsExtractor(content, ctx)`, which works for any content.

### TextFileExtractor
- Supported when the MIME type is `text/plain`. If the DB MIME type is empty, it runs `FileTypeDetector` on the fly.
- Charset detection (`EncodingUtils.getEncoding`):
  1. ICU4J/Tika `CharsetDetector.detectAll()`. If the top pick is `IBM500` and an alternative exists, the second pick is used instead (a known false positive). The result is accepted when its confidence is ≥ 41 and the charset is supported by the JVM. The threshold is 41 because Chrome cache files were detected as UTF‑16 at 40.
  2. Otherwise, Decodetect 0.3 (statistical) runs on the first 100,000 bytes. The result is accepted when its confidence is ≥ 0.4.
  3. Otherwise the result is `UNKNOWN_CHARSET`, and the extractor falls back to UTF‑8. Detection errors also fall back to UTF‑8.
- Output is a buffered `InputStreamReader` over the raw file content.

### HtmlTextExtractor (Jericho HTML Parser 3.4)
- MIME whitelist: `application/javascript`, `application/xhtml+xml`, `application/json`, `text/css`, `text/html`, `text/javascript`. The size cap is 50 MB; larger files fall through to Tika.
- Text: `Source.fullSequentialParse()`, then `Renderer` with newline `\n`, no hyperlink URLs, no font-style decoration, no alt text, and `maxLineLength=0` (no wrapping). It catches `Throwable` because Jericho can hit a StackOverflowError on deep nesting.
- `getMetadata()` walks all start tags and builds numbered lists of:
  - `Scripts`: the tag attributes plus the inner script content.
  - `Links`: `<a>` tag content.
  - `Images`: `<img>` tag content.
  - `Comments`.
  - `Others`: any other tag that has attributes, written as `tagname:content`.

  Each key is present only when its count is > 0. The indexer shows this metadata.

### SqliteTextExtractor
- Supported for MIME `application/x-sqlite3`. Tika explicitly skips SQLite because it fails on virtual tables.
- A streaming `Reader` over `SQLiteTableReader`, which copies the DB file to a temp location and opens it via JDBC. For each table, in name order from `getTableNames()`, it emits this text format:
  ```
  \n<table_name>\n\n\t<col1> <col2> ... <colN>\n
  \t<v1> <v2> ... <vN>\n      (one line per row)
  ```
  BLOB values are emitted as empty strings and nulls as `""`. When one table fails to read, the extractor logs it and moves on.
- Edge cases:
  - If the table names cannot be read, the extractor returns EOF immediately.
  - Text is buffered across `read()` calls through an "ExcessBytes" carry-over.

### StringsTextExtractor (fallback for any content)
- Defaults: script `LATIN_2` (Latin + extended), UTF‑8 on, UTF‑16 off. It is supported when UTF‑8 or UTF‑16 is enabled.
- `StringsConfig` sets `extractUTF8`, `extractUTF16` and `languageScripts` (a list of `SCRIPT`). Any of the three may be null, which means "keep the default".
- Two engines:
  - **EnglishOnlyStream**: used when the scripts are exactly `[LATIN_1]`.
    - Reads 64 KiB chunks.
    - Printable ASCII runs of at least 4 characters become strings, separated by `\n`.
    - A single NUL between printable characters is tolerated, so UTF‑16LE ASCII is captured.
    - Strings that span chunk boundaries are stitched together.
  - **InternationalStream**: reads 1 MiB chunks and runs `StringExtract.extract()`.
    - Scans per byte offset with UTF‑8 and/or UTF‑16 (LE/BE) decoders.
    - Keeps runs whose characters belong to the enabled Unicode scripts.
    - `MIN_CHARS_STRING = 4`.
    - Output is UTF‑8.
- Available `SCRIPT` values: NONE, COMMON, LATIN_1, GREEK, CYRILLIC, ARMENIAN, HEBREW, ARABIC, SYRIAC, THAANA, DEVANAGARI, BENGALI, GURMUKHI, GUJARATI, ORIYA, TAMIL, TELUGU, KANNADA, MALAYALAM, SINHALA, THAI, LAO, TIBETAN, MYANMAR, GEORGIAN, HANGUL, ETHIOPIC, CHEROKEE, CANADIAN_ABORIGINAL, OGHAM, RUNIC, KHMER, MONGOLIAN, HIRAGANA, KATAKANA, BOPOMOFO, HAN, YI, OLD_ITALIC, GOTHIC, DESERET, INHERITED, TAGALOG, HANUNOO, BUHID, TAGBANWA, LIMBU, TAI_LE, LINEAR_B, UGARITIC, SHAVIAN, OSMANYA, CYPRIOT, BRAILLE, BUGINESE, COPTIC, NEW_TAI_LUE, GLAGOLITIC, TIFINAGH, SYLOTI_NAGRI, OLD_PERSIAN, KHAROSHTHI, BALINESE, CUNEIFORM, PHOENICIAN, PHAGS_PA, NKO, CONTROL, LATIN_2. The table is built from a bundled Unicode script table of 65,536 entries.
- Read errors: returns what has been collected so far, then EOF on the next call.

### TikaTextExtractor (Apache Tika 3.0.0; standard + scientific + sqlite3 + advancedmedia parser packages)
- `isSupported()` is true when the DB MIME type is non-null, is in Tika's supported-types list (`new Tika().getParser().getSupportedTypes()`), and is not excluded. Exclusions:
  - Binary: `application/octet-stream`, `application/x-msdownload` (the strings fallback is used instead).
  - Archives: 7z, ace, alz, arj, cab, cfs, dgc, apple-diskimage, gca, dar, lzx, lzh, rar, stuffit(x), gtar, archive, x-executable, gzip, zip, zoo, cpio, shar, tar, bzip, bzip2, lzip, lzma, lzop, x-z, compress. The rationale is that unpacking is the extractor ingest module's job.
  - All `video/*` except `video/x-flv`.
  - `application/x-sqlite3`.
- Parse setup:
  - `AutoDetectParser`.
  - The `ParseContext` re-registers the parser itself, so **embedded documents are parsed recursively** and their text is concatenated.
  - `OfficeParserConfig` sets `useSAXDocxExtractor=true` and `useSAXPptxExtractor=true` to keep memory low (XLSX is already SAX).
- Timeout: `ParsingReader` construction runs in a single-thread executor (`tika-reader-%d`) as a `Future`, with a size-based timeout:

  | File size | Timeout |
  |---|---|
  | < 1 MB | 60 s |
  | < 10 MB | 1,200 s (20 min) |
  | < 100 MB | 3,600 s (1 h) |
  | ≥ 100 MB | 3 h |

  When the timeout expires, the extractor throws "Tika parse timeout for content: id, name". The future is always cancelled in `finally`.
- If the reader is empty (the first `read()` returns -1), the extractor throws "Tika returned empty reader". A `PushbackReader` is used for this check.
- Metadata: captured from the parse into `metadataMap`. If `getMetadata()` is called before any parse, a separate full `parser.parse` runs with a no-op content handler.
- Logging: errors go to a separate "Tika" java.util.logging log (`tika.log`) as well as the Autopsy log.
- **OCR (Tesseract)**:
  - Enabled only when **all** of the following hold:
    - `ImageConfig.OCREnabled` is true (default `false`).
    - The bundled `Tesseract-OCR/tesseract.exe` was located.
    - The platform is 64-bit Windows.
    - `isSupported()` is true.
  - Languages: by default, every `*.traineddata` in `<userdir>/ocr_language_packs`, joined with `+`, for example `eng+fra`. `ImageConfig.setOCRLanguages` overrides this.
  - OCR timeout: `ImageConfig` sets 30 min (`TimedProcessTerminator`). It is combined with any `ProcessTerminator` from the context (for example a cancellation check) into a `HybridTerminator`.
  - `willUseOCR()` is true for `image/*` and for the `OCR_DOCUMENTS` types: pdf, msword, docx, ms-powerpoint, pptx, ms-excel, xlsx.
  - Image path, which bypasses Tika so it can be cancelled:
    - Writes the file to `<case>/Temp/<id><name>`.
    - Runs `tesseract "<in>" "<case temp>/<id>tess_output" --tessdata-dir <packs> -l <langs>`, polling every 1 s against the terminator.
    - Reads `…tess_output.txt` through a `CleanUpStream`, which deletes the file on close. The input temp file is always deleted.
  - Document path: a `TesseractOCRConfig` (`tessdataPath`, `tesseractPath`, language) goes into the context. `PDFParserConfig.ocrStrategy = AUTO` makes Tika OCR a page when that page has too few extracted characters or too many unmapped ones.

### ArtifactTextExtractor
- For every attribute, emits `"<attr type display name> : <value>"` followed by a line separator.
- `DATETIME` attributes are formatted with `TimeZoneUtils.getFormattedTime` (user timezone preference). All other attributes use `getDisplayString()`.
- Always supported. It is used for indexing artifacts in Solr.

### ImageConfig / StringsConfig defaults
| Config | Field | Default |
|---|---|---|
| ImageConfig | OCREnabled | false |
| ImageConfig | ocrLanguages | null (meaning all installed packs) |
| ImageConfig | OCR timeout | 30 min |
| StringsConfig | extractUTF8 | null → extractor default true |
| StringsConfig | extractUTF16 | null → extractor default false |
| StringsConfig | languageScripts | null → extractor default [LATIN_2] |

## Machine translation (`texttranslation/`)

Source:
- `texttranslation/TextTranslator.java`, `TextTranslationService.java`, `TranslationOptionsPanel(.java|Controller.java)`
- `translators/{GoogleTranslator, BingTranslator, *Settings, *SettingsPanel, LanguageWrapper}.java`
- `ui/{TranslatedTextViewer, TranslationContentPanel, TranslateTextTask}.java`
- `utils/FileNameTranslationUtil.java`
- `datamodel/utils/FileNameTransTask.java`, `datamodel/AbstractAbstractFileNode.java`

### Service provider model
- Interface `TextTranslator`: `translate(String) -> String`, `getName()`, `getSettingsPanel() -> JPanel`, `saveSettings()`, `getMaxTextChars()`.
- Implementations are discovered once at class load through `Lookup.lookupAll(TextTranslator.class)` (`@ServiceProvider`).
- `TextTranslationService` is a singleton. The selected translator is the one whose `getName()` equals the preference `TextTranslatorName` (ExternalServices preferences node, default `null`, meaning **no provider, so translation is disabled**).
- `hasProvider()` gates the Translation viewer and the file-name translation column. `translate()` throws `NoServiceProviderException` when no translator is selected. All methods are synchronized, so translations are serialized globally.

### Options panel: Tools → Options → "Machine Translation"
- Top-level options category at position 7, icon `translate32.png`.
- Controls:
  - Description: "Configure a 3rd party text translation service to enable text and file name translation."
  - "Text translator:" combo box. Entries: "Translation disabled" plus each provider name ("Google Translate", "Bing Translator"). Choosing one swaps in that provider's settings panel.
  - Checkbox "Enable Optical Character Recognition (OCR) in the translation content viewer". Preference `OcrTranslationEnabled`, **default true**.
- `store()` saves `TextTranslatorName` and calls the selected provider's `saveSettings()`, then `updateSelectedTranslator()`. If a provider fails, the panel shows "Unable to get selected text translator, translation is disabled."

### Google Translate provider
- Settings (ModuleSettings config `GoogleTranslate.properties`):
  - `CredentialPath`: default `""`. A path to a service-account JSON key.
  - `TargetLanguageCode`: default `TranslateOptions.getDefaultInstance().getTargetLanguage()` (the locale language, typically `en`).
- Settings panel:
  - "Credentials Path:" field with a Browse button (`.json` filter).
  - "Target Language:" combo box. Its options come from the Google API using the selected credentials, so a successful load also validates the JSON. Language names are localized to the current target.
  - Test row: "Untranslated:" field (default text `traducción exitoso`), a Test button, and "Result:".
  - Instructions with a link to the GCP service-account key docs.
- Translation behaviour:
  - Pings `www.google.com` (`InetAddress.isReachable(1500 ms)`) before every call and fails with "Cannot connect to Google" if unreachable.
  - Trims the input, replaces `\r\n|\n` with `<br />` (the API defaults to HTML), and truncates to **5,000 chars** (`MAX_PAYLOAD_SIZE`).
  - Converts `<br />` back to `\n` and HTML-unescapes the result.
  - Without credentials it throws "Google Translator has not been configured".
  - The source language is auto-detected.

### Bing (Microsoft Translator) provider
- Settings (ModuleSettings `BingTranslate.properties`):
  - `Credentials`: subscription key, default `""`.
  - `TargetLanguageCode`: default `en`.
- Request:
  - `POST https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&to=<lang>`
  - Header `Ocp-Apim-Subscription-Key`.
  - Body `[{"Text": "..."}]`.
  - The response is parsed from `[0].translations[0].text`.
  - Input is trimmed and truncated to 5,000 chars. Newlines are kept as-is.
- Settings panel:
  - "Authentication Key:" field.
  - "Target Language:" combo box, filled from `GET .../languages?api-version=3.0&scope=translation`. If the fetch fails, the combo is disabled and a warning is shown.
  - Same Test row as Google.

### Translation content viewer ("Translation" tab, a `TextViewer` inside the Text content viewer)
- Supported when a provider is selected and the node has an `AbstractFile`.
- UI:
  - "Show:" combo with "Translated Text" (default) and "Original Text".
  - "OCR:" combo, enabled only if OCR is on in options. It lists "Autodetect language" first, then the installed `.traineddata` packs mapped to display names (~120 Tesseract codes such as `afr` → Afrikaans and `chi_sim` → Chinese - Simplified; unknown codes are hidden). The last selection is remembered.
  - Warning label "Up to the first 5KB of text will be translated" (`maxChars/1000`).
  - Read-only auto-wrapping text pane.
- Flow: a single-thread background executor runs one task at a time. Changing a combo cancels the running task.
  1. Shows "Extracting text, please wait...".
  2. If the file is an image and OCR is disabled, shows "OCR is not enabled. To change, go to Tools->Options->Machine Translation".
  3. Extracts with `TextExtractorFactory`. With OCR on, it passes `ImageConfig{OCR=true, langs=[selected]}` and a terminator bound to task cancellation. It falls back to the strings extractor.
  4. Reads at most **25,600 bytes**, measured as UTF‑8.
  5. Shows "Translating text, please wait..." in italics, then calls the service.
  6. Detects text direction (RTL/LTR) from the first 1,024 characters of the result and sets the component orientation.
- Error strings:
  - "File has no text."
  - "The machine translation software was not found."
  - "An error occurred while translating the text (msg)."
  - "The machine translation software did not return any text."

  Errors are shown in italics.

### File-name translation in result tables
- Preference `DisplayTranslatedNames` (View preferences): default **true**, but it has an effect only when a provider exists.
- When both conditions are true, file nodes get an extra column "Original Name". A background `TranslationTask`/`FileNameTransTask` (node property-sheet pool) calls `FileNameTranslationUtil.translate(name)` and fires a `TRANSLATION_AVAILABLE` event. The node then shows the translated name in Name and the original in "Original Name".
- `FileNameTranslationUtil`:
  - Returns `""`, meaning no translation, if the name is pure ASCII (`^\p{ASCII}+$`).
  - Otherwise it translates only the base name and re-appends `.ext`.
  - Failures yield `""`.

## Text summarizer (`textsummarizer/`)
- An SPI only: `TextSummarizer { String getName(); TextSummary summarize(AbstractFile, int summarySize) }`. `TextSummary` holds `summaryText`, `sampleImage` and `numberOfImages`.
- No implementation ships in Core. Discovery (`discovery/search/FileSearch.summarize`) looks one up and calls `summarize(file, 40)`. Otherwise it uses `SummaryHelpers.getDefaultSummary`, which:
  - takes the first 256 chars of extracted text (`PREVIEW_SIZE`), translated if a provider exists;
  - caches the result in `<case>/Cache/summaries/<md5>-default-256-translated.txt`;
  - uses the largest image child (embedded image) thumbnail as the sample image, plus a count of image children.

## Image Writer (VHD copy of a local disk during ingest) (`imagewriter/`)

Source: `imagewriter/{ImageWriter, ImageWriterService, ImageWriterSettings}.java`, UI in `casemodule/LocalDiskPanel.java`.

- **UI** (Add Data Source → Local Disk panel):
  - Checkbox "Make a VHD image of the drive while it is being analyzed".
  - Path field. Default: `<case>/ModuleOutput/Image Writer/<sanitized disk name> <epochMillis>.vhd`.
    - Sanitizing keeps only `[0-9A-Za-z _()]`.
    - If the name is empty or starts with a space, it is prefixed with `localDisk`.
    - The folder is created on demand.
  - Checkbox "Update case to use VHD file upon completion" (`updateDatabasePath`).
- **Validation**, shown in red:
  - "Error - enter path for VHD"
  - "Error - directory does not exist" (the parent is missing or null)
  - "Error - VHD path is a directory"
  - "Error - VHD path already exists"
- **Mechanics**:
  - `ImageWriterSettings(path, updateDatabasePath)` is passed to the add-image task. TSK's image writer writes every sector read during ingest into a sparse VHD.
  - `ImageWriterService.createImageWriter(imageId, settings)` registers an `ImageWriter` that listens for `DATA_SOURCE_ANALYSIS_COMPLETED` for that data source. Only the first event counts (`isStarted`).
  - It then runs `SleuthkitJNI.finishImageWriter(handle)` on its own thread, which reads all sectors not yet copied.
  - A progress bar reads "Finishing acquisition of {name} (unplug device to cancel)" and polls `getFinishImageProgress` every 250 ms (0–100).
  - Even when finishing fails or is cancelled, `caseDb.updateImagePath(vhdPath, dsId)` runs if the setting is on, so a partial image is still usable.
- **Case close** (`closeCaseResources`):
  - Writers that have not started are cancelled.
  - If any are in progress, a dialog asks "Wait for VHD(s) in progress to complete?" (title "Local disk image copy", default **No**). No calls `cancelFinishImage`. In both cases close then waits for the jobs to return.
  - Progress text: "Waiting for VHD(s) to complete".
- Only `doUI` (GUI mode) shows progress. There are no global options.

## Logical Imager (`logicalimager/`)

Source:
- `logicalimager/configuration/*`: the config wizard (`CreateLogicalImagerAction`, `ConfigWizardPanel1-3`, `ConfigVisualPanel1-3`, `NewRulePanel`, `EditRulePanel`, `EditFullPathsRulePanel`, `EditNonFullPathsRulePanel`, `LogicalImagerConfig`, `LogicalImagerRuleSet`, `LogicalImagerRule`, `LogicalImagerConfigDeserializer`, `EncryptionProgramsRule`).
- `logicalimager/dsp/*`: importing the results (`LogicalImagerDSProcessor`, `LogicalImagerPanel`, `AddLogicalImageTask`, `AddMultipleImagesTask`, `DriveListUtils`).

### Create Logical Imager wizard (Tools → "Create Logical Imager", position 2000, separator before)
The menu item is enabled only when the bundled `tsk_logical_imager/tsk_logical_imager.exe` exists, which in practice means Windows builds. The wizard is a NetBeans 3-step `WizardDescriptor` titled "Create Logical Imager", with numbered steps.

1. **Select location** (`ConfigVisualPanel1`)
   - Description: "Select a location for the Logical Imager. This location will contain the imaging program and a configuration file. If that location already contains a configuration file, it will be loaded to edit. Imaging results will be saved to this location, so ensure it has enough free space. Drives with FAT format are not supported."
   - Radio "Configure selected external drive:" shows a drive list with a Refresh button. Each item is `"{root} ({description}) ({size}) - File system: {fs}"`.
     - Network drives are excluded.
     - The first removable drive (`volume:isRemovable`) is auto-selected, otherwise the first drive.
     - Shows "No drive found" when the list is empty.
     - The config path becomes `<drive root>logical-imager-config.json`.
   - Radio "Configure in a folder:" uses a text field and Browse. The JSON file chooser defaults to `logical-imager-config.json`; `.json` is appended if missing.
   - An existing file is loaded and validated. Errors: "Invalid config JSON: …" or "Configuration file {path} is empty".
   - Next is valid only when the target file system is **not FAT/FAT32** and a drive or file is chosen.
2. **Configure imager** (`ConfigVisualPanel2`)
   - Shows "Configuration rule file:" (the path).
   - Rules table (Rule Name, Description) with New Rule, Edit Rule and Delete Rule (with confirmation) buttons.
   - The read-only detail area for the selected rule shows: Rule name, Description, Extensions, File names (table), Folder names (table), Full paths (table), "File size in bytes" Minimum/Maximum, Modified Within N day(s), "Extract file" checkbox, "Alert in imager console" checkbox.
   - Global checkboxes:
     - "Alert if encryption programs are found": adds or removes a built-in rule named "Encryption Programs" ("Find encryption programs"), alert=true, matching file names: truecrypt.exe, AxCrypt.exe, VeraCrypt*.exe (4 files), the GnuPG exes (gpg-agent, gpg-connect-agent, gpg-preset-passphrase, gpg-wks-client, gpg, gpgconf, gpgme-w32spawn, gpgsm, gpgtar, gpgv), and PGP/Symantec exes (PGP Viewer, PGPcbt64, PGPdesk, PGPfsd, PGPmnApp, pgpnetshare, pgpp, PGPpdCreate, pgppe, pgpstart, PGPtray, PGPwde, PGP Portable).
     - "Create VHD" (`create-VHD`, default false). Its child "Continue imaging after searches are performed" (`finalize-image-writer`, default false) is indented and applies when a VHD is being made.
     - "Prompt before exiting imager" (`prompt-before-exit`, default true).
   - Duplicate rule names are rejected: 'A rule with name "{0}" already exists. Please enter a different rule name'.
3. **Save imager** (`ConfigVisualPanel3`)
   - Text: "Press Save to write the imaging tool and configuration file to the destination.\nDestination: {path}".
   - The Save button writes pretty-printed JSON (Gson, `@Expose` only, HTML escaping off, platform line separators, UTF‑8) and copies `tsk_logical_imager.exe` next to it.
   - Two status lines: "Logical Imager config file save status:" and "Logical Imager executable save status:". Each shows "File has not been saved", "Saving file, please wait", "Saved", or "Unable to save file" (in red).
   - Finish is enabled only after a successful save.

### Rule editor (New Rule → choose type)
- `NewRulePanel` combo "Choose the type of rule":
  - **Attribute**: "Search for files based on one or more attributes or metadata fields."
  - **Full Path**: "Search for files based on full exact match path."
- **Attribute rule** (`EditNonFullPathsRulePanel`). Every criterion has an enable checkbox.
  - Rule name (required) and Description (optional).
  - Extensions: comma-separated, with prompt "Example: gif,jpg,png". Case-insensitive; an empty entry causes an error. **Mutually exclusive** with File names.
  - File names: one per line, prompt "filename.txt / readme.txt". Case-insensitive; a blank line causes an error.
  - Folder names: one per line. Notes "Folder name matches are case insensitive and occur anywhere in a path." and "Starting a folder name with the token [USER_FOLDER] will allow matches of all user folders in the file system." Example: `[USER_FOLDER]/My Documents/Downloads`.
  - Minimum size / Maximum size: a number plus a units combo (Bytes / Kilobytes / Megabytes / Gigabytes), converted to bytes. Values must be non-negative numbers, and max must be ≥ min ("Maximum file size: {0} bytes must be bigger than minimum file size: {1} bytes"). 0 means unset.
  - Modified within N day(s): a non-negative integer stored as `min-days`.
  - "If file is found:" with "Extract file" (`shouldSave`, default true in a new rule) and "Alert in imager console" (`shouldAlert`, default false in a new rule).
- **Full path rule** (`EditFullPathsRulePanel`):
  - Rule name, Description (Optional), Full paths (one per line, e.g. `/Program Files/Common Files/system/wab32.dll`), and the same two checkboxes.
  - A full-paths rule cannot be combined with extensions, file-names or folder-names; the deserializer rejects that combination.

### Config JSON format (version "1.0")
```json
{
  "version": "1.0",
  "finalize-image-writer": false,
  "prompt-before-exit": true,
  "create-VHD": false,
  "rule-sets": [
    { "set-name": "...",
      "rules": [
        { "shouldAlert": false, "shouldSave": true, "name": "...", "description": "...",
          "extensions": ["jpg"], "file-names": [], "folder-names": ["[USER_FOLDER]/Downloads"],
          "full-paths": [],
          "size-range": {"min": 1024, "max": 1048576},
          "date-range": {"min-days": 30} } ] } ]
}
```
- The deserializer also accepts `date-range.min` and `date-range.max` (integer dates). Any unknown key raises "Unsupported key: {key}". A missing `rule-sets` raises "Missing rule-set".
- Rule defaults differ by path:
  - When a rule is **parsed** from a file, missing `shouldSave` defaults to **false** and missing `shouldAlert` to **true**.
  - A **new** rule in the UI has `shouldSave=true` and `shouldAlert=false`.

  Keep both behaviours for file compatibility.

### Import results: data source type "Autopsy Logical Imager Results" (`LogicalImagerDSProcessor`)
- Panel (`LogicalImagerPanel`):
  - Radio **"Import From External Drive"**: "Select Drive" list plus Refresh. Selecting a drive lists its top-level folders that match `Logical_Imager_<HOST>_<yyyymmdd>_<HH>_<MM>_<SS>` and contain a `.vhd` or a subdirectory. The table columns are Hostname and Extraction date (`yyyy/mm/dd HH:MM:SS`), and the first row is auto-selected. Messages: "Drive has no images" and "No image selected".
  - Radio **"Manually Choose Folder"**: Browse, then "Selected Folder:". The name must match the pattern: "Directory {0} does not match format Logical_Imager_HOSTNAME_yyyymmdd_HH_MM_SS".
- `run()`:
  1. If the source folder has vanished, fails with "{path} not found.\nUSB drive has been ejected."
  2. Creates `<case>/ModuleOutput/LogicalImager/` and sets dest = `<that>/<folder name>`.
  3. If dest already exists, asks "The logical imager folder {0} already exists, do you want to add it again using a new folder name?" Yes appends `_<timestamp>`; No fails.
  4. `deviceId` is a random UUID. The timezone is the local default.
- `AddLogicalImageTask`, in order:
  1. Copies the entire source directory to dest ("Copying image from {src} to {dest}").
  2. Requires `SearchResults.txt`; without it the task fails. Adds it as a case **Report** named `"SearchResults.txt <srcFolderName>"` (module "LogicalImager"), plus every `*_users.txt` in the same way.
  3. Counts lines minus the header to get the total for progress.
  4. **If the dest contains `*.vhd` files**:
     - Adds them all as images via `AddMultipleImagesTask`. If a VHD has no file system, it is added as an image without FS and its content as an unallocated-space file.
     - Maps each result line to a file with `data_source_obj_id=<vhd ds> AND meta_addr=<field2> AND name=<field7>`.
  5. **Otherwise** it needs a `root/` dir. It creates one LocalFilesDataSource (name = source folder name) in a single transaction and adds each extracted file:
     - `addLocalFile(src/extractedPath, filename, parent "root/<vhdName>/<parentPath>", ctime, crtime, atime, mtime)`.
     - The transaction rolls back on cancel or error.
  6. For every matched file, creates a `TSK_INTERESTING_ITEM` analysis result:
     - Score LIKELY_NOTABLE, conclusion null.
     - Configuration "Logical Imager results".
     - `TSK_SET_NAME` = "Logical Imager results", `TSK_CATEGORY` = rule name.
     - Artifacts are posted in batches of 1,000. Progress text is updated every 100 lines.
- `SearchResults.txt` format: UTF‑8, tab-separated, one header line, **exactly 14 fields per row**, otherwise "File does not contain enough fields at line {n}, got {x}, expecting 14". The fields:
  `0 vhdFilename, 1 fileSystemOffset, 2 fileMetaAddress, 3 extractStatus, 4 ruleSetName, 5 ruleName, 6 description, 7 filename, 8 parentPath, 9 extractedFilePath, 10 crtime, 11 mtime, 12 atime, 13 ctime` (epoch seconds).
- Cancellation:
  - Before artifacts are added, the dest dir is deleted. This does not happen in VHD mode (known bug 5453).
  - Once interesting-file creation has begun, the dest dir is kept.

## Live triage drive (`livetriage/`)
Source: `livetriage/CreateLiveTriageDriveAction.java`, `SelectDriveDialog.java`.
- Tools → **"Make Live Triage Drive"** (position 1850, separator before). It is always enabled, but works only on an installed Windows build: it needs `<install>/bin/<appName>64.exe`, otherwise "Executable could not be found".
- `SelectDriveDialog` "Create Live Triage Drive":
  - Description: "This feature copies the application and a batch file to a removable drive, allowing systems to be analyzed without installing the software or imaging the drives. To analyze a system, insert the drive and run "RunFromUSB.bat" as administrator, then select the "Local Disk" option on the Add Data Source panel."
  - Table "Disk Name | Disk Size", populated on a background thread from `PlatformUtil.getPartitions()` (may take time). It shows "Disks were not detected. On some systems it requires admin privileges" when empty.
  - Buttons: Refresh, Ok, Cancel.
- Action:
  - Strips the `\\.\` prefix from the path.
  - A SwingWorker with a modal progress dialog ("Please wait" / "Copying live triage files to {drive}") writes `RunFromUSB.bat` to the drive root, then copies the whole install directory to `<drive>/<appName>/`.
  - Final message: "Finished creating live triage disk" or "Error copying live triage files".
- `RunFromUSB.bat`:
  - `cd` to the script dir.
  - Creates `configData\userdir` and recreates `configData\cachedir` and `configData\temp` from scratch.
  - Creates `cases`.
  - Runs `%appName%\bin\%appName%64.exe --userdir ..\configData\userdir --cachedir ..\configData\cachedir -J-Djava.io.tmpdir=..\configData\temp --liveAutopsy`.
  - Ends with `pause`.
- `--liveAutopsy` puts the app in live-triage mode, in which local disks of the running host are offered.

## URL / domain categorization (`url/analytics/`)

Source:
- `url/analytics/{DomainCategorizer, DomainCategory, DomainCategorizerException}.java`
- `url/analytics/domaincategorization/{CustomWebCategorizer, WebCategoriesDataModel, WebCategoriesOptionsPanel, WebCategoriesOptionsController, AddEditCategoryDialog}.java`
- Consumer: `RecentActivity/.../DomainCategoryRunner.java`, `DefaultDomainCategorizer.java`, `DefaultPriorityDomainCategorizer.java`, `default_domain_categories.csv`.

### SPI (marked `@Beta`)
- `DomainCategorizer extends AutoCloseable`:
  - `initialize()` (default no-op)
  - `DomainCategory getCategory(String domain, String host)`, which returns null when there is no match
  - `close()`
- `DomainCategory(hostSuffix, category)`.
- Provider order in the Recent Activity ingest step:
  1. the custom (user) categorizer
  2. `DefaultPriorityDomainCategorizer`: hardcoded search-engine domains (all `google.*` ccTLDs, `*.yahoo.com` variants, other search engines) → "Search Engine"
  3. any other third-party providers, sorted by class name
  4. `DefaultDomainCategorizer`, which reads the bundled CSV `suffix,category` (~101k lines: ~100.8k "Disposable Email", 153 "Web Email", 10 "Messaging"; `#` comments)

  The first non-null result wins. A `TSK_WEB_CATEGORIZATION` artifact is created per host.

### Custom categories store
- SQLite file at `<userConfigDir>/DomainCategorization/custom_list.db`. It is global, not per case. `PRAGMA journal_mode=OFF`.
- Table: `domain_suffix(suffix VARCHAR(255) PRIMARY KEY, category VARCHAR(300)) WITHOUT ROWID`.
- Normalization:
  - Suffix: split on `.`; strip every character except `[0-9a-zA-Z-]` in each label; trim leading and trailing hyphens; drop empty labels; join with `.`; cap at 255; lowercase.
  - Category: trim, cap at 300.
- Lookup:
  - The host (or the domain if the host is blank) is lowercased.
  - All dot-suffixes are generated, e.g. `a.b.c.com` → `a.b.c.com`, `b.c.com`, `c.com`, `com`.
  - Query: `WHERE suffix IN (...) ORDER BY LENGTH(suffix) DESC LIMIT 1`. The longest match wins.
- Writes:
  - `INSERT OR REPLACE`.
  - Delete uses `LOWER(suffix)=LOWER(?)`.
  - Changes save immediately; the panel has no Apply step.

### Options panel: Tools → Options → "Custom Web Categories" (top-level, position 21)
- Description: "This feature allows the Recent Activity ingest module to perform custom categorization of web sites based on domain suffixes."
- "Categories:" table with columns **Domain Suffix** (width 300) and **Category**, with tooltips. Rows are sorted by suffix.
- Buttons: **New Entry**, **Edit Entry**, **Delete Entry**, **Import Set**, **Export Set**.
- While ingest is running, every button is disabled and a warning reads "Ingest is currently running. No editing can take place at this time." The panel listens to ingest job events.
- Add/Edit dialog ("Add Entry" / "Edit Entry"): "Domain Suffix:" and "Category:" fields with live validation. Save is disabled until the input is valid. Messages:
  - "Please provide a domain suffix that is no more than 255 characters that includes at least one period."
  - "Please provide a category that is no more than 255 characters." (the code shows 255 although storage caps at 300)
  - "Please provide a unique domain suffix."
  - "Please provide a new category for this domain suffix." (on edit, when the category is unchanged)
- **Import/export format is JSON, not CSV**:
  ```json
  [ { "category": "Gambling", "domains": ["bet365.com", "pokerstars.net"] }, ... ]
  ```
  - Unknown properties are ignored. Entries with a null category or null domains are skipped. Each domain is normalized, and blank entries are skipped.
  - Import conflicts: the same suffix with a different category (compared case-insensitively) raises "Domain suffix {0} already exists. What would you like to do?" with **Overwrite / Skip / Cancel**. Cancel aborts the rest of the import, but the inserts already done remain.
  - Export defaults to the file name "Custom Categories Export.json". It refuses to overwrite an existing file ("A file already exists at the selected path. The categories will not be exported."). Output is a pretty-printed array grouped by category.

## Performance/usability problems & improvement ideas for postmortem (supporting subsystems)

- **Tika replacement (largest gap).** Tika 3 with recursive embedded parsing covers hundreds of formats. A pure-Rust port needs one crate or hand-written parser per family. The priority list below is based on what forensic cases actually contain:
  - **PDF**: `lopdf` / `pdf-extract`. Use `pdfium-render` only if a native dependency is acceptable; it is not pure Rust. Needs per-page "too few chars → OCR" logic, which replaces Tika's `OCR_STRATEGY.AUTO`.
  - **OOXML** (docx/xlsx/pptx): `zip` + `quick-xml` streaming SAX readers (cheap to hand-roll); `calamine` for xlsx/xls/ods.
  - **Legacy OLE2** (doc/xls/ppt/msg): the `cfb` crate for the container. The text streams need custom parsers: Word piece table, PPT records, `calamine` for BIFF8. Outlook `.msg` comes through the cfb property streams.
  - **ODF** (odt/ods/odp): zip + quick-xml.
  - **RTF**: a small hand-written tokenizer.
  - **EPUB/CHM**: zip + HTML, and CHM via LZX.
  - **Email**: `mail-parser` (EML/MIME, attachments recursive). PST/OST is handled elsewhere.
  - **HTML/XML**: `html5ever`/`scraper` or `tl`, replacing Jericho. Keep the metadata buckets (Scripts, Links, Images, Comments, Others).
  - **Media metadata**: `kamadak-exif`, `lofty` (audio tags), `mp4`/`matroska` for container metadata.
  - **Plain-text charset detection**: `chardetng` + `encoding_rs` replace ICU CharsetDetector + Decodetect. Keep the "reject IBM500/EBCDIC" style heuristics and the UTF‑8 fallback.
  - **OCR**: Tesseract is a C++ library, so there is no pure-Rust equivalent. Options are an optional external `tesseract` binary (as Autopsy does, but cross-platform rather than Windows-only) or an `ocrs` (pure Rust, ONNX via `rten`) backend.
- **Run every extractor in a sandboxed worker** (subprocess or `catch_unwind` + thread). Tika hangs and Jericho stack overflows are the reason for the timeout machinery. Use cooperative cancellation tokens instead of 1 s polling. Keep the size-tiered timeouts as configurable defaults.
- **Stream, don't buffer.** The SQLite extractor copies the whole DB to a temp file, and Tika materializes readers. In Rust:
  - Open SQLite read-only from an in-image VFS (`rusqlite` with a custom VFS, or a pure-Rust SQLite page reader for corrupt/WAL cases).
  - Emit rows lazily.
  - Include BLOBs as hex or strings optionally rather than dropping them.
- **Strings extraction**: a single pass with SIMD-friendly ASCII/UTF‑16LE/UTF‑8 scanning (`memchr`, `bstr`), with min-length 4 configurable. Multithread over chunks with overlap handling. Use `unicode-script` for script filtering instead of the 64k-entry custom table.
- **Text cache**: Autopsy re-extracts the same file for indexing, the Indexed Text view, Translation and Discovery summaries. Store extracted text (zstd-compressed, keyed by file id + extractor version) in the case DB or a sidecar so viewers open instantly.
- **Translation**:
  - Make the provider trait async (reqwest) with a per-provider rate limiter and a translation cache keyed by (hash(text), target lang). Autopsy re-translates on every selection and serializes all calls behind a global lock.
  - Drop the blocking `isReachable(google.com)` ping before every call.
  - Chunk long texts (sentence-aware) so translation is not limited to 5,000 chars / 25.6 KB. Show a progress bar and allow "translate more".
  - Add an offline/local provider option (e.g. a local LLM/NLLB HTTP endpoint, or LibreTranslate) for air-gapped labs.
  - File-name translation: batch many names per request instead of one HTTP call per table row, and persist results in the DB (a `translated_name` column) so sorting and filtering on translated names work.
- **Image Writer**:
  - Implement VHD (fixed/dynamic) writing in Rust as a tee in the image reader layer.
  - Keep the "finish remaining sectors after ingest" pass with a progress bar and cancel, and the "update case path on completion" flag.
  - Do not block case close with a modal; offer a "continue in background" option.
- **Logical Imager**:
  - The imager itself (`tsk_logical_imager.exe`) is a separate native Windows tool. Postmortem must at least support importing its output, which is the stable part: the `SearchResults.txt` 14-field TSV, `*_users.txt`, VHDs, and the `root/` extraction tree. Preserve the config JSON schema exactly, including the inconsistent `shouldSave`/`shouldAlert` defaults, for round-trip editing.
  - Replace the per-line `findAllFilesWhere` query (an N+1 SQL pattern with string-escaped SQL) with one bulk join on (data_source, meta_addr, name).
  - Copying the full results folder before import duplicates data on disk. Offer "reference in place" vs "copy into case".
  - The wizard can be a single egui window with a rule list and an editor side panel instead of nested modal dialogs.
- **Live triage**: copying an installed Java + NetBeans tree is slow and large. A single static Rust binary (plus a `postmortem.toml` pointing userdir and cache at the USB stick) makes this a seconds-long copy. Support Linux/macOS targets as well, and replace the `.bat` with a small launcher script per OS.
- **Domain categorization**:
  - Load the custom list plus the default CSV into an in-memory reversed-label trie or `HashMap` of suffixes at ingest start, instead of one SQL `IN` query per host.
  - Ship the default list compressed and updatable (it is about 100k disposable-mail domains).
  - Keep the JSON import/export format but add a CSV `suffix,category` import, since users expect it.
  - Make conflict resolution "apply to all" rather than one modal per conflicting row.
  - Allow editing during ingest by snapshotting the list at job start.

---

## Performance/usability problems in Autopsy & improvement ideas for postmortem (egui), cross-cutting

Subsystem-specific bullets are at the end of each section above. This section collects architecture-level issues that cut across the whole UI.

### 9.1 Problems observed in Autopsy

1. **Node/ChildFactory model costs memory and latency.** Every table row is a NetBeans `Node`, wrapped in `TableFilterNode` and then an outline-view row with `Sheet`/`PropertySet` objects. Each row eagerly builds a property sheet: name, timestamps, sizes, MIME type, hashes and more, and for artifacts all attributes. At about 10 000 rows (the default page size) this is tens of MB of objects and seconds of EDT work. Paging exists only because unbounded tables would freeze the UI.
2. **Hard page size (10 000) instead of virtualisation.** The user must click through pages. Sorting is per page in the Swing table (it does not sort the whole result set). Filtering and search within a result is impossible across pages.
3. **S/C/O columns (Score, Comment, Occurrences) need one or more Central Repository queries per row.** They are computed by background workers that update cells later, which makes rows flicker and makes sorting by these columns inconsistent. There is a preference to hide them entirely "to reduce loading times".
4. **Every content-viewer tab is evaluated on every selection change.** `DataContentPanel` calls `isSupported()` / `isPreferred()` on each viewer for every selected node, often on the EDT, and several of these hit the DB (artifact lookups, CR lookups, file reads for MIME detection). Arrow-key scrolling through a table therefore stutters.
5. **Viewers load content page by page, with heavyweight per-viewer setup.** Hex and Strings show one fixed 16 KiB page (or one strings page) at a time. They load through a SwingWorker, but the user has to click through pages. Indexed text calls Solr. The media viewer initialises GStreamer. HTML/PDF/JavaFX panels are heavyweight to create. Several viewers have no cancellation when the selection changes quickly.
6. **Mixed toolkits (Swing + JavaFX + GStreamer + embedded browser)** cause focus bugs, double DPI scaling problems (the Application options panel even documents a Windows compatibility work-around), and JavaFX NPEs on close (see the close ordering in `CoreComponentControl`).
7. **Thumbnail view** generates thumbnails per page with a fixed small thread pool and caches them on disk in the case folder. Video thumbnails need GStreamer/OpenCV. Changing the thumbnail size regenerates everything.
8. **Window layout fragility.** Layout is stored in NetBeans `Windows2Local`, and "Reset Windows" needs a full application restart. Core windows are force-redocked on each case open.
9. **Global singletons and EDT coupling.** Selection is propagated through the NetBeans `Lookup` and `ExplorerManager`. Popouts are frozen copies. There is no "follow selection" and no multi-selection comparison.
10. **Preferences are scattered** over four `.properties` files plus NbPreferences plus case-level preferences plus JVM `.conf` editing. Several settings require a restart (memory, threads, multi-user).
11. **Text-heavy viewers render HTML through Swing's HTML 3.2 `JEditorPane`.** This is slow for large documents, gives poor CSS support, and has no incremental search.

### 9.2 Recommended postmortem architecture (egui)

- **Data access layer separate from the UI thread.** All DB and content I/O runs on a worker pool (tokio or rayon plus channels). The egui frame only reads from immutable snapshots (`Arc<…>`) and draws placeholders ("loading…" skeleton rows) for missing data. Every request carries a generation/epoch id, so stale results from a superseded selection are dropped. This gives cancellation for free.
- **Virtualised tables over keyset-paginated queries.** Use `egui_extras::TableBuilder::rows()` / `body.rows(row_height, total_rows, |row| …)` so that only visible rows are laid out. Back it with a row cache that fetches blocks of, say, 500 rows by `(sort_key, obj_id)` keyset pagination from SQLite/PostgreSQL. `SELECT count(*)` runs asynchronously, and until it returns the scrollbar is estimated. Sorting and filtering are pushed into SQL (indexed columns), so they apply to the whole result set, not to one page. Remove the "page size" option, or keep it only as a soft cap for exports.
- **Lazy, columnar row model.** Store rows as compact structs (ids, interned strings, i64 timestamps). Format timestamps and sizes at draw time with the selected time zone. Only columns that are visible are fetched (projection pushdown).
- **S/C/O and translated names as async "enrichment" columns.** Batch them by visible row ids (`WHERE value IN (...)`, one query per visible block, not per row) and cache them per case. Sorting by them is enabled once the column is fully materialised, or use precomputed denormalised score columns in the postmortem schema (score is already aggregated per object in TSK `tsk_aggregate_score`).
- **Content-viewer selection pipeline.** On selection, compute a single `ViewerContext` once on a worker: object kind, MIME type, size, artifact types present, analysis results count, and whether there is a CR hit. Each viewer's `supports()` and `priority()` become pure functions of that context (no I/O). The tab strip updates in one frame. The active tab loads its data lazily, and inactive tabs load nothing until they are clicked.
- **Debounce selection** (e.g. 75–150 ms) during keyboard scrolling before kicking off heavy viewers (media, HTML, PDF), while lightweight viewers (hex first page, metadata) update immediately.
- **Hex and Strings over a memory-mapped or block-cached reader** with virtual scrolling over the entire file (no "pages"). Use offset-based scrolling, and goto-offset that jumps the scroll position. Strings extraction is incremental and runs in the background with progressive results.
- **Thumbnails**: an LRU GPU texture cache plus an on-disk cache keyed by `(obj_id, size bucket)`. Generate in the background with a priority queue ordered by visibility. Use pure-Rust decoders (`image`, `zune-*`, `jxl-oxide`, `libheif-rs` if allowed). Video frames come from `ffmpeg` via an optional feature or an external process.
- **One rendering stack.** Use egui for everything. Render HTML as sanitised text or a simplified layout (or via an optional `wry` webview in a popout). PDF via `pdfium-render` (binary dep) or `hayro`/`pdf-rs` text plus a page raster. Media via `ffmpeg` or `gstreamer-rs` optional.
- **Layout**: `egui_dock` with a serialisable `DockState`, multi-viewport popouts, "Reset layout" in-process, and per-case layout overrides optional.
- **Selection bus**: a central `Selection` state (`Vec<ObjRef>`, plus focus) with subscribers. Content popouts can be "pinned" (frozen) or "follow". Allow two pinned viewers side by side for comparison.
- **Settings**: one typed `Settings` struct (serde to TOML in the config dir) plus a `CaseSettings` struct in the case DB, with live application (no restarts). Keep the Autopsy keys as aliases for an importer.
- **Keyboard first**: a command palette, configurable shortcuts, type-to-search in tables (Autopsy only has the Swing default prefix search), and Ctrl+F in all text viewers with match highlighting and next/previous.
- **Accessibility and DPI**: egui handles DPI natively, so offer zoom (Ctrl +/-) and a font size setting, and remove the Windows High-DPI help text.

---

