# Test fixtures

Binary fixtures are kept small and are reproducible from the scripts in
`scripts/fixtures/`. Prefer generating fixtures in Rust test code
(`pm-testkit`) where an independent writer exists.

| File | Size | Produced by | Notes |
|------|------|-------------|-------|
| `ewf/small.E01` | 71 kB | `scripts/fixtures/make-raw.py src.raw 256` then `ewfacquire 20140817 -u -q -t small -c deflate:best -C CASE-42 -D "pm fixture" -e "Examiner Ä" -E EV-7 -N "notes here" -d sha1 src.raw` | EnCase 6 format, 8 chunks (compressed and stored), header2 with non-ASCII examiner; MD5 `b7d9562dc88251034f015fcb757c7ec7`, SHA-1 `9c6a5079404c0af11ca2a0cc382541de694bd1dd` |
| `ewf/small.L01` | 9 kB | `make-l01.py --media > media.raw`, `ewfacquire 20140814 -u -q -t small -c none -d sha1 media.raw`, then `scripts/fixtures/make-l01.py small.E01 small.L01` | EWF-L01: `LVF` signature plus an `ltree` section listing `docs/a.txt` (with times and MD5), `docs/Ääkköset.txt` (empty) and `b.bin` (512 bytes at 0x200); libewf 20140814 `ewfexport -f files` extracts the same bytes |

## evtx/

Sample Windows event logs from the `evtx` crate's repository
(https://github.com/omerbenamram/evtx, `samples/`, commit 7479d02; MIT OR Apache-2.0):
`new-user-security.evtx` and `Security_short_selected.evtx` (70 kB each).
Used by `pm-evtlog` and the Windows Event Logs module tests.
| `recyclebin/win10_I2JRX90`, `win7_ID5HSV3.exe`, `xp_INFO2`, `xp_INFO2_MultiFiles` | 44 B – 3.2 kB | Copied from Eric Zimmerman's RBCmd test data (`RecycleBin.Test/TestFiles`, MIT licence) | Real `$I` (versions 2 and 1) and XP `INFO2` files; expected values in `pm-winartifacts/tests/analyses.rs` were computed independently from the raw bytes |

## encryption/

Encryption Detection fixtures (120 kB in all), produced with the libraries
Autopsy uses, all bundled in `tika-app-3.0.0.jar` (Apache-2.0):

* `pw.{doc,xls,ppt}` (password "secret", RC4 CryptoAPI), `default-pw.xls`
  (Excel's default password `VelvetSweatshop`), `plain.{doc,xls,ppt,docx}`,
  `pw-agile.docx` / `pw-standard.docx` (OOXML in an OLE2 container),
  `plain.pdf`, `owner-only.pdf` / `owner-only-aes-256.pdf` (empty user
  password), `pw-rc4-40.pdf`, `pw-rc4-128.pdf`, `pw-aes-128.pdf`,
  `pw-aes-256.pdf` (user password "secret"): written by Apache POI 5.3 and
  PDFBox 3.0.3 with `java -cp tika-app-3.0.0.jar
  scripts/fixtures/MakeEncrypted.java fixtures/encryption testWORD.doc`
  (the `.doc` files derive from Tika's `testWORD.doc`). The script also
  prints real Tika's verdict per file (`EncryptedDocumentException` or
  not), which `crates/pm-encryption/tests/oracle.rs` asserts.
* `head-testAccess2_encrypted.accdb`, `head-testAccess2.accdb`,
  `head-testAccess2_2000.mdb`, `head-testAccess_V1997.mdb`: the first
  8 KiB of Tika's test databases (`tika-parser-microsoft-module` test
  documents, Apache-2.0). Only the encrypted ACCDB is password protected
  for Autopsy's Jackcess code path (`scripts/fixtures/AccessOracle.java`
  on the full files).
