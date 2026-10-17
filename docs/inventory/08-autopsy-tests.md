# 08 — Autopsy's own tests: parity inventory (Autopsy → postmortem)

This is the test-parity reference. It lists every automated test, test harness and test fixture in the Autopsy source tree (`/home/tatu/slop/autopsy`, read-only), what each one checks with concrete inputs and expected outputs, which fixtures it needs and whether they can be reused, and which postmortem area it maps to. It also describes Autopsy's golden-DB-dump regression harness and proposes a Rust equivalent. It ends with a tickable parity checklist.

Per `docs/LOOP.md` ("Testing rules"): before a feature is finished, port the cases listed here for its area and tick the matching checklist line.

Unless stated otherwise, paths are relative to `/home/tatu/slop/autopsy/`. Crate names in parentheses are suggestions. `docs/PLAN.md` is authoritative once it exists.

## Contents

1. Overview: what Autopsy tests, and what it does not
2. Fixture catalogue (sizes, origin, licence, reusability)
3. Ingest pipeline: file filters
4. Embedded file extraction (archives, zip bombs)
5. Encryption detection
6. Hash sets and keyword-list inputs (`test/input`)
7. Central repository: normaliser, data model, accounts, personas
8. Common properties search (intra-case and inter-case)
9. Keyword search
10. Data source summary
11. Discovery (domain search)
12. External viewer rules
13. Machine translation
14. Recent activity: IE `index.dat` and Windows time formats
15. YARA
16. Not relevant to postmortem
17. Test-support code (custom artefact creators, integration-test framework, test utilities)
18. The regression harness (golden DB dump), and the postmortem equivalent
19. Gaps: important behaviour Autopsy never tests
20. Parity checklist

---

## 1. Overview

Autopsy's automated coverage is thin. Most of its confidence comes from the GUI-driven regression harness (section 18). The JUnit tests fall into four groups.

| Group | Location | Runs how | Count |
|---|---|---|---|
| Unit tests (Mockito, no case DB) | `Core/test/unit/src/…` | `ant test-unit` | 11 classes: data source summary (3), discovery (3), external viewer (2), translators (2, empty), plus helpers |
| Keyword search unit tests | `KeywordSearch/test/unit/src/…` | `ant test-unit` | 3 classes plus a suite |
| qa-functional (NetBeans module suite, real case, real ingest, downloaded images) | `Core/test/qa-functional/src/…` | `ant test-qa-functional`; images fetched by `getTestDataFiles` | 4 central repository, 6 common properties (2 effectively empty), 2 ingest, 1 encryption detection |
| Integration-test framework (YAML output and gold diff) | `Core/src/…/integrationtesting/**`, `Core/test/qa-functional/…/integrationtesting/TestRunner.java` | `ant integration-test` (4 h timeout) | 1 group: `UserActivitySummaryTests` |
| GUI regression harness | `test/script/regression.py`, `Testing/…/RegressionTest.java`, `AutopsyTestCases.java` | Windows or Cygwin only | 1 end-to-end flow per image |
| Third-party | `thirdparty/pasco2/test` (JUnit 3, never wired in), `thirdparty/yara/YaraWrapperTest` (manual `main`) | manual | 7 + 1 |
| Release tooling | `release_scripts/localization_scripts/test/*.py` | manual | 4 (not product behaviour) |

Things that may surprise:
- **There are no tests for these core areas** (the qa-functional tests touch some indirectly):
  - file-type (MIME) detection
  - hash-database parsing and lookup
  - interesting-files rule evaluation, apart from the ingest file-filter cases, which use the same `FilesSet` engine
  - timeline filters and events
  - Recent Activity / registry / browser parsers
  - email parsing, carving and EXIF
  - any filesystem (TSK's job)
  - reports
  - tags
  - the keyword-search regex lists

  Section 19 lists what postmortem must derive itself.
- Several tests are **dead or vacuous**:
  - the translator tests are fully commented out
  - the inter-case common-property test bodies are commented out (JIRA-4166)
  - the pasco2 tests use absolute `D:\` paths and the Win32 API
  - the CR "commented out" Javadoc is wrong: those tests are active

  The intended expected values are still recorded below, because they document behaviour.
- Several assertions **encode known bugs**. Each is flagged with **(bug)**. For each one, decide deliberately whether to fix it, and record an ADR when the fix changes behaviour relative to Autopsy.

## 2. Fixture catalogue

| Fixture | Size | Where | Used by | Reuse in postmortem? |
|---|---|---|---|---|
| `Core/test/qa-functional/data/PasswordDetection_img1_v1.img` | 3,997,696 B (3.9 MB). MD5 `bee770e3472abd27823be6092e21bc05`, SHA-256 `aa9aaea5252ab222b95c897587b961fc0b08e7b969a4c7a4608f34830fc68d5f` | committed in git (the only qa image that is) | EncryptionDetectionTest.testPasswordProtection | **Partly.** ISO9660 with a UDF 1.5 bridge (ImgBurn 2.5.8), no partition table, 2048-byte blocks. It contains an Apache-2.0 `License.txt` and two third-party `.mdb` files: `Recovery-protected.mdb` is **GPL-3** (MaxSoft, © Craig Phillips) and `Viewer-protected.mdb` is freeware (Hamzasoft). The office and PDF files have no stated licence. **Do not vendor the image into the Apache-2.0 repo.** Reference it by path or hash from an opt-in `slow-tests` fixture directory, and regenerate equivalent protected documents ourselves (see section 5). |
| `IngestFilters_img1_v1.img`, `IngestFilters_local1_v1.zip`, `EmbeddedIM_img1_v2.vhd`, `BitlockerDetection_img1_v1.vhd`, `SqlCipherDetection_img1_v1.vhd`, `VeracryptDetection_img1_v1.vhd`, `CommonFiles_img{1..4}_v1.vhd`, `c{1,2,3}ds{1,2}_v1.vhd`, `CommonFilesAttrs_img{1..4}_v1.vhd` | unknown (not downloaded) | Google Drive `https://drive.google.com/uc?id=<ID>`, fetched by `Core/build.xml` target `getTestDataFiles` (IDs in section 17.4) | qa-functional tests | **No licence stated.** Do not vendor. Rebuild equivalents synthetically with `pm-testkit` builders from the contents described in this document. Optionally keep a `slow-tests` job that downloads the originals for cross-checks. |
| `test/input/NSRL.txt-md5.idx`, `notablehashes.txt-md5.idx` | 307 B, 316 B | git | regression harness | **Yes.** Trivial, factual data. Reuse as hash-index parser fixtures (section 6). |
| `test/input/notablekeywords.xml` | 689 B | git | regression harness | **Yes.** Tiny keyword-list XML with a BOM and Arabic terms (section 6). |
| `thirdparty/pasco2/test/isi/pasco2/{empty,first}.{cache,history}.index.dat` | 33 kB, 33 kB, 49 kB, 33 kB | git | pasco2 TestRecordLayer | **No: GPL v2** (pasco2). Use the record counts as an oracle only, and generate our own `index.dat` files. |
| `thirdparty/yara/YaraWrapperTest/resources/hello.txt` / `hello.compiled` | 11 B / 5.2 kB | git | manual YARA harness | `hello.txt` content (`Hello World`) can be retyped. The compiled rule is libyara-version-specific: write a source rule instead. |
| Regression-harness images | n/a | not in the repo (README: `ant test-download-imgs`) | regression.py | Pick our own corpus (section 18). |

---

## 3. Ingest pipeline: file filters (ingest scheduler + interesting-files `FilesSet` engine)

Source: `Core/test/qa-functional/src/org/sleuthkit/autopsy/ingest/IngestFileFiltersTest.java`. Area: ingest pipeline and file filters (`pm-ingest`), and the interesting-files rule engine (`pm-rules`). See also `03-ingest-modules.md`.

### Engine semantics under test
These come from `modules/interestingitems/FilesSet.java` and `ingest/IngestTasksScheduler.java`.
- `FilesSet(name, description, ignoreKnownFiles, ignoreUnallocatedSpace, rules)`.
- `Rule(ruleName, fileNameCondition, metaTypeCondition, parentPathCondition, mimeTypeCondition, fileSizeCondition, dateCondition, exclusive)`. The tests pass null for mime, size, date and exclusive, so every rule is inclusive.
- **Membership:** any inclusive rule matches and no exclusive rule matches. Conditions within a rule are ANDed.
- With `ignoreUnallocatedSpace = true`, file types UNALLOC_BLOCKS, SLACK and UNUSED_BLOCKS are rejected.
- `MetaTypeCondition.FILES` means `isFile()`.
- `ParentPathCondition("dir1")` is a **case-insensitive substring** match against `getParentPath() + "/"`.
- `ExtensionCondition("jpg")` is a case-insensitive exact match on the extension; a leading `.` is stripped.
- `FullNameCondition("file.docx")` is a case-insensitive exact match on the name.
- **Scheduler:** `.` and `..` are never processed. Directories bypass the filter (`if (!file.isDir() && !shouldBeCarved && !fileAcceptedByFilter) skip`).
- The tests' oracle: "processed" means the file's MIME type is non-null and non-empty (FileTypeId ran). "Skipped" means the MIME type is null. All listings use `findFiles("%%")`, which covers all files in all data sources.

> **Inconsistency to resolve.** The tests assert that every file outside the filter, including directories, has a null MIME type. But the scheduler always processes directories, and `testEmbeddedJpg` expects DERIVED/LOCAL directories to be typed. Either filesystem directories are never assigned a MIME type, or the assertions are stale. Decide postmortem's rule (recommendation: a directory gets no MIME type, but the filter never excludes directory traversal) and write the tests to match.

### Inputs
- Disk image `IngestFilters_img1_v1.img` (raw) and logical set `IngestFilters_local1_v1.zip`.
- Each test creates a fresh case `IngestFilter_<test>_<ts>` with ingest type FILES_ONLY.
- Image contents are known only from the assertions: `dir1/` with `file.jpg` and others, `dir2/`, a root-level jpg/gif, `file.docx`, unallocated space with 2 carvable jpg/gif, **62** files in total (72 after carving).

### Cases

| Test | Filter (set desc, ignoreUnalloc) | Modules | Expected |
|---|---|---|---|
| testBasicDir | Rule "testFileType": FILES + ParentPath("dir1") ("Filter to find all files in dir1.", **false**) | FileTypeId | `dir1/file.jpg` → `image/jpeg`. Every file under `/dir1/` (excluding `.`, `..` and SLACK) is typed; everything else is null. |
| testExtAndDirWithOneRule | One rule: Ext("jpg") AND FILES AND ParentPath("dir1") (true) | FileTypeId | Total **62**. Only jpgs in `/dir1/` are typed. |
| testExtAndDirWithTwoRules | Rules "FindJpgExtention" Ext(jpg)+FILES, **OR** "FindDir1Directory" FILES+ParentPath(dir1) (true) | FileTypeId | Total 62. Every `.jpg` anywhere is typed. In `/dir1/`, everything except `.`, `..` and SLACK is typed. Everything else is null. |
| testFullFileNameRule | FullName("file.docx") + FILES (true) | FileTypeId | Total 62. Only `file.docx` is typed. |
| testCarvingWithExtRuleAndUnallocSpace | Ext(jpg) OR Ext(gif), FILES (**false**, so unallocated space is processed) | FileTypeId + PhotoRec | Total **72**. Every jpg/gif is typed (dir1, dir2, `$CarvedFiles`, root). Every `Unalloc_*` file is typed. Everything else is null. Exactly **2** jpg/gif files have a parent path starting with `/$CarvedFiles/`. |
| testCarvingNoUnallocatedSpace | Same rules (**true**) | FileTypeId + PhotoRec | Ingest start-up returns exactly **1** module error, from module "PhotoRec Carver". A carver must refuse to start when unallocated space is excluded. |
| testEmbeddedJpg | Local-files data source with the zip. Ext(jpg) OR Ext(zip) (false) | FileTypeId + EmbeddedFileExtractor | Total **39**. Every jpg/zip is typed, exactly **11** of them (10 jpg + 1 zip). DERIVED/LOCAL directories are typed. Everything else is null. Files extracted from a filtered-in zip are themselves subject to the filter. |

**Port plan.** Build a synthetic FAT image in `pm-testkit` with `dir1/{file.jpg, a.txt, …}`, `dir2/b.jpg`, `root.gif`, `file.docx` and two JPEG/GIF signatures in unallocated clusters. Assert the same *relations*: the filter logic, a carver refusing without unallocated space, extracted children being filtered. Do not assert Autopsy's absolute counts, which depend on its image. Also port the `FilesSet` condition semantics as pure unit tests (see the checklist).

---

## 4. Embedded file extraction (archives, zip bombs, archive encryption)

Source: `Core/test/qa-functional/src/org/sleuthkit/autopsy/ingest/EmbeddedFileTest.java`. Area: embedded file extractor (`pm-ingest-efe`), hash calculation, and encryption artefacts. See `03-ingest-modules.md`.

Setup:
- Image `EmbeddedIM_img1_v2.vhd`, case `EmbeddedFileTest`.
- Modules: EmbeddedFileExtractor + HashLookup (defaults, no hash DB, so MD5s are only computed). FILES_ONLY.
- `HASH_VALUE = "098f6bcd4621d373cade4e832627b4f6"`, which is MD5(`"test"`).
- One `testAll()` runs the sub-checks in this order:

1. **EncryptionAndZipBomb.**
   - `name LIKE '%%'` returns **2221** files.
   - `password_protected.zip`, `level1_protected.zip` and `42.zip` (case-insensitive) each have exactly 1 artefact, `TSK_ENCRYPTION_DETECTED`; 3 in total. The 42.zip used here is password-protected.
   - `DepthTriggerZipBomb.zip` and `RatioTriggerZipBomb.zip` each have exactly 1 `TSK_INTERESTING_ITEM` with `TSK_SET_NAME` = **"Possible Zip Bomb"**; 2 in total.
   - Every other file has 0 artefacts.
   - Thresholds in `SevenZipExtractor`: `MAX_DEPTH = 4`; `MAX_COMPRESSION_RATIO = 600` (a ratio of 600 or more triggers).
2. **Extension.**
   - `extension='txt' AND parent_path='/ZIP/extension/zipFileWithTxtExtension.txt/'` returns exactly 1 file, `file.txt`.
   - A zip renamed `.txt` is detected by signature and extracted.
3. **BigFolder.**
   - `parent_path LIKE '%big folder/'` (excluding dots and `%slack`) returns **1000** files.
   - Names match `file[1-9]\d*.txt` (file1.txt … file1000.txt), and each MD5 = `098f6bcd…`.
4. **DeepFolder.**
   - `%deep folder/` has one child, `dir1`. Recursion gives `dirN` at depth N.
   - **25** `file.txt` files are reached.
   - The dir chain starts `dir1/dir2/` and ends `dir24/dir25/`.
   - The first file reached is the deepest one. This relies on child listing in **name order**: `dirN+1` sorts before `file.txt`.
5. **EmbeddedFile.** Each of these returns exactly 1 row:
   - `level3.txt` @ `/ZIP/embedded/level3.zip/`
   - `level2.txt` @ `/ZIP/embedded/level3.zip/level2.zip/`
   - `level1.txt` @ `/ZIP/embedded/level3.zip/level2.zip/level1.zip/`
   - `%embedded/` has exactly one non-slack child, `level3.zip`.
   - Following each first child gives level3.zip → level2.zip → level1.zip → level1.txt.
   - **Extracted children live at `/<parent path>/<archive name>/<entry path>`.**
6. **Content.**
   - `extension='txt' AND name != 'zipFileWithTxtExtension.txt' AND parent_path NOT LIKE '%_protected%'` returns **1029** files, each with MD5 `098f6bcd…`.
   - Entries inside password-protected zips are not extracted and have no hash.

**Port plan.** Use a `pm-testkit` zip builder (pure Rust, for example the `zip` crate in tests) to generate:
- nested zips 3 deep
- a zip renamed `.txt`
- 1000 files containing `test`
- 25-deep directories
- ZipCrypto/AES-protected zips
- a depth bomb (more than 4 levels) and a ratio bomb (600:1 or more, for example 600 MB of zeros declared, built streaming)

Assert the same paths, artefacts, set name, hashes and ordering. The hash `098f6bcd4621d373cade4e832627b4f6` is a free MD5 test vector.

---

## 5. Encryption detection

Source: `Core/test/qa-functional/src/org/sleuthkit/autopsy/modules/encryptiondetection/EncryptionDetectionTest.java`. Area: encryption detection module (`pm-ingest-encryption`). It needs volume support, filesystem support (including ISO9660/UDF for the password image) and OLE/OOXML/PDF/Access/SQLite sniffing. See `03-ingest-modules.md`.

Default settings (`EncryptionDetectionIngestJobSettings`):
- minimum entropy **7.5**
- minimum file size **5,242,880 B** (5 MiB; display it as 5.2 MB in SI)
- size must be a multiple of 512: **true**
- slack files allowed: **true**

| Test | Image / ingest | Expected |
|---|---|---|
| testBitlockerEncryption | `BitlockerDetection_img1_v1.vhd`, FILES_ONLY | The volume named **`vol2`** has exactly 1 `TSK_ENCRYPTION_DETECTED` whose `TSK_COMMENT` is exactly **`"Bitlocker encryption detected."`**. All other volumes have 0. |
| testPasswordProtection | `PasswordDetection_img1_v1.img`, FILES_ONLY | `findFiles("%%", parentSubstring)` counts: `ole2` **11**, `ooxml` **13**, `pdf` **6**, `mdb` **25**, `accdb` **10**. Each non-slack regular file whose first dot-segment ends with `-protected` has exactly 1 `TSK_ENCRYPTION_DETECTED`; all others have 0. |
| testVeraCryptSupport | `VeracryptDetection_img1_v1.vhd`, ALL_MODULES with only this module | Exactly 1 data source. Summed over volumes, exactly 1 `TSK_ENCRYPTION_SUSPECTED`: a volume with no recognisable filesystem and high entropy. `findFiles("veracryptContainerFile")` returns 1 file with exactly 1 `TSK_ENCRYPTION_SUSPECTED` (container larger than 5 MiB). |
| testSqlCipherEncryption | `SqlCipherDetection_img1_v1.vhd`, FILES_ONLY | `findFiles("%%","sqlcipher")` returns **15**. Each non-slack file whose name starts with `sqlcipher-` and whose last dot-segment is `db` has exactly 1 `TSK_ENCRYPTION_SUSPECTED`; others have 0. |

Contents of `PasswordDetection_img1_v1.img`, read from the ISO9660 directory records (sizes in bytes):

```
/accdb/Apache/License.txt (533)  testAccess2.accdb (794624)  testAccess2-protected.accdb (557056)
/mdb/Apache/License.txt (533)  testAccess2_2000.mdb (421888)  testAccess2_2002-2003.mdb (417792)  testAccess_V1997.mdb (118784)
/mdb/Recovery/Readme.txt (1214)  Recovery-protected.mdb (188416)
/mdb/Viewer/Readme.txt (928)  Viewer-protected.mdb (139264)
/ole2/Bits.xls (35328)  Bits-protected.xls (35328)  Hello.ppt (85504)  Hello-protected.ppt (40960)  Testing.doc (22528)  Testing-protected.doc (22528)
/ooxml/Bits.xlsx (13328)  Bits-protected.xlsx (19968)  Hello.pptx (30214)  Hello-protected.pptx (36864)  Testing.docx (11470)  Testing-protected.docx (17920)
/pdf/Testing.pdf (148440)  Testing-protected.pdf (149055)
```

- **Expected `TSK_ENCRYPTION_DETECTED` (10):** Bits-protected.xls, Hello-protected.ppt, Testing-protected.doc, Bits-protected.xlsx, Hello-protected.pptx, Testing-protected.docx, Testing-protected.pdf, Recovery-protected.mdb, Viewer-protected.mdb, testAccess2-protected.accdb.
- Protected OOXML files are OLE `EncryptedPackage` containers (Agile: AES-256-CBC, SHA-512, spinCount 100000).
- The entry counts include `.`, `..` and one `-slack` file per regular file whose size is not a multiple of 2048:
  - ole2: 6 + 3 + 2 = 11
  - ooxml: 6 + 5 + 2 = 13
  - pdf: 2 + 2 + 2 = 6
  - mdb and accdb: this model gives 22 and 9 against Autopsy's 25 and 10. That is one extra entry per subdirectory, probably a TSK ISO9660 quirk. **Do not hard-code 25/10.** Assert the per-file artefacts, and document our own directory-entry model.

**Port plan.** Port the per-file decision table as unit tests on generated files:
- OLE with `EncryptionInfo`/`EncryptedPackage` streams
- legacy `.doc`/`.xls`/`.ppt` with the encryption flag
- PDF with an `/Encrypt` dictionary
- `.mdb`/`.accdb` with the password-flag header bytes
- a SQLCipher database (random first 16 bytes, unlike the `SQLite format 3\0` header)
- a BitLocker volume header (`-FVE-FS-` OEM ID at offset 3)
- a high-entropy unformatted volume or container at least 5 MiB, size a multiple of 512, entropy at least 7.5

The exact comment text "Bitlocker encryption detected." is part of parity.

---

## 6. Hash sets and keyword-list inputs (`test/input`)

These are the only hash and keyword fixtures. The regression harness uses them; no unit test does. Area: hash lookup (`pm-hashdb`), keyword lists (`pm-keyword`). See `03-ingest-modules.md`.

`test/input/NSRL.txt-md5.idx` is in the TSK sorted-index format: two header records keyed by 41 zeros and by `…01`, then sorted `MD5|offset-into-original-text-db` lines. LF endings.

```
00000000000000000000000000000000000000000|md5sum
00000000000000000000000000000000000000001|NSRLcreator.txt
0D4A1C9ED5A49CAF22FD5F52C666DE2C|0000000000000045
35BA15EC1C3CF03531282147DB10A089|0000000000000100
91C66396EEC4BCEEAF6EDE7A48F60C63|0000000000000152
A99F69068F958CF412E7F8B8A0142B41|0000000000000000
```

`test/input/notablehashes.txt-md5.idx`:

```
00000000000000000000000000000000000000000|md5sum
00000000000000000000000000000000000000001|notablehashescreator.txt
48199F51973F317459E80E18DC744B12|0000000000000000
5CCB10AEA1EC335139715D4AA44D0EE0|0000000000000062
94610B03A4295300AA29C3364DB18683|0000000000000108
A06B65C36E0E5A8229749375C3AAC4B1|0000000000000160
```

Parser tests to write. Autopsy has none.
- Parse both files: header name `md5sum`, source name `NSRLcreator.txt` / `notablehashescreator.txt`, and 4 entries each.
- Lookups are case-insensitive: a lower-case MD5 must hit.
- A miss returns no entry.
- Entries are sorted, so binary search works.
- Index-only databases (no source text DB) are allowed.
- In the harness, **both sets are imported as Notable**, including the NSRL one, because the import dialog defaults to "Notable".

`test/input/notablekeywords.xml` is UTF-8 **with a BOM**, and the DIRT line has trailing spaces:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<keyword_lists>
 <keyword_list created="2012-03-23 11:06:17" modified="2012-03-23 11:08:48" name="notable_keywords" use_for_ingest="true">
  <keyword literal="true">Jean</keyword>
  <keyword literal="true">Personalized</keyword>
  <keyword literal="true">DIRT</keyword>  
  <keyword literal="true">Osama</keyword>
  <keyword literal="true">bomb</keyword>
  <keyword literal="true">hacking</keyword>
  <keyword literal="true">molotov</keyword>
  <keyword literal="true">nuclear</keyword>
  <keyword literal="true">صحافة و إعلام</keyword>
  <keyword literal="true">مطلوبا</keyword>
 </keyword_list>
</keyword_lists>
```

Import test: BOM tolerated; 1 list named `notable_keywords`, `use_for_ingest=true`, created and modified timestamps parsed; **10** literal keywords in order, including the two Arabic terms (one containing spaces) byte-exact. Round-trip export, then import again, gives an equal list.

---

## 7. Central repository / correlation engine

Sources: `Core/test/qa-functional/src/org/sleuthkit/autopsy/centralrepository/datamodel/`, which holds `CorrelationAttributeNormalizerTest`, `CentralRepoDatamodelTest`, `CentralRepoAccountsTest` and `CentralRepoPersonasTest`. Production code is in `Core/src/org/sleuthkit/autopsy/centralrepository/datamodel/`. Area: central repository (`pm-cr`). See `06-multiuser-infra.md` and `04-analysis-reports.md`.

### 7.0 Common setup and semantics
- No external services and no data files.
- A **SQLite** CR at `${tmp}/CentralRepoDatamodelTest/testcentralrepo.db`, freshly created per test method:
  - `initializeDatabaseSchema()` + `insertDefaultDatabaseContent()`
  - pragmas: WAL, synchronous OFF, read_uncommitted, UTF-8, page_size 4096, **foreign_keys ON**. Several expected errors are FK violations.
- Schema: `organizations`, `cases`, `data_sources`, `reference_sets`, `correlation_types`, `db_info`, `account_types`, `accounts`, `<type>_instances` per default type, `reference_file`, and the persona tables (`confidence`, `examiners`, `persona_status`, `personas`, `persona_alias`, `persona_metadata`, `persona_accounts`).
- `db_info`: `SCHEMA_VERSION`=1, `SCHEMA_MINOR_VERSION`=6, `CREATION_SCHEMA_MAJOR_VERSION`=1, `CREATION_SCHEMA_MINOR_VERSION`=6. Not asserted.
- Default content:
  - org **"Not Specified"**
  - `confidence` LOW=1, MODERATE=2, HIGH=3
  - `persona_status` UNKNOWN=1, ACTIVE=2, MERGED=3, SPLIT=4, DELETED=5
  - `account_types` for every predefined TSK account type **except DEVICE**
- **Default correlation types:**
  - Ids 0–11:

    | Id | Display name | Table base |
    |---|---|---|
    | 0 | File MD5 | `file` |
    | 1 | Domain | `domain` |
    | 2 | Email Address | `email_address` |
    | 3 | Phone Number | `phone_number` |
    | 4 | USB Device | `usb_devices` |
    | 5 | Wireless Network | `wireless_networks` |
    | 6 | MAC Address | `mac_address` |
    | 7 | IMEI Number | `imei_number` |
    | 8 | IMSI Number | `imsi_number` |
    | 9 | ICCID Number | `iccid_number` |
    | 10 | Installed Program | `installed_programs` |
    | 11 | Os Account | `os_accounts` |

  - Ids 1000+: one per predefined account type except DEVICE, EMAIL and PHONE. The table is `lower(typeName)+"_acct"`.
  - The test expects **29** default types in total. This assumes 17 account types (CREDIT_CARD … SHAREIT), from TSK `Account.Type.PREDEFINED_ACCOUNT_TYPES`; **verify the list against sleuthkit 4.15 `Account.java`**.
  - Phone, email and all account types carry an `account_id` column.
- **Duplicate handling:**
  - Silently ignored (SQLite `ON CONFLICT IGNORE`): `cases(case_uid)`, instances `(data_source_id, value, file_path)` and reference `(reference_set_id, value)`.
  - Error on duplicate: org name, reference set (name, version), correlation type (display_name, db_table_name), data source (case_id, datasource_obj_id), and db_info name.
- Instance file paths are **lower-cased** at construction.
- Constructor check order: a null path is checked **before** the value is normalised.
- **Error-type parity matters.** A null type or value gives a "CR exception". A badly formatted value gives a distinct **normalisation exception**. Model these as two error variants.
- Constants:
  - `DEFAULT_BULK_THRESHHOLD = 1000`; the bulk queue auto-flushes when it reaches 1000.
  - `MAX_VALUE_LENGTH = 256`: values of 256 characters or more are rejected on a single add and skipped with a warning in bulk.
  - `randomHash()` in the test never emits `9`, an off-by-one in `ints(32, 0, len-1)`. This is irrelevant to semantics.

### 7.1 CorrelationAttributeNormalizerTest
A pure function, `normalize(typeId, value)`. Unknown type id → CR error. Null value → CR error (`"Correlation value was null."`). Otherwise the value is **trimmed** and then dispatched. "Throws" means either error kind, which the test accepts.

**MD5 (0):** lower-case, then it must match `^[a-f0-9]{32}$`.

| Input | Expected |
|---|---|
| `e34a8899ef6468b74f8a1048419ccc8b` | same |
| `E34A8899EF6468B74F8A1048419CCC8B` | `e34a8899ef6468b74f8a1048419ccc8b` |
| `e34asdfa8899ef6468b74f8a1048419ccc8b` | throws |
| `""` | throws (normalisation error) |
| null | throws (CR error) |

**Domain (1):**
1. If `DomainValidator(allowLocal=true).isValid(v)`, return `extractDomain(v.toLowerCase())`.
   - `extractDomain` takes the URL host, or strips `.*://` and everything after `/`.
   - It then takes the registrable domain via the public-suffix list.
   - It returns `""` if the result contains any of ``~`!@#$%^&*()+={}[];:?<>,/`` or a space, or has no `.`.
2. Otherwise, if `v` is a dotted-quad IPv4 address, return `extractDomain(v)`.
3. Otherwise throw.

| Input | Expected |
|---|---|
| `www.test.com` | `test.com` |
| `http://www.test.com` | throws |
| `test.com` | `test.com` |
| `http://1270.0.1` | throws |
| `?>\/)(*&.com` | throws |
| null | throws |
| `""` | throws |
| `HTTP://tests.com` | throws |
| `http://www.test.com/aPage?aQuestion=aParam&anotherQuestion=anotherParam` | throws |
| `WWW.TEST.COM` | `test.com` |
| `TEST.COM` | `test.com` |

Note that **URLs are rejected**: only bare host names are accepted. Port the public-suffix behaviour with a bundled PSL, the same as Autopsy's `DomainTokenizer` resource.

**Email (2):** non-empty and `EmailValidator(allowLocal=true, allowTld=true)`; the result is lower-cased and trimmed.

| Input | Expected |
|---|---|
| `bsweeney@cipehrtechsolutions.com` | same |
| `BSWEENEY@ciphertechsolutions.com` | `bsweeney@ciphertechsolutions.com` |
| `""` | throws |
| null | throws |
| `asdf` | throws |
| `asdf@asdf` | `asdf@asdf` (single-label domain allowed) |
| `asdf.asdf` | throws |

**Phone (3):**
- Every character must be one of: `+` at index 0 only, a space character, a digit, or one of `-`, `(`, `)`.
- Length must be at least 5.
- Strip `\s+`, then `[-()]`; the result must still be at least 5 characters. The leading `+` is kept.

| Input | Expected |
|---|---|
| `19784740486` | `19784740486` |
| `1(978) 474-0486` | `19784740486` |
| `+19784740486` | `+19784740486` |
| `1 978-474-0486` | `19784740486` |
| `9879879819784740486` | not asserted (`fail()` commented out); production returns it unchanged (no maximum length) |
| `+1(978) 474-0486` | `+19784740486` |
| `asdfasdfasdf` | throws |
| `asdf19784740486adsf` | throws |

**USB ID (4):** the trimmed value is returned unchanged and **not lower-cased**: `0202:AAFF` → `0202:AAFF`. The commented-out ideas in the test (`0202:axxf` fails, `0202 AAFF` passes, and so on) are not active; production only rejects null.

**Not tested by Autopsy.** Port these anyway, from the production rules:

| Type | Rule |
|---|---|
| SSID (5) | Unchanged (case-sensitive) if its length is at most 32, otherwise throws. |
| MAC (6) | Lower-case, remove `[\s-:]`, then must match `^([a-f0-9]{12}\|[a-f0-9]{16})$`. |
| IMEI (7) | Remove `[\s-:]`, then must match `^[0-9]{14,16}$`. |
| IMSI (8) | Remove `[\s-:]`, then must match `^[0-9]{14,15}$`. |
| ICCID (9) | Lower-case, remove `[\s-:]`, then must match `^89[f0-9]{17,22}$`. |
| Installed program (10), OS account (11), account types (1000+) | Trimmed only. |

Suggested vectors:
- MAC: `00:1A:2b-3C 4d5E` → `001a2b3c4d5e`; 14 hex characters → throws.
- IMEI: 14, 15 and 16 digits pass; 13 throws.
- IMSI: 14 and 15 digits pass; 16 throws.
- ICCID: `8901 2602 1234 5678 901` → `8901260212345678901`; `9901…` throws.
- SSID: 32 characters pass; 33 throw.

### 7.2 CentralRepoDatamodelTest

Fixtures created in setUp:
- **Cases:** case1 `("case1_uuid","case1")`, case2 `("case2_uuid","case2")`.
- **Data sources:**

  | Data source | Case | Device id | Name | objId |
  |---|---|---|---|---|
  | ds1c1 | case1 | `dataSource1_deviceID` | `dataSource1` | 11 |
  | ds2c1 | case1 | `dataSource2_deviceID` | `dataSource2` | 12 |
  | ds1c2 | case2 | `dataSource3_deviceID` | `dataSource3` | 21 |

- **Orgs:** org1, org2.
- **Types:** fileType = type 0, usbDeviceType = type 4.
- **Constants:** `BAD_PATH="badPath"`, `FILE_PATH="/"`.
- An in-memory `CorrelationCase` has id -1.

**testNotableArtifactStatus.**
- H1 `e34a8899ef6468b74f8a1048419ccc8b` is BAD in case1/ds1c1 "path1" and in case2/ds1c2 "path2". Lookup returns 2, both BAD.
- H2 `d293f2f5cebcb427cde3bb95db5e1797` is BAD in case1/ds1c1 "path3" and KNOWN in case2/ds1c2 "path4". Lookup returns 2, with the per-case status preserved.
- H3 `23bd4ea37ec6304e75ac723527472a0f` is added KNOWN for case1 with **ds1c2** (a case/data-source mismatch is allowed), then `setAttributeInstanceKnownStatus(BAD)`. Lookup returns 1, BAD.
- `setAttributeInstanceKnownStatus` on an instance that is **not in the DB** must not fail: it upserts, inserting the data source if needed.
- Error cases, each a CR error: a null attribute; a null status; a null case; a null data source.
- `getCountArtifactInstancesKnownBad(file, H1)` = **2**; for H2 = **1**. A null type → CR error. A null value → CR error.
- `getListCasesHavingArtifactInstancesKnownBad(file, H1)` has size 2. For H2 it is `["case1"]`, the case display names. A null type → CR error.

**testBulkArtifacts.**
1. `getCountArtifactInstancesByCaseDataSource(ds1c1)` = 0. This is the sum over all instance tables.
2. Queue 500 file instances (random hash, path `C:\bulkInsertPath1\file<i>`): count still 0.
3. Queue 1000 more (`C:\bulkInsertPath2\file<i>`): auto-flush happens at 1000 queued, so the count is **1000**.
4. Commit: count **1500**. A comment of `""` is stored as NULL. An empty commit is a no-op.
5. Error cases:
   - Null type, null path: CR error in the constructor.
   - Null case, null data source, null status: CR error **at commit**. The bad item stays queued.

**testArtifacts.** Values:

| Purpose | Value | Path |
|---|---|---|
| unused | `00000000000000000000000000000000` | |
| inAll | `6cddb0e31787b79cfdcc0676b98a71ce` | `C:\files\path0.txt` (case1/ds1c1, case1/ds2c1, case2/ds1c2) |
| twice in ds1c1 | `b2f5ff47436671b6e533d8dc3614845d` | path1, path2 |
| only in ds1c2 | `2af54305f183778d87de0c70c591fae4` | path3 |
| callback | `fb9dd8f04dacd3e82f4917f1a002223c` | `C:\files\processinstancecallback\path{1,2}.txt` |
| email | `test@gmail.com` | |
| phone | `202-555-1234` (stored `2025551234`) | |
| domain | `www.mozilla.com` (stored `mozilla.com`) | |
| USB | `94B21234` (stored unchanged) | |

- Adds succeed. Constructing an instance with a null case or data source succeeds; validation happens on add.
- Add errors:
  - value `"badInstances"` (an invalid MD5) → **normalisation** error, not a CR error
  - an unsaved case (id -1) → CR error (FK)
  - a null data source → CR error
  - an unsaved data source → CR error (FK)
  - a null path → CR error (checked before the value)
  - a null type → CR error
  - a null value → CR error
- Queries:
  - `getArtifactInstancesByTypeValue(file, inAll)` has size 3; paths equalIgnoreCase `C:\files\path0.txt` (stored lower-case).
  - `(email, inAll)` → normalisation error. A null type or value → CR error.
  - `getCountArtifactInstancesByTypeValue`: inAll = 3; unused = 0; nulls → CR error.
- **`getFrequencyPercentage`** = `int(distinct (case, ds) with value / count(data_sources) × 100)`: inAll **100**, twice **33**, email **33**, unused **0**. A null attribute → CR error.
- Comment update:
  - `getCorrelationAttributeInstance(usb, case1, ds1c1, "94B21234", "C:\files\devIdPath.txt")` returns the instance.
  - `setComment("new comment")` + `updateAttributeInstanceComment`; a re-fetch shows `"new comment"`.
  - The lookup uses the normalised value and the lower-cased path.
- `getCountArtifactInstancesByCaseDataSource(ds1c1)` = **7**.
- `getCountUniqueCaseDataSourceTuplesHavingTypeValue`: inAll **3**, twice **1**, unused **0**; nulls → CR error.
- `processInstanceTable(file, cb)` iterates every row of `file_instances`. With the two callback instances added, rows whose lower-cased path contains `processinstancecallback` number **2**, and other rows are more than 0. A null type or callback → CR error.
- Re-adding the two callback instances is a silent duplicate. `processInstanceTableWhere(file, "value='fb9dd8f0…'", cb)` gives 2 and 0. Null arguments → CR error.

**testCorrelationTypes.**
- `newCorrelationType(Type("customType","custom_type",supported=false,enabled=false))` returns an id. No instance table is created.
- A duplicate → CR error. A null display name → CR error.
- `Type("temp", null, …)` → constructor error. The table name must match `[a-z][a-z0-9_]*`.
- `newCorrelationType(null)` → CR error.
- Counts: defined **30**, enabled **29**, supported **29**.
- By id: the custom type round-trips. Id 5555 → CR error.
- Updating the type to (`newName`, `new_db_table`, enabled, supported) round-trips.
- Updating a type with nonexistent id 12345 is a **silent no-op**. Updating with a null name → CR error.

**testOrganizations.**
- `orgA`, and `orgB` with poc `pocName`/`pocEmail`/`pocPhone`: both get ids other than -1.
- A duplicate `orgA`, a null org, an org that already has an id, or a null name → CR error.
- `getOrganizations()` = **5**: Not Specified, org1, org2, orgA, orgB.
- By id: orgB's fields round-trip. Id 12345 → CR error.
- Updating to `newOrgName`/`newPocName`/`newPocEmail`/`newPocPhone` round-trips. Updating an unsaved org, a null org or a null name → CR error.
- Delete: the count drops by 1.
- Deleting an org used by a reference set (`inUseOrg` / set `inUseOrgTest` v`1.0`) → CR error. Deleting an unsaved or null org → CR error.

**testReferenceSetInstances.**
- Hashes: notable1 `d46feecd663c41648dbf690d9343cf4b`, known1 `39c844daee70485143da4ff926601b5b`, inAllSets `6449b39bb23c42879fa0c243726e27f7`.
- Sets: "notable set 1" v1.0 BAD, "notable set 2" v2.4 BAD, "known set 1" v5.5.4 KNOWN, all org1.
- Instances: inAll in all three sets (comments comment1–3), notable1 in set 1 ("comment4"), known1 in the known set ("comment5").
- Add errors, each a CR error:
  - nonexistent set 2345 (FK)
  - null hash (constructor)
  - null status (constructor)
  - null type
- Bulk insert of 1500 random hashes into notable set 2 runs in one transaction; a spot check finds the first hash. Bulk insert with a null set, set 2345 (rolled back) or a null type → CR error.
- Lookups:

  | Call | Expected |
  |---|---|
  | `getReferenceInstancesByTypeValue(file, inAll)` | **3** |
  | `getReferenceInstancesByTypeValue(file, randomHash)` | 0 |
  | `getReferenceInstancesByTypeValue(email, inAll)` | error |
  | `getReferenceInstancesByTypeValue` with null arguments | CR error |
  | `isFileHashInReferenceSet(known1, known)` | true |
  | `isFileHashInReferenceSet(known1, notable1)` | false |
  | `isFileHashInReferenceSet(known1, 5678)` | false, no error |
  | `isFileHashInReferenceSet(null, …)` | CR error |
  | `isValueInReferenceSet(known1, known, type 0)` | true |
  | `isValueInReferenceSet(known1, notable1, 0)` | false |
  | `isValueInReferenceSet(known1, 5678, 0)` | false |
  | `isValueInReferenceSet(null, …)` | CR error |
  | `isValueInReferenceSet(known1, known, type 2)` | **normalisation** error |
  | `isArtifactKnownBadByReference(file, notable1)` | true |
  | `isArtifactKnownBadByReference(file, known1)` | false |
  | `isArtifactKnownBadByReference(file, random)` | false |
  | `isArtifactKnownBadByReference` with a null value or type | CR error |

**testReferenceSets.**
- `referenceSet1` v1.0 BAD (org1). `referenceSet2` with version **""** (allowed) KNOWN (org2).
- A duplicate name and version → CR error. The same name with v2.0 is OK.
- Org 5000 (FK), a null name, a null version, a null status or a null type → CR error.
- Validity and existence:
  - `referenceSetIsValid(id, "referenceSet1", "1.0")` = true.
  - `referenceSetIsValid` with a bad id, a null name or a null version = false.
  - `referenceSetExists("referenceSet1", "1.0")` = true; with `"5.5"` = false; with a null name or version = false.
- Lookups:
  - `getReferenceSetByID(id)` round-trips the name and version; id 1234 → **None**, not an error.
  - `getAllReferenceSets(file)` = **3**; `(email)` = 0; `(null)` → CR error.
- Delete removes the `reference_file` rows, then the set; the count drops by 1. Deleting id 1234 is a no-op.
- `getReferenceSetOrganization(set1)` = org1; for id 4567 → CR error.

**testDataSources.**
- A = (case2, `dataSourceA_deviceID`, `dataSourceA`, obj 0) is OK. Adding the identical A again **returns the existing row**, no error. B = (case1, the same device and name, obj 0) is OK.
- An unsaved case → CR error ("Case ID is -1"). A null device id or a null name → CR error.
- `getDataSource(case2, 0)` → found. `(case2, 9999)` → None. `(null, 0)` → CR error. `(case2, -1)` → None.
- `getDataSources()` = **5**; `getCountUniqueDataSources()` = **5**.

**testCases.** This needs an open postmortem case (Autopsy creates one in `CentralRepoDatamodelTestCase`).
- caseA `("caseA_uuid","caseA")`. A null uuid or a null name → CR error.
- The same uuid with a different name returns the **existing** case; the count is unchanged.
- `newCase(currentCase)` creates caseB:
  - uuid = the case *name*
  - org = the default org, which is null
  - details copied from the case
- `newCase(null)` → CR error.
- Update caseA with number `12-34-56`, creation date `01/12/2018`, display `Test Case`, examiner `John Doe` / `john@sample.com` / `123-555-4567`, notes `Notes` and org org1. Everything round-trips. Empty strings are stored as NULL.
- `updateCase(null)` → CR error.
- Lookups: `getCase(current)` and `getCaseByUUID("caseA_uuid")` → found; `"badUUID"` → None. `getCases()` contains the uuids of case1, case2, caseA and caseB.
- `bulkInsertCases`: 1500 cases (uuid `bulkTestName_<i>`, name `bulkTestUUID_<i>`; the prefixes are swapped in the test) raise the count by exactly 1500, executed in batches of 1000. `null` → CR error. An empty list is a no-op.

**testDbInfo.**
- `newDbInfo("testName1","testValue1")` → OK. A null name or a null value → CR error.
- `getDbInfo("testName1")` = `"testValue1"`; `"testName3"` → None; `null` → None.
- `updateDbInfo("testName1","testValue2")` round-trips. A null value → CR error. `updateDbInfo(null, "x")` → **no error** (0 rows updated).

### 7.3 CentralRepoAccountsTest
Setup: the SQLite CR only. Normalisation (`normalizeAccountIdentifier(type, id)`): a blank id is an error; PHONE uses phone normalisation; EMAIL is lower-cased; **every other type is just `toLowerCase()`**. `getOrCreateAccount` is idempotent (`INSERT OR IGNORE`, then select).

| Test | Expected |
|---|---|
| testPredefinedAccountTypes | Every predefined TSK account type except DEVICE is found by type name. Its type name and display name equal the TSK definitions. |
| testRejectionOfDeviceAccountType | `getAccountTypeByName("DEVICE")` → None |
| testNonExistentAccountType | `"NotARealAccountType"` → None, no error |
| testCreatingAccount | FACEBOOK `"+1 401-231-2552"` → OK |
| testRetreivingAnAccount | FACEBOOK `"+1 441-231-2552"` twice. The second result has the FB type and identifier exactly `"+1 441-231-2552"`. FACEBOOK is not phone-normalised. |

### 7.4 CentralRepoPersonasTest

**Setup.**
- Cases case1–case4 (`caseN_uuid` / `caseN`).
- Data sources:

  | Data source | Case | Device id | objId |
  |---|---|---|---|
  | ds1c1 | case1 | `dataSource1_deviceID` | 11 |
  | ds2c1 | case1 | `dataSource2_deviceID` | 12 |
  | ds1c2 | case2 | `dataSource3_deviceID` | 21 |
  | ds1c3 | case3 | `dataSource4_deviceID` | 31 |
  | ds2c3 | case3 | `dataSource5_deviceID` | 32 |
  | ds1c4 | case4 | `dataSource6_deviceID` | 41 |

- Orgs org1 and org2.
- Account types PHONE, EMAIL, FACEBOOK, TEXTNOW, WHATSAPP and SKYPE, each resolved to its correlation type: phone = 3, email = 2, others 1000+.
- Constants:
  - `PHONE_NUM_1 = "+1 441-231-2552"` (stored `+14412312552`)
  - `FACEBOOK_ID_CATDOG = "BalooSherkhan"` (stored `baloosherkhan`)
  - `DOG_EMAIL_ID = "superpupper@junglebook.com"`
  - `CAT_WHATSAPP_ID = "1112223333@s.whatsapp.net"`
  - `EMAIL_ID_1 = "rkipling@jungle.book"`
  - `HOLMES_SKYPE_ID = "live:holmes@221baker.com"`

**Semantics.**
- `createPersonaForAccount(name, comment, status, account, justification, confidence)`:
  - random UUID
  - a blank name → **"Unnamed"**; a blank comment or justification → `""`
  - created = modified = now (epoch ms)
  - the examiner is the OS user, via `getOrInsertExaminer`
  - it then adds the account link
- Removing an alias, metadata or account is a hard delete. Metadata is UNIQUE(persona, name).
- **Every listing and search excludes DELETED personas (status 5).**
- `getPersonaByName` is a case-insensitive `LIKE %x%` with `!`, `%` and `_` escaped (`ESCAPE '!'`).
- `getPersonaAccountsForIdentifierLike` is a case-insensitive `LIKE %x%`, without escaping.
- `getCases()` and `getDataSources()` follow the persona's accounts to their correlation instances. Data sources are de-duplicated by **datasource_obj_id**, a quirk.

| Test | Steps → expected |
|---|---|
| testBasicPersonaCreation | dog = ("Baloo McDog", "The best dog ever", ACTIVE, phone acct, "Because I said so", LOW). Name and status match; examiner = OS user; created within the last 10 min. Aliases "Good Boy" (MODERATE) and "WoofWoof" (LOW) → 2. Metadata Color=Black (MODERATE) and Gender=Male (LOW) → 2. Add FB acct to dog ("Looks like dog, barks like a dog...", MODERATE) → 2 persona accounts, each with a justification, an identifier and positive dates. cat = ("SherKhan", "The fiercest cat alive.", ACTIVE, FB acct, "Smells like a cat.", LOW). `getPersonaAccountsForAccount(fb)` → **2**. `cat.removeAccount(first)` → cat has 0 accounts. |
| testPersonaAliasesAndMetadata | dog as above: 2 aliases, 2 metadata. Cat alias "CutieKitty"; metadata Color=White (MODERATE), Breed=Persian (LOW), Legs=Four (HIGH) → 1 alias, 3 metadata. Remove the alias → 0. Remove Breed → 2, and Breed is no longer present. holmes = ("Sherlock Holmes", "Has a Pipe in his mouth.", ACTIVE, Skype acct, "The name says it all.", LOW) → 0 aliases, 0 metadata. |
| testPersonaWithCases | Instances: FB `BalooSherkhan` @ case1/ds1 path1 obj 1001, case1/ds2 path2 1002, case2/ds3 path3 1003; dog email @ case1/ds2 path3 1002; cat WhatsApp @ case1/ds1 path4 1005 and case2/ds3 path5 1006; Holmes Skype @ case3/ds4 path8 1011. dog and cat on FB; dog adds the email account ("Thats definitely a dog email account", MODERATE); cat adds WhatsApp ("The cat has a WhatsApp account", MODERATE); holmes on Skype. Expected: dog accounts 2 (FB, email); cat 2 (FB, WhatsApp). Cases: dog 2, cat 2, holmes 1 (case3). Data sources: dog 3, cat 3, holmes 1 (`dataSource4_deviceID`). `getPersonasForCase`: case1 2, case2 2, case3 1, case4 0. `getPersonasForDataSource`: ds1c1 2, ds2c1 2, ds1c2 2, ds1c3 1, ds2c3 0, ds1c4 0. |
| testPersonaCreationEdgeCases | A null name on `rkipling@jungle.book` → name **"Unnamed"**. |
| testPersonaSearchByName | Four personas on `rkipling@jungle.book` (comment "Creator of Jungle Book."): "Rudyard Kipling", "Rudy", "Kipling Senior", "Senor Kipling". Searches: `kipling` **3** (comments match), `Rudy` **2**, `Sen` **2**, `IPL` **3**, `Rudyard Kipling` **1**, `""` **4**. |
| testPersonaSearchByAccountIdentifier | Email `joeexotic555@yahoo.com` → "Joe Exotic", "Tiger King". Phone `+1 999 555 3366` (stored `+19995553366`) → "Carol Baskin" and a null-named persona (becomes "Unnamed"). Email `jodoe@mail.com` → "John Doe", "Joanne Doe". `getPersonaAccountsForIdentifierLike`: `joe` 2, `exotic` 2, `999` 2, `555` **4**, `doe` 2, `@` 4, `""` 6. Each result's identifier contains the query. |
| testAccountSearchByAccountIdentifier | Accounts: EMAIL `joeexotic555@yahoo.com`, PHONE `+1 999 555 3366`, EMAIL `jodoe@mail.com`, WHATSAPP `1112223333@s.whatsapp.net`. `getAllAccounts` 4. `getAccountsWithIdentifierLike("555")` 2. `("JO")` 2 (case-insensitive). `getAccountsWithIdentifier("joeexotic555@yahoo.com")` 1; `("1112223333@s.whatsapp.net")` 1; `("IdDoesntExist")` 0. The type-less normaliser tries phone, then email, then lower-case and trim. |
| testExaminers | `getOrInsertExaminer`: `"abcdefg"` → same; `""` → `""` (allowed); `"D'Aboville"` → same. Quotes must be handled through parameterised SQL. |

---

## 8. Common properties search

Sources: `Core/test/qa-functional/src/org/sleuthkit/autopsy/commonpropertiessearch/*.java`. Area: common properties search (`pm-search-common`), on top of the case DB and the CR. See `04-analysis-reports.md`.

### 8.1 Searcher semantics (production)

All-sources SQL:

```sql
SELECT obj_id, md5, data_source_obj_id FROM tsk_files
WHERE md5 IN (SELECT md5 FROM tsk_files
              WHERE (known != KNOWN OR known IS NULL) [AND mime_type IN (…)]
              GROUP BY md5 HAVING COUNT(DISTINCT data_source_obj_id) > 1)
ORDER BY md5
```

- The single-source variant also requires the md5 to occur in the selected data source.
- A null md5 or the empty-file MD5 (`d41d8cd98f00b204e9800998ecf8427e`) is skipped.
- A duplicate *within* one data source needs a second data source to match.
- **Media MIME filter set:** image/bmp, image/gif, image/jpeg, image/png, image/tiff, image/vnd.adobe.photoshop, image/x-raw-nikon, image/x-ms-bmp, image/x-icon, video/webm, video/3gpp, video/3gpp2, video/ogg, video/mpeg, video/mp4, video/quicktime, video/x-msvideo, video/x-flv, video/x-m4v, video/x-ms-wmv, application/vnd.ms-asf, application/vnd.rn-realmedia, application/x-shockwave-flash.
- **Document MIME filter set:** application/rtf, application/pdf, text/css, text/html, text/csv, application/json, application/javascript, application/xml, text/calendar, application/x-msoffice, application/x-ooxml, application/msword, application/vnd.openxmlformats-officedocument.wordprocessingml.document, application/vnd.ms-powerpoint, application/vnd.openxmlformats-officedocument.presentationml.presentation, application/vnd.ms-excel, application/vnd.openxmlformats-officedocument.spreadsheetml.sheet, application/vnd.oasis.opendocument.presentation, application/vnd.oasis.opendocument.spreadsheet, application/vnd.oasis.opendocument.text.
- **Frequency filter**, applied only when the CR is enabled and the threshold is not 0:
  - pct = `int(trunc(uniqueCaseDataSourceTuplesHavingValue / countUniqueDataSources × 100))`
  - a value is removed when **pct > threshold**

### 8.2 Intra-case fixtures (`IntraCaseTestUtils`)

| Data source (image name) | Files |
|---|---|
| SET1 `CommonFiles_img1_v1.vhd` | `IMG_6175.jpg` ×2 (identical), `BasicStyleGuide.doc` |
| SET2 `CommonFiles_img2_v1.vhd` | `adsf.pdf`, `IMG_6175.jpg` |
| SET3 `CommonFiles_img3_v1.vhd` | `BasicStyleGuide.doc`, `IMG_6175.jpg` |
| SET4 `CommonFiles_img4_v1.vhd` | `file.dat` (empty) |

The sets are added in order SET1–4. The failure message format is `"Expected to find {0} matches for {1} in {2} but found {3}."`.

### 8.3 Intra-case tests (counts per data source S1/S2/S3/S4; threshold 0)

**IngestedWithHashAndFileTypeIntraCaseTest** (HashLookup + FileTypeId):

| Test | Searcher (media, doc) | IMG | DOC | PDF | EMPTY |
|---|---|---|---|---|---|
| testOneA | All (F,F) | 2/1/1/0 | 1/0/1/0 | 0 | 0 |
| testOneB | All (T,F) | 2/1/1/0 | 0 | 0 | 0 |
| testOneC | All (F,T) | 0 | 1/0/1/0 | 0 | 0 |
| testTwoA | Single SET1 (F,F) | 2/1/1/0 | 1/0/1/0 | 0 | 0 |
| testTwoB | Single SET1 (T,F) | 2/1/1/0 | 0 | 0 | 0 |
| testTwoC | Single SET1 (F,T) | 0 | 1/0/1/0 | 0 | 0 |
| testThree | Single SET2 (F,F) | 2/1/1/0 | 0 | 0 | 0 |
| testFour | Single SET4 (F,F) | 0 | 0 | 0 | 0 |
| testFive | Single SET3 (F,F) | 2/1/1/0 | 1/0/1/0 | 0 | 0 |

**(bug)** testFour checks DOC/SET1 twice and never DOC/SET2. Port the intended check.

Other intra-case tests:
- **IngestedWithNoFileTypesIntraCaseTest** (HashLookup only, so there are no MIME types): All with media=true → empty. Single SET3 with media=true → empty, and no error. **A MIME filter with no MIME data yields nothing.**
- **MatchesInAtLeastTwoSourcesIntraCaseTest** (only SET1 and SET4): All (F,F) gives IMG, DOC and EMPTY 0 in both. **Two copies inside one data source do not match.** (bug) The test passes the data-source-id map where a file-id map is expected; it only passes because the result is empty.
- **UningestedCasesIntraCaseTest** (no ingest, so no MD5s): All and Single SET1 give 0 results.

### 8.4 Inter-case tests (CR-backed; **bodies commented out** in Autopsy, but intended behaviour is recorded)

`InterCaseTestUtils`:
- SQLite CR in `${tmp}/InterCaseCommonFilesSearchTest/testcentralrepo.db`.
- Cases: Case1 = `c1ds1_v1.vhd` and `c1ds2_v1.vhd`; Case2 = `c2ds1`, `c2ds2`; Case3 = `c3ds1`, `c3ds2`.
- Contents (files with the same name are identical):
  - C1 DS1 and C1 DS2: `Hash-0.dat` (0 B), `Hash-A.jpg`, `Hash-A.pdf`
  - C2 DS1: `Hash-B.jpg`, `Hash-B.pdf`
  - C2 DS2: `Hash-A.jpg`, `Hash-A.pdf`, `Hash-D.doc`
  - C3 DS1: `Hash-A.jpg`, `Hash-A.pdf`, `Hash-C.jpg`, `Hash-C.pdf`, `Hash-D.jpg`
  - C3 DS2: `Hash-C.jpg`, `Hash-C.pdf`, `Hash-D.doc`
- Frequencies (out of 6 data sources): A.* 4/6, B.* 1/6, C.* 2/6, D.doc 2/6, D.jpg 1/6; Hash-0 is always excluded.
- Ingest settings:
  - hashAndFileType = HashLookup + FileTypeId + CentralRepoIngest. **(bug)** The no-filetype variant never gets the CR template.
  - kitchenSink = PictureAnalyzer, EFE, InterestingItems, FileTypeId, HashLookup, VMExtractor, PhotoRec, DataSourceIntegrity, CR, FileExtMismatch.

**IngestedWithHashAndFileTypeInterCaseTest.** Case3 is the current case.
- testOne: `AllInterCase(F,F, FILES type, 0)` is non-empty. Expected counts (file @ data source):

  | Case | Data source | Expected counts |
  |---|---|---|
  | Case1 | c1ds1 | 0.dat 0, A.pdf 1, A.jpg 1 |
  | Case1 | c1ds2 | 0.dat 0, A.pdf 1, A.jpg 1 |
  | Case2 | c2ds1 | B.pdf 0, B.jpg 0 |
  | Case2 | c2ds2 | A.pdf 1, A.jpg 1, D.doc 1 |
  | Case3 | c3ds1 | A.jpg 1, A.pdf 1, C.jpg 0, C.pdf 0, D.jpg 0 |
  | Case3 | c3ds2 | C.jpg 0, C.pdf 0, D.doc 1 |

- testTwo: `SingleInterCase(<Case2 CR id>, …)` gives the same counts. The CR case id is found by stripping the 20-character timestamp suffix.
- testThree: threshold **50**, then `filterMetadata()`. Everything becomes 0 **except D.doc** in c2ds2 and c3ds2 (=1 each): A is at 66% > 50% and removed; D.doc is at 33% and stays.

**CommonAttributeSearchInterCaseTest** (`CommonFilesAttrs_img1..4_v1.vhd`, kitchenSink):
- testOne: for each of USB, DOMAIN, FILE, EMAIL and PHONE, the all-inter-case result is non-empty and every instance has that type.
- testTwo: USB thresholds 100 → 13, 20 → 0, 90 → 2. These were never active.

**Port plan.** Build synthetic data sources with `pm-testkit`: tiny FAT or logical file sets with identical content per name. Run hashing and file type, then the searcher. All the tables above become straightforward tests. Enable the inter-case ones, because postmortem has no JIRA-4166 blocker.

---

## 9. Keyword search (pm-keyword)

Sources: `KeywordSearch/test/unit/src/org/sleuthkit/autopsy/keywordsearch/`. These are pure unit tests with no fixtures. `KeywordSearchTestSuite` only bundles `CreditCardValidatorTest` and asserts nothing itself.

### CreditCardValidatorTest → `ccn::is_valid_ccn(&str) -> bool`

Production logic is in `KeywordSearch/src/.../CreditCardValidator.java`. Port it exactly. It is used to filter regex hits from the "Credit Card Numbers" list before a `TSK_ACCOUNT` (credit card) artefact is created.

The algorithm:
1. A number may contain spaces *or* dashes, never both. A mix returns false.
2. The canonical form has the separator stripped. The groups are the raw input split on the separator.
3. **BIN length check**: take the first 8 digits as an integer and look them up in a range map from IIN range to the allowed lengths. If no range matches, any length is accepted. If a range matches, the canonical length must be in its set. Java will throw if there are fewer than 8 chars (not tested). In Rust, return false.
4. **Grouping check**, by canonical length. With one group (no separator) every length is fine. Otherwise:
   - 15 digits: exactly 3 groups of 4-6-5 (Amex). The UATP 4-5-6 grouping is commented out.
   - 16 digits: exactly 4 groups of 4-4-4-4.
   - 19 digits: either 2 groups of 6-13 (UnionPay) or 5 groups of 4-4-4-4-3.
   - Any other length: the first group must have exactly 4 digits. The rest is free.
5. Luhn check on the canonical digits.

The IIN range table is order-sensitive. Guava `RangeMap.put` makes a later range override an earlier overlapping range. Insertion order, half-open `[lo, hi)` on the first 8 digits:

| Range | Lengths | Brand |
|---|---|---|
| 34000000–35000000, 37000000–38000000 | {15} | Amex |
| 40000000–50000000 | 12..=19 | Visa |
| 40260000–40270000, 41750000–41750100, 44050000–44060000, 45080000–45090000, 48440000–48450000, 49130000–49140000, 49170000–49180000 | {16} | Visa Electron |
| 62000000–63000000 | {16,17,18,**29**} (sic: a typo for 19) | China UnionPay |
| 51000000–56000000, 22210000–27210000 | {16} | MasterCard |
| 50609900–50619900, 65000200–65002700 | {16,19} | Verve |
| 50000000–50100000, 56000000–59000000, **60000000–70000000**, 63900000–63910000, 67000000–68000000 | 12..=19 | Maestro |
| 30000000–30600000, 30950000–30960000, 38000000–40000000 | {16,17,18,29} | Diners Intl |
| 36000000–37000000 | 14..=19 | Diners Intl |
| 54000000–56000000 | 14..=19 | Diners US/CA |
| 60110000–60120000, 62212600–62292600, 64400000–66000000 | {16,17,18,29} | Discover |
| 35280000–35900000 | {16,17,18,29} | JCB |
| 50190000–50200000 | {16,17,18,29} | Dankort |
| 63600000–63700000 | {16,17,18,29} | InterPayment |

Note that the Maestro `60000000–70000000` put happens *after* UnionPay, so it overrides it. That is why the 19-digit UnionPay case `6239747947839518659` passes despite the `29` typo. Verve 65000200–65002700 is overridden by Maestro too, but Discover 64400000–66000000, inserted later, overrides it again. To reach parity, implement the same "last insert wins" semantics, or precompute a flattened, non-overlapping table and test it against all the vectors below. Decide whether to fix the `29` typo deliberately and record an ADR. The test vectors below pass either way, as long as the override order is kept.

Test vectors (port verbatim; `T` = valid, `F` = invalid):

`testLengthMatchesBin`
- Amex: `3431 136294 58529` T; `3431-136294-5850` F (too short); `34311362945850` F; `3431 1362 9458 5034` F (too long)
- Verve: `506099002418342` F (15); `5060990024183426` T (16); `50609900241834267` F (17); `506099002418342675` F (18); `5060990024183426759` T (19); `50609900241834267591` F (20)
- Visa: `4000 0000 000` F (11); `4000 0000 0002` T (12); `4000 0000 0002 2` T (13); `4000 0000 0002 26` T (14); `4000 0000 0002 267` F (15, wrong grouping); `4000 000000 02267` T (15, 4-6-5); `4000 0000 0002 2675` T (16); `4000 0000 0002 2675 9` T (17); `4000 0000 0002 2675 91` T (18); `4000 0000 0002 2675 918` T (19); `4000 0000 0002 2675 9183` F (20)
- Visa Electron (16 only): `4026 1200 006` F; `4026 0000 0065` F; `4026 0000 0065 9` F; `4026 0000 0065 91` F; `4026 0000 0065 918` F; `4026 0000 0065 9187` T; `4026 0000 0065 9187 0` F; `4026 0000 0065 9183 00` F; `4026 0000 0065 9183 000` F; `4026 0000 0065 9183 0000` F

`testIsValidCCN16`: `1234567890318342` T; `1234-5678-9031-8342` T; `1234 5678 9031 8342` T; `1234567890318341` F (Luhn); `1234-5678-9031 8342` F (mixed separators); `1234-5678-90-318342` F; `1234 5678 90 318342` F; `1-2-3-4-5-6-7-8-9-0-3-1-8-3-4-2` F

`testIsValidCCN15`: `3431 136294 58529` T; `3431-136294-58529` T; `343113629458529` T; `343113629458528` F; `3431 13629458 529` F; `3431 136294-58529` F; `377585291285489` T; `3775-852912-85489` T; `3775 852912 85489` T; `377585291285488` F; `3775-852912 85489` F; `37-7585-29-1285489` F; `377585 29128548 9` F

`testIsValidCCN19`: `4539747947839518654` T; `4539-7479-4783-9518-654` T; `4539 7479 4783 9518 654` T; `4539747947839518653` F; `4539-7479 4783 9518 654` F; `45374 79 4783 9518 654` F; `6239747947839518659` T; `623974 7947839518659` T; `623974-7947839518659` T; `6239747947839518658` F; `623974-79478395 18659` F; `62397-47947839518659` F

`testIsValidCCN18`: `123456789031834267` T; `1234 5678 9031 8342 67` T; `1234-56789031834-267` T; `123456789031834266` F; `123 456789031834267` F; `1234-56789 031834267` F

`testIsValidCCN17`: `12345678903183426` T; `1234 5678 9031 8342 6` T; `1234-56789031834-26` T; `12345678903183425` F; `123 45678903183426` F; `1234-56789 03183426` F

`testIsValidCCN14`: `12345678903183` T; `1234 5678 9031 83` T; `1234-5678903183` T; `12345678903182` F; `123 45678903183` F; `1234-56789 03183` F

`testIsValidCCN13`: `1234567890318` T; `1234 5678 9031 8` T; `1234-567890318` T; `1234567890317` F; `123 4567890318` F; `1234-56789 0318` F

`testIsValidCCN12`: `123456789031` T; `1234 5678 9031` T; `1234-56789031` T; `123456789030` F; `123 456789031` F; `1234-56789 031` F

### MiniChunkHelperTest → keyword index chunk IDs

Solr document IDs are `<objId>_<chunk>` with an optional `_mini` suffix. Mini-chunks are the overlap window used for highlighting across chunk boundaries.
- `isMiniChunkID("1_1_mini")` = true; `"1_1"` = false; `"1"` = false
- `getBaseChunkID("1_1_mini")` = `"1_1"`; `"1_1"` = `"1_1"`; `"1"` = `"1"`. Only a trailing `_mini` is stripped (`replaceFirst("_mini$", "")`).

These only matter if postmortem keeps a chunk/mini-chunk scheme in its own index (for example tantivy). Port them as tests of whatever chunk-ID codec we choose.

### LanguageSpecificContentQueryHelperTest → language-specific (Japanese) query expansion
- `expandQueryString("query")` = `"text:query OR content_ja:query"`. The query is OR'd across the `text` field and every language-specific content field. Only `content_ja` exists today.
- `findNthIndexOf(s, pattern, n)` returns the index of the n-th (0-based) occurrence, or -1:
  - `("A1AA45","_",0)` = -1; `("A1AA45","A",0)` = 0; `(…,"A",1)` = 2; `(…,"A",2)` = 3; `(…,"A",3)` = -1
  - `("A1AA45","",0)` = 0; `("","A",0)` = -1; `("A1AA45","A",-1)` = -1; `("A1AA45","A",999)` = -1
- `buildTermfreqQuery("query", {field1:[term1], field2:[term1,term2]})` = `termfreq:sum(termfreq("field1","term1"),termfreq("field2","term1"),termfreq("field2","term2"))`

Solr-specific. Port only the semantics we keep: a multi-field OR query when CJK tokenisation is enabled, and n-th-occurrence search for hit snippets.

### Keyword search: what Autopsy does not unit-test (derive tests ourselves)

Autopsy has no unit tests for these. The only coverage is the regression harness (keyword hit counts in the gold DB dump, see the harness section):
- The built-in locked regex lists in `KeywordSearchList.java` (`Phone Numbers`, `IP Addresses`, `Email Addresses`, `URLs`, `Credit Card Numbers`). Derive positive and negative cases from those regexes when porting.
- Literal, substring and regex matching semantics, and the case-insensitive/exact-match options of `KeywordSearchQuery` and `LuceneQuery`/`RegexQuery`.
- Keyword list XML import/export. `test/input/notablekeywords.xml` is a tiny real example of the format; see the harness section.

---

## 10. Data source summary (pm-summary)

Sources: `Core/test/unit/src/org/sleuthkit/autopsy/datasourcesummary/datamodel/`. These are Mockito tests against a mocked `SleuthkitCase`/`Blackboard`. In Rust, build an in-memory case (or a fake `ArtifactSource` trait) holding the listed artefacts and call the summary functions. Common helpers:
- `RandomizationUtils.getMixedUp(list)` interleaves first, last, second, second-last and so on. It is used to feed unsorted input. Port it as a test helper, or use a seeded shuffle.
- `TskMockUtils.getDataSource(id)` and `getArtifact(type, id, ds, attrs…)` are mock builders. Use an equivalent artefact builder in `pm-testkit`.
- `DAY_SECONDS = 86400`.
- The integration-test group `Core/test/qa-functional/src/org/sleuthkit/autopsy/datasourcesummary/UserActivitySummaryTests.java` snapshots `getRecentDomains(ds, 10)` per data source against gold YAML (section 17.1).

### DataSourceInfoUtilitiesTest → generic "get artefacts sorted by attribute, limited"

Function: `getArtifacts(case, artifactType, dataSource, sortAttrType, sortOrder, maxCount)`.
- **Sort ascending and descending** on each value type. Strings are compared **case-insensitively**: `aardvark, Bear, cat, Dog, elephant` (TSK_WEB_COOKIE / TSK_NAME). Ints `22,31,42,50,60` (TSK_PROG_RUN / TSK_COUNT). Longs, the same values on TSK_BYTES_SENT. Datetimes `1d..5d` (TSK_RECENT_OBJECT / TSK_DATETIME_ACCESSED). Doubles `68.5,70.1,71.5,72.3,73.5` (TSK_GPS_BOOKMARK / TSK_GEO_LATITUDE). Input is mixed up and output must be in exact order. For descending, the expected list is reversed.
- **Limits**: count 0 means unlimited. With 5 ints, count 3 → the first 3; count 5 → 5; count 10 → 5.
- **Invalid attribute types**: sorting on a JSON attribute (TSK_GPS_ROUTE / TSK_GEO_WAYPOINTS value `{ "data": "none" }`) or a BYTE attribute (custom type 999 "BYTE_ARR_ATTR_TYPE" on TSK_YARA_HIT) is an `IllegalArgumentException`. In Rust, return an error variant.
- **purgeAttrNotPresent**: artefacts without the sort attribute are dropped. Input: (id10 count5 dt1d), (id20 count6, no dt), (id30 count7 dt3d). Sort by DATETIME ascending → [id10, id30].
- **multipleAttrsPresent**: sorting by COUNT ascending, ignoring the other attributes: (10: 7), (20: 6), (30: 5) → [30, 20, 10].
- A DB error propagates as an error. `maxCount = -1` → `IllegalArgumentException` without touching the DB. An empty result → empty list.
- Null-safe getters: `getStringOrNull` (TSK_ACCOUNT_TYPE "Skype"), `getIntOrNull` (TSK_COUNT 16), `getLongOrNull` (TSK_ASSOCIATED_ARTIFACT 1001), `getDateOrNull` (TSK_DATETIME 42 days, returned as a date whose epoch seconds = 3628800). Each returns None when the attribute is absent.

### RecentFilesSummaryTest → "Recent Files" tab (recent docs, downloads, attachments)

Each of `getRecentlyOpenedDocuments`, `getRecentDownloads` and `getRecentAttachments`:
- count ≤ 0 → error, and the DB is never queried. A `None` data source → empty list without a query. (The test named `getRecentAttachments_noDataSource_ReturnsEmptyList` actually calls the negative-count helper, a copy-paste bug. Port the intent.) An empty DB result → empty list, with exactly one query.
- **Sorted and limited**: generate N ∈ {1, 9, 10, 11} artefacts with `time(idx) = (365 - idx) * 86400 + 1`, request 10, expect `min(10, N)` results in descending time order (idx 0 first).
  - Docs: TSK_RECENT_OBJECT with TSK_DATETIME_ACCESSED and TSK_PATH `"/path/to/downloads/<idx>"`.
  - Downloads: TSK_WEB_DOWNLOAD with DATETIME_ACCESSED, TSK_DOMAIN `www.domain<idx>.com` and TSK_PATH `/path/to/downloads/doc<idx>.pdf`. Asserts path and domain.
  - Attachments: a TSK_ASSOCIATED_OBJECT artefact whose parent file is `/path/to/attachment/<idx>` + `<idx>-filename.png`. Its TSK_ASSOCIATED_ARTIFACT points to a TSK_MESSAGE with TSK_EMAIL_FROM `person<idx>@basistech.com` and TSK_DATETIME_SENT. The result's path is parent path joined with name, sender = email-from, date = sent time.
- **Unique paths**: the same path at times 1d, 1d+1 and 1d+2 gives 1 result with the latest time. For downloads, path `/a/path1` appears with domains domain1.com (1d), domain1.com (1d+1, not passed), domain2.com (1d+2) and domain2a.com (1d+3) → 1 result, `/a/path1`, time 1d+3, domain `domain2a.com`. Dedupe is by path, keeping the newest.
- **Filters missing data**: an artefact with a null time or a time of 0 is dropped. Only the 1d item remains.
- **Attachment filtering** (`getRecentAttachments_filterData`). Of these items, only 2 survive, ordered newest first:
  - kept: EMAIL_MSG `person@sleuthkit.com` 1d `/parent/path/msg.pdf`
  - kept: MESSAGE `person_on_skype` 1d+1 `/parent/path/to/skype/skype.png`
  - dropped: CALLLOG as the associated message type (wrong type)
  - dropped: null sent time; sent time 0
  - dropped: associated object with no parent file
  - dropped: no TSK_ASSOCIATED_ARTIFACT attribute
  - dropped: associated artefact id that does not resolve
- **Attachment unique path**: three attachments at `/parent/path/msg.pdf` (EMAIL 1d, MESSAGE 1d+1, EMAIL 1d+2) → 1 result, time 1d+2, sender `person2@sleuthkit.com`.

### UserActivitySummaryTest → "User Activity" tab

Covers `getMostRecentWebSearches`, `getRecentAccounts`, `getRecentDomains`, `getRecentDevices` and `getTopPrograms`.
- Shared invariants, for all 5: count 0 or -1 → error with no query (the loop passes -1 both times, a bug). `None` data source → empty with no query. An empty DB → empty list.
- **Devices** (TSK_DEVICE_ATTACHED: DEVICE_ID, DATETIME, DEVICE_MAKE, DEVICE_MODEL):
  - The excluded model list is `ROOT_HUB` and `ROOT_HUB20`. Of models `ROOT_HUB`, `ROOT_HUB20` and `ACCEPTED DEVICE`, only the last is returned: id `ID ACCEPTED DEVICE`, make `MAKE ACCEPTED DEVICE`, time 42d.
  - Limited to `min(10, N)` for N ∈ {1, 9, 10, 11}.
  - Unique by device ID: ID1 at 1d, 1d+1 and 1d+2 → 1 result with the latest time.
- **Web searches** (TSK_WEB_SEARCH_QUERY: TSK_TEXT, DATETIME_ACCESSED):
  - Grouped case-insensitively. Inputs: "This is Query 1"@1d, "This is Query 2"@2d, Q2-upper@3d, Q1-upper@4d, Q1-lower@5d → 2 results: Q1 (last 5d), then Q2 (last 3d). Ordered by most recent.
  - Translation: with no provider, `translate` is never called and the translation is null. With a provider, it is called once per query. A null, empty or whitespace translation, or one equal to the original after trim and uppercase, gives null. Otherwise the result is `orig + " [TRANSLATED]"`.
  - Limited to `min(10, N)`.
- **Domains** (TSK_WEB_HISTORY: TSK_DOMAIN, DATETIME_ACCESSED):
  - The window is 30 days back from the *most recent access in the data*, with an inclusive boundary.
  - Inputs: google@60d, google@30d, basistech@30d-1s, github@30d, github@1s, stackoverflow@1s → [google (last 60d, 2 visits), github (last 30d, 1 visit)].
  - Excluded domains: `localhost` and `127.0.0.1`, compared after trim and uppercase.
  - Sorted by visit count, descending: google@1,6 and basistech@2,3,4 → [basistech (3 visits, last 4), google (2 visits, last 6)].
  - Limited to `min(10, N)`.
- **Accounts**. Queries TSK_MESSAGE, TSK_EMAIL_MSG and TSK_CALLLOG once each.
  - Email: date = max(RCVD, SENT); with neither, the artefact is dropped. Its type label is the bundle string `DataSourceUserActivitySummary_getRecentAccounts_emailMessage` ("Email Message").
  - Call log: max(START, END), labelled "Call Log".
  - Message: needs both TSK_MESSAGE_TYPE and DATETIME, and is labelled by the message type, for example "Skype".
  - Grouping and ordering: input emails at 1d-11, 1d-12, 1d+13; call logs at 1d-21, 1d+22; Skype at 1d-31, 1d+32; Facebook at 1d-41, 1d+42. Result: [Facebook 1d+42, Skype 1d+32, Call Log 1d+22, Email 1d+13]. Each group keeps its latest entry, sorted newest first.
- **`getShortFolderName(path, appName)`**, case-insensitive. Each case must also hold with the input fully upper-cased and fully lower-cased:
  - `/Program Files/Item/Item.exe` → `Item`
  - `/Program Files (x86)/Item/Item.exe` → `Item`
  - `/Program_Files/Item/Item.exe` → ``
  - `/User/test_user/item/AppData/Item/Item.exe` → `AppData`
  - `/User/test_user/item/Application Data/Item/Item.exe` → `AppData`
  - `/Other Path/Item/Item.exe` → ``
- **Top programs** (TSK_PROG_RUN: PROG_NAME, PATH, DATETIME, COUNT):
  - Filtered out: a name equal to `ntosboot` (case-insensitive), a path starting with `/WINDOWS` (case-insensitive: `/Windows/`, `/Windows/Nested/`), and a blank or null name. ProgramSuccess.exe (no count or date) and ProgramSuccess2.exe (22, 22) survive, ordered [Success2, Success].
  - Grouping is by (name, path). Run times = **max** COUNT (not the sum); last run = max DATETIME.
    - program1 /etc/ with (21,21), (1,31), (2,11) → runs 21, last 31
    - program1 /another/ with (31,21), (1,31), (2,11) → runs 31, last 31
    - program2 /another/ with (10,21), (1,22), (2,11) → runs 10, last 22
    - Order: [program1 /another/, program1 /etc/, program2].
  - Ordering: runs descending, then last run descending, then name case-insensitively:
    - counts 8, 9, 10 → [P3, P2, P1]
    - no counts, dates 1, 3, 2 → [P2, P3, P1]
    - no counts or dates, names cProgram, BProgram, aProgram → [aProgram, BProgram, cProgram]
  - Limit: when count or date data exists, `min(10, N)`. When neither exists, the list is **not limited** (all N returned) and is sorted alphabetically.

---

## 11. Discovery: domain search (pm-discovery)

Sources: `Core/test/unit/src/org/sleuthkit/autopsy/discovery/search/`. These are mock-based.

### DomainSearchTest → grouping and paging over cached results

Pre-grouped results from the cache: group 1 = [google, yahoo, bing, amazon], group 2 = [facebook, spotify, netbeans], group 3 = [youtube].
- `getGroupSizes`: a single group → {1: 4}; multiple groups → {1: 4, 2: 3, 3: 1}; an empty map → no groups (size 0).
- `getDomainsInGroup(group, offset, pageSize)`:
  - (0, 3) → first 3, in order
  - (0, 100) → all 4
  - (0, 2) → 2
  - (3, 1) → [amazon]
  - (20, 5) → []
  - (0, 0) → []
  - group 2 (1, 2) → [spotify, netbeans]
  - paging over 8 domains [google, yahoo, bing, amazon, facebook, capitalone, spotify, netsuite] with size 2 → pages are consecutive slices
- `TestSearchContextImpl(false)` is a "not cancelled" token. Port it as a cancellation token.

### DomainSearchCacheLoaderTest → grouping and sorting

`ResultDomain(domain, start, end, totalVisits, visits, filesDownloaded, dataSourceId)`.
- Group by data source, groups sorted by name, results by domain name. Six domains on data sources 110, 100, 105, 110, 100, 20 → **4 groups**. Within each group, domains are strictly ascending by name.
- No grouping, results by domain → 1 group, ascending by domain.
- No grouping, results by data source → 1 group, ascending by data source id.
- Group by data source, groups sorted by size, results by domain → 2 groups.
- The loader also reads TSK_WEB_CATEGORIZATION artefacts, mocked as empty.

### DomainSearchArtifactsCacheTest → "artefacts for a domain" lookup
- A non-web artefact type (TSK_CALLLOG) → `IllegalArgumentException`. The type label must start with `TSK_WEB`.
- An interrupted thread → a `DiscoveryException` caused by `InterruptedException`. The loader checks interruption per artefact. In Rust, check a cancellation token per artefact.
- Matching is on the TSK_DOMAIN attribute only, with **trim and lowercase** on both sides:
  - `google.com` vs request `google.com` → 1
  - `google.com` vs `facebook.com` → 0
  - only TSK_URL `https://www.dce1.com/search`, request `dce.com` → 0 (URLs are not consulted)
  - `xYZ.coM` vs `xyz.com` → 1
  - `google.com` vs `GooGle.coM` → 1
- The artefact type used is TSK_WEB_BOOKMARK.

---

## 12. External viewer rules (pm-ui settings)

Sources: `Core/test/unit/src/org/sleuthkit/autopsy/directorytree/`. These test a Swing table model and panel. In egui, port them as tests of the settings *model* (a rule list) and its selection and enable logic.

`ExternalViewerRule(name, exePath, RuleType::{MIME, EXT})`.

TableModelTest:
- Add `image/png`/`test.exe`/MIME → 1 row with fields intact. Rows 0 → 3 after adding pngMime, `.txt`→`notepad.exe` EXT and `.wav`→`video.exe` EXT.
- Column names come from the constructor (`A`, `B`). Column 2 is out of bounds (error). The column class is String. The column count is 2, or 5 for 5 names.
- `getValueAt(1,0)` = `.txt`, `(1,1)` = `notepad.exe`, `(0,0)` = `image/png`, `(0,1)` = `test.exe`. `getRuleAt` returns the rule.
- `setRule(i, rule)` replaces at index i. The test's first check reads index 1, which is unchanged.
- `removeRule(0)` → 0 rows, and `containsRule` is false. Cells are not editable. `containsRule(pngMime)` = true; `containsRule(other)` = false; `containsRule(null)` = false. Rule equality is by value.

PanelTest:
- Edit and delete are enabled only when a row is selected. They are false on an empty model, false after adding with no selection, and true after selecting row 0. They become false again after the selected rule is removed.
- Deleting row 0 through the button → row count 0, buttons disabled. Deleting index -1 → `IndexOutOfBoundsException`; in Rust, return an error or no-op without panicking.
- **Single selection**: with 5 rules, `setSelectionInterval(0, 2)` selects only row 2.

Postmortem should also unit-test the actual lookup behaviour (MIME rule vs extension rule precedence). The inventory for viewers covers this; Autopsy does not test it.

---

## 13. Machine translation (pm-translate)

`BingTranslatorTest` and `GoogleTranslatorTest` (`Core/test/unit/src/.../texttranslation/translators/`) are **empty**. Every assertion is commented out because the tests need online API keys. The disabled cases were Bing `"gato"`→`"cat"`, a German quick-start sentence, `"\"gato\""` escaping and `"gato\nperro"` line breaks; Google `"traducar"`→`"translate"`. Nothing to port. Write offline tests of request building and response parsing against recorded JSON instead. The translation-service *contract* (no provider → no call; blank or identical translation → null) is covered by UserActivitySummaryTest above.

---

## 14. Recent activity: IE `index.dat` and Windows time formats (pm-core time, pm-recentactivity)

Sources: `thirdparty/pasco2/test/isi/pasco2/` (pasco2 by Bradley Schatz, **GPL v2**). Autopsy bundles `pasco2.jar` for IE `index.dat` parsing in RecentActivity. The tests are JUnit 3 and were never wired into Autopsy's build. Several use hard-coded `D:\…` paths or the Win32 API through ctypes4j, so they never ran in Autopsy CI.

Portable vectors, all UTC:

- **FILETIME, little-endian hex** (`TestFileTime.testHexLittleEndian`):
  - `40B3B13FED2AC001` → `2000-09-30T14:46:43.060Z`
  - `39B1C8FFD2AFBF01` → `2000-04-26T22:58:55.899Z`
  - `69D8F2DDA9D7C101` → `2002-03-30T05:15:11.620Z`
  - `308F41C6C6D2C501` → `2005-10-17T02:59:22.147Z`
- **FILETIME, big-endian hex** (`testHexBigEndian`):
  - `01C1D7A9DDF2D869` → `2002-03-30T05:15:11.620Z`
  - `01C1C7F9DDFBD86F` → `2002-03-10T06:07:32.813Z`
  - `01C3DF121D432432` → `2004-01-20T04:58:58.985Z`
- `testPassingInts`: low bytes `30 8F 41 C6` and high bytes `C6 D2 C5 01` as unsigned LE u32 → `2005-10-17T02:59:22.147Z`.
- `testBitStuff`: bytes `40 B3 B1 3F` read as an LE i32 = `1068610368`.
- `TestReadFunctions`: LE i32 of `{0,0,40,0}` = `2621440`; of `{0,0x90,1,0}` = `102400`.
- `testFileTime` (low = `-543274336`, high = `29744990`) asserts `2005-11-02T03:31:12.657Z`. That **encodes a pasco2 bug**: the low dword is treated as *signed*. The correct value, low read as u32 `3751692960`, is **`2005-11-02T03:38:22.154Z`**. Port the correct value and do not replicate the bug.
- **DOS date/time** (`TestDOSTime`). Note that the comments in that file are wrong; the asserted strings are correct:
  - LE hex `F788F41E` (time = 0x88F7, date = 0x1EF4) → `1995-07-20T17:07:46.000Z`
  - `B67A3F28` → `2000-01-31T15:21:44.000Z`
  - `41B14C2B` → `2001-10-12T22:10:02.000Z`
  - `DOSTime(date=13404, time=1075)` → `2006-02-28T00:33:38.000Z`
  - All are interpreted as UTC with no local-time conversion.
- `TestPlatform` needs Win32. The portable facts: FILETIME 0 → 1601-01-01 00:00:00.000; the Unix epoch is FILETIME `0x019DB1DED53E8000` (116444736000000000); and `2006-02-21T04:35:17.625Z` round-trips. Map these to `pm_core::time` FILETIME/DOS converter tests. Some may already exist; tick them off.
- `TestRecordLayer`, with fixtures in the same dir:
  - `empty.cache.index.dat` (33 kB) → 0 valid records
  - `first.cache.index.dat` (49 kB) → **30** valid records
  - `empty.history.index.dat` (33 kB) → 0
  - `first.history.index.dat` (33 kB) → **2** valid records
  - All are `Client UrlCache MMF Ver 5.2` files. The history file has `Visited: …@…` URL records (about:Home, qut.edu.au, ctypes4j.sourceforge.net). The cache file has `URL ` records with HTTP headers and cache directories `0VUUYHWA`, `K9EV0DE3`, `HQJJGYM3` and `MOFP9EZS`.
  - Note the history count of 2 conflicts with the roughly 8 "Visited:" strings visible: only allocated, valid records count, and parsing was not called before iterating in the "first" tests. Treat the counts as an approximate oracle and verify against an independent parser (for example libmsiecf's documentation).
- `TestAllocationLayer`: the allocation bitmap at file offset `0x250`, one bit per 0x80-byte block starting at the hash-table offset (history storage start 0x4000; cache 0x5000). It asserts that `isRecordAllocated(addr)` matches the bitmap bit. The fixture paths are absolute `D:\` paths, so the test never ran. Port the invariant as a property test on our own synthetic index.dat.

**Licence**: the pasco2 tests and `.dat` fixtures are GPL v2 and must not be vendored into Apache-2.0 postmortem. The time vectors are facts and can be re-typed. Generate our own index.dat fixtures with a builder in `pm-testkit`.

---

## 15. YARA (pm-yara)

`thirdparty/yara/YaraWrapperTest/` is a manual `main()` harness for the JNI bridge (`YaraJNIWrapper.findRuleMatch(compiledRulePath, bytes, len, timeoutSec=100)` and `findRuleMatchFile`), not an automated test.
- Fixtures: `resources/hello.txt` (11 bytes, `Hello World`, no newline) and `resources/hello.compiled` (5.2 kB compiled YARA rules, magic `YARA`, libyara-version-specific). The strings show rule `HelloWorld` in namespace `default` with strings `Hello` and `World`, plus metadata `name`, `description` "The file attempted to install a driver", `categories` (copied from an `InstallsDriver` example rule).
- Expected behaviour (implicit): scanning `hello.txt` returns a match list containing `HelloWorld`. A non-match prints "FindRuleMatch return an empty list".
- Port: use source rules, not compiled blobs (a compiled `.yarc` is tied to the libyara version, and a pure-Rust engine such as `yara-x` cannot load them). Write a rule `HelloWorld { strings: $a="Hello" $b="World" condition: all of them }` and assert one match on `b"Hello World"` and none on `b"Goodbye"`. Test both the in-memory and the file-path scan paths, plus the timeout setting.

---

## 16. Not relevant to postmortem

- `release_scripts/localization_scripts/test/*.py` (`test_csvutil`, `test_fileutil`, `test_itemchange`, `test_propsutil`) test Autopsy's translation-bundle maintenance scripts (`.properties` diffing and CSV round-trip). No product behaviour. Skip them, unless postmortem adopts a similar i18n workflow; Fluent/gettext tooling has its own tests.
- `Core/test/unit/src/mockito-extensions/…MockMaker` and `CoreTestLibs/` (mockito-core 4.8.1, byte-buddy 1.15.11, objenesis 3.4, junit 4.13.2) are only test-library plumbing.

---

## 17. Test-support code

### 17.1 Integration-test framework (`Core/src/org/sleuthkit/autopsy/integrationtesting/**`)

This is a newer, headless alternative to the GUI regression harness. It runs ingest on configured data sources, calls `@IntegrationTest` methods that return data, writes the results as YAML, and diffs them against a gold directory. **It is the closest model for the postmortem harness** (see section 18.3).

**Environment config (`EnvConfig`).**
- Supplied as JSON via the system property `integration-test.configFile`, or as individual `integration-test.<key>` properties. Nested keys are dotted, for example `integration-test.dbConnection.userName`.
- Fields:
  - `rootCaseOutputPath` and `rootTestOutputPath` are required.
  - `rootTestSuitesPath`.
  - `rootGoldPath` and `diffOutputPath` are optional.
  - `workingDirectory` defaults to the config file's directory. Relative paths resolve against it.
  - `useRelativeOutput`.
  - Connections `dbConnection`, `mqConnection`, `solrConnection`, `zkConnection` and `crConnection`, each `{hostName, port?, userName?, password?}`. Default ports: Postgres 5432, ActiveMQ 61616, Solr 8983, ZooKeeper 9983.
- Unknown keys are ignored.
- Example: `Core/src/…/integrationtesting/doc/example_envconfig.json`.

**Test suite JSON (`TestSuiteConfig`).**
- The suites path is scanned recursively for `*.json`; a file holds one suite object or an array of them.
- Fields:
  - `name` (required)
  - `description`
  - `dataSources` (required, non-empty)
  - `caseTypes` (`singleUser` | `multiUser` | `both`, where both means multi then single)
  - `configurationModules`: list of parameterised resources, each a class name string or `{resource, parameters}`
  - `integrationTests` (required), with `excludeAllExcept` (a whitelist with parameters) or `includeAllExcept` (a blacklist), matched case-insensitively on the class name
- JSON schemas are in `integrationtesting/jsonschema/`; the example suite is `doc/example_testsuite.json`.

**Run flow.** For each suite and each case type:
1. Set the CR choice (SQLite for single-user).
2. Create the case `<name>_<timestamp>`.
3. Add each data source with the first matching auto-ingest data-source processor.
4. Apply configuration modules in a chain:
   - With no modules, the default is every ingest module with default settings, except Plaso and Malware Scan, as ALL_MODULES.
   - The built-in `IngestModuleSetupManager` takes `{modules: [factory class names]}`.
5. Run ingest.
6. For each `IntegrationTestGroup` service: `setupClass`; then `setup` → method → `tearDown` for each `@IntegrationTest` method (at most one Gson-deserialised parameter); then `tearDownClass`. A thrown exception becomes the method's result.
7. Revert the configuration modules, write results, close the case.

**Output (`OutputResults`).**
- A nested map `package → class → method → result`. Throwables are written as `{message, stackTrace, innerException}`.
- The file is `<rootTestOutputPath>[/<suite rel path>]/<suiteName>-<singleUser|multiUser>.yml`, in SnakeYAML block style with no class tags.

**Diff (`DiffService`).**
- Recursively compares the output and gold trees by relative path.
- A gold file missing from the output → `MISSING FILE IN CURRENT: <rel>`; an extra output file → `ADDITIONAL FILE IN CURRENT: <rel>`. Each header is wrapped in 59-dash lines.
- Otherwise a line diff, with each delta written as `-<srcPos+1>,<n> +<tgtPos+1>,<m>` followed by `< ` and `> ` lines.
- A non-empty diff is written to `diffPath` and fails with `IntegrationTestDiffException`.
- **(bug)** Missing paths only log and do not stop the run.

**The one test group, `datasourcesummary/UserActivitySummaryTests.getRecentDomainsTest()`.** For each data source, it outputs `getRecentDomains(ds, 10)` as a list of `{domain, visitTimes, lastAccessed}`. The semantics are in section 10. Ties keep HashMap order, so the output is **nondeterministic** (bug). The postmortem port must sort ties deterministically, for example by domain.

`Core/test/qa-functional/src/org/sleuthkit/autopsy/integrationtesting/TestRunner.java` is the JUnit entry point. A diff becomes `Assert.fail`. It runs only through `ant integration-test` (`test.timeout = 14400000` ms) and is excluded from `test-qa-functional`.

### 17.2 Synthetic artefact generators (`Core/src/org/sleuthkit/autopsy/test/**`; disabled, since `@ServiceProvider` is commented out)

These are useful templates for a postmortem "fake data" ingest module used to snapshot-test the UI and the blackboard.

- **CustomArtifactsCreator** (data-source and file modules; the file module skips directories and virtual files).
  - Registers artefact type `CUSTOM_ARTIFACT` / "Custom Artifact" with attributes:

    | Attribute type | Value type | Display name |
    |---|---|---|
    | `CUSTOM_INT_ATTRIBUTE` | INTEGER | "Custom Integer" |
    | `CUSTOM_DOUBLE_ATTRIBUTE` | DOUBLE | "Custom Double" |
    | `CUSTOM_LONG_ATTRIBUTE` | LONG | "Custom Long" |
    | `CUSTOM_DATETIME_ATTRIBUTE` | DATETIME | "Custom Datetime" |
    | `CUSTOM_BYTES_ATTRIBUTE` | BYTE | "Custom Bytes" |
    | `CUSTOM_STRING_ATTRIBUTE` | STRING | "Custom String" |
    | `CUSTOM_JSON_ATTRIBUTE` | JSON | "Custom Json" |

  - Values: int 0, double 0.0, long 0, datetime 60, bytes `ABCD` (hex), string `Zero`, json `{"fruit": "Apple","size": "Large","color": "Red"}`.
  - Each attribute gets the extra source "Another Module" twice; **the second add must be a no-op**, because attribute sources are a set.
  - It posts a data artefact or an analysis result (score UNKNOWN), depending on the type's category.
- **InterestingArtifactCreator.** For each file it randomly creates one of:
  - TSK_WEB_BOOKMARK, TSK_URL `www.thisWebsiteIsStillFake.com`
  - TSK_KEYWORD_HIT: preview `Fake Keyword Preview Text`, set `Fake`, keyword `FakeKeyword`
  - TSK_CALLLOG, from `555-555-5555`

  It then adds a TSK_INTERESTING_ITEM with set `ArtifactsAndTxt` and TSK_ASSOCIATED_ARTIFACT pointing at the base artefact. The output is random, so it cannot be used with gold data.
- **TestAutopsyService.** Simulates case-open work: progress 20/40/60/80/100 with 1 s sleeps, then a 10,000-iteration loop that honours cancellation. Port it as a cancellation test of case-open hooks.

### 17.3 Test utilities (`Core/src/org/sleuthkit/autopsy/testutils/**`, `Core/test/unit/src/org/sleuthkit/autopsy/testutils/**`)

| Utility | Behaviour | Postmortem equivalent |
|---|---|---|
| `CaseUtils` | `createAsCurrentCase(name)` creates a single-user case `name_<timestamp>` in the temp dir; `closeCurrentCase` closes it without deleting | `pm-testkit::case::temp_case()` |
| `IngestUtils.addDataSource` | Fails if the path is missing. Runs the processor blocking. Fails on CRITICAL_ERRORS. Notifies "data source added". | `pm-testkit` ingest helpers |
| `IngestUtils.runIngestJob` | Fails if any module has a start-up error | `pm-testkit` ingest helpers |
| `IngestJobRunner` | Blocks until COMPLETED/CANCELLED for each data source; returns start-up errors instead of throwing them | Port the "return start-up errors" variant: the PhotoRec test in section 3 needs it |
| `DataSourceProcessorRunner` | Synchronous wrapper around the asynchronous data-source processor callback | |
| `TskMockUtils`, `RandomizationUtils`, `DataSourceSummaryMockUtils` | Mockito builders and the first/last interleave ("mixed up" order) | `pm-testkit` artefact builder, plus a seeded shuffle |

`Core/src/org/sleuthkit/autopsy/centralrepository/datamodel/DatabaseTestResult.java` is the CR connection-test status enum: `UNTESTED`, `CONNECTION_FAILED`, `SCHEMA_INVALID`, `DB_DOES_NOT_EXIST`, `TESTED_OK`.
- Postgres: connection failure → CONNECTION_FAILED; missing DB → DB_DOES_NOT_EXIST; bad schema → SCHEMA_INVALID.
- SQLite: missing file → DB_DOES_NOT_EXIST; **a connection failure also gives SCHEMA_INVALID** (bug).

This is worth a small test in the CR settings code.

### 17.4 Test-data acquisition (`Core/build.xml`, target `getTestDataFiles`)
- Files go into `Core/test/qa-functional/data` (`test-input`), downloaded with `skipexisting`. The target is a dependency of `test-init`.
- `.gitignore` excludes them, except the password image.
- URLs are `https://drive.google.com/uc?id=<ID>`:

| File | ID |
|---|---|
| EmbeddedIM_img1_v2.vhd | `1gyKzqJHtaBjFBqeB29N5vSpf1oH9N0bV` |
| BitlockerDetection_img1_v1.vhd | `1gvvV797dKczkwpWII4hIryBXCc0w5zL-` |
| SqlCipherDetection_img1_v1.vhd | `1lYzd--9YUB7yDccM7NPOo8PC1xxuOiKs` |
| IngestFilters_img1_v1.img | `1QRNKs824kksiJLZ4qcs59Nytw0fJKEjb` |
| IngestFilters_local1_v1.zip | `1OMTB5gD4_VuaFkLWu2I33FN8VAHoRQbW` |
| VeracryptDetection_img1_v1.vhd | `1X2DzJOJ1SjUkAtZXJ3oQHXkjF0NX_jOz` |
| CommonFiles_img1..4_v1.vhd | `19gRepVVvLsDHtBwIkX2VVjsBWIub7aIV`, `1chUMtA0CNyPBZ0yuTl9F_O103R21Ox0p`, `1TmdjSjV7MLwVEmgWGJLOYnWDiZeqOgmd`, `10u8ocYv6KKDVox6SRv_0fQeVqJjWAFJr` |
| c1ds1, c1ds2, c2ds1, c2ds2, c3ds1, c3ds2 (`_v1.vhd`) | `1pcYsYB1tTzg63kgTG52u40osbZ-rUHwr`, `11F_Kjht23aFfbdHN1Ryl2fLFv3A76E6O`, `1gVnHJEzUmCGVVqhcpI9UHB9zHY2zY5VL`, `1RodtAXWXrDvwuMVQYBM_QaOi3XR2B41F`, `1t4RKK375nKFAh2iGuu5Nttdy4EFyUqlv`, `1be6Szhb57duDkfCc_EGeyHCrAzrIDjs3` |
| CommonFilesAttrs_img1..4_v1.vhd | `1ns2olaWsBu_c4EoE4Seh8t_B3U5RnLKd`, `1c2egfrKDQqFD04EqhGIDfs82Uub-fGlx`, `1UczQeY5ZZiQaql_ErnPRxVyX6qMgbvil`, `13YJ6nNXiAxRjNKwzcN9ohwvnVupxPvXc` |

These are optional cross-check inputs for a `slow-tests` job. They do not state a licence, so do not commit them.

`Core/test/qa-functional/src/org/sleuthkit/autopsy/README.txt` notes two things:
- qa-functional code must not import external jars (NetBeans classloader issue JIRA-6954).
- The images come from `getTestDataFiles`.

---

## 18. The regression harness (golden DB dump)

Sources:
- `test/script/regression.py` (2227 lines), `regression_utils.py`, `config.xml`, `getcounts.py`, `Emailer.py`
- `test/README.txt`
- `Testing/src/org/sleuthkit/autopsy/testing/AutopsyTestCases.java`, `Testing/test/qa-functional/src/org/sleuthkit/autopsy/testing/RegressionTest.java`, `Testing/build.xml`
- `docs/doxygen-dev/regression_testing.dox`

**The dumper itself is not in the Autopsy repo.** It lives in sleuthkit's `db_diff/tskdbdiff.py`, imported from `$TSK_HOME/db_diff`. The description below follows the current upstream version. The `.dox` page, which shows `INSERT INTO "tsk_files" VALUES(...)` lines, describes an older `iterdump()`-based version and is out of date.

### 18.1 How Autopsy runs it

1. **Driver.**
   - `python3 regression.py [-f IMAGE | -l config.xml] [-r rebuild | -b compare then rebuild] [-k keep Solr index] [-o DIR collect diffs] [-e STR grep logs] [-v]`.
   - Windows or Cygwin only. Needs `cygpath`, `sort`, `diff`, `ant` and `%TMP%`.
   - `-u` (ignore unallocated space) has **no effect**: it is never forwarded.
   - Exit code 1 on any failure.
2. **config.xml.**
   - Each setting is an element with a `value` attribute.
   - Elements: `userCaseType` (Single / Multi / Both; default Both), `indir`, `singleUser_outdir`, `singleUser_golddir` (default `..\output\gold\single_user`), `multiUser_outdir`, `multiUser_golddir`, `timing`, `autopsyPlatform`, `build` (path to `Testing/`), the `db*`/`solr*`/`messageService*`/`cr*`/`zooKeeper*` connection settings, and repeatable `image` elements.
   - The input directory is never scanned; only `image` elements and `-f` are used.
   - Image type is decided by extension: a directory is logical files; `.img`/`.dd` is raw; `.e01` is EnCase; `.aa`/`.001` is split.
   - Every output is keyed by the image basename without its extension.
3. **Per image.**
   - Run `ant -f Testing/build.xml regression-test` with `-Dimg_path`, `-Dknown_bad_path=../input/notablehashes.txt-md5.idx`, `-Dnsrl_path=../input/nsrl.txt-md5.idx`, `-Dkeyword_path=../input/notablekeywords.xml`, `-Dgold_path`, `-Dout_path=<out>/<image>` and the connection properties.
   - A forked JUnit run executes the Jemmy GUI script.
4. **GUI script** (`RegressionTest` → `AutopsyTestCases`), in order:
   1. *(multi-user only)* configure a Postgres CR.
   2. New case: name **`AutopsyTestCase`**, base directory = out path, number **`000`**, examiner **`Examiner 1`**.
   3. Wait 120 s.
   4. Add data source: an image file, with timezone **`(GMT-5:00) America/New_York`** and other options at their defaults; or a logical files directory.
   5. Ingest settings → Hash Lookup → Global Settings: import `NSRL.txt-md5.idx`, then `notablehashes.txt-md5.idx`. **Both are imported as Notable**, because that is the dialog default.
   6. Keyword Search → Global Settings: import `notablekeywords.xml`.
   7. Finish, then wait while ingest is running.
   8. Sleep a random 15–30 s.
   9. Expand the Data Sources tree.
   10. Generate Report → the first report module (HTML) → all data sources → wait for "Complete".
   - On a Jemmy timeout: log, run `wmic` diagnostics, take `TimeoutScreenshot.png`. The JUnit run still "passes".
   - **Enabled ingest modules:** the defaults. In 4.23 Plaso, Keyword Search and Malware Scan are disabled by default, and the script only *selects* rows (it never ticks checkboxes). **So Keyword Search apparently does not run, even though a list is imported.** That is a latent harness bug.
5. **Post-processing.**
   - Scan `antlog.txt` for `BUILD FAILED`, `fatal error` or `crashed`; any of them is fatal.
   - Require `<out>/<image>/AutopsyTestCase/autopsy.db`.
   - Dump it to `<image>-DBDump.txt` and `<image>-BlackboardDump.txt`.
   - Copy the logs.
   - Build the "common SEVERE log": every log line that starts with `SEVERE`, prefixed with the file name, the output-directory prefix replaced by `test_data`, then sorted.
   - Gather log statistics: exceptions, OOMs, TskCore/TskData exceptions, ingest time, indexed file and chunk counts.
   - Delete the Solr index (unless `-k`).
6. **Compare.**
   - Unzip `<gold>/<image>-archive.zip`. It holds `autopsy.db` and the HTML `Report/`, but **neither is actually compared**.
   - Re-dump the output DB.
   - Re-sort the gold dumps with the local `sort`.
   - Require **exact equality** of DBDump **and** BlackboardDump **and** the SEVERE log with every digit replaced by `d`.
   - On mismatch, write `DBDump-Diff.txt` / `BlackboardDump-Diff.txt` (plain `diff gold new`) and `Gold-*.txt` copies.
   - The HTML report diff is commented out.
   - The timing check (gold `-Time.txt`, 5% tolerance) is printed but **does not affect pass/fail**.
7. **Rebuild (`-r`).** Write `<gold>/<image>-archive.zip`, `-DBDump.txt`, `-BlackboardDump.txt`, `-Exceptions.txt` and `-Time.txt`. An image is tested only if `<image>-archive.zip` exists.
8. **Reports.**
   - `AutopsyTestCase.html` with per-image statistics.
   - A pipe-delimited `Global_CSV.log`. **(bug)** It is shifted by one column against its header.
   - `Emailer.py` is never called.
   - `getcounts.py` is a standalone helper: for each `.db` it prints `count(*)` of `tsk_objects`, `blackboard_artifacts` and `blackboard_attributes`.

### 18.2 The dump format (tskdbdiff)

**Stable object keys.** Every object id is replaced by a content-derived key (`TskGuidUtils`):

| Object | Key |
|---|---|
| file | `parent_path + name` (after path normalisers) |
| vs part | `addr_start` |
| vs info | `vstype_offset` |
| fs | `offset_fstype` |
| image | basename of the first segment |
| OS account | `addr` (the SID) |
| report | path |
| artefact | `<parent key>/<TSK_TYPE_NAME>` (**not unique**: several artefacts of one type on one file share a key, so the comparison is effectively a multiset) |

**Path normalisers:**
- `Unalloc_[0-9]+_` → `Unalloc_`
- `regripper-[0-9]+-full` → `regripper-full`
- module-output paths are trimmed to start at `ModuleOutput`
- `ModuleOutput/EFE/<objid>/` → `ModuleOutput/EFE/`
- BulkExtractor and Smirk timestamp folders are dropped
- aLEAPP/iLEAPP index → `ModuleOutput/<aleapp|ileapp>/index.html`
- `Reports/AutopsyTestCase HTML Report*` → `Reports/AutopsyTestCase HTML Report`
- `…report.html` → `Reports/html-report.html`

**DBDump.txt (non-blackboard tables).**
1. Every table's schema SQL, flattened to one line per statement (SQLite `sqlite_master` ordered by type DESC, tbl_name; Postgres `pg_dump --schema-only`, sanitised). **Any schema change shows as a diff.**
2. Every row of every table except `blackboard_artifacts`, `blackboard_attributes` and `image_gallery_groups_seen`, one line per row:

```
tsk_files: {obj_id: 'MASKED_OBJ_ID', fs_obj_id: 4, data_source_obj_id: 'small2.img', …, parent_path: '/', mime_type: 'text/plain', extension: 'txt', …}
```

- Columns appear in ordinal order. NULL is written as `NULL`. Strings are single-quoted, with `'` doubled and `\n` escaped as `\\n`. Numbers use Python `str()`.
- Then the whole file is **sorted line-wise** (external, locale-dependent `sort`).

Per-table masks:

| Table | Normalisation |
|---|---|
| `data_source_info` | `device_id` → `{device id}`, `added_date_time` → `{dateTime}` |
| `ingest_jobs` | `host_name` → `{host_name}`; start and end → `MASKED_TIME` only if start ≤ end, so an unfinished job shows as a diff |
| `reports` | `obj_id` → `MASKED_OBJ_ID`, `path` → `AutopsyTestCase`, `crtime` → `MASKED_TIME` |
| `tsk_files` | `obj_id` → `MASKED_OBJ_ID`; `os_account_obj_id` → `MASKED_OS_ACCOUNT_OBJ_ID`; `data_source_obj_id` → image key; `parent_path` and `name` normalised; a TIFF extracted from a PDF gets `SIZE_IGNORED` / `MD5_IGNORED` / `SHA256_IGNORED`. Everything else is raw (timestamps, md5, mime, known, `fs_obj_id`). |
| `tsk_files_path` | the path is made `ModuleOutput`-relative with the EFE id removed; `obj_id` → file key |
| `tsk_objects` | `obj_id` and `par_obj_id` → keys, or `MASKED_OBJ_ID` / `MASKED_PARENT_OBJ_ID` |
| `tsk_file_layout` | `obj_id` → file key |
| `tsk_image_names` | `name` → basename |
| `tsk_vs_parts`, `tsk_os_accounts` | ids masked |
| `tsk_os_account_attributes`, `tsk_os_account_instances` | `id` → `MASKED_ID`; account and source ids → keys |
| `tsk_data_artifacts`, `tsk_analysis_results`, `tsk_aggregate_score` | object ids → keys, or `'… Object ID Omitted'` |
| `tsk_events` | `event_id` → `MASKED_EVENT_ID`, `event_description_id` → `ID OMITTED` |
| `tsk_event_descriptions` | ids masked; `artifact_id` → artefact type name; when full = med = short descriptions and match `^\s*(.+?)\s*:\s*\d+\s*$`, they become `<name>:<artifact_id>` |
| `tsk_examiners` | `login_name` → `{examiner_name}` |
| `image_gallery_groups` | `group_id` → `MASKED_ID`, data source → key |

Tables dumped **raw**: `tsk_db_info*` (schema versions), `tsk_image_info` (tz `America/New_York`, size, hashes), `tsk_vs_info`, `tsk_fs_info`, type tables, tags, accounts, hosts and persons, `tsk_file_attributes`, and `ingest_modules` (including **module version strings**, so every version bump is a diff).

**BlackboardDump.txt.** One line per artefact *whose parent is a `tsk_files` row* (an inner join; artefacts on images, OS accounts, reports or other artefacts are silently omitted):

```
/dir/file.txt <artifact type="Hashset Hit" > <attribute source="Hash Lookup" type="Hash Set Name" value="notablehashes.txt-md5" /><attribute …/> <artifact/>
```

- The name has `^Unalloc_[0-9]+_` normalised. The artefact type uses its **display name**.
- Attributes are ordered by `source, attribute display_name, value_type, value_text, value_int32, value_int64, value_double`.
- Values by type:
  - STRING: as is
  - INT: as is
  - LONG: as is, except **TSK_PATH_ID (attribute type 36)**, which is resolved to the object's path key
  - DOUBLE: `"%20.10f"` (fixed 10 decimals, left-padded to width 20)
  - BYTE: the literal word `bytes`
  - DATETIME: epoch seconds
  - JSON: as is
- "Associated Artifact" values are rendered as `File path: <parent_path> Artifact Type: <display name>`.
- Characters `\n \0 \a \b \r \f` become spaces.
- Keyword Search "Keyword Preview" values become `<Keyword Preview placeholder>`.
- Nothing is XML-escaped. Artefact ids and review status are not printed.
- The file is then sorted.

### 18.3 Postmortem golden-dump harness: requirements derived from the above

Keep:
- A **headless** run: `postmortem-cli ingest --case <tmp> --image <img> --profile regression`, then `postmortem-cli dump --case <tmp> --out <dir>`.
- Content-keyed object identities.
- Per-table normalisation rules, declared in one table-driven place, the equivalent of `TABLE_NORMALIZATIONS`.
- Sorted, line-oriented, diff-friendly text dumps.
- A separate artefact/blackboard dump with typed value rendering.
- A digit-masked "errors/warnings" log comparison.
- Compare, rebuild, and compare-then-rebuild modes.
- A one-image `-f` mode.
- Diff output plus a copy of the gold file on failure.

Do better than Autopsy:
1. **Sort in-process and byte-wise.** Do not depend on locale `sort`. Sort the gold file on write, and never re-sort it at compare time.
2. **Give every object a unique stable key**: `<parent key>/<kind>:<name or type>#<ordinal among same-key siblings, by a deterministic content order>`. Artefacts on non-file parents (data source, OS account, other artefacts, reports) are included.
3. **Normalise everything nondeterministic**:
   - all surrogate ids, including fs, vs and image ids, tag ids, account ids and host ids
   - absolute paths: relative to the case or evidence root, with `/` separators on every platform
   - wall-clock timestamps: case creation, ingest start and end, report time, examiner and host names
   - tool and module version strings, moved into a separate `versions.txt` that is allowed to differ
4. **Render timestamps** as ISO-8601 UTC alongside epoch seconds, matching ADR-005. Render doubles with a fixed format (`{:.10}`) and bytes as hex. Escape control characters instead of turning them into spaces, so data is not lost.
5. **Schema snapshot**: one normalised statement per line, so schema migrations show up deliberately.
6. **Pass/fail**: exact equality of the dumps plus the error log. Timing is reported but never gates. Store golds per fixture under `tests/golden/<fixture>/{db.txt, blackboard.txt, errors.txt}`. Use `insta`-style review (`cargo insta review`) or a `PM_UPDATE_GOLDEN=1` rebuild switch.
7. **Fixtures**: small synthetic images built by `pm-testkit` that run on every `cargo test`, plus an opt-in `slow-tests` corpus of real images by hash, never committed. Make the regression profile explicit, which Autopsy leaves implicit:
   - timezone America/New_York, matching Autopsy's harness
   - hash sets `test/input/*.idx`, both Notable
   - keyword list `notable_keywords`, with **keyword search enabled**
   - the default module set
   - an HTML report
8. Port **`getcounts`** as a `dump --counts` summary (objects, artefacts and attributes per type). It is useful as a quick first-line check.
9. **Optional cross-check against Autopsy**: an importer, or a "TSK-schema view" exporter, lets the Autopsy dumper run on postmortem output. ADR-002 says the schema is our own, so this is only an aid for spot checks, not a gate.

---

## 19. Gaps: important behaviour Autopsy never tests (derive tests from source)

These have no Autopsy test. When porting each area, derive cases from the production code and the other inventory files, and add a checklist line for each.

| Area | Derive from (Autopsy source) | Suggested oracle |
|---|---|---|
| File type detection (MIME) | `Core/src/…/modules/filetypeid/` (Tika, custom signatures, `FileTypeDetector`) | Magic-byte table tests: one per supported type, plus user-defined signatures (offset, relative-to-end) |
| Hash DB formats and lookup | `Core/src/…/modules/hashdatabase/` (TSK idx, NSRL, EnCase `.hash`, `.kdb`, md5sum text) | The `test/input` idx files (section 6) plus generated files per format |
| Interesting-files rules (full) | `modules/interestingitems/FilesSet*.java`, `InterestingItemsFilesSetSettings` XML | Each condition type (name or extension, regex variant, path, MIME, size ranges, date, meta-type), exclusive rules, and XML import/export round-trip |
| File extension mismatch | `modules/fileextmismatch/` | MIME vs extension table cases |
| Keyword search regexes, literal and substring matching | `KeywordSearch/src/…/KeywordSearchList.java` (built-in lists), `RegexQuery`, `LuceneQuery`, `KeywordSearchUtil.escapeLuceneQuery` | Positive and negative strings per built-in list (phone, IPv4/IPv6, email, URL, CCN, with Luhn via section 9) |
| Timeline events and filters | `Core/src/…/timeline/` (filter state, zoom, event types) | Filter-combination unit tests on a synthetic event set |
| Recent Activity parsers (browsers, registry, LNK, prefetch, recycle bin, …) | `RecentActivity/src/…` | Generated SQLite/registry fixtures; RegRipper outputs |
| Email (mbox / PST / EML) | `thunderbirdparser/` | Generated mbox and EML; threading |
| EXIF, carving, VM extractor, data-source integrity (hash verification) | Their `modules/*` directories | Generated JPEG with EXIF; E01 with stored MD5 |
| Tags, reports (HTML/Excel/KML/STIX/portable case) | `Core/src/…/report/` | Golden report snapshots (normalised) |
| Image Gallery grouping, Geolocation, Communications | `ImageGallery/`, `geolocation/`, `communications/` | Grouping and order unit tests |
| Filesystems and volume systems | TSK (C), so not tested in Autopsy | `pm-testkit` builders plus fuzzing (already in the plan) |

---

## 20. Parity checklist

Tick each line when an equivalent postmortem test exists and passes. Lines marked (derive) have no Autopsy test; they come from section 19 or from untested production rules.

### Core primitives and time
- [ ] port: pasco2 TestFileTime.testHexLittleEndian — FILETIME LE hex vectors (4) → ISO-8601 UTC
- [ ] port: pasco2 TestFileTime.testHexBigEndian — FILETIME BE hex vectors (3)
- [ ] port: pasco2 TestFileTime.testPassingInts — FILETIME from two u32 halves → 2005-10-17T02:59:22.147Z
- [ ] port: pasco2 TestFileTime.testFileTime — low −543274336 / high 29744990 → **2005-11-02T03:38:22.154Z** (correct value; pasco2's signed-low bug gives 03:31:12.657)
- [ ] port: pasco2 TestFileTime.testBitStuff + TestReadFunctions — LE i32 decoding vectors (1068610368, 2621440, 102400)
- [ ] port: pasco2 TestDOSTime — DOS date/time vectors (F788F41E, B67A3F28, 41B14C2B, (13404,1075))
- [ ] port: pasco2 TestPlatform — FILETIME 0 = 1601-01-01; Unix epoch = 0x019DB1DED53E8000; 2006-02-21T04:35:17.625Z round-trip

### Ingest pipeline and file filters
- [ ] port: IngestFileFiltersTest.testBasicDir — parent-path rule; only dir1 files processed
- [ ] port: IngestFileFiltersTest.testExtAndDirWithOneRule — AND of extension + path in one rule
- [ ] port: IngestFileFiltersTest.testExtAndDirWithTwoRules — OR across rules
- [ ] port: IngestFileFiltersTest.testFullFileNameRule — full-name rule (case-insensitive)
- [ ] port: IngestFileFiltersTest.testCarvingWithExtRuleAndUnallocSpace — unallocated space included; carved files filtered; Unalloc_* processed
- [x] port: IngestFileFiltersTest.testCarvingNoUnallocatedSpace — carver start-up error when unallocated space is excluded (exactly 1 error, module "PhotoRec Carver")
- [ ] port: IngestFileFiltersTest.testEmbeddedJpg — extracted children obey the filter; derived/local directories typed
- [x] port: FilesSet condition semantics — ParentPath substring (case-insensitive, `parent + "/"`), Extension exact (leading dot stripped), FullName exact, meta-type FILES, ignoreUnallocated rejects UNALLOC/SLACK/UNUSED
- [ ] port: ingest scheduler — `.`/`..` never processed; directories bypass the filter
- [x] decide: directory MIME-type semantics (section 3 inconsistency), with an ADR (ADR-019: directories get none)

### Embedded file extraction
- [x] port: EmbeddedFileTest.runTestEncryptionAndZipBomb — protected zips → TSK_ENCRYPTION_DETECTED; depth bomb (> MAX_DEPTH 4) and ratio bomb (≥ 600) → TSK_INTERESTING_ITEM set "Possible Zip Bomb"; other files no artefacts
- [x] port: EmbeddedFileTest.runTestExtension — zip named .txt is extracted
- [x] port: EmbeddedFileTest.runTestBigFolder — 1000 extracted files, names file1..file1000.txt, MD5 098f6bcd4621d373cade4e832627b4f6
- [x] port: EmbeddedFileTest.runTestDeepFolder — 25-deep directories inside an archive; name-ordered child listing
- [x] port: EmbeddedFileTest.runTestEmbeddedFile — nested zips, paths `/<parent>/<archive>/<entry>`
- [x] port: EmbeddedFileTest.runTestContent — all extractable .txt hashed; protected entries not extracted or hashed

### Encryption detection
- [x] port: EncryptionDetectionTest.testBitlockerEncryption — BitLocker volume → TSK_ENCRYPTION_DETECTED, comment exactly "Bitlocker encryption detected."; other volumes none
- [x] port: EncryptionDetectionTest.testPasswordProtection — protected doc/xls/ppt/docx/xlsx/pptx/pdf/mdb/accdb (10 files) → DETECTED; unprotected → none
- [x] port: EncryptionDetectionTest.testVeraCryptSupport — unformatted high-entropy volume and ≥ 5 MiB container → TSK_ENCRYPTION_SUSPECTED
- [x] port: EncryptionDetectionTest.testSqlCipherEncryption — SQLCipher DBs → SUSPECTED; plain SQLite → none
- [x] port: encryption settings defaults — entropy 7.5, minimum size 5,242,880 B, 512-multiple, slack allowed

### Hash lookup and keyword-list inputs
- [ ] port (derive): parse `test/input/NSRL.txt-md5.idx` and `notablehashes.txt-md5.idx` — headers, 4 entries each, case-insensitive lookup, miss
- [ ] port (derive): import `test/input/notablekeywords.xml` — BOM, list name/flags/timestamps, 10 literal terms incl. Arabic; export round-trip

### Central repository
- [ ] port: CorrelationAttributeNormalizerTest.testValidateMd5 — 5 cases
- [ ] port: CorrelationAttributeNormalizerTest.testValidateDomain — 11 cases (URLs rejected, PSL registrable domain, lower-case)
- [ ] port: CorrelationAttributeNormalizerTest.testValidateEmail — 7 cases (single-label domain allowed)
- [ ] port: CorrelationAttributeNormalizerTest.testValidatePhone — 9 cases (+ kept, separators stripped, ≥ 5 chars)
- [ ] port: CorrelationAttributeNormalizerTest.testValidateUsbId — unchanged, not lower-cased
- [ ] port (derive): normaliser SSID / MAC / IMEI / IMSI / ICCID / passthrough types (section 7.1 table)
- [ ] port: CR error-kind parity — null → CR error vs bad format → normalisation error
- [ ] port: CentralRepoDatamodelTest.testNotableArtifactStatus — known/notable status, upsert on set-status, known-bad counts and case lists
- [ ] port: CentralRepoDatamodelTest.testBulkArtifacts — bulk queue, auto-flush at 1000, commit, error-at-commit cases
- [ ] port: CentralRepoDatamodelTest.testArtifacts — add/query/count/frequency (100/33/0), comment update, per-data-source count 7, tuples, processInstanceTable(Where), duplicate-ignore
- [ ] port: CentralRepoDatamodelTest.testCorrelationTypes — custom type CRUD; defined 30, enabled 29, supported 29; table-name pattern
- [ ] port: CentralRepoDatamodelTest.testOrganizations — CRUD, 5 orgs, delete blocked when in use
- [ ] port: CentralRepoDatamodelTest.testReferenceSetInstances — reference instances, bulk 1500 in one transaction, membership and known-bad lookups
- [ ] port: CentralRepoDatamodelTest.testReferenceSets — create/validate/exists/get/delete, empty version allowed
- [ ] port: CentralRepoDatamodelTest.testDataSources — duplicate returns existing; lookups; count 5
- [ ] port: CentralRepoDatamodelTest.testCases — duplicate uuid returns existing; update round-trip; bulk 1500; current-case mapping
- [ ] port: CentralRepoDatamodelTest.testDbInfo — insert/get/update/null semantics
- [ ] port: CR default content — 12 base types, 1000+ account types (verify the TSK predefined list), "Not Specified" org, confidence and persona_status rows, FK enforcement
- [ ] port: CentralRepoAccountsTest.testPredefinedAccountTypes — all predefined types except DEVICE present
- [ ] port: CentralRepoAccountsTest.testRejectionOfDeviceAccountType — DEVICE absent
- [ ] port: CentralRepoAccountsTest.testNonExistentAccountType — unknown → None
- [ ] port: CentralRepoAccountsTest.testCreatingAccount — create FB account
- [ ] port: CentralRepoAccountsTest.testRetreivingAnAccount — idempotent getOrCreate; non-phone types only lower-cased
- [ ] port: CentralRepoPersonasTest.testBasicPersonaCreation — persona, aliases, metadata, accounts, shared account, remove account
- [ ] port: CentralRepoPersonasTest.testPersonaAliasesAndMetadata — add/remove alias and metadata counts
- [ ] port: CentralRepoPersonasTest.testPersonaWithCases — persona → cases/data sources; personas per case/data source counts
- [ ] port: CentralRepoPersonasTest.testPersonaCreationEdgeCases — null name → "Unnamed"
- [ ] port: CentralRepoPersonasTest.testPersonaSearchByName — LIKE search counts (3/2/2/3/1/4)
- [ ] port: CentralRepoPersonasTest.testPersonaSearchByAccountIdentifier — identifier LIKE counts (2/2/2/4/2/4/6)
- [ ] port: CentralRepoPersonasTest.testAccountSearchByAccountIdentifier — account listing and LIKE/exact searches
- [ ] port: CentralRepoPersonasTest.testExaminers — empty login and quote handling
- [ ] port (derive): DELETED personas excluded from every listing and search
- [ ] port (derive): DatabaseTestResult status mapping for the CR connection test

### Common properties search
- [ ] port: IngestedWithHashAndFileTypeIntraCaseTest.testOneA/B/C — all-sources search, no/media/doc filters
- [ ] port: IngestedWithHashAndFileTypeIntraCaseTest.testTwoA/B/C — single-source SET1 with filters
- [ ] port: IngestedWithHashAndFileTypeIntraCaseTest.testThree/testFour/testFive — single-source SET2/SET4/SET3 (fix the testFour DOC/SET2 omission)
- [ ] port: IngestedWithNoFileTypesIntraCaseTest.testOne/testTwo — MIME filter without MIME data → empty
- [ ] port: MatchesInAtLeastTwoSourcesIntraCaseTest.testOne — duplicates within one data source don't match
- [ ] port: UningestedCasesIntraCaseTest.testOne/testTwo — no hashes → no results
- [ ] port: empty-file MD5 and known files excluded from common-property matches
- [ ] port: IngestedWithHashAndFileTypeInterCaseTest.testOne/testTwo — inter-case counts table (enable; Autopsy's is commented out)
- [ ] port: IngestedWithHashAndFileTypeInterCaseTest.testThree — frequency threshold 50 leaves only Hash-D.doc
- [ ] port: CommonAttributeSearchInterCaseTest.testOne — results per correlation type have that type
- [ ] port: CommonAttributeSearchInterCaseTest.testTwo — USB thresholds (100 → 13, 20 → 0, 90 → 2; only if the synthetic fixture reproduces them)

### Keyword search
- [ ] port: CreditCardValidatorTest.testLengthMatchesBin — IIN length table (Amex, Verve, Visa, Visa Electron vectors)
- [ ] port: CreditCardValidatorTest.testIsValidCCN12..19 (8 methods) — grouping, separators, Luhn vectors
- [ ] port: CCN IIN range-map "last insert wins" semantics; decide on the `29` typo (ADR)
- [ ] port: MiniChunkHelperTest.isMiniChunkID / getBaseChunkID — if the chunk-ID scheme is kept
- [ ] port: LanguageSpecificContentQueryHelperTest.makeQueryString / findNthIndexOf / buildTermfreqQuery — semantics we keep (multi-field OR; n-th occurrence)
- [ ] port (derive): built-in regex lists (phone, IP, email, URL, CCN) positive and negative strings
- [ ] port (derive): literal/substring/regex matching and case options

### Data source summary
- [ ] port: DataSourceInfoUtilitiesTest.getArtifacts_sortAscending / _sortDescending — string (case-insensitive), int, long, datetime, double
- [ ] port: DataSourceInfoUtilitiesTest.getArtifacts_limits — 0 = unlimited; 3/5/10
- [ ] port: DataSourceInfoUtilitiesTest.getArtifacts_failOnJson / _failOnBytes — unsortable types → error
- [ ] port: DataSourceInfoUtilitiesTest.getArtifacts_purgeAttrNotPresent / _multipleAttrsPresent
- [ ] port: DataSourceInfoUtilitiesTest.getArtifacts_tskCoreExceptionThrown / _throwOnLessThan0 / _emptyListReturned
- [ ] port: DataSourceInfoUtilitiesTest.get{String,Int,Long,Date}OrNull_handlesNull
- [ ] port: RecentFilesSummaryTest non-positive count / no data source / no results (×3 methods)
- [ ] port: RecentFilesSummaryTest.getRecentlyOpenedDocuments_sortedByDateTimeAndLimited / _uniquePaths / _filtersMissingData
- [ ] port: RecentFilesSummaryTest.getRecentDownloads_sortedByDateTimeAndLimited / _uniquePaths / _filtersMissingData
- [ ] port: RecentFilesSummaryTest.getRecentAttachments_sortedByDateTimeAndLimited / _filterData / _uniquePath
- [ ] port: UserActivitySummaryTest.testMinCountInvariant / testNullDataSource / testNoResultsReturned (5 methods each)
- [ ] port: UserActivitySummaryTest.getRecentDevices_appropriateFiltering / _limitedToCount / _uniqueByDeviceId (ROOT_HUB, ROOT_HUB20 excluded)
- [ ] port: UserActivitySummaryTest.getMostRecentWebSearches_grouping / _handlesTranslation / _limitedToCount
- [ ] port: UserActivitySummaryTest.getRecentDomains_withinTimeWIndow / _appropriatelyFiltered / _groupedAppropriately / _limitedAppropriately (30-day inclusive window, localhost/127.0.0.1 excluded)
- [ ] port: UserActivitySummaryTest.getRecentAccounts_filtersNoDate / _rightGrouping / _rightLimit ("Email Message", "Call Log", message type labels)
- [ ] port: UserActivitySummaryTest.getShortFolderName_rightConversions — 6 paths × 3 casings
- [ ] port: UserActivitySummaryTest.getTopPrograms_filtered / _correctGrouping / _correctOrdering / _limited (ntosboot, /WINDOWS, blank names; max count; unlimited alphabetical fallback)
- [ ] port: UserActivitySummaryTests.getRecentDomainsTest (integration) — golden output of recent domains per data source, with deterministic tie order

### Discovery
- [ ] port: DomainSearchTest.groupSizes_* (3) — group sizes 4 / {4,3,1} / empty
- [ ] port: DomainSearchTest.getDomains_* (9) — paging (offset, size, oversized, zero, last item, multi-group, simulated paging)
- [ ] port: DomainSearchCacheLoaderTest (4) — group by data source/none × sort by name/size × results by domain/data source
- [ ] port: DomainSearchArtifactsCacheTest (7) — non-web type error, cancellation, TSK_DOMAIN match (trim + lower-case), URL ignored

### UI settings, translation, YARA
- [ ] port: ExternalViewerGlobalSettingsTableModelTest (13) — rule list model (add/get/set/remove/contains, columns)
- [ ] port: ExternalViewerGlobalSettingsPanelTest (5) — button enablement, delete, single selection
- [ ] port (derive): translation service contract — offline request and response parsing (Autopsy's tests are empty)
- [ ] port: YaraWrapperTest — `HelloWorld` rule matches `Hello World` (memory and file scan), no match otherwise, timeout honoured

### Recent activity (IE index.dat)
- [ ] port: pasco2 TestRecordLayer — empty cache/history → 0 records; "first" cache → 30, history → 2 (own fixtures; verify counts)
- [ ] port: pasco2 TestAllocationLayer — allocation bitmap at 0x250 matches record allocation (property test)

### Test-support parity
- [ ] port: CustomArtifactsCreator — custom artefact/attribute types of every value type; duplicate attribute source is a no-op
- [ ] port: TestAutopsyService — case-open hook cancellation
- [ ] port: IngestJobRunner semantics — start-up errors returned, not thrown

### Regression / golden-dump harness
- [ ] build: `postmortem dump` — schema lines plus normalised per-table rows, byte-sorted
- [ ] build: blackboard dump — one line per artefact, typed value rendering, all parents (not only files)
- [ ] build: stable unique object keys (path-based plus sibling ordinal) and the normaliser table (Unalloc_, EFE ids, module-output-relative paths, masked times, hosts, examiners, versions)
- [ ] build: error-log capture with digit masking
- [ ] build: compare / rebuild / compare-then-rebuild modes, unified diff plus gold copy on failure, `--counts` summary
- [ ] build: regression profile (America/New_York, both idx hash sets Notable, notable_keywords with keyword search enabled, default modules, HTML report)
- [ ] build: synthetic per-commit golden fixtures, plus an opt-in `slow-tests` real-image corpus
- [ ] build: integration-test style YAML outputs for summary queries (UserActivitySummaryTests equivalent) diffed against gold

### Derived (section 19)
- [x] derive: file-type detection signature table tests (pm-filetype unit tests + 10,861-file differential run against Tika 3.0.0)
- [ ] derive: hash DB formats (TSK idx, NSRL, EnCase, md5sum text) parse and lookup
- [x] derive: interesting-files rules — every condition type, exclusive rules, XML round-trip
- [x] derive: file extension mismatch table (pm-modules ext_mismatch unit + end-to-end tests)
- [ ] derive: timeline filter combinations
- [ ] derive: Recent Activity, email, EXIF, carving, VM extractor, data-source integrity, reports, tags (per-module inventories)
