# Track `browsers` — status

Web browser artifacts for Recent Activity (PLAN.md M6). Paused at the
coordinator's request after the first piece; the remaining items are
listed at the end.

## Done (first commit)

### New crates
- **`pm-browser`** — binary browser formats, positional reads through
  `ReadAt`, every offset bounds-checked:
  - `index_dat`: IE 4–9 MSIECF (`URL `/`LEAK`/`REDR` records via the
    hash-table chain, multiple tables, cycle/size limits, cache directory
    names, filename/data/hits).
  - `binarycookies`: Safari `Cookies.binarycookies` (records read at their
    offsets — Autopsy reads sequentially).
  - `chrome_cache`: blockfile cache (`index`, `data_N`, `f_xxxxxx`; cache
    addresses with the correct block count; collision chains followed;
    multi-block keys; HTTP header parsing as Autopsy).
  - `testing` builders for each format (used by tests and fuzz seeds).
- **`pm-ese`** — pure-Rust ESE (JET Blue) reader: header, small/large page
  formats, tags/nodes with key-prefix compression, B-tree walk with loop
  guards and a bounded page cache, catalog, fixed/variable/tagged columns,
  multi-values, separated long values (LV tree seek + chunk reassembly),
  7-bit ASCII/Unicode and XPRESS (LZ77) decompression, code pages.
  `testing` builder writes small/large-page databases with multi-level
  B-trees and long values.

### Case DB helpers (`crates/pm-casedb/src/browsers.rs`, no schema change)
`add_artifact_batch` (one transaction, returns ids, optional dedupe key),
`files_matching` (one keyset-paged candidate scan by name/prefix/extension),
`file_id_by_path` (case-insensitive walk with the `(parent_id, name)`
index), `artifact_attribute_rows` (artifacts + selected attribute values,
keyset paged).

### Recent Activity module (`crates/pm-modules/src/recent_activity/`)
Rewritten around one candidate scan (Autopsy runs dozens of `LIKE` scans)
and a batched sink that writes directly to the case DB (needed for
`TSK_ASSOCIATED_ARTIFACT` ids and for the post-passes to read back).
Inbox messages as Autopsy (`Started …`, `Finished … - …`, `… - Browser
Results` with Found/Not Found per browser).
- **Chromium** (all 7 Autopsy browsers incl. UC Browser `%` patterns):
  `Local State` profiles (`TSK_LOCAL_STATE`), extensions from `Secure
  Preferences` (`TSK_CHROME_EXTENSIONS`, Opera `opsettings`, malicious list
  flagging), history (per-visit time, real referrer — Autopsy bugs fixed),
  bookmarks (recursive, root key as comment), cookies, logins, autofill
  (pre-8x and 8x, encrypted BLOB comment), web form addresses (both
  schemas, `TSK_ACCOUNT` email/phone instances, `TSK_ENCRYPTION_DETECTED`
  NOTABLE), downloads (pre-v30 and v30+, `TSK_PATH_ID`, associated object),
  favicons (`TSK_FAVICON`), profile user attribution.
- **Chrome cache**: `TSK_WEB_CACHE` per entry; `f_` payloads get
  `TSK_PATH_ID` + associated object; in-block payloads carry
  `PM_CACHE_DATA_LAYOUT` (JSON file id/offset/length/name/content-encoding)
  for the derived-files integration (this track may not change pm-ingest).
- **Firefox**: history (+referrer, `place:` without domain), bookmarks,
  downloads (`moz_downloads` and v24+ annotations), cookies (v3 and later),
  form history (incl. v64), `autofill-profiles.json` addresses.
- **IE**: Favorites, cookie `.txt`, history from every `index.dat` natively
  (program "Internet Explorer Analyzer" as Autopsy).
- **Edge legacy** (via `pm-ese`, any platform): History containers,
  `CookieEntryEx_*` cookies, `iedownload` downloads (Autopsy's handler is
  dead code), `Spartan.edb` Favorites.
- **Safari**: History.db, Bookmarks.plist (correct title), Downloads.plist
  (seconds, not ms), binarycookies.
- **Search queries** (SEUQA exact semantics: engine regex on full URL,
  shared "Wiki" tokens, regex-token literals, Java split/decoding; program
  and domain from the source artifact), **web account roles**
  (`TSK_WEB_ACCOUNT_TYPE`), **domain categories** (`TSK_WEB_CATEGORIZATION`,
  priority search-engine list + the 100 k-line CSV embedded gzipped,
  computed in one streaming pass bounded by distinct hosts).
- **`extract_domain`** with the full public suffix list (Autopsy's copy)
  using the exact PSL algorithm, Java-URL host semantics, DomainValidator
  TLD check, IPv4 rule.

## Verification status (WIP commit)
The full gate (`scripts/gate.sh`) was **not** run to completion (stopped on
request). Verified per crate instead: `cargo fmt --all`; `cargo clippy -p
pm-modules -p pm-ese -p pm-browser -p pm-casedb --all-targets -D warnings`
clean; `cargo test` of those four crates green (plus the opt-in real-data
tests listed below). Unverified: workspace-wide clippy/tests (other crates
were not touched apart from `Cargo.toml`/`Cargo.lock` and the shared
`pm-casedb/src/lib.rs` + `pm-modules/src/util.rs` one-line edits).

## How it is tested
- `pm-browser`: unit + proptest never-panic for all three parsers; real
  data: plaso's MSHist `index.dat` matches libmsiecf record for record,
  plaso's `Cookies.binarycookies` (91 cookies, values asserted), opt-in
  plaso Chrome cache (217 entries as plaso, payload location).
- `pm-ese`: builder-based tests (small/large pages, multi-level trees,
  NULLs, LV, compression, loop detection, proptest); opt-in comparison with
  dissect.esedb on three real WebCache databases: 274 167 values identical
  (one value differs because of a dissect long-value bug, documented in the
  test). ESE fuzzing found an overflow (fixed, regression replayed).
- `pm-modules`: unit tests for domains, PSL, search engines/queries, roles,
  categories, IE location splitting, Edge helpers, Firefox paths, Safari
  bookmark tree; e2e demo-disk run; opt-in Dell image (`PM_REAL_DISK`): IE
  history per `index.dat` equals libmsiecf's URL counts (934 records over
  9 files) and a second run adds nothing.
- Fuzz targets `index_dat`, `binarycookies`, `chrome_cache`, `ese`
  (90–300 s smoke runs clean after the fix).
- Opt-in data: `scripts/fixtures/fetch-browser-testdata.py <dir>` then
  `PM_BROWSER_TESTDATA=<dir>`; `scripts/fixtures/ese_oracle.py` makes the
  dissect dumps.

## Remaining (next session)
- End-to-end module tests with generated NTFS images for Chromium (all
  tables, both download schemas, profiles, extensions, cache), Firefox,
  Safari, Edge (ESE builder), IE Favorites/cookies; idempotency per type;
  opt-in runs over plaso's Chrome/Firefox/Safari databases.
- Tick boxes in `docs/inventory/08-autopsy-tests.md`; benchmark numbers in
  `docs/PERF.md` (ESE read: 78 MB of WebCache in 0.36 s release).
- Chrome "Simple" cache (`Cache_Data/*_0`, `index-dir`) — beyond Autopsy.
- Brotli MIME for `f_` cache payloads (Autopsy sets
  `application/x-brotli`); derived files for in-block cache payloads once
  pm-ingest supports them.
- User-defined custom domain categorizer (Autopsy Options) — needs UI.
- Note for the coordinator: Recent Activity writes artifacts directly
  (batched transactions) so `IngestProgress.artifacts` does not count
  them; a `JobContext` API returning ids / flushing the writer would let it
  use the shared writer.

## Deviations from Autopsy (intentional)
Per-visit Chromium times/referrers; recursive bookmarks; login user name in
`TSK_USER_ID` (no `TSK_URL_DECODED` quirk); profile user prefers
`user_name`; Chromium cookies also get created/expiry times; Firefox
artifacts carry the Windows user; IE times are UTC; URLs are not
colon-stripped; malicious-extension references without CSV quotes; exact
PSL instead of Autopsy's trie; re-runs are idempotent.
