# Track: search (text extraction, keyword search, file-name search)

Status: **done and verified** for the scope below (branch of the search
track agent, 2026-10-17). Every slice passed the full quality gate before
it was committed. UI integration (the coordinator's job) is described in
[Query API for the UI](#query-api-for-the-ui).

## What is in

### `crates/pm-text` — streaming text extraction
Extractors push text into a `TextSink` (the sink can stop extraction:
cancellation, limits) and return the document metadata under Tika's names
(`Author`, `Last-Author`, `Creation-Date`, `Last-Save-Date`,
`Last-Printed`, `Title`, `Company`, `Application-Name`, `Producer`,
`pdf:PDFVersion`, …). `extract()` catches panics from third-party parsers.

| Module | Covers |
|---|---|
| `mime` | `kind_for_mime` in Autopsy's `TextExtractorFactory` order (plain text → web types ≤ 50 MB → SQLite → the formats Autopsy hands to Tika; octet-stream, `x-msdownload`, archives and video excluded); `sniff_mime` when File Type Identification has not run; `ARCHIVE_MIME_TYPES` |
| `encoding` | BOM → UTF-16 NUL-interleave heuristic → valid UTF-8 → `chardetng` over 100 KB; streaming `encoding_rs` decoding |
| `plain` | `text/*` |
| `strings` | Autopsy's two scanners: English-only (≥ 4 printable, one NUL tolerated) and international (UTF-8 / UTF-16LE / UTF-16BE per offset, script locking with Autopsy's own per-code-point table, generated into `strings/script_table.rs` by `scripts/fixtures/gen-strings-script-table.py` from `StringExtract.properties`) |
| `html` | tolerant streaming tokenizer/renderer (blocks → newlines, cells → tabs, entities, `<pre>`, charset from `<meta>`), Jericho-style metadata `Scripts`/`Links`/`Images`/`Comments`/`Others` in Autopsy's numbered format |
| `xml` | quick-xml walker, generic XML text |
| `office` | DOCX (body, headers, footers, notes, comments, deleted text), PPTX (slides, notes, comments), XLSX (shared strings, inline strings, sheets), ODT/ODS/ODP, EPUB (spine order); `docProps/core.xml`/`app.xml`/`meta.xml` metadata; per-part and total decompression caps |
| `ole` | Word 97+ piece table (field instructions dropped, results kept), Excel BIFF8 (SST with CONTINUE, LABELSST/LABEL/NUMBER/RK/MULRK/STRING), PowerPoint text atoms, Outlook `.msg` (body, headers, attachments), Summary/DocumentSummary property sets; encrypted documents reported |
| `rtf` | control-word scanner: destinations, `\'hh` in `\ansicpg`, `\uN` + `\ucN`, `\bin`, `\info` metadata |
| `pdf` | `pdf-extract` page by page into the sink, Info dictionary, PDF dates → UTC, empty-password decryption |
| `email` | RFC 822/MIME via `mail-parser`: text and HTML bodies, attachments extracted two levels deep, headers as metadata |
| `sqlite` | Autopsy's exact table-dump format from a temporary copy (with the `-wal` file), `trusted_schema = OFF` |
| `chunker` | Autopsy `Chunker` (30 720 / 31 744 / 32 760-byte base/window rules on text *and* lower-cased sizes, NFKC, Solr-invalid characters → `^`), push-based, streaming |
| lib | `metadata_block` (Autopsy's METADATA separator), `artifact_text` (`ArtifactTextExtractor`) |

### `crates/pm-search` — keyword lists, matching, indexes
* `keywords`: Autopsy's locked built-in lists with the exact regexes
  (phone, IP, e-mail, URL, credit card; only "Email Addresses" on by
  default), Autopsy `keywords.xml` import/export (BOM, missing flags
  default to true, export without ingest flags, locked lists never
  written), EnCase import (UTF-16, tree from child counts, `parent/child`
  list names, `(2)` suffixes, GREP skipped), `java_regex` (`\<`, `\>`,
  `\Q…\E`), `KeywordListStore` (global `keywords.xml` load/save/CRUD).
* `matcher`: Autopsy `InlineSearcher` semantics compiled once per job:
  Aho-Corasick literal prefilter + `RegexSet`, exact (word tokens,
  Lucene-like UAX #29), substring (whole surrounding word, boundary
  characters stripped), regex; phone/IP trimming with one-character
  step-back, e-mail ≥ 8 characters + IANA TLD, credit-card validation;
  per-text de-duplication; Autopsy snippets (`20 chars «hit« 20 chars`).
* `ccn`: `is_valid_ccn` (separators, IIN length table with Guava
  "last insert wins", grouping, Luhn incl. the all-zero rule),
  `ccn_hit_is_valid` (Autopsy's shrinking region), track 1/2 parsing,
  `BinDb` (binlist `ranges.csv`).
* `plan`: trigram formulas for substring and regex queries (regex-syntax
  HIR, exact-set analysis), property-tested never to exclude a match.
* `content::ContentIndex` (`<case>/Index/content`, Tantivy, pure Rust, LZ4
  doc store): one document per chunk (word tokens with positions,
  distinct lower-cased trigrams, stored text, overlap base) and one
  summary document per object (name/label, chunk count). Exact (phrase),
  substring (trigrams; trigram-dictionary regex for 1–2 characters) and
  regex (planned trigrams, else scan) queries, every candidate verified
  with the ingest `Matcher`; hits in a chunk's overlap belong to the next
  chunk; one row per object (lowest page), keyset-paged by object key,
  bounded memory, parallel verification, cancellable. `object_hits` for
  viewer navigation (ranges mapped back from lower-cased text), `page`,
  `num_chunks`, `stats`, `delete_object`, `delete_data_source`,
  `maybe_commit` (≤ once a minute).
* `names::NameIndex` (`<case>/Index/names`): positional trigrams of
  `\u{2}name\u{3}` and of the full path; substring = phrase of
  non-overlapping trigrams at fixed offsets (exact, no verification),
  prefix/exact anchored on the markers, 1–2 characters through the
  trigram dictionary; filters (data sources, deleted, directories,
  extensions, size range); keyset paging by file id; `count`;
  `catch_up(db)` indexes files above the last indexed id (commit
  payload), committing every million names; `add`/`add_new` for
  derived files; `delete_data_source`.
* `CaseIndex::open(case_dir)`: one shared instance per case per process;
  an `Index/VERSION` file wipes indexes of another layout version; a
  second process opens the indexes read-only.

### `crates/pm-casedb/src/search.rs` — read helpers (no schema change)
`keyword_hit_keys(file_id)` (identity of a file's keyword hits, one
query) and `artifacts_after(ds, after, limit)` (keyset paging).

### `crates/pm-modules/src/keyword_search/` — the ingest module
`KeywordSearchFactory` (`keyword_search`, "Keyword Search", **disabled by
default**), placed after the Picture Analyzer in `core_modules()`
(Autopsy's `PipelineConfig.xml` order: Hash Lookup, File Type, Embedded
File Extractor, EXIF, **Keyword Search**, E-mail, …).

Per file (Autopsy's decision tree, inventory 05 §1.3):
1. virtual directories: skipped;
2. unallocated blocks and carved `.txt` files: string extraction;
3. known files (`skip_known`), directories, empty files: metadata-only
   index entry (name, no search);
4. archives (`ARCHIVE_MIME_TYPES`): metadata only;
5. `application/octet-stream`: string extraction;
6. a format extractor (MIME from File Type Identification, else
   `sniff_mime`) + Autopsy's metadata block → chunk → inline search →
   index; documents with empty output count as failures;
7. failed and `.txt` (not carved): plain-text fallback;
8. still nothing: string extraction;
9. extractor metadata → `TSK_METADATA` (Autopsy's key priority table;
   dates read as UTC).

The file name is searched as page 0. Hits become `TSK_KEYWORD_HIT`
(score Likely Notable, configuration = list name; exact:
`TSK_KEYWORD_PREVIEW`, `TSK_KEYWORD`, `TSK_SET_NAME`, selector attribute,
`TSK_KEYWORD_SEARCH_TYPE` 0; substring/regex: `TSK_KEYWORD`,
`TSK_KEYWORD_REGEXP`, `TSK_SET_NAME`, `TSK_KEYWORD_PREVIEW`,
`TSK_KEYWORD_SEARCH_TYPE` 1/2; `TSK_ASSOCIATED_ARTIFACT` for artifact
hits). Credit-card hits become `TSK_ACCOUNT` instances through
`CaseDb::account_instance` (card number, track fields, holder, BIN
details, set name, preview, document id for unallocated space). Lists
with `ingest_messages` get a data message per hit term (first 20 per
term, as Autopsy's `QueryResults`).

A late data-source stage (`ARTIFACT_STAGE` = 9) indexes and searches the
text of every artifact of the data source except keyword hits and
associated-object links (Autopsy's `KwsDataArtifactIngestModule` /
`KwsAnalysisResultIngestModule`), then runs the file-name index
catch-up. `finish` commits and posts "Keyword Indexing Results" (and the
error/warning notices). Re-runs post nothing twice and re-index each
object in place.

Settings (`KeywordSearchSettings`, JSON): `lists` (full lists), 
`index_text` (default true; false = index only hit chunks and their
neighbours), `skip_known`, `extract_utf8`, `extract_utf16`, `scripts`,
`bin_database`, `index_artifacts`, `update_name_index`.

## Query API for the UI

Everything below is synchronous and may take from milliseconds to
seconds on large cases: call it from a background job, never the UI
thread (ADR-014), and pass a cancellation closure where offered. All
types are in `pm_search` unless noted.

### Opening the indexes
```rust
let ix: Arc<CaseIndex> = CaseIndex::open(case.dir())?; // shared per case and process
ix.content().reload()?;  // pick up commits made by an ingest job (cheap)
ix.names().reload()?;
```
`CaseIndex::open` is cheap after the first call (it returns the shared
instance). Indexes live in `<case>/Index/{content,names}`. On case
close nothing is needed; drop the `Arc`. When a data source is removed,
call `ix.delete_data_source(ds_id)` (see coordinator to-dos).

### File-name search (toolbar search box, Ctrl/⌘+F)
```rust
ix.names().catch_up(case.db(), &|| cancel.is_cancelled())?; // after loading a data source
let q = NameQuery {
    text: "invoice".into(),
    mode: NameMatch::Substring,   // or Prefix / Exact (whole name)
    in_path: false,               // true: match against "/dir/sub/name"
    ds_ids: vec![],               // restrict to data sources
    exclude_deleted: false,
    exclude_dirs: false,
    extensions: vec![],           // e.g. ["pdf", "docx"], no dot needed
    min_size: None, max_size: None,
};
let page: NamePage = ix.names().search(&q, after, 500)?; // file ids ascending
// page.ids → load rows with CaseDb::file / your keyset table query;
// page.next → pass as `after` for the next page (None: done)
let total = ix.names().count(&q)?; // for the "N files" label
```
Matching is case-insensitive and exact (no false positives), so the ids
can be shown directly. An empty `text` lists every file matching the
filters. `catch_up` is incremental (only files above the last indexed
id), so call it after `add_image`/`add_evidence` and on case open; the
Keyword Search module also runs it at the end of each ingest job.

### Ad-hoc keyword search (Autopsy's "Keyword Search" toolbar panel)
```rust
let q = TextQuery {
    term: "jean paul".into(),
    search_type: SearchType::Exact, // Exact | Substring | Regex
    ds_ids: vec![],                 // "Restrict search to the selected data sources"
};
let page: SearchPage = ix.content().search(&q, after, 200, &|| cancel.is_cancelled())?;
for h in &page.hits {
    // h.obj: ObjRef::File(id) | ObjRef::Artifact(id)
    // h.page: lowest page with a hit (0 = the name)
    // h.preview: Autopsy's "Keyword Preview" snippet ("…«hit«…")
    // h.hits: distinct hit terms (for "Save search results")
}
// page.next → continue (keyset by object); None → done.
```
* Errors: `SearchError::InvalidQuery` for an invalid regex or an exact
  term without words — show it like Autopsy's "Invalid query statement".
* Autopsy's guard messages: if `ix.content().stats()?.objects == 0` show
  "No files are in index yet…"; while ingest runs, confirm "Keyword
  Search Ingest is currently running…".
* Lists ("Lists" tab): run one `TextQuery` per keyword of each ticked
  list (`Keyword::search_type()` gives the type) and merge by object.
* "Save search results": for each object and hit term build the
  result with `pm_search::results::keyword_hit_artifact(&KeywordHitSpec
  { … })` (the same attributes the ingest module writes) and add it with
  `CaseDb::add_artifact`, skipping identities already in
  `CaseDb::keyword_hit_keys(file_id)`. The list name is empty for a
  single term, so the tree groups it as "Single Literal Keyword Search" /
  "Single Regular Expression Search". `snippet`/`hit` come from
  `ObjectHit::preview`/`hits` (or `Matcher::single(..).search(..)` on
  the page text for one snippet per hit term).
* "Files Indexed: N": `ix.content().stats()?.objects`.

### Extracted Text viewer
```rust
let n = ix.content().num_chunks(obj)?;            // None: not indexed (fall back to on-the-fly extraction)
let p: Option<PageText> = ix.content().page(obj, page)?; // page 1..=n (0 = name); p.text, p.base
let marks: Vec<PageHits> = ix.content().object_hits(obj, &q)?; // per page: byte ranges in p.text
```
* Pages are Autopsy's chunks; `p.text` includes the overlap window that
  repeats at the start of the next page — show `&p.text[..p.base]` to
  avoid the repetition, or the whole text with the overlap dimmed.
* Highlight ranges are byte offsets into `p.text` (original case), one
  per occurrence, sorted. "Matches on page: i of N" and next/previous hit
  come straight from `marks`.
* For a `TSK_KEYWORD_HIT` result build the query from its attributes:
  `TSK_KEYWORD_REGEXP` present → `Regex` with that term; else
  `TSK_KEYWORD_SEARCH_TYPE` 1 → `Substring` with `TSK_KEYWORD`; else
  `Exact` with `TSK_KEYWORD`. `TSK_ASSOCIATED_ARTIFACT` present → the
  object is `ObjRef::Artifact(that id)`. For a credit-card `TSK_ACCOUNT`
  use `Substring` with its `TSK_KEYWORD` (the number as found);
  `TSK_KEYWORD_SEARCH_DOCUMENT_ID` (unallocated space) names the page:
  `pm_search::docid::parse_document_id`.
* Files that are not indexed (Keyword Search not run, or text indexing
  off) can be shown by running the extractor now:
  `pm_text::kind_for_mime(mime, size)` → `pm_text::extract(kind, &*src,
  &mut ChunkingSink::new(|chunk| …), &opts)` and paging through the
  chunks, as Autopsy's `FileReaderExtractedText` does (not for
  unallocated/carved files, directories, empty files, archives,
  octet-stream).

### Keyword list management (Options → Keyword Search)
```rust
let path = config_dir.join("keywords.xml");          // the user's global lists
let mut store = KeywordListStore::load(&path)?;       // built-ins (locked) + user lists
store.put(KeywordList::new("Drugs", vec![Keyword::new("cocaine", SearchType::Exact)]))?;
store.get_mut("Drugs").map(|l| l.add_keywords([Keyword::new("heroin", SearchType::Exact)]));
store.remove("Drugs");
store.save(&path)?;                                   // atomic; locked lists never written
let imported = pm_search::keywords::import_file(&p)?; // Autopsy .xml or EnCase .txt
let xml = pm_search::keywords::to_xml(&lists, true);  // Autopsy's "Export"
```
* Validate new terms with `Matcher::single(term, search_type)` (an
  error carries the regex message). Unlike Autopsy, literals are never
  rejected as regexes (`C++` is a fine exact keyword).
* `KeywordList::add_keywords` skips duplicates and returns how many were
  added (Autopsy's "N keywords were successfully added / already in the
  list").
* Warn when a regex starts with `^` or ends with an unescaped `$`
  (Autopsy's boundary-character hint).

### Ingest settings (Add Data Source wizard / Run Ingest Modules)
`KeywordSearchFactory::info().enabled_by_default` is **false**: seed the
module's checkbox from it. Settings are
`pm_modules::keyword_search::KeywordSearchSettings` (serde JSON):
`lists` = the full lists ticked in the panel (default: built-ins where
`keywords::enabled_by_default(name)`, i.e. "Email Addresses"),
`index_text` ("Add text to the keyword index"), `skip_known`,
`extract_utf8`, `extract_utf16`, `scripts` (Autopsy `SCRIPT` names),
`bin_database` (optional binlist `ranges.csv` path),
`index_artifacts`, `update_name_index`.

## Tests
* `pm-text`: 67 unit tests (every extractor with hand-built documents:
  DOCX/XLSX/PPTX/ODT/EPUB zips, OLE compound files for Word/Excel/
  PowerPoint/MSG/property sets, RTF code pages and `\uN`, PDFs, MIME
  e-mail with HTML parts and attachments, SQLite dump format, encoding
  detection, both string scanners incl. block boundaries, chunker
  invariants incl. a proptest that chunk bases tile the sanitised text)
  plus `tests/never_panics.rs` (proptest: every extractor on random bytes
  and on random bytes after each format's magic number; strings and
  chunker).
* `pm-search`: 48 unit tests — Autopsy's `CreditCardValidatorTest` (all
  vectors of the nine methods), `MiniChunkHelperTest`, Autopsy's
  `test/input/notablekeywords.xml` import/export round trip, built-in
  regex positives/negatives (phone, IP, e-mail incl. TLD and length,
  URL, credit cards incl. track data), exact/substring/regex semantics,
  snippets, EnCase tree import, trigram plans (proptest: never excludes
  a match), content-index search/paging/overlap/highlight mapping/
  deletes/reopen, name-index modes/filters/paging/replacement, the
  `CaseIndex` registry and versioning.
* `pm-casedb`: `search.rs` unit test (hit identities, artifact paging).
* `pm-modules`: `tests/keyword_search.rs` runs real ingest jobs on a
  generated FAT32 image (text, HTML, DOCX with metadata, binary with a
  card number, zip, known file, a name hit, an empty file, a deleted
  file found in unallocated space, a pre-existing web-bookmark
  artifact): hit attributes, set names, search types, scores, credit-card
  account instance, `TSK_METADATA`, archives/known/empty files skipped,
  artifact-text hits with `TSK_ASSOCIATED_ARTIFACT`, ad-hoc and name
  queries on the indexes written by the job, the summary message, an
  idempotent second run; hits-only indexing; disabled by default.
  Plus unit tests for settings defaults, the metadata mapping and
  artifact value formatting.
* Fuzz targets (`fuzz/fuzz_targets/`): `text_ole`, `text_pdf`,
  `text_rtf`, `text_html` (HTML, XML, plain), `text_email`,
  `text_office` (OOXML, ODF, EPUB), `text_strings` (both scanners,
  chunker invariants, MIME sniffing), `keyword_lists` (XML round trip,
  EnCase import, matcher on arbitrary text). Smoke runs: see
  [Benchmarks and fuzzing](#benchmarks-and-fuzzing).

## Benchmarks and fuzzing
Release builds on the shared Linux machine (8 cores, other agents
building at the same time), recorded in `docs/PERF.md`:

| Benchmark | Result |
|---|---|
| Name index build, 10 M names | 195 k names/s, 1.59 GB |
| Name search, 10 M names, first page of 100 | rare substring 34 ms, prefix 46 ms, exact 27 ms, path substring 206 ms, two characters 34 ms; 700 k-match substring 225 ms (count 102 ms) |
| Content index add | 14.9 MB/s per index (4 writer threads), index 1.7× text |
| Ad-hoc search (8 456 chunks, first 100 verified objects) | exact common word 426 ms, rare word 39 ms, substring 4.5 ms, regex 5.4 ms |
| Inline matcher (built-ins + 101 keywords) | 38.9 MB/s per thread |
| Chunker | 132 MB/s per thread (NFKC quick check; was 32.5 MB/s) |
| String extraction | English-only 346 MB/s, international 31 MB/s |

Fuzz smoke runs (`cargo +nightly fuzz run -O <target> -- -max_total_time=60
-timeout=10 -rss_limit_mb=2048`), no crashes, timeouts or OOMs:
text_strings 227 k runs, text_html 353 k, text_rtf 1.34 M, text_email
1.65 M, text_office 604 k, text_ole 4.58 M, text_pdf 3.91 M,
keyword_lists 843 k.

Follow-ups: per-file timeouts for PDF; the content-index writer (about
15 MB/s per index) is now the slowest ingest stage after extraction.

## Decisions and deviations from Autopsy
Recorded in ADR-020 (`docs/DECISIONS.md`). In short: Tantivy indexes in
`<case>/Index/`, text stored once, trigram candidates verified with the
ingest matcher; one regex engine (Rust `regex`, Java syntax translated
for `\<`, `\>`, `\Q…\E`); results posted per file and idempotent;
Autopsy's IIN typo, lazy CCN separator and built-in regex quirks kept;
fixed: exact-match re-examination, snippet off-by-one, BIN end padding,
UTC metadata dates, locale-independent lower-casing, no stop words,
literals never rejected as regexes, per-hit inbox messages honoured
(first 20 per term). Page 0 is the name, chunks are 1…N.

Other choices:
* MIME: `file_type::mime_or_detect` (the type File Type Identification
  recorded, else detected and recorded), `pm_text::sniff_mime` only when
  no type exists. Documents whose extractor yields no text fall back to
  strings, like Tika's empty output in Autopsy; images therefore get
  string extraction, as in Autopsy.
* The artifact stage waits briefly (3 × 200 ms) for results that file
  modules posted just before it, because the ingest writer has no flush
  call (see to-dos).
* Keyword hits are de-duplicated per file by identity (keyword, set,
  type, regexp, associated artifact), read once per file with hits,
  instead of the writer's per-insert scan (quadratic for files with
  thousands of hits).

## Gaps vs Autopsy
* OCR (Tesseract; Autopsy only on Windows) and the "limited OCR" /
  "OCR only" options.
* Language detection and Japanese morphological analysis
  (`content_ja`, mini-chunks); CJK is indexed one ideograph per token
  (exact phrases work, as in Autopsy's `text` field).
* "Extract Unique Words" report (`ExtractAllTermsReport`).
* Word 6/95 and Excel 5 (BIFF5) cell text beyond labels; PowerPoint and
  PDF passwords; Tika-only formats (e.g. iWork, WordPerfect, Visio,
  OneNote, CHM).
* No per-file timeouts or process isolation for the PDF library (Tika
  has size-tiered timeouts); panics are caught, hangs are not. PDFs up to
  512 MiB are parsed in memory.
* Multi-user index service (`postmortem-indexd`); a second process gets
  the indexes read-only.
* Autopsy's ingest-time "regex hits per keyword" inbox messages are kept
  only for lists with `ingest_messages`; the old periodic search and its
  "update frequency" are gone in Autopsy too.

## Coordinator to-dos
* **UI** (see the query API above): toolbar name search, ad-hoc keyword
  search panel (single term and lists, data-source restriction, save
  results), Extracted Text viewer with page and hit navigation, Keyword
  Search options panel (lists CRUD, import/export, string-extraction
  scripts), the module's ingest settings panel (seed `disabled` from
  `enabled_by_default`), Keyword Hits tree grouping (Autopsy SQL in
  inventory 05 §1.12; keyword-hit attributes as listed above).
* Call `CaseIndex::open(case.dir())?.names().catch_up(db, …)` after
  adding a data source and on case open (cheap when current).
* `Case::delete_data_source` (pm-case) should also call
  `CaseIndex::delete_data_source(ds_id)`; deleting a case removes
  `<case>/Index` with the folder.
* `pm-ingest`: a `JobContext::flush()` (barrier through the writer)
  would let the artifact stage drop its 600 ms straggler wait.
* No schema changes are needed.
