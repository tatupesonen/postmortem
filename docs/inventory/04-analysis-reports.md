# 04 — Analysis Tools & Reporting: Feature Inventory

Scope: Autopsy analysis tools and reporting, documented so they can be reimplemented in **postmortem** (pure Rust, egui with multi-viewport popouts, own SQLite/PostgreSQL schema, multi-user) without reading the Java. Covered: Timeline, Communications (CVT), Geolocation, Discovery, Common Properties Search, All Cases Search, Data Source Summary, Health Monitor, Central Repository, Reporting (all report modules and the wizard), and Image Gallery.

Source root: `../autopsy` (read-only); most of the code is under `Core/src/org/sleuthkit/autopsy/<area>`, and Image Gallery is under `ImageGallery/`. Class names appear only as brief pointers. The descriptions focus on behaviour, defaults, algorithms, queries and the data produced.

Conventions:
- **(TSK)** marks facts that come from the sleuthkit Java bindings, which are not in this tree. Check these against the TSK sources before relying on exact IDs.
- Units are metric. Times are stored as epoch seconds unless stated otherwise.
- Each section ends with a list of Autopsy problems and postmortem ideas for that area. The final section collects the problems that span several areas.

## Contents

1. [Timeline](#timeline)
2. [Communications](#communications)
3. [Geolocation](#geolocation)
4. [Discovery (File & Domain)](#discovery-file--domain)
5. [Common Properties Search](#common-properties-search)
6. [All Cases Search](#all-cases-search)
7. [Data Source Summary](#data-source-summary)
8. [Health Monitor](#health-monitor)
9. [Central Repository](#central-repository)
10. [Reporting](#reporting)
11. [Image Gallery](#image-gallery)
12. [Performance/usability problems in Autopsy & improvement ideas for postmortem](#performanceusability-problems-in-autopsy--improvement-ideas-for-postmortem)

## Timeline

Source: `Core/src/org/sleuthkit/autopsy/timeline/**` (about 19.6k lines, JavaFX embedded in a NetBeans TopComponent). The event store and query engine (`TimelineManager`, `TimelineEventType`, `TimelineFilter`, `TimelineLevelOfDetail`) live in the sleuthkit Java bindings. No sleuthkit checkout was available when this was written, so the TSK-side facts below come from usage in this repo plus TSK knowledge. Items marked (TSK) should be verified against sleuthkit `TimelineManager.java` before copying numbers.

### Entry points & lifecycle
- **Open**: Tools menu > "Timeline", and a case toolbar button (26 px icon). These are enabled only when a case is open. If `countFilesWhere("1=1") > 6,000,000`, the app shows the info dialog "There are too many files in the DB to ensure reasonable performance. Timeline will be disabled." and disables the action. A module setting `timeline/enable_timeline=false` (set on Linux look-and-feel failure) disables it with "Timeline functionality is not available yet." A case with no data source shows "Could not create timeline, there are no data sources."
- **One controller per case**: it is created on `CURRENT_CASE` open and torn down on close (executor shutdown, window closed). A single-thread background executor serialises DB work.
- **During ingest**: a modal confirm appears: header "Ingest is still going, and the Timeline may be incomplete.", text "Do you want to continue?", buttons [Continue] [Cancel]. If a prompt is already open, it is brought to front instead of opening a second one.
- **Old cases**: if the case DB schema creation version is 8.2 or older, an info toast says events from older data sources will not show. Events have been generated at ingest time since Autopsy 4.13, and no separate timeline DB exists.
- **Window**: a floating window in its own "timeline" mode (can only dock with the timeline/ImageGallery modes). It is not persisted (`PERSISTENCE_NEVER`).
- **Dead code**: `PromptDialogManager.confirmRebuild` ("The Timeline DB is incomplete and/or out of date… Update DB / Continue Without Updating") and the "Populating Timeline Data" progress dialog are never called. Do not port them.

### Data model (case DB, TSK `TimelineManager`)
- **Tables (TSK)**:
  - `tsk_event_types(event_type_id, display_name, super_type_id)`
  - `tsk_event_descriptions(event_description_id, full_description, med_description, short_description, data_source_obj_id, content_obj_id, artifact_id NULL, hash_hit 0/1, tagged 0/1)`
  - `tsk_events(event_id, event_type_id, event_description_id, time)`. `time` is in **epoch seconds**, and (event_type_id, event_description_id, time) is unique.
  - Postmortem should keep this normalised split (descriptions are shared by the 4 MAC events of a file). It needs indexes on `time`, `event_type_id`, `data_source_obj_id`, `content_obj_id` and `artifact_id`.
- **TimelineEvent fields**: id, time (s), type, data source id, content obj id (file, or the source of the artifact), artifact id (optional), descriptions at 3 LODs, `hashHit`, `tagged`. The UI converts to ms everywhere (`time*1000`).
- **Description LOD** (`TimelineLevelOfDetail` LOW/MEDIUM/HIGH, shown as Short/Medium/Full). Each event carries all three strings. They are used for clustering in the Details view only.
  - File events (TSK): short = `<datasource>/<first path component>/`, medium = `<datasource><parent path>`, full = medium + file name. The user doc says "grouped by root folder when zoomed all the way out."
  - Web events: full = URL, medium = domain, short = top private domain. Web search: full = query text, short = program name.
  - Other artifact types: per-type attribute extractors in TSK.
- **Event generation** (all inside TSK, not in this package):
  - Files: when a file row is added, one event is created per MAC timestamp that is > 0: mtime→File Modified, atime→File Accessed, crtime→File Created, ctime→File Changed. A file with all four times ≤ 0 gets no events. Slack and unallocated-blocks files are excluded from "View File in Timeline".
  - Artifacts: on `Blackboard.postArtifact`, each artifact type mapped to an event subtype is converted using its configured datetime attribute and description extractors. A `TSK_TL_EVENT` artifact uses its `TSK_TL_EVENT_TYPE` attribute (manual and Plaso events). Other artifacts with any DATETIME attribute fall into the "catch-all" standard/custom artifact types.
  - Each insert fires `TimelineEventAddedEvent`, which the Case republishes as `TIMELINE_EVENT_ADDED`.
- **Plaso module mapping**:

  | Plaso source | Event type |
  |---|---|
  | `WEBHIST` (cookie in name/type) | Web Cookie |
  | `WEBHIST` (otherwise) | Web History |
  | `EVT`, `LOG` | Log Entry |
  | `REG`, usb/usbstor | Devices Attached |
  | `REG`, other | Registry |
  | anything else | Standard-artifact catch-all (dropped if its description is empty) |

- **Manual events**: see "Add Event" below. They are stored as `TSK_TL_EVENT` data artifacts, so they are multi-user safe and survive like any artifact.

### Event type hierarchy
- **Levels**: `HierarchyLevel` = ROOT, CATEGORY ("base type"), EVENT ("sub type"). Type IDs are TSK constants; postmortem may renumber them, but the tree is:

| Category | Sub-types (display names; icon file in `timeline/images`) |
|---|---|
| (root) "Event Types" | – |
| File System (`blue-document`) | File Modified (`-attribute-m`), File Accessed (`-a`), File Created (`-b`), File Changed (`-c`) |
| Web Activity (`web-file`) | Web Downloads, Web Cookies, Web Bookmarks, Web History, Web Searches, Web Form Autofill, Web Form Addresses (+ newer TSK: web cache etc.) |
| Misc Types / "Other" (`block`) | Messages, GPS Routes, Location History (GPS trackpoint), Calls, Email, Recent Documents, Installed Programs, Exif, Devices Attached, Log Entry, Registry, Metadata Created / Last Saved / Last Printed, GPS bookmark/last-known/search/track, program execution, … |
| Custom Types | User Created (`hand_point`), standard- and custom-artifact catch-alls |

- **Icons**: unknown types fall back to `timeline_marker.png`.
- **Colour algorithm** (legend, bars, nodes):
  - Root = HSB(359°, 0.9, 0.9).
  - Category i: hue = root hue + i·360/(nCategories+1).
  - Sub-type j: hue = category hue + j·(360/nCategories)/(maxChildrenOfAnyCategory+1).
  - Saturation and brightness are inherited.
  - Bars and legend squares use the type colour as fill and the parent type colour as stroke.

### View state, history, caches
- **State tuple** `EventsModelParams` = (time range [ms interval], type zoom level, root filter state, description LOD). It is immutable, and its equality includes the full filter tree.
- **Initial state**: the full spanning interval, CATEGORY, default filters, LOW, view mode **Counts**.
- **History**: a back stack, a forward stack and the current state. `advance(new)` is ignored if the new state equals the current one. Otherwise it pushes the current state, and clears the forward stack unless the new state equals the forward top (in which case it pops). `retreat` skips entries equal to the current state.
  - View mode, time zone, selection, pins, quick-hides and details layout settings are **not** in history.
  - Back = Alt+Left, Forward = Alt+Right (scene-wide). There are also buttons and a "Zoom History" context submenu.
- **pushTimeRange clamping**:
  - The requested range is intersected with the case spanning interval [min(time)·1000, max(time)·1000+1000).
  - If they do not overlap, the full span is used instead.
  - No history entry is made if the clamped range is unchanged. The method returns false in that case, and callers then just resync the UI.
- **Caches** (Guava; postmortem: LRU):
  - id→event: 5000 entries, expire 10 min after access.
  - params→counts-by-type: 1000 entries, 10 min.
  - min/max event time.
  - Details: params→event list, 1000 entries, 10 min.
- **Invalidation**:
  - `invalidateCaches(ids)` clears min/max, the given ids, all counts and the details cache, then posts `CacheInvalidatedEvent`.
  - Triggers: tag added/removed (content or artifact), hash-set-hit `DATA_ADDED`, `DATA_SOURCE_ADDED` (also re-reads the data-source list and adds new data-source filter rows), `TIMELINE_EVENT_ADDED`.
  - `FILE_DONE` is deliberately ignored ("HUGE slow downs during ingest"). Remote (multi-user) ingest events are ignored. Remote case events (tags) are still processed.

### Window layout & toolbars
- **Left column** (top to bottom):
  - "Display Times In:" with Local Time Zone (default) or GMT/UTC. This is a global static setting, not persisted, and every view refreshes when it changes.
  - "History" Back/Forward.
  - "Zoom" pane with 3 sliders (hidden in List mode).
  - Tab pane: **Filters** (funnel icon) and **Events** (enabled only in Details mode; selection jumps back to Filters otherwise).
- **Top toolbar**:
  - "View Mode:" segmented [Counts|Details|List].
  - View-specific settings inserted after it.
  - Right side: **Add Event**, **Snapshot Report**, **Refresh View**. Refresh is disabled unless the view needs a refresh.
- **Bottom time bar**:
  - A 32 px histogram with a range slider overlaid (70 % opacity).
  - Below it: "Start:" date-time field (200 px), zoom-out and zoom-in buttons, "Zoom in/out to" preset menu, "End:" field. List mode replaces the zoom buttons and preset menu with its own navigation controls.
- **Lower Swing split** (results):
  - Counts/Details: a DataResultPanel table of selected events (left) and the content viewer (right). The path label reads "`<start> to <end>`" (yyyy-MM-dd HH:mm:ss in the display zone).
  - List: content viewer only.
  - Bookmark-tag shortcut and external-viewer shortcut are bound in this window.
- **Status bar**: current task title, message and progress (the first task in the list), plus a hover hint "Drag the mouse to select a time interval to zoom into. Right-click for more actions."
- **No-events overlay**: when a refresh returns no events, a grey 30 % overlay shows "No Visible Events – There are no events visible with the current zoom / filter settings." with buttons Zoom to events, Back, Reset all filters and dismiss.
- **Busy overlay**: while a view refreshes, a masker pane shows task message and progress, and the cursor is WAIT.

### Zooming (three independent axes)
- **Time Units slider**:
  - Labels are Years, Months, Days, Hours, Minutes, Seconds. The current position = the RangeDivision unit of the current range.
  - Choosing label L pushes a range one unit coarser around the current midpoint, so the axis then ticks in L: Years → full range, Months → 1 year, Days → 1 month, Hours → 1 day, Minutes → 1 hour, Seconds → 1 minute.
  - This is an off-by-one in code, but it is the effective behaviour. Keep it.
- **RangeDivision** (bin size for the axis, counts and histogram), first rule that matches:
  - range > 3 years → YEARS
  - > 3 months → MONTHS
  - > 3 days → DAYS
  - > 3 hours → HOURS
  - > 3 minutes → MINUTES
  - otherwise → SECONDS
  - Bounds are floor(start) and ceil(end) to that unit, in the display time zone.
- **Tick formats**:

  | Unit | Format |
  |---|---|
  | Years | `YYYY` |
  | Months | `YYYY-MMMM` |
  | Days | `YYYY-MMMM-dd` |
  | Hours | `…dd HH` |
  | Minutes | `…HH:mm` |
  | Seconds | `…HH:mm:ss` |

  - The axis is two rows. Each label is split at its last space, `-` or `:` into "context" (lower row, one label per run with a 1 px left divider) and "specific" (upper row, 10 pt, dropped if it overlaps the previous label).
- **Event Type slider**: CATEGORY ↔ EVENT (ROOT hidden). It affects Counts (stacking granularity) and legend colours. Details always clusters by category (see below).
- **Description Detail slider**: Short/Medium/Full. Disabled in Counts mode.
- **Zoom in / zoom out buttons**:
  - Zoom in removes ¼ of the duration from each side ("about half as much time").
  - Zoom out adds ¼ to each side ("about 50 % more time"). Zoom out is disabled when the range already contains the whole span.
- **"Zoom in/out to" presets**: 1 min, 15 min, 1 h, 6 h, 12 h, 1 day, 3 days, 1 week, 2 weeks, 1 month, 3 months, 6 months, 1 year, 3 years, 5 years, 10 years, All. Each sets width = period, centred on the current midpoint. "All" is 1,000,000 years, which is clamped to the span.
- **Zoom to events** (no-events dialog): TSK `getSpanningInterval(currentRange, filter, tz)` expands the range outward to the nearest matching events on each side. Disabled when the range already contains the span.
- **Date-time pickers** (start/end):
  - Editing one pushes `range.withStart/EndMillis`. Invalid input is silently ignored.
  - Values outside the span fail validation. Calendar days entirely outside the span are disabled.
- **Range slider**:
  - Values are offsets in ms from the RangeDivision lower bound of the span.
  - The range is pushed only on drag release (not while changing), as [low+min, high+min+1000).
  - Both thumbs move independently. Dragging the middle bar pans the range.
- **Histogram**:
  - Bins = RangeDivision of the whole span. The code queries **one `countEventsByType` per bin**, with the current filters (the docs wrongly say filters do not apply). The whole bar row is re-rendered after each bin.
  - Bar height = ln(count)/ln(max) × 32 px. Bars are grey with a lighting effect and a hover tooltip showing the count.
  - Rebuilt on refresh requests. A previous rebuild task is cancelled.
- **Interval selector** (Counts and Details):
  - Mouse-drag on empty chart space creates a translucent band with start/end labels.
  - Hover shows [Zoom] and [x]. Double-click the band zooms; right-click or [x] clears it.
  - Drag either edge (within 1.5 px) to resize, or the middle to move. The cursor shows W/E-resize or hand.
  - Counts snaps to bin bounds: floor(start)..ceil(end) + one bin. Details uses the exact span.
  - Cleared whenever the time range changes.

### Filters (Filters tab; TSK `RootFilter` = AND of all active sub-filters)
- **Tree table**: columns are checkbox+name and a legend (type icon plus a 20×20 colour square, indented 10 px per hierarchy level; the square is only coloured for types at the current type-zoom level). The text filter's legend cell is a text field with the prompt "enter filter string".
- **Editing does not apply**: checkbox and text edits only change the panel's copy. Changes take effect on **Apply** (tick icon, "(Re)Apply filters").
  - Gotcha: any type-zoom, LOD or time-range change also auto-applies the pending panel state.
- **Reset**: "Reset" / "Reset all filters" restores the defaults. It is disabled when the current state equals the defaults.
- **Filter states**: each filter state has *selected* and *disabled*; active = selected ∧ ¬disabled.
  - A compound filter disables its children when it is inactive.
  - Selecting any child selects the parent. Deselecting all children deselects the parent.
  - Selecting a parent with no selected children selects all children.
- **Row context menu**: "select" submenu with all / none / only (this row) / others, applied to the siblings.

| Filter (UI order) | Default | SQL semantics (TSK) |
|---|---|---|
| Hide Known Files | unselected; **hidden from UI** (known_status column removed from tsk_events, logic not re-implemented) | exclude events whose file `known = KNOWN` |
| Text Filter | unselected, empty | description LIKE `%text%` (doc: searches the full description even if a shorter LOD is shown) |
| Tags Filter | unselected | `tagged = 1` (event's file or artifact has any tag) |
| Hash Hits Filter | unselected | `hash_hit = 1` |
| Data Sources (compound) | unselected; one child per data source (`name`, id), added live on DATA_SOURCE_ADDED | `data_source_obj_id IN (selected)` |
| File Types (compound) | unselected; children Media, Documents, Executables (FileTypeUtils MIME sets) + "Other" = NOT IN union of those | join file `mime_type IN/NOT IN (…)` |
| Event Type Filter (compound, ROOT) | unselected; children = categories, grandchildren = subtypes | `event_type_id IN (subtree)` |

- Because every default is "unselected/inactive", the default view shows **all** events. Checking a child narrows the results.
- **Quick-hide ("Hidden Descriptions" pane)**: shown below the tree in Details mode only.
  - Entries are added by **Hide** on a stripe, a (description, LOD) pair matched case-insensitively.
  - Checkbox checked = hidden. Context menu: "Unhide and remove from list" or "Remove from list".
  - Applied in the layout pass (hidden nodes are unmanaged). It is not SQL, not part of history, and not persisted. The pinned lane ignores it.
- **Data-source summary panel** "View in Timeline": pushes default filters with only that data source selected and a range = its recent-activity days (+1 day), then opens the timeline.

### Counts view
- **Stacked bars**:
  - One category per bin of RangeDivision(current range). For each bin, `countEventsByType(start_s, end_s, activeFilter, typeZoom)` (cached) runs, then one series per event type at the zoom level.
  - Category gap 2 px, alternating row fill, no legend, animated.
- **Scale toggle**: **Logarithmic (default)** or Linear.
  - Log height per segment = log10(count)+1, with y-axis labels hidden. Each scale has a help popover explaining it.
  - Linear: y upper bound = 1 + max·1.2, tick = 10^max(0, floor(log10 max)−1), integer labels only.
  - Bin labels are **bold** if the bin has any events.
- **Bar segment rendering**: 2 px border in the parent colour, hover drop shadow (10 px, type colour). Tooltip: "`N <type> events\nbetween <bin start>\nand <bin end>`" with the type icon.
- **Single click** on a segment: select events of (bin, type). The events are loaded in the background via `getEventIDs(range ∩ span, filter ∧ type)` and sent to the result table. The segment gets a lighting effect.
- **Double click**: zoom to the bin. If the bin is ≤ 1 s, the app asks "There is no temporal resolution smaller than Seconds. Would you like to switch to the Details view instead?"
- **Right click on a segment**:
  - Select Time Range: all types in the bin, using the ROOT type.
  - Select Event Type: that type over the whole span; highlights the whole series.
  - Select Time and Type.
  - (separator) Zoom into Time Range (only if > 1 s).
  - Zoom History > Back/Forward.
- **Empty-area right-click**: Zoom History only. **Empty-area left click**: clears the selection.

### Details view
- **Data fetch**: `TimelineManager.getEvents(range, activeFilter)` returns full event rows (cached by params) and runs on a background task.
- **Clustering algorithm** (`DetailsViewModel`):
  1. For each event, key = (**category type**, description at the current LOD). The sub-type is ignored even at EVENT type zoom. Create a point cluster [t,t].
  2. Per key, sort the clusters by start. Merge each next cluster into the current one if the gap between them is null (overlap or abut), or if gap ≤ ¼ × (one RangeDivision unit of the visible range). Merging unions the span, event IDs, hash-hit IDs and tagged IDs.
  3. Group the merged clusters with the same (type, description) into one **EventStripe**: a horizontal row holding several clusters.
  4. Sort the stripes by start time.
- **Size guard**: if there are > 2000 stripes, a modal warning offers [Continue] / [Back (Cancel)]: "You are about to show details for N events. This might be very slow and could exhaust available memory." Cancelling (also on List refresh) calls history **retreat**.
- **Refresh skip**: the refresh is skipped if params are unchanged and the view is not marked dirty.
- **Node kinds** (min height 24 px):
  - *Stripe*: an info row with type icon, hash-hit icon (if any hash hits), tag icon (if any tagged), description (centre-ellipsis) and " (n)". Below the row are its cluster bars. A single-cluster stripe hoists the cluster's −/+ buttons onto itself.
  - *Cluster*: a bar from x(start) to x(end) (min 1 px). The border is 2/1 px in the type colour at 40 % alpha, the background is the type colour at 10 % alpha, and it shows the count only.
  - *Single event*: 2 px left border, icon and full description.
  - Child descriptions are rendered as "    ..." + suffix after the parent description.
- **Hover controls**: shown on the hovered node only (the parent's are hidden). They are Pin/Unpin, Hide (stripes, primary lane only), and −/+ collapse/expand (clusters). Hover also adds a drop shadow.
- **Tooltip**: "`N <type> events\n<desc>\nbetween <start>\nand <end+1s>`", built lazily in a background thread.
- **Expand (+, or double-click a cluster)**:
  - Re-query only that cluster's span [start, end+1 s) with the current filters, AND the cluster's category type, AND a UI predicate "description at the parent's LOD equalsIgnoreCase the parent description", at LOD+1.
  - If that yields exactly 1 stripe, keep going to the next LOD.
  - The result is nested stripes, or single-event nodes for 1-event stripes.
  - **Collapse (−)** steps back one LOD. Reaching the original LOD removes the children.
  - + is disabled at Full; − is disabled at the original LOD.
- **Layout pass** (`layoutEventBundleNodes`, recursive for children):
  - Nodes are sorted by start.
  - x = axis position(start) − parent's x.
  - Rows are placed greedily with a range map y-band → rightmost x used. Starting at yTop = minY, the code scans the node's height in 24 px steps. If any band's maxX ≥ xLeft − 4 px, it moves down to that band + 4 px and retries. It then records [yTop, yTop+h] → xLeft + w + 4.
  - *One Per Row* instead stacks every node below the previous one (+4 px).
  - *Band by Type* runs the layout separately per category, each band starting at the previous band's max y.
  - Nodes animate to their new positions over 100 ms. Lane height = max y, which drives a custom vertical scrollbar.
- **Axis**: x = (t − lower)/(upper − lower) × (width − **200 px**). The 200 px reserved on the right lets labels of late events fit. Lower/upper are the RangeDivision floor/ceil.
- **Layout settings** (toolbar; session-only, reset whenever the view is recreated):
  - Band by Type: off.
  - One Per Row: off.
  - Truncate Descriptions: off, width slider 50–500 px, default 200 px, in "Advanced Layout Options".
  - Description: Show Full Description (default) / Show Counts Only / Hide Description.
  - "Pinned Events" toggle.
- **Selection and highlight**:
  - Click = select only; Shift+click = add; Ctrl/Cmd+click = remove; click on empty space = clear.
  - Selected nodes get a 2 px darker border, and their event IDs go to the result table.
  - Each selected cluster is projected as a 5 px rounded line (type colour, 50 % alpha) on the time axis.
  - Highlight = bold text plus 30 % background, applied to selected nodes and to nodes whose description equals a description selected in the Events tree.
- **Pinning**:
  - Pin/Unpin puts the event object (stripe, cluster or single, a frozen snapshot) into a controller-level set.
  - A separate "Pinned Events" lane is shown above the main lane in a split pane. It opens automatically when the first pin is added; min height 50 px, max = content + 30 px.
  - Pins survive view-mode switches and zoom changes within the session, but are not persisted.
- **Marker (guide line)**: context menu "Place Marker" adds a vertical dashed line at the click x. It is draggable, its tooltip shows the date-time, right-click removes it, and all markers are cleared on time-range change.
- **Chart context menu**: Place Marker; Add Event (time prefilled from the click x); separator; Zoom History. Node menus prepend Pin/Unpin plus Expand/Collapse or Hide.
- **Scrolling**: mouse wheel; ↑/↓ = 10 % of the lane height; PgUp/PgDn = 70 %; clamped to [0, max+50].
- **Events tab (tree)**:
  - Mirrors every displayed stripe, cluster and single event: category, then description, nested by stripe parentage.
  - Label = "desc (n)", with the parent prefix stripped.
  - Sort by Description (default) or Count (desc); always grouped by type first.
  - Multi-select highlights the matching nodes. Chart selection selects the tree items.
  - Hidden descriptions are greyed (60 %) and collapsed. Right-click offers Hide/Unhide.

### List view
- **Data**: `getEvents(range, activeFilter)` returns every event in the range, with no limit and no paging.
- **Combining rows**: events are grouped by (time s, content obj id, full description). If all events in the group are File System sub-types (or there is only one), they form a single row. Otherwise there is one row per type. Rows are sorted by time.
- **Columns**: Date/Time (zoned `YYYY-MM-dd HH:mm:ss`); Event Type; Description (full, centre-ellipsis " ... ", tooltip); Tagged; Hash Hit. An ID column exists but is removed.
  - Event Type for MACB rows: File System icon plus a positional letter string, one letter per FS sub-type (M, A, B=created/"born", C=changed) with `_` for absent ones. The tooltip lists each type.
  - Tagged: the icon's tooltip shows "Tags:" plus the tag names on the file and artifact.
  - Hash Hit: the tooltip shows "Hash Sets:" plus the file's set names.
- **Footer**: "N events". Multi-select maps rows to **one representative event ID per row**.
- **Navigation**: "Jump By:" Years (default) / Months / Days / Hours / Minutes / Seconds, plus First, Previous, Next and Last buttons.
  - Next = the first row whose time ≥ (focused row time + 1 unit, with smaller fields truncated to their minimum).
  - Previous = the nearest earlier row whose time ≤ (time − 1 unit, with smaller fields set to their maximum).
  - The target row is scrolled to the centre (24 px row assumption).
- **Row context menu**: the full Autopsy node action set (open, extract, tag, View File/Source File in Timeline, …) minus Properties and Tools.
- **Ignored inputs**: the list ignores LOD and type zoom. The Zoom pane is hidden.

### "View in Timeline" from elsewhere
- **Actions**:
  - "View File in Timeline…" on file and directory nodes. Disabled for slack and unallocated files, and when all MAC times are ≤ 0.
  - "View Source File in Timeline…" and "View <assoc> File in Timeline" on artifact nodes.
  - "View Result in Timeline…" on data artifacts (not analysis results) that have any DATETIME attribute.
  - Also used from Discovery, File Search, Geolocation and the Data Source Summary.
- **Show-in-timeline dialog** (modal):
  - Table of the item's events (Date/Time, Event Type with icon), height min(200, 24·n+28) px.
  - For a file, the user must pick a row ("You must select an event"). For an artifact, the first row is preselected and the table is hidden.
  - "Choose the amount of time to show before and after the selected event": a spinner 1–1000 (default 1; digits only) plus a unit (Years, Months, Days, Hours, **Minutes (default)**, Seconds).
  - [Show Timeline] opens the window in **List** mode with range [t − d, t + 1 ms + d] and that event selected and scrolled to.

### Add Event (manual)
- **Dialog** (document-modal): DataSource choice "name (ID: n)" (first preselected); Description (required); DateTime (defaults to now, or to the clicked chart time); Time Zone combo with autocomplete, default = timeline zone. Validation disables OK.
- **Save**: creates a `TSK_TL_EVENT` data artifact on the chosen data source with attributes TL_EVENT_TYPE = User Created, DESCRIPTION and DATETIME (epoch s). The source string is "Manual Creation: <examiner login>". The artifact is then posted to the blackboard, so the event appears via `TIMELINE_EVENT_ADDED` (cache invalidation plus the "may be out of date" banner).

### Snapshot report
- **Name prompt**: Swing input dialog, "Enter a report name for the Timeline Snapshot Report." An empty name means the default `<case display name> MM-dd-yyyy-HH-mm-ss` (filename-escaped). Cancel aborts. Invalid path characters give an error dialog.
- **Output**: `<case>/Reports/<name>/Timeline Snapshot/`, containing:
  - `index.html`, `nav.html`, `summary.html` (branding, generation time, ingest-running flag, case info)
  - `snapshot.html` (PNG plus a table: time range in full date-time, LOD, type zoom, filters tree with [x]/[ ] marks)
  - `snapshot.png`: an image of the current view node only (no filter panel)
  - CSS, favicon, and the agency/generator logos
- **Registration and success dialog**: the report is registered with `Case.addReport(path, "Timeline", name)`. A success dialog offers [Open Report] / [OK].
- **Template bug**: the template still references the old getters `getDescriptionLOD`, `getTypeZoomLevel` and `getFilter`, so the LOD, zoom and filter rows render **empty**. Postmortem should fix this.

### Refresh & DB-change behaviour
- **Automatic refresh**: views refresh automatically on any params change or time-zone change. The previous refresh task is cancelled.
- **External changes** (tags, hash hits, new events, new data source): caches are invalidated, but the view is **not** auto-redrawn. A dark notification bar appears instead: "The event data has been updated, the visualization may be out of date." or "Tags have been created and/or deleted. The view may not be up to date."
  - The bar has a [Refresh View] action that re-runs the view and rebuilds the histogram.
  - Detail nodes do flip their tag icon live on TagsAdded. On TagsDeleted the icon is (buggily) shown again rather than hidden.
- **Selection reset**: the selection clears on view-mode change and on any params change (Details).

### Keyboard & mouse summary
| Input | Effect |
|---|---|
| Alt+← / Alt+→ | history back / forward |
| drag on empty chart | interval selector; dbl-click it = zoom, right-click = clear |
| click / Shift / Ctrl(Cmd) click node | select / add / remove (Details) |
| dbl-click cluster | expand one LOD (Details) |
| dbl-click count bar | zoom to bin (or offer Details if ≤ 1 s) |
| wheel, ↑↓, PgUp/PgDn | vertical scroll of detail lanes |
| right-click | context menus listed above |
| bookmark/external-viewer shortcuts | standard Autopsy shortcuts in the result area |

### Edge cases
- **Clamping**: time ranges are clamped to the event span. With zero events the span is degenerate, and the time UI is skipped when max ≤ min.
- **Event time resolution**: times are whole seconds. Single-event clusters have zero width. End labels add 1 s.
- **Time zone**: all binning and labels follow the display zone (DST-aware via Joda). Stored times are UTC seconds.
- **Large result sets**: the result table shows at most 5000 nodes. Above that it shows a single "Too many events to display. Maximum = 5000. But there are N to display."
- **Late data sources**: data sources added after the timeline opened appear as new data-source filter rows (unselected) in all states.
- **Missing artifact**: a missing artifact id is stored as 0/NULL; nodes treat 0 as "no artifact".

### Data persisted
- In the case DB (TSK): the event tables, the hash_hit and tagged flags (updated on tag and hash events), and `TSK_TL_EVENT` artifacts for manual events.
- Report files plus a `reports` row for snapshots.
- Nothing else: view mode, zoom, filters, history, pins, quick-hides, markers, layout settings and time zone are all session-only.

### Autopsy problems & postmortem ideas (Timeline)
- **Per-bin query storms**:
  - Counts and the histogram run one `countEventsByType` per bin, up to about 180 queries per refresh. The histogram repaints the entire bar row after every bin.
  - Fix: a single `GROUP BY bucket, type` SQL using `(time - offset)/width` for fixed-length units, and calendar-aware bucketing (precomputed bin edges plus `CASE`/range join, or a `generate_series` join on PostgreSQL). Cache per (filter-hash, bins).
- **Details loads every event row into memory** (no LIMIT) before clustering in Java. Instead, cluster in SQL or a streaming iterator: `GROUP BY category, desc_at_lod ORDER BY time` and merge gaps while streaming. Fetch only ids, time, type and the needed LOD string.
- **The 2000-stripe modal and the List view's unbounded table** are workarounds for a slow scene graph.
  - Use an immediate-mode egui painter with spatial culling: draw only visible rows and bins.
  - The list should be a virtualised table over a SQL cursor/keyset (`ORDER BY time, event_id`), with "jump by unit" done as a SQL seek.
- **Threading**:
  - FX thread, AWT thread and a single executor are juggled with `Platform.runLater`. There are races (selection versus refresh), synchronized blocks around blocking DB calls, and tooltips that spawn a raw `Thread` per hover.
  - Postmortem: one async query worker pool with generation counters (drop stale results), and pure immutable view state in the UI thread.
- **Filter-panel UX**: edits are silently applied by unrelated zoom changes, there is an explicit Apply button, and "unselected = no filtering" is confusing. Prefer live apply (debounced 200 ms) with the checkbox meaning "include" (default all checked), an "n of m" summary, and a visible active-filter chip bar. Port the Hide Known filter properly (join `tsk_files.known`).
- **Clustering ignores the Event Type zoom** (always category), and the time-unit slider is off by one. Make sub-type clustering honour the zoom, and give the slider honest labels ("show ~1 year").
- **The histogram ignores the docs** (it is filtered). Pick one meaning: unfiltered context (cheap, cacheable) plus a filtered overlay line.
- **No persistence of investigative state**:
  - Persist named "timeline views" (params plus pins plus quick-hides plus markers plus layout) per case and per examiner in the postmortem DB.
  - Allow multiple timeline windows (multi-viewport popouts) with independent states, synced via an optional "link selection" toggle.
- **Out-of-date banner requires a manual refresh**: auto-refresh when the window is idle, or show a diff count ("+124 events since view"). Multi-user: subscribe to remote event and tag notifications and invalidate per-case caches rather than ignoring remote ingest.
- **Bugs not to replicate**:
  - Snapshot template shows empty LOD, zoom and filter rows.
  - TagsDeleted handler re-shows the tag icon.
  - List selection maps a combined MACB row to only one event ID.
  - Range-slider push adds a hard-coded +1000 ms.
  - 6M-file hard cap disables the feature.
- **Snapshot**: render to PNG and SVG from the egui painter at chosen dimensions (not a screen grab). Also export the visible events as CSV/JSON alongside the HTML.
- **Accessibility**: colours are generated hues with no colour-blind check. Use a curated categorical palette plus icons, and keyboard navigation between nodes (←/→ by time, Enter = expand, P = pin, H = hide).

## Communications

Autopsy name: "Communications Visualization Tool" (CVT). Opened from Tools > Communications or a case toolbar button. It is only enabled while a case is open in GUI mode. It runs as a single floating window and is not persisted between sessions.

### Window layout
- Left side has two tabs, **Browse** (accounts table) and **Visualize** (graph), and a collapsible **Filters** side tab (click the arrow to hide or show it). Right side is the **relationship browser**, a split at 25 %, with five tabs: Summary, Messages, Call Logs, Contacts, Media Attachments.
- Switching Browse/Visualize clears the relationship-browser selection. Every new selection forces the right pane back to the **Summary** tab.
- Components talk over a single in-process event bus. Events: `FilterChange(filter, startDateState, endDateState)`, `PinAccounts(set, replace)`, `UnpinAccounts(set)`, `ScaleChange(zoom)`, `StateChange(historyState)`.

### Data model (TSK CommunicationsManager; reimplement in our schema)
- **Account** = (type, type-specific ID, e.g. normalised phone number or e-mail). **Account device instance (ADI)** = account + device ID (the data source's device_id). The same account seen on two data sources gives two ADIs, so two rows and two graph vertices.
- **Relationship** row: `account1, account2, relationship_source_obj_id (artifact), date_time, relationship_type (MESSAGE | CALL_LOG | CONTACT), data_source_obj_id`. E-mails count as MESSAGE. A contact book entry gives a CONTACT relationship between the device account and the contact's account(s).
- **Account types**: the predefined TSK list (DEVICE, PHONE, EMAIL, FACEBOOK, TWITTER, INSTAGRAM, WHATSAPP, MESSAGING_APP, WEBSITE, SKYPE, VIBER, LINE, IMO, TANGO, TEXTNOW, THREEMA, XENDER, ZAPYA, SHAREIT, … and CREDIT_CARD) plus custom types. Each type has an icon (`Accounts.getIconFilePath`). CREDIT_CARD is always excluded from CVT.
- API calls used, each with the current filter:
  - `getAccountDeviceInstancesWithRelationships(filter)`: rows of the accounts table.
  - `getRelationshipSourcesCount(adi, filter)`: the "Items" count and the vertex size. Counts distinct source artifacts.
  - `getRelatedAccountDeviceInstances(adi, filter)`: graph neighbours.
  - `getRelationshipCountsPairwise(set<adi>, filter)`: Map<(adi1, adi2), count> used for edge weights.
  - `getRelationshipSources(set<adi>, filter)` and `getRelationshipSources(adi1, adi2, filter)`: source artifacts feeding every right-hand tab.
  - `getAccountFileInstances(account)`: files where the account was found (Summary "File References").
  - `getAccountTypesInUse()`.
- Filter = AND of the sub-filters below.

| Sub-filter | Semantics |
|---|---|
| DeviceFilter(set deviceId) | relationship.data_source's device_id is in the set |
| AccountTypeFilter(set type) | the returned/related account's type is in the set |
| DateRangeFilter(start, end) | epoch seconds, 0 = unbounded. `date_time >= start`, `date_time <= end`. TSK adds 86 400 s to a non-zero end so the end *day* is inclusive |
| RelationshipTypeFilter | always {CALL_LOG, MESSAGE, CONTACT} |
| MostRecentFilter(N) | N > 0 limits to the N most recent relationships by date_time, a global sub-select `ORDER BY date_time DESC LIMIT N`. -1 means all |

### Filters panel
- **Devices**: one checkbox per data source that has at least one row in `account_relationships` (`SELECT DISTINCT data_source_obj_id`). Labelled with the data source name, sorted case-insensitively, with Check All / Uncheck All.
- **Account Types**: the DEVICE type is always present, plus `getAccountTypesInUse()` minus CREDIT_CARD. Each entry has an icon and display name, with Check All / Uncheck All.
- **Date Range (<zone>)**: Start and End checkboxes, each enabling a date picker. Both are **unchecked by default**. The pickers default to today−3 weeks and today, then get overwritten with MIN/MAX(`account_relationships.date_time`) as dates in the display zone.
  - Veto rule: start ≤ end when both are enabled.
  - The zone is the user's "display times in local / selected time zone" preference, and the label updates live.
  - Each date is converted as start-of-day in that zone, to epoch seconds.
- **Communications Limit**: "Most Recent:" is an editable combo with values All, 10000, 5000, 1000, 500, 100. The default is **All**.
- **Validation**: at least one device, at least one type, and a limit of "All" or a positive int. Otherwise a red "Select at least one." or "Invalid integer value." message is shown and Apply is disabled.
- **Apply**: posts a FilterChange event. The accounts table, graph and history all react.
- **Refresh**: does the same as Apply. It is only enabled while a red "Displayed data may be out of date. Press Refresh to update." banner is showing.
- **When the banner appears**:
  - On an ingest DATA_ADDED event for TSK_MESSAGE, TSK_CONTACT, TSK_CALLLOG or TSK_EMAIL_MSG.
  - On ingest job COMPLETED.
  - In multi-user mode, remote nodes' ingest events are forwarded through the same channel.
- **Loading options**:
  - On window open: a background load runs ("Loading filters..."). All found devices and types are **checked**, then filters are applied automatically.
  - On later DATA_ADDED events: options reload through a throttled refresher, at most once per 5 s. Newly discovered devices and types are added **unchecked**, with no auto-apply.
  - On case change: the lists are cleared.

### Browse tab (accounts table)
- Rows are the ADIs from `getAccountDeviceInstancesWithRelationships`. Columns:
  - **Account**: type icon + type-specific ID.
  - **Device**: data source name, falling back to the device ID.
  - **Type**: display name.
  - **Items**: relationship-source count under the filter.
- Default sort is Items descending. Multi-select is allowed. Column widths auto-fit from the first 100 rows, capped at 500 px.
- The table is lazily rebuilt (background) on each FilterChange and on history navigation.
- Tooltip: "Contact: <contact name> - Persona: <persona name>[(1 of N)]".
  - Contact name comes from the first contact artifact whose TSK_NAME*, TSK_PHONE*, TSK_EMAIL* or TSK_ID* attribute equals the account ID. That lookup is cached for 10 min and invalidated on TSK_CONTACT DATA_ADDED or case close.
  - Persona comes from the Central Repository (CR) and is cached for 5 min. "None" if there is none or CR is disabled.
- Selecting rows sends SelectionInfo(selected ADIs, no edges, filter) to the relationship browser.
- Context menu:
  - "Add Selected Account(s) to Visualization" pins the accounts, adding to existing pins.
  - "Visualize Only Selected Account(s)" resets the graph and pins only these.
  - Both switch to the Visualize tab.

### Visualize tab (relationship graph)
- **Empty state**: placeholder text "Right-click an account in the Browse Accounts table, and select 'Visualize' to begin." (shown when nothing is pinned).
- **Rebuild algorithm**: background task with a modal progress dialog and a Cancel button. Steps:
  1. For each pinned ADI, add its vertex and collect `getRelatedAccountDeviceInstances`.
  2. Run `getRelationshipCountsPairwise(pinned ∪ related)`.
  3. Add an edge for each pair **where at least one end is pinned**. Edges between two related-only accounts are omitted.
  4. Re-run the current layout and fit the view.
- On cancel, the graph is reset (pins cleared), which shows the empty placeholder.
- **Vertex**:
  - Key = account ID + device ID. Ellipse of diameter √(count)+10 px, where count = relationship-source count.
  - Initial position is random inside 400×400.
  - Label is HTML: [pin icon if pinned][lock icon if locked][type icon] account ID. Font size = round(ln(count)+5) px.
  - Tooltip adds "data source: <deviceId>".
- **Edge**: undirected, no arrows, no label. Stroke width = ln(count)+1. Tooltip "<v1 - v2><br>N relationship(s)". Drawn behind vertices.
- **Interaction**:
  - Rubber-band and click selection. Vertices can be dragged; they cannot be resized, edited or connected.
  - Wheel zooms in or out by one step per notch.
  - Selection change sends selected vertices plus selected edges (as ADI pairs) to the relationship browser.
- **Right-click on a vertex**:
  - Lock or Unlock Selected Account(s): locked vertices are ignored by the layout, so they keep their position.
  - If the vertex is pinned: "Remove Selected Accounts" (unpin), which removes the vertex and its non-shared neighbours on rebuild.
  - Otherwise: Add to Visualization or Visualize Only. Pinning a neighbour **expands** the graph by its relations.
- **Toolbar**:
  - Back / Forward (history).
  - Clear (unpins everything).
  - Layout button "Fast Organic" (redraws). Circle, Organic and Hierarchical layout classes exist in the code but have no button. Each layout honours locks; errors show "<layout> layout failed [with locked vertices]…".
  - Zoom label "%".
  - Zoom out, Fit, 100 %, Zoom in.
  - "Snapshot Report".
- **Fit** algorithm: reset to 100 %, translate cells to the origin, then zoom = average(widthRatio, heightRatio). This is not the minimum of the two, so the graph may overflow.
- **History** (back/forward stack of states). A state is (filter, pinned set, zoom or −1, date-control states). New states are pushed on every filter apply, pin, unpin and **zoom**.
  - Restoring a zoom state only sets the scale.
  - Restoring any other state broadcasts StateChange, so the filters panel re-checks boxes and the table reloads. It then resets the graph, re-pins and rebuilds. Locks are lost.
- **Snapshot report**:
  - Prompts for a name. The default is `<case display name> MMddyyyyHHmmss` (file-escaped). A blank name asks to confirm the default; an existing folder asks to overwrite.
  - Writes to `<case>/Reports/<name>/Communications Snapshot/`:
    - `snapshot.png`: graph rendered at the current scale on white.
    - `snapshot.html`: title, date range as "MMMMM dd, yyyy", every data source with a disabled checkbox showing the selection, every predefined account type except CREDIT_CARD with its checkbox state.
    - Generic snapshot `index.html`, `summary.html`, logos and favicon.
  - The report is registered with the case (module "Communications"). An "Open Report" button opens it with the OS handler.
  - Bug: an overwritten report is not re-registered.

### Relationship browser tabs
All tabs take the SelectionInfo: selected ADIs + selected edges + filter. The source set = union of `getRelationshipSources(selectedADIs)` and `getRelationshipSources(a, b)` per edge, keeping BlackboardArtifacts only. Each tab recomputes this itself.

| Tab | Content |
|---|---|
| **Summary** | Only when exactly one account is selected; otherwise "Summary information is not available when multiple accounts are selected." See details below the table. |
| **Messages** | Two cards. See details below the table. |
| **Call Logs** | TSK_CALLLOG sources sorted by start time ascending. Columns: Device (data source's device-account ID, cached per data source), Direction, To/From, Date/Time, Duration(seconds). See details below the table. |
| **Contacts** | TSK_CONTACT sources. Columns: Name (TSK_NAME, else TSK_NAME_PERSON), Email, Phone. See details below the table. |
| **Media Attachments** | Thumbnail grid of every attached file, sorted by size then name. See details below the table. |

**Summary tab details**
- Account line: phone accounts are formatted international via libphonenumber, with "Country: XX" (region code). Other types show the raw ID.
- Description: "This account represents a device in the case." for DEVICE accounts; otherwise "…referenced by a device in the case."
- Counts are computed in the background, showing "Loading...." meanwhile:
  - **Messages** = TSK_MESSAGE + TSK_EMAIL_MSG.
  - **Call Logs**.
  - **Book Entries** = contacts that are *not* about this account.
  - **Communication References** = contacts with any PHONE* or EMAIL* attribute that, once normalised, equals the account ID.
  - **Total Attachments**: from the TSK_ATTACHMENTS JSON (file + URL attachments), or legacy child files of the artifact.
  - **Media Attachments**: attachments with thumbnail support.
- **File References**: unique paths of the account's file instances.
- **Other Occurrences**: CR cases with the same correlation value (account type → CR correlation type; not available for DEVICE). Columns: Case Name, Creation Date. Without CR: "<Enable Central Repository to see Other Occurrences>".
- **Personas**:
  - Without CR, a message is shown.
  - With personas: a list of names, each with a **View** button (persona dialog).
  - With none: "No personas found" plus **Create**. Create opens the persona editor with name = account ID and the CR account pre-added at confidence HIGH, or warns "Unable to find an account with identifier X in the Central Repository."

**Messages tab details**
- **Threads card**:
  - Groups TSK_MESSAGE and TSK_EMAIL_MSG sources by TSK_THREAD_ID. Messages with no thread ID go under "Unthreaded".
  - Representative row per thread: earliest e-mail by DATETIME_SENT, latest message by DATETIME (the representative rule is inconsistent).
  - Columns: Type, Earliest Message (date), Subject. For SMS the subject is replaced by the text, truncated to 120 chars plus "...".
  - Threads are sorted newest first.
- **Messages card**: opened by selecting a thread, or the "All Messages" button.
  - Header: "Showing Messages for Thread: <subject | Unthreaded | All Messages>". A "Threads" back button returns.
  - Table columns: Type (artifact type name with "message(s)" stripped), From, To, Date, Subject, Attachments.
  - From: TSK_EMAIL_FROM, else TSK_PHONE_NUMBER_FROM. To: TSK_EMAIL_TO, else TSK_PHONE_NUMBER_TO. Date: DATETIME_SENT, else DATETIME.
  - Sorted ascending by date. The first row is auto-selected.
  - Lower half: the full message viewer (headers, text/HTML, attachments). From/To tooltips show contact and persona.

**Call Logs tab details**
- To/From = first non-blank of: incoming FROM, PHONE, TO; outgoing TO, PHONE, FROM; otherwise TO, FROM, PHONE.
- Duration = DATETIME_END − DATETIME_START. The column is omitted if either is missing.
- Lower pane: the call-log artifact viewer.
- Empty state: "<No call logs found for selected account>".

**Contacts tab details**
- Lower pane: the contact artifact viewer.
- Empty state: "<No contacts found for selected account>".

**Media Attachments tab details**
- Attachments come from the TSK_ATTACHMENTS JSON (objectId ≥ 0) or legacy child files.
- Selecting a thumbnail looks up its TSK_ASSOCIATED_OBJECT artifact, then the parent message artifact, and shows that message in the lower viewer.

- Standard artifact and file context actions (tag, extract, etc.) are available on rows through the node actions.

### Edge cases
- No relationships in the case: device/type lists contain only DEVICE, and the date pickers keep their defaults (MIN/MAX return 0).
- `date_time` is read as a 32-bit int.
- Accounts on several data sources are separate ADIs everywhere. Messages, thread grouping and counts may double-count artifacts that create several relationships.
- Thread comparator bug: a null check on the wrong variable.
- E-mail "From" account lookup uses the To value (bug).
- The Other Occurrences "exclude current case" check compares case UUID to case *name* (bug, so it never excludes).
- `getContacts()` in the contact cache can return null (no entry) and is handled.
- A NullPointerException while painting the graph is swallowed and logged.

### Autopsy problems & postmortem ideas (Communications)
- **Query fan-out**: every table row and vertex calls `getRelationshipSourcesCount` (N+1 queries), and each tab re-queries relationship sources independently. Postmortem: one aggregated SQL per refresh (`GROUP BY account, device` with counts), with the source-artifact set computed once per selection and shared by all tabs (an `Arc` cache keyed by selection+filter hash).
- **Attribute N+1**: thread, sort and summary code calls `getAttribute` per artifact per comparison. Prefetch the needed attributes in one join into typed structs before sorting.
- **Graph engine** (JGraphX): one layout only, synchronous on the UI thread with a modal dialog. Use a petgraph-style model with an incremental force-directed layout (Fruchterman-Reingold/ForceAtlas2) stepped per frame in egui, honouring locked/pinned positions. Offer circle, hierarchical and radial layouts, plus "expand neighbours" as an explicit action. Show edge counts and direction (sender → recipient) and colour edges by relationship type.
- **History pollution**: every wheel notch is a history entry, and history restores drop locks and positions. Keep zoom/pan out of history and store vertex positions plus lock state in the state.
- **Randomness**: random 400×400 placement makes layouts irreproducible. Use seeded placement so snapshots are deterministic.
- **Fit**: uses the average of the axis ratios. Use the minimum, plus padding.
- **Selection always resets to the Summary tab**, which is annoying. Keep the active tab.
- **Date filter**: day granularity only, and the defaults are invisible because the checkboxes start unchecked. Offer a date-time range with presets and a histogram of activity (relationship counts per day) so users can brush-select.
- **MostRecent limit is global** across all accounts, which is confusing. Label it clearly, or offer per-account limits.
- **Persistence**: no persistence of pins, locks, layouts or filters. In multi-user mode, store named "graph views" in the case DB per user, so investigators can share views.
- **Search**: no way to find an account in the graph or the table. Add type-ahead search/filter over accounts (ID, contact name, persona) and a text search inside message bodies.
- **Refresh**: needs a manual click. Debounce DB-change notifications (in multi-user mode, use Postgres LISTEN/NOTIFY) and offer auto-refresh that keeps the current selection.
- **Popouts**: make the graph, the message thread view and the account summary each detachable into their own viewport. Render conversation threads chat-style (bubbles, direction, timestamps).
- **Exports**: the snapshot is PNG+HTML only. Also export SVG/PDF of the graph and CSV/JSON of the selected relationships, recording filter provenance.

## Geolocation

Opened from Tools > Geolocation or the case toolbar (enabled only with an open case and the GUI). It runs as a single floating window, "Geolocation", made of:
- the map
- a right-hand collapsible "Filters" side tab
- a bottom status bar with the mouse coordinate label, a "Loading Waypoints" progress bar and a **KML Report** button

It is also opened by the Data Source Summary "Geolocation" panel's links that open the map. These pre-set a filter for one data source and the relevant types: either show all, or "last 30 days" with no-timestamp points excluded. If the window is closed, the filter is stashed and applied after the lists load.

### Waypoint kinds (data model)
Common point fields: artifact, label, timestamp (nullable, epoch seconds), latitude, longitude, altitude (nullable), image (EXIF only), parent path (route/track/area), source content, and "other properties".

**Other properties** = every attribute of the artifact that is not JSON-typed and not already handled (NAME, GEO_LAT, GEO_LON, GEO_ALT, DATETIME, DATETIME_CREATED, LAT/LON_START, LAT/LON_END), as display name → display string.

A point **without latitude or longitude is rejected** at parse time; it is logged and counted as a parse failure. There is **no range validation**: latitude outside ±90 or longitude outside ±180 passes through and is projected as-is.

| Kind | Source artifact | Label rule | Timestamp | Map colour | Map glyph |
|---|---|---|---|---|---|
| Bookmark | TSK_GPS_BOOKMARK | TSK_NAME else "" | DATETIME | blue | pin |
| Last known location | TSK_GPS_LAST_KNOWN_LOCATION | NAME else "Last Known Location" | DATETIME | red | pin |
| Search | TSK_GPS_SEARCH | NAME, else LOCATION, else "GPS Search" | DATETIME | green | pin |
| Trackpoint (legacy single) | TSK_GPS_TRACKPOINT | NAME, else PROG_NAME, else FLAG, else "GPS Trackpoint" | DATETIME | orange | 12 px dot |
| EXIF photo | TSK_METADATA_EXIF (artifact's file = image) | image file name | DATETIME_CREATED | magenta | pin + 150 px thumbnail in popup |
| Route | TSK_GPS_ROUTE | see "Route" below | route DATETIME | cyan | 12 px dots |
| Track | TSK_GPS_TRACK, attribute TSK_GEO_TRACKPOINTS (JSON list of lat, lon, alt, time, velocity, distanceTraveled, distanceFromHome) | path name = TSK_NAME; points "Trackpoint for track: <name>" | per point; track start/end from JSON | orange | 12 px dots + black 2 px polyline |
| Area | TSK_GPS_AREA, attribute TSK_GEO_AREAPOINTS (JSON list of lat, lon) | "Area outline point for area: <name>" | none | blue-violet #8A2BE2 | 6 px dots + polygon at 20 % alpha |
| Custom | any other artifact type returned by the query | NAME else "" | DATETIME | grey | pin |

**Route details**
- Path label is "As-the-crow-flies Route".
- If a TSK_GEO_WAYPOINTS JSON list exists, every point is labelled "Waypoints for route: …". A point's own name is added as the property TSK_LOCATION.
- Otherwise the route has two points, labelled "Start" (LAT/LON_START) and "End" (LAT/LON_END), sharing the route's altitude.

**Track point properties** are velocity, "Distance traveled" and "Distance from home point", shown as raw numbers with no units in the source. Postmortem shows metres and m/s with explicit units.

### Loading query (filter → points)
1. **Filter lists**: run in the background when the window opens. For each data source and each of the 8 GPS types, count distinct artifacts that:
   - have a LAT, LON, TRACKPOINTS, WAYPOINTS or AREAPOINTS attribute, and
   - have review status ≠ REJECTED.

   Data sources with ≥ 1 artifact are listed by name. Types are listed as "<type display name> (<total count>)" with a colour-swatch icon. All entries start **checked**.

   If no data source qualifies, the filter panel is disabled and the error "There are no data sources with Geolocation artifacts found." is shown. Otherwise points load immediately.
2. **Main SQL**: selects artifact_id and artifact_type_id from blackboard_attributes where:
   - the artifact has a time attribute (DATETIME or DATETIME_CREATED), and
   - the artifact is in the set of artifacts with LAT, LAT_START or WAYPOINTS, restricted to the selected data sources.

   When showing only the last N days, it adds `value_int64 > MAX(time attrs of those artifacts) − N×86 400`. The window is relative to the **newest waypoint**, not to today.

   When "All" is selected, or "include no-timestamp" is checked, it adds a `UNION` of geo artifacts with **no** time attribute.
3. **Per row** (in Java): keep the row if its type is in the selected types, load the full artifact plus all its attributes, and parse it by kind.
4. **Tracks and areas** are fetched separately: all TSK_GPS_TRACK and TSK_GPS_AREA artifacts, filtered by data source in Java, only if their type is selected.
5. **Second time filter** (Java):
   - Window end = max(newest point timestamp, newest track start). Window start = end − N days.
   - A point is kept if it is in the window, or has no timestamp and the no-timestamp flag is on.
   - A track is kept by its start time.
   - Areas are never time-filtered.
6. **Partial parse failures** show "There was an error gathering some GPS Track Data. Some results have been excluded."
7. **Zero points** clears the map and shows "Applied filter failed to find waypoints that matched criteria. Revise filter options and try again."

### Filters panel
- **Dates** section has two options:
  - **All** (the default)
  - **Only last [N] days of activity**: a spinner, default 10, minimum 1. Selecting it enables the checkbox "Include waypoints with no time stamps" (default unchecked; disabled while All is selected).
- **Data Sources** and **Types** checkbox lists, each with select-all.
- **Apply**: an empty data source or type selection gives the message "Unable to apply filter, please select one or more data sources/artifact types."
- While loading, the panel is disabled and the progress bar is shown. Loading runs on a raw background thread.

### Map UI and behaviour
- **Initial view**:
  - Centre (0°, 0°), zoom at the second-most-zoomed-out level (whole world).
  - The view is **not** auto-fitted to the loaded waypoints.
  - The zoom slider plus +/− buttons are clamped to the tile source's minimum and maximum zoom.
  - The mouse wheel zooms around the cursor. Left-drag pans, with vertical clamping. A click re-centres (jxmapviewer listener).
  - Moving the mouse updates the coordinate label "[lat, lon]".
- **Paint order**: areas, then tracks (black 2 px polylines in point order), then waypoints.
  - Pin glyph: white pin image tinted with the type colour, anchored at bottom-centre.
  - Dots: filled circles with a 1 px black outline.
  - The selected waypoint, and *all points of its track or area*, are drawn **yellow**. The selected point is painted last, so it is on top.
  - **Routes are not drawn as lines**, only as dots.
- **Hit-testing**:
  - Convert the click to lat/lon. Kd-tree nearest neighbour (k = 1) using haversine distance (Earth radius 6 371 km).
  - Accept the hit only if the click lies inside that one point's glyph rectangle (pin w×h above the anchor, or a 12 px / 6 px dot box).
  - So a glyph overlapping another point's position can be unclickable.
- **Left-click** on a waypoint selects it and shows the **details popup**:
  - 300×200 px, docked 10 px from the map's top-right corner, re-positioned 200 ms after a resize.
  - Has a close (×) button.
  - Left-clicking empty map clears the selection and the popup.
- **Popup content** (HTML key: value lines):
  - Name, Timestamp (`yyyy-MM-dd'T'HH:mm:ssX`, JVM default time zone), Latitude, Longitude, Altitude if present, then the non-empty other properties.
  - EXIF points get a 150 px thumbnail.
  - Right-clicking inside the popup opens the same context menu.
- **Right-click** on a waypoint selects it (and refreshes an open popup), then shows the context menu:
  - "View in Timeline" (if the artifact has a supported timestamp).
  - The standard file/content actions for the **source content**: view in directory, "Open in ExternalViewer", "Extract File(s)", **Add File Tag**, **Add Result Tag** (artifact), etc. CSV export is removed.
  - "Remove File Tag" (if the source is a file) and "Remove Result Tag".
  - There is no tag indicator or tag filter on the map.
- **Refresh banner**: a DATA_ADDED event for any of the 8 GPS types shows "The geolocation data has been updated, the visualization may be out of date." with **Refresh View** and close buttons. Refresh View re-runs the filter-list load, clears the map, then reloads points with the current selection.
- **Case change**: the map is cleared and reloaded for the new case.

### Tile sources (Options > Geolocation, "Map Tile Data Source")

| Option (stored int) | Behaviour |
|---|---|
| 0 Default online tile server | Bing/VirtualEarth road map (the default) |
| 1 OpenStreetMap server | User URL (http/https). The **Test** button validates the URL and fetches tile (1,1) at zoom 1. An empty URL means the map fails to initialise |
| 2 OpenStreetMap zip file | Local zip of `z/x/y.png` tiles, read via `jar:file:/<path>!`. The **Browse** button filters *.zip. The file must exist and be readable |
| 3 MBTiles file | SQLite file. Valid only if `metadata.format = 'jpg'`. Tiles are read with `SELECT tile_data FROM images WHERE tile_id='z/x/y'`, where z = totalZoom − viewerZoom (a non-standard schema, not the MBTiles `tiles` table with TMS row flip). Loaded by 4 low-priority threads with a High/Low priority queue, 3 retries and an in-memory tile cache |

- Settings are stored in user preferences: tile option, OSM URL, zip path, MBTiles path.
- *Any* user-preference change rebuilds the tile factory.
- A failure to initialise shows "Failed to connect to map tile source. Please review map source in Options dialog." and no points are loaded.

### KML export
Two entry points:
- the map's **KML Report** button, which exports *visible* points only (screen position inside the current viewport)
- the general "Google Earth KML" report module, which exports everything, filtered by the data sources selected in the report wizard

**Output location and registration**
- From the map: `<case reports>/<case display name> Google Earth KML <MM-dd-yyyy-HH-mm-ss>/`. A progress dialog is shown.
- Files: `ReportKML.kml`, `style.kml` (styles red/green/blue/purple/white/yellowFeature), and copies of EXIF images.
- The report is registered with the case as "KML Report" (prefixed "Incomplete " on error), source module "Geospatial Data".
- With no visible waypoints: "Unable to generate KML report due to a lack of waypoints…".

**Document and folder structure**
- KML 2.2 with gx and atom namespaces. Document name "<report branding title> KML". If ingest is running, a `<snippet>` ingest warning is added.
- Folders, in order (each with an icon href to a GitHub raw PNG):
  1. EXIF Metadata (red)
  2. GPS Bookmarks (blue)
  3. GPS Last Known Location (purple)
  4. GPS Routes (green)
  5. GPS Searches (white)
  6. GPS Trackpoints (white)
  7. GPS Tracks (sub-folder per track, green)
  8. GPS Areas (sub-folder per area, green)

**Placemark contents**
- name = label, else ISO timestamp, else "".
- `styleUrl`.
- HTML description: an `<h3>` kind header plus the popup fields.
- `TimeStamp/when`.
- Geometry.
- `<snippet>` "lat, lon" to 2 decimals (about 1.1 km).
- `Point` coordinates are `lon,lat,alt`, with alt = 0 when unknown. `altitudeMode` is `clampToGround` when alt ≠ 0.

**Per-kind geometry**
- **Routes**: a `LineString` (extrude, tessellate) of the **first two points only**, plus start and end placemarks. The description has Start/End lat/lon/alt.
- **Tracks**: one Point placemark per trackpoint. There is no LineString.
- **Areas**: one `Polygon` with a closed outer ring.
- **EXIF**: the image is copied by file name, so collisions overwrite. The description contains `<img src='<name>' width='400'/>` and the "Source Path" (unique path minus the leading image/volume parts).
- A map export of a partly visible track, route or area exports the whole path.

### Edge cases
- **Dedup by coordinates**: map waypoint equality and hashing use (lat, lon) only, and points are kept in sets. **Distinct waypoints at identical coordinates collapse into one** (only the first survives). This also happens inside tracks and areas: a revisited coordinate is dropped and the line skips it.
- A missing lat/lon rejects the point, and the parse result is flagged partially failed.
- Malformed track/area JSON skips that track or area, with a warning.
- **NullPointerException risks**:
  - "Last N days" with no-timestamp points can hit an NPE in the newest-timestamp calculation, which unboxes a null.
  - Hit-test returns null when the nearest-neighbour search is empty.
- **Inconsistent counts**: REJECTED artifacts are excluded from the type counts but not from the map query.
- **Two-pass time filter** (SQL relative to the newest point, Java relative to the newest point including tracks) can disagree.
- **Areas ignore the date filter**.
- KML: if an EXIF point lacks a geometry, the whole remaining EXIF loop aborts (`return`).

### Autopsy problems & postmortem ideas (Geolocation)
- **N+1 loading**: one artifact fetch plus all its attributes per row, and tracks/areas fetched for the whole case then filtered by data source in Java. Postmortem: one SQL join returning (artifact_id, type, lat, lon, alt, time, name, data source) for all point kinds, plus a separate query for JSON paths. Parse JSON off-thread with rayon and stream results to the UI in batches.
- **No auto-fit or clustering**: tens of thousands of pins overdraw and are hard to click. Fit to the bounds of the results on load. Add zoom-dependent clustering (grid or supercluster style) with count badges and spiderfy on click. Use an R-tree (`rstar`) in Web-Mercator pixel space for hit-testing all glyphs under the cursor, then show a chooser when several overlap.
- **Coordinate dedup bug**: key waypoints by (artifact_id, path index), never by coordinates.
- **Validation**: reject or flag lat outside ±90 and lon outside ±180, NaN and (0, 0) "null island". Show an "invalid coordinates" count in the status bar with a list.
- **Time filtering**: replace "last N days relative to newest point" with an explicit date-time range slider plus a histogram/timeline brush. Optionally animate tracks over time. Make the area/no-timestamp handling explicit in the UI.
- **Rendering gaps**: draw routes as lines and add direction arrows on tracks. Show a distance/scale bar in metric (m/km), measure distance and area (haversine, km and km²), and show speed in km/h. Track properties should carry units.
- **Tile sources**:
  - Support standard MBTiles (`tiles` table, TMS row flip, png/jpg/webp), XYZ URL templates with `{z}/{x}/{y}` and optional API key, and offline directory/zip tiles.
  - Add a disk tile cache under the case or user directory.
  - Default to an offline-safe setting (forensic labs are often air-gapped). Never silently fetch online tiles without an indication.
  - Keep tile I/O on async workers (e.g. `walkers` crate or a custom egui painter).
- **Tagging**: a tag/bookmark indicator on markers, filter by tag, and tagging multiple points with a lasso/box selection. Sync tags through the case DB so multi-user peers see them; refresh on tag events.
- **Details**: a side panel or popout window with a sortable table of visible waypoints linked to the map (select in either), copyable coordinates in decimal and DMS, and "open source file / artifact" navigation into the main tree.
- **Refresh**: debounce DB change notifications, and refresh incrementally while keeping the viewport and selection.
- **KML/KMZ export**:
  - Bundle images into a KMZ.
  - Emit LineStrings for tracks and full multi-point routes; drop external GitHub icon URLs.
  - Write UTC ISO-8601 timestamps and full-precision coordinates.
  - Also offer GeoJSON/GPX/CSV.
  - Export the current *filter result* or *selection*, not only "what is inside the viewport".
- **Multi-viewport**: allow several map popouts with independent filters (e.g. per device) and synchronised time cursors.

## Discovery (File & Domain)

Source: `Core/.../discovery/search` (engine) and `discovery/ui` (dialog + results window). Entry: **Tools > Discovery** and a "Discovery" button on the case toolbar. Both are enabled only while a case is open.

### Entry flow and pre-checks
- Opening the dialog first cancels any running search. It then scans `ingest_jobs`/module info per data source and, if any data source lacks **Hash Lookup**, **File Type Identification** or **Picture Analyzer**, shows a 600x100 px scroll message titled "Discovery results may be incomplete" that lists `"<module> module was not run on data source: <name>"`.
- The dialog is a modal singleton. Its filter panels are rebuilt lazily (on next open) when CURRENT_CASE, DATA_SOURCE_ADDED or DATA_SOURCE_DELETED fires, or when ingest DATA_ADDED brings a **new** set name for TSK_OBJECT_DETECTED (TSK_DESCRIPTION), TSK_HASHSET_HIT (TSK_SET_NAME) or TSK_INTERESTING_ITEM/FILE_HIT/ARTIFACT_HIT (TSK_SET_NAME).

### Wizard dialog layout ("Discovery")
- **Step 1: Choose result type**: a toolbar of 4 equal-width toggle buttons with icons: Images | Videos | Documents | Domains. Selected background is RGB(216,230,242), unselected RGB(240,240,240). Images is selected initially.
- **Step 2: Filter which <type> to show**: a 2-column split pane (resize weight 0.5) of filters. Each filter is a checkbox, optionally followed by a check-list or editor that is enabled only while the checkbox is ticked.
- **Step 3: Choose display settings**: combos "Group By:", "Order Groups By:" and "Order Within Groups By:", a red error label and a **Search** button. Search is disabled while any filter reports an error, and the first error is shown.
- Each type panel remembers its own last group-by, sort and group-sort choices. Defaults for files are group by Parent Folder, order by File Name, groups by Group Size. Defaults for domains are group by Final Activity Date, order by Domain Name, groups by Group Size.
- Search hides the dialog, opens/resets the results window, posts SearchStarted and runs the search in the background.

### Result types
| Type | Definition |
|---|---|
| Image | `mime_type IN` the IMAGE category from FileTypeUtils |
| Video | `mime_type IN` the VIDEO category |
| Document | its own list: text/html, text/csv, application/rtf, application/pdf, application/xhtml+xml, application/x-msoffice, application/msword, application/msword2, application/vnd.wordperfect, docx, ppt, pptx, xls, application/vnd.ms-excel.sheet.4, xlsx, odp, ods, odt |
| Domain | built from artifacts TSK_WEB_BOOKMARK, CACHE, COOKIE, DOWNLOAD, HISTORY, SEARCH_QUERY (plus TSK_WEB_ACCOUNT_TYPE internally) |

A FileTypeFilter for the chosen type is always added first (except for Domain). Types also carry a ranking for sorting: Image 0, Audio 1, Video 2, Executable 3, Document 4, Other 5, Domain 6.

### Filters (per type, column, default)
| Filter (UI label) | Img | Vid | Doc | Dom | Default | Implementation |
|---|---|---|---|---|---|---|
| File Size: | c0 | c0 | c0 | – | ON, Large+XLarge+XXLarge | SQL OR of buckets: `(size > min AND size <= max)`, or `size >= min` for the open-ended bucket |
| Data Source: | c0 | c0 | c0 | c0 | off | `data_source_obj_id IN (...)`. List shows "name (ID: n)", sorted by name case-insensitively |
| Past Occurrences: | c0 | c0 | c0 | c0 | ON (files) with Rare+Unique+Common if CR is enabled, otherwise Unknown. Domains: off, and disabled without CR | Post-filter on the computed Frequency (see CR) |
| Possibly User Created | c0 | c0 | – | – | off | `obj_id IN` artifacts of type TSK_USER_CONTENT_SUSPECTED (desc "that contain EXIF data") |
| Hash Set: | c1 | c1 | c1 | – | off. Disabled if no TSK_HASHSET_HIT set names exist | **BUG:** builds an InterestingFileSetFilter, so it actually matches *interesting-item* artifacts whose set name equals the hash set name. Intended: `obj_id IN TSK_HASHSET_HIT artifacts with TSK_SET_NAME IN (...)` |
| Interesting Item: | c1 | c1 | c1 | – | off. Disabled if no names | `obj_id IN` artifacts of type TSK_INTERESTING_FILE_HIT or TSK_INTERESTING_ITEM with TSK_SET_NAME value_text = any selected. List from TSK_INTERESTING_ITEM set names only |
| Object Detected: | c1 | c1 | – | – | off. Disabled if no names | TSK_OBJECT_DETECTED artifacts with TSK_DESCRIPTION = any selected |
| Parent Folder: | c1 | c1 | c1 | – | off. List is pre-seeded with `/Windows/` and `/Program Files/` as substring+exclude | Terms are typed into a text field with radios Full/Substring and Include/Exclude, then Add/Delete. SQL: Full uses `parent_path='x'`/`!=`, Substring uses `LIKE '%x%'`/`NOT LIKE`. Includes are joined with OR, excludes with AND, and the two groups with AND. Label "(All will be used)" |
| Include only previously notable domains | – | – | – | c1 | off. Disabled without CR | Post-filter: CR domain instance rows with `known_status = BAD(2)` |
| Include only domains with a known account type | – | – | – | c1 | off | Post-filter: the domain's count of TSK_WEB_ACCOUNT_TYPE artifacts must be > 0 |
| Result Type: | – | – | – | c1 | off | Check-list of the 6 web artifact types. It replaces the default artifact-type set in the domain query |
| Date Filter: | – | – | – | c1 | off | "Only final: [7] days of activity" (spinner 1..100000, the default radio) or "Date Range (<tz>):" with optional Start:/End: date pickers. See below |

- Validation messages, all checked live:
  - "At least one size / data source / value in the past occurrence filter / hash set name / interesting file set name / object type name / Result type must be selected."
  - "At least one parent path must be entered."
  - Date filter: "Range or Only Last must be selected." / "A start or end date must be specified to use the range filter." / "Start date should be before the end date when both are enabled."
- **Date filter semantics**:
  - The clause is `attribute_type_id IN (TSK_DATETIME, TSK_DATETIME_CREATED, TSK_DATETIME_ACCESSED) AND value_int64 BETWEEN start AND end`.
  - Range mode: start = 00:00 of the start date in the user zone (or LocalDate.MIN). End = 00:00 of **end+1 day** (inclusive end), or LocalDate.MAX.
  - "Only final N days" mode: start = (timeline max event time / 86400 as an epoch day) − N days. If that lookup fails, it falls back to today − N.
- **Backend-only filters, not exposed in the UI** (worth exposing in postmortem):
  - KeywordListFilter: TSK_KEYWORD_HIT (type 9) whose TSK_SET_NAME (attr 37) is in the lists.
  - TagsFilter: `obj_id IN content_tags WHERE tag_name_id IN`.
  - ScoreFilter: Notable = `known=BAD` OR notable-status tag. Interesting = has TSK_INTERESTING_ITEM/FILE_HIT OR a non-notable tag. Both selected = any tag.
  - KnownFilter: `known != KNOWN` ("which are not known").
  - NotableFilter (CR): the file MD5 has `getCountArtifactInstancesKnownBad > 0`.
- **Execution, files**:
  1. All non-empty `getWhereClause()` results are wrapped in parentheses and ANDed into a single `tsk_files` query.
  2. Each row becomes a ResultFile.
  3. The "alternate" (non-SQL) filters run in list order and stop early once the list is empty.
  4. Set names in SQL are string-concatenated, not parameterised.

### Domain query (DomainSearchCacheLoader)
- **Inner query**, on `blackboard_attributes`:
  - Grouped by `artifact_id`. Selects `LOWER(MAX(value_text)) AS domain`, `MAX(value_int64) AS date`, `MAX(artifact_type_id)`.
  - WHERE (OR of the SQL filters, plus the date-attr clause when no date filter is present, plus `TSK_DOMAIN AND value_text<>''`) AND `artifact_type_id IN (<selected types> + TSK_WEB_ACCOUNT_TYPE)`.
  - HAVING one `SUM(CASE WHEN <clause> THEN 1 END) > 0` per clause, so each artifact must have a non-empty TSK_DOMAIN **and** a date attribute (in range when filtered).
  - Consequence: web artifacts that have no date attribute never produce domains.
- **Accounts subquery**: per TSK_WEB_ACCOUNT_TYPE artifact, `GROUP_CONCAT(DISTINCT value_text)` (SQLite) or `STRING_AGG(DISTINCT value_text, ',')` (PostgreSQL) over its TSK_TEXT values.
- **Outer query**: `blackboard_artifacts` JOIN inner LEFT JOIN accounts, with the data source filter (if any) as WHERE, `GROUP BY domain` (or `data_source_obj_id, domain` when grouping by data source). It computes:
  - activity_start = MIN(date), activity_end = MAX(date).
  - fileDownloads = count of WEB_DOWNLOAD artifacts. totalPageViews = count of WEB_HISTORY artifacts.
  - pageViewsInLast60 = WEB_HISTORY artifacts with date in [maxEventTime − 60 days, maxEventTime]. The window is relative to the **timeline's latest event**, not to now.
  - countOfKnownAccountTypes = count of WEB_ACCOUNT_TYPE artifacts. dataSource = MAX(data_source_obj_id). accountTypes = aggregated string.
- Rows are fetched with fetch size 500. Domains `localhost` and `127.0.0.1` are dropped.
- The needed attributes (grouping attribute + sort's required attributes) are added first; the alternate filters run after that.

### Central-repo dependency and Frequency
- **Frequency buckets** (`fromCount`):

  | Bucket | Occurrences (distinct cases) |
  |---|---|
  | Unique | ≤ 1 |
  | Rare | 2–10 |
  | Common | 11–100 (label says "Common (11 - 100)") |
  | Very Common | > 100 (label says "100+") |
  | Known | label "Known (NSRL)" |
  | Unknown | ranks 0..5 |

- Filter options: with CR, Unique, Rare, Common, Very Common, Known (domains drop Known). Without CR, Known and Unknown (domains get none).
- **Files with CR**:
  - Known status takes precedence: `known==KNOWN` gives Known.
  - MD5s are batched (50 per query): `SELECT value, COUNT(value) FROM (SELECT DISTINCT case_id, value FROM file_instances WHERE value IN (...)) GROUP BY value`.
  - MD5s found get fromCount(count). MD5s **not found become Unique**. Files without an MD5 stay Unknown.
- **Files without CR**: Known if `known==KNOWN`, otherwise Unknown.
- **Domains**:
  - Values are normalized with the CR normalizer (failures are skipped) and batched 500 at a time.
  - The same DISTINCT case_id count runs against the domain table. Domains not found **stay Unknown**, unlike files.
  - Previously notable: `SELECT value FROM domain_instances WHERE value IN (...) AND known_status=2`, in batches of 500.
- The frequency of a file counts the current case too, so "Unique" means seen only in this case.
- Group-by Frequency/Previous Notability and sort by Frequency are offered only when CR is enabled. The CR instance is obtained once per search; if that fails, all CR options degrade silently.

### Grouping attributes (Group By)
- **Options shown for files**: File Size, Past Occurrences (CR only), Parent Folder, Object Detected / Hash Set / Interesting Item (each only if the *Images* panel has any such set names).
- **Options shown for domains**: Page Views, Past Occurrences (CR), Final Activity Date, First Activity Date, Previous Notability (CR), Domain Category.
- Enum values that exist but are not offered: Keyword, Data Source, Tag, None ("All Files"), and a File Type key.

| Attribute | Key computation and display | Key order (BY_GROUP_NAME) |
|---|---|---|
| File Size | video → video buckets, else image buckets. Lower bounds are exclusive (`size > min`). Image buckets: XXLarge >200 MB, XLarge 50–200 MB, Large 1–50 MB, Medium 100 kB–1 MB, Small 16–100 kB, XSmall 0–16 kB. Video buckets: XXLarge >10 GB, XLarge 5–10 GB, Large 1–5 GB, Medium 100 MB–1 GB, Small 500 kB–100 MB, XSmall 0–500 kB. Decimal units (1 MB = 1 000 000 bytes). Shown as "Large: 1-50MB" | rank ascending (largest first) |
| Past Occurrences | frequency enum | Unique→Unknown |
| Parent Folder | walk up from the file while the parent is itself a file (so embedded files go to the real directory). Key = (parent uniquePath, parent obj id). Fallback parent_path with id −1 | path string, then id |
| Hash Set / Interesting Item / Object Detected / Keyword / Tag | sorted, de-duplicated names joined with "," (a file in two sets forms a combined group "A,B"). Empty → "None" | "None" last, otherwise string compare |
| Data Source | "name(ID: n)" | obj id |
| Final / First Activity Date | activity_end/start in the display time zone, moved forward to the **next-or-same Sunday**. Shown as "Week of Mon D, YYYY" | newest week first |
| Page Views | buckets of totalPageViews (null → 0): 0–10, 11–50, 51–100, 101–500, 501–1000, "1000+ page views" | enum order (1000+ first) |
| Previous Notability | "Previously Notable" / "Previously Not Notable" | notable first |
| Domain Category | from TSK_WEB_CATEGORIZATION artifacts (TSK_NAME = category, TSK_DOMAIN trimmed and lowercased). A domain is added to **one group per category** (it is duplicated across groups). No category → "Uncategorized" | weak comparator (size check) |

- Category key attachment is computed by scanning *all* TSK_WEB_CATEGORIZATION artifacts. Hash set, interesting item, object detected and keyword names are fetched in one join query over the obj id list: `blackboard_artifacts JOIN blackboard_attributes WHERE artifact_type_id=? AND attribute_type_id=? AND obj_id IN (...)`. Tags are loaded per file (N queries).
- **Group sort**:
  - "Group Name" uses the key order above.
  - "Group Size" sorts by size descending, with key order as tie-break.

### Within-group ordering (Order Within Groups By)
- **Files**: File Size (descending), Full Path (lowercase parent uniquePath), File Name (lowercase), Data Source (obj id ascending).
- **Domains**: Page Views (descending), File Downloads (descending), Last Activity Date (descending), Domain Name (lowercase).
- Defined but not offered: File Type (type rank, then MIME), Central Repo Frequency (rarest first), Keyword List Names.
- Every sort has an appended default tie-break: files by name then obj id; domains by frequency rank.
- Domain sorts require the Category and PreviouslyNotable attributes to be loaded.

### De-duplication, result objects, ranking
- Inside a group, files with the same non-blank MD5 (excluding the empty-file MD5) merge into one result that keeps an instance list.
  - The display shows the first instance plus "and N more".
  - `deleted` is true only if **every** instance has UNALLOC set.
  - The score shown is the aggregate score of the **last** merged instance. This is a bug: it should be the maximum.
- Score icon: significance NOTABLE gives a red circle; LIKELY_NOTABLE gives a yellow circle. Tooltip "Has an <significance> analysis result score".
- There is no relevance ranking beyond the sorts above.

### Caching and paging
- **File searches**:
  - Static LRU of 10 searches. Key = OS `user.name` + each filter's `toString()` + group attribute + group sort + sort. The value is an ordered map from group key to results.
  - AbstractFilter has **no toString**, so keys are effectively per filter-instance: a re-run with identical criteria misses the cache, while paging within one search hits it.
  - When a group is missing from the cache, the search re-runs, then looks the cache up by `keyString` (a String) instead of the key object. That lookup always misses, so an empty page is returned. This is a bug.
- **Domain searches**: a LoadingCache (10 entries) with the same key plus identity of the case DB and CR objects.
- **Domain artifacts cache**: one entry per artifact type (max 10, TTL 5 min, invalidated on every SearchStarted). It loads **all** artifacts of that type, then maps lowercased, trimmed TSK_DOMAIN to its artifacts.
- **Domain thumbnail cache**: 500 entries, keyed by (case, domain, icon size).
- **Paging**: page size combo 25/50/75/100/125/150/175/200 (default **100**).
  - Shows "Page: X of Y", Prev/Next and "Go to Page:". An invalid page gives the dialog "The selected page number N does not exist. Please select a value from 1 to M."
  - Changing the page size jumps to page 1. Paging controls are disabled while a page loads.
  - The page worker slices the cached group list `[start, start+size)`.

### Results window (floating TopComponent "Discovery")
- **Header**:
  - A progress text area. During a search it reads (red) "Performing search for results of type IMAGE. Please wait." and the button reads "Cancel Search".
  - When done: "Results with <filter descs joined '; '>" (domains get the prefix "Type: Domain; "). Button: "New Search".
  - Cancelled: "Search has been cancelled." (red).
- **Left pane, "Groups" list**:
  - Each entry is "<key> (<count>)". Parent-path labels are middle-ellipsized to fit; the full text is in the tooltip.
  - The first group is auto-selected.
  - With 0 groups, a modal "No results found" appears with module reminders. Files: File Type ID required, Hash Lookup for past occurrence, Picture Analyzer for user-created. Domains: Recent Activity, Central Repository module, iOS Analyzer (iLEAPP).
- **Right top**: the results viewer for the type, inside a split pane.
- **Right bottom**: the details area. It slides open/closed in animated 30 px steps when a domain is (de)selected. The divider position is remembered (default 250 px).
- **Image viewer**:
  - Horizontal-wrap grid of 200 px thumbnails (the generic ImageUtils thumbnail).
  - Each tile shows a name label (parent_path + name, truncated from the left to 30 chars), size label, deleted icon and score icon.
  - Size labels use 1000-based units (bytes/KB/MB/GB/TB, integer division).
- **Video viewer**:
  - One row per video: name (120-char limit) above **4 frames** with H:MM:SS captions, plus size/deleted/score.
  - Frames come from the file copied to a temp dir and opened with OpenCV. Duration = 1000·frames/fps ms. Frames are taken at 1 %, 25 %, 50 % and 75 %.
  - Each frame is converted BGR→RGB and resized fit-to-height into 200x100.
  - Cached as PNG at `<case cache>/video-thumbnails/<md5>/<i>-<ms>.png`, reloaded if that directory exists (the reload does not sort by filename).
  - A failure on any frame uses "failedToCreateVideoThumb". Cancelling deletes the partial cache directory.
- **Document viewer**: one row per document with the name (90-char limit), a summary text pane, and "1 of N images" plus a sample image.
  - PDF and XHTML show "0 of ? images" with a "not supported" icon; other documents with no images show "No images".
- **Document summary algorithm**:
  1. If a pluggable TextSummarizer service exists (none in the open-source tree), call `summarize(file, 40)`.
  2. Otherwise use the default: look for `<case cache>/summaries/<md5>-default-256-translated.txt`. If it is absent, extract the **first 256 characters** with the file's text extractor (falling back to the strings extractor).
  3. If a machine-translation provider is configured, translate those characters and cache the result in that file; otherwise use them raw.
  4. Sample image: among the file's **child** objects (embedded files) that support thumbnails, count them and take the **largest by size**. Its 200 px thumbnail is scaled to 100x100.
  - Errors produce text such as "No bytes read for document, unable to display preview." or "Error trying to extract text from file."
- **Domain viewer**: one card per domain showing:
  - the domain name; "Activity: MMM dd YYYY to MMM dd YYYY" in the display time zone;
  - "Total page views:", "Page views in final 60 days:", "Files downloaded:";
  - "Previously tagged as notable: Yes/No", "Account type: <list or Unknown>", "Categories: a,b";
  - a 100 px thumbnail, with "Loading thumbnail..." meanwhile.
- **Domain thumbnail algorithm**:
  1. Candidates are files of the domain's TSK_WEB_DOWNLOAD artifacts (the artifact's source obj).
  2. Keep only extensions jpg/svg/png/webp/ico/gif or MIME types gif/jpeg/png/webp/svg+xml/vnd.microsoft.icon/x-icon.
  3. Sort non-ICO files by crtime, with ICO files last (ICOs sorted by size). Iterate from the end, i.e. ICO files are tried first, then the newest image; the first one that renders wins.
  4. Otherwise use the TSK_WEB_CACHE artifacts' TSK_PATH_ID files, sorted by size with ICO last; iterate from the end.
  5. Otherwise use the "image-extraction-not-supported" icon.
- Every thumbnail, summary and frame job is cancellable and is cancelled on page change or window close.

### Details area
- **Files (FileDetailsPanel)**:
  - Top: "Instances" list, i.e. all duplicate instances of the selected result.
  - Bottom: the standard content-viewer tabs for the selected instance.
  - Instance right-click menu: View File in Directory, Open in External Viewer, View File in Timeline, Extract File, Add tag, Remove tag, Add to hash set.
- **Domains (DomainDetailsPanel)**: tabs are Timeline, then one tab per web type (display names of Bookmarks, Cache, Cookies, Downloads, History, Web Search), then **Other Occurrences** (only with CR; the CR other-occurrences table for the Domain type).
  - Tabs load lazily (UNPOPULATED → POPULATING (loading panel) → POPULATED). Switching domains resets every tab.
  - The selected tab persists across domains and across searches. If a new search has a Result Type filter, the tab for that type is selected.
- **Web-type tab**: a split pane (divider remembered, default 300 px).
  - Left: a table sorted by Date/Time **descending**.
  - Column 2 is Title by default, Name for Cache/Downloads, URL for Cookies, Term for Web Search. Cache adds a third column, MIME Type.
  - Values come from TSK_DATETIME_ACCESSED, the TSK_PATH_ID file name or TSK_PATH basename, or TSK_TITLE. Fallbacks: any TSK_DATETIME*, then URL/NAME/TEXT. Missing values show "No value available."
  - Right: an artifact viewer. History, Cookie, Search and Bookmark use the generic artifact viewer; Download and Cache use the file content viewer.
  - Row menu:
    - "View in Timeline" if the artifact has a supported timestamp.
    - If the artifact has a file (TSK_PATH_ID, or the artifact's own object for non-download types): the standard file actions (external viewer, Extract, add file tag, etc.) plus remove file tag.
    - Otherwise: a disabled red "File does not exist." plus add artifact tag.
    - Always: remove artifact tag.
- **Timeline tab** (mini timeline):
  - All 6 types of artifacts for the domain are bucketed by `yyyy-mm-dd`, taken from the first TSK_DATETIME* attribute formatted in the display time zone. Artifacts without a date are dropped.
  - Left: table Date | Count (descending).
  - Middle: Result Type | Description. Description = the first non-blank of TITLE, NAME, PROG_NAME, TEXT, URL.
  - Right: the artifact detail viewer.

### Edge cases worth reproducing or fixing
- Documents reuse the image size buckets.
- An empty or `NULL` MD5 never merges duplicates.
- Hash Set, Object Detected and Interesting Item group-by options are hidden unless at least one name exists in the **image** panel, regardless of the type in use.
- Frequency filtering without CR offers only Known/Unknown. "Unknown" then means every file that is not NSRL-known.
- Domain values in the result are taken via `LOWER(MAX(value_text))`, i.e. raw TSK_DOMAIN strings, not normalized. In contrast, CR lookups normalize the value and the artifact caches use trim+lowercase.
- Multi-user: the search cache key includes the OS user name. All state is per client; there is no shared state.

### Autopsy problems & postmortem ideas (Discovery)
- **Performance**:
  - The whole result set is materialized: all files are turned into objects, then filtered in Java.
  - Tags take N+1 queries. Set-name lists and web categories come from full artifact scans. Domain artifact caches load *every* artifact of a type.
  - Postmortem should push everything into SQL (CTEs, `GROUP BY` producing group counts directly) and fetch pages with `LIMIT/OFFSET` or keyset pagination per group. Use indexed side tables (e.g. a `file_set_membership` view or materialized table).
- CR frequency lookups run one batch of 50 per round trip. Use a single `IN (temp table)` join, or attach a CR frequency cache table refreshed after ingest.
- The search cache key is broken (identity `toString`) and the cache-refill bug returns empty pages. Postmortem should hash a canonical serialized filter spec (serde + blake3), keyed by case and user, and stream results.
- SQL is built by string concatenation (path terms and set names are injectable and break on quotes). Use parameterized queries everywhere.
- Hash Set filter bug (it uses the interesting-item query). The duplicate score takes the last instance instead of the maximum. Domain cards silently miss dateless artifacts. Fix all three and add tests.
- Domain summary images loop synchronously over files. Use a bounded thread pool and a GPU texture cache in egui. Persist thumbnails in a content-addressed `thumbs/<md5>-<size>.webp` store shared by Discovery, Gallery and the thumbnail viewer.
- Video frame extraction copies the whole file to temp. Decode via ffmpeg/gstreamer bindings with a seekable reader over the image (no temp copy), and cache frames by MD5 plus position.
- Document "summary" is just the first 256 characters. Optionally use better extraction: first meaningful paragraph, or highlighted keyword-hit snippets.
- UX:
  - Make the three steps a non-modal side panel with live result counts per filter value (facets).
  - Allow multiple saved searches open as tabs or popout viewports.
  - Expose the hidden filters (keyword list, tags, score, known, CR notable) and the hidden group-bys (data source, tag, keyword, none).
  - Make the frequency thresholds configurable.
  - Unify size units display (show both decimal and binary).
- Multi-user: store saved searches in the case DB with an owner. Use DB-side notifications (PostgreSQL LISTEN/NOTIFY) to mark results stale when ingest adds data, instead of the ingest-event `shouldUpdate` flag.

## Common Properties Search

Entry: **Tools > Find Common Properties**. The action is enabled when a case is open.
- **Pre-check**: if the current case has more than 1 data source, the dialog opens. Otherwise all of these must hold, and the failing ones are reported in a message ("Address one of these issues…"):
  - CR enabled and valid;
  - at least 2 CR cases;
  - the current case is present in CR.

### Dialog ("Find Common Properties", modal)
- Text "Find items that exist in multiple data sources or cases". **Scope of Search:** radios:
  - "Between data sources in current case" (intra). Disabled if the case has only 1 data source.
  - "Between current case and cases in the Central Repository" (inter). Disabled unless CR is enabled, has more than 1 case, and contains the current case.
  - Default is inter when it is available.
- **Current Case Options (intra)**:
  - Checkbox "Common items need to exist in a specific data source:" with a data source combo.
  - "File Types to Show:" radio "All file types" / "Only the selected file types:" with checkboxes "Pictures and Videos" and "Documents".
- **Central Repository Options (inter)**:
  - Checkbox "Common items need to exist in a specific case:" with a combo of all CR cases except the current one.
  - "Property Type to Match:" combo of all defined CR correlation types sorted by id (Files first), **excluding OS-account tables**.
  - The same File Types controls, enabled only when the type is "Files".
- "Display results organized by:" radios **Case** / **Number of data sources** (default). Visible only for inter; intra is always by count.
- Threshold: "Hide items found in over [20] % of data sources in Central Repository." plus the label "The current Central Repository contains N data source(s)." It is enabled and checked by default only if the CR has more than 0 data sources.
- **Validation**:
  - The percentage must parse and satisfy 0 < % ≤ 100. The message reads "0 < % < 100".
  - "Only selected" mode requires at least one category ("No file categories are included in the search.").
  - The Search button stays disabled on errors.
- **There is no "include only notable" option.** "Hide known" is implicit and always on: known = NSRL-known files and CR `known_status` KNOWN(1) are excluded from the seed set.
- **File categories** (they are documents, not executables):
  - Media = image/bmp, gif, jpeg, png, tiff, vnd.adobe.photoshop, x-raw-nikon, x-ms-bmp, x-icon; video/webm, 3gpp, 3gpp2, ogg, mpeg, mp4, quicktime, x-msvideo, x-flv, x-m4v, x-ms-wmv; application/vnd.ms-asf, vnd.rn-realmedia, x-shockwave-flash.
  - Documents = rtf, pdf, text/css, html, csv, json, javascript, xml, text/calendar, x-msoffice, x-ooxml, msword, docx, ppt, pptx, xls, xlsx, odp, ods, odt.
  - Both selected = union.
- **Inter pre-search warning**: for every data source that has an ingest job, check that its device_id exists in CR `data_sources` **and** that the Central Repository ingest module ran in one of its jobs. Otherwise show "Results may be incomplete… The following data sources have not been processed: - X … Continue with search anyway?" (Yes/No).

### Algorithms
- **Intra, all data sources**:
  ```sql
  SELECT obj_id, md5, data_source_obj_id FROM tsk_files WHERE md5 IN (
    SELECT md5 FROM tsk_files WHERE (known != 1 OR known IS NULL) [AND mime_type IN (...)]
    GROUP BY md5 HAVING COUNT(DISTINCT data_source_obj_id) > 1) ORDER BY md5
  ```
- **Intra, single data source**: the same, but the inner set = md5s present in the chosen data source (known/MIME filtered) that still occur in more than 1 data source.
- Rows whose MD5 is null or the empty-file MD5 are skipped. Rows are grouped by MD5 into a value holding its instances, then bucketed by the **number of distinct data sources**. That number is computed by loading each instance's AbstractFile, which is slow.
- Intra "by case" is not supported and throws.
- **Inter, seed values**:
  - Files type: the current case DB via `SELECT md5 AS value FROM tsk_files WHERE known!=1 AND md5 IS NOT NULL [AND mime_type IS NOT NULL AND mime_type IN (...)]`. SQL `!=` drops rows where known is NULL.
  - Other types: CR `<type>_instances WHERE case_id=<cur> AND (known_status != 1 OR NULL)`.
  - Values are de-duplicated; null and empty-MD5 values are skipped.
- **Inter, by count**:
  - For **each value, one CR query**: all instances (all cases), or only instances in [current, target] for a specific case.
  - size = number of distinct CR data source ids.
  - Keep the value if size > 1 **and** at least one instance belongs to another case. Bucket by size.
- **Inter, by case**:
  1. Fetch instances in batches of 500 values. A batch is kept if it has more than 1 instance in total; this is a batch-level check, which is a bug.
  2. Build case name → "<ds name> (Id: <ds obj id>)" → list of single-instance values.
  3. Post-filter: the values to keep are the current case's values that pass the threshold.
  4. The output tree **omits the current case**. Each other case/data source lists the *current case's* matching value objects, so the leaf column is "Parent Path in Current Case".
- **Percentage threshold**:
  - Skipped if the threshold is 0 or CR is disabled. It applies to intra as well, against the Files type.
  - For each value: pct = int(count of distinct (case_id, data_source_id) in CR having the value ÷ total rows in CR `data_sources` × 100).
  - Remove the value if pct > threshold. Remove a bucket when it becomes empty.
  - This costs one CR query per value.
- File resolution for CR instances:
  - If the CR case UUID equals the current case name: use `file_obj_id` (when non-zero). Otherwise match `lower(name)` + `lower(parent_path)` (backslashes → "/") + data source, taking the first hit.
  - When no file is found in the current case, a CR-only leaf is produced instead.

### Result presentation
- A new results tab in the standard results viewer (table/tree). Title format:
  - "Common Properties (All Data Sources, <cats>[, Threshold N%])"
  - "Common Properties (Data Source: X, …)"
  - "Common Properties (All Central Repository Cases, <type or cats>…)"
  - "Common Properties (Central Repository Case: X, …)"
  - <cats> = "All File Categories", "Documents", "Media" or "Documents, Media". The type name replaces it when the type is not Files.
- Path text "Common Properties Results". No results → an empty node "No results found." Progress text: "Gathering…" then "Displaying Common Properties Results."
- **By count tree**:
  - Root "Common Files" → "Exists in N data sources (M)" (M = number of values; fileset icon) → value node → instance leaves.
  - Value node display name = the most frequent file name among its instances (Files type), else "Value: X". Its Data Source column = the data source names joined.
- **By case tree**: root → case node (briefcase icon) → data source node "name (Id: n)" → instance leaves.
- Children populate lazily: the value lists ("delayed metadata") are copied on tree expansion.
- **Leaf columns**:
  - Case-DB-backed leaves (a real file node with its full context menu) show: Name, S/C/O (score/comment/occurrences, unless hidden in prefs), MIME Type, then either Parent Path / Data Source / Case (count mode) or Parent Path in Current Case / Value (case mode).
  - CR-only leaves show: Name (from the CR path basename), the same mode-specific columns, and an empty MIME Type.
- Column widths in px: Name 260, S/C/O 20 each, Instances 65, Parent Path 300, Case, Value, Data Source and Parent Path in Current Case 200 each, MIME 130, others 100.
- Total count in the tab = sum of instances across values.

### Autopsy problems & postmortem ideas (Common Properties)
- **N+1 everywhere**: one CR query per value for by-count, and again for the percentage filter. File objects are loaded per instance just to count data sources.
  - Replace with set-based SQL: `GROUP BY value HAVING COUNT(DISTINCT data_source_id) > 1` directly in CR (or in the case DB for intra).
  - Join the counts of distinct (case, data source) tuples for the threshold in the same query.
  - With one PostgreSQL holding both CR and cases, this is a single query. For SQLite CR, `ATTACH` the case DB.
- **Inconsistent known handling**: intra uses `known != 1 OR known IS NULL`; inter uses `known != 1`, which drops NULL. Unify the rule. Expose "hide known" and "only notable (CR BAD)" as real toggles, plus a min/max data-source count.
- The by-case batch-level `instances.size()>1` check is a bug. Showing current-case files under other cases' nodes confuses users. Show both sides: the current-case file and the remote instance, side by side.
- Intra "by case/data source" grouping is missing. Add it (it is a cheap pivot of the same data).
- File categories are hard-coded MIME lists that diverge from Discovery's lists. Share one category registry, and add Executables, Archives, etc.
- Results are not cancellable and have no progress beyond indeterminate. Run on a tokio task with cancellation, stream buckets into the tree as they are computed, and allow popout viewports per result set.
- The display name (most common file name) hides divergent names. Show the distinct name count and the hash value column.

## All Cases Search

Entry: **Tools > Search Central Repository**. It is enabled when CR is enabled and a case is open. Modal dialog "Search Central Repository".

### Dialog
- Text "Search the Central Repository for the given values." plus a "The Central Repository contains N case(s)." label.
- **Type:** combo of all defined CR correlation types: defaults plus one per account type `<type>_acct`, plus custom ones. Display names are adjusted:
  - names containing "addresses" become "…Address";
  - "Files" becomes "**File MD5**";
  - any other name ending in "s" (not "address") loses the trailing s, e.g. Domain, Phone Number, USB Device, Wireless Network, Installed Program, OS Account.
  - The list is sorted alphabetically.
  - The code mutates the shared type objects' display names in place, which is a side-effect risk.
- **Value:** a multi-line text area. **One value per line**; lines are split on CRLF, LF or CR after trimming the whole text. Gray placeholder examples per type: MD5 `f0e1d2c3b4a5968778695a4b3c2d1e0f`, `domain.com`, `user@host.com`, `(800)123-4567`, USB `4&1234567&0`, `WirelessNetwork-5G`, MAC `0C-14-F2-01-AF-45`, IMEI `351756061523999`, IMSI `310150123456789`, ICCID `89 91 19 1299 99 329451 0`.
- Note "Values will be normalized to ensure consistent case and formatting."
- Search is enabled when the text is non-empty.
- **Validation**: every line must pass normalization. On failure, a type-specific error appears ("The supplied value is not a valid MD5 hash / domain / e-mail address / phone number / wireless network / MAC address / IMEI number / IMSI number / ICCID number", else "…not valid."), Search is disabled, and focus returns to the text area. Changing the type clears the error.
- A blank line in the middle fails most validators.

### Normalization (CR normalizer, applied after trim)
| Type | Rule |
|---|---|
| File MD5 | lowercase, must match `^[a-f0-9]{32}$` |
| Domain | Apache DomainValidator (allow local) → `extractDomain(lowercased)`. Otherwise a dotted IPv4 (0–255 octets) passes through extractDomain |
| Email | Commons EmailValidator (allow local and TLD) → lowercase |
| Phone | chars: optional leading '+', digits, spaces, `-()`. Length ≥ 5. Strip whitespace and `-()`; the result must still be ≥ 5 |
| USB ID | unchanged |
| SSID (Wireless Network) | ≤ 32 characters, unchanged |
| MAC | lowercase, strip `[\s-:]`, must be 12 or 16 hex digits |
| IMEI | strip separators, 14–16 digits |
| IMSI | strip separators, 14–15 digits |
| ICCID | lowercase, strip separators, `^89[f0-9]{17,22}$` |
| Other default types (Installed Programs, OS Accounts, account types) | trimmed only. Unknown custom types throw an error |

### Query and results
- Background worker: for each line, run `SELECT <inst>.id, value, file_obj_id, cases.*, org fields, data_sources.* , device_id, file_path, known_status, comment FROM <type>_instances LEFT JOIN cases LEFT JOIN organizations LEFT JOIN data_sources WHERE value IN ('<normalized>')`. That is one query per line, and errors are only logged.
- Results open in a new results tab "All Cases (<type display name>)", description "Search Central Repository", count = number of instances. When empty: "No results found."
- Flat list, one row per CR instance. Display name = basename of file_path. **Columns**: Name, Case (display name), Data Source (name), Value, Known (known-status name), Path (full CR file_path), Comment, Device (data source device id).
- Rows offer only the generic node actions. There is no jump-to-file, even for instances in the current case.

### Autopsy problems & postmortem ideas (All Cases)
- One query per value. Use a single `IN`/`ANY($1)` parameterized query (string-built SQL is injection-prone for non-validated types such as USB, SSID and custom types). Paginate large result sets.
- Lines that fail validation abort the whole search. Instead, validate per line with inline highlighting, ignore blank lines, and offer a "search valid lines only" option.
- The UI mutates shared correlation-type display names. Keep display labels separate from model types.
- Add useful columns and actions:
  - case creation date, examiner, org;
  - known status as an icon;
  - "open in current case" when the instance belongs to the current case (resolve via file_obj_id);
  - "open other case" (multi-user: launch read-only viewer of that case);
  - export CSV;
  - group by case, with counts.
- Allow wildcard/prefix search for non-hash types (e.g. a domain suffix match on the normalized value with an index on `value text_pattern_ops`), and paste-in hash lists (thousands of lines) streamed through a temp table.

## Data Source Summary

Per-data-source dashboard: a set of tabs with statistics computed from the case DB (tsk_files, blackboard, timeline events, ingest_jobs) and, for geolocation, a bundled world-cities table. All statistics are read-only and computed on demand when a data source is selected. Nothing is cached between selections.

### Entry points and containers
- **Case menu → "Data Source Summary"** (menu position 302, after Case Details). Enabled only while a case is open. Opens the modal dialog with the first data source selected.
- **Data source node context menu → "View Summary Information"**, on Image nodes and SpecialDirectoryNode (local-files/logical data source roots). Opens the dialog with that data source preselected.
- **Result viewer tab "Summary"**: when the selected tree node's lookup contains a `DataSource`, the main result area gets a "Summary" tab with the same tabbed pane, without the data-source browser. A wait cursor shows while it switches.
- No other place opens it automatically, for example not after "Add Data Source". The Excel export moved out of the dialog into a report module (see Export).

#### Dialog layout ("Data Sources Summary", modal, resizable)
- Vertical split with the divider at 130 px. Top: data-source browser table. Bottom: tabbed pane. "Close" button. Closing cancels the background load and unregisters all listeners.
- **Browser columns**: Data Source Name | Ingest Status | Type | Files | Artifacts | Tags. Single selection. Count columns are right-aligned. All rows load in one background worker (wait cursor while it runs).
  - *Type*: every `TSK_DATA_SOURCE_USAGE` artifact's `TSK_DESCRIPTION` for that data source, joined with ", ".
  - *Files*: `SELECT data_source_obj_id, COUNT(*) FROM tsk_files WHERE meta_type=REG AND type<>VIRTUAL_DIR AND dir_type<>VIRT_DIR AND name<>'' GROUP BY data_source_obj_id`.
  - *Artifacts*: `COUNT(*) FROM blackboard_artifacts WHERE review_status_id <> REJECTED GROUP BY data_source_obj_id`.
  - *Tags*: content_tags joined to tsk_files, plus blackboard_artifact_tags joined to blackboard_artifacts, each grouped by data source and summed.
  - *Ingest Status*: reads `status_id` from `ingest_jobs WHERE obj_id=<ds>`. If any job is STARTED it shows "Started", otherwise "Completed" if any job completed, otherwise blank. On a `DataSourceAnalysisCompleted` event the row becomes COMPLETED (or blank if cancelled) and the selection is preserved.
  - Row double-click or context "Go to Data Source" selects the data source in the directory tree and closes the dialog.
- The tabbed pane shows "No data source has been selected." until a row is selected. On case close the data source is set to null.

### Tabs (fixed order) and the ingest modules they rely on
| # | Tab | Depends on (ingest module / artifact) |
|---|-----|------|
| 1 | Types | File Type ID (mime_type), Recent Activity/OS detection (TSK_OS_INFO, TSK_DATA_SOURCE_USAGE) |
| 2 | User Activity | Recent Activity (PROG_RUN, WEB_HISTORY, WEB_SEARCH_QUERY, DEVICE_ATTACHED), Email/Android etc. (MESSAGE, EMAIL_MSG, CALLLOG), Translation service (optional) |
| 3 | Analysis | Hash Lookup, Keyword Search, Interesting Files |
| 4 | Recent Files | Recent Activity (RECENT_OBJECT, WEB_DOWNLOAD), Email parser (ASSOCIATED_OBJECT → EMAIL_MSG/MESSAGE) |
| 5 | Past Cases | Central Repository ingest (TSK_PREVIOUSLY_NOTABLE / TSK_PREVIOUSLY_SEEN) |
| 6 | Geolocation | GPX parser, EXIF, Recent Activity/Android (TSK_GPS_*, METADATA_EXIF) |
| 7 | Timeline | Timeline events (created at add time and by ingest) |
| 8 | Ingest History | ingest_jobs / ingest_job_modules tables |
| 9 | Container | data source / image metadata, unallocated files |

- "Module not run" messages are defined but never shown in the current code: "No results will be shown because the Central Repository module was not run." (Past Cases) and "...because the GPX Parser was not run." (Geolocation). In practice an empty result shows the generic "No data exists." Postmortem should implement real detection (see ideas).
- Every tab except Container and Ingest History has a top banner, "⚠ Ingest is currently running.", visible while any ingest job runs. It toggles on ingest STARTED, CANCELLED and COMPLETED.

### Loading, refresh and caching (common machinery)
- **Per-tab sequential executor**: each tab owns one worker thread. Selecting a data source cancels running workers, sets every widget to "Loading results...", then submits one fetch job per widget. Each job's result goes to its widget on the UI thread. A job that was cancelled or interrupted is discarded silently.
- **Widget states**: loading → data | "No data exists." (null or empty list) | "There was an error loading results." (exception, logged). The pie chart shows "No Data" when empty.
- **Tables keep the selected row across refreshes**, matched by a key function per table (program path:name, domain, search string, device id, account type, file path, city name).
- **Refresh triggers** (per tab):
  - Ingest module events `DATA_ADDED`, `CONTENT_CHANGED` and `FILE_DONE` go through a throttler, so a tab refreshes at most once every **5 s**.
  - `DATA_ADDED` counts only if at least one artifact belongs to the current data source and the artifact type is in the tab's trigger set. `CONTENT_CHANGED` and `FILE_DONE` count only for content in the current data source.
  - Ingest job `COMPLETED` and `CANCELLED` refresh immediately.
  - Case event `DATA_SOURCE_NAME_CHANGED` refreshes the Container tab.
- **Trigger sets**:

  | Tab | Refresh trigger |
  |-----|-----------------|
  | Types, Timeline | Every content change or FILE_DONE (so the full count queries re-run every 5 s during ingest) |
  | Types (OS and Usage fields) | Also OS_INFO and DATA_SOURCE_USAGE artifacts |
  | User Activity | WEB_SEARCH_QUERY, MESSAGE, EMAIL_MSG, CALLLOG, DEVICE_ATTACHED, WEB_HISTORY, PROG_RUN |
  | Analysis | INTERESTING_FILE_HIT, INTERESTING_ARTIFACT_HIT, INTERESTING_ITEM, HASHSET_HIT, KEYWORD_HIT |
  | Recent Files | RECENT_OBJECT, WEB_DOWNLOAD, ASSOCIATED_OBJECT, EMAIL_MSG, MESSAGE |
  | Past Cases (bug) | Interesting-item types instead of PREVIOUSLY_SEEN/NOTABLE |
  | Geolocation | the 8 GPS types |
  | Container | content changes, OS_INFO, DATA_SOURCE_USAGE |
- **Row navigation**: right-clicking a row (hint text under each table: "Right click on row for more options") offers:
  - **"View Source Result"**: selects the artifact in the directory tree and results view, then closes the dialog.
  - **"View Source File in Directory"** (Recent Files only): drops a leading `X:` drive letter, normalizes to forward slashes, runs `findFiles(ds, fileName, parentPath)`, takes the first match, navigates to it and closes the dialog. This DB lookup happens lazily when the menu opens.
- Date cells use the format `yyyy/MM/dd HH:mm:ss` in the JVM default time zone. Counts use the `#,###` format.

### Tab 1: Types
Label/value rows, top to bottom:
- **Usage**: the same `TSK_DATA_SOURCE_USAGE` descriptions, from a join query `blackboard_artifacts ⋈ blackboard_attributes WHERE data_source_obj_id=? AND artifact_type_id=DATA_SOURCE_USAGE AND attribute_type_id=TSK_DESCRIPTION`, values joined with ", ".
- **OS**: the same query for `TSK_OS_INFO` / `TSK_PROG_NAME` values, joined with ", ".
- **Size**: `datasource.size` in SI units (divisor 1000: bytes, KB, MB, GB, TB, PB), formatted as a whole number with unit and no byte count (e.g. "500 GB").
- **File Types pie chart**. Every slice counts regular files only (`meta_type=REG AND type<>SLACK`, scoped by data_source_obj_id):

  | Slice | Colour (RGB) | Definition |
  |-------|--------------|------------|
  | Images | 156,39,176 | `mime_type IN` the IMAGE set |
  | Videos | yellow | VIDEO set |
  | Audio | blue | AUDIO set |
  | Documents | green | DOCUMENTS set |
  | Executables | 0,188,212 | EXECUTABLE set |
  | Unknown | orange | `mime_type IN ('application/octet-stream')` |
  | Other | 78,52,46 | allRegNonSlack − (sum of the six above + Not Analyzed) |
  | Not Analyzed | white | `mime_type IS NULL OR mime_type=''` |

  - Slices with a value of 0 are dropped. Slice label: `"{name}: {count} ({pct with 1 decimal}%)"`.
  - The chart's "useful content" flag is true when the category total or Other is greater than 0. The export uses it to skip the chart.
  - **MIME sets**:
    - IMAGE: the ImageIO reader MIME types (minus `application/octet-stream`; so the set depends on the runtime) plus bmp, gif, jpeg, png, tiff, vnd.adobe.photoshop, x-raw-nikon, x-ms-bmp, x-icon, webp, vnd.microsoft.icon, x-rgb, x-xbitmap, x-portable-graymap, x-portable-bitmap.
    - AUDIO: midi, mpeg, webm, ogg, wav, vnd.wave, x-ms-wma.
    - VIDEO: webm, 3gpp, 3gpp2, ogg, mpeg, mp4, quicktime, x-msvideo, x-flv, x-m4v, x-ms-wmv.
    - DOCUMENTS: text/{plain,css,html,csv,xml,x-log}; application/{rtf,pdf,json,javascript,xml,xhtml+xml,x-msoffice,x-ooxml,msword,msword2,vnd.wordperfect}; the OOXML word, presentation and spreadsheet types; vnd.ms-powerpoint, vnd.ms-excel, vnd.ms-excel.sheet.4; the ODF presentation, spreadsheet and text types.
    - EXECUTABLE: application/{x-bat,x-dosexec,vnd.microsoft.portable-executable,x-msdownload,exe,x-exe,dos-exe,x-winexe,msdos-windows,x-msdos-program}, vms/exe.
- **Allocated Files**: `meta_type=REG AND type<>SLACK AND (meta_flags & ALLOC) > 0`. **Unallocated Files**: same with UNALLOC. **Slack Files**: `meta_type=REG AND type=SLACK`. **Directories**: `meta_type=DIR AND type<>VIRTUAL_DIR`. Counts shown as "#,###"; null shown as "0".

### Tab 2: User Activity
Five tables. Each has a heading and the right-click hint. Limits: programs 10, domains 10, searches 10, devices 10, accounts 5.

**Recent Programs** (columns: Program 250 px, tooltip = full path | Folder 150 | Run Times 80 | Last Run 150)
- Source: `TSK_PROG_RUN` artifacts of the data source, using `TSK_PROG_NAME`, `TSK_PATH`, `TSK_COUNT` (int) and `TSK_DATETIME`.
- Rows are excluded when the name is blank, the name equals "NTOSBOOT" (case-insensitive), or the path starts with "/WINDOWS" (case-insensitive).
- Rows are grouped by the pair (uppercased name, uppercased path). A group keeps the maximum run count and the latest date, plus the artifact of the best-ranked row.
- Sort order: run count descending (null is lowest), then last run descending, then name ascending (case-insensitive).
- The 10-row limit applies **only if the top row has a run count > 0 or a last-run date > 0**. Otherwise the full alphabetical list is returned with no limit.
- **Folder** column: build the path element list `[root…leaf, programName]`. If the first element is "PROGRAM FILES" or "PROGRAM FILES (X86)", show the second element; else if any element is "APPLICATION DATA" or "APPDATA", show "AppData"; else empty.

**Recent Domains** (Domain 250 | Visits 100 | Last Accessed 150)
- Source: `TSK_WEB_HISTORY` rows that have `TSK_DATETIME_ACCESSED` and `TSK_DOMAIN`.
- Domains "127.0.0.1" and "LOCALHOST" are excluded. Domains are lowercased and trimmed.
- The window is the 30 days ending at the **latest visit in the data source**, not at the current date. Visits counts the domain's visits with `visit + 30 d ≥ mostRecent`.
- Domains with 0 visits in the window are dropped. Sort by visits descending and take 10. Last Accessed is that domain's latest visit. The navigation artifact is the latest visit.

**Recent Web Searches** (Search String 250 | Date Accessed 150 | Translated 250)
- Source: `TSK_WEB_SEARCH_QUERY` with `TSK_TEXT` and `TSK_DATETIME_ACCESSED`. Rows missing either are dropped. A date of 0 counts as missing.
- Rows are grouped by uppercased search string and keep the latest date. Sort by date descending and take 10.
- If a machine-translation provider is configured, each search string is translated. The translation is shown only when it is non-blank and differs from the original (case-insensitive, trimmed). Errors are logged and leave the cell blank.

**Recent Devices Attached** (Device Id 250 | Last Accessed 150 | Make and Model 250)
- Source: `TSK_DEVICE_ATTACHED` with `TSK_DEVICE_ID`, `TSK_DATETIME`, `TSK_DEVICE_MAKE` and `TSK_DEVICE_MODEL`.
- Rows whose model (trimmed, uppercased) is "ROOT_HUB" or "ROOT_HUB20" are dropped, unless the device id or the model is null.
- Rows are grouped by device id and keep the latest date. Take 10.
- **Bug**: grouping goes into a hash map, so the "recent" order is lost before the limit. Postmortem should sort by date descending and then take 10.
- Make and Model cell: "make - model", or just whichever of the two is non-empty.

**Recent Account Types Used** (Account Type 250 | Last Accessed 150)
- Sources: `TSK_MESSAGE` (type = `TSK_MESSAGE_TYPE`, date = `TSK_DATETIME`); `TSK_EMAIL_MSG` (type "Email Message", date = max(`TSK_DATETIME_RCVD`, `TSK_DATETIME_SENT`)); `TSK_CALLLOG` (type "Call Log", date = max(`TSK_DATETIME_START`, `TSK_DATETIME_END`)). Rows without type or date dropped; grouped by type keeping the latest date; sorted by date descending; top 5.

### Tab 3: Analysis
- Three tables: **Hashset Hits** (`TSK_HASHSET_HIT`), **Keyword Hits** (`TSK_KEYWORD_HIT`) and **Interesting Item Hits** (`TSK_INTERESTING_FILE_HIT` + `TSK_INTERESTING_ARTIFACT_HIT` + `TSK_INTERESTING_ITEM`).
- Columns Name 300 | Count 100. Name = `TSK_SET_NAME` (hash set, keyword list or rule-set name); count = number of artifacts with that name; blank names skipped. Sorted by count descending, **no limit**. The keyword exclusion set exists but is empty. No right-click navigation.

### Tab 4: Recent Files
Three tables, top 10 each. Each table deduplicates by uppercased path, keeping the newest row, and sorts by date descending. Rows with a blank path or a date of 0 are dropped.
- **Recently Opened Documents** (Path 250 | Date 80): `TSK_RECENT_OBJECT`, path = `TSK_PATH`, date = `TSK_DATETIME_ACCESSED`.
- **Recent Downloads** (Domain 100 | Path 250 | Date 80): `TSK_WEB_DOWNLOAD` with `TSK_PATH`, `TSK_DATETIME_ACCESSED`, `TSK_DOMAIN`.
- **Recent Attachments** (Path 250 | Date 80 | Sender 150): `TSK_ASSOCIATED_OBJECT` artifacts whose `TSK_ASSOCIATED_ARTIFACT` points to a `TSK_EMAIL_MSG`/`TSK_MESSAGE` and whose parent is a file. Path = parent path + name; date = message `TSK_DATETIME_SENT`; sender = message `TSK_EMAIL_FROM`; navigation artifact = the message.
- Right-click offers "View Source File in Directory" (path lookup) and "View Source Result".

### Tab 5: Past Cases
Three tables (Case 300 | Count 100). They have no limit, no navigation, and are sorted by count descending. Footnote: "NOTE: These results are from the time of ingest. They are not real-time updates."
- **"Cases with common items that were marked as notable"**: all `TSK_PREVIOUSLY_NOTABLE` artifacts of the data source.
- **"Cases with the same addresses (Email, Phone, etc.)"**: `TSK_PREVIOUSLY_SEEN` artifacts whose **parent object is not** a device-type artifact.
- **"Cases with the same device IDs (USB, Wifi, SIM, etc.)"**: `TSK_PREVIOUSLY_SEEN` artifacts whose parent object is an artifact of type `TSK_DEVICE_ATTACHED`, `TSK_DEVICE_INFO`, `TSK_SIM_ATTACHED` or `TSK_WIFI_NETWORK_ADAPTER`. This costs one `getContentById` call per artifact.
- **Case extraction**:
  - Read the `TSK_OTHER_CASES` attribute. Use it only if its *sources* include the Central Repository module name.
  - Split the value on ",", trim, and group case-insensitively. The first spelling seen is the one displayed.
  - Count = the number of artifacts that reference the case.

### Tab 6: Geolocation
- Two tables (Closest City 300 | Count 100):
  - **"Recent Cities from Geolocation Results"**: points within 30 days of the newest point.
  - **"Most Common Cities from Geolocation Results"**: all points.
- Each table has a note "Locations further than 150km from a city will be listed as 'Unknown'" and a **"View in Map"** button. The button is enabled only after a non-empty successful load.
- **Point gathering**:
  - Uses the geolocation waypoint builder over the 8 types: GPS_BOOKMARK, GPS_LAST_KNOWN_LOCATION, GPS_ROUTE, GPS_SEARCH, GPS_TRACK, GPS_TRACKPOINT, METADATA_EXIF and GPS_AREA. Waypoints without timestamps are included.
  - Each **track and area** is reduced to its set of distinct closest cities, each with the latest timestamp of its points in that city. So a track contributes 1 per city.
  - Standalone waypoints (those not part of any track or area) contribute 1 each.
- **Counting**:
  - mostRecent = the newest timestamp among all point/city pairs. minTime = mostRecent − 30 × 86 400 s.
  - For each city: *common* = the total count; *recent* = the count with `time ≥ minTime`. Points with no time count only toward *common*.
  - Points with no city within 150 km go into "Unknown", which has its own common and recent counts.
  - Each list: take the top 10 cities by count, append "Unknown", drop rows with a count of 0, re-sort descending and take 10. Unknown can therefore displace a city.
- **City name format**: "City, State; Country" when all three are present, "A, B" when two are, otherwise the single value.
- **Closest-city algorithm**:
  - The bundled `worldcities.csv` (simplemaps, about 26 560 rows) is loaded lazily as a singleton.
  - Columns used: city (0), lat (2), lng (3), country (4), admin_name (7).
  - Parsed with a naive regex over quoted values. Countries written "X, Y" become "X Y" (e.g. "Korea, South" → "Korea South").
  - Cities are bucketed into a grid of ≈300 km cells. Bucket index: distance from the equator / 300 km, and east-west distance along that latitude / 300 km, signed. Each cell holds a KD-tree.
  - Query: the KD-tree nearest neighbour in the 2×2 cells around the point (the second cell is chosen by rounding). Candidates beyond **150 km** (half the cell size) are rejected, and the closest remaining candidate wins. Distance is the haversine formula with Earth radius 6371 km.
  - **Bug**: the KD-tree point stores x=lat, y=lng. The bucket and 150 km filter code treats y as latitude, so bucketing and the threshold use swapped coordinates. Postmortem: implement a correct lat/lng haversine distance, for example with an R-tree or a sorted-latitude band.
- **"View in Map" button**:
  - Closes the dialog, sets the Geolocation window filter to this data source and the 8 types, and opens the window.
  - Recent: `showAll=false`, `mostRecentNumDays=30`, waypoints without timestamps excluded. Common: `showAll=true`.

### Tab 7: Timeline
- **Activity Range**: Earliest and Latest dates, formatted "MMM d, yyyy".
- **"Last 30 Days" stacked bar chart**, series "File Events" (228,22,28) and "Result Events" (21,227,100). Only the first and last bars are labelled with dates.
- Button **"View in Timeline"**.
- **Data**:
  - Fetch every timeline event for the data source in the interval [1 ms, now]. The filter is the Timeline's *default* filter state with only this data source selected.
  - Bucket each event by local date in the user's display time zone (`UserPreferences.timeZoneForDisplays`). File events are FILE_MODIFIED, FILE_ACCESSED, FILE_CREATED and FILE_CHANGED. Everything else counts as an artifact ("Result") event.
  - min/max day come from all events. If there are no events, the result is null and the tab shows "No data exists."
  - The window is `[max(maxDay−29, minDay) … maxDay]`, i.e. relative to the **last activity day, not today**. Missing days are zero-filled.
  - **Bug**: Result counts are multiplied by 100 before charting, in both the UI and the export. The "UTC" date formatter does not actually set UTC.
- **View in Timeline**: closes the dialog, pushes the data source filter to the Timeline controller, and opens Timeline zoomed to [first window day, last window day + 1 day].

### Tab 8: Ingest History
- Reuses the ingest-job panel: upper table = jobs of this data source (Start Time | End Time | Ingest Status); lower = modules of the selected job (Module Name | Module Version). Reloads on ingest STARTED/CANCELLED/COMPLETED and case change.

### Tab 9: Container
- Fields: Display Name and Name (both `ds.name`); Device ID; Time Zone; Acquisition Details (read-only, 4 rows); Image Type (TSK type name); Size (SI, "#.##" + full value, e.g. "1.5 GB (1500000000 bytes)"); Sector Size; MD5/SHA1/SHA256 (as stored on the image); Unallocated Space; File Paths (header-less list of segment paths).
- **Non-image data sources** (logical files, local directories) show "N/A" for Unallocated Space, Image Type, Size, Sector Size, Time Zone, the hashes and File Paths.
- **Unallocated Space**: `SELECT SUM(size) FROM tsk_files WHERE meta_flags & UNALLOC > 0 AND type<>SLACK AND type<>VIRTUAL_DIR AND dir_type<>VIRT_DIR AND name<>'' AND data_source_obj_id=?`. Shows "N/A" when the sum is ≤ 0. Deleted files are included along with the unallocated-block files, so space can be double counted.
- Refreshes on data-source rename. If loading fails, the fields are cleared and the error is only logged.

### Export to Excel (now "Data Source Summary Report" general report module)
- Runs from the report wizard (data-source selection, default all). Writes one `.xlsx` per data source to `<reportDir>/DataSourceSummary/<escapedDsName>-MM-dd-yyyy-HH-mm-ss.xlsx` and registers it as a case report (module "Data Source Summary"). Progress: 11 labelled steps.
- **Sheet order**:
  1. **Types**: key/value rows Usage, OS, Size (bytes cell), then an embedded native Excel pie chart with its data table (only when useful), then the Allocated, Unallocated, Slack and Directories counts.
  2. User Activity tables, same columns as the UI, one sheet each: **Recent Programs**, **Recent Domains**, **Recent Web Searches**, **Recent Devices Attached**, **Recent Account Types Used**.
  3. Recent Files tables: **Recently Opened Documents**, **Recent Downloads**, **Recent Attachments**.
  4. Analysis tables: **Hashset Hits**, **Keyword Hits**, **Interesting Item Hits**.
  5. Past Cases tables: **Cases with common notable items**, **Cases with the same addresses**, **Cases with the same device IDs**.
  6. Geolocation tables: **Most Recent Cities**, **Most Common Cities**.
  7. **Timeline**: an "Activity Range" title, "Earliest:" and "Latest:" rows, then a native stacked bar chart with a Date column showing every date.
  8. **Ingest History**: one row per module (Start Time, End Time, Ingest Status, Module Name, Module Version). Jobs are sorted by start time and modules by name. Job fields appear only on each job's first row.
  9. **Container**: key/value rows in UI order. Acquisition Details and File Paths are titled multi-row blocks.
- **Formatting**: header rows are bold and frozen, columns are auto-sized, and dates use `yyyy/MM/dd HH:mm:ss`.
- **Failures**: a sheet whose fetch fails is silently omitted (the error is logged). An empty list still produces a sheet with only the header row.

### Autopsy problems & postmortem ideas (Data Source Summary)
- **Full artifact scans in Java**: every "top N" loads every artifact of a type, plus each attribute via separate queries, into the JVM. On large phones or browser histories this means hundreds of thousands of round-trips. Postmortem: push grouping, max and limit into SQL (`GROUP BY lower(domain)`, window functions) with indexes on (data_source_obj_id, artifact_type_id) and attribute (artifact_id, attribute_type_id).
- **Refresh storm**: Types and Timeline re-run all count queries and the full timeline event fetch every 5 s during ingest. Postmortem options:
  - Maintain incremental counters per data source (mime category, alloc flags) updated by the ingest pipeline.
  - Or debounce and only refresh visible tabs.
  - Timeline daily histogram: `SELECT day_bucket, is_file, COUNT(*)` with a time-zone offset in SQL, not loading every event object.
- **Hidden tabs still fetch**: all 9 tabs fetch on every selection. Fetch lazily for the visible tab and cache per (data source, tab, data version), invalidating on artifact-type events.
- **Geolocation**: the coordinate swap bug distorts the 150 km threshold. The waypoint builder is async-wrapped-as-sync and computes the closest city per point (O(n·log m) with a per-call allocation). Postmortem:
  - Precompute the closest city once per waypoint at ingest and store it in a table.
  - Ship the city DB in SQLite with an R-tree.
  - Make the distance threshold configurable, default 150 km.
- **Wrong orderings and limits**: devices lose their recency order; programs return an unlimited list when there are no run counts; Unknown can push out a 10th city. Define deterministic ordering with tie-breakers.
- **Past Cases**: refreshes on the wrong artifact types, and runs one `getContentById` per previously-seen artifact. Join on parent type in SQL.
- **"Module not run" detection is dead code**. Postmortem: read `ingest_job_modules` for the data source and show a specific "Run <module> to populate this" message with a "Run ingest module" action.
- **Timeline chart**: the ×100 scaling bug; only first and last date labels. Postmortem: plot with egui_plot, add hover tooltips with exact counts per day, and let clicking a bar open the Timeline zoomed to that day.
- **Unallocated Space** double counts. Compute it from volume and file-system unallocated block runs instead, and show it in SI units consistent with the org metric default (offer IEC as an option).
- **Popout and multi-viewport**: make the summary a dockable or pop-out egui viewport instead of a modal dialog, so it can stay open beside the tree. Navigation actions should focus the main viewport instead of closing the summary.
- **Multi-user**: there is no cross-node notification; a remote node's ingest does not refresh the summary. Postmortem: subscribe to the shared event bus (Postgres LISTEN/NOTIFY) for artifact-added events of the selected data source.
- **Export**: make it available directly from the summary panel, not only the report wizard. Write with `rust_xlsxwriter`, which supports native charts, and keep the same sheet order for parity. Also offer CSV/JSON and a per-tab "copy table".

## Health Monitor

Health Monitor is an **optional, multi-user-only** telemetry facility. Each Autopsy node buffers timing measurements and user/case events in memory and periodically appends them to a shared PostgreSQL database. An admin dashboard plots the data. Single-user mode never enables it.

### Lifecycle and enabling
- **Startup** (module install `restored`):
  - Creates the singleton and records a `LOG_ON` user event.
  - Host = local host name. If the host name cannot be resolved, a random UUID is used for the session.
  - Username = OS `user.name`.
  - Registers a case listener: the CURRENT_CASE event records `CASE_OPEN` (old value null) or `CASE_CLOSE` (new value null).
- **Shutdown** (module `close`): records `LOG_OFF`, then flushes immediately.
- **Enabled state is global**, stored in the shared DB as `db_info.name='MONITOR_ENABLED'` with value `'true'` or `'false'`. Each node re-reads it at startup, on every periodic tick, and whenever the dashboard opens. Nothing is stored in local preferences.
- **Re-read rules** (on each check):
  - If multi-user mode is off, the database `HealthMonitor` does not exist, or `db_info` has no SCHEMA_VERSION, the monitor counts as disabled and buffers are dropped.
  - If the case-DB connection settings (host, port, user, password) changed since the last check, the connection pool is rebuilt.
  - When the flag flips, the node activates (create, upgrade or verify the schema, then clear buffers) or deactivates (clear timing buffer, close pool) locally.
- **Turning it on or off**: only from the dashboard's admin panel. Enabling creates the DB (`CREATE DATABASE "HealthMonitor" OWNER <case-db user>` via the `postgres` maintenance DB on the **same server and credentials as the multi-user case DB**). It then initializes the schema, which sets MONITOR_ENABLED='true'.
- **Locking**: a coordination-service (ZooKeeper) lock with category `HEALTH_MONITOR` on node `HealthMonitor`:
  - **Exclusive** for create, initialize or upgrade.
  - **Shared** for writes and reads.
  - Acquisition times out after 5 min, which raises an error.
- **Connection pool**: DBCP with initialSize 3, maxIdle 10 and validation query `SELECT version()`.

### Shared DB schema (PostgreSQL database "HealthMonitor", schema version 1.2)
```sql
CREATE TABLE timing_data (id SERIAL PRIMARY KEY, name text NOT NULL, host text NOT NULL,
  timestamp bigint NOT NULL /*epoch ms of flush*/, count bigint NOT NULL,
  average double precision NOT NULL, max double precision NOT NULL, min double precision NOT NULL /*ms*/);
CREATE TABLE db_info (id SERIAL PRIMARY KEY NOT NULL, name text NOT NULL, value text NOT NULL);
  -- rows: SCHEMA_VERSION='1', SCHEMA_MINOR_VERSION='2', MONITOR_ENABLED='true'|'false'
CREATE TABLE user_data (id SERIAL PRIMARY KEY, host text NOT NULL, timestamp bigint NOT NULL /*epoch ms of event*/,
  event_type int NOT NULL /*0 LOG_ON,1 LOG_OFF,2 CASE_OPEN,3 CASE_CLOSE*/,
  is_examiner boolean NOT NULL /*true if node mode == STANDALONE (examiner), false = auto-ingest node*/,
  case_name text NOT NULL /*case display name or ''*/, username text /*added in 1.2*/);
```
- **Schema history**: 1.0 had timing_data and db_info. 1.1 added user_data. 1.2 added `user_data.username` (only if the column is missing; upgrades must be idempotent because old clients re-run them). Version rows are updated with plain `UPDATE`s.
- There are no indexes besides the primary keys, and no retention or purge job, so the tables grow without bound.

### Metrics recorded
- **Timing metric API**:
  - `getTimingMetric(name)` returns null when the monitor is disabled, which makes it a no-op. Otherwise it captures `System.nanoTime`.
  - `submitTimingMetric(m)` computes the duration in **ms** (double).
  - `submitNormalizedTimingMetric(m, n)` divides the duration by n when n > 1.
  - Samples are aggregated in memory per metric name as count, sum, max and min.
- **Metric names in use**:

  | Name | Where | Normalization |
  |------|-------|---------------|
  | `Disk Reads: Hash calculation` | hash lookup ingest (MD5+SHA-256 calculation per file) | Files of 0 bytes are skipped. Files under 1 000 000 bytes are not normalized. Larger files are divided by `size/500000` (i.e. per 0.5 MB). |
  | `Database: getImages query` | timer-based, run by the monitor itself each tick if a case is open | Divided by (number of images + 2 if 0 images, +3 if 1, +5 if fewer than 10, else +7) |
  | `Solr: Index chunk` | Keyword Search indexing of each text chunk (two code paths) | none |
  | `Solr: Connectivity check` | Solr server connectivity checks (two places) | none |
  | `Central Repository: Notable artifact query` | CR ingest, notable lookup | none |
  | `Central Repository: Bulk insert` | CR bulk insert of correlation attributes | none |

- **User events**: `LOG_ON`, `LOG_OFF`, `CASE_OPEN`, `CASE_CLOSE`. Each carries a timestamp (ms), is_examiner and the case display name, plus the host and username at write time.

### Persistence interval
- A single-thread scheduler runs every **60 min**, with fixed delay and the first run 60 min after startup. Each tick:
  1. Re-reads the global enabled flag.
  2. If enabled, runs the getImages timing query.
  3. Under the shared lock, swaps out the buffers and inserts one `timing_data` row per metric name (timestamp = now, plus count, avg, max, min) and one `user_data` row per event, then commits.
- Nothing is written when both buffers are empty.
- Each timing row therefore represents roughly one hour of aggregated samples from one host.
- If a write fails, that hour's data is lost because the buffers were already cleared. While disabled, user events still accumulate in memory and are cleared only on activation.

### Dashboard UI ("Health Monitor" dialog)
- **Entry point**: the "Health Monitor" button on the Auto Ingest node-status dashboard (Experimental module). There is no menu item in the examiner UI.
- **Data load**: on open, the dashboard refreshes the enabled flag and loads `SELECT * FROM timing_data WHERE timestamp > now-31d` and the same for user_data (shared lock). All filtering after that happens in memory. To refresh, close and reopen. Enabling or disabling re-creates the dialog.
- If the monitor is disabled, each section shows "No data to display - monitor is not enabled". With no rows it shows "No data to display".
- **Timing Metrics section**:
  - **Controls**: "Max days to display" combobox (One day = default, One week = 7, Two weeks = 14, One month = 31 days); "Filter by host" checkbox (default off) enabling a combobox of every host in the data; "Show trend line" (default **on**); "Do not plot outliers" (default off).
  - **One graph per metric name** in a vertical scroll pane. Each is a line graph of `average` (ms) over `timestamp`, points sorted by x, 2 px blue line (18,32,219,α180).
  - **Axis scaling**:
    - Axis ranges come from **all hosts' data in the selected range**, so per-host graphs stay comparable.
    - X axis: [min − 2 h, max + 2 h].
    - Y axis: [max(0, min − 0.1·max), 1.1·max].
  - **Outlier skipping**: when enabled and max > 5 × mean, y-max is clamped to mean + 1 σ (population standard deviation over all hosts). Points above that are drawn off-scale.
  - **Y units**: the unit is chosen from the mid-range value (ns, µs, ms, s, min or h). There are 10 y divisions with labels rounded to 2 decimals. The text "displaying time in <unit>" appears beside the metric name title.
  - **X grid**: vertical lines at midnights counting back from the last midnight. At most 20 divisions; beyond that, the days per division are rounded up. Labels are "M/D" in GMT.
  - **Trend line**: ordinary least squares on the (host-filtered) points, `y = slope·timestamp + yInt`. Slope is 0 when the denominator is 0. The line is clipped to the y range by solving for x, and drawn in dark red (150,10,10).
- **User Metrics section**:
  - **Controls**: the same "Max days to display" combobox and a "Generate Report" button.
  - **Two bar graphs**, sampled **hourly** from now back to the threshold. The full 31 days of events are used so that a node's state carries over from before the window.
    - **"Cases open"**: per host, take the latest event at or before the sample time. If it is CASE_OPEN, count its case name, deduplicated across hosts. The result is the number of distinct open cases.
    - **"Users logged in - examiner nodes in blue, auto ingest nodes in green"**: per host, the latest event is anything other than LOG_OFF. Bars are stacked examiner (18,32,219) and auto-ingest (18,128,32).
    - Both graphs share a y-max equal to the larger peak, rounded up to a multiple of 5, with a minimum of 5. X axis: [threshold − 1 h, now + 2 h].
    - **Bug**: state is tracked **per host**, not per host and user, so two users on one host conflate.
- **Generate Report** (CSV):
  - Save dialog with default name `UserReport_yyyyMMdd_HHmmss.csv` (last directory remembered in preferences). Always covers the **last 7 days**, regardless of the combobox. Header: `Case open,Case close,Duration,Host,User,Case name`.
  - For each CASE_OPEN event, in timestamp order, find the next event with the same host and user. If that event is a CASE_CLOSE with the same case name, fill in the close time and the duration `H:MM:SS`. Otherwise those two cells stay blank, which covers crashes and still-open cases.
  - Timestamps are written as `yyyy/MM/dd HH:mm:ss`. The case name is quoted without escaping embedded quotes. The final event in the list is never evaluated as an open.
  - Shows "Report saved to: <path>" or "Error generating report".
- **Admin panel**: shown only if a file named `admin` exists in the user config directory. It has "Enable monitor" and "Disable monitor" buttons; the one matching the current state is disabled. A click calls the global set-enabled routine and redisplays the dialog.
- **Developer helper**: a sample-data generator (not exposed in the UI) deletes all timing_data rows and synthesizes hourly data for the 6 metric names across N hosts named `testHost<i>`. It includes outliers and a "slow node" ramp over the last 3 days.

### Multi-user aspects
- **Shared database**: every node (examiner and auto-ingest) writes to one shared DB on the case PostgreSQL server. Hosts are distinguished only by host name, and the user only in user_data.
- **Enable/disable propagation**: a change made on one node reaches the others only on their next hourly tick or dashboard open.
- **ZooKeeper locks**: these serialize schema creation and upgrade. Normal writes take shared locks only, so they run concurrently.

### Autopsy problems & postmortem ideas (Health Monitor)
- **Up to 60 minutes of latency**: the flag poll and the flush share one hourly tick. Data is lost on a crash or a failed write, because the buffers are cleared before the insert. Postmortem:
  - Flush every 5 min and on shutdown.
  - Keep the buffer until the commit succeeds.
  - Use one transaction with multi-row INSERT or COPY.
  - Watch the flag via `LISTEN/NOTIFY`.
- **No indexes or retention on an append-only table**: the dashboard runs full-table scans. Add an index on `(timestamp)` and `(name, host, timestamp)`, a configurable retention period (e.g. 90 days), and server-side downsampling for ranges longer than a week.
- **SQL built by string concatenation** for timestamps and flags, and DB creation that reuses case-DB superuser-ish credentials. Use parameterized queries in postmortem and a dedicated role and schema, possibly inside the main postmortem Postgres database, to avoid needing CREATE DATABASE rights.
- **Sample-based aggregation is lossy**: only avg, min and max per hour are stored, so there are no percentiles. Store count, sum, sum of squares, min, max, plus a small histogram or t-digest (p50, p95) so the dashboard can show latency distributions and real outlier bands, instead of the "mean + 1 σ if max > 5×mean" heuristic.
- **Units**: the dashboard picks ns through h per graph. Postmortem should store durations as integer µs, display SI time units consistently, and label axes clearly.
- **Hard to reach**: the dashboard is hidden behind the auto-ingest dashboard plus an `admin` marker file. Postmortem should provide a proper Tools → "Health Monitor" multi-viewport window gated by a role or permission in the multi-user user table. It should also have:
  - live refresh,
  - host multi-select instead of a single host,
  - a custom date range,
  - per-metric toggles,
  - hover tooltips (egui_plot).
- **Wrong state granularity**: user state is tracked per host, so multiple users on one host are wrong. Track state per (host, username, session id); add a session UUID column to pair open and close events robustly.
- **Instrumentation**: extend timing metrics generically through a tracing layer (`tracing` spans with a field `health_metric=...`) instead of explicit start/stop calls. Candidates: ingest module per-file time, case-DB query latency, central-repo latency, text-index latency, disk throughput normalized per MB.
- **Health beyond timing**: add disk free space on case and output volumes (GB), DB connection-pool saturation, ingest queue depth, and per-node heartbeats so stale nodes are visible.

## Central Repository

The Central Repository (CR) is a database that lives outside any one case and is shared between cases. It stores normalized "correlation attribute instances" (an MD5, domain, email, phone, device ID, account, and so on), each with its case, data source, path, known status and comment. It also holds reference hash sets, organizations, accounts and personas. Its main uses are: the "Other Occurrences" content viewer, the O (occurrences) and C (comment) columns in result tables, the "Previously Notable/Seen/Unseen" analysis results created at ingest, CR-backed hash sets, personas (also shown in CVT and the communication artifact viewers), and the Common Properties and Discovery searches (those two are documented in their own sections).

### Configuration, enabling and platforms

- **Settings store**: module properties file `<moduleConfigDir>/CentralRepository/CentralRepository.properties`. The module settings key is the path of that folder relative to the user config dir. At startup, settings from the legacy key `CentralRepository` are migrated. If the old DB was in the legacy default `<userdir>/central_repository/`, its files (matched by `startsWith(dbName)`) are copied to the new default dir.
- **Keys**:

| Key | Meaning / default |
|---|---|
| `db.useCentralRepo` | master enable flag (`true`/`false`) |
| `db.selectedPlatform` | `Disabled` / `Sqlite` / `PostgreSQL_Multiuser` / `PostgreSQL` (custom). Unknown values map to Disabled |
| `disabledDueToFailure` | set to true when an upgrade or connection fails. The CR is then disabled and the choice saved as DISABLED |
| `initialized` | first-run auto-setup done |
| `db.sqlite.dbName` | default `central_repository.db`. Regex `[a-z][a-z0-9_]*(\.db)?` |
| `db.sqlite.dbDirectory` | default `<moduleConfigDir>/CentralRepository/LocalDatabase`. Stored relative to the user dir when it is inside it |
| `db.sqlite.bulkThreshold`, `db.postgresql.bulkThreshold` | default **1000**. Values ≤0 or invalid fall back to 1000 |
| `db.postgresql.host` / `port` / `dbName` / `user` / `password` | host default "", port **5432** (allowed 1–65535), dbName **`central_repository`** (regex `[a-z][a-z0-9_]*`, lowercased), user regex `[a-zA-Z]\w*`. The password is PBEWithMD5AndDES-encrypted (fixed key/salt, 20 iterations) and Base64-encoded |

- **"PostgreSQL using multi-user settings"**: host, port, user and password come from the multi-user case DB connection settings. The DB name is always `central_repository`, and nothing is ever saved under CR keys. The choice is only offered when multi-user mode is enabled *and* that connection tests OK.
- `CentralRepository.isEnabled()` = `db.useCentralRepo == true` AND the saved choice ≠ DISABLED.
- **First run** (core installer): if not `initialized`, and `db.useCentralRepo` was not already true, create a default CR automatically. In multi-user mode it creates the multi-user PostgreSQL CR, otherwise a SQLite DB at the default path. Then it sets `initialized=true`. In RELEASE builds with a GUI it shows a one-time info notice ("Autopsy stores data about each case in its Central Repository… Ignore common items, Identify where an item was previously seen, Create personas…").
- **Test/create flow** (`testStatus` returns `TESTED_OK`, `DB_DOES_NOT_EXIST`, `SCHEMA_INVALID`, `CONNECTION_FAILED` or `UNTESTED`):
  - SQLite: if the file is missing → DB_DOES_NOT_EXIST. Otherwise it connects and runs `SELECT count(*) from sqlite_master`, then checks that `db_info` has `SCHEMA_VERSION`.
  - PostgreSQL: connects to the `postgres` DB and runs `SELECT version()`. Then `SELECT datname FROM pg_catalog.pg_database WHERE lower(datname)=lower(?)`, then the schema-version check.
  - Create: SQLite makes the directory. PostgreSQL runs `CREATE DATABASE <name> OWNER <user>`. Then schema init and default content are written. On failure the DB is deleted (`DROP DATABASE` / delete the file).
  - Prompts: "does not exist, create?", "exists but is not the right format" (schema invalid), "connection failed".
- **Saving a new choice**: shuts down the old pool, clears caches, writes the flag, choice and settings, then calls `updateSettings()`. The dialog says "Please restart Autopsy to begin using the new database platform" on error. The Personas window is closed whenever settings change.
- **Multi-user toggle interplay**:
  - When multi-user is turned on, the user is asked "update the CR to use the PostgreSQL server on {host}? (SQLite data is not transferred)". Yes → choice PostgreSQL_Multiuser, then test and create.
  - When multi-user is turned off while CR = MU-Postgres, a 3-way dialog offers Use SQLite / Configure PostgreSQL / Disable.
  - When MU connection settings change, the CR is re-tested. There is **no data migration** between platforms.
- **Global options panel**: "Use a Central Repository" checkbox; Database Configuration (Type/Name/Location + Configure + Test, with an OK/fail icon and message); Correlation Properties ("Manage Correlation Properties"); Organizations ("Manage Organizations"); Case Details ("Manage Cases"). Editing is disabled while ingest runs ("Cannot make changes … when ingest is running!"). The DB platform cannot be changed while a case is open.
- **Case open** (`CentralRepositoryService.openCaseResources`, only when the CR is enabled):
  1. Schema upgrade under the multi-user exclusive lock.
  2. The v1.2 data fix-up: any `data_sources` rows of this case with `datasource_obj_id=0` are matched to case data sources by (device_id, name) and get the obj id.
  3. The case event listener starts. Case close shuts down the listener executor and drains it ("Finishing adding data to central repository database…").
- **Multi-user DB lock**: coordination-service exclusive lock in category CENTRAL_REPO, node `<host>_<dbName>`, 5-minute timeout. It is only taken for PostgreSQL in multi-user mode and only around schema upgrade. SQLite ingest into a multi-user case is refused ("Multi-user cases require a PostgreSQL central repository").

### Connection handling, SQLite vs PostgreSQL

| Aspect | SQLite | PostgreSQL |
|---|---|---|
| Driver/URL | `jdbc:sqlite:<dir>/<name>` | `jdbc:postgresql://host:port/db` |
| Pool | DBCP `initialSize=50`, `maxTotal=-1`, `maxIdle=-1`, `maxWait=1000 ms`. The init SQL is `PRAGMA foreign_keys = ON` (OFF for upgrade connections) | DBCP `initialSize=5`, `maxIdle=10` |
| Locking | in-process fair `ReentrantReadWriteLock`. Every write method takes the write lock and every read takes the read lock. There is no cross-process locking, so the UI warns "SQLite should only be used by one examiner at a time" | none client-side. Coordination lock only for upgrade |
| Schema-create pragmas | `journal_mode=WAL`, `synchronous=OFF`, `read_uncommitted=True`, `encoding='UTF-8'`, `page_size=4096`, `foreign_keys=ON` | — |
| PK | `integer primary key autoincrement NOT NULL` | `SERIAL PRIMARY KEY` |
| BIGINT | `INTEGER` | `BIGINT` |
| Conflict handling | `ON CONFLICT IGNORE` baked into the UNIQUE constraints (cases.case_uid, `*_instances`, `reference_*`). `INSERT OR IGNORE` for accounts/examiners/types | `ON CONFLICT DO NOTHING` appended to every insert |
| Column check | `PRAGMA table_info(t)` | `information_schema.columns` |
| `reset()` (unused by UI) | `DELETE` from all tables, then `VACUUM`. It is buggy: it references `artifact_types` and `global_files` | `TRUNCATE … RESTART IDENTITY CASCADE` |

- Inserts use auto-commit, except bulk reference-set inserts, which run in one explicit transaction.
- Caches (Guava, static):
  - case by UUID and by id: 5 min.
  - data source by (case, dsObjId) and by (case, id): 5 min.
  - accounts by (type, id): 5 min.
  - account types: forever.
  - correlation types: forever until `clearCaches`.

### Schema (current version 1.6)

`db_info(id PK, name TEXT UNIQUE NOT NULL, value TEXT NOT NULL)` holds the rows `SCHEMA_VERSION`=1, `SCHEMA_MINOR_VERSION`=6, `CREATION_SCHEMA_MAJOR_VERSION` and `CREATION_SCHEMA_MINOR_VERSION`.

| Table | Columns | Constraints / indexes |
|---|---|---|
| `organizations` | id, org_name text NN, poc_name NN, poc_email NN, poc_phone NN | UNIQUE(org_name). Default row `"Not Specified"`, '', '', '' |
| `cases` | id, case_uid NN (the Autopsy unique case *name*, e.g. `Foo_20200101_120000`), org_id int, case_name NN (display name), creation_date text NN (`yyyy/MM/dd HH:mm:ss (z)`), case_number, examiner_name, examiner_email, examiner_phone, notes | FK org_id→organizations ON UPDATE/DELETE SET NULL. UNIQUE(case_uid). Indexes `cases_org_id`, `cases_case_uid`. Empty strings are stored as NULL |
| `data_sources` | id, case_id NN, device_id NN, name NN, datasource_obj_id bigint, md5, sha1, sha256 (DEFAULT NULL) | FK case_id. New DBs: UNIQUE(case_id, datasource_obj_id). DBs upgraded through 1.3: UNIQUE(case_id, device_id, name, datasource_obj_id). Indexes `data_sources_name`, `data_sources_object_id` |
| `reference_sets` | id, org_id NN, set_name NN, version NN, known_status int NN, read_only bool NN, type int NN (correlation type id), import_date text NN (ISO `LocalDate`) | UNIQUE(set_name, version). FK org_id. Index `reference_sets_org_id` |
| `reference_file` (only for FILES) | id, reference_set_id, value NN, known_status NN, comment | UNIQUE(reference_set_id, value). Indexes on (value) and (value, known_status) |
| `correlation_types` | id, display_name NN, db_table_name NN, supported int NN, enabled int NN | UNIQUE(display_name, db_table_name) |
| `<db_table_name>_instances` (non-account types) | id, case_id NN, data_source_id NN, value NN, file_path NN, known_status int NN, comment, file_obj_id bigint | UNIQUE(data_source_id, value, file_path). FKs to cases and data_sources. Indexes `_case_id`, `_data_source_id`, `_value`, `_value_known_status`(value, known_status), `_file_obj_id` |
| same, account types (email, phone, and ids ≥1000) | plus `account_id bigint DEFAULT NULL` FK→accounts | same |
| `account_types` | id, type_name NN UNIQUE, display_name NN, correlation_type_id bigint FK | one row per TSK predefined account type except DEVICE |
| `accounts` | id, account_type_id NN FK, account_unique_identifier NN | UNIQUE(account_type_id, account_unique_identifier) |
| `examiners` | id, login_name NN UNIQUE, display_name | |
| `confidence` | id, confidence_id int UNIQUE, description | 1 "Low confidence", 2 "Moderate confidence", 3 "High confidence" |
| `persona_status` | id, status_id UNIQUE, status | 1 Unknown, 2 Active, 3 Merged, 4 Split, 5 Deleted |
| `personas` | id, uuid NN UNIQUE, comment NN, name NN, created_date bigint (epoch ms), modified_date bigint, status_id FK, examiner_id FK | |
| `persona_alias` | id, persona_id, alias NN, justification NN, confidence_id, date_added (ms), examiner_id | |
| `persona_metadata` | id, persona_id, name, value, justification, confidence_id, date_added, examiner_id | UNIQUE(persona_id, name) |
| `persona_accounts` | id, persona_id, account_id, justification, confidence_id, date_added, examiner_id | no uniqueness constraint (duplicates are prevented only in the UI) |

**Known status values** (TSK `FileKnown`): 0 UNKNOWN, 1 KNOWN, 2 BAD (notable).

**Upgrade path**: `upgradeSchema` runs on a connection with foreign keys off and auto-commit off, as one transaction that is rolled back on error.
- If the DB major version is greater than the software's → incompatible: the CR is disabled with the error "please upgrade the application". If the DB version is ≥ the software's, nothing is done.
- **<1.1**: add `reference_sets.known_status`, `read_only` and `type`; insert the default org.
- **<1.2**: add `data_sources.datasource_obj_id` plus an index; insert the SSID/MAC/IMEI/IMSI/ICCID types and create their tables; add `file_obj_id` plus an index to all instance tables; add `data_sources.md5/sha1/sha256`; recreate `db_info` with the creation-version rows.
- **<1.3**: widen the data_sources unique constraint (SQLite rebuilds the table via rename and copy).
- **<1.4**: create `account_types` and `accounts`; create the `*_acct_instances` tables (ids ≥1000) and insert their types; `ALTER … ADD account_id` on email/phone (PostgreSQL also adds an FK `account_id_fk`); fill account_types.
- **<1.5**: persona tables plus confidence/status rows.
- **<1.6**: `installed_programs_instances` and `os_accounts_instances` (account-style template) plus their types.

Finally the version rows are updated.

### Correlation types

Default table (supported=1, enabled=1 for all):

| ID | Display name | db_table_name | Normalization / validation (input is `trim()`med first) |
|---|---|---|---|
| 0 | File MD5 | `file` | lowercase. Must match `^[a-f0-9]{32}$` |
| 1 | Domain | `domain` | Valid per Apache `DomainValidator(allowLocal=true)` → `extractDomain(lower)`. Otherwise it must be an IPv4 (`^((25[0-5]\|2[0-4]\d\|1\d\d\|[1-9]?\d)\.){3}(…)$`) and goes through `extractDomain`. Otherwise reject |
| 2 | Email Address | `email_address` | Apache `EmailValidator(allowLocal=true, allowTld=true)`. Result is `lower().trim()` |
| 3 | Phone Number | `phone_number` | Valid if every char is a digit, a Unicode space-char, `-`, `(`, `)` or a leading `+`, and length ≥5. Normalized: strip `\s+`, then strip `[-()]`. The result must still be ≥5 chars. Not lowercased; `+` is kept |
| 4 | USB Device | `usb_devices` | unchanged |
| 5 | Wireless Network | `wireless_networks` | SSID, rejected if longer than 32 characters; otherwise unchanged (case kept) |
| 6 | MAC Address | `mac_address` | lowercase, remove `[\s-:]`, then must match `^([a-f0-9]{12}\|[a-f0-9]{16})$` |
| 7 | IMEI Number | `imei_number` | remove `[\s-:]`, then `^[0-9]{14,16}$` |
| 8 | IMSI Number | `imsi_number` | remove `[\s-:]`, then `^[0-9]{14,15}$` |
| 9 | ICCID Number | `iccid_number` | lowercase, remove `[\s-:]`, then `^89[f0-9]{17,22}$` |
| 10 | Installed Program | `installed_programs` | unchanged (trim only) |
| 11 | Os Account | `os_accounts` | unchanged (the SID/addr). Account-style table |
| 1000+ | one per TSK `Account.Type.PREDEFINED_ACCOUNT_TYPES` entry except DEVICE, EMAIL and PHONE, in list order: display = account type display name, table = `lower(type_name)+"_acct"`. In TSK order this is likely Credit Card=1000 (`credit_card_acct`), Facebook, Twitter, Instagram, WhatsApp, MessagingApp, Website, IMO, ICQ, LINE, Skype, Telegram, TextNow, Threema, Viber, Xender, Zapya, ShareIt (verify against the TSK version) | trim only (**case preserved**, unlike the `accounts` table) |

- `extractDomain`:
  1. If the value parses as a URL, use its host; otherwise use the raw string.
  2. Strip `.*://` and anything after the first `/`.
  3. Reduce to the registrable domain using the Public Suffix List. The labels are lowercased and the trie is walked from the right. A wildcard rule takes one more label; an exception rule `!x` stops the walk. If no suffix matches, the whole host is returned.
  4. Return "" (which counts as a failure) if the result contains any of ``~`!@#$%^&*()+={}[];:?<>,/`` or a space, or has no `.`. The final check is DomainValidator or IPv4.
- An instance's `file_path` is always lowercased. A value of 256 characters or more is rejected: a single insert throws, a bulk insert skips it with a log entry.
- Display names (`displayName`) can be edited in the DB. `db_table_name` must match `[a-z][a-z0-9_]*`.
- Users can only toggle **enabled** (Manage Correlation Properties dialog: a two-column table "Correlation Properties | Enable" with a warning "There are no correlation properties enabled" if none are checked). The dialog note says "these properties are global and impact all users".
- Only FILES consults the `enabled` flag, in the file ingest module. Other types are always extracted.

**Account normalization** (`accounts.account_unique_identifier`):
- With a known type: PHONE → phone normalizer; EMAIL → email normalizer; anything else → `toLowerCase()`. Blank values are rejected.
- Without a type (persona search): if it looks like a phone, normalize as a phone; otherwise if it is a valid email, normalize as an email; otherwise `lower().trim()`.

### Attribute extraction (which artifacts produce which correlation values)

Common to all: the case, plus the data source (CR row created on demand). `file_path` is `parentPath+name` of the source file, or the source content name if that is not a file or the artifact is TSK_INSTALLED_PROG. `file_obj_id` is the source content id. Known status is UNKNOWN. The comment is empty.

| Source | Attribute(s) → type |
|---|---|
| AbstractFile | MD5 → FILES, only if the file type is supported: CARVED, DERIVED, LOCAL, LAYOUT_FILE, or FS *with the ALLOC flag*. Unalloc/unused blocks, slack and virtual/local dirs are excluded. MD5 must be non-empty and not the empty-data MD5. At most one per file |
| TSK_WEB_BOOKMARK / COOKIE / DOWNLOAD / HISTORY / CACHE | TSK_DOMAIN → DOMAIN (skips `localhost` and `127.0.0.1`) |
| TSK_DEVICE_ATTACHED | TSK_DEVICE_ID → USBID; TSK_MAC_ADDRESS → MAC |
| TSK_WIFI_NETWORK | TSK_SSID → SSID |
| TSK_WIFI_NETWORK_ADAPTER, TSK_BLUETOOTH_PAIRING, TSK_BLUETOOTH_ADAPTER | TSK_MAC_ADDRESS → MAC |
| TSK_DEVICE_INFO | TSK_IMEI, TSK_IMSI, TSK_ICCID |
| TSK_SIM_ATTACHED | TSK_IMSI, TSK_ICCID |
| TSK_WEB_FORM_ADDRESS | TSK_PHONE_NUMBER → PHONE; TSK_EMAIL → EMAIL |
| TSK_ACCOUNT | TSK_ACCOUNT_TYPE matched case-insensitively against predefined types (not DEVICE) → CR account type → its correlation type. The TSK_ID value becomes the instance and `getOrCreateAccount` sets account_id |
| TSK_INSTALLED_PROG | TSK_PATH if non-empty, otherwise TSK_PROG_NAME → INSTALLED_PROGS |
| TSK_CONTACT / CALLLOG / MESSAGE | **search only** (never saved): the first of PHONE_NUMBER, PHONE_NUMBER_FROM, PHONE_NUMBER_TO → PHONE |
| OsAccount (per data source) | addr → OS_ACCOUNT unless it is S-1-5-18, S-1-5-19 or S-1-5-20. Known status is **KNOWN**; file_path = data source name |
| AnalysisResult | never saved. For search: an INTERESTING_ITEM or INTERESTING_ARTIFACT_HIT recurses into its TSK_ASSOCIATED_ARTIFACT; a KEYWORD_HIT whose set name is "Email Addresses" gives TSK_KEYWORD → EMAIL. Otherwise it recurses into its parent (file, artifact, or OsAccount instance in the same data source) |

Each value is normalized while being created. On a normalization failure the item is skipped; for a data artifact the list built so far is returned, so later attributes are lost.

### Ingest module "Central Repository"

It is both a file module and a data-artifact module. Job settings (all per job, panel "Ingest Settings"):

| Setting (UI label) | Default |
|---|---|
| Flag items previously tagged as notable | **false** |
| Flag devices and users previously seen in other cases | **false** |
| Save items to the Central Repository | **true** |
| Flag apps and domains not seen in other cases | **false** |

The module has a global settings panel (the CR options).

- **Startup**:
  - Fails if the CR is disabled ("Central repository required, but not enabled") or inaccessible.
  - Fails for a multi-user case with SQLite.
  - The file module ensures the CR case and data source rows exist; first instance per job, ref-counted.
- **File module `process`**:
  1. Skip if neither notable-flagging nor saving is on, if FILES is disabled, if the file is KNOWN (NSRL), if the file type is unsupported, or if there is no or empty MD5.
  2. With notable-flagging on: `SELECT DISTINCT case_name FROM file_instances JOIN cases WHERE value=? AND known_status=2`. This includes the **current case**. If any rows come back, a Previously Notable result is created.
  3. With saving on: the instance goes to the bulk queue.
  4. The last module instance of the job commits the bulk queue at shutdown.
- **Data-artifact module**:
  - For each correlation attribute in `makeCorrAttrsToSave`, deduplicated per job by `toString()` (case uuid + device + accountId + path + type + value + status + comment):
    - Previous occurrences = all instances with the same type and value whose case UUID differs from the current one (`SELECT … WHERE value IN ('v')` joined to cases, orgs and data_sources).
    - **Notable** (flag 1): the cases among those with known_status BAD → TSK_PREVIOUSLY_NOTABLE.
    - **Previously seen** (flag 2, only for USBID, ICCID, IMEI, IMSI, MAC, EMAIL, PHONE): distinct case display names → TSK_PREVIOUSLY_SEEN. Score by number of previous cases: ≤10 → LIKELY_NOTABLE; 11–20 → NONE; **>20 → no result**.
    - **Unseen** (flag 4, only for INSTALLED_PROGS and DOMAIN): zero previous occurrences → TSK_PREVIOUSLY_UNSEEN, score LIKELY_NOTABLE, justification "Previously seen in zero cases".
    - Then, if saving is on, the instance is queued in bulk.
  - At shutdown:
    1. OS accounts of the data source are processed if saving or flag 2 is on: their attributes are queued and flag 2 creates a Previously Seen result on the OsAccount.
    2. The bulk queue is committed.
    3. For Image data sources, the image MD5/SHA1/SHA256 are synced into `data_sources` when they differ (null becomes "").
- **Result attributes** (module name "Central Repository"):
  - Notable: TSK_SET_NAME "Previously Tagged As Notable (Central Repository)", TSK_CORRELATION_TYPE (display name), TSK_CORRELATION_VALUE, TSK_OTHER_CASES (case names joined with ","). Score NOTABLE. Justification "Previously marked as notable in cases {list}". It also posts an ingest inbox message "Notable: {name}" with an HTML table (File Name, MD5 Hash, Previous Cases; or Artifact Type, Notable Attribute, Previous Cases).
  - Previously Seen: set name "Previously Seen (Central Repository)", the same attributes, justification "Previously seen in cases {list}".
  - Unseen: type and value only.
  - A result is not created if an identical one already exists (`artifactExists` with the same attributes).
- **Bulk insert**: an in-memory map of table → list, guarded by a global lock. It flushes once `bulkArtifactsCount ≥ threshold` (default 1000) and at module shutdown. SQL per table: `INSERT INTO t (case_id, data_source_id, value, file_path, known_status, comment, file_obj_id) VALUES ((SELECT id FROM cases WHERE case_uid=? LIMIT 1), (SELECT id FROM data_sources WHERE datasource_obj_id=? AND case_id=? LIMIT 1), ?,?,?,?,?) [ON CONFLICT DO NOTHING]`, using JDBC batching. **`account_id` is not written by the bulk path.**

### Event listeners (case-scoped, single-threaded executor `CR-Case-Event-Listener-%d`, LOCAL events only)

- **CURRENT_CASE opened**: if the case is not in the CR, insert it: uid = case name, org = null, display name, created date, number, examiner, email, phone, notes.
- **DATA_SOURCE_ADDED**: create the `data_sources` row if missing. device_id comes from the case DB; image hashes are included if present.
- **DATA_SOURCE_NAME_CHANGED**: `UPDATE data_sources SET name=?`.
- **CONTENT_TAG_ADDED/DELETED**:
  - The tagged content must be an AbstractFile. If any remaining content tag on it is "notable", set known_status BAD, otherwise UNKNOWN.
  - Notable means a tag-name type BAD. By default only "Notable Item" is BAD; "Bookmark" and "Follow Up" are SUSPICIOUS. User tag definitions marked notable also count.
  - The status is applied to its MD5 instance.
- **BLACKBOARD_ARTIFACT_TAG_ADDED/DELETED**: same rule, skipped if the source content is a KNOWN file. It is applied to every search attribute of the artifact.
- **`setAttributeInstanceKnownStatus`**: find the instance by (case_id, data_source_id, value, file_path). If found, `UPDATE known_status`. If not found, create the data source if needed and **insert a new instance** with that status. So tagging alone puts items into the CR even if ingest never saved them.
- **TAG_DEFINITION_CHANGED** (the notable flag of a tag name changed): every artifact and content tag with that name is re-applied with the tag's new known status. When the new type is UNKNOWN or SUSPICIOUS, items that still carry another BAD tag are skipped.
- **Case details edits** (case properties panel) → `UPDATE cases SET org_id, case_name, creation_date, case_number, examiner_*, notes WHERE case_uid=?`.
- **New Case wizard**: the user picks an organization (a combo of CR orgs with "Not Specified" as default, plus "New organization" which opens the add-org dialog; the POC name/email/phone are shown read-only). After creation the CR case is created and the org assigned.

### Other Occurrences content viewer

- **Visibility and priority**: a level-one-priority viewer, position 10, title "Other Occurrences". Supported when the CR is enabled and the node has an OsAccount, a blackboard artifact, an artifact tag, or an AbstractFile with an MD5.
- **Loading**:
  - A background worker, cancelled whenever a new node is selected, builds the search attributes from the node content (file, artifact, analysis result, content/artifact tag, or OsAccount).
  - For each attribute: `getArtifactInstancesByTypeValue`. The selected item itself is excluded: same case UUID, same data source name, same device id and same file_obj_id.
  - Results are deduplicated by **UniquePathKey** (device id, lowercased path, type display name, case UUID).
  - Summary label: "Found %d instances in %d cases and %d data sources." Cases and data sources count only *other* cases. "Central Repository Starting Date:" shows the earliest `cases.creation_date` (parsed `yyyy/MM/dd HH:mm:ss`), or "Not Available."
- **Layout**: three single-column, sortable, multi-select tables chained left to right: **Case** (case display name) → **Data Source Name** (key case uuid + device id) → **File Name** (basename of path; rows grouped by caseName + dsName + deviceId + path + caseUUID). The first row of each table is auto-selected.
- **Placeholder rows**: "Item has no attributes with which to search." / "No results found." / "Loading results" / "No Data."
- **Details pane** (OccurrencePanel), depending on the selection:
  - One file selected: "Common Properties" per matching instance (Type, Value, Known Status, Comment), then File Details (File Path), Data Source Details (Name), Case Details (Name, Created Date).
  - One data source selected: case plus data source.
  - One case selected: case only.
- **Context menu**:
  - "Show Case Details".
  - "Export all Other Occurrences to CSV": file name defaults to `YYYYMMDDhhmmss_other_data_sources.csv`, `.csv` is enforced. Header row: `"Case","Data Source","Matched Attribute","Attribute Value","Known","Path","Comment"`. Every field is wrapped in `"` with no escaping. Rows are the deduplicated instances of all attributes.
  - "Show Frequency": for each attribute, "{pct}% of data sources have {value} (type: {type})". pct = distinct (case, data_source) pairs having the value ÷ `count(*) FROM data_sources` × 100, truncated.
- **Single-type mode** (`populateTableForOneType`, used by other features): it excludes the current item by case + data source + device + path equality instead of object id.

### O / C columns and Annotations

- **O column** (shown only when the CR is enabled):
  - SQL: `count(DISTINCT case_id)` over instances with the same value, **excluding the row with the same (file_obj_id, case_id, data_source_id)**. The current case is counted if the value appears in other files of it.
  - Description: "There were {n} datasource(s) found with occurrences of the MD5 correlation value". It is −1 (shown blank) when there is no value, or "Hash lookup had not been run…". An artifact with more than one correlation attribute shows "Multiple different correlation properties exist for this result" and no count.
- **C column**: `SELECT EXISTS(SELECT 1 FROM <t>_instances WHERE value=? AND comment<>'' LIMIT 1)` for any attribute of the item, *in any case*. Combined with tag comments it gives one of NO_COMMENT, TAG_NO_COMMENT, TAG_COMMENT, CR_COMMENT or CR_AND_TAG_COMMENTS.
- **Annotations viewer**, section "Central Repository Comments": all instances in all cases with the same type and value and a non-blank comment. For files only the MD5 is used. Each shows Case, Comment, Path.
- **"Add/Edit Central Repository Comment"** (file context menu):
  - Shown only for exactly one supported file. The label has variants "(Empty File)" and "(No MD5 Hash)", which leave the action disabled.
  - The current instance is looked up by (case, file_obj_id), then by (case, ds, md5, path). If none exists it is created with `addArtifactInstance`.
  - The dialog limits the comment to **500 characters** and truncates longer legacy comments.
  - Save: `UPDATE <t> SET comment=? WHERE case_id=? AND data_source_id=? AND value=? AND file_path=?`, then a `CR_COMMENT_CHANGED` case event refreshes the C column.

### Organizations, cases, data sources management

- **Manage Organizations**:
  - List plus details (Organization Name, POC Name, POC Email, POC Phone); New / Edit / Delete buttons.
  - Add/edit dialog: Organization Name is required ("Organization Name is required."). Names are unique.
  - Delete asks for confirmation, then checks `(SELECT count(*) FROM cases WHERE org_id=?) + (SELECT count(*) FROM reference_sets WHERE org_id=?)`. If the result is >0: "Unable to delete selected organization".
- **Manage Cases**: **read-only**.
  - A cases table (Case Name, Creation Date) and, for the selected case, its data sources table (Data Source Name, Device ID).
  - Case info (Organization, Case Number, Examiner Name/Email/Phone, Notes) is read-only.
  - There is **no UI to delete a case or data source from the CR**, and no API for it; postmortem should add one with cascade.
- `data_sources` md5/sha1/sha256 are kept in sync at ingest shutdown.

### Reference sets (CR-backed hash sets)

- These are created from the Hash Lookup options ("Create Hash Set" or "Import Hash Set" with storage type *Central Repository*). The radio button is disabled when the CR is off.
- Fields: name, version, organization (default "Not Specified", or the first org), type Known or Notable (known_status 1 or 2), read-only flag, plus the local-only options search-during-ingest and send-ingest-messages.
- **Create**: version is always "". The set is editable (read_only=false). Duplicate name check: `referenceSetExists(name, "")`.
- **Import**:
  - "Read only" is checked by default. When read-only, a **version is required**; editable sets get version "".
  - Duplicate check is on (name, version).
  - Supported formats by extension: `.idx`/`.txt` (TSK index / md5 text), `.hash`/`.Hash` (EnCase), `.kdb` (TSK SQLite), `.hsh` (HashKeeper).
  - The hashes are normalized as MD5 and lose the set on failure.
  - Inserts run in transactional batches of **10 000** with progress = processed ÷ expected, capped at 99 % until done.
  - Cancel or failure deletes the partial set (entries, then the set row).
- **Lookup**: `SELECT count(*) FROM reference_file WHERE value=? AND reference_set_id=?` (quick). Full lookup returns value plus comment.
- **Add hashes** (editable sets only): single insert or bulk.
- **Validation**: the set id must still exist with the same name and version.
- **Delete**: `DELETE FROM reference_file WHERE reference_set_id=?`, then `DELETE FROM reference_sets WHERE id=?`. Cancelling the options panel deletes sets created during that session.
- **Loading on startup**: all sets of type FILES become hash DBs with searchDuringIngest=false and sendIngestMessages = (type is Notable), unless they are overridden by saved settings.
- `isArtifactKnownBadByReference(type, value)`: notable in *any* reference set (FILES only).

### Personas

- **Window** (Tools → Personas; enabled only when the CR is enabled):
  - Intro text, then "Filter personas by name or account" checkbox with a search field and radio Name / Account, plus "Show".
  - Results table with columns ID and Name. Buttons: New Persona, Edit Persona, Delete Persona, Create Account. A details panel in VIEW mode shows the selection.
  - Search:
    - Unfiltered: `getPersonaByName("")`.
    - Name: `LOWER(name) LIKE LOWER('%x%') ESCAPE '!'` (escapes `! % _`).
    - Account: personas having an account whose identifier is LIKE `%x%`.
    - All searches exclude status DELETED.
- **Details dialog**: modes CREATE, EDIT, VIEW.
  - Header fields: Name (default "Unnamed"), Comment, "Created by" (examiner login), "Created on" (`yyyy/MM/dd HH:mm:ss`).
  - Tables:
    - Accounts: Type, Identifier.
    - Metadata: Name, Value.
    - Aliases: Alias.
    - "Cases found in": Case.
  - Each of the Accounts, Metadata and Aliases tables has Add / Edit / Delete buttons. Edit changes only confidence and justification.
  - Validation on OK: at least **one account**, a non-blank comment and a non-blank name.
  - Changes are staged and applied on OK without a transaction. Create = insert the persona plus its first account, then the remaining accounts, metadata and aliases.
- **Sub-dialogs**:
  - Add Account: Type combo (CR account types), Identifier, Confidence (Low/Moderate/High), Justification. The account must **already exist** in `accounts` ("Account not found for given identifier and type."); duplicates are refused.
  - Create Account: Type and Identifier → `getOrCreateAccount`; invalid identifiers are rejected.
  - Alias: Alias, Confidence, Justification. It must be non-empty and not a duplicate.
  - Metadata: Name, Value, Confidence, Justification. Name and value must be non-empty; the name must be unique within the persona.
- **Data semantics**:
  - The examiner is the OS user name (`user.name`), auto-inserted into `examiners`.
  - `created_date` and `modified_date` are set to now on create. **modified_date is never updated afterwards.**
  - Persona delete is soft (`status_id=5`). Removing an alias, metadata or account is a hard `DELETE`.
  - Persona cases and data sources come from `SELECT DISTINCT case_id FROM <acct type table> WHERE account_id=?` for each linked account. This only finds instances that have an `account_id` set.
  - `getPersonasForCase` / `ForDataSource`: one query per account type, joining instance → persona_accounts → personas → examiners.
- **CVT and artifact viewers**:
  - The CVT keeps a persona cache (`LoadingCache<Account, List<PersonaAccount>>`, expires 5 minutes after access). Lookup is by `LOWER(identifier)=LOWER(?) AND type_name=?`, excluding deleted personas.
  - Contact, call-log and message viewers show "Persona: name (1 of N)" with a **View** button, or "Unknown" with a **Create** button. Create pre-fills the name with the identifier and adds every CR account (across all types) matching the identifier, with justification "Account found in Call Log artifact" and confidence HIGH.

### Autopsy problems & postmortem ideas (Central Repository)

- **SQL built by string concatenation**: `WHERE value IN ('v1','v2')` is assembled from normalized values without escaping, so an apostrophe in an SSID, program path or account ID breaks the query. Case IDs are quoted as strings. Postmortem should always bind parameters (or use `= ANY($1)` / `IN (SELECT value FROM json_each(?))`).
- **Bulk insert drops `account_id`**: ingest-saved account instances have a NULL account_id, so persona→case/data-source lookups miss them. Write account_id in all paths.
- **SQL bug**: `getCountCasesWithOtherInstances` has a malformed fallback statement (missing `)`), used when obj id or ds id is missing. `getCorrelationAttributeInstance` casts obj id to int.
- **Inconsistent normalization**: account-type instance values are case-sensitive while `accounts` is lowercased. USB IDs and SSIDs are not normalized at all. Domain normalization depends on a bundled PSL snapshot. Define one Rust `normalize(type, &str) -> Result<String>` used everywhere, with property tests, the `publicsuffix` and `addr`/email crates, and a PSL version stored in `db_info`.
- **One table per type** (~30 instance tables plus a reference table): every cross-type feature (O column, frequency, persona cases, reset) loops over tables. Use a single `instances(type_id, value, case_id, ds_id, path, known, comment, obj_id, account_id)` table with a `(type_id, value)` index. Use PostgreSQL LIST partitions by type_id if needed. Keep reference hashes in `reference_values(set_id, type_id, value)`.
- **Schema divergence**: the data_sources unique key differs between fresh and upgraded DBs; FK cascade rules are SET NULL on NOT NULL columns (meaningless); persona_accounts has no unique key. Use one migration set (e.g. refinery/sqlx migrations) with numbered steps and real `ON DELETE CASCADE`.
- **SQLite safety and concurrency**: `synchronous=OFF` risks corruption on crash. Locking is an in-process RW lock only, so two Autopsy instances on one file corrupt or lock it. Pool initialSize=50 is wasteful. Use WAL + `synchronous=NORMAL`, a single writer task with a channel, `BEGIN IMMEDIATE` batches, and `busy_timeout`. Detect other writers via a lock file.
- **Ingest throughput**: one global synchronized bulk map, auto-commit per batch, and a per-row subselect for case and data source ids. Resolve ids once per job. For PostgreSQL, batch with `COPY` into a temp table plus `INSERT … ON CONFLICT DO NOTHING`; for SQLite, use prepared statements in one transaction per ~10 k rows.
- **N+1 lookups during ingest**: every artifact attribute fetches *all* instances with joins just to test existence or count cases. Use `EXISTS` / `COUNT(DISTINCT case_id)` queries and a per-job LRU of already-checked values. Previously-seen thresholds (10 and 20 cases) should be configurable.
- **Notable check counts the current case**: the file module's "previously notable" query includes the current case, unlike the artifact path. Results list case *display names*, which are not unique; prefer case UID plus display name.
- **Tag → known_status**: tagging inserts a new instance when none exists (silently writes to the CR even with "save items" off). Tag-definition changes iterate over all tags synchronously. Make this explicit and batch it.
- **Other Occurrences UI**: every case or data-source selection change re-runs all CR queries (SelectionWorker), and "earliest case date" parses every case row on each node selection. Frequency is computed on the UI thread. Postmortem should fetch once per node into an in-memory model, filter locally, show the case → data source → file trees as one virtualized egui table with filter chips, and offer a popout window.
- **CSV export** has no quote escaping and uses platform line separators. Use the `csv` crate with RFC 4180 output, and include device ID, case UID and timestamps.
- **No case or data-source deletion, and no merge or import between CR databases**: add "Delete case from CR" (cascade instances), "Rename/merge organizations", and "Migrate SQLite → PostgreSQL" (stream copy with id remapping), since Autopsy explicitly does not transfer data on platform switch.
- **Stale caches in multi-user setups**: static 5-minute case/data-source caches and never-expiring type caches mean that enabling or disabling a type on one workstation is not seen elsewhere. Use PostgreSQL `LISTEN/NOTIFY` (or a `db_info` generation counter) for invalidation. Take the multi-user lock only for migrations, as Autopsy does, but use a PostgreSQL advisory lock instead of ZooKeeper.
- **Password storage**: PBEWithMD5AndDES with a hard-coded key is obfuscation, not encryption. Use the OS keychain (`keyring` crate) with a fallback to an encrypted file.
- **Personas**: create/edit is not transactional (partial personas on failure), modified_date is never updated, and there is no audit or merge/split even though the statuses exist. Wrap edits in one transaction, update modified_date, implement Merge/Split with history, and allow "Add Account" to create missing accounts inline.
- **Comments**: the 500-character cap is UI-only, and the C column reports comments from *any* case. Show the comment source in a tooltip, and allow per-case filtering.
- **Hash-set import**: progress uses expected counts from the parser, which is wrong for `.txt` files. The duplicate check is by name+version only. Stream-parse in Rust with byte-based progress, validate MD5s up front, and support NSRL RDS and SHA-1/SHA-256 reference values (add types instead of FILES only).
- **Upgrade UX**: on an incompatible or failed upgrade the CR is silently disabled (flag `disabledDueToFailure`). Show a persistent banner with a retry action and a read-only mode, instead of disabling it.

## Reporting

Source: `Core/.../report/` (`infrastructure/`, `modules/*`, `uisnapshot/`), plus `KeywordSearch/.../ExtractAllTermsReport.java`. Case-level reports live under `<case dir>/Reports/`. In the case DB, the `reports` table holds one row per generated report: path, source module name, report name, created time.

### Module taxonomy

Autopsy has four module kinds. The infrastructure drives each kind differently.

| Kind | Contract | Who iterates the data | Modules |
|---|---|---|---|
| **TableReportModule** | `startReport(dir)`, `startDataType(name, desc)`, `startSet/endSet`, `addSetIndex(list)`, `addSetElement(name)`, `startTable(headers)`, `addRow(cells)`, `endTable`, `endDataType`, `endReport`, `dateToString` | The shared `TableReportGenerator` | HTML Report, Excel Report |
| **FileReportModule** | `startReport`, `startTable(List<FileReportDataTypes>)`, `addRow(AbstractFile, cols)`, `endTable`, `endReport` | `ReportGenerator.generateFileListReport` | Files – Text (TSV/CSV) |
| **GeneralReportModule** | `generateReport(GeneralReportSettings{reportDirectoryPath, selectedDataSources}, progress)`; `supportsDataSourceSelection()` | The module itself | TSK Body File, Google Earth KML, CASE-UCO, Save Tagged Hashes, Data Source Summary Report (XLSX), Extract Unique Words (KeywordSearch) |
| **PortableCaseReportModule** (special case) | `generateReport(dir, PortableCaseReportModuleSettings, progress)` | The module itself | Portable Case |

- Every module has `getName()`, `getDescription()`, `getRelativeFilePath()` (used to build the clickable link in the progress panel: `null` means no link, `""` means link to the report folder), and an optional configuration panel with serializable `ReportModuleSettings` (`getDefaultConfiguration`/`getConfiguration`/`setConfiguration`).
- The STIX module was **removed**. `ReportingConfigLoader.DELETED_REPORT_MODULES` silently drops `org.sleuthkit.autopsy.report.modules.stix.STIXReportModule` from old saved configs. postmortem should do the same and not implement STIX.
- Modules are discovered via NetBeans Lookup (`ReportModuleLoader`). A module whose name is empty triggers the error popup "Encountered an invalid Report Module (X)" and is dropped.

### Report wizard (Tools > Generate Report, plus a toolbar button on the Case toolbar)

- Enabled only when a case is open and the GUI is running. Implemented as a NetBeans `WizardDescriptor`, titled "Generate Report".
- **Page 1, "Select and Configure Report Modules"**: a split pane.
  - Left: a radio-style list of all modules, with HTML swapped to the first position. The initially selected module is whichever was enabled in the last saved config, else index 0 (HTML).
  - Right: the module description, with the module's configuration panel beneath it.
  - Only ONE module can be run per wizard invocation.
  - Selecting a GeneralReportModule that does not support data-source selection enables **Finish** and disables **Next**. Every other module enables Next.
- **Page 2, "Select which data source(s) to include"**: a check-box list of data sources, all checked by default.
  - Shown for Table modules, File modules, and General modules with `supportsDataSourceSelection()==true`.
  - Proceeding with nothing checked shows the confirmation "Are you sure you want to proceed with no selections?".
  - The selection is stored as `List<Long>` data-source object ids. `null` means "all".
- **Page 3a (Table modules), "Configure Artifact Reports"**: radio buttons:
  - "All Results" (default). "Choose Result Types..." opens `ArtifactSelectionDialog`, a checkbox list of `getArtifactTypesInUse()` with all types checked by default, plus Select All / Deselect All.
  - "All Tagged Results".
  - "Specific Tagged Results": enables a checkbox list of tag names in use. Notable tag names get the suffix " (Notable)". Select All / Deselect All are available, and Finish is enabled only when at least one tag is checked.
  - Persisted as `TableReportSettings{artifactSelections: Map<Type,Boolean>, tagSelections: Map<String,Boolean>, option: ALL_RESULTS|ALL_TAGGED_RESULTS|SPECIFIC_TAGGED_RESULTS, useStoredTagsAndArtifactsLists, selectedDataSources}`.
- **Page 3b (File modules)**: "Select items to include in File Report:", a checkbox list of the 13 `FileReportDataTypes`, all checked by default, plus Select All / Deselect All. Finish is enabled only when at least one is checked.
- **Page 3c (Portable Case)**:
  - Two side-by-side lists:
    - Tags: "All Tagged Results" checkbox (default on), or a specific tag-name list.
    - Interesting-item sets: "All Interesting Items" checkbox (default on), or a specific list of set names, each with its count from `blackboard_attributes` grouped by `TSK_SET_NAME` for artifact types 3, 40 and 51 (interesting file/artifact/item).
  - "Package case into an archive:" checkbox (Windows only; otherwise disabled with the label "Windows only"), plus a chunk-size combo:
    - "Do not split"
    - "Split into 100 MB chunks" (100m)
    - "Split into 700 MB chunks (CD)" (700m)
    - "Split into 1 GB chunks" (1000m)
    - "Split into 4.5 GB chunks (DVD)" (4500m)
  - "Include application in folder (may add up to 1GB of files)". When it is on, the chunk combo is disabled.
  - Defaults: compress = false, chunk = NONE, all tags = true, all sets = true, include app = false.
- **On Finish**:
  - The config is saved as the reporting configuration named **"ReportAction"**. Data-source selections are copied into the General, File and Table settings.
  - A modal **"Report Generation Progress"** dialog opens (`ReportGenerationPanel`). Generation runs on a SwingWorker.
- **Saved configurations ("report profiles")**:
  - Stored under `<user config>/ReportingConfigs/<name>/` as Java-serialized files: `TableReportSettings.settings`, `FileReportSettings.settings`, `GeneralReportSettings.settings`, `ModuleConfigs.settings` (map of module-class → `{enabled, settings}`).
  - Names are validated: no path separators, not "." or "..", and path traversal is rejected.
  - The command-line ingest settings panel lets users create named profiles (letters, digits and underscore only) by running the same wizard with `runReports=false`. Headless/command-line runs generate the profile via `new ReportGenerator(profileName, progressIndicator).generateReports()`.
  - postmortem: use JSON/TOML instead of Java serialization, and keep the same fields.
- **Output folder**: `<case>/Reports/<case display name> <module name> <MM-dd-yyyy-HH-mm-ss>/`. Each module writes inside this folder.
- **Progress panel** (one row per module):
  - Module name, a clickable path (opens the file, or the parent folder if that fails), a progress bar and a status label.
  - Statuses: QUEUING, RUNNING, COMPLETE, CANCELED, ERROR.
  - Per-row Cancel. Closing the dialog asks "Are you sure you'd like to close the dialog? All reports will be canceled."
  - Errors from `TableReportGenerator.errorList` are concatenated into the status label.

### Tree node "Reports"

- A Reports node in the data explorer tree lists the `reports` table rows. Columns: Source Module Name, Report Name, Created Time, Report File Path.
- Double-click or the Open action opens the file externally.
- **Delete Report(s)** asks for confirmation ("The report(s) will remain on disk.") and removes only the DB row.

### TableReportGenerator (shared by HTML and Excel)

Order of output:
1. One data type per artifact type.
2. "Tagged Files".
3. "Tagged Results".
4. Thumbnails (HTML only).

Progress maximum is `#artifactTypes + 2`. Cancellation is checked between types and per row.

- **Artifact types reported**:
  - When `useStoredTagsAndArtifactsLists`: the user's selection.
  - Otherwise: all artifact types except `TSK_GEN_INFO`, `TSK_TOOL_OUTPUT`, `TSK_ASSOCIATED_OBJECT` and `TSK_TL_EVENT`.
- **Tag filter**:
  - The set of tag display names, with notable ones suffixed " (Notable)".
  - Empty means no filter ("All Results").
  - "All tagged results" means the set of all tag names in use, so only tagged items pass.
  - An item passes when the intersection of its tag names with the filter is non-empty.
  - When a filter is active, the per-type comment reads "Contains results that were tagged with one of the following: a, b".
- **Data-source filter**: an item is skipped if its content's data source is not in the selection. Content with no data source is always included.
- **Per-type table**:
  - Fetch `blackboard.getArtifacts([type], dsIds)`, then each artifact's tags and attributes. This is an N+1 pattern.
  - Columns are determined per type (table below). For types not in the table, the columns are every attribute type present on any artifact of that type, sorted by display name, followed by "Source File" and "Tags".
  - For known types, the fixed columns come first. Any remaining attribute types present on the artifacts are appended in display-name order. Then "Source File" (for the listed types) and "Tags".
  - Rows are sorted by lexicographic comparison of the rendered row strings, then by artifact id.
  - Cell rendering: DATETIME attributes use `TimeZoneUtils.getFormattedTime(epoch)`, which follows the user's display-time-zone preference (format `yyyy-MM-dd HH:mm:ss z`). Other attributes use `getDisplayString()`. Missing values become "".
  - Rows with no data abort the table, which is a bug.
- **TSK_ACCOUNT**: prepends a "Review Status" column and hides `TSK_ACCOUNT_TYPE`, `TSK_ASSOCIATED_ARTIFACT`, `TSK_SET_NAME` and `TSK_KEYWORD_SEARCH_DOCUMENT_ID`. Grouping by account type is commented out. **TSK_WEB_CACHE** hides `TSK_PATH_ID`.
- **Special: Keyword hits**:
  1. Count "ad hoc" hits: keyword-hit artifacts without `TSK_SET_NAME`.
  2. Query the distinct list names (union '' when ad hoc hits exist, ordered by list; PostgreSQL uses `convert_to(list,'SQL_ASCII')` ordering). The empty name is displayed as "User Searches".
  3. `addSetIndex(lists)`.
  4. A single UNION query of list hits and ad hoc hits, joining `tsk_files` for name/parent_path, ordered by list, keyword, parent_path, name, preview.
  5. For each new list: `startSet(list)`. For each new keyword: `addSetElement(keyword)` plus a table with columns **Preview | Source File | Tags**.
- **Special: Hash set hits**: grouped per set name, one table per set with columns **File | Size | Tags**.
- **Special: Extension mismatch**: columns File | Extension | MIME type | Path | Tags.
- **Special: Interesting file/item**: a 7-slot row: Set Name, Rule(category), Path, Comment, Description, Associated Artifact, Program. The tags column is the union of the artifact's tags and the file's content tags.
- **Tagged Files** table:
  - Columns: Tag | File (unique path) | Comment | User Name | Modified Time | Changed Time | Accessed Time | Created Time | Size (Bytes) | Hash (MD5).
  - Any content tag on an image file adds that file to the thumbnail set.
- **Tagged Results** table: Result Type | Tag | Comment | Source File (name) | User Name.
- **Thumbnail candidates**: files that are not directories and not unallocated/unused blocks, for which `ImageUtils.thumbnailSupported` is true.

Fixed column order per known artifact type (Source File and Tags are appended as described above):

| Artifact | Columns |
|---|---|
| WEB_BOOKMARK | URL, Title, Date Created, Program |
| WEB_COOKIE | URL, Date/Time, Name, Value, Program |
| WEB_HISTORY | URL, Date Accessed, Referrer, Title, Program, URL Domain |
| WEB_DOWNLOAD | Destination, Source URL, Date Accessed, Program |
| RECENT_OBJECT | Path, Date/Time |
| INSTALLED_PROG | Program Name, Install Date/Time |
| DEVICE_ATTACHED | Device Make, Device Model, Device ID, Date/Time |
| WEB_SEARCH_QUERY | Text, Domain, Date Accessed, Program Name |
| METADATA_EXIF | Date Taken, Device Manufacturer, Device Model, Latitude, Longitude, Altitude |
| CONTACT | Person Name, Phone Number, Phone (Home), Phone (Office), Phone (Mobile), Email |
| MESSAGE | Message Type, Direction, Read Status, Date/Time, From Phone, From Email, To Phone, To Email, Subject, Text |
| CALLLOG | Person Name, From Phone, To Phone, Date/Time, Direction |
| CALENDAR_ENTRY | Calendar Entry Type, Description, Start Date/Time, End Date/Time, Location |
| SPEED_DIAL_ENTRY | Short Cut, Person Name, Phone Number |
| BLUETOOTH_PAIRING | Device Name, Device Address, Date/Time |
| GPS_TRACKPOINT | Latitude, Longitude, Date/Time |
| GPS_BOOKMARK / LAST_KNOWN_LOCATION / GPS_SEARCH | Latitude, Longitude, Altitude, Name, Location Address, Date/Time |
| GPS_ROUTE | Category, Date/Time, End Lat, End Lon, Start Lat, Start Lon, Name, Location, Program |
| SERVICE_ACCOUNT | Category, User ID, Password, Person Name, App Name, URL, App Path, Description, ReplyTo Address, Mail Server |
| ENCRYPTION_SUSPECTED | Name |
| OS_INFO | processorArchitecture, osName, osInstallDate |
| EMAIL_MSG | E-Mail To, E-Mail From, Subject, Date Sent, Date Received, Path, E-Mail CC, E-Mail BCC, Message ID |
| INTERESTING_ARTIFACT_HIT | Set Name, Associated Artifact, Program |
| INTERESTING_ITEM | Set Name, Associated Artifact, Program, Rule, Path, Comment, Description |
| PROG_RUN | Program, Associated Artifact, Date/Time, Count |
| OS_ACCOUNT | User Name, User ID |
| REMOTE_DRIVE | Local path, Remote path |

### HTML Report (`HTMLReport`, output `report.html`)

- **Layout**:
  - `report.html` is a frameset (`cols="350px,*"`): `content/nav.html` on the left and `content/summary.html` on the right. It has a noframes fallback.
  - `content/index.css` and favicon.
  - One page per data type: `content/<escaped name with spaces→_>.html`.
  - `content/thumbs/` holds thumbnails.
  - `content/<tag name>/<name>_<objId>.<ext>` holds copies of tagged files. `content/original/` and `content/thumbs_fullsize/` hold thumbnail originals.
  - Logos and icons are also copied.
- `report.html` is registered in the case `reports` table with source module "HTML Report".
- **Nav page**: "Report Navigation" with a "Case Summary" link, then every data type in alphabetical order (TreeMap) with its row count, e.g. "Web History (123)". Each entry has a per-type 16 px icon (≈50 icon mappings; unknown types get a star).
- **Summary page**:
  - Title from branding (default "Autopsy Forensic Report"), plus a red "Warning, this report was run before ingest services completed!" when ingest is running.
  - "HTML Report Generated on yyyy/MM/dd HH:mm:ss".
  - Case details: Case, Case Number, Number of data sources, Notes, Examiner. The agency logo appears on the left if one is set.
  - Image Information: per data source, the name, plus Timezone and every image path for Image sources.
  - Software Information: Autopsy version, plus every distinct ingest module ever run with its version.
  - Ingest History: "Job N:" with Data Source, Status and the list of enabled modules.
  - Generator logo, branding footer (default "Powered by Autopsy Open Source Digital Forensics Platform - www.sleuthkit.org"), and the optional custom page header/footer.
- **Module config**: free-text **Header** and **Footer** (default ""), HTML-escaped and rendered centered at the top and bottom of every page.
- **Data type pages**:
  - A blue header bar with the type name, the optional bold description, then tables.
  - Keyword pages get a `<ul>` index of lists with anchors, an `<h1>` per list, an `<h4>` per keyword and a table per keyword.
  - All cells are HTML-escaped except thumbnail cells.
  - CSS: `nowrap` cells, alternating row shading, `min-width:125px`.
- **Tagged Files page**:
  - The file name cell links (`target=_top`) to a locally extracted copy saved under `content/<tag name>/`.
  - Directories and unallocated blocks are not copied.
  - The copy is skipped if it already exists (multiple tags with the same name).
  - The size column is right-aligned.
- **Thumbnails ("Tagged Images")**:
  - 5 columns per row, max **1000 thumbnails per page**. Overflow pages are named "Tagged Images - N".
  - Each cell links to the original copy and shows the file name and "Tags: …".
  - When content-viewer image region tags exist (`ImageTagRegion`), it writes a PNG of the full image with the regions drawn plus a thumbnail with them drawn, and adds a "View Original" link.
  - Thumbnails use `ImageUtils.getThumbnail(file, ICON_SIZE_MEDIUM)`.

### Excel Report (`Excel.xlsx`)

- Apache POI XSSF. The first sheet is **"Summary"**: Case Name, Case Number (if any), Number of data sources in case, Case Notes (if any), Examiner (if any).
- One sheet per data type. The sheet name has `/ \ : ? *` replaced by `_`.
  - Sets produce a bold 14 pt row with the set name. Set elements (keywords) produce a 14 pt row.
  - Each table is a header row followed by data rows and a blank separator row.
  - Columns are auto-sized at the end of each sheet.
- Cells longer than 32 767 characters are replaced with "Value is too long to fit into an Excel cell. <msg>". The code constant is mistyped as 36767.
- Does not report thumbnails. `addSetIndex` is ignored. Registered as source module "Excel Report".

### Files – Text (`file-report.txt`)

- Writes a UTF-8 BOM. Module config: **Tab delimited** (default) or **Comma delimited** radio.
- Every value is wrapped in double quotes without escaping embedded quotes, which is a bug. Lines are separated by the OS separator, with a header row of selected column names.
- **Source rows**: `findAllFilesWhere("meta_type != DIR")`, i.e. all non-directory files of all data sources loaded into memory, then filtered by selected data-source ids. Progress label updates every 100 files.
- **Columns** (`FileReportDataTypes`, all default-on, in this order):

  | Column | Value |
  |---|---|
  | Name | file name |
  | File Extension | text from the last dot, including the dot; "" when there is none |
  | File Type | meta type string |
  | Is Deleted | "yes" if the meta flags equal UNALLOC |
  | Last Accessed | atime as date |
  | File Created | crtime as date |
  | Last Modified | mtime as date |
  | Size | size |
  | Address | meta addr |
  | Hash Value | MD5 |
  | Known Status | known status |
  | Permissions | mode string |
  | Full Path | unique path |

### TSK Body File (`BodyFile.txt`)

- Query: `type = FS(0) AND name != '.' AND name != '..'`, filtered to the selected data sources. Supports data-source selection.
- Line format (TSK 3.x body format): `MD5|unique path|meta_addr|mode_string|uid|gid|size|atime|mtime|ctime|crtime\n`. Times are raw epoch seconds. A null MD5 or path becomes an empty string.
- If ingest is running, the first line is "Warning, this report was run before ingest services completed!", which corrupts the body format. The file is opened in append mode.
- Progress increments every 100 files.

### Google Earth KML (`ReportKML.kml` + `style.kml`)

- Supports data-source selection; `null` means all. Also callable from the Geolocation window with an explicit waypoint list (`generateReport(dir, progress, waypoints)`).
- **Document**:
  - KML 2.2 namespace with `gx`, `kml` and `atom` prefixes.
  - `<name>` is "<report title> KML". When ingest is running, it adds `<snippet>` "Warning, this report was run before ingest services completed!".
- **Folders**, each with an icon `href` pointing at the Autopsy GitHub raw image URLs:
  - EXIF Metadata (RED), GPS Bookmarks (BLUE), GPS Last Known Location (PURPLE), GPS Routes (GREEN), GPS Searches (WHITE), GPS Trackpoints (WHITE), GPS Tracks (GREEN; one subfolder per track), GPS Areas (GREEN; one subfolder per area, Polygon).
  - Styles come from the bundled `style.kml` StyleMaps: `redFeature`, `greenFeature`, `blueFeature`, `purpleFeature`, `whiteFeature`, `yellowFeature`. Each uses a pushpin icon; highlight scale is 1.3 and line width goes from 5 to 10.
- **Placemark**:
  - `<name>`: the label, else the timestamp, else "".
  - `<styleUrl>`.
  - `<description>`: HTML "`<h3>type</h3><b>Name: </b>…<br>`" plus Timestamp, Latitude, Longitude, Altitude, and all other waypoint properties.
  - `<TimeStamp><when>yyyy-MM-dd'T'HH:mm:ssX</when>`.
  - Geometry, then `<snippet>` with "%.2f, %.2f" coordinates.
- **Geometry**:
  - Point coordinates are `lon,lat,alt` (alt defaults to 0.0). When alt is non-zero, add `altitudeMode=clampToGround`.
  - Routes use only the first two points: a LineString (extrude, tessellate, clampToGround) plus start and end placemarks.
  - Areas are a Polygon `outerBoundaryIs/LinearRing`, closed by repeating the first point.
- **EXIF placemarks** copy the image file into the report folder (64 KiB buffer copy) and embed `<img src='name' width='400'/>` plus "Source Path:" (the unique path with the `img_` and `vol_` prefixes stripped).
- Partial track or area parse failures mark the report "Incomplete" in the reports table and show "There was an error creating the report. Some items were not exported."

### CASE-UCO (`CASE_UCO_output.jsonld`)

- Uses the TSK `CaseUcoExporter`, with Gson pretty-print and 4-space indent. Output is `{"@graph":[ … ]}`.
- Contents, in order:
  1. The case export.
  2. Per selected data source: the data-source export, then a depth-first walk of all children. Only files with meta type UNDEF, REG or VIRT are exported.
  3. Every artifact of every in-use standard type whose data source is selected, except **TSK_KEYWORD_HIT** (not representable in CASE). Artifacts that cannot be exported are logged and skipped.
- Warns in the status label if ingest is running. Registered as "CASE-UCO Report".

### Save Tagged Hashes (General, no output file: `getRelativeFilePath()==null`)

- **Config**: a combo of *updateable* hash sets (`HashDbManager.getUpdateableHashSets()`), a "Configure Hash Sets…" button to create or import one, and the tag list with an "All Tags" checkbox (default `exportAllTags=true`).
- **Behaviour**: for each selected tag name, for each content tag whose content is an `AbstractFile` with an MD5: `hashSet.addHashes(file, caseDisplayName)`, where the comment is the case name.
- Completion message: "N file(s) added to hash set. M file(s) without a hash skipped." Errors: "No hash set selected for export.", "No tags selected for export.", or "Failed to export hashes for the following file(s)…".
- Artifact tags are ignored. There is no data-source filtering.

### Data Source Summary Report (XLSX)

- For each selected data source, writes `<report dir>/DataSourceSummary/<escaped ds name>-<MM-dd-yyyy-HH-mm-ss>.xlsx`.
- Contains the same data as the Data Source Summary panel tabs. Sheets come from ExportTypes, ExportUserActivity, ExportAnalysisResults, ExportRecentFiles, ExportPastCases, ExportGeolocation, ExportTimeline (with a bar chart), ExportIngestHistory and ExportContainerInfo. Pie and bar charts are rendered into the sheets.
- Progress has 11 weighted steps. See the Data Source Summary section for statistic definitions.

### Extract Unique Words (KeywordSearch module)

- Writes `Unique Words.txt` with all unique lower-cased terms in the Solr index for the case via `server.extractAllTermsForDataSource`. No data-source selection.
- Errors if nothing is indexed. Asks for confirmation if ingest is still running.

### Portable Case

Purpose: copy selected tagged and interesting items into a new single-user case that can be opened standalone.

- **Output**:
  - `<report dir>/<case display name> (Portable)/` is a new TSK SQLite case created via `Case.createPortableCase`.
  - If "include application" is on, there is an extra level: `<report dir>/<case> (Portable)/<case> (Portable)/` plus `<AppName>/` (a full copy of the install dir) and `open.bat` (`"%~dp0<App>\bin\<App>64.exe" "..\<case> (Portable)"`).
  - If the case folder already exists, the module errors.
- **Files**: copied to `PortableCaseFiles/<Audio|Documents|Executables|Images|Videos|Other>/<objId>-<escaped name>`. The category comes from the MIME type via `FileTypeCategory`. Copied files are added as **local files** with the original times, MD5, SHA-256, known status, MIME type and OS account, and `isFile=true`.
- **Recreated structure**:
  - The parent chain is recreated recursively: Image (with md5/sha1/sha256, timezone, device id, host), VolumeSystem, Volume, Pool, FileSystem, and directories as local directories.
  - LocalFilesDataSource, Hosts, OS accounts and realms (realm name = first name) are also recreated.
  - Old→new id maps prevent duplicate copies.
- **Table `portable_case_max_ids(table_name TEXT PK, max_id TEXT)`**: stores the source case's max `obj_id` (`tsk_objects`), `content_tags.tag_id`, `blackboard_artifact_tags.tag_id` and `tsk_examiners.examiner_id`. This lets a later merge distinguish new data.
- **Steps**:
  1. Validate that the output dir exists and is a directory.
  2. Resolve tag names and set names (all, or selected). If both are empty, fail with "No interesting files, results, or tagged items to copy".
  3. Create the case.
  4. Create the content-viewer image tag table.
  5. Copy tag names (name, description, color, known status).
  6. For each tag name:
     1. Copy files with content tags. Re-add the content tag with comment and byte offsets, and copy the `image_tag` app_data, which holds region annotations.
     2. Copy artifacts with artifact tags:
        - Copy the source content first.
        - Copy the artifact recursively, including `TSK_ASSOCIATED_ARTIFACT` targets.
        - Copy `TSK_ATTACHMENTS` files and rewrite the JSON with new ids. For legacy emails, child derived files are copied instead.
        - Copy the file referenced by `TSK_PATH_ID`.
        - Re-tag the artifact.
  7. For each selected interesting set: copy every `TSK_INTERESTING_FILE_HIT`, `TSK_INTERESTING_ARTIFACT_HIT` and `TSK_INTERESTING_ITEM` analysis result whose `TSK_SET_NAME` matches, with its source.
  8. Write the CASE-UCO file `Reports/portable_CASE_UCO_output` inside the portable case. It contains only unique copied files, using a temp dir of marker files `case_uco_tmp/<objId>` for dedupe, and writes each data source once.
  9. Optionally copy the application.
  10. Optionally compress:
      - Close the DB.
      - Run `7z a <tmp>/<case>.zip <dir> [-v<chunk>]` using the bundled `7-Zip/7z.exe` (Windows only).
      - Poll every 200 ms for cancel.
      - Clean the case folder and move the zip(s) into it.
- **Copy details**:
  - Analysis results keep score, conclusion, configuration and justification. Data artifacts keep the OS account.
  - Custom artifact and attribute types are re-created by name.
  - Attribute value types BYTE, DOUBLE, INTEGER, LONG/DATETIME, STRING and JSON are copied, with sources joined by ",".
- **Cancellation**: checked between every item. On cancel, close the DB, clear the maps, and mark the status CANCELED. Partial output is left on disk.

### Branding (Tools > Options > General/"Report" area)

- `ReportBranding` stores, in `ModuleSettings` (module `ReportBranding`): **AgencyLogoPath** (stored relative to the user dir when inside it), **ReportTitle** (default "Autopsy Forensic Report") and **ReportFooter** (default "Powered by Autopsy …").
- The generator logo is extracted to `<userconfig>/Reports/branding`.
- The Options panel validates that the agency logo is a readable image and previews it at 64×64.
- **UI snapshot reports** (`UiSnapShotReportWriter`, used by Timeline and Communications "Snapshot" report):
  - A Mustache-templated mini site: `index.html` (frames), `summary.html` (report name, branding, generation time, ingest-running warning, case info, agency logo), `nav.html` and CSS.
  - A subclass writes the snapshot page, typically a PNG of the view plus filter description.
  - Registered in the reports table.

### Autopsy problems & postmortem ideas (Reporting)

- **One module per wizard run.** Users routinely want HTML + Excel + KML in one go. Allow multi-select, run the modules in parallel, and give each its own progress row.
- **N+1 queries everywhere.** Each artifact triggers separate fetches of attributes, tags and source content (`getUniquePath` walks parents). Keyword and hash reporting runs `getAbstractFileById` twice per row. Instead, run one SQL query per type joining attributes (pivot) and tags (`string_agg`), and precompute unique paths with a recursive CTE or a cached parent-path table.
- **Everything is loaded into memory before sorting.** This affects artifact lists per type and `findAllFilesWhere(meta_type != DIR)` for the file report, which is millions of rows for big images. Use streaming cursors with ORDER BY in SQL and write rows incrementally. Use a streaming XLSX writer (`rust_xlsxwriter` constant-memory mode), since POI XSSF keeps the full workbook in RAM.
- **Sorting by rendered strings** gives lexicographic date and number ordering. Sort by typed values in SQL instead, using a stable default per type (e.g. datetime desc).
- **HTML frameset** is obsolete and has no search, sort or paging. Generate a modern single-page or static site: sticky nav, client-side filter/sort, paging for large tables, and JSON data islands. Keep the "open offline from a USB stick" property with no server. Escape set names in anchors, which are currently unescaped.
- **CSV correctness**: no quote escaping, an OS-dependent newline, and a BOM. Use RFC 4180 quoting (`csv` crate), consistent `\n`, and an optional BOM.
- **Body file**: the ingest warning is written into the data. Put it in a side file or log instead. The file is opened in append mode, which duplicates content on re-run. Include deleted, orphan and non-FS files as an option, and emit `name -> link` and the "(deleted)" suffix like TSK `fls -m`.
- **Excel 32 767-character limit**: truncate with a marker rather than replacing the whole value. Sheet names are not limited to 31 chars or de-duplicated (POI throws), so handle collisions.
- **KML** depends on online GitHub and Google icon URLs and copies EXIF images by bare name (collisions overwrite). Embed icons in a KMZ (zip) together with images renamed `<objId>_name`. Support multi-point routes, not just the first two points.
- **Portable case** compresses on Windows only via bundled 7z, and "include application" is Windows-only. Use the pure-Rust `zip`/`sevenz-rust` crates with split volumes on all platforms, and bundle the postmortem binary for the target OS. Store the max-ids table so portable cases can be **merged back**, a feature Autopsy lacks.
- **Report records** store only path and name. Also store module, settings JSON, data-source scope, tag filter, generator version, examiner and the hash of output files, to support court defensibility and report re-generation. "Delete report" should optionally delete the files too.
- **Multi-user**: report output goes to the shared case folder synchronously. In postmortem, run generation as a background job, so the user can keep working and get a notification, and record in the DB which node and user produced the report.
- **Timezone**: report cells use the user's display-TZ preference silently. Print the TZ used in the report header and offer UTC or a per-report TZ.
- **Configuration storage**: Java-serialized profiles are brittle across versions. Use versioned JSON with explicit defaults, and keep named profiles for CLI/headless use.

## Image Gallery

Source: `ImageGallery/src/org/sleuthkit/autopsy/imagegallery/` (JavaFX embedded in a Swing TopComponent via `JFXPanel`). Menu: **Tools > Images/Videos**, plus a toolbar button "Images/Videos". The window is titled "Image/Video Gallery", opens **floating** by default, and is allowed only in the "ImageGallery" or "timeline" window modes.

### Lifecycle, enablement and "listening"

- An `AutopsyService` ("Image Gallery Update Service") runs on case open:
  1. Ensures the **"Project VIC"** tag set exists (creating it if missing).
  2. Creates one `ImageGalleryController` per case.
- On case close it shuts the controller down: unsubscribes events, clears the thumbnail cache, history and groups, stops the DB executor (30 s await), and closes `drawable.db`.
- **Enabled / listening**:
  - Per-case property `ENABLED` lives in `<case module dir>/Image Gallery/` per-case properties. Fallback: global pref `enabled_by_default` (default **true**).
  - Options panel (Tools > Options > Image/Video Gallery):
    - "Enable Image Gallery for new cases by default."
    - "Enable Image Gallery updates for the current case." (disabled during ingest)
    - "Don't show a warning when overwriting categories, by acting on an entire group." (`group_categorization_warning_disabled`, default false)
  - Hidden pref: `multi_user_case_info_dialog_disabled`.
  - Switching listening from off to on in a single-user case with stale data sources triggers an automatic rebuild.
- **Open action**:
  - Enabled only if a case is open, JavaFX initialised, and the case has data.
  - Multi-user case: show an info dialog once (with "Don't show this message again"). It explains that local ingest updates live, while other nodes' results arrive only when the gallery is opened and the local DB is rebuilt.
  - Compute per-data-source build status:
    - Stale if `UNKNOWN`-with-MIME-typed-files or `REBUILT_STALE`.
    - No-analysis if `UNKNOWN` without MIME-typed files.
  - If any data source is stale, show the dialog "The image / video database may be out of date. Do you want to update and listen for further ingest results?…" with Yes / No / Cancel:
    - Yes: rebuild, then open.
    - No: open as is.
    - Cancel: do not open.
  - If all data sources are no-analysis, show "No image/video files available to display yet. Please run FileType and EXIF ingest modules." and do not open.
- **Data-source chooser on open**:
  - If the case has more than one data source and the current group-by is PATH, a modal "Choose a data source to view." ChoiceDialog appears. Options: "All" plus each data source. Each cell shows the data source's build status and whether it has too many files.
  - If the selected scope has **more than 6 000 000 files** (`countFilesWhere`, all files, not just images), the dialog "There are too many files in the selected datasource(s) to ensure reasonable performance." appears and the gallery does not open.
  - Otherwise: `regroup(ds, groupBy, sortBy, sortOrder, force=true)`.

### Drawable DB (`<case>/ModuleOutput/Image Gallery/drawable.db`, SQLite, local per machine even for multi-user)

- **PRAGMAs**: `synchronous=OFF`, `read_uncommitted=True`, `foreign_keys=ON`, `journal_mode=MEMORY`, `count_changes=OFF`, `temp_store=MEMORY`, `cache_size=50000`, `auto_vacuum=0`.
- All access is serialized by a single fair `ReentrantReadWriteLock` *write* lock (reads take it too).
- If the existing file lacks `drawable_files` or the `data_source_obj_id` column, it is deleted and recreated.
- **Tables**:

  | Table | Columns |
  |---|---|
  | `image_gallery_db_info` | `name TEXT PK, value TEXT NOT NULL` — keys `IG_SCHEMA_MAJOR_VERSION`, `IG_SCHEMA_MINOR_VERSION`, `IG_CREATION_SCHEMA_MAJOR_VERSION`, `IG_CREATION_SCHEMA_MINOR_VERSION`; current 1.2 |
  | `datasources` | `id INTEGER PK, ds_obj_id BIGINT UNIQUE NOT NULL, drawable_db_build_status VARCHAR(128)` (UNKNOWN / IN_PROGRESS / COMPLETE / REBUILT_STALE) |
  | `drawable_files` | `obj_id BIGINT PK, data_source_obj_id BIGINT NOT NULL FK→datasources(ds_obj_id) ON DELETE CASCADE, path TEXT, name TEXT, created_time INTEGER, modified_time INTEGER, make TEXT DEFAULT NULL, model TEXT DEFAULT NULL, analyzed INTEGER DEFAULT 0` |
  | `hash_sets` | `hash_set_id INTEGER PK, hash_set_name TEXT UNIQUE NOT NULL` |
  | `hash_set_hits` | `hash_set_id FK, obj_id FK→drawable_files ON DELETE CASCADE, PK(hash_set_id,obj_id)` |

  Indexes: `path_idx`, `name_idx`, `make_idx`, `model_idx` and `analyzed_idx` on `drawable_files`.
- **Tables in the CASE DB** (shared between multi-user nodes; created through `CaseDbAccessManager`, BIGSERIAL on PostgreSQL):
  - `image_gallery_db_info(id PK, name TEXT UNIQUE, value TEXT)`: the same version keys.
  - `image_gallery_groups(group_id PK, data_source_obj_id BIGINT DEFAULT 0, value TEXT NOT NULL, attribute TEXT NOT NULL, is_analyzed INTEGER DEFAULT 0, UNIQUE(data_source_obj_id,value,attribute))`. The data source id is 0 for every attribute except PATH. `is_analyzed` was added in the 1.0→1.1 upgrade with DEFAULT 1.
  - `image_gallery_groups_seen(id PK, group_id FK→groups ON DELETE CASCADE, examiner_id FK→tsk_examiners, seen INTEGER DEFAULT 0, UNIQUE(group_id,examiner_id))`.
  - Standard category groups (one per Project VIC tag name, attribute CATEGORY) are inserted on startup.
- **`path` value**: `getUniquePath()` with the trailing file name removed, e.g. `/img_x.E01/vol_vol2/Users/bob/Pictures/`. PATH groups are therefore folders.
- **Make/model**: from the `TSK_METADATA_EXIF` attributes `TSK_DEVICE_MAKE` and `TSK_DEVICE_MODEL`, with `WordUtils.capitalizeFully` applied.
- **Startup cleanup**: rows in `datasources` whose ids are no longer in `data_source_info` are deleted, cascading to their files. The in-memory set of all `obj_id`s (`fileIDsInDB`) is loaded and used by `isInDB`.
- **Group-membership queries**: prepared `SELECT obj_id, analyzed FROM drawable_files WHERE <attr> = ?`, optionally with `AND data_source_obj_id = ?`. Hash set membership joins `hash_sets` and `hash_set_hits`. CATEGORY, TAGS and MIME_TYPE membership is computed from the case DB (content tags, or `tsk_files.mime_type`) and filtered by `isInDB`.
- **Group value enumeration**:

  ```sql
  SELECT data_source_obj_id, <attr>, COUNT(*) FROM drawable_files
  WHERE LENGTH(<attr>) > 0 [AND data_source_obj_id = ?]
  GROUP BY data_source_obj_id, <attr>
  ORDER BY <attr> | COUNT(*) ASC|DESC
  ```

  Special cases:
  - CATEGORY: the fixed category list.
  - TAGS: non-category tag names in use.
  - ANALYZED: `{false, true}`.
  - HASHSET: distinct names from `hash_sets`.
  - MIME_TYPE: `group_concat(obj_id)` per mime type over **all of `tsk_files`**, then keep types having any id in the DB.
- **Seen state**:
  - Mark seen: `INSERT INTO image_gallery_groups_seen (group_id, examiner_id, seen) VALUES ((SELECT group_id … WHERE attribute=? AND value=? AND data_source_obj_id=?), <examiner>, 1)`. PostgreSQL adds `ON CONFLICT (group_id, examiner_id) DO UPDATE SET seen=1`.
  - Unseen: `UPDATE … SET seen=0 WHERE group_id IN (…)` for **all examiners**.
  - Queries count `seen=1` rows: with examiner id −1 meaning "any examiner", otherwise the current examiner.
  - Caches: seen state 30 s, group inserts 5 min.

### Populating the DB

- **What is drawable**:
  - MIME type (lower-cased) starts with `image/` or `video/`, or is in the supported set. The supported set is ImageIO reader MIME types plus `application/x-shockwave-flash`, `application/x-123` and `application/x-wmf`.
  - Excluded: `application/octet-stream`, `image/vnd.microsoft.icon`, `application/x-emf`.
  - Special case: `audio/x-aiff` with extension `tiff`.
  - **Known files (NSRL `KNOWN`) are excluded and removed.**
  - Video = MIME `video/*` or `application/x-shockwave-flash`. The extension lists (bmp, gif, jpg/jpeg/jpe/jp2/jpx, pbm/pgm/ppm, png, tga, psd, tif/tiff, yuv, ico, ai, svg, sn, ras, wmf/emf/wmz/emz; videos fxm, aaf, 3gp, asf, avi, m1v, m2v, m4v, mp4, mov, mpeg, mpg, mpe, rm, wmv, mpv, flv, swf) exist but detection is MIME-only.
- **Incremental (listening, local ingest)**:
  - `DATA_SOURCE_ANALYSIS_STARTED`: set the status to IN_PROGRESS unless already COMPLETE, and build the metadata caches. These are in-memory sets of obj_ids with content tags, hash hits or EXIF artifacts, so each file insert can skip expensive lookups.
  - `FILE_DONE` with a drawable, not-known file: queue `UpdateDrawableFileTask`, which upserts the file with `analyzed=true`, inserts hash set rows, and inserts a group row for every groupable attribute value (PATH with its data source id, others with 0).
  - `DATA_ADDED` events update the EXIF and hash caches.
  - When a file arrives from a new parent path, the previous PATH group is marked `is_analyzed=1` and populated in the UI.
  - `DATA_SOURCE_ANALYSIS_COMPLETED`:
    - Flush the current path group.
    - Set status COMPLETE if any file has a MIME type, else UNKNOWN (the file type module did not run).
    - Free the caches.
  - Remote (other node) completion: mark the model stale, and if the gallery is open prompt "A new data source was added and finished ingest. The image / video database may be out of date. Do you want to update…?".
- **Data source added** (local, listening): insert it with status UNKNOWN. **Data source deleted**: delete it, cascading.
- **Bulk rebuild** (`DrawableFileUpdateTask`, one data source at a time, for each stale id):
  1. Build caches.
  2. Note whether any regular files lack a MIME type (`meta_addr >= 32`, `parent_path <> '/'`, name not like `$%:%`).
  3. Query the case DB:

     ```sql
     data_source_obj_id = ? AND meta_type = REG
     AND (mime_type LIKE <each supported> OR mime_type LIKE 'video/%' OR mime_type LIKE 'image/%')
     ORDER BY parent_path
     ```

  4. For each file: KNOWN files are removed, drawable files are upserted, and other files are removed. The case DB transaction is committed every 200 files with a 500 ms sleep, to let ingest breathe.
  5. Commit the drawable transaction, which notifies the GroupManager of changed and removed ids.
  6. Status becomes `REBUILT_STALE` if files without MIME existed or the run was cancelled or failed, else `COMPLETE`. Then recompute the "model stale" flag.
- All DB work runs on a single "ImageGallery-DB-Worker" thread. The queue size shows in the status bar as "N File Update Tasks". Regrouping (the group-by box) is disabled while the queue is non-empty **or any ingest is running**.

### Grouping

- **Group-by options** (Toolbar "Group By:", default **Path**): Path, Hashset, Category, Tags, Camera Make, Camera Model, MIME type.
  - Non-groupable display attributes: Name, Analyzed, Created/Modified Time, MD5 Hash, Internal Object ID, Width, Height.
- **Data-source combo**:
  - "All" plus each data source. Data sources with more than 6 M files are shown as not viewable.
  - Only enabled for PATH grouping. Switching from PATH to another attribute while a data source is selected shows the confirmation "Grouping by attributes other than path does not support the data source filter. Files and groups from all data sources will be shown. Proceed with regrouping?"; on OK the data source resets to All.
- **Group sort** (`SortChooser`):
  - Options: **Priority** (hash-hit density, descending), None, **Group Name**, Group Size.
  - Ascending/Descending radios are disabled for Priority and None.
  - Initial property default is PRIORITY, but `reset()` sets GROUP_BY_VALUE/ASCENDING. Hash-hit density = hits × 100 / size.
  - A help popover explains that the sort affects the unseen-group queue order.
- **When a group exists**:
  - A group appears only when it is "analyzed". For PATH this is `image_gallery_groups.is_analyzed=1` (all files in that folder processed). Other attributes are always analyzed.
  - The group must also contain at least one file in the DB.
  - The GroupManager keeps `analyzedGroups` and `unSeenGroups` for the current group-by, both sorted with the chosen comparator.
- **Live updates**:
  - Category or tag added: the file moves into the new CATEGORY or TAGS group and is removed from its other category groups. CATEGORY groups are never removed when empty.
  - Any file added to an existing group resets the group to **unseen**.
  - File removal empties groups, and the group is then marked seen and removed (except CATEGORY groups).
- **Regroup task**: cancellable. Progress "regrouping by X: <value>" shows in the status bar. Afterwards, if the viewed group is no longer valid, it shows the first unseen group, else the first analyzed group, else nothing.
- **Within-group file order**: the insertion order from the DB query (effectively obj_id or rowid order). There is no user-selectable file sort.

### Seen / next unseen / collaboration

- The header "Next Unseen Group" button (double-arrow icon) glows with a pulsing drop-shadow animation while enabled. **Enter** also triggers it.
- On press:
  1. Mark the current group seen for the current examiner.
  2. Show `unSeenGroups[0]` in tile mode.
- States:
  - "Mark Group Seen" (stop icon, disabled) when the only unseen group is the one being viewed.
  - "All Groups Have Been Seen" when no unseen groups remain.
- Checkbox **"Seen by other examiners"** (collaborative mode, default off): when checked, "seen" means seen by any examiner (−1). Toggling it recomputes the seen state for all groups in the background.
- Group cells in the nav tree are **bold when unseen**.
- History: **Back** (Alt+Left) and **Forward** (Alt+Right) step through viewed `GroupViewState`s (group + mode + slideshow file). The label "Group Viewing History:" is shown. Changing view state clears the selection and the undo stack.

### UI layout

- **Top toolbar**:
  - Group By combo, Sort chooser + help icon, Data Source combo.
  - "Tag Group's Files:" split button. The default action is the "Follow Up" tag; the menu lists all non-category tag names.
  - "Categorize Group's Files:" split button. The default action is the first category; the menu lists all categories.
  - "Thumbnail Size (px):" slider, 100–300 in steps of 100, default 100.
- **Left pane**: a TabPane with **"All Groups"** (GroupTree) and **"Only Hash Hits"** (HashHitGroupList), above the **Category summary table** (columns: Category | # Files, counts from `CategoryManager`).
  - GroupTree: a hierarchical tree for PATH grouping (path split on "/"; the first segment gets "(Id: <ds id>)" appended), otherwise a flat list.
  - Nav sort options: Uncategorized Count, Group Name (default for the tree), Hit Count (default for the hash list, descending by default), Group Size, Hit Density. Each cell shows "name (value-of-sort-key)".
  - Hash list = analyzed groups with `hashSetHitsCount > 0`.
  - Empty state: "There are no groups."
- **Centre: GroupPane header**:
  - "<group value or 'unknown'> -- <N> hash set hits / <M> files".
  - Tile/Slideshow segmented toggle.
  - "Tag Selected Files:" (default Follow Up) and "Categorize Selected File:" split menus, disabled when the selection is empty.
  - Undo and Redo buttons (tile mode only), Back and Forward, the Next Unseen button, and the "Seen by other examiners" checkbox.
- **Right: MetaDataPane "Details"**:
  - Attribute | Value table for the last selected file: Name, Analyzed, Category, Tags, Path, Created Time, Modified Time, MD5 Hash, Hashset, Camera Make, Camera Model, Internal Object ID, Width, Height, MIME type.
  - Context menu "Copy" (Ctrl+C) copies the value.
  - Placeholder: "Select a file to show its details here."
- **Status bar**: "<n> File Update Tasks" with a progress bar; "Regrouping" with the message and progress; a stale indicator with tooltip "Some data may be out of date. Enable Image Gallery in Tools | Options … after ingest is complete…".
- **Overlay notices** (translucent grey) when no groups are available:
  1. "No groups are fully analyzed; but listening to ingest is disabled…"
  2. "…ingest is still ongoing. Please Wait." (with spinner)
  3. "…image / video data is still being populated. Please Wait."
  4. "There are no images/videos available…; listening disabled…"
  5. "There are no images/videos in the added datasources."
  6. "There are no fully analyzed groups to display: the current Group By setting resulted in no groups…"
- **Tile**:
  - Cell size = thumbnail size + 75 px.
  - Shows the image, file name label (with tooltip), a video badge, a hash-hit badge, and a follow-up flag toggle (red/grey) that adds or removes the "Follow Up" tag.
  - Selection border is 3 px: blue when selected, grey otherwise. The last selected tile gets a glow effect.
  - **Category border** is 10 px solid in the category tag's colour, or **dashed cyan when the file has hash hits** (hash takes precedence). No border when the colour is NONE.
- **Slideshow**:
  - Full-size image or video, with left/right buttons (hidden when the group has only one file) wrapping around the group.
  - Label "<name> ( i of n in group )".
  - Video: copied to a temp file, then played by JavaFX MediaPlayer with play/pause, a time slider with seek, and a volume slider with mute. Unsupported codecs show "<error>: <message>".
  - GIFs are read directly for animation.

### Categories, tags, undo

- **Categories are a tag set ("Project VIC")**, ranked:
  0. "Non-Pertinent": GREEN, Suspicious
  1. "Child Abuse Material - (CAM)": RED, Notable/BAD
  2. "Child Exploitive (Non-CAM) Age Difficult": YELLOW, BAD
  3. "CGI/Animation - Child Exploitive": FUCHSIA, BAD
  4. "Comparison Images": BLUE, Suspicious

  Being in a tag set means a file holds at most one category; adding one removes the others (the TSK tag set semantics). Older Autopsy versions used CAT-0 … CAT-5 names. The slideshow category toggles still parse "CAT-n" display names, which is a latent bug with the new names.
- **Shortcuts**:
  - **Digits 0–9** (accelerator = tag rank; ranks below 10) apply that category to the selected files.
  - **Alt + 0–5 / Numpad 0–5** in tile mode first selects all files in the group, so the category applies to the whole group.
  - Categorize-group with existing different categories shows an "Overwrite" confirmation listing "<count> with <category>", with "Don't show this message again".
- **Undo/Redo** (Ctrl+Z / Ctrl+Y) applies to category changes only: an undo stack of `CategorizationChange{newCat, Map<fileId, oldCat>}`. Undo re-applies each old category. Changing group clears the stack.
- **Tag menu**: all non-category tag names, plus "New Tag…" (via the Autopsy tag dialog). Delete Tag works on a single file.
- Context menu on a tile:
  - Categorize ▸
  - Tag ▸
  - Remove Tag ▸ (single selection only)
  - Extract File(s)
  - Show Content Viewer (opens a new Autopsy content-viewer window for the file)
  - Open in External Viewer (Ctrl+E)
  - Plus any registered `ContextMenuActionsProvider` actions.
- **Category counts**: cached per tag name. Computed lazily as the number of content tags of that name whose obj_id is in the DB, then kept in sync by tag events. `getUncategorizedCount(group)` uses SQL over `tsk_files` for file ids not in content_tags of any category tag.

### Keyboard and mouse summary

| Input | Effect |
|---|---|
| Click tile | select (clear others) |
| Ctrl+click | toggle selection |
| Shift+click / Shift+arrows | range select from anchor |
| Arrow keys (tile) | move selection by ±1 / ±columns (columns = floor((width−18)/(cell+2·hgap))) |
| Double-click tile | open slideshow on that file |
| Space | toggle tile and slideshow (slideshow starts at the last selected file) |
| Left/Right (slideshow) | previous/next, wrapping |
| Double-click in slideshow | back to tiles |
| Page Up / Page Down | scroll to top / bottom |
| Enter | next unseen group (marks current seen) |
| 0–9 | categorize selection |
| Alt+0–5 | select all in group (then categorize) |
| Ctrl+Z / Ctrl+Y | undo / redo categorization |
| Alt+Left / Alt+Right | group history back / forward |
| Ctrl+E | open in external viewer |
| Ctrl+C (details pane) | copy value |
| Click empty grid | clear selection; right-click shows the group context menu if the selection is non-empty |

### Thumbnails

- In-memory Guava cache: maximum **1000 entries**, soft values, expiring 10 min after access, keyed by obj_id. `MAX_THUMBNAIL_SIZE = 300` px.
- Disk cache: `ImageUtils.getCachedThumbnailFile(file, 300)`, the case-level `<case>/Cache/` thumbnails shared with the main thumbnail viewer. A cached file is used only if narrower than 300 px. Otherwise `ImageUtils.getThumbnail` generates it; video thumbnails come from a frame grab.
- The default icon result is treated as a failure (shows an error tile).
- Tiles load thumbnails via background `Task`s with a progress mask. A bug puts finished tasks into the cache under key `Long.MIN_VALUE`, so the in-memory cache is mostly ineffective for the tile path.

### Main-UI integration

- Gallery selection is mirrored into the TopComponent's ExplorerManager as `FileNode`s. Autopsy's global lookup therefore sees the selected `AbstractFile`s, enabling the global actions (extract, tag, external viewer) and the content viewers of the main window.
- Tags added anywhere in Autopsy flow back via `CONTENT_TAG_ADDED` and `CONTENT_TAG_DELETED` events when the file is in the drawable DB.

### Autopsy problems & postmortem ideas (Image Gallery)

- **Separate per-node SQLite DB plus manual "rebuild" prompts** is the root of most UX pain: staleness dialogs, the multi-user caveat dialog, the "listening" toggle and REBUILT_STALE states. In postmortem, make the drawable index **a view or materialized table in the main case DB**, maintained transactionally by the ingest pipeline (MIME, EXIF and hash stages write the columns directly). The gallery is then always current for all nodes, with no rebuild and no listening flag.
- **6 M file hard limit** counted on *all* files, not images, blocks large cases entirely. With indexed SQL and virtualized grids there is no need for a limit; paginate groups and lazily load the file ids of large groups.
- **Global write lock serializes reads** and every group query, and MIME-type grouping scans all of `tsk_files` with `group_concat`. Use proper indexes (`(data_source, mime)`, `(path)`, `(make, model)`) and a read pool; group enumeration becomes a single `GROUP BY` with counts.
- **PATH groups appear only after the folder is "analyzed"**, detected by a change of parent path in the ingest stream. This is fragile with parallel ingest and leaves the last folder hanging until data-source completion. Instead, show groups immediately with live counts and a "still analyzing" badge.
- **The seen state is shared ambiguously.** Unseen resets the seen state for all examiners, while seen is per examiner. Model it as `(group, examiner, seen_at)` events: "unseen since new files arrived" compares the group's `last_file_added_at` to `seen_at`, and collaboration mode is a simple query switch.
- **Within-group sort is not selectable.** Add sorting by name, size, time, category and hash hits, and filters such as "uncategorized only", "hash hits only" and "has EXIF GPS".
- **Thumbnail caching bug** (`cache.put(Long.MIN_VALUE, …)`) and JavaFX image memory pressure. In egui, use a texture LRU bounded by GPU memory (e.g. 256 MB) plus an on-disk WebP/JPEG thumbnail store in the case folder keyed by obj_id and size. Decode thumbnails on a rayon pool, prioritize visible cells, and cancel offscreen requests. Support thumbnail sizes up to about 512 px, and fix the slider granularity (100/200/300 is coarse).
- **Category naming is hardcoded** to Project VIC US and the toggles parse "CAT-n". Make category sets configurable (Project VIC US/UK, custom), with shortcut keys bound to rank. Show the shortcut digit on the tile.
- **Undo exists only for categorization and is cleared when the group changes.** Make undo global within the session and cover tags too.
- **Video playback** depends on JavaFX codecs, which miss many formats. Use ffmpeg (via `ffmpeg-next` or a sidecar) for frame-strip thumbnails and playback. Show multiple keyframes on hover ("scrub preview").
- **Arrow-key column math** depends on the rendered width. egui's grid knows its columns exactly; also add Home/End and Ctrl+A.
- **Popout**: the gallery is a natural candidate for its own viewport window. The slideshow could be a separate full-screen viewport on a second monitor, synchronized with the tile selection.
- **Hash-hit display**: the dashed cyan border hides the category colour. Show both (inner category ring plus hash badge with the set name on hover).
- **"Known" files are silently excluded.** Make this a visible filter toggle, since examiners sometimes need NSRL images.

## Performance/usability problems in Autopsy & improvement ideas for postmortem

Each section above ends with its own list of problems and ideas. This section collects the problems that come up in several areas, and the architectural fixes postmortem should adopt for all of them.

### Recurring problems

1. **N+1 query patterns everywhere.**
   - Report generation fetches attributes, tags and unique path per artifact.
   - The Timeline and histogram run one count query per bin.
   - The Other Occurrences viewer re-queries on every selection.
   - The Image Gallery runs group-membership queries per group.
   - CVT relationship lookups are per account.

   → Write set-based SQL: pivot attributes with `GROUP BY` plus aggregation, precompute unique paths (a materialised `parent_path` column, or a recursive CTE cached per data source), and batch lookups with `IN (...)` or temp-table joins.
2. **Load-everything-then-sort-in-memory.**
   - The file report loads all non-directory files.
   - Timeline Details/List loads all events.
   - Discovery loads all results before grouping.
   - Common Properties loads all matches.
   - KML loads all waypoints.

   → Stream with server-side `ORDER BY` and keyset pagination, and give the UI virtualised tables and plots that only request visible ranges.
3. **Hard caps instead of scalability.**
   - Timeline and Image Gallery switch off above 6 000 000 files.
   - The Timeline Details view has a 2000-stripe modal.
   - HTML reports allow 1000 thumbnails per page.

   → With indexed queries and immediate-mode drawing that skips anything off-screen, none of these caps are needed. Show progressive results instead.
4. **Side databases that drift out of sync.**
   - The Image Gallery keeps `drawable.db` per machine, with "out of date / rebuild?" prompts and a "listening" toggle.
   - The Timeline still carries leftover rebuild prompts.
   - Discovery and Data Source Summary keep caches with ad-hoc invalidation.

   → Keep every derived index in the main case DB (PostgreSQL for multi-user), maintained transactionally by the ingest pipeline. Push invalidation through one typed event bus (`DataChanged{kind, ids, data_source}`) that every view subscribes to. Remote nodes receive these events through PostgreSQL `LISTEN/NOTIFY`, which replaces Autopsy's ActiveMQ.
5. **Threading spread across several UI toolkits.**
   - Code mixes the Swing EDT, the JavaFX thread, ad-hoc `new Thread` calls and SwingWorkers.
   - It blocks on DB calls inside synchronized blocks.
   - Tasks race against refreshes.

   → Use one async runtime (tokio) or a rayon worker pool for DB and CPU work. Version every query with a generation counter so stale results are dropped. Keep UI state immutable on the egui thread and pass messages over channels.
6. **Modal-dialog-heavy UX.**
   - Wizards allow one report module per run.
   - Modal progress dialogs block the app.
   - Confirmation dialogs appear for routine actions: regroup warnings, rebuild prompts, ingest-running prompts.

   → Use non-modal side panels and a global **Jobs** panel showing progress and cancel controls for reports, rebuilds and searches. Replace prompts with inline banners such as "Data changed — refresh". Allow several report modules per run.
7. **Filter UX is inconsistent across tools.**
   - Timeline filters need Apply but are also applied by zooming.
   - In some places "unchecked = everything".
   - The CVT filter needs a manual refresh.
   - Geolocation's "last N days" counts back from the newest waypoint, not from today.
   - Discovery has filters that exist in the backend but are not shown in the UI.

   → Build one shared filter-model crate covering data sources, time range (absolute or relative, and saying explicitly what it is relative to), tags, hash hits, known status, score and types. Apply filters live with a debounce, show active-filter chips, and use the same widgets in every tool. Save filter sets as named presets per examiner.
8. **No persisted investigative state.**
   - Timeline pins and hides, CVT pinned graphs, gallery navigation history, Discovery searches and map views are all session-only.

   → Store "views" and "workspaces" per case and examiner in the DB so they reopen exactly. This works naturally with multi-viewport popouts: each popout window serialises its own view state.
9. **Multi-user gaps.**
   - Image Gallery seen-state semantics are ambiguous: marking a group unseen clears it for everyone.
   - The health monitor works only with PostgreSQL and only from the auto-ingest dashboard.
   - Report generation is not recorded per user.
   - The CR bulk insert drops `account_id`.

   → Record the examiner, node and timestamp on every user action (tags, categories, seen state, reports, persona edits). Make per-examiner versus collaborative views a simple query switch. Build health and performance metrics in, and make them viewable from the normal UI.
10. **SQL safety and correctness.**
    - Values are concatenated into SQL strings in CR instance queries, Image Gallery group queries and report keyword queries, so an apostrophe can break them.
    - Some CR count queries are malformed.
    - Case sensitivity is inconsistent between the CR `accounts` table and the instance tables.

    → Use parameterised queries everywhere (`sqlx`/`rusqlite` with compile-time-checked SQL where possible). Store one canonical normalised value per correlation attribute, and keep the raw value for display.
11. **Output-format correctness.**
    - CSV values are not escaped.
    - The body file contains an ingest-warning line.
    - Excel cells are capped at 32 767 characters and sheet names collide.
    - KML depends on online icon URLs.
    - CASE-UCO omits keyword hits.
    - The HTML report uses framesets.

    → Use standards-conformant writers (`csv`, `rust_xlsxwriter` in constant-memory mode, KMZ with embedded assets, a modern static HTML site with client-side search). Golden-file test every report format.
12. **Platform lock-in.**
    - Portable-case compression and "include application" work only on Windows.
    - Video playback depends on JavaFX codecs.
    - The map depends on JXMapViewer tile quirks.
    - The non-standard MBTiles layout only reads JPEG tiles.

    → Build these features cross-platform:
    - Pure-Rust zip/7z with split volumes.
    - ffmpeg for video thumbnails and playback.
    - A standard MBTiles/XYZ tile reader that handles PNG, JPEG and WebP.
    - An offline-first tile cache stored in the case folder, sized in MB.

### Cross-cutting architecture for postmortem (summary)

- **Single source of truth.** Use the case DB, plus the Central Repository DB for cross-case data, and no hidden per-tool databases.
  - Derived tables include `timeline_event`, `drawable`, `relationship` and `waypoint`.
  - These tables are written by ingest stages in the same transaction as the artifacts they derive from.
- **Query layer.** One `Query` API returns typed, paginated cursors. Every UI tool consumes it, and results are cached per (filter hash, data version).
- **Event bus.** A typed in-process bus feeds PostgreSQL `LISTEN/NOTIFY` for multi-user. Views subscribe and invalidate precisely.
- **Jobs framework.** Long operations implement `Job { progress, cancel, result }` and appear in the Jobs panel; this covers reports, rebuilds, CR imports, searches and exports. Jobs run headless as well, from the CLI or auto-ingest.
- **View-state persistence.** Each tool's state is serialisable, so it can back popout windows, saved workspaces and "open in new window".
- **Reporting as data.** Report modules consume the same query layer. They record a manifest (module, settings, scope, version, examiner, output hashes) in the DB so reports can be reproduced and defended in court.
- **Performance budgets.** Target first paint under 100 ms and every query under 300 ms at the 95th percentile on a 10 M-file case, with explicit pagination and no unbounded in-memory loads. Each tool gets a benchmark fixture at that scale in CI.
