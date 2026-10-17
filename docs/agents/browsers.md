# Track `browsers` — status

> **Historical spec** from the parallel-agent period (2026-10-17). The work it describes is merged or folded into other crates; current status is in `docs/PARITY.md`, current direction in `docs/DIRECTION.md`.

Web browser artefacts for Recent Activity (PLAN.md M6). Reimplemented on
the Linux box after the earlier branch was lost (2026-10-17). Paused at
the coordinator's request; remaining work is listed at the end.

Architecture: **parsing** lives in the new crate `pm-browser` (pure
functions / record structs, no case database), the **ingest glue** in
`crates/pm-modules/src/recent_activity/web/` (to be ported onto
`pm-plugin-api` by the coordinator).

## Done

### `pm-browser` (new crate)
- `domain`: Autopsy `NetworkUtils.extractDomain` — Java `URL.getHost`
  semantics, `DomainTokenizer` trie walk over Autopsy's own
  `public_suffix_list.dat` (MPL-2.0, embedded), special-character and
  `DomainValidator` checks (TLDs from the PSL ICANN section), IPv4 rule;
  `strip_leading_dots` (`^\.+(?!$)`).
- `search_engines`: `SearchEngineURLQueryAnalyzer` with the embedded
  `SEUQAMappings.xml` (tokens attached by engine name, engine regex on the
  whole URL, Java `split` trailing-empty rule, stray-`%` escaping,
  `URLDecoder` UTF-8 decoding).
- `account_type`: `ExtractWebAccountType` rules (myBB, phpBB, Joomla,
  WordPress, service accounts) and the highest-rank-wins processor.
- `categories`: `DefaultPriorityDomainCategorizer` (search engines),
  `DefaultDomainCategorizer` (Autopsy's 101 k-line CSV, embedded gzipped,
  longest suffix), custom table, `DomainCategoryRunner` host extraction
  (Java host, then Autopsy's URL regex).
- `sqlite::open`: hardened connection to a temp copy (defensive mode, no
  trusted schema, triggers/views disabled, length limits, `query_only`,
  WAL replay); `has_column` (`Util.checkColumn`).
- `chromium`: browser path map (7 browsers incl. UC `%` names), History
  visits, downloads (pre-v30 / v30+ chains), cookies, logins, autofill
  (pre-8x / 8x, BLOB = encrypted), addresses (both schemas), favicons,
  Bookmarks JSON (recursive), Local State profiles, Secure Preferences
  extensions (Opera `opsettings`), malicious extension list (DRL 1.1).
- `firefox`: history (+referrer, hidden skipped), bookmarks, cookies
  (with/without `creationTime`), downloads (`moz_downloads` and v24+
  annotations), form history (v64 usage columns, `it`/`ts` skipped),
  `autofill-profiles.json`.
- `safari`: History.db, Bookmarks.plist (correct `URIDictionary.title`),
  Downloads.plist (seconds) via the `plist` crate (=1.10.1).
- `binarycookies`: Safari cookies, records read at their offsets.
- `index_dat`: IE 4–9 MSIECF over `ReadAt` — hash-table chains with loop
  and size limits, URL/LEAK/REDR records, cache directories, file names,
  HTTP header data, hits, allocation bitmap; pasco2 location split.
- `ie`: Favorites `.url` and cookie text files (first cookie, as Autopsy).
- `chrome_cache`: blockfile cache (`index`, `data_N`, `f_xxxxxx`):
  collision chains followed, correct block counts, multi-block keys,
  Autopsy's header parsing, payload location (`data_N__%08x`), brotli.
- `testing` feature: builders for index.dat, binarycookies, Chrome cache.

### Recent Activity glue (`pm-modules/src/recent_activity/`)
- Autopsy's extractor order (Windows steps → Chromium (+cache) → Firefox
  → IE → Edge → Safari → search queries → web account types → Zone /
  prefetch / execution → domain categories).
- Inbox: `Started <ds>`, `Finished <ds> - No errors reported / n errors
  found` (error lines in details), `<ds> - Browser Results` (Found / Not
  Found per browser). Details are plain text (the inbox does not render
  HTML).
- One keyset-paged candidate scan by file name
  (`CaseDb::files_matching_names`, new file `pm-casedb/src/browsers.rs`
  with `children_named`, `file_has_artifact_type`, `artifact_rows`)
  instead of Autopsy's dozens of `LIKE` scans.
- SQLite stores copied (streamed) to the case temp folder with `-wal` /
  `-journal` companions; batched artifact writes (2 000 per transaction)
  with `TSK_ASSOCIATED_OBJECT` links; `TSK_PATH_ID` via path resolution.
- Artifact types/attributes as Autopsy: TSK_WEB_HISTORY / BOOKMARK /
  COOKIE / DOWNLOAD / FORM_AUTOFILL / FORM_ADDRESS (+ EMAIL/PHONE account
  instances), TSK_SERVICE_ACCOUNT, TSK_ENCRYPTION_DETECTED (notable),
  custom TSK_LOCAL_STATE / TSK_CHROME_EXTENSIONS / TSK_FAVICON,
  TSK_WEB_CACHE (+ derived files for in-block payloads via
  `derive::DerivedTree`), TSK_WEB_SEARCH_QUERY, TSK_WEB_ACCOUNT_TYPE,
  TSK_WEB_CATEGORIZATION. Program names as Autopsy ("Google Chrome", …,
  "FireFox Analyzer", "Internet Explorer Analyzer", "Safari Analyzer").
- Setting `custom_web_categories` (`[{suffix, category}]`) — stands in
  for Autopsy's Custom Web Categories options until there is UI.
- Re-runs are idempotent (store skipped per artifact type; analyses
  deduplicate against existing artifacts).

## Tests
- `pm-browser`: 33 unit tests incl. proptest never-panics for
  binarycookies, index.dat, Chrome cache; pasco2 vectors (FILETIME,
  location split, allocation-bitmap invariant, empty files → 0 records).
- `pm-modules/tests/browsers.rs` (3): end-to-end on a generated NTFS
  image with Chrome (two profiles, Local State, WAL-only visits, both
  download schemas, cookies, logins, autofill + encrypted value, address,
  favicons, bookmarks, extensions incl. malicious, cache with derived and
  `f_` payloads), Firefox (current + old profile schemas), IE (index.dat,
  favorites, cookie), Safari (db, binary plists, binarycookies), search
  queries, account roles, categories, inbox texts, idempotent re-run;
  custom categories; hostile stores reported, not fatal.
- Full gate green at the commit.

## Not done / next
- **Edge legacy (ESE)**: `pm-ese` (pure-Rust ESE reader: header, small and
  large pages, B+-tree walk/seek, catalog, fixed/variable/tagged columns,
  multi-values, long values, 7-bit/Xpress decompression, test builder,
  tests, fuzz target) and `pm-browser::edge` (WebCache history
  containers, CookieEntryEx cookies, Spartan Favorites) were drafted but
  **not committed** (paused before they compiled/tested). Today
  `web/edge.rs` only reports Found / Not Found. Autopsy itself only reads
  these on Windows with ESEDatabaseView.
- Fuzz targets (`index_dat`, `binarycookies`, `chrome_cache`, `ese`,
  `safari_plist`) drafted, not added to `fuzz/`.
- Benchmark (`pm-browser/tests/bench.rs`: Chromium history, search
  analysis, index.dat throughput) drafted; numbers not yet in PERF.md.
- Port CR normalizer domain vectors (08 §7.1) as `extract_domain` tests;
  tick the pasco2 boxes in 08-autopsy-tests.md.
- Chrome Simple cache (`Cache_Data`), modern Chrome address tables
  (`addresses`/`local_addresses`), Firefox logins/sessions — beyond
  Autopsy.
- IDN (non-ASCII) domains are rejected by the validator port.

## Deviations from Autopsy (intentional)
Per-visit Chromium times and real referrers; recursive Chromium
bookmarks; Chromium login name in `TSK_USER_ID` (no `TSK_URL_DECODED`
quirk, no second `TSK_USER_NAME`); Chromium cookies also get
`TSK_DATETIME_CREATED`; profiles missing from Local State are read (user
= directory name), Local State users kept per `User Data` folder; WAL /
journal read; Firefox download paths keep `+`; Safari bookmark titles
and download times fixed; IE times UTC; Edge "found" only when files
exist; address `TSK_DATETIME_MODIFIED` kept (TSK drops it); malicious
extension URLs without CSV quotes; Chrome cache collision chains.
