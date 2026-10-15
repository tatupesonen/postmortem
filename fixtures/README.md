# Test fixtures

Binary fixtures are kept small and are reproducible from the scripts in
`scripts/fixtures/`. Prefer generating fixtures in Rust test code
(`pm-testkit`) where an independent writer exists.

| File | Size | Produced by | Notes |
|------|------|-------------|-------|
| `ewf/small.E01` | 71 kB | `scripts/fixtures/make-raw.py src.raw 256` then `ewfacquire 20140817 -u -q -t small -c deflate:best -C CASE-42 -D "pm fixture" -e "Examiner Ä" -E EV-7 -N "notes here" -d sha1 src.raw` | EnCase 6 format, 8 chunks (compressed and stored), header2 with non-ASCII examiner; MD5 `b7d9562dc88251034f015fcb757c7ec7`, SHA-1 `9c6a5079404c0af11ca2a0cc382541de694bd1dd` |

## evtx/

Sample Windows event logs from the `evtx` crate's repository
(https://github.com/omerbenamram/evtx, `samples/`, commit 7479d02; MIT OR Apache-2.0):
`new-user-security.evtx` and `Security_short_selected.evtx` (70 kB each).
Used by `pm-evtlog` and the Windows Event Logs module tests.
