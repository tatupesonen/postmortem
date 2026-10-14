# 05 — Keyword Search, Recent Activity, Email: feature inventory

Source: Autopsy (read-only, `/Users/tatu/slop/autopsy`), modules `KeywordSearch`,
`Core/src/org/sleuthkit/autopsy/keywordsearchservice`, `Core/.../textextractors`
(used by KWS), `RecentActivity`, `thunderbirdparser`.
Target: **postmortem** (pure Rust, egui, own SQLite/PostgreSQL schema, Tantivy).

Conventions in this document:

* Regexes are shown **unescaped** (as the regex engine sees them, not as Java string literals).
* `TSK_*` names are Sleuth Kit `BlackboardArtifact.ARTIFACT_TYPE` / `BlackboardAttribute.ATTRIBUTE_TYPE`
  enum names. postmortem needs equivalent artifact/attribute type IDs (keep the names for report/CASE-UCO compatibility).
* "Autopsy bug" marks behaviour that is almost certainly unintended. Decide per item whether to replicate it
  (for result parity with Autopsy cases) or fix it. Each one is called out so the decision is explicit.

---

## 1. Keyword Search (KWS)

### 1.1 Components and responsibilities

| Java class | Role | postmortem equivalent |
|---|---|---|
| `KeywordSearchModuleFactory` | Creates three ingest modules sharing one `KeywordSearchJobSettings`: file module, data-artifact module, analysis-result module | one `kws` ingest module with three entry points |
| `KeywordSearchIngestModule` | Per file: choose extractor → extract text (+metadata) → chunk → inline search → index | `kws::ingest::process_file` |
| `KwsDataArtifactIngestModule` | Indexes and searches every new **data artifact** except `TSK_ASSOCIATED_OBJECT` | `kws::ingest::process_artifact` |
| `KwsAnalysisResultIngestModule` | Indexes and searches every new **analysis result** except `TSK_KEYWORD_HIT` (prevents feedback loops) | same |
| `Ingester` | Chunk loop, Solr document construction, "index only hit chunks" mode | `kws::index::Indexer` |
| `Chunker` | Splits text into ~32 KB overlapping chunks, NFKC + sanitising | `kws::chunk::Chunker` |
| `InlineSearcher` | Searches keyword lists on every chunk during ingest (no Solr round-trip); creates artifacts at job end | `kws::inline` |
| `LuceneQuery` / `RegexQuery` | Ad-hoc (and viewer) queries against Solr | Tantivy queries + Rust `regex` post-filter |
| `Server` | Starts/stops embedded Solr 8 (JVM), opens cores/collections, batches docs, counts | **delete** – Tantivy is in-process; multi-user needs a service (1.14) |
| `SolrSearchService` | `KeywordSearchService` impl (Core API): `index(Content)`, `deleteDataSource`, `deleteTextIndex`, `tryConnect`, open/close case resources, index upgrade | `kws::service` |
| `XmlKeywordSearchList`, `KeywordSearchList`, `KeywordList`, `Keyword` | Global keyword list storage, built-in lists | `kws::lists` |
| `EnCaseKeywordSearchList` | Import EnCase keyword `.txt` exports | importer |
| `HighlightedText`, `SolrIndexedText`, `FileReaderExtractedText`, `AccountsText`, `ExtractedTextViewer`, `ExtractedContentPanel` | "Extracted Text" content viewer with paging + hit navigation | egui viewer |
| `AdHoc*`, `Dropdown*` | Ad-hoc search toolbar (single term / lists) and results | egui search panel |
| `ExtractAllTermsReport` | "Extract Unique Words" general report | Tantivy term dictionary dump |
| `CreditCardValidator` (+ `Core/.../datamodel/CreditCards`) | Luhn + IIN length + digit grouping; BIN DB lookup | `kws::ccn` |
| `LanguageDetector`, `LanguageSpecific*`, `MiniChunkHelper` | Japanese detection & `content_ja` field | Tantivy `lindera` tokenizer field |

### 1.2 Settings

Global (Autopsy `ModuleSettings` property files in the user config dir; `KeywordSearchSettings`):

| Key | Default | Meaning |
|---|---|---|
| `SkipKnown` | `true` | Files with `known == KNOWN` (NSRL / known hash) get **metadata-only** indexing. UI: "Do not add files in NSRL (known files) to keyword index during ingest" |
| `EXTRACT_UTF8` | `true` | String extraction from unknown/binary files: UTF-8 |
| `EXTRACT_UTF16` | `true` | String extraction: UTF-16LE and BE |
| Scripts (one key per `SCRIPT` enum name = "true"/"false") | `LATIN_1` = true | Unicode scripts for string extraction. Supported: LATIN_1, LATIN_2, ARABIC, CYRILLIC, HAN, HIRAGANA, KATAKANA, HANGUL, ARMENIAN, BENGALI, KHMER, ETHIOPIC, GEORGIAN, HEBREW, LAO, MONGOLIAN, THAI, TIBETAN |
| `showSnippets` | `true` | Ask for highlighted previews in ad-hoc results ("will result in longer search times") |
| `ocrEnabled`, `limitedOcrEnabled` | `false`, `false` | Deprecated global fallbacks for the per-job OCR options |
| `IndexingServerPort` / `IndexingServerStopPort` | `23232` / `34343` | Local Solr HTTP / Jetty stop port |
| NB pref `IndexingMaxShards` | `0` (= number of live Solr nodes) | Multi-user: shards per new collection |
| NB pref `IndexingNumThreads` | `5` | (ConcurrentUpdateSolrClient threads – effectively unused) |
| NB pref `IndexingDocumentQueueSize` | `30` | Docs buffered before an `add` batch is sent (admin-only, no UI) |
| NB pref `SolrConnectionTimeoutMs` | `0` (unlimited) | Socket + connect timeout |
| Core pref `SolrMaxJVMSize` | 2048 MB (64-bit) / 512 MB (32-bit) | Solr heap (`-Xmx`) |
| Core prefs `Solr8ServerHost`/`Solr8ServerPort` | `""` / `8983` | Multi-user Solr 8 server |
| Core prefs `IndexingServerHost`/`IndexingServerPort` | `""` | Legacy multi-user Solr 4 server |

The global settings panel also shows "Files in keyword index" and "Chunks in keyword index" counters, and an "Ingest is ongoing, some settings will be unavailable until it finishes." warning (settings locked during ingest).

Per ingest job (`KeywordSearchJobSettings`, serialised with the ingest profile):

| Field | Default | Meaning |
|---|---|---|
| `namesOfEnabledKeywordLists` | all lists **except** "Phone Numbers", "IP Addresses", "URLs", "Credit Card Numbers" (so "Email Addresses" + user lists are on) | lists searched inline |
| `namesOfDisabledKeywordLists` | empty | lists explicitly off |
| `ocrEnabled` | global (`false`) | OCR images and embedded images in PDF/Office |
| `limitedOCREnabled` | `false` | OCR only PDF/Office docs and images that are > 100 KiB (`LIMITED_OCR_SIZE_MIN = 100*1024`) or are `DERIVED` files. Other OCR-capable files are **skipped entirely** (not even indexed) |
| `ocrOnly` | `false` | Skip every file whose extractor would not use OCR |
| `indexToSolr` | `true` | "Add text to Solr Index". If false: inline search only; only chunks around hits are indexed (1.6) |

Job panel labels: "Enable Optical Character Recognition (OCR)", "Only process PDFs, MS Office docs and images which are over 100KB in size or extracted from another file (Beta)", "Only index text extracted using OCR", "Add text to Solr Index", list checkboxes "Select keyword lists to enable during ingest:", plus read-only display of string-extraction scripts and encodings.

### 1.3 File ingest pipeline – exact decision tree

`startUp`:
1. If `indexToSolr`: index core must be open, else fail "The index could not be opened or does not exist." Index schema must be compatible (same **major** version as current `2.3`) else "Adding text no longer supported for schema version X".
2. First module instance per job (ref-counted by job id):
   * multi-user: read `solrserver.txt`/prefs (1.14), `tryConnect(host, port)`; bad port → "Invalid port number."
   * single-user with Solr: check local Solr process running and run a real query (`*:*`, rows 0) to detect "status OK but 404".
   * Inbox warnings: `!indexToSolr` → "Solr indexing is disabled." / "Text indexing is disabled. See user manual section 'Limitations of Ad Hoc Keyword Search'…"; else if no enabled list has a keyword → "No keywords in keyword list." / "Only indexing will be done and keyword search will be skipped…".
3. Build `StringsConfig` from global UTF8/UTF16/scripts.

`process(file)`:
1. Not initialised → status `SKIPPED_ERROR_INDEXING`.
2. `VIRTUAL_DIR` → skip.
3. Pick text extractor (1.4) with `ImageConfig{ocrEnabled}` + cancellation terminator. MIME = `FileTypeDetector.getMIMEType(file).trim().toLowerCase()`.
4. If OCR enabled:
   * `ocrOnly` and (no extractor or extractor won't OCR) → return (nothing indexed).
   * `limitedOCR` and extractor will OCR and file is **not** a limited-OCR file → return (nothing indexed).
     Limited-OCR file = MIME ∈ {`application/pdf`, `application/msword`, `application/vnd.openxmlformats-officedocument.wordprocessingml.document`, `application/vnd.ms-powerpoint`, `…presentationml.presentation`, `application/vnd.ms-excel`, `…spreadsheetml.sheet`} OR (MIME starts `image/` AND (size > 102400 OR type == DERIVED)).
5. `SkipKnown && known == KNOWN` → `searchFile(indexContent=false)`; else `searchFile(indexContent=true)`.

`searchFile`:
1. Type `UNALLOC_BLOCKS` / `UNUSED_BLOCKS`, or (`CARVED` and extension `txt`, case-insensitive) → **string extraction** regardless of MIME → `STRINGS_INGESTED`.
2. `!indexContent` OR directory OR size 0 → metadata-only doc (id=`<objId>`, lower-cased `file_name`, empty content) → `METADATA_INGESTED`.
3. MIME ∈ `ARCHIVE_MIME_TYPES` → metadata only (archives are handled by the extractor module). List (all `application/`): `x-7z-compressed, x-ace-compressed, x-alz-compressed, x-arj, vnd.ms-cab-compressed, x-cfs-compressed, x-dgc-compressed, x-apple-diskimage, x-gca-compressed, x-dar, x-lzx, x-lzh, x-rar-compressed, x-stuffit, x-stuffitx, x-gtar, x-archive, x-executable, x-gzip, zip, x-zoo, x-cpio, x-shar, x-tar, x-bzip, x-bzip2, x-lzip, x-lzma, x-lzop, x-z, x-compress`. Note `x-executable` is included → ELF binaries get no string extraction.
4. MIME == `application/octet-stream` → string extraction.
5. Extractor present → extract text **concatenated with a metadata block** → chunk/search/index. Success → `TEXT_INGESTED`; extraction failure → `SKIPPED_ERROR_TEXTEXTRACT`; `IngesterException` → `SKIPPED_ERROR_INDEXING`.
6. No text yet AND extension `txt` AND not CARVED → `TextFileExtractor` (encoding detection) fallback.
7. Still no text → string extraction.
8. Extractor metadata non-empty → `TSK_METADATA` data artifact (1.4.3), created **after** indexing.

Metadata block appended to the extracted text (so metadata is searchable):

```
\n\n------------------------------METADATA------------------------------\n\n
<key>: <value>          (entries sorted by key, joined with "\n")
```

`shutDown` (last instance per job): `InlineSearcher.makeArtifacts(job)` → `cleanup(job)` → commit → log counts → inbox "Keyword Indexing Results" HTML table: "Files with known types" (TEXT), "Files with general strings extracted" (STRINGS), "Metadata only was indexed", "Error (indexer)", "Error (text extraction)", "Error (I/O)". Index errors > 0 → error notification "Keyword index service had errors ingesting {0} files."; else text/IO errors → warning. **On cancellation nothing is created** (in-memory hits are dropped).

### 1.4 Text extraction (Core `textextractors`, used by KWS)

`TextExtractorFactory.getExtractor(content, ctx)`:
* `AbstractFile` → first supported of `TextFileExtractor` → `HtmlTextExtractor` → `SqliteTextExtractor` → `TikaTextExtractor` (Tika last).
* `BlackboardArtifact` → `ArtifactTextExtractor`.
* `Report` → `TikaTextExtractor` (but its `isSupported()` is false for non-files → reports fail; Autopsy bug).
* `getStringsExtractor` → `StringsTextExtractor`.

| Extractor | Supported when | Output |
|---|---|---|
| `TextFileExtractor` | MIME (stored, else detector) == `text/plain` | Whole file decoded with `EncodingUtils.getEncoding(file)` (charset detection); unknown → UTF-8 |
| `HtmlTextExtractor` | MIME ∈ {`application/javascript`, `application/xhtml+xml`, `application/json`, `text/css`, `text/html`, `text/javascript`} AND size ≤ 50,000,000 bytes | Jericho `Renderer`: newline `\n`, no hyperlink URLs, no font decoration, no alt text, no wrapping. Metadata keys `Scripts`, `Links`, `Images`, `Comments`, `Others`, each a numbered list `"\n1) …\n2) …"` of `<script>` attrs+contents, `<a>` tag contents, `<img>` tag contents, comments, and any other tag that has attributes (`name:content`) |
| `SqliteTextExtractor` | MIME == `application/x-sqlite3` | Per table: `"\n<table>\n\n\t" + col1 + " " + … + colN + "\n"`, then rows `"\t" + v1 + " " + … + vN + "\n"`; BLOBs → empty |
| `ArtifactTextExtractor` | always | Per attribute: `<attribute display name> : <value>` + line separator; DATETIME formatted with case display timezone |
| `TikaTextExtractor` | file MIME ∉ {`application/octet-stream`, `application/x-msdownload`} ∪ archive list, not `video/*` (except `video/x-flv`), not `application/x-sqlite3`, and MIME ∈ Tika supported types | Tika `AutoDetectParser`, recursive into embedded documents, SAX DOCX/PPTX. Timeout by size: < 1 MiB 60 s; < 10 MiB 1200 s; < 100 MiB 3600 s; else 10800 s. Empty output → error (falls back to strings). Metadata = all Tika metadata (obtained by a **second full parse**) |
| `StringsTextExtractor` | `extractUTF8 || extractUTF16` | 1.4.2 |

#### 1.4.1 OCR (Tesseract)

* Only on **64-bit Windows** with bundled `Tesseract-OCR/tesseract.exe` → OCR is silently unavailable on Linux/macOS.
* Language packs: every `*.traineddata` in `<userdir>/ocr_language_packs`, joined with `+` for `-l` (ImageConfig may override).
* `willUseOCR()` = OCR supported AND (MIME starts `image/` OR MIME is one of the 7 PDF/Office types above).
* Images: file written to `<case>/Temp/<escaped(objId+name)>`; run
  `tesseract "<in>" "<case temp>/<objId>tess_output" --tessdata-dir <packs> -l <langs>`; stream `<out>.txt` (deleted on close). Timeout 30 min (`ImageConfig` `TimedProcessTerminator(1800 s)`) combined with ingest cancellation, polled every 1 s.
* PDF/Office: Tika `TesseractOCRConfig` (tessdataPath, tesseractPath, language) and `PDFParserConfig.OcrStrategy.AUTO` (OCR a page only when text extraction yields too few chars or too many unmapped glyphs).

postmortem options: Tesseract via `leptess`/`tesseract-rs` (C++ dependency) or pure-Rust `ocrs` (rten models; Latin-focused). Make OCR cross-platform; keep "limited OCR" semantics. PDF rasterisation: `pdfium-render` (C dep) or pure-Rust `hayro` (young).

#### 1.4.2 String extraction (binary, unallocated)

* If the only enabled script is `LATIN_1` → **EnglishOnlyStream**: 64 KiB reads; printable = bytes 32..126 or 9 (TAB); runs of ≥ 4 printable chars (`MIN_PRINTABLE_CHARS`) are emitted followed by `\n`. A *single* NUL inside a run is tolerated and dropped (so UTF-16LE ASCII `a\0b\0c\0d\0` becomes `abcd`). Runs straddling buffer boundaries continue.
* Otherwise **InternationalStream** using `StringExtract` (1 MiB reads): at each offset (skipping `00 00` pairs) try UTF-16LE and UTF-16BE (even offsets only) and UTF-8; keep the decode with most chars; accept if ≥ 4 chars (`MIN_CHARS_STRING`); chars must belong to an enabled script (per-codepoint table in `StringExtract.properties`; "generic" chars such as digits/punctuation allowed in any run; a run may not switch scripts; LATIN_1 is implied by LATIN_2). Results joined with `\n`.
* `StringsTextExtractor` defaults to `LATIN_2` + UTF-8 when no config is passed.

postmortem: implement both natively (block scan with lookup tables, `memchr` for NUL detection); script table from the `unicode-script` crate.

#### 1.4.3 `TSK_METADATA` artifact from extractor metadata

Tika/Jericho metadata keys → attributes; per attribute type the key earliest in its list wins:

| Attribute | Metadata keys (priority order; Tika property constants) |
|---|---|
| `TSK_DATETIME_MODIFIED` | `Last-Save-Date`, `TikaCoreProperties.MODIFIED`, `FileSystem.MODIFIED`, `DublinCore.MODIFIED`, `PDF.DOC_INFO_MODIFICATION_DATE`, `PDF.PDFVT_MODIFIED`, `XMP.MODIFY_DATE`, `XMPDM.AUDIO_MOD_DATE`, `XMPDM.METADATA_MOD_DATE`, `XMPDM.VIDEO_MOD_DATE` |
| `TSK_USER_ID` | `Last-Author`, `Office.LAST_AUTHOR`, `TikaCoreProperties.MODIFIER` |
| `TSK_DATETIME_CREATED` | `Creation-Date`, `TikaCoreProperties.CREATED`, `FileSystem.CREATED`, `DublinCore.CREATED`, `IPTC.DATE_CREATED`, `Office.CREATION_DATE`, `PDF.DOC_INFO_CREATED`, `Photoshop.DATE_CREATED`, `XMP.CREATE_DATE` |
| `TSK_ORGANIZATION` | `Company`, `DublinCore.PUBLISHER`, `IPTC.ORGANISATION_NAME`, `OfficeOpenXMLExtended.COMPANY` |
| `TSK_OWNER` | `Author`, `TikaCoreProperties.CREATOR`, `DublinCore.CREATOR`, `Office.INITIAL_AUTHOR`, `Office.AUTHOR`, `Photoshop.AUTHORS_POSITION`, `PDF.DOC_INFO_CREATOR` |
| `TSK_PROG_NAME` | `Application-Name`, `Producer`, `OfficeOpenXMLExtended.APPLICATION`, `RTFMetadata.EMB_APP_VERSION` |
| `TSK_LAST_PRINTED_DATETIME` | `Last-Printed`, `OfficeOpenXMLCore.LAST_PRINTED` |
| `TSK_DESCRIPTION` | `Title`, `DublinCore.TITLE`, `IPTC.TITLE`, `PDF.DOC_INFO_TITLE` |
| `TSK_VERSION` | `PDF.PDF_VERSION`, `OfficeOpenXMLCore.VERSION` |

Values empty or starting with a space are dropped. DATETIME: replace `T`→space, delete `Z`, parse `yyyy-MM-dd HH:mm:ss` in the **JVM default timezone** (Autopsy bug: should be UTC); failures dropped. Artifact: data artifact `TSK_METADATA` on the file. Since postmortem won't use Tika, define its own key names per extractor (PDF Info dict, OOXML `docProps/core.xml`/`app.xml`, OLE `SummaryInformation`) and map to the same attributes.

postmortem replacements for Tika: `lopdf`/`pdf-extract` (PDF), own OOXML reader (`zip` + `quick-xml`) for docx/pptx/xlsx or `calamine` for spreadsheets (xls/xlsx/ods), `cfb` for legacy OLE2 doc/xls/ppt text streams, `rtf-parser`, `html2text`/`scraper` for HTML, `mail-parser` for eml/msg bodies, `epub`. Run in a sandboxed worker with the same size-tiered timeout and a memory cap.

### 1.5 Chunking (`Chunker`) – exact algorithm

Constants (UTF-8 byte sizes):

```
MAX_TOTAL_CHUNK_SIZE            = 32760   // < Lucene max term 32766: content_str is ONE indexed term
MINIMUM_BASE_CHUNK_SIZE         = 30*1024 // 30720
MAXIMUM_BASE_CHUNK_SIZE         = 31*1024 // 31744
WHITE_SPACE_BUFFER_SIZE         = 512
READ_CHARS_BUFFER_SIZE          = 512 chars
MAX_CHAR_SIZE_INCREASE_IN_BYTES = 10      // lower-casing can grow a char's UTF-8 size
```

Per chunk:
1. **Base**: read 512-char blocks while the next block keeps both the original and the lower-cased UTF-8 size < 30720 (an overflowing block is pushed back). A trailing high surrogate is pushed back so surrogate pairs are never split. Then read single chars until a whitespace char is read or size reaches 31744−10.
2. **Window** (overlap): same, up to 32760−512 bytes, then to whitespace up to 32760−10. Total ≈ 32 KB with ≈ 1–2 KB overlap.
3. If EOF was reached the window is part of the base (`baseChunkSizeChars` = whole length); otherwise the window is pushed back (`PushbackReader` capacity 32760 chars) and re-read as the start of the next chunk.
4. `chunk_size` = **base length in chars (UTF-16 units)**, used to discard hits in the overlap (the next chunk reports them).

Sanitising applied to every segment: NFKC normalise → round-trip through the UTF-16 encoder (replaces unpaired surrogates) → replace every UTF-16 unit that is not "valid Solr UTF-8" with `^`:
valid ⇔ `(c <= 0xFDD0 || c >= 0xFDEF) && (c > 0x1F || c ∈ {0x9,0xA,0xD}) && c % 0x10000 != 0xFFFF && c % 0x10000 != 0xFFFE`.
The chunk also keeps a lower-cased copy (`String.toLowerCase()`, default locale – Autopsy bug on Turkish locale).

IDs: chunk docs `<objId>_<n>` with n starting at **1**; parent doc `<objId>` with `num_chunks = n`. Artifacts use their artifact id (negative in current TSK) as objId.

Edge cases: lower-casing can change length (`İ` → `i̇`), so offsets in the lower-cased text ≠ original. Autopsy computes hit offsets and snippets on the lower-cased copy (previews are lower-case) but `chunk_size` is in original chars.

postmortem: chunking is needed for (a) bounded memory between extractor and searcher and (b) viewer paging. Tantivy has no 32 KB term limit (do **not** index pages as single terms). Keep ~32 KB *pages* with configurable overlap (≥ longest expected hit, default 1 KiB), measure in bytes, store page text in a compressed store keyed by `(obj_id, page)`; index tokens with positions; regex runs over stored text (1.8, 4.1).

### 1.6 Indexing (`Ingester`) and the Solr document model

Solr configset `AutopsyConfig`, schema `Autopsy Keyword Search` **version 2.3**, `luceneMatchVersion 8.6.3`. Schema history: 1.5 `content_ws`; 1.6 `num_chunks`; 1.7 `_version_`; 1.8 `content_str` for regex; 2.0 `chunk_size`; 2.1 `content_str` populated with **lower-cased** text by Autopsy (no copyField); 2.2 `text_ja`, `content_ja`, `language`.

| Field | Type | Indexed / stored | Content |
|---|---|---|---|
| `id` | string | I+S, uniqueKey | `<objId>` (parent/metadata doc), `<objId>_<chunk>`, `<objId>_<chunk>_mini` |
| `image_id` | string | I, required | data source obj id (`-1` if unknown; reports without data source `-1`) |
| `_version_` | long | I+S | SolrCloud |
| `content` | string | neither | raw chunk; `copyField` → `text` |
| `file_name` | text_general | stored | lower-cased file name; `copyField` → `text` and → `content_str` |
| `text` | text_general (multi) | I+S, term vectors + positions + offsets | default field: chunk text + file name |
| `content_str` | string (multi) | I+S | **lower-cased chunk** (one term) + copy of file name — regex/substring search |
| `content_ws` | text_ws | — | legacy, unused |
| `content_ja` | text_ja (multi) | I+S, term vectors | Japanese-analysed chunk + file name (only if language = ja) |
| `language` | string | stored | `ja` |
| `num_chunks` | int | I+S | parent doc |
| `chunk_size` | int | I+S | chunk docs: base length (chars) |
| `ctime`,`atime`,`mtime`,`crtime` | tdate | neither | sent (ISO-8601) but discarded |

Analyzers:
* `text_general` (index & query): `StandardTokenizer` → `LimitTokenCount(200000)` → `StopFilter(stopwords_en.txt, ignoreCase)` → `LowerCase`. Stop words: `a an and are as at be but by for if in into is it no not of on or such that the their then there these they this to was will with` (+ placeholder `stopworda stopwordb`). **Consequence:** exact-match searches for stop words find nothing.
* `text_ja`: `JapaneseTokenizer(mode=search)` → `JapaneseBaseForm` → `JapanesePartOfSpeechStop(lang/stoptags_ja.txt)` → `CJKWidth` → `Stop(lang/stopwords_ja.txt)` → `JapaneseKatakanaStem(minimumLength=4)` → `LowerCase`.
* No CJK bigram analysis for Chinese/Korean: StandardTokenizer emits one token per Han ideograph, Hangul per word; exact match works as a phrase query, substring/regex via `content_str`.

Document emission (`Ingester.search`):
* Language detection on the **first chunk only**: first `min(baseLen, 4096)` chars → Optimaize `langdetect` (all built-in profiles, `forDetectingOnLargeText`); only `ja` is acted on (schema ≥ 2.2).
* Each chunk: inline keyword search (1.7). If `indexToSolr`: chunk doc `{id:<obj>_<n>, image_id, file_name?, chunk_size, content:<chunk>, content_str:<lower chunk>}`; if Japanese also `content_ja:[chunk, file_name]`, `language:"ja"`, and (when more chunks follow) a **mini-chunk** doc `{id:<obj>_<n>_mini, content_ja:<overlap text only>, language}` whose term frequency is subtracted to ignore overlap hits.
* `!indexToSolr` (inline-only): sliding list; a run of chunks with hits is indexed **together with the chunk before and the chunk after it** so the viewer has context. Files without hits produce nothing in the index.
* Finally (not cancelled and something indexed): parent doc `{id:<obj>, image_id, file_name, num_chunks}`.
* All string field values are sanitised (`Chunker.sanitize`).
* Files: the **file name** is searched inline as "chunk 0" (`searchString(name, objId, 0)`); artifacts skip this.
* Metadata-only: `{id:<obj>, image_id, file_name:lower(name), content:"", content_str:""}`.
* Artifacts: `indexMetaDataOnly(artifact, "<artifact display name>_<artifactId>")`, then chunk docs keyed by artifact id.

Batching / commit (`Server.Collection`): buffer of 30 docs → `add` (10 retries, 10 s apart); background flush every 10 min; after **5 consecutive failed batches all further indexing for the case is skipped** ("Unable to add data to text index. All future text indexing for the current case will be skipped."). Commit: at job end (`commit(waitFlush, waitSearcher)`), on `index(Content)` when no ingest runs, plus Solr `autoCommit` every 300 s (no new searcher). `ramBufferSizeMB=256`, native locks.

Suggested Tantivy schema:

```
obj_id:   i64  FAST|INDEXED|STORED   // file obj id or artifact id
page:     u64  FAST|INDEXED|STORED   // 0 = name/metadata doc
ds_id:    i64  FAST|INDEXED          // data source (delete-by-data-source, filters)
kind:     u64  FAST                  // file / artifact / report
text:     TEXT (unicode tokenizer + lowercase, positions; stop words optional)
text_ja:  TEXT (lindera ipadic, positions)       // only when lang = ja
name:     TEXT + raw lower STRING
base_len: u64  STORED                 // overlap cut-off in bytes
```
Page text lives outside Tantivy (zstd blobs). Default: no English stop words (fixes the stop-word blind spot), compatibility flag available.

### 1.7 Inline search during ingest (`InlineSearcher`) — replaces the old periodic search

Autopsy ≥ 4.21 no longer runs the periodic Solr search during ingest (the old `IngestSearchRunner` and "update frequency" option are gone; only stale comments remain). Each chunk is searched in memory as it is produced; artifacts are created once at job end. The only remaining periodic behaviour is the 10-minute Solr batch flush.

Per chunk (`searchChunk` uses the **lower-cased** chunk; file name is searched raw):
1. For each enabled list → each keyword:
   * literal (exact or substring): quick reject unless `containsIgnoreCase(text, term)`.
   * regex: `Pattern.compile(term, CASE_INSENSITIVE)` (**compiled per chunk**); quick reject unless `find()`; invalid regex silently ignored.
2. Candidate → `createKeywordHits`:
   * **Exact match** (literal + whole word): tokenise keyword and text with Lucene `StandardAnalyzer` (UAX#29 + lowercase, no stop words in Lucene 8); match consecutive tokens; hit = original substring from first token start to last token end. (Autopsy bug: after a partial multi-token mismatch the consumed tokens are not re-examined.)
   * **Substring** (literal, not whole word): pattern `[\w[\.']]*<Pattern.quote(lower(term))>[\w[\.']]*` (case-insensitive) – the hit is **expanded to the whole surrounding "word"** (word chars, `.`, `'`), then leading/trailing `BOUNDARY_CHARACTERS` runs are stripped. E.g. term `ell` → hit `hello`.
   * **Regex**: pattern as-is (Java semantics, case-insensitive); `find(offset)` loop; empty match stops.
   * Phone/IP fix-up (selector `TSK_PHONE_NUMBER`/`TSK_IP_ADDRESS` and pattern not ending in `.*`): remove one leading char matching `[^0-9(]` (phone) or `[^0-9]` (IP), one trailing `[^0-9]`, and move the search offset back one so the shared boundary char can start the next match.
   * Dedupe: one hit per distinct hit string per chunk.
   * Validators by selector:
     * `TSK_EMAIL`: keep only if length ≥ 8 and the TLD after the last `.` is a valid IANA TLD (commons-validator `DomainValidator.getInstance(allowLocal=true).isValidTld`).
     * `TSK_CARD_NUMBER`: run `(?<ccn>[2-6]([ -]?[0-9]){11,18})` on the hit with region end shrinking from `len` to 12; each region whose match passes `CreditCardValidator.isValidCCN` adds the hit.
   * Snippet: `text[max(0,start-20)..start] + "«" + hit + "«" + text[min(L-1,end)..min(L-1,end+20)]` (`L` = text length; Autopsy bug: the last char of a chunk never appears in a snippet). Delimiter U+00AB on **both** sides.
3. Hits accumulate in a static map `job → source obj → original keyword → hit term → [UniqueKeywordHit]` (equality: snippet, obj, chunk, hit (case-insensitive), list, literal, whole-word, original term, selector).

`makeArtifacts` (job end): for each source object × original keyword × distinct hit term → take the **first** hit → one `TSK_KEYWORD_HIT` analysis result (score `LIKELY_NOTABLE`, configuration = list name), posted per keyword batch with module name "KeywordSearch". Content for artifact hits is resolved with `SELECT obj_id FROM blackboard_artifacts WHERE artifact_id = <id>` and gets `TSK_ASSOCIATED_ARTIFACT`.

### 1.8 Search semantics & `TSK_KEYWORD_HIT` attributes

Keyword types (`KeywordSearch.QueryType` ordinal stored in `TSK_KEYWORD_SEARCH_TYPE`): `LITERAL=0` (exact), `SUBSTRING=1`, `REGEX=2`. Hit is in an artifact iff Solr object id < 0.

| | Exact match | Substring | Regex |
|---|---|---|---|
| Autopsy query class | `LuceneQuery` | `RegexQuery` (escaped literal) | `RegexQuery` |
| Solr query (ad-hoc) | `text:"<escaped>"` (schema ≥ 2.2: `text:"q" OR content_ja:"q"`) | `content_str:/.*<escaped lower>.*/` | `content_str:/.*<lower regex>.*/` (no `.*` prefix if regex starts with `.*` or `^`; no suffix if it ends with `.*` or unescaped `$`) |
| Hit extraction | Solr highlighter snippet; hit = keyword | Java regex `[\w[\.']]*<quoted>[\w[\.']]*` over each `content_str` value | Java regex over each `content_str` value |
| Overlap handling | drop doc if first `indexOf(lower kw)` in `content_str` ≥ `chunk_size` | skip matches starting ≥ `chunk_size` | same |
| Attributes | `TSK_KEYWORD_PREVIEW` (if snippet), `TSK_KEYWORD` (lower-cased keyword), `TSK_SET_NAME` (if not blank), selector attr (e.g. `TSK_EMAIL` = hit), `TSK_KEYWORD_SEARCH_TYPE` (0 if whole-word else 1), `TSK_ASSOCIATED_ARTIFACT` (artifact hits) | `TSK_KEYWORD` (lower-cased hit), `TSK_KEYWORD_REGEXP` (original term), `TSK_SET_NAME`, `TSK_KEYWORD_PREVIEW`, `TSK_ASSOCIATED_ARTIFACT`, `TSK_KEYWORD_SEARCH_TYPE=1` | same with type `2`; **CCN keywords produce `TSK_ACCOUNT` instead** (1.10) |

Ad-hoc highlighter for exact match: FastVectorHighlighter, 1 snippet, fragsize 50, tags `&laquo;`, `hl.fragCharSize = len(query)` (Solr bug workaround), `hl.maxAnalyzedChars = 500000`; snippet HTML-unescaped and trimmed.

Notes:
* Keyword terms are NFKC-normalised on construction.
* Regex validation (ad hoc): compiles as Java regex with `UNICODE_CHARACTER_CLASS`, and must not contain `\d \D \w \W \s \S \n \t \r \f \a \e \v \V \h \H \p` (Lucene regex would treat `\d` as `d`). Error: "Invalid query statement. If this is a regular expression, only Lucene regular expression patterns are supported. POSIX character classes (such as \n or \w) are not."
  Inline ingest search uses Java regex directly → the same keyword behaves **differently** during ingest (Java semantics) and ad hoc (Lucene pre-filter + Java). postmortem: one engine (`regex` crate; `fancy-regex` if look-around needed) everywhere, no class restriction.
* UI warning when a regex starts with `^` or ends with unescaped `$`: "Boundary characters ^ and $ do not match word boundaries. Consider replacing with an explicit list of boundary characters, such as [ \.,]".
* `escapeLuceneQuery`: trim; prefix `\` before `/ + - & | ! ( ) { } [ ] ^ " ~ * ? : \`, except the outer quotes of a fully quoted string. `quoteQuery` wraps in `"…"` unless already quoted.
* Everything is case-insensitive (content_str lower-cased; query lower-cased when schema ≥ 2.1).
* A hit spanning a chunk boundary is found only if it fits in the overlap window.

### 1.9 Built-in keyword lists (locked, not saved to XML)

`BOUNDARY_CHARACTERS` (`<B>` below) = `[ \t\r\n\.\-\?\,\;\\!\:\[\]\/\(\)\"\'\>\{\}]`

| List name | Selector attribute | Regex (unescaped) | On by default |
|---|---|---|---|
| Phone Numbers | `TSK_PHONE_NUMBER` | PHONE (block below) | no |
| IP Addresses | `TSK_IP_ADDRESS` | IP (block below) | no |
| Email Addresses | `TSK_EMAIL` | `(\{?)[a-zA-Z0-9%+_\-]+(\.[a-zA-Z0-9%+_\-]+)*(\}?)\@([a-zA-Z0-9]([a-zA-Z0-9\-]*[a-zA-Z0-9])?\.)+[a-zA-Z]{2,4}` | **yes** |
| URLs | `TSK_URL` | URL (block below) | no |
| Credit Card Numbers | `TSK_CARD_NUMBER` | `(%?)(B?)([0-9][ \-]*?){12,19}(\^?)` | no |

Unbroken forms (for copy/paste):

```
PHONE: <B>(\([0-9]{3}\)|[0-9]{3})([ \-\.])[0-9]{3}([ \-\.])[0-9]{4}<B>
IP:    <B>(([0-9]|[1-9][0-9]|1[0-9]{2}|2[0-4][0-9]|25[0-5])\.){3}(1[0-9]{2}|2[0-4][0-9]|25[0-5]|[1-9][0-9]|[0-9])<B>
URL:   (((((h|H)(t|T))|(f|F))(t|T)(p|P)(s|S?)\:\/\/)|(w|W){3,3}\.)[a-zA-Z0-9\-\.]+\.([a-zA-Z]{2,5})(\:[0-9]+)*(\/($|[a-zA-Z0-9\.\,\;\?\'\\+&amp;%\$#\=~_\-]+))*
```

Quirks to keep or fix consciously:
* Email TLD limited to 2–4 letters (misses `.museum`, `.photography`); then filtered by TLD validity and length ≥ 8.
* URL: `(s|S?)` means "lower-case `s`, or optional `S`"; the path class literally contains `&`, `a`, `m`, `p`, `;` (left-over `&amp;` XML escaping) and backslash; no IPv6, `file:`, `mailto:`. Hits are lower-cased.
* Phone: North-American format only.
* All built-ins are `isLiteral=false, isWholeWord=true`.

### 1.10 Credit cards: validation, track data, BIN lookup, accounts

`CreditCardValidator.isValidCCN(raw)`:
1. Separators: spaces **or** dashes, not both. Canonical = separators removed; groups = split on the separator.
2. Length check against the IIN (first 8 digits) range map. Guava `TreeRangeMap.put` overwrites overlapping earlier ranges, so insertion order matters (listed in insertion order, half-open `[start,end)`):

```
[34000000,35000000) {15}   [37000000,38000000) {15}                          Amex
[40000000,50000000) {12..19}                                                 Visa
[40260000,40270000) [41750000,41750100) [44050000,44060000) [45080000,45090000)
[48440000,48450000) [49130000,49140000) [49170000,49180000) {16}             Visa Electron
[62000000,63000000) {16,17,18,29}                                            UnionPay
[51000000,56000000) {16}   [22210000,27210000) {16}                          MasterCard
[50609900,50619900) {16,19}   [65000200,65002700) {16,19}                    Verve
[50000000,50100000) [56000000,59000000) [60000000,70000000)
[63900000,63910000) [67000000,68000000) {12..19}                             Maestro
[30000000,30600000) [30950000,30960000) {16,17,18,29}; [36000000,37000000) {14..19};
[38000000,40000000) {16,17,18,29}                                            Diners Intl
[54000000,56000000) {14..19}                                                 Diners US/CA
[60110000,60120000) [62212600,62292600) [64400000,66000000) {16,17,18,29}    Discover
[35280000,35900000) {16,17,18,29}                                            JCB
[50190000,50200000) {16,17,18,29}                                            Dankort
[63600000,63700000) {16,17,18,29}                                            InterPayment
```
   Autopsy bug: `Set16to19 = {16,17,18,29}` — valid 19-digit cards in those ranges are rejected. BIN not in map → any length accepted.
3. Grouping: 15 digits → 1 group or `4-6-5`; 16 → 1 group or `4-4-4-4`; 19 → 1 group, `6-13` or `4-4-4-4-3`; other lengths → 1 group or first group of 4.
4. Luhn (commons-validator `LuhnCheckDigit`).

Account creation (`RegexQuery.createCCNAccount`) for hits of keywords whose selector is `TSK_CARD_NUMBER`:
* Parse the **snippet** with track-1 then track-2 regex (`find()`; the first capture of each attribute wins):
  * Track 1: `(?:%?B)?(?<accountNumber>[2-6]([ -]?[0-9]){11,18})\^(?<name>[^^]{2,26})(?:\^(?:(?:\^|(?<expiration>\d{4}))(?:(?:\^|(?<serviceCode>\d{3}))(?:(?<discretionary>[^?]*)(?:\?(?<LRC>.)?)?)?)?)?)?`
  * Track 2: `[:;<=>?]?(?<accountNumber>[2-6]([ -]?[0-9]){11,18})(?:[:;<=>?](?:(?<expiration>\d{4})(?:(?<serviceCode>\d{3})(?:(?<discretionary>[^:;<=>?]*)(?:[:;<=>?](?<LRC>.)?)?)?)?)?)?`
  * Attributes: `TSK_CARD_NUMBER` (spaces/dashes removed; raw form also as `TSK_KEYWORD`), `TSK_CARD_EXPIRATION`, `TSK_CARD_SERVICE_CODE`, `TSK_CARD_DISCRETIONARY`, `TSK_CARD_LRC`, `TSK_NAME_PERSON` (track 1 only). No card number → nothing created.
  * Note: the `accountNumber` group only matches when the snippet contains a track separator for track 1; track 2's tail is optional, so a bare number matches track 2.
* BIN lookup `CreditCards.getBINInfo(first 8 digits)` adds `TSK_CARD_SCHEME`, `TSK_CARD_TYPE`, `TSK_BRAND_NAME`, `TSK_BANK_NAME`, `TSK_PHONE_NUMBER` (bank), `TSK_URL` (bank), `TSK_COUNTRY`, `TSK_CITY` when present.
  BIN DB = `ranges.csv` downloaded **at build time** (Core `build.xml`) from `https://raw.githubusercontent.com/binlist/data/master/ranges.csv`; columns `iin_start, iin_end, number_length, scheme, brand, type, country, bank_name, bank_url, bank_phone, bank_city`; start right-padded with `0` to 8 digits, end (or start if blank) right-padded with `9`; closed ranges; loaded lazily once.
* Files of type `UNUSED_BLOCKS`/`UNALLOC_BLOCKS` additionally get `TSK_KEYWORD_SEARCH_DOCUMENT_ID = "<objId>_<chunk>"` (viewer page hint).
* Plus `TSK_SET_NAME`, `TSK_KEYWORD_PREVIEW`, `TSK_ASSOCIATED_ARTIFACT`, `TSK_KEYWORD_SEARCH_TYPE=2`.
* Created via `CommunicationsManager.createAccountFileInstance(Account.Type.CREDIT_CARD, ccn, module, content, attrs, jobId)` → `TSK_ACCOUNT` data artifact (`TSK_ACCOUNT_TYPE="CREDIT_CARD"`, `TSK_ID=ccn`) + account rows. **No** `TSK_KEYWORD_HIT` for CCN hits.

postmortem: ship `ranges.csv` as an updatable data file, load into a sorted `Vec<(u32,u32,BinInfo)>` with binary search; Luhn inline.

### 1.11 Keyword list management

Storage `<user config>/keywords.xml` (UTF-8; XSD `KeywordsSchema.xsd`):

```xml
<keyword_lists>
  <keyword_list name="..." created="yyyy-MM-dd HH:mm:ss" modified="yyyy-MM-dd HH:mm:ss"
                use_for_ingest="true" ingest_messages="true">
    <keyword literal="true|false" whole="true|false" selector="TSK_EMAIL">term</keyword>
  </keyword_list>
</keyword_lists>
```
* `use_for_ingest` / `ingest_messages` omitted on **export**, default `true` on import; missing `whole` → `true`; `selector` = attribute type label.
* Locked built-in lists are never written; on reload the locked lists are kept in memory and user lists re-read.
* Import: `.xml` (Autopsy) or EnCase `.txt`. EnCase format: UTF-16; skip 5 header lines; then pairs of lines (structure, metadata), TAB-separated. Structure `[1]` = child count; metadata `[0]` = type (`"5"` folder, `""` expression), `[1]` = name, `[2]` = value, `[3..19]` = 17 flags `pc pu pb p8 p7 pg an ph or di um st ww pr lo ta cp` (non-empty = set). The tree is rebuilt from child counts; each folder becomes a list named `parent/child` (made unique with `(2)`, `(3)`…); expressions become **exact-match** keywords; **GREP expressions (`pg` flag) are skipped**; empty folders produce no list. Name collisions prompt Overwrite / Skip / Cancel.
* Adding keywords (dialog, one per line; type Exact / Substring / Regular Expression): blank lines ignored; duplicates counted and skipped; every term (even literal) must compile as a Java regex or it is re-displayed as invalid (Autopsy bug: literals like `C++` rejected). Result summary "N keywords were successfully added / already in the list / could not be parsed".
* Export: Autopsy XML only. Lists are global (not per case); job settings snapshot only list *names*.

### 1.12 Ad-hoc search UI

Toolbar (top right of the main window; enabled when a case and index are open):
1. **Single term**: text field; radios Exact Match / Substring Match / Regular Expression; "Restrict search to the selected data sources:" (multi-select, enabled when > 1 data source); "Save search results" ("Saving will add Keyword Hit Results to the current case"); "Files Indexed: N" label updated during ingest.
2. **Lists**: tick global lists; same data-source restriction and save option.

Execution (`AdHocSearchPanel.search`):
* Indexed files == 0 → error ("No files are in index yet. If Solr keyword search indexing was enabled, wait for ingest to complete" / "No files were indexed. Re-ingest the image with the Keyword Search Module and Solr indexing enabled.").
* Ingest running → confirm "Keyword Search Ingest is currently running. Not all files have been indexed and this search might yield incomplete results. Do you want to proceed with this search anyway?".
* Empty list / invalid syntax → errors.
* One query per keyword (`getQueryForKeyword`), data-source filter `image_id:<id> image_id:<id> …` (OR).
* Results open in a new result viewer titled `Keyword search <n> - <first 14 chars of concatenated terms>...`; one row per **object** (lowest-chunk hit), columns "Keyword Preview" + standard file columns (or Location for non-files); artifact hits are named "<artifact display name> Artifact".
* Solr paging: cursorMark, 512 rows, sort `id asc`, POST.
* "Save" → background writer creates artifacts (one per object per hit term; missing snippets fetched with a highlight query). Inbox messages (ingest only) capped at 20 per keyword.
* Results tree (Core `KeywordHits`): Keyword Hits → list name (unsaved ad-hoc lists with empty name grouped as "Single Literal Keyword Search" / "Single Regular Expression Search") → keyword (exact/substring) or regex → regex instance (actual hit) → artifacts. Built from SQL:

```sql
SELECT blackboard_attributes.value_text, blackboard_attributes.value_int32,
       blackboard_artifacts.artifact_obj_id, blackboard_attributes.attribute_type_id
FROM blackboard_attributes, blackboard_artifacts
WHERE blackboard_attributes.artifact_id = blackboard_artifacts.artifact_id
  AND blackboard_artifacts.artifact_type_id = <TSK_KEYWORD_HIT>
  AND (attribute_type_id = <TSK_SET_NAME> OR attribute_type_id = <TSK_KEYWORD>
       OR attribute_type_id = <TSK_KEYWORD_SEARCH_TYPE> OR attribute_type_id = <TSK_KEYWORD_REGEXP>)
```

### 1.13 Extracted Text viewer ("Extracted Text", content-viewer position 2, preferred value 4)

Supported for: ad-hoc result rows; `TSK_KEYWORD_HIT` artifacts; `TSK_ACCOUNT` with `TSK_ACCOUNT_TYPE == CREDIT_CARD`; files fully indexed (`num_chunks` on parent == number of `<id>_*` docs) or extractable on the fly; artifacts/reports fully indexed. Fully-indexed answers are cached per object id (cleared on case change).

Sources (combo "Text Source:"); default precedence: highlighted hits > artifact "Result Text" > "File Text":

| Source | Behaviour |
|---|---|
| `HighlightedText` ("Search Results") | Pages = chunk ids with hits (ordered). For a keyword-hit artifact the keyword query is re-run restricted to `id:<obj>_*` (FilterType CHUNK) to find pages. Literal: Solr highlighting of the whole page (`hl.fragsize=0`), FastVectorHighlighter (`hl.fragListBuilder=single`) except `language=ja` docs (original highlighter); tags `<span style='background:yellow'>…</span>`; `hl.maxAnalyzedChars=500000`. Regex or no highlight returned: fetch page text, HTML-escape, highlight manually (case-insensitive `indexOf` of each escaped hit string; overlapping ranges merged). `<a name='HighlightedText_<n>'></a>` anchors inserted before each highlight; "Matches on page: i of N"; next/prev flips pages. Japanese: the mini-chunk's hit count is subtracted to cut highlights in the overlap. |
| `AccountsText` ("Credit Card Number(s)") | Pages from `TSK_KEYWORD_SEARCH_DOCUMENT_ID` or a CCN regex re-query; highlights both `TSK_KEYWORD` (raw) and `TSK_CARD_NUMBER` (canonical) in `content_str` (lower-cased text). |
| `SolrIndexedText` ("File Text" / "Result Text") | Page = stored `text` of `<obj>_<page>` (2nd value for files; value 1 is the file name), HTML-escaped in `<pre>`. No chunks → "This file is a known file (based on MD5 hash) and does not have indexed text." (if SkipKnown) or "No text available for this file." |
| `FileReaderExtractedText` ("File Text") | Files not in the index: run the extractor (+ metadata block) now, page lazily through `Chunker` (page count unknown until the end; no previous page). Not for unalloc/unused/carved, dirs, empty files, archives, octet-stream. |

Controls: page prev/next ("Page: x of y"), match prev/next, zoom (`TextZoomPanel`, 100 % centre step, reset), copy / select all, wait cursor while paging.

postmortem: render pages as text with highlight spans (egui `LayoutJob`, no HTML), virtualised scrolling; compute hit offsets with the same matcher that produced the hit so highlighting is identical for all three types.

### 1.14 Index lifecycle, upgrade, multi-user

Single-user layout: `<case>/ModuleOutput/keywordsearch/data/solr8_schema2.3/index` (Solr data dir = parent). Core name = sanitised case name (`[^-.0-9A-Z_a-z]` → `_`, lower-cased, `_` prefix if it starts with `-`, empty → `text_index`) + `_yyyyMMdd_HHmmss`.
Metadata `<case>/SolrCore.properties` (XML despite the name):

```xml
<SolrCores><Core><CoreName>…</CoreName><SolrVersion>8</SolrVersion>
<SchemaVersion>2.3</SchemaVersion><TextIndexPath>relative/path/index</TextIndexPath></Core></SolrCores>
```

Case open (`openCaseResources`, 7 progress steps): read metadata → none: create new index → else find index with Solr 8 & schema 2.3 → else `identifyIndexToUse` (highest Solr version ≤ 8, then highest schema) → none + a future Solr version present → "The text index for the case is for Solr X. This version of Autopsy is compatible with Solr 8." → older Solr (4) → GUI dialog "Unsupported Keyword Search in Case … Continue / Upgrade Solr Core" (after a 2 s sleep); Upgrade creates a **new empty** current-version index (old text not migrated – user must re-run KWS with indexing); headless → exception. Schema major mismatch → error. Write metadata, open core (`CoreAdminRequest.Create`, configset `AutopsyConfig`, `loadOnStartup=false`, `transient=true`; stale `core.properties` deleted first), test query, register for TSK events.
Case close: stop ad-hoc writers, sleep 2 s, unload core (single-user). Delete case: unload core (single-user) / `CollectionAdminRequest.Delete` (multi-user), then delete the index dir. Delete data source: `deleteByQuery image_id:<dsId>` + commit.

Local Solr process: started at application start on a background thread (`bin/autopsy-solr start -p 23232`; env `SOLR_JAVA_HOME`, `SOLR_HOME=<userdir>/solr`, `STOP_KEY=jjk#09s`, `SOLR_JAVA_MEM=-Xmx<heap>m`); configsets re-copied to `<userdir>/solr` on every start; if the port is busy and the owner is a Solr process (java process args match `-DSTOP.KEY=jjk#09s%start.jar`) it is killed, otherwise error naming the `IndexingServerPort` property; up to 12×5 s waiting for status; logs `<userdir>/var/log/solr.log.{stdout,stderr}`; stop with `stop -k <key> -p <port>`, then kill.

Multi-user:
* SolrCloud (Solr 8). Host/port from prefs or the per-case override `<case dir>/solrserver.txt` (first line `host,port`). Auto-ingest load balancing: `<root output dir>/solrServerList.txt` (lines `host,port`; lines without a comma ignored); one is picked at random at case creation and written to `solrserver.txt`.
* Collection per case; created with `numShards = IndexingMaxShards` or the number of live nodes (`CLUSTERSTATUS`), 1 NRT replica, 0 TLOG/PULL, config `AutopsyConfig`; 5 attempts 1 s apart. Queries through `HttpSolrClient(http://host:port/solr/<collection>)`, indexing through `CloudSolrClient` (all live node URLs).
* `tryConnect` = `CLUSTERSTATUS`; error mapping: "server refused connection" + host reachable in 1 s → "Invalid port number", else hostname/port message; "ioexception occurred when talking to server" → hostname message.
* Backup/restore collection helpers exist but are unused. Legacy Solr 4 cases are searchable through separate Solr 4 host/port prefs; indexing not supported.

postmortem multi-user design: Tantivy is a library, so a shared index needs a `postmortem-indexd` service (one per case or per cluster) exposing RPC (index batch, search, fetch page, delete-by-data-source, term dump, counts) with a single writer and many snapshot readers, index files on shared case storage. Keep per-case server pinning (`solrserver.txt` equivalent) and a server list for load balancing; keep "delete by data source".

### 1.15 Other KWS features

* **Extract Unique Words** general report (`ExtractAllTermsReport`): writes `Unique Words.txt` in the report dir — every term of field `text` in index order via the `/terms` handler (1000 per request, `terms.lower` exclusive paging, `mincount 0`), lower-cased, **not filterable by data source**. Guards: no indexed files → error; ingest running → confirm.
* `KeywordSearchService.index(Content)` (Core API used by other modules to make content searchable): artifacts with positive ids are ignored. Autopsy bug: it passes `keywordListNames = null` and `Ingester.search` only indexes inside `if (keywordListNames != null)`, so this path currently indexes **nothing**. `indexArtifact` is deprecated.
* Health-monitor timing metrics "Solr: Index chunk", "Solr: Connectivity check".
* Tika log `<userdir>/var/log/tika.log` (3 rotating files).

---

## 2. Recent Activity (RA) — `RecentActivity/src/org/sleuthkit/autopsy/recentactivity`

### 2.0 Framework

**Module**: `RecentActivityExtracterModuleFactory` → data-source ingest module "Recent Activity" ("Extracts recent user activity, such as Web browsing, recently used documents and installed programs."). No global or per-job settings.

**Extractor order** (`RAImageIngestModule.startUp`; strictly sequential, later steps read artifacts posted by earlier ones):

| # | Extractor (display name) | Dependency |
|---|---|---|
| 1 | `ExtractRecycleBin` ("Recycle Bin Analyzer") | |
| 2 | `ExtractJumpLists` ("Windows Jumplist Analyzer") | |
| 3 | `RecentDocumentsByLnk` | |
| 4 | `ExtractRegistry` ("Windows Registry Analyzer") | before 6 |
| 5 | `ExtractOs` ("OS Info Analyzer") | before 6 |
| 6 | `DataSourceUsageAnalyzer` | after 4, 5 |
| 7 | `Chromium` (+ `ChromeCacheExtractor` at the end) | |
| 8 | `Firefox` ("FireFox Analyzer") | |
| 9 | `ExtractIE` ("Internet Explorer Analyzer") | |
| 10 | `ExtractEdge` ("Microsoft Edge Analyzer") | |
| 11 | `ExtractSafari` ("Safari Analyzer") | |
| 12 | `SearchEngineURLQueryAnalyzer` | after browsers |
| 13 | `ExtractWebAccountType` | after browsers |
| 14 | `ExtractZoneIdentifier` | after browsers (needs downloads) |
| 15 | `ExtractSru` | |
| 16 | `ExtractPrefetch` | |
| 17 | `ExtractThumbcache` | |
| 18 | `DomainCategoryRunner` | last |

* Inbox: one line per browser extractor (Chrome, Firefox, IE, Edge, Safari) saying whether data was found (`dataFound`). Progress bar determinate, one step per extractor; each extractor in try/catch; errors collected into one HTML `<ul>` inbox message (ERROR level if any). Cancellation checked between extractors.
* Folders: temp `<case temp>/RecentActivity/<module>_<jobId>/`, output `<ModuleOutput>/RecentActivity/<module>_<jobId>/`.

**`Extract` base class helpers**
* `createArtifactWithAttributes`: data-artifact types via `newDataArtifact`; analysis-result types via `newAnalysisResult(type, SCORE_UNKNOWN, null, null, null, attrs)`.
* `createAssociatedArtifact(content, art)`: `TSK_ASSOCIATED_OBJECT` on the *target* file with `TSK_ASSOCIATED_ARTIFACT = art id` (links downloaded file / executed program / recent document to the RA artifact).
* `postArtifacts`: post to blackboard (module "Recent Activity", job id) unless cancelled → triggers data-artifact ingest modules (incl. KWS indexing).
* `querySQLiteDb(path, sql)`: runs on the temp copy; returns list of column→value maps; NULL → `""`; **SQL errors return an empty list** (schema mismatch fails silently).
* Attribute builders (source "Recent Activity"):
  * History: `TSK_URL` always; `TSK_DATETIME_ACCESSED` if not null; `TSK_REFERRER`, `TSK_TITLE`, `TSK_PROG_NAME`, `TSK_USER_NAME` if not blank; `TSK_DOMAIN` whenever the URL is not blank (even if domain is empty).
  * Cookie: `TSK_URL`; `TSK_DATETIME_CREATED`/`_ACCESSED`/`_END` only if non-null and non-zero; `TSK_NAME`, `TSK_VALUE`, `TSK_PROG_NAME`, `TSK_DOMAIN` always (null → "").
  * Bookmark: `TSK_URL`, `TSK_TITLE`, `TSK_DATETIME_CREATED` (if not null), `TSK_PROG_NAME`, `TSK_DOMAIN`.
  * Download: `TSK_PATH`, `TSK_PATH_ID` (if not null and ≠ −1), `TSK_URL`, `TSK_DATETIME_ACCESSED`, `TSK_DOMAIN`, `TSK_PROG_NAME`.
* `createTemporaryFile`: `<RA temp>/<displayName>_<job>/<name><id><ext>`.
* **No dedupe across runs**: re-running ingest duplicates every RA artifact except `TSK_OS_INFO` (checked per file/data source), `TSK_DATA_SOURCE_USAGE` (by description), domain categories (per host, within a run), LNK recent docs (per path+name, within a run).

**`Util`**: `filetimeToMillis(ft) = ft/10_000 − 11644473600000`; `findID(ds, path)`: `\`→`/`, drop `X:` prefix, split parent/name, `findFilesExactNameExactPath`, first obj id or −1; `checkColumn(col, table, db)` via `PRAGMA table_info(<table>)` (schema-version detection); `extractDomain(url)` (host via URL parsing / regex, see 2.17).

**Conventions**
* All timestamp attributes = seconds since Unix epoch. Chrome/WebKit µs since 1601: `v/1e6 − 11644473600`; Mozilla µs: `v/1e6`; Apple (since 2001): `v + 978307200`.
* File lookup `FileManager.findFiles(ds, namePattern, parentSubString)` = `name LIKE pattern` (case-insensitive, `%` wildcard) AND `parent_path LIKE '%parentSubString%'`.
* Chromium per-file skip: size 0, or name contains `-slack`, `cache`, `media`, `index` (some functions omit `media`), or unallocated (for JSON/SQLite stores).
* Rust stack: `rusqlite` (bundled; open read-only/`immutable=1`), `serde_json`, `plist`.

### 2.1 Chromium family (`Chromium.java`)

| Browser | Parent-path substring |
|---|---|
| Microsoft Edge | `Microsoft/Edge/User Data` |
| Yandex | `YandexBrowser/User Data` |
| Opera | `Opera Software/Opera Stable` |
| SalamWeb | `SalamWeb/User Data` |
| UC Browser | `UCBrowser/User Data%` (file names also get a `%` suffix) |
| Brave | `BraveSoftware/Brave-Browser/User Data` |
| Google Chrome | `Chrome/User Data` |

**Profiles (`Local State` JSON)**: no `profile` key → the browser location itself is the profile (user "Default"); else for each key of `profile.info_cache` read `gaia_id`, `hosted_domain`, `shortcut_name`, `name`, `user_name` → custom artifact **`TSK_LOCAL_STATE`** ("Chromium Profiles") on the Local State file: `TSK_PATH` (profile dir), `TSK_USER_ID` (gaia_id), `TSK_DOMAIN` (hosted_domain), `TSK_SHORTCUT`, `TSK_NAME`, `TSK_USER_NAME`, `TSK_PROG_NAME` (browser).
Bugs: the per-profile "user" is always `name` (`userName.contains("")` is always true); `<location>/Default`, `/Guest Profile`, `/System Profile` are always added.
Per profile, in order: extensions, history, bookmarks, cookies, logins, autofill, downloads, favicons; then the cache extractor (2.2). DBs copied to `<temp>/<browser>_<job>/<name><j>.db`, deleted afterwards.

**Extensions (`Secure Preferences`)**: `extensions.settings` (Opera: `extensions.opsettings`); per extension `state` (1 = "Enabled" else "Disabled"), `active_permissions.api[]` joined ", ", `manifest.name/version/description` → custom **`TSK_CHROME_EXTENSIONS`** ("Chromium Extensions"): `TSK_ID`, `TSK_COMMENT` ("Malicious Extension Found - <reference URL>" only if id is in the blocklist), `TSK_NAME`, `TSK_DESCRIPTION`, `TSK_VERSION`, `TSK_FLAG`, `TSK_PERMISSIONS`, `TSK_USER_NAME`, `TSK_PROG_NAME`.
Blocklist `malicious_chrome_extensions.csv` (336 lines, DRL-1.1, from randomaccess3/detections): copied to user config (overwrite); `#` lines skipped; `id,reference_url` (header row loaded harmlessly).

**History (`History`)**
```sql
SELECT urls.url, urls.title, urls.visit_count, urls.typed_count, last_visit_time, urls.hidden, visits.visit_time,
 (SELECT urls.url FROM urls WHERE urls.id=visits.url) AS from_visit, visits.transition
FROM urls, visits WHERE urls.id = visits.url
```
→ one `TSK_WEB_HISTORY` per visit row: URL, title, accessed = converted `last_visit_time`, referrer = `from_visit`, browser, domain, profile user.
Bugs: every visit gets the URL's *last* visit time (not `visits.visit_time`); `from_visit` subquery returns the visit's own URL, not the referrer (should join `visits.from_visit → visits.id → urls`).

**Bookmarks (`Bookmarks` JSON)**: for each key of `roots`, `children[]` one level only (no folder recursion) → `TSK_WEB_BOOKMARK`: `TSK_URL`=`url`, `TSK_TITLE`=`name`, `TSK_DATETIME_CREATED`=`date_added` (WebKit µs), `TSK_PROG_NAME`, `TSK_DOMAIN`, `TSK_USER_NAME`, `TSK_COMMENT` = root key name.

**Cookies (`Cookies`)**
```sql
SELECT name, value, host_key, expires_utc,last_access_utc, creation_utc FROM cookies
```
→ `TSK_WEB_COOKIE`: `TSK_URL`=host_key, `TSK_DATETIME_ACCESSED`=last_access_utc, `TSK_NAME`, `TSK_VALUE`, `TSK_PROG_NAME`, `TSK_DOMAIN` = host_key with leading dots removed (`replaceFirst("^\\.+(?!$)","")`), `TSK_USER_NAME`. `encrypted_value` ignored (DPAPI/v10/v20 cookies lost).

**Downloads (`History`)**: if `downloads` has a `url` column (pre-v30):
```sql
SELECT full_path, url, start_time, received_bytes FROM downloads
```
else (v30+, one row per URL-chain entry):
```sql
SELECT current_path AS full_path, url, start_time, received_bytes FROM downloads, downloads_url_chains WHERE downloads.id=downloads_url_chains.id
```
→ `TSK_WEB_DOWNLOAD` (`TSK_PATH`, `TSK_PATH_ID` via `findID`, `TSK_URL`, `TSK_DATETIME_ACCESSED`=start_time, `TSK_DOMAIN`, `TSK_USER_NAME`, `TSK_PROG_NAME`) + `TSK_ASSOCIATED_OBJECT` on the first file matching the normalised `full_path`.

**Logins (`Login Data`)**
```sql
SELECT origin_url, username_value, date_created, signon_realm from logins
```
→ `TSK_SERVICE_ACCOUNT`: `TSK_URL`=origin_url, `TSK_DATETIME_CREATED`, `TSK_URL_DECODED` = domain of origin_url (quirk), `TSK_USER_NAME` = username_value (with `'` doubled), `TSK_REALM`=signon_realm, `TSK_DOMAIN` = domain of signon_realm, `TSK_PROG_NAME`, second `TSK_USER_NAME` = profile user. Passwords not extracted.

**Autofill (`Web Data`)** (only size-0 and `-slack` checks). 8x schema ⇔ `autofill.date_created` exists.
```sql
-- pre-8x
SELECT name, value, count, date_created FROM autofill, autofill_dates WHERE autofill.pair_id = autofill_dates.pair_id
-- 8x
SELECT name, value, count, date_created, date_last_used from autofill
```
→ `TSK_WEB_FORM_AUTOFILL`: `TSK_NAME`, `TSK_VALUE`, `TSK_COUNT`, `TSK_DATETIME_CREATED` (already Unix seconds), `TSK_DATETIME_ACCESSED` (8x), `TSK_USER_NAME`, `TSK_PROG_NAME`, `TSK_COMMENT` "The data was encrypted." when the value is a BLOB.
Addresses pre-8x:
```sql
SELECT first_name, middle_name, last_name, address_line_1, address_line_2, city, state, zipcode, country_code, number, email, date_modified FROM autofill_profiles, autofill_profile_names, autofill_profile_emails, autofill_profile_phones WHERE autofill_profiles.guid = autofill_profile_names.guid AND autofill_profiles.guid = autofill_profile_emails.guid AND autofill_profiles.guid = autofill_profile_phones.guid
```
8x: same joins selecting `first_name, middle_name, last_name, full_name, street_address, city, state, zipcode, country_code, number, email, date_modified, use_date, use_count`. Name = `full_name` or "first middle last"; street = `street_address` or `address_line_1 + " " + address_line_2`; location = "street, city, state, zip, country" → `WebBrowserArtifactsHelper.addWebFormAddress(name, email, phone, location, 0, use_date, use_count, extra)` (extra: `TSK_DATETIME_MODIFIED` if > 0, encrypted comment) → `TSK_WEB_FORM_ADDRESS` + EMAIL/PHONE account instances. Any BLOB field → analysis result `TSK_ENCRYPTION_DETECTED` (score NOTABLE) on Web Data, comment "<browser> Autofill Database Encryption Detected".

**Favicons (`Favicons`)**
```sql
SELECT page_url, last_updated, last_requested FROM icon_mapping, favicon_bitmaps WHERE icon_mapping.icon_id = favicon_bitmaps.icon_id
```
→ custom **`TSK_FAVICON`** ("Favicon"): `TSK_URL`, `TSK_DATETIME_MODIFIED`=last_updated, `TSK_DATETIME_ACCESSED`=last_requested, `TSK_DOMAIN`, `TSK_USER_NAME`, `TSK_PROG_NAME`.

### 2.2 Chrome disk cache (`ChromeCacheExtractor`, blockfile format)

* Cache folders: every `findFiles(ds, "index", "default/cache")` with size > 0; folder must contain `data_0`..`data_3` else skipped. External `f_*` files whose parent path equals the folder exactly (allocated preferred). Duplicate name+path → prefer path ending `default/cache`, then allocated, then lowest obj id. Files copied to temp, memory-mapped, little-endian.
* Index header (368 bytes): magic u32 `0xC103CAC3`, skip 2, version u16, num_entries i32, num_bytes i32, last_file i32, this_id i32 (skip), stats addr u32 (skip), table_len i32; hash table at offset 368: `table_len` × u32 cache addresses.
* Cache address u32: bit 31 initialised; bits 28–30 file type (EXTERNAL, RANKINGS, BLOCK_256, BLOCK_1K, BLOCK_4K, BLOCK_FILES, BLOCK_ENTRIES, BLOCK_EVICTED). External: file number `addr & 0x0FFFFFFF` → `f_%06x`. Block file: selector `(addr & 0x00FF0000) >> 16` → `data_%d`, start block `addr & 0xFFFF`, block count `(addr & 0x03000000) >> 24` (Java has a precedence bug here). Block sizes: RANKINGS 36, BLOCK_256 256, BLOCK_1K 1024, BLOCK_4K 4096, BLOCK_FILES 8, BLOCK_ENTRIES 104, BLOCK_EVICTED 48. Data offset = `8192 + start_block × block_size`.
* Entry (`EntryStore`): hash u32, next u32 (**collision chain not followed** → only the first entry per bucket), rankings u32, reuse i32, refetch i32, state i32 (0 normal, 1 evicted, 2 doomed, other → throws), creation_time i64 (WebKit µs), key_len i32, long_key u32, data_size 4×i32, data_addr 4×u32, flags u32, 5×i32 padding, key inline NUL-terminated ≤ 160 chars unless `long_key`.
* Stream 0 = HTTP headers: from first "HTTP", split on NUL, double NUL ends; line 1 = response line; others `key:value` (key lower-cased); headers string = "key : value" pairs joined by " \n". Stream 1 = payload (only one exported). Entries with < 2 streams skipped.
* Output: payload in an `f_` file → that file is the cached item; payload inside `data_N` → written to `<RA output>/<cachePath>/data_N__%08x` and added as a derived file under that `data_N` (all 4 times = entry creation time, version 1.0.0), content event + added to the job. `content-encoding: br` → MIME `application/x-brotli`.
  `TSK_WEB_CACHE` on the entry's data file: `TSK_URL`=key, `TSK_DOMAIN`, `TSK_DATETIME_CREATED`, `TSK_HEADERS`, `TSK_PATH` (cached item unique path), `TSK_PATH_ID`; `TSK_ASSOCIATED_OBJECT` on the cached item.
* Gaps: Chrome "Simple" cache (`Cache_Data/*_0`, `index-dir/the-real-index`) and `Code Cache` not handled → postmortem should parse both formats natively.

### 2.3 Firefox (`Firefox.java`)

Inputs (parent substring "Firefox", size 0 skipped, allocation ignored): `places.sqlite` (history, bookmarks, downloads ≥ v24), `cookies.sqlite`, `downloads.sqlite` (< v24), `formhistory.sqlite`, `autofill-profiles.json`. Order: history, bookmarks, downloads, cookies, form history, autofill.

```sql
-- History
SELECT moz_historyvisits.id, url, title, visit_count,(visit_date/1000000) AS visit_date,from_visit,
 (SELECT url FROM moz_historyvisits history, moz_places places where history.id = moz_historyvisits.from_visit and history.place_id = places.id ) as ref
FROM moz_places, moz_historyvisits WHERE moz_places.id = moz_historyvisits.place_id AND hidden = 0
-- Bookmarks
SELECT fk, moz_bookmarks.title, url, (moz_bookmarks.dateAdded/1000000) AS dateAdded FROM moz_bookmarks INNER JOIN moz_places ON moz_bookmarks.fk=moz_places.id
-- Cookies (if creationTime column exists)
SELECT name,value,host,expiry,(lastAccessed/1000000) AS lastAccessed,(creationTime/1000000) AS creationTime FROM moz_cookies
-- Cookies (older)
SELECT name,value,host,expiry,(lastAccessed/1000000) AS lastAccessed FROM moz_cookies
-- Downloads pre-v24 (both download queries always run)
SELECT target, source,(startTime/1000000) AS startTime, maxBytes FROM moz_downloads
-- Downloads v24+
SELECT url, content AS target, (lastModified/1000000) AS lastModified FROM moz_places, moz_annos, moz_anno_attributes WHERE moz_places.id = moz_annos.place_id AND moz_annos.anno_attribute_id = moz_anno_attributes.id AND moz_anno_attributes.name='downloads/destinationFileURI'
-- Form history (v64 detected by timesUsed column)
SELECT fieldname, value FROM moz_formhistory
SELECT fieldname, value, timesUsed, firstUsed, lastUsed FROM moz_formhistory
```
* History → `TSK_WEB_HISTORY` (URL, title, visit time, referrer = `ref`, program "FireFox Analyzer", domain; no user). `place:` URLs get null domain.
* Bookmarks → `TSK_WEB_BOOKMARK` (`TSK_URL`, `TSK_TITLE`, `TSK_DATETIME_CREATED` if > 0, `TSK_PROG_NAME`, `TSK_DOMAIN` if not empty).
* Cookies → `TSK_WEB_COOKIE` (`TSK_URL`=host, `TSK_DATETIME_ACCESSED`, `TSK_NAME`, `TSK_VALUE`, `TSK_PROG_NAME`, `TSK_DATETIME_CREATED` when present, `TSK_DOMAIN` = host without leading dots). `expiry` ignored.
* Downloads: path = `URLDecoder.decode(target.replaceAll("file:///", ""), "UTF-8")` → `TSK_WEB_DOWNLOAD` (`TSK_URL`, `TSK_DATETIME_ACCESSED`, `TSK_PATH`, `TSK_PATH_ID`, `TSK_PROG_NAME`, `TSK_DOMAIN`) + `TSK_ASSOCIATED_OBJECT`.
* Form history: fields `it` and `ts` skipped → `TSK_WEB_FORM_AUTOFILL` (`TSK_NAME`, `TSK_VALUE`, `TSK_PROG_NAME`; v64: `TSK_DATETIME_CREATED`=firstUsed/1e6, `TSK_DATETIME_ACCESSED`=lastUsed/1e6, `TSK_COUNT`=timesUsed).
* `autofill-profiles.json` `addresses[]`: `name`, `email`, phone = `tel` or `tel-country-code`+`tel-national`, `timeCreated`/`timeLastUsed` /1000, `timesUsed`, address = `address-line1..3`, `postal-code`, `country` joined ", " (empty parts skipped) → `addWebFormAddress` → `TSK_WEB_FORM_ADDRESS`.
* Gaps: no cache2, no `logins.json`/`key4.db`, no session store, no `profiles.ini` (no user attribution).

### 2.4 Safari (`ExtractSafari.java`, `BinaryCookieReader`)

* `History.db` (parent "Safari"):
  ```sql
  SELECT url, title, visit_time + 978307200 as time FROM 'history_items' JOIN history_visits ON history_item = history_items.id;
  ```
  → `TSK_WEB_HISTORY` (URL, title, time (double truncated), program "Safari Analyzer", domain).
* `Bookmarks.plist` (parent "Safari"): recursive walk — dict with `Children` = folder, dict with `URLString` = bookmark; title from `URIDictionary.title` (bug: tests `title` on the wrong dict first) → `TSK_WEB_BOOKMARK` (no created time).
* `Downloads.plist`: `DownloadHistory[]` → `DownloadEntryURL`, `DownloadEntryPath`, `DownloadEntryDateAddedKey` (bug: stored in **milliseconds**) → `TSK_WEB_DOWNLOAD` + `TSK_ASSOCIATED_OBJECT`.
* `Cookies.binarycookies` (parent "Cookies"): file header big-endian: magic `cook`, page count u32, one u32 size per page. Page: `00 00 01 00`, then LE cookie count u32, u32 offset per cookie, u32 zero. Cookie (LE): size u32, skip 16, offsets of URL, name, path, value (u32 each), skip 8, expiration f64 and creation f64 (+978307200), strings NUL-terminated at their offsets (Java reads records sequentially instead of seeking — works only because they are contiguous) → `TSK_WEB_COOKIE` (URL, name, value, program, domain, `TSK_DATETIME_CREATED`=creation, `TSK_DATETIME_END`=expiration).

### 2.5 Internet Explorer (`ExtractIE.java`)

* Bookmarks: `findFiles(ds, "%.url", "Favorites")`, size > 0; URL = text after `=` on the first line starting `URL` → `TSK_WEB_BOOKMARK` (URL, title = file name, created = file crtime, program "Internet Explorer Analyzer", domain (null for `res://`)).
* Cookies: `findFiles(ds, "%.txt", "Cookies")`; split on newlines: line 0 name, 1 value, 2 URL — **only the first cookie per file** → `TSK_WEB_COOKIE` (`TSK_URL`, `TSK_DATETIME_CREATED` = file crtime, `TSK_NAME`, `TSK_VALUE`, `TSK_PROG_NAME`, `TSK_DOMAIN`).
* History: every `index.dat`, copied to `<temp>/IE_<job>/index<id>.dat`, parsed by **pasco2** (Java):
  `java --add-exports=java.xml/com.sun.org.apache.xalan.internal.xsltc.dom=ALL-UNNAMED -cp pasco2.jar:pasco2/* isi.pasco2.Main -T history <file>` → `results/pasco2Result.<id>.txt` (+ `.err`). Lines starting `URL`, TAB-split, ≥ 4 fields: field 1 `Visited: user@url`, field 3 time `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` (bug: parsed as local time). Split field 1 at the first `@`: if the left part looks like a valid domain the whole field is the URL, else left = user (strip `Visited:`, `:Host:`, `:...:`) and right = URL → `TSK_WEB_HISTORY` (URL, time, program, domain, user).
* postmortem: native index.dat parser (header + hash tables, URL/LEAK/REDR records in 0x80-byte blocks, FILETIME last-access/modified, user from container path).

### 2.6 Edge Legacy / IE10+ WebCache (`ExtractEdge.java`)

* Inputs: `findFiles(ds, "WebCacheV01.dat", "WebCache")` (history, cookies), `findFiles(ds, "Spartan.edb", "MicrosoftEdge")` (favorites). **Windows only** (elsewhere: returns).
* Tool: NirSoft `ESEDatabaseView.exe /table <db> * /scomma <outdir>\*.csv` (dumps every table). `Containers.csv` (`name`, `containerid`) maps container names → `Container_<id>.csv`.
* History: containers named "History", rows containing `Visited:`; naive `,` split; `url` column `Visited: user@url`; `accessedtime` parsed by format guessing (`MM/dd/yyyy hh:mm:ss a`, then locale day/month order and values > 12, slash vs dot, AM/PM vs 24 h; last good format cached) → `TSK_WEB_HISTORY` (URL, time, program "Microsoft Edge Analyzer", domain, user).
* Cookies: every CSV whose name contains "cookie": `lastmodified` (accessed), `rdomain` (reversed; only 2–3 labels are flipped back), `name`/`value` (space-separated hex bytes, chars > 31 kept) → `TSK_WEB_COOKIE`.
* Downloads: `iedownload` handler exists but is never called (dead code).
* Bookmarks: `Favorites.csv` from Spartan.edb, split with `,(?=(?:[^"]*"[^"]*")*[^"]*$)` → `TSK_WEB_BOOKMARK` (`url`, `title`, quotes removed).
* postmortem: pure-Rust ESE reader; read `Containers` and container tables' `Url`, `AccessedTime`/`ModifiedTime`/`ExpiryTime` (FILETIME), `ResponseHeaders` directly — cross-platform, no locale guessing; also iedownload containers.

### 2.7 Windows Registry (`ExtractRegistry.java`) — RegRipper based

Inputs (case-insensitive, unallocated not filtered): `findFiles(ds, "sam", "/system32/config")`, `ntuser.dat` and `usrclass.dat` anywhere, `system`, `software`, `security` under `/system32/config`.

Tools: two RegRipper copies — `thirdparty/rr` (Autopsy plugins printing pseudo-XML) and `thirdparty/rr-full` (RegRipper 3.x). **If either is missing the whole RA module fails to start.** Windows: `rip.exe`; else `perl -I <home> <home>/rip.pl` (`/usr/bin/perl` or `/usr/local/bin/perl`). Command `<cmd> -r <hive copy> -f <profile>` (cwd = RegRipper dir). Output `<RA output>/reg_<job>/<hive>-regripper-<objId>-autopsy.txt` (+ `-autopsy.err.txt`, `-full.txt`, `-full.err.txt`), `regripper-info.txt` (`<id>\t<uniquePath>`). The full output is added as a case **Report** "RegRipper <path>" (source "RecentActivity"). rejistry is **not** used by RA (only by the Core registry viewer).

Profile chosen by substring of the *local temp path* (fragile; first match wins):

| contains | autopsy profile | full profile |
|---|---|---|
| `system` | `autopsysystem` | `system` |
| `software` | `autopsysoftware` | `software` |
| `ntuser` | `autopsyntuser` | `ntuser` |
| `sam` | — | `sam` |
| `security` | — | `security` |
| `usrclass` | — | `usrclass` |

Autopsy plugin profiles: `autopsysystem` = autopsyusb, autopsynic2, autopsyprocarchitecture, autopsycompname, autopsyprofiler; `autopsysoftware` = autopsywinver, autopsyuninstall, autopsyprofilelist; `autopsyntuser` = autopsylogin, autopsyrecentdocs, arunmru, autopsyshellfolders, officedocs2010, officedocs, autopsyntusernetwork. Full profiles: `sam` = samparse(+_tln); `security` = auditpol, secrets, securityproviders; `usrclass` = appx, clsid, muicache, photos, recyclepersist, scriptleturl, shellbags, shellbags_xp, uacbypass (+_tln); `system`/`software`/`ntuser` ≈ 100 plugins each — only the sections below are parsed, the rest exists only in the text report.

**Autopsy-profile output parsing**: remove dash dividers and CR/LF; replace `'`→`&apos;` then `&`→`&amp;` (double-escapes – bug); NUL → space; wrap in `<document>`; parse XML. Each top-level element = section with optional `<mtime>` (`EEE MMM d HH:mm:ss yyyy`, US locale, local time) and `<artifacts>`.

| Section | Key (hive) | Output |
|---|---|---|
| WinVersion | SOFTWARE `Microsoft\Windows NT\CurrentVersion` | OS info: name = ProductName (+ " " + CSDVersion), SystemRoot, ProductId, RegisteredOwner, RegisteredOrganization, InstallDate (gmtime string + "+0000") |
| Profiler | SYSTEM `ControlSetXXX\Control\Session Manager\Environment` | OS info: `OS`, `PROCESSOR_ARCHITECTURE`, `TEMP` |
| CompName | SYSTEM ComputerName + Tcpip Hostname/Domain | OS info ComputerName, Domain; then every OS account known (SID → login) created/updated with scope LOCAL |
| usb | SYSTEM `<CurrentControlSet>\Enum\USB\<VID_xxxx&PID_yyyy>\<serial>` | per `<device mtime=epoch dev=class>serial</device>` → **`TSK_DEVICE_ATTACHED`**: `TSK_DATETIME` (mtime if > 0), `TSK_DEVICE_MAKE` (vendor from USB DB when `dev` contains "vid", else ""), `TSK_DEVICE_MODEL` (product from USB DB, else `dev`), `TSK_DEVICE_ID` = serial |
| uninstall | SOFTWARE `Microsoft\Windows\CurrentVersion\Uninstall` (+ Wow6432Node) | **`TSK_INSTALLED_PROG`**: `TSK_PROG_NAME` = DisplayName (or key name) + " v." + DisplayVersion, `TSK_DATETIME` = subkey last write |
| office | NTUSER Office MRU | **`TSK_RECENT_OBJECT`**: `TSK_DATETIME_ACCESSED` = section mtime, `TSK_NAME` = `name` attr, `TSK_VALUE` = text, `TSK_PROG_NAME` = element name |
| ProfileList | SOFTWARE `...\ProfileList\<SID>` → ProfileImagePath | OS account: user = last path component minus ".domain"; scope LOCAL if SID machine prefix seen in SAM else DOMAIN (domain from CompName); home dir (`%VAR%` removed, `\`→`/`) as `TSK_HOME_DIR` |
| NtuserNetwork | NTUSER `Network\<letter>` → RemotePath | **`TSK_REMOTE_DRIVE`**: `TSK_LOCAL_PATH` = "Network\X", `TSK_REMOTE_PATH` |
| SSID | SYSTEM `ControlSet00<n>\Services\Tcpip\Parameters\Interfaces\<GUID>\<subkey>` | **`TSK_WIFI_NETWORK`**: `TSK_SSID` = subkey name read as hex with nibbles of each byte reversed, `TSK_DATETIME` = interface last write, `TSK_DEVICE_ID` = adapter GUID |
| recentdocs, ProcessorArchitecture, shellfolders, logon | | ignored |

**`TSK_OS_INFO`** (one on the data source, only if it has none): `TSK_NAME` (computer name), `TSK_DOMAIN`, `TSK_PROG_NAME` (OS name, default "Windows"), `TSK_PROCESSOR_ARCHITECTURE`, `TSK_TEMP_DIR`, `TSK_DATETIME` (install), `TSK_PATH` (SystemRoot), `TSK_PRODUCT_ID`, `TSK_OWNER`, `TSK_ORGANIZATION` – each only when set; first value wins except OS name (last non-empty wins). Bug: install-time setter checks `domain == null`.

**SAM (samparse, full profile)**: after the divider following "User Information": `Username : name [RID]`, then `SID` and `Key : value` lines until blank; `--> flag` sets a flag. "Group Membership Information": `Group Name : X [count]`, `Users :` then `count` SID lines. Existing OS accounts matched by SID get an account instance (LAUNCHED) and update; unknown SIDs → new LOCAL Windows accounts. Dates `EEE MMM dd HH:mm:ss yyyy 'Z'` GMT; "Never" skipped.

| SAM field | Stored as |
|---|---|
| Account Created | account creation time |
| Last Login Date | `TSK_DATETIME_ACCESSED` |
| Login Count | `TSK_COUNT` |
| Account Type contains "admin" | `TSK_IS_ADMIN = 1` |
| User Comment | `TSK_DESCRIPTION` |
| InternetName | `TSK_EMAIL` + EMAIL communications account |
| Full Name (else Name) | full name |
| Pwd Rest Date / Pwd Fail Date | `TSK_DATETIME_PASSWORD_RESET` / `TSK_DATETIME_PASSWORD_FAIL` |
| Password Hint | `TSK_PASSWORD_HINT` |
| Question 1–3 / Answer 1–3 | custom string attrs `RA_SAM_QUESTION_1..3` / `RA_SAM_ANSWER_1..3` (Q3 label wrongly "Question 2") |
| flags "Password does not expire", "Password not required" | `TSK_PASSWORD_SETTINGS` |
| flags "Account auto locked", "Home directory required", "Account Disabled" | `TSK_ACCOUNT_SETTINGS` |
| flags Normal user, Server trust, Workstation trust, Interdomain trust, MNS logon, Temporary duplicate | `TSK_FLAG` |
| groups | `TSK_GROUPS` (bug: only written when the list is empty → effectively never) |
| Username | login name |

**SYSTEM (full profile)**:
* BAM (`bam v.*`; lines `epoch|...|...|...|path (S-1-...)`): strip `\Device\HarddiskVolumeN`; user = SID's login name or SID → **`TSK_PROG_RUN`**: `TSK_PROG_NAME` = path, `TSK_USER_NAME`, `TSK_DATETIME`, `TSK_COMMENT` "Recently Executed according to Background Activity Moderator (BAM)" + associated object on the executable.
* Bluetooth (`bthport v..*`): after each "Device Unique ID" line → `TSK_DEVICE_ID`, optional `Name` → `TSK_NAME`, next line `TSK_DATETIME` (last seen), next `TSK_DATETIME_ACCESSED` (last connected); dates `EEE MMM d HH:mm:ss yyyy` (" z" removed) → **`TSK_BLUETOOTH_PAIRING`**.

**NTUSER / USRCLASS (full profile)**:
* Shellbags → custom **`RA_SHELL_BAG`** ("Shell Bags"): `TSK_PATH`, custom `RA_SHELL_BAG_KEY` (string), custom `RA_SHELL_BAG_LAST_WRITE` (datetime), `TSK_DATETIME_MODIFIED`/`_CREATED`/`_ACCESSED` when non-zero. `ShellBagParser`:
  * `shellbags_xp v.*`: data regex `^(\d+?.*?\s*? | \s*?)\|.*?\|.*?\|.*?\|.*?\|.*?` (≥ 6 fields); resource = field 5; key `Software\Microsoft\Windows\ShellNoRoam\BagMRU`; fields 0–3 = last write, modified, accessed, created.
  * `shellbags v.*`: ≥ 7 fields; path = field 6 minus `[...]`; key = `Local Settings\Software\Microsoft\Windows\Shell\BagMRU` + the line's trailing `[...]` minus "[Desktop" and the last char.
  * `itempos.*`: a line containing `Software\` sets the bag path, `LastWrite:` sets last write; data lines `^\d*?\s*?\|.*?\|.*?\|.*?\|.*?` → resource field 4, fields 1–3 modified/accessed/created.
  * Section ends at a dash-only line. Dates `yyyy-MM-dd HH:mm:ss` or `EEE MMM dd HH:mm:ss yyyyy`, local time.
* MRU lists → **`TSK_RECENT_OBJECT`** (`TSK_PATH`, `TSK_COMMENT`, associated object if the file exists):

| Section | Parsing | Comment |
|---|---|---|
| `adobe v.*` | header `Key name,file name,sDate,uFileSize,uPageCount`; quote-aware CSV; file = col 1 minus last char & quotes; `/c/x` → `c:/x`; sDate `yyyyMMddHHmmssZ` → `TSK_DATETIME_ACCESSED` | "Recently opened according to Adobe MRU" |
| `mpmru v.*` | after "LastWrite", `x> path` lines until divider or "RecentFileList has no values." | "…Media Player MRU" |
| `ArcHistory:` | lines until "PathHistory:" or blank | "Recently opened by 7Zip" |
| `applets v.*` | `x> path` lines | "Recently opened according to Applets registry key" |
| `mmc v.*` | `x> path` lines | "…Windows Management Console MRU" |
| `winrar v.*` | after "LastWrite", `x> path` lines | "…WinRAR MRU" |
| `msoffice v.*` | `epoch\|…\|desc`; desc split on " - " (file = part 2 if > 2 parts else part 1); "MSOffice LastLoginTime" skipped; epoch → `TSK_DATETIME_ACCESSED` | " MRU " in line → "Recently opened according to Office MRU"; else "Stored in TrustRecords because Office security exception was granted" |

* WSL (`lxss v.*`; desc " - distro - basePath - kernelCmd") → custom **`WSL_NAME`** ("Windows Subsystem For Linux"): `TSK_NAME` = distro, `TSK_PATH` = basePath + `\ext4.vhdx`, `TSK_DATETIME_ACCESSED`, custom `WSL_KERNAL_COMMAND`, `TSK_COMMENT` "Windows Subsystem For Linux".
* Robustness: most loops read lines without null checks → truncated output throws (caught only at module level).

**postmortem registry design**: parse hives natively with `notatin` (transaction logs, deleted cells; preferred) or `nt-hive`; read keys directly: SAM `Domains\Account\Users\<RID>` (F and V records, `Names`, `Aliases\Members`), BAM `Services\bam\State\UserSettings\<SID>` (FILETIME at value offset 0), `Services\BTHPORT\Parameters\Devices\<mac>`, `Enum\USB` + `Enum\USBSTOR`, Uninstall, ProfileList, ComputerName, Tcpip, CurrentVersion (InstallDate u32), Environment, NTUSER `Network`, Tcpip Interfaces, BagMRU/Bags (needs shell-item parser), Adobe `AVGeneral\cRecentFiles`, `MediaPlayer\Player\RecentFileList`, `7-Zip\Compression\ArcHistory` & `FM\PathHistory`, `Applets\*\Recent File List`, MMC `Recent File List`, `WinRAR\ArcHistory`, Office `File MRU`, `User MRU\*\File MRU`, `Trusted Documents\TrustRecords`, `Lxss\{GUID}` (DistributionName, BasePath, KernelCommandLine). Optionally emit a RegRipper-style text report.

### 2.8 Recycle Bin (`ExtractRecycleBin.java`)

* Inputs: `$I*` files under `$RECYCLE.BIN`; all `$R*` files in the data source grouped by name. Custom artifact **`TSK_RECYCLE_BIN`** ("Recycle Bin").
* `$I` layout (LE): version i64 (1 = Vista/7, 2 = Win10), original size i64, deleted time FILETIME; name v1 = UTF-16LE from offset 24 to EOF; v2 = u32 char count at 24, name from 28 (count×2 bytes). Trimmed; v1 NUL padding not stripped (bug).
* User SID = `$I` parent path from one char before the first `-`, `/` removed; user name = lookup in an OS-account map (bug: map keyed by account name, not SID → user often empty); no SID in path → skipped.
* For each `$R` with matching name (`$I`→`$R`), same parent path and same meta flags: artifact on the `$R` file: `TSK_PATH` (original Windows path), `TSK_DATETIME_DELETED`, `TSK_USER_NAME`.
* **Re-create the file at its original location**: missing folders added as virtual directories recursively (matched by `fs_obj_id`, `parent_path`, `name`); file added with `addFileSystemFile` (same meta address/attributes as `$R`, name flag UNALLOC, meta flags UNALLOC|USED, mtime = deleted time, other times copied); recycled directories recursed. Deleted files thus appear in their original tree.

### 2.9 Jump Lists (`ExtractJumpLists.java`)

* Inputs: `findFiles(ds, "%", "%/AppData/Roaming/Microsoft/Windows/Recent/AutomaticDestinations/")`; skip `-slack`, `.`, `..`, size 0. `CustomDestinations` not handled.
* Copy to `<temp>/jumplists_<job>/<name>_<id>`; open as OLE compound file (Apache POI); each top-level stream with size > 0 parsed as LNK; name = best name + ".lnk"; raw stream written to `<RA output>/jumplists_<job>/<jumplist>_<id>/<stream>-<best>.lnk`; added as **derived file** under the jump list (all times 0, version 1.0.0). `DestList` fails LNK parsing → skipped (MRU times, hostname, entry ids ignored). Derived files added to the job. No artifacts directly.
* Rust: `cfb` + `lnk`; add DestList parsing and CustomDestinations.

### 2.10 LNK recent documents (`RecentDocumentsByLnk.java`)

`findFiles(ds, "%.lnk", "Recent")`, size > 0; Autopsy `JLnkParser`; path = best path (local, network, or from ID list); dedupe within run by path + lnk name → **`TSK_RECENT_OBJECT`** on the .lnk: `TSK_PATH`, `TSK_PATH_ID` (may be −1), `TSK_DATETIME_ACCESSED` = the .lnk file's own crtime; associated object on the target (found by name and parent path ending with the path). Rust: `lnk` crate; also record the target MAC times stored in the LNK header (Autopsy ignores them).

### 2.11 Prefetch (`ExtractPrefetch.java`)

* Inputs: `findFiles(ds, "%.pf")` with parent path containing `/windows/prefetch` (case-insensitive), size > 0; copied to `<temp>/<dsId>-Autopsy_PF_DB.db3_<job>/<base>_<objId>.pf`.
* Tool: `thirdparty/markmckinnon/mm_artifact_parser_{x64_win.exe|x64_linux|x64_macos|aarch64_linux|aarch64_macos}` `-a prefetch -f <dir> -db <ModuleOutput>/prefetch/<dsId>-Autopsy_PF_DB.db3` (stdout `Output.txt`, stderr `Error.txt`).
  ```sql
  SELECT prefetch_File_Name, actual_File_Name, file_path, Number_time_file_run, Embeded_date_Time_Unix_1, Embeded_date_Time_Unix_2, Embeded_date_Time_Unix_3, Embeded_date_Time_Unix_4, Embeded_date_Time_Unix_5, Embeded_date_Time_Unix_6, Embeded_date_Time_Unix_7, Embeded_date_Time_Unix_8 FROM prefetch_file_info;
  ```
* Obj id recovered from the file name with `_(?<objId>\d*)\s*$`. One **`TSK_PROG_RUN`** per distinct non-zero run time (≤ 8) on the .pf: `TSK_PROG_NAME` (exe), `TSK_PATH` (file_path), `TSK_DATETIME`, `TSK_COUNT` (run count), `TSK_COMMENT` "Prefetch File"; associated object on the exe (`findFiles(ds, exe.lower())` with parent ending `filePath + "/"`).
* Rust: versions 17/23/26/30/31; Win10 `MAM\x04` LZXPRESS-Huffman compression (`frnsc-prefetch` or own decoder); 8 last-run times + run count; also volume info and referenced files (unused by Autopsy).

### 2.12 SRUM (`ExtractSru.java`)

* Inputs: `findFiles(ds, "SOFTWARE")` with parent ending `/config/` and `findFiles(ds, "SRUDB.DAT")` (last found wins for each). Tool: `mm_artifact_parser -a sru -sr <srudb> -sh <software> -db <ModuleOutput>/sru/<id>_srudb.db3`.
* Program path map:
  ```sql
  SELECT DISTINCT SUBSTR(LTRIM(IdBlob, '\Device\HarddiskVolume'), INSTR(LTRIM(IdBlob, '\Device\HarddiskVolume'), '\'))  application_name, idBlob source_name FROM SruDbIdMapTable WHERE idType = 0 AND idBlob NOT LIKE '!!%'
  ```
  name cut at " [", looked up with `findFiles(name, path)` (parent ending with path) → map lower(source_name) → file (bug: static map shared across jobs).
* Network usage:
  ```sql
  SELECT STRFTIME('%s', timestamp) ExecutionTime, b.application_name, b.Application_Name formatted_application_name, username User_Name, bytesSent, BytesRecvd FROM network_Usage a, SruDbIdMapTable s, exe_to_app b, userNames u WHERE s.idType = 0 and s.idIndex = appId and idblob = b.source_name and u.idindex = userid order by ExecutionTime;
  ```
  → `TSK_PROG_RUN` on SRUDB.DAT: `TSK_PROG_NAME`, `TSK_USER_NAME`, `TSK_DATETIME`, `TSK_BYTES_SENT`, `TSK_BYTES_RECEIVED` (bug: read as int → overflow > 2 GB), `TSK_COMMENT` "System Resource Usage - Network Usage".
* Application usage: same shape against `Application_Resource_Usage` → `TSK_PROG_RUN` (`TSK_PROG_NAME`, `TSK_USER_NAME`, `TSK_DATETIME`, `TSK_COMMENT` "System Resource Usage - Application Usage"); associated objects via the path map.
* Rust: ESE reader; tables `{D10CA2FE-6FCF-4F6D-848E-B2E99266FA89}` (application usage), `{973F5D5C-1D90-4944-BE8E-24B94231A174}` (network usage), `SruDbIdMapTable` (IdType 0 = app path, 3 = SID → resolve via ProfileList/SAM).

### 2.13 Thumbcache (`ExtractThumbcache.java`)

Windows only. `findFiles(ds, "thumbcache_%.db", "")` → `thumbcache_parser/thumbcache_viewer_cmd.exe -O <outDir> <file>`; every produced file (except Output.txt/Error.txt) added as derived file under the db (times from the extracted file), added to the job + content event. No artifacts. Rust: native CMMM parser (header, entries: hash, extension, data size, checksums); also `iconcache_*.db`.

### 2.14 Zone.Identifier (`ExtractZoneIdentifier.java`)

* `findFiles(ds, "%:Zone.Identifier")` (TSK exposes NTFS ADS as `name:stream`). Known downloads = set of `TSK_PATH_ID` of all `TSK_WEB_DOWNLOAD` artifacts (whole case).
* Parse as Java Properties: `ZoneId`, `ReferrerUrl`, `HostUrl`, `LastWriterPackageFamilyName` (referrer and package name read but unused).
* Downloaded file candidates: obj id − 1, obj id − 2, then name lookup in the parent folder; must have same meta address, name = stream name minus `:Zone.Identifier`, same parent path.
* If not already a known download → **`TSK_WEB_DOWNLOAD`** on the ADS file: `TSK_PATH` (parent+name), `TSK_PATH_ID`, `TSK_URL` = HostUrl, `TSK_DOMAIN`, `TSK_COMMENT` = zone name (0 "Local Machine Zone", 1 "Local Intranet Zone", 2 "Trusted Sites Zone", 3 "Internet Zone", 4 "Restricted Sites Zone"); `TSK_ASSOCIATED_OBJECT` on the downloaded file if it has none.

### 2.15 Search engine queries (`SearchEngineURLQueryAnalyzer`, `SEUQAMappings.xml`)

* At start-up `SEUQAMappings.xml` is copied to the user config dir (overwrite) and validated against `SearchEngineSchema.xsd` (invalid → only logged). `<SES><SearchEngine engine domainSubstring><splitToken plainToken regexToken/>…`. Tokens are attached by engine **name**, so the two "Wiki" engines share tokens.
* Engine match: `^(.*[./])?<domainSubstring>([./].*)?$` against the **full URL** (substring not escaped; `.` matches any char) → "wiki" matches any URL containing it.

| Engine | domainSubstring | splitTokens (plain → regex where different) |
|---|---|---|
| Google | google | `?q=`, `&q=` |
| Yahoo | yahoo | `?p=`, `?text=` |
| Twitter | twitter | `?q=`, `&q=` |
| LinkedIn | linkedin | `&keywords=`, `?keywords=` |
| Facebook | facebook | `?value=`, `?q=` |
| Bing | bing | `search?q=` |
| Baidu | baidu | `?wd=`, `?kw=`, `baidu.com/q?`→`word=`, `baidu.com/m?`→`word=`, `baidu.com/i?`→`word=`, `/qw=`→`\qw=`, `?qw=`→`\qw=`, `bs=`→`&bs=`, `?ie=` |
| Sogou | sogou.com | `query=`, `?ie=` |
| Soso | soso.com | `p=S`, `?w=`, `&w`, `?ie=`, `&query=` |
| Youdao | youdao | `search?q=`→`\?q=`, `?i=`, `#keyfrom=`, `?spc=` |
| Yandex | yandex | `?text=`, `&text=`→`&text` |
| Biglobe | biglobe | `?search=`, `?q=`, `/key/`, `&q=` |
| Linkestan | linkestan.com | `?psearch=` |
| Parseek | parseek.com | `?q=` |
| Parset | parset.com | `?Keyword=` |
| Amazon | amazon.com | `?k=` |
| DuckDuckGo | duckduckgo.com | `?q=` |
| Youtube | youtube.com | `?search_query=` |
| Reddit | reddit.com | `?q=` |
| CCSearch | ccsearch.creativecommons | `?q=` |
| Swisscows | swisscows.com | `?query=` |
| GIbiru | gibiru.com | `?q=` |
| Wiki | wiki | `?search=` |
| Cutestat | cutestat.com | `?q=` |
| Givewater | search.givewater | `?q=` |
| Ekoru | ekoru | `?q=` |
| Ecosia | ecosia | `?q=` |
| Slideshare | slideshare | `&q=` |
| Wikipedia | wikipedia.org | `?search=` |
| Wiki | wiki.com | `?cx=` |

(XML regexTokens escape `?` as `\\?`, e.g. Google `?q=` → `\\?q=`.)

* Query extraction: per matching engine, the first token whose plainToken occurs in the URL; regexToken with `\?`→`?`… split URL on it (regex), take the last piece up to the first `&`; escape stray `%` (`%(?![0-9a-fA-F]{2})` → `%25`); URL-decode UTF-8 (`+` → space); non-empty results collected in a set per artifact.
* Inputs: all `TSK_WEB_BOOKMARK` and `TSK_WEB_HISTORY` of the data source whose source is a file. Output: one **`TSK_WEB_SEARCH_QUERY`** per distinct query on the same file: `TSK_DOMAIN` (source artifact's domain), `TSK_TEXT` (query), `TSK_PROG_NAME`, `TSK_DATETIME_ACCESSED` (source time or 0).
* Rust: `url` crate, match on host suffix, read query parameters properly; keep a user-editable mapping file compatible with this XML.

### 2.16 Web account roles (`ExtractWebAccountType.java`)

* Inputs: `TSK_WEB_HISTORY` and `TSK_SERVICE_ACCOUNT` with both `TSK_URL` and `TSK_DOMAIN` (lower-cased).
* Rules (URL substring → platform/role): myBB `/admin/index.php` ADMIN, `/modcp.php` MOD, `/usercp.php` USER; phpBB `/adm/index.php` ADMIN, `/mcp.php` MOD, `/ucp.php` USER; Joomla `/administrator/index.php` ADMIN; WordPress URL containing `/wp-admin/` → USER if it ends with `/wp-admin/` or contains `/wp-admin/index.php` or `/wp-admin/profile.php`, else ADMIN; service account without rule → platform = domain, role USER.
* One result per (domain, platform), highest rank wins (USER 0 < MOD 1 < ADMIN 2) → **`TSK_WEB_ACCOUNT_TYPE`** on the winning artifact's file: `TSK_DOMAIN`, `TSK_TEXT` = "<User role|Moderator role|Administrator role> (<platform>)", `TSK_URL`.

### 2.17 Domain categorisation (`DomainCategoryRunner`, `DefaultPriorityDomainCategorizer`, `DefaultDomainCategorizer`)

* Inputs: bookmark, cache, cookie, download, history, search-query artifacts of the data source, sorted by accessed time desc, then URL (case-insensitive), then id (comparator loads attributes on every comparison – slow).
* Host: Java URL parsing; fallback regex
  `^\s*(((?<scheme>[^:\/?#]+):?)?\/\/)?(((?<userinfo>[^\/?#@]*)@)?(?<host>[^\/\.?#:]*\.[^\/?#:]*)?(:(?<port>[0-9]{1,5}))?\/?)?(?<path>([^?#]*)(\?([^#]*))?(#(.*))?)?`.
  Missing domain derived from host, missing host = domain; both lower-cased.
* Categorizers, first non-empty answer wins:
  1. User custom categorizer `org.sleuthkit.autopsy.url.analytics.domaincategorization.CustomWebCategorizer` (Core; edited in Options).
  2. `DefaultPriorityDomainCategorizer` → "Search Engine": exact host (leading `www.` removed) in ~190 Google country domains (`google.com` … `google.cat`), Yahoo regional hosts (`yahoo.com`, `uk.yahoo.com`, `yahoo.co.jp`, …), and `bing.com, baidu.com, sogou.com, soso.com, duckduckgo.com, swisscows.com, gibiru.com, cutestat.com, youdao.com, biglobe.ne.jp, givewater.com, ekoru.org, ecosia.org, yandex.ru, yandex.com`.
  3. Other installed `DomainCategorizer` services, sorted by class name.
  4. `DefaultDomainCategorizer`: `default_domain_categories.csv` (~101 k lines `suffix,category`, `#` comments; "Disposable Email" 100,854, "Web Email" 153, "Messaging" 10); lookup tries host suffixes longest → shortest.
* Dedupe: skip artifacts whose host was already seen; create only the first time each matched suffix is seen → **`TSK_WEB_CATEGORIZATION`** on the artifact's file: `TSK_DOMAIN`, `TSK_HOST`, `TSK_NAME` = category.

### 2.18 OS detection (`ExtractOs.java`)

For each OS type, exact name/path lookup of marker files; on the first found file (if it has no `TSK_OS_INFO`) create `TSK_OS_INFO` with `TSK_PROG_NAME` = label. Windows has an empty label here (registry covers it). File contents are not read (no version).

| OS | Marker path(s) | `TSK_OS_INFO` label | Data-source-usage label |
|---|---|---|---|
| Windows | `/windows/system32` | — | OS Drive (Windows) |
| macOS | `/System/Library/CoreServices/SystemVersion.plist` | Mac OS X | OS Drive (OS X) |
| Android | `/data/com.android.providers.settings/databases/settings.db` | Android | OS Drive (Android) |
| Red Hat | `/etc/redhat-release`, `/etc/redhat_version` | Linux (Redhat) | OS Drive (Linux Redhat) |
| SUSE | `/etc/SUSE-release` | Linux (Novell SUSE) | OS Drive (Linux Novell SUSE) |
| Fedora | `/etc/fedora-release` | Linux (Fedora) | OS Drive (Linux Fedora) |
| Slackware | `/etc/slackware-release`, `/etc/slackware-version` | Linux (Slackware) | OS Drive (Linux Slackware) |
| Debian | `/etc/debian_release`, `/etc/debian_version` | Linux (Debian) | OS Drive (Linux Debian) |
| Mandrake | `/etc/mandrake-release` | Linux (Mandrake) | OS Drive (Linux Mandrake) |
| Yellow Dog | `/etc/yellowdog-release` | Linux (Yellow Dog) | OS Drive (Linux Yellow Dog) |
| Sun JDS | `/etc/sun-release` | Linux (Sun JDS) | OS Drive (Linux Sun JDS) |
| Solaris/Sparc | `/etc/release` | Linux (Solaris/Sparc) | OS Drive (Linux Solaris/Sparc) |
| Gentoo | `/etc/gentoo-release` | Linux (Gentoo) | OS Drive (Linux Gentoo) |
| United Linux | `/etc/UnitedLinux-release` | Linux (United Linux) | OS Drive (Linux United Linux) |
| Ubuntu | `/etc/lsb-release` | Linux (Ubuntu) | OS Drive (Linux Ubuntu) |

### 2.19 Data source usage (`DataSourceUsageAnalyzer.java`)

All results: **`TSK_DATA_SOURCE_USAGE`** on the data source with `TSK_DESCRIPTION`, deduped by description.
1. For each `TSK_OS_INFO` of this data source with non-empty program name: contains "windows" → "OS Drive (<name>)"; else mapped OS type → its usage label; else "OS Drive (<name>)". If no Windows OS info but `/windows/system32` exists → "OS Drive (Windows)".
2. Disk image ≤ 100 GiB with exactly one FAT16/FAT32/exFAT file system → "Android Media Card" if any root entry (case-insensitive) is `.android_secure`, `android`, `audio`, `photos`, `dcim`, `music`, `pictures`, `videos`; else "Flash Drive".
3. Any `FLY___.DAT` → "DJI Internal SD Card".

### 2.20 USB ID database (`UsbDeviceIdMapper`, `USB_DATA.txt`)

* Copy of linux-usb.org `usb.ids` (version 2022.05.20, 25,244 lines, ~3,400 vendors), copied to the user config dir (no overwrite).
* Load: skip comments/blank lines; stop at the first line starting `C 00` (class section). Vendor line `vvvv  Name` → key `VVVV0000` (value has trailing space); TAB-indented `pppp  Product` → key `VVVVPPPP`; interface lines (two TABs) wrongly read as products.
* `parseAndLookup(dev)`: split `dev` (e.g. `VID_0781&PID_5567`) on `_` and `&`; VID = part 1, PID = part 3 (or "0000" if missing/shorter than 4), upper-cased; exact `VID+PID` → vendor+product; else `VID+"0000"` → vendor + product "Product: <pid>"; else nothing.
* Rust: compile-time `phf` map from `usb.ids`; parse interface lines correctly.

### 2.21 External tools and pure-Rust replacements

| Autopsy tool | Used by | Platforms | Rust replacement |
|---|---|---|---|
| RegRipper (`rr`, `rr-full`, Perl / `rip.exe`) | Registry | all (Perl off Windows) | `notatin` (preferred) / `nt-hive` + native key logic (2.7) |
| pasco2 (Java) | IE history | all (spawns a JVM) | native index.dat parser |
| NirSoft `ESEDatabaseView.exe` | Edge | Windows only | pure-Rust ESE reader (`ese_parser`; `libesedb` FFI stop-gap) |
| `mm_artifact_parser` (markmckinnon) | Prefetch, SRUM | Win/Linux/macOS x64+arm64 | Prefetch: `frnsc-prefetch` or own + LZXPRESS Huffman; SRUM: ESE + SOFTWARE hive |
| `thumbcache_viewer_cmd.exe` | Thumbcache | Windows only | own parser |
| Apache POI | Jump lists | — | `cfb` |
| JLnkParser | LNK, jump lists | — | `lnk` |
| dd-plist | Safari | — | `plist` |
| sqlite-jdbc | browsers, Prefetch, SRUM | — | `rusqlite` (bundled) |
| Gson | Chromium, Firefox | — | `serde_json` |

### 2.22 Everything RA produces

Data artifacts: `TSK_WEB_HISTORY` (Chromium, Firefox, IE, Edge, Safari), `TSK_WEB_BOOKMARK` (all 5), `TSK_WEB_COOKIE` (all 5), `TSK_WEB_DOWNLOAD` (Chromium, Firefox, Safari, Zone.Identifier), `TSK_WEB_CACHE` (Chrome cache), `TSK_WEB_FORM_AUTOFILL` & `TSK_WEB_FORM_ADDRESS` (Chromium, Firefox), `TSK_SERVICE_ACCOUNT` (Chromium logins), `TSK_WEB_SEARCH_QUERY`, `TSK_WEB_ACCOUNT_TYPE`, `TSK_WEB_CATEGORIZATION`, `TSK_RECENT_OBJECT` (LNK, registry MRUs, Office), `TSK_PROG_RUN` (BAM, Prefetch, SRUM), `TSK_INSTALLED_PROG`, `TSK_DEVICE_ATTACHED`, `TSK_BLUETOOTH_PAIRING`, `TSK_WIFI_NETWORK`, `TSK_REMOTE_DRIVE`, `TSK_OS_INFO`, `TSK_DATA_SOURCE_USAGE`, `TSK_ASSOCIATED_OBJECT`.
Analysis result: `TSK_ENCRYPTION_DETECTED` (NOTABLE).
OS accounts + instances (SAM, ProfileList, CompName) with attributes `TSK_HOME_DIR`, `TSK_DATETIME_ACCESSED`, `TSK_COUNT`, `TSK_IS_ADMIN`, `TSK_DESCRIPTION`, `TSK_EMAIL`, `TSK_DATETIME_PASSWORD_RESET`, `TSK_DATETIME_PASSWORD_FAIL`, `TSK_PASSWORD_HINT`, `TSK_PASSWORD_SETTINGS`, `TSK_ACCOUNT_SETTINGS`, `TSK_FLAG`, `TSK_GROUPS`, `RA_SAM_QUESTION_1..3`, `RA_SAM_ANSWER_1..3`; EMAIL communications account from SAM InternetName.
Custom artifact types: `TSK_LOCAL_STATE` "Chromium Profiles", `TSK_CHROME_EXTENSIONS` "Chromium Extensions", `TSK_FAVICON` "Favicon", `TSK_RECYCLE_BIN` "Recycle Bin", `RA_SHELL_BAG` "Shell Bags", `WSL_NAME` "Windows Subsystem For Linux". Custom attributes: `RA_SHELL_BAG_KEY` (string), `RA_SHELL_BAG_LAST_WRITE` (datetime), `WSL_KERNAL_COMMAND` (string), `RA_SAM_QUESTION_1..3`, `RA_SAM_ANSWER_1..3` (string).
Derived / rebuilt files: Chrome cache payloads (`data_N__xxxxxxxx`), `.lnk` streams from jump lists, thumbcache images, recycle-bin files re-added at their original location (+ virtual directories). Reports: RegRipper full text per hive.

### 2.23 Bugs / edge cases to fix (not copy)

* Chromium: wrong visit times & referrers; bookmark folders not recursed; profile user always `name`; Default/Guest/System profiles always added; logins put domain in `TSK_URL_DECODED` and have two `TSK_USER_NAME`; encrypted cookies ignored; no Simple Cache; collision chains not followed; block-count precedence bug.
* Safari: download times in ms; bookmark title lookup wrong. IE: pasco times local; first cookie per file only. Edge: Windows-only; locale-guessed dates; naive CSV split breaks on commas in URLs; downloads dead code.
* Registry: profile chosen by temp-path substring; XML escape order wrong; loops crash on truncated output; install-time setter checks wrong field; groups never written; missing RegRipper disables the whole module.
* Recycle Bin: user map keyed by name not SID; v1 NUL padding; `$R` lookup across the whole disk by name (slow).
* SRUM: static path map shared across jobs; byte counters overflow > 2 GB.
* Search queries: engine regex matched against the full URL with unescaped substring.
* Domain categorisation: sort comparator loads attributes per comparison.
* General: sequential extractors; every DB copied to temp first; **browser `-wal` files are not copied** → recent records lost (copy `-wal`/`-journal` alongside, or replay the WAL, then open `immutable=1`).
* Not covered by Autopsy at all (candidates beyond parity): **Windows Timeline (`ActivitiesCache.db`) is not implemented** despite the task list; Firefox logins/sessions; Chromium Sessions, Top Sites, Shortcuts, Network Action Predictor; macOS/Linux user activity beyond OS detection; Shimcache, Amcache, UserAssist, TypedPaths, RunMRU, WordWheelQuery (only in the RegRipper text report); event logs.

---

## 3. Email Parser (`thunderbirdparser`)

### 3.0 Overview

* Display name "Email Parser" (module name is also the attribute *source* string). File ingest module only; **no settings** (no global or per-job panel).
  Description: "This module detects and parses mbox and pst/ost files and populates email artifacts in the blackboard."
* Java deps: apache-mime4j core/dom + mbox-iterator 0.8.11, java-libpst 0.9.5-SNAPSHOT, ez-vcard 0.12.1 (vinnie 2.0.2), ICU4J `CharsetDetector`.
* Classes: `ThunderbirdMboxFileIngestModule` (dispatch, artifacts, accounts, attachments), `MboxParser`, `EMLParser`, `MimeJ4MessageParser` (shared MIME logic), `PstParser`, `VcardParser`, `EmailMessage` (+ `Attachment`, `AttachedEmailMessage`), `EmailMessageThreader`.

### 3.1 Per-file gating and format detection (`process`)

1. Skip (OK) if `known == KNOWN`, type `UNALLOC_BLOCKS` or `SLACK`, or `!isFile()`.
2. Read the first 64 bytes – **only if size > 64** (files ≤ 64 bytes can never be MBOX/EML).
3. First match wins, in this order:
   * **MBOX**: the 64 bytes (default charset) start with `"From "` **and** MIME == `application/mbox` (case-insensitive; if the stored MIME is empty, `FileTypeDetector` runs now).
   * **EML**: extension (TSK lower-cased) == `eml` **and** the 64 bytes contain `':'`.
   * **PST/OST**: first 4 bytes big-endian == `0x2142444E` (`"!BDN"`) – ANSI/Unicode PST and OST; no extension check.
   * **vCard**: size > 22 and first 11 bytes equal `BEGIN:VCARD` (case-insensitive). A BOM, UTF-16 or leading whitespace defeats detection.
4. If matched, create `CommunicationArtifactsHelper(case, "Email Parser", file, Account.Type.EMAIL, jobId)` (used only for `addAttachments`).

Paths: temp `<case>/Temp/EmailParser/<fileName>-<objId>` (mbox split chunks get `-<splitOffset>`), deleted in `finally`. Output `<case>/ModuleOutput/Email Parser/` (DB stores relative path `<ModuleOutput rel>/Email Parser/<name>`). Disk guard: if free space is known and `size >= freeSpace` → ingest error "Out of disk space. Cannot copy {0} to parse.", return OK.

### 3.2 MBOX

**Folder path → `TSK_PATH`**: if `parentPath` contains `/Mail/` → `substring(idx+5)` (keeps the leading `/`); else if it contains `/ImapMail/` → `substring(idx+9)`; else `""`. Then `+= fileName`, then `.replaceAll(".sbd", "")` – a **regex**, so any char followed by `sbd` is removed (Autopsy bug). Example: `.../ImapMail/imap.gmail.com/INBOX.sbd/Work` → `/imap.gmail.com/INBOX/Work`.

**Size / splitting**: files < 1,048,576,000 bytes (`MBOX_SIZE_TO_SPLIT` = 1000 MiB) are copied whole to temp and parsed. Larger files: `findMboxSplitOffset` skips 1000 MiB, discards one 7-byte read, then reads **non-overlapping** 7-byte windows until one equals `\r\nFrom ` (13 10 70 114 111 109 32), records `curPos-5` (the `F`), skips another 1000 MiB, … and parses chunks `[0,o1)`, `[o1,o2)`, …
Autopsy bugs (do not replicate): separator found only if 7-byte aligned; LF-only mbox (Thunderbird on Unix) never splits; **the tail after the last offset is never parsed** (and if no offset is found, nothing is parsed); threading is per chunk.

**`processMboxFile`**: iterate `MboxParser.getEmailIterator(emailFolder, file, objId)` (whole-message mode) collecting non-null messages; post parse errors as "Error while processing {0}" (HTML `<li>` list); then call `processEmails(list, getEmailIterator(...))`, which **parses the file a second time** (each attachment is written twice under the same name). `getThreadInfoIterator` (partial mode) exists but is unused.

**Charset**: candidates ISO-8859-1, US-ASCII, UTF-16, UTF-16BE, UTF-16LE, UTF-8, then each ICU `detectAll()` match; the first for which `MboxIterator.fromFile(f).fromLine("^From .*\r?\n").charset(c).build()` does not throw wins → in practice always ISO-8859-1 (byte-transparent). IOException → "Failed to read mbox file from disk."

**Splitting into messages**: mime4j `MboxIterator` mmaps the file and splits on `^From .*\r?\n`; no `>From ` unescaping (mboxrd), ignores `Content-Length`. Each chunk → `DefaultMessageBuilder.parseMessage`; runtime/IO errors → message `null` (skipped).

### 3.3 EML

`EMLParser.parse` → mime4j → `extractEmail(msg, "", objId)`. Path is `""`, so **EML emails have no `TSK_PATH`**. One email per file, no threading. Flow: `createEmailArtifact` → `ModuleContentEvent` per derived file → `addFilesToJob`. IO/MimeException → `ERROR`.

### 3.4 Shared MIME extraction (`MimeJ4MessageParser`)

Builder: `MimeConfig` with `maxLineLen=-1`, `maxHeaderLen=-1`, `maxHeaderCount=-1` (unlimited).

`extractEmail` fields:
* sender / To / Cc / Bcc: `Mailbox.toString()` per mailbox joined with `"; "` (a trailing `"; "` remains; groups flattened).
* Subject: RFC 2047 decoded. `sentDate = getDate()/1000` (0 if missing/unparseable).
* `messageID`: raw value incl. `<>`; missing → `""`. `In-Reply-To`: raw field body.
* References: `split(">")`, each piece `trim() + ">"` (a trailing fragment yields a bogus `">"` entry); In-Reply-To appended if not present – when In-Reply-To is absent this appends **`null`**.

Body dispatch: multipart → `handleMultipart`; single `TextBody` → `handleTextBody(type = message MIME type, fields = message headers)`; any other single body → attachment index 1.

`handleMultipart` (recursive; `index` is 0-based within *that* multipart): nested multipart → recurse; `Content-Disposition: attachment` → attachment; `text/html` while no HTML body yet, or `text/plain` while no text body yet → `handleTextBody`; anything else → attachment (inline images, second text parts, `text/calendar`, inline `message/rfc822`). First plain and first HTML part anywhere in the tree win.

`handleTextBody`: reads `TextBody.getReader()` (charset + transfer decoding), lines re-joined with `\n`. On **every call** it sets `headers = "\n-----HEADERS-----\n" + ("\n" + name + ": " + body)* + "\n\n---END HEADERS--\n\n"` using the **part's** fields → for multipart mail `TSK_HEADERS` holds the last text part's MIME headers, not the RFC 822 headers (Autopsy bug). `rtfBody` is never set for MIME mail.

`handleAttachment`:
* File name: `getFilename()` or `"attachment" + hashCode()`; `FileUtil.escapeFileName`; if > 64 chars → `UUID.randomUUID()` (extension dropped).
* Unique name `<fileID>-<index>-<sentDate>-<filename>` (collisions overwrite silently), written to `ModuleOutput/Email Parser/` through `EncodedFileOutputStream(XOR1)`.
* `message/rfc822` body (`Message`): write the full serialized message and create `AttachedEmailMessage(extractEmail(inner, parentLocalPath, fileID))` (recursive parse). Otherwise `writeBody` (decoded bytes).
* Attachment metadata: `name`, `localPath` (relative output path + unique name), `size` = bytes written, encoding XOR1, all timestamps 0.

### 3.5 PST / OST (`PstParser`, java-libpst)

The whole file is copied to temp (`RandomAccessFile` required).

`open()` result:

| java-libpst outcome | Result |
|---|---|
| `PSTException` "Only unencrypted and compressable PST files are supported at this time" (high/cyclic encryption) | ENCRYPT |
| message starting "unable to" | ERROR (empty/corrupt) |
| other `PSTException` / IOException | ERROR |
| `IllegalArgumentException` | ENCRYPT |

PST passwords are ignored (password-protected PSTs parse normally). Autopsy bug: `open()` never assigns `fileID` → it is **0** in attachment names.

* ENCRYPT → `TSK_ENCRYPTION_DETECTED` analysis result (score `NOTABLE`, conclusion "File-level Encryption", attr `TSK_NAME` = "File-level Encryption"), posted; return OK.
* ERROR → ingest error "Error while processing {0}" / "Only files from Outlook 2003 and later are supported."; return ERROR.
* OK → `getPartialEmailMessages()` (full traversal, partial mode) then `getEmailMessageIterator()` (second full traversal) feeding `processEmails`.

Folder traversal: root path `"\\"`; child path = `path + "\\" + displayName` (e.g. `\\Top of Personal Folders\Inbox`); lazy depth-first (Guava `Iterables.concat`). For every folder with `getContentCount() > 0`, loop `getNextChild()` cast to `PSTMessage` → **contacts, appointments, tasks are emitted as emails**. A `PSTException`/IOException increments `failureCount` and ends that folder.

Full-mode fields:
* To/Cc/Bcc from `getDisplayTo/CC/BCC` (mostly display names). If a field contains `getReceivedByName()`, that text is replaced with `getReceivedBySMTPAddress()` (if the name is empty, `replace("", x)` inserts x between every char – guard!).
* Sender: name `getSenderName()`, address `getSentRepresentingSMTPAddress()` else `getSenderEmailAddress()` (may be an X.500 DN → no account); formatted `"name <addr>"` or whichever is non-empty.
* `sentDate = getMessageDeliveryTime()`; text `getBody()`, HTML `getBodyHTML()`, RTF `getRTFBody()` (LZFu-decompressed, errors swallowed).
* Headers `"\n-----HEADERS-----\n\n" + transportHeaders + "\n\n---END HEADERS--\n\n"` only if transport headers are non-empty.
* `localPath` = folder path; `id` = `getDescriptorNodeId()` (NID); `messageID = getInternetMessageId()`; `inReplyTo = getInReplyToId()`.
* References: scan transport headers line by line; `^References:.*` starts a buffer, following lines appended trimmed until a line matches `^\w+:.*$`, then split on `>`. **If References is the last header the result is null** (bug). In-Reply-To appended if absent.
* Attachments: skip when `getAttachSize() >= freeSpace`; name `getLongFilename()` else `getFilename()`; unique name `0-<nid>-<contentId>-<escapedName>` (no length cap); streamed in 8176-byte blocks through XOR1 (first read −1 → throws); `crtime = getCreationTime()`, `mtime = getModificationTime()` (seconds), size `getFilesize()`. **Embedded MAPI messages are not recursed.** Errors go to a list that is never shown.
* Partial mode sets only subject, id, messageID, inReplyTo, references.

### 3.6 `processEmails`, `TSK_EMAIL_MSG`, accounts, attachments

1. Per container: `AccountFileInstanceCache` (address → `AccountFileInstance`).
2. `EmailMessageThreader.threadMessages(partial)` (exceptions caught).
3. Walk the full iterator (skip nulls), matching each message **positionally** with `partial[msgCnt++]`; copy thread id only if messageID **and** subject are equal (any skew silently drops thread ids).
4. `createEmailArtifact` per message.
5. At the end: `ModuleContentEvent` for every derived file, then `addFilesToJob(all)` → attachments enter the pipeline only after the whole container is done.

Address extraction regex: `\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,4}\b` (case-insensitive), `find()` loop into a HashSet (TLDs > 4 chars break it).
* Sender account only if **exactly one** address is found in From: `createAccountFileInstance(Account.Type.EMAIL, addr, "Email Parser", containerFile, null, jobId)` via the cache.
* Recipients: every address in To, Cc, Bcc → EMAIL account instance.

`TSK_EMAIL_MSG` data artifact on the **container file** (attributes in this order):

| Attribute | Value | When |
|---|---|---|
| `TSK_HEADERS` | headers block | non-empty |
| `TSK_EMAIL_FROM` | sender string | non-empty |
| `TSK_EMAIL_TO` | to string | non-empty |
| `TSK_SUBJECT` | subject | non-empty |
| `TSK_DATETIME_RCVD` | sentDate | > 0 |
| `TSK_DATETIME_SENT` | sentDate (same value) | > 0 |
| `TSK_EMAIL_CONTENT_PLAIN` | text body | non-empty |
| `TSK_MSG_ID` | PST NID, or literal `"Not available"` for MIME (id = −1) | always |
| custom `EMAIL_HAS_ATTACHMENT` (STRING, "Has Attachments") | `"Yes"` | has attachments |
| `TSK_PATH` | folder path | non-empty |
| `TSK_EMAIL_CC` | cc | non-empty |
| `TSK_EMAIL_CONTENT_HTML` | html | non-empty |
| `TSK_EMAIL_CONTENT_RTF` | rtf (PST only) | non-empty |
| `TSK_THREAD_ID` | UUID | non-empty |

`TSK_EMAIL_BCC` is never set (BCC still creates accounts); the Internet Message-ID is never stored; `TSK_EMAIL_REPLYTO` unused.
Relationships: `CommunicationsManager.addRelationships(senderInstance /*nullable*/, recipientInstances, artifact, Relationship.Type.MESSAGE, sentDate)`. Post: `blackboard.postArtifact(art, "Email Parser", jobId)` (triggers KWS indexing of the artifact).

Attachments → derived files: `fileManager.addDerivedFile(name, relPath, size, ctime, crtime, atime, mtime, isFile=true, parent = container file, "", "Email Parser", version, "", XOR1)`, then `communicationArtifactsHelper.addAttachments(art, new MessageAttachments(fileAttachments, emptyList))` → `TSK_ATTACHMENTS` JSON attribute (file attachments reference the derived file obj id). Each `AttachedEmailMessage` recurses into `createEmailArtifact` on the **same container file** (no thread id, no depth limit). Cancellation is checked between steps (an artifact can exist without relationships).

**XOR1 encoding** (`EncodedFileOutputStream`): an ASCII header beginning `TSK_CONTAINER_XOR1_` padded with `x` (verify exact length in sleuthkit `EncodedFileUtil`), then every byte XOR `0xCA`; DB `encoding_type = 1`. Purpose: keep host AV from quarantining malicious attachments. postmortem must at least decode XOR1 to read imported Autopsy cases (and may keep it for the same AV reason).

### 3.7 Threading (`EmailMessageThreader`, JWZ-derived) – as implemented

Runs per container file (or per mbox chunk).
1. **ID table** by messageID (empty id `""` is one shared key). If the found container already holds a message → new container with key `"<Bogus-id: %d >"`; if it is an empty placeholder → put the message in it; else create.
   `processMessageReferences`: references list (+ inReplyTo appended **again**; may contain `null`/`">"` junk creating junk placeholders); walk refs creating containers; link `ref` under `parent_ref` if `parent_ref != null`, `ref.parent == null`, `parent_ref != ref`, and `!parent_ref.isChild(ref)` (deep search with visited set); `parent_ref = ref`. Then if `parent_ref == container` or `container.isChild(parent_ref)` → `parent_ref = null`. Detach the container from any existing parent. If `parent_ref` set: Autopsy bug `container.setParent(container)` (self) then `parent_ref.addChild(container)`.
2. **Root set** = containers with `parent == null`.
3. **Prune**: drop roots with no message and no children; recursively remove message-less children whose grandparent is non-null, promoting their children. Message-less roots with children are kept (JWZ would promote a single child).
4. **Subject grouping**: simplified subject: if subject matches `^[R|r][E|e].*?:.*` → `subject.replaceAll("[R|r][E|e].*?:", "").trim()` and `isReply = true`; else raw subject (untrimmed). Over-matches ("Report: x", "Regarding:"); no Fwd/AW/SV. Subject table: key = root message's simplified subject, or first child with a subject for empty roots; empty keys skipped. Insert if absent; replace if table entry has a message and this root is empty, or this root is not a reply and the entry is a reply. Merge each root R with T = table[subject]: T null or T == R → keep R; both empty → move R's children into T, drop R; T empty, R not → `T.addChild(R)`, drop R; R empty, T not → `R.addChild(T)`, table = R, keep R (T may already be in the final set → two thread ids, last wins); T non-reply & R reply → `T.addChild(R)`; T reply & R not → `R.addChild(T)`, keep R; else new dummy container with T and R as children, keep dummy. Iteration over a HashSet ⇒ **non-deterministic**.
5. **Thread ids**: each final root gets `UUID.randomUUID()`, assigned by DFS to all messages in its subtree.

postmortem: implement a correct deterministic JWZ / RFC 5256 REFERENCES algorithm; proper msg-id tokenizer (`<…>` extraction); subject prefixes `Re`, `Fwd`, `Fw`, `AW`, `SV`, `VS`, `Antw`, `RE[n]`, `[list-tag]`; stable thread id (e.g. hash of root Message-ID or of the earliest message); thread across the whole data source/case as a post-pass over stored message-ids (store `Message-ID`, `In-Reply-To`, `References` as attributes — Autopsy discards them).

### 3.8 vCard (`VcardParser`, ez-vcard)

* `Ezvcard.parse(new InputStreamReader(stream, UTF_8)).all()` – multiple cards per file, vCard 2.1/3.0/4.0.
* `TSK_NAME`: `FN` if present, else from `N`: each prefix + " ", given + " ", family + " ", each suffix + " ", then `"(" + each additional name + " " + ")"` (not trimmed).
* `TEL`: number = `getText()` or `getUri().getNumber()`; skip if both empty. No TYPE → `TSK_PHONE_NUMBER` with `getText()` (NPE if only a URI – guard). With TYPE: first token `value.toUpperCase().replaceAll("\\s+","").split(",")[0]` → custom STRING attribute `TSK_PHONE_NUMBER_<TOKEN>`, display "Phone Number (<Capitalized>)" (e.g. `_HOME`, `_CELL`, `_WORK`, `_VOICE`, `_PREF`, any `X-…`). Each number also → `PHONE` account instance.
* `EMAIL`: no TYPE → `TSK_EMAIL`; else `TSK_EMAIL_<TOKEN>`, display "Email (<Token>)" (`_HOME`, `_WORK`, `_INTERNET`, `_PREF`…). Each → `EMAIL` account instance.
* Custom attribute types cached in a static `ConcurrentHashMap` (`getOrAddAttributeType`), cleared at startUp/shutDown.
* `TSK_URL` per URL; `TSK_ORGANIZATION` first ORG value only. **Not extracted**: ADR, BDAY, NOTE, TITLE, NICKNAME, IMPP, GEO, UID.
* Device account: `DEVICE` account from `dataSource.getDeviceId()`.
* Dedupe: skip if `blackboard.artifactExists(file, TSK_CONTACT, attrs)`.
* Else: `TSK_CONTACT` data artifact on the .vcf → `extractPhotos` → `addRelationships(deviceAccount, accounts, art, Relationship.Type.CONTACT, file.getCrtime())` → `postArtifact`.
* Photos: folder `ModuleOutput/Email Parser/<fileName>_<objId>/` (the "already extracted" check runs after creating the folder → always extracts). Photo i: skip URL photos and photos without TYPE; strip `image/`; bmp→`.bmp`, gif→`.gif`, jpeg→`.jpg`, png→`.png`, else no extension; name `photo_<i><ext>`; plain `FileOutputStream` (**not XOR-encoded**, never closed); `addDerivedFile(..., ctime=file.ctime, crtime=file.crtime, atime=file.atime, mtime=file.atime /*bug*/, parent = the TSK_CONTACT artifact, encoding NONE)`; then `ModuleContentEvent(vcf)` + `addFilesToJob`.

Errors are posted with `IngestMessage.createErrorMessage(moduleVersion, …)` (version string used as the source – bug). Unused bundle strings: `couldntFindCharset`, `failedToParseNMsgs`, `MboxParser.handleAttch.*`.

### 3.9 Pure-Rust replacements

| Concern | Crates | Notes |
|---|---|---|
| MIME / EML | **mail-parser** (Stalwart): zero-copy, RFC 5322/2045-49/2047/2231, `encoding_rs` charsets, lenient, nested rfc822. Alt: `mailparse` | `text_body`/`html_body` handle multipart/alternative; raw header offsets give the true top-level header block for `TSK_HEADERS` |
| mbox | `mail_parser::mailbox::mbox::MessageIterator` or `memchr::memmem` scan for `\nFrom ` | Stream from the image (no temp copy, no 1000 MiB split); optional mboxrd `>From ` unquoting and `Content-Length` |
| Charset fallback | `chardetng` + `encoding_rs` | |
| PST/OST | **outlook-pst** (MS-PST NDB/LTP/messaging, ANSI+Unicode); libpff via FFI is not pure Rust | Needs permute + cyclic decoding (cyclic is obfuscation – do not flag as encrypted), OST 2013 4 K pages, LZFu (`compressed-rtf` or MS-OXRTFCP ~100 lines), RTF de-encapsulation (MS-OXRTFEX), recipient table (`PR_SMTP_ADDRESS`) instead of display strings, recursion into embedded messages, `PR_CLIENT_SUBMIT_TIME` = sent vs `PR_MESSAGE_DELIVERY_TIME` = received |
| vCard | **calcard** (Stalwart, lenient 2.1/3.0/4.0); alt `ical`, `vcard_parser` | 2.1 QUOTED-PRINTABLE/CHARSET; BOM-tolerant detection |
| Addresses | mail-parser structured `Address` | lower-case for account identity |
| XOR1 | trivial | reader needed for imported cases |
| Threading | hand-written JWZ (~200 LOC) | deterministic ids |

### 3.10 Edge cases and gaps to fix in postmortem

* Detection: ≤ 64-byte files; vCards with BOM; EML requires `.eml`; `.msg` (Outlook OLE/CFB – `cfb` crate + MAPI property streams), `.emlx` (Apple Mail: length line + RFC822 + plist), Maildir (extension-less) are never parsed – add them.
* Headers/dates: Message-ID-less messages collide on `""`; References junk; PST References lost when last; multipart `TSK_HEADERS` = part headers; RCVD == SENT (use last `Received:` header / PST delivery vs submit time).
* File-name collisions (PST `fileID = 0`; MIME index+sentDate).
* Resources: no rfc822 nesting depth limit; bodies held whole in memory; mbox parsed twice, PST traversed twice; attachments queued only at container end (20 GB PST ⇒ long delay and all derived-file objects in memory).
* Non-mail PST items become `TSK_EMAIL_MSG`; map IPM.Contact → `TSK_CONTACT`, IPM.Appointment → `TSK_CALENDAR_ENTRY`, IPM.Task → task artifact.
* Random UUID thread ids; threads never cross containers.

---

## 4. Performance problems & improvement ideas for postmortem

### 4.1 Keyword search / indexing

| # | Autopsy problem | postmortem improvement |
|---|---|---|
| K1 | Out-of-process Solr JVM (2 GB heap default), HTTP + javabin per 30-doc batch, 10-retry/10 s backoff, 12×5 s start-up wait, port conflicts, orphaned Solr processes killed by command-line pattern | In-process Tantivy: one `IndexWriter` per case (heap budget e.g. 256–512 MB, configurable), documents added from ingest worker threads via a bounded channel; no network, no start-up latency |
| K2 | Every chunk is stored **three times**: `text` (stored + term vectors with positions & offsets), `content_str` (stored, lower-cased, indexed as one 32 KB term), `content_ja`; plus the window overlap duplicated in the next chunk. Index ≈ 3–5× extracted text | Index tokens once (positions only, no term vectors — compute highlights by re-scanning the stored page). Store page text once, zstd-compressed, outside the inverted index. Drop the giant `content_str` term entirely |
| K3 | Substring/regex search = Lucene `RegexpQuery` `/.*x.*/` over every 32 KB term in `content_str` → full scan of the term dictionary per query, then Java regex again on every returned doc | Two-tier: (a) build an n-gram (trigram) side index over pages (`content_tri` field or a dedicated `fst`/`roaring` postings table) to pre-filter candidate pages for literals & regex-required literals (extract mandatory literals with `regex-syntax` HIR, as in Google Code Search / `zoekt`); (b) verify with `regex::bytes`/`aho-corasick` on decompressed pages in parallel (rayon). Unindexed regex still works by brute-force scan, but at memory bandwidth |
| K4 | Ingest-time inline search runs keywords one by one: `containsIgnoreCase` + `Pattern.compile` **per keyword per chunk** (regex recompiled for every chunk!), then a second pass to extract hits | Compile all enabled keywords once per job: literals into one `aho-corasick` automaton (ASCII case-insensitive, or Unicode case-folded input), regexes into a `regex::RegexSet` for the "any match?" test, then run only the matching individual regexes for hit extraction. One pass over each page |
| K5 | Exact-match inline search re-tokenises the whole chunk with Lucene `StandardAnalyzer` for every candidate keyword | Tokenise each page once (same tokenizer used for the index), match multi-token phrases with a token-level Aho-Corasick over token ids |
| K6 | NFKC normalisation + UTF-16 round trip + per-char validity scan + `toLowerCase` + UTF-8 re-encoding (twice: original and lower-case sizes) for every 512-char segment | Single streaming pass on UTF-8: `unicode-normalization` NFKC iterator → invalid-char replace → case-fold, sizes counted in bytes as produced. Or skip NFKC for pure-ASCII runs (fast path with `simdutf8`/`is_ascii`) |
| K7 | Hits for the whole ingest job kept in a static `ConcurrentHashMap` until job end; cancel = lose all hits; very large jobs = unbounded memory | Write hits to the case DB incrementally (staging table keyed by job, de-duplicated with a unique index on (obj, keyword, hit)); create artifacts in batches; resumable after crash |
| K8 | Only the *first* hit per (object, keyword, hit-term) is kept; counts per file are lost | Store hit count and first offset/page; the viewer can enumerate all occurrences cheaply |
| K9 | String extraction (EnglishOnlyStream) is byte-at-a-time with `StringBuilder`; International mode tries UTF-16LE, UTF-16BE and UTF-8 at **every** offset | SIMD scan (`memchr`-style classification tables) for printable runs; UTF-16 detection via NUL-interleave heuristics on 64 KB blocks; parallelise unallocated space by splitting into independent blocks with small overlaps |
| K10 | Tika: separate thread per file, `PushbackReader`, metadata parsed in a **second full parse** (`getMetadata()` re-parses the whole document), tiered timeouts up to 3 h | Pure-Rust per-format extractors returning text + metadata in one pass; run in a worker *process* pool (crash/timeout isolation, `memory limit`), timeouts proportional to size with a hard cap |
| K11 | OCR: image copied to case temp, `tesseract.exe` spawned per image, polled every 1 s; Windows-only | Persistent OCR worker (tesseract API via `leptess` or `ocrs`) fed from memory buffers; GPU optional; skip tiny/icon images by dimension (not file size); cache by hash so duplicate images are OCRed once |
| K12 | `queryIsFullyIndexed` = 2 Solr queries per selected node (cached for 10 entries) to decide whether the text viewer is available | Keep a `text_pages(obj_id, n_pages, status)` row in the case DB; the viewer decision is one indexed lookup |
| K13 | Ad-hoc search paginates 512 docs per cursor with stored `content_str` returned for every doc (32 KB each) | Return doc ids only; fetch/verify pages lazily and in parallel; stream results into the table as they arrive; cancelable |
| K14 | Unique-words report walks the `/terms` handler 1000 terms per HTTP request, cannot filter by data source | Iterate the Tantivy term dictionary directly (all segments, k-way merge); filter by data source using a `ds_id` fast field + doc-freq on a filtered bitset |
| K15 | Duplicate content (same file hash in many places) is extracted, indexed and searched N times | Content-addressed text store: extract/index once per SHA-256 (or MD5 already computed by hash module); map additional obj ids to the same text id; keyword hits are fanned out to all objects sharing the hash |
| K16 | Multi-user: SolrCloud + ZooKeeper cluster to operate; collection per case; shards = live nodes | Single `postmortem-indexd` per case on the shared storage host (or embedded in the coordinator), clients use gRPC; one writer, snapshot readers; index replication only if needed. Much less ops burden |

### 4.2 Recent Activity

| # | Autopsy problem | postmortem improvement |
|---|---|---|
| R1 | Every SQLite/ESE/registry artifact source is **copied out** of the image to `<case>/Temp/RecentActivity/...` before parsing (`ContentUtils.writeToFile`), often for each of several extractors that read the same file (e.g. `History` copied separately for history and downloads, `Web Data` for autofill and addresses; hives copied before each RegRipper run) | Parse directly from the image via a `Read+Seek` adapter: `rusqlite` with a custom VFS (read-only, backed by the TSK/`postmortem-vfs` reader, merges `-wal`/`-journal` siblings), `nt-hive`/`notatin` on `&[u8]`/mmap, ESE reader over `Read+Seek`. Cache opened artifacts per job so several extractors share one parse |
| R2 | RegRipper (Perl, external process) runs **twice per hive** (Autopsy profile + full profile), each run re-reads the whole hive; output text is then re-parsed with fragile line/XML parsing; Perl must be installed off Windows | Native registry parsing once per hive (`notatin`, which also replays transaction logs, or `nt-hive`); all registry extractors walk one in-memory hive; implement only the keys listed in §2.7 natively, plus an optional text report |
| R3 | External executables with temp files and intermediate SQLite DBs: `mm_artifact_parser` (Prefetch, SRUM), NirSoft `ESEDatabaseView.exe` (Edge, Windows-only, dumps **every** table to CSV), `thumbcache_viewer_cmd.exe` (Windows-only), pasco2 (spawns a JVM per index.dat) | Pure-Rust parsers in-process: prefetch (+ LZXPRESS-Huffman), ESE (read only the needed tables/columns), thumbcache, index.dat; no temp files, cross-platform |
| R4 | Domain categorisation, web-account detection and search-engine analysis each re-query **all** web artifacts after the browsers finish (3 extra full scans), each doing per-row `BlackboardArtifact.getAttribute` lookups (N+1 queries) | Emit URL-derived artifacts in the same pass in which the browser artifact is created (stream processor over a channel), or one bulk SQL pass with joined attributes. Pre-compile domain suffix trie (`psl` crate / `publicsuffix`) and a `HashMap` of categories |
| R5 | The 101 k-line domain-category CSV and 25 k-line USB database are parsed at module start-up every job | Ship as pre-built `fst`/`phf` maps (compile-time `include_bytes!` + lazy zero-copy deserialisation) |
| R6 | Recent-activity extractors run **sequentially** on one thread per data source | Run independent extractors in parallel (rayon/tokio): browsers, registry, prefetch, SRUM, jump lists, recycle bin all have disjoint inputs; only the post-processors (domain categorisation, accounts) depend on browser output |
| R7 | Chrome cache: every cache entry becomes a derived file written to disk (`ModuleOutput/...`) even when it is tiny or already present in another profile | Store cache bodies content-addressed (dedupe by hash) and only materialise to disk on demand; keep metadata in DB |
| R8 | Lots of string date parsing per row via `SimpleDateFormat`/regex | Parse integer timestamps directly (WebKit µs since 1601, Mac absolute time, Unix ms) with `chrono`/`time`; batch-insert artifacts in one transaction per extractor |
| R9 | Recycle Bin looks up `$R*` files across the **whole data source** by name for every `$I` file; Zone.Identifier loads all `TSK_WEB_DOWNLOAD` of the case into memory | Resolve `$R` siblings in the same `$RECYCLE.BIN/<SID>` directory (one directory listing); look up downloads by indexed `path_id` |
| R10 | Domain categorisation sort comparator fetches artifact attributes on every comparison (O(n log n) DB round-trips) | Single query returning (artifact_id, url, domain, time) rows, sort in memory, group by host |
| R11 | No re-run dedupe → repeated ingest doubles RA output | Make extractors idempotent: natural keys per artifact (source obj, type, key attributes hash) with `INSERT … ON CONFLICT DO NOTHING` |

### 4.3 Email

| # | Autopsy problem | postmortem improvement |
|---|---|---|
| E1 | MBOX parsed twice; PST traversed twice (partial + full) for threading | Single pass collecting thread keys (Message-ID, In-Reply-To, References, subject) in a compact side table; thread as a post-pass (case-wide, deterministic) |
| E2 | Whole MBOX/PST copied to case temp before parsing (> 1000 MiB MBOX split with a broken splitter) | Stream directly from the image (`Read+Seek` adapter); mbox splitting by streaming `\nFrom ` scan with no size threshold |
| E3 | Attachments are queued to the ingest pipeline only after the whole container is processed; derived-file objects held in memory | Add attachments in batches (e.g. every 1 000 messages or 256 MB) so downstream modules (hash, KWS, EXIF) start early and memory stays bounded |
| E4 | Every message = 1 artifact + up to 14 attribute rows + account lookups (cache per container only) + relationship rows, each posted individually (`postArtifact` per message, which fires events and KWS indexing one by one) | Batch inserts (prepared statements, one transaction per N messages), case-wide account cache, bulk event emission, KWS indexing of message bodies through the normal text pipeline in batches |
| E5 | Bodies stored as attribute text (HTML + plain + RTF + headers duplicates) | Store raw RFC822 (or MAPI properties) once, compressed and content-addressed; expose parsed fields lazily; index text once |
| E6 | Attachments XOR-written then re-read by every downstream module | Optional XOR; or keep attachments virtual (offset/length into the container + decoder) and only materialise when exported |

### 4.4 Priorities (highest leverage first)

1. Replace Solr with in-process Tantivy + stored compressed pages + trigram pre-filter; single regex engine for ingest and ad hoc (K1–K4, K16).
2. Compile keyword sets once per job (Aho-Corasick + RegexSet) and persist hits incrementally (K4, K7).
3. Content-addressed text extraction/indexing per file hash (K15) – large wins on images with many duplicates (OS files, mail attachments).
4. Zero-copy parsing from the image for SQLite/registry/ESE/PST/MBOX (R1, E2) and parallel RA extractors (R6).
5. Native parsers for every external tool (RegRipper, pasco2, ESEDatabaseView, mm_artifact_parser, thumbcache viewer, Tesseract optional) so all features work on Linux/macOS (R2, R3, K11).
6. Batch DB writes for artifacts/attributes/accounts/relationships (E4, R8) and bounded-memory streaming of attachments (E3).
