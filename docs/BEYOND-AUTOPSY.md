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
