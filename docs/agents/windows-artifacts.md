# Track: windows-artifacts (PLAN M6 — Windows OS artefacts in Recent Activity)

> **Historical spec** from the parallel-agent period (2026-10-17). The work it describes is merged or folded into other crates; current status is in `docs/PARITY.md`, current direction in `docs/DIRECTION.md`.

Status: **paused at a coordinator checkpoint** (2026-10-15). Parsers, analyses
and the Recent Activity glue are in and tested on generated data; the
real-image verification and the remaining items below are next.

## Done

### New crates
| Crate | What | Tests |
|---|---|---|
| `pm-registry` | regf hive parser over `ReadAt` (no mmap, no whole-file reads; bounded LRU cell cache): base block + checksum, `nk`/`vk`/`sk`, `li`/`lf`/`lh`/`ri` lists (hint-assisted case-insensitive lookups with scan fallback), big data (`db`), class names, walk with cycle guard; **transaction-log replay** for dirty hives — old format (`DIRT` dirty vector, XP–8) and new format (`HvLE` entries with Marvin32 hashes, 8.1+), sequence-number checks, hive growth; **deleted-key recovery** from free cells (with values); `builder` feature: hive writer (all list kinds, big data, deleted keys) and old/new log writers. Key names may contain `/`; only `\` separates paths. | 19 unit+integration, 2 proptests, fuzz target `registry_hive`, `#[ignore]` walk benchmark |
| `pm-lnk` | MS-SHLLINK parser (header times, link info local/network paths incl. Unicode, string data, env/tracker (machine id, droids, MAC)/known-folder/special-folder/Vista ID-list blocks, Autopsy's `getBestPath` order); shell items (root GUID folders, volumes, file entries with `BEEF0004` long names, created/accessed times and NTFS reference, network, URI, control panel, `CFSF` delegates); bounded OLE compound-file reader; jump lists: AutomaticDestinations (+ `DestList` v1 and v3+) and CustomDestinations; app-id names. `builder` feature: LNK, shell item, compound file, DestList, custom-destinations writers. | 11, 2 proptests, fuzz targets `lnk_parse`, `jumplist_parse` |
| `pm-prefetch` | Prefetch versions 17/23/26/30 (both file-info layouts)/31, run count, up to 8 run times, file names, volumes (device, serial, creation, directories); `MAM\x04`/`MAM\x84` with a native **LZXPRESS Huffman** decoder (MS-XCA). `builder` feature: prefetch writer + Xpress Huffman encoder (literals, short/long matches, multi-block). | 8, 2 proptests (round-trip, never-panic), fuzz target `prefetch_parse` |
| `pm-winartifacts` | Pure analyses: SYSTEM (computer name, hostname, domain/NV domain, environment, **time zone** incl. DST rules, last shutdown, page files, hibernation, crash dump, BitLocker boot status, upgrade history, USB devices, interface SSIDs (`autopsynic2` decoding), TCP/IP interfaces, BAM/DAM, Bluetooth, **ShimCache** XP/2003/Vista/7 x86+x64/8.0/8.1/10/11), SOFTWARE (version/edition/build/UBR/display version/install date/owner/org/product id, **product key** decode legacy + Win8, uninstall list, ProfileList, NetworkList profiles + signatures, LastLoggedOnUser/Winlogon/auto-logon password, Defender exclusions and switches, default browser), SAM (machine SID, users from F/V incl. created/last login/password reset+fail/login count/flags/hint/Internet name/security questions, built-in groups with member SIDs), NTUSER (network drives, Office ≤2007 `officedocs`, Adobe, Media Player, 7-Zip, Paint applets, MMC, WinRAR, Office 2010+ MRUs and TrustRecords, WSL, UserAssist (XP + 7 formats, ROT13, known folders), RunMRU, TypedURLs(+Time), TypedPaths, default browser), shellbags (NTUSER XP and UsrClass roots), Amcache (new and old layouts), RecentFileCache.bcf, `$I` v1/v2, INFO2, Zone.Identifier, embedded USB ID database (Autopsy's `usb.ids` 2022.05.20, interface lines handled). | 22 (incl. RBCmd's real `$I`/INFO2 files under `fixtures/recyclebin`) |

### Ingest glue (`crates/pm-modules/src/recent_activity/windows/`)
Appended to the Recent Activity step list (after the browsers, so
Zone.Identifier sees their downloads); all artifacts are written in one
transaction per extractor with duplicate checks on identifying attributes
(re-ingest adds nothing — tested). Associated objects (`TSK_ASSOCIATED_OBJECT`)
link to resolved targets.

| Step | Autopsy source | Output |
|---|---|---|
| Windows Registry | `ExtractRegistry` (+ RegRipper plugins) | `TSK_OS_INFO` (data source, once), `TSK_OS_ACCOUNT` per SID (SAM + ProfileList; see gaps), `TSK_ACCOUNT` (EMAIL from SAM InternetName), `TSK_INSTALLED_PROG`, `TSK_DEVICE_ATTACHED` (USB DB make/model), `TSK_WIFI_NETWORK` (interface SSIDs, wireless NetworkList profiles), `TSK_REMOTE_DRIVE`, `TSK_RECENT_OBJECT` (office legacy, Adobe/MP/7-Zip/applets/MMC/WinRAR/Office MRU + TrustRecords, TypedPaths), `TSK_PROG_RUN` (BAM/DAM, UserAssist, RunMRU), `TSK_BLUETOOTH_PAIRING`, `RA_SHELL_BAG`, `WSL_NAME`, `TSK_WEB_HISTORY` (TypedURLs, comment "Typed URL"), custom `PM_SHIMCACHE`, `PM_NETWORK_INTERFACE`, `PM_NETWORK_PROFILE`, `PM_DEFENDER_EXCLUSION` |
| Recycle Bin | `ExtractRecycleBin` | `TSK_RECYCLE_BIN` on `$R` (SID → user via SAM/ProfileList); INFO2 (`RECYCLER`/`RECYCLED`) records on `Dc<n>` files; records whose content is gone are kept on the index file with a comment |
| Jump Lists | `ExtractJumpLists` | `TSK_RECENT_OBJECT` per entry on the jump-list file (DestList access time, app name) — see gaps |
| Recent documents (LNK) | `RecentDocumentsByLnk` | `TSK_RECENT_OBJECT` (path, path id, LNK crtime) + target MAC times, machine id, MAC, arguments as `PM_*` attributes |
| OS Info | `ExtractOs` | `TSK_OS_INFO` on marker files of non-Windows OSes |
| Data Source Usage | `DataSourceUsageAnalyzer` | `TSK_DATA_SOURCE_USAGE` (OS drive, flash drive/Android card, DJI) |
| Zone.Identifier | `ExtractZoneIdentifier` | `TSK_WEB_DOWNLOAD` on the ADS (+ referrer) unless a browser reported it |
| Prefetch | `ExtractPrefetch` (external `mm_artifact_parser`) | `TSK_PROG_RUN` per distinct run time |
| Program execution caches | — (not in Autopsy) | custom `PM_AMCACHE_FILE` (path, SHA-1, times), `TSK_PROG_RUN` from RecentFileCache.bcf |

`pm-casedb/src/windows_artifacts.rs` adds `child_named`, `resolve_path` and
`files_by_extension` (extension-index lookups instead of `LIKE` scans).

### OS overview beyond Autopsy (coordinator request)
Autopsy's `TSK_OS_INFO` has name, domain, OS name, architecture, temp dir,
install time, system root, product id, owner, organization. Added on the same
artifact: `TSK_VERSION`, `TSK_HOST`, and custom `PM_TIMEZONE`,
`PM_UTC_OFFSET`, `PM_ACTIVE_UTC_OFFSET`, `PM_DAYLIGHT_NAME`, `PM_DST_RULES`,
`PM_DYNAMIC_DST_DISABLED`, `PM_LAST_SHUTDOWN`, `PM_EDITION`, `PM_BUILD`
(build.UBR), `PM_DISPLAY_VERSION`, `PM_INSTALLATION_TYPE`, `PM_BUILD_LAB`,
`PM_PRODUCT_KEY`, `PM_NV_DOMAIN`, `PM_PROCESSOR_IDENTIFIER`,
`PM_PROCESSOR_COUNT`, `PM_PAGE_FILES`, `PM_CLEAR_PAGEFILE`, `PM_HIBERNATION`,
`PM_CRASH_DUMP`, `PM_BITLOCKER`, `PM_UPGRADE_HISTORY`,
`PM_LAST_LOGGED_ON_USER`, `PM_LAST_LOGGED_ON_SAM_USER`, `PM_AUTOLOGON`,
`PM_AUTOLOGON_PASSWORD` (stored `DefaultPassword`), `PM_DEFENDER`,
`PM_DEFAULT_BROWSER`. Network interfaces and NetworkList profiles are separate
artifacts. None of these exist in Autopsy. "Last boot" is not derivable from
the hives alone (needs event logs / SRUM), so it is not reported.

## How it is tested
* Unit tests next to the code; integration tests in each crate's `tests/`
  built from the crates' own writers (hives, logs, LNK, shell items, compound
  files, prefetch, Xpress Huffman); RBCmd's real `$I`/INFO2 samples.
* proptest never-panic suites for every parser, plus round-trip for Xpress.
* `crates/pm-modules/tests/windows_artifacts.rs`: generated NTFS volume →
  `add_image` → Recent Activity ingest → artifact dump assertions → second
  ingest adds nothing.
* Fuzz targets (`fuzz/fuzz_targets/{registry_hive,lnk_parse,jumplist_parse,prefetch_parse}.rs`)
  compile (`cargo check`); no campaign run yet.

## Known gaps vs Autopsy / follow-ups (next)
1. **Opt-in real-data test** on `~/Downloads/4Dell Latitude CPi.E01`
   (`PM_REAL_DISK`) with verified values (computer N-1A9ODN6ZXK4LQ, owner
   Greg Schardt, XP SP?, accounts incl. "Mr. Evil", INFO2, prefetch v17) —
   not run yet.
2. **OS accounts** are posted as `TSK_OS_ACCOUNT` artifacts (one per SID)
   because the case DB has no OS-account tables (schema is owned by
   another track); Autopsy uses `tsk_os_accounts` + instances.
3. **Jump-list entries** are reported on the jump-list file; Autopsy adds each
   LNK stream as a derived file (needs the derived-files track's API).
4. **Recycle Bin**: recycled files are not re-created at their original
   location (Autopsy adds virtual directories + file rows; needs the
   derived-files track's insertion API).
5. **SRUM** (`ExtractSru`): waits for `pm-ese` (browsers track) on master.
6. **Thumbcache** (`ExtractThumbcache`, Windows-only in Autopsy, derived files
   only): not done.
7. Autopsy's RegRipper full-text **report** per hive is not produced.
8. Fuzz smoke runs, Xpress decoder verified only against our encoder (no
   real Windows 10 `.pf` sample available yet), benchmark numbers for
   `docs/PERF.md`, `docs/inventory/08-autopsy-tests.md` ticks.
9. RECmd batch-file checklist review for further keys (e.g. USBSTOR friendly
   names, MountedDevices, TypedPaths times, WordWheelQuery).
10. Ingest progress counter does not count artifacts written by these
    extractors (they write through their own batch, not the job writer).
