//! Password protection of legacy (OLE 2) Office documents (Word, Excel,
//! `PowerPoint`), decided the way Autopsy learns it: Tika's
//! `AutoDetectParser` throwing
//! `EncryptedDocumentException` while Apache POI 5.3 opens the document.
//!
//! POI reads the document's `EncryptionInfo` (XOR obfuscation, binary
//! RC4, RC4 `CryptoAPI` or AES "standard" encryption) and tries the default
//! password `VelvetSweatshop` (Excel's "password to modify" only). A
//! document is protected when that fails, or when POI throws its
//! `EncryptedDocumentException` for the format (unknown algorithms, an
//! invalid encryption type). Which POI exceptions Tika turns into
//! `EncryptedDocumentException` differs per format and is followed here;
//! anything else is a parse failure ([`CheckError`]), which Autopsy logs
//! without flagging the file.

use pm_core::ReadAt;
use pm_filetype::cfb::{Cfb, Entry};

use crate::CheckError;
use crate::crypto::{self, Hash};

/// `Decryptor.DEFAULT_PASSWORD`.
pub const DEFAULT_PASSWORD: &str = "VelvetSweatshop";
/// XOR obfuscation key and verifier of the default password
/// (`CryptoFunctions.createXorKey1` / `createXorVerifier1`).
const DEFAULT_XOR_KEY: u16 = 0xB359;
const DEFAULT_XOR_VERIFIER: u16 = 0x9A0A;
/// Standard encryption's password hash spin count.
const STANDARD_SPIN: u32 = 50_000;
/// Largest record or stream prefix read.
const MAX_RECORD: usize = 1 << 20;

/// The legacy Office formats Autopsy checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeKind {
    Word,
    Excel,
    PowerPoint,
}

impl OfficeKind {
    /// The kind for Autopsy's MIME types (`application/msword`,
    /// `application/vnd.ms-excel`, `application/vnd.ms-powerpoint`).
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "application/msword" => Some(Self::Word),
            "application/vnd.ms-excel" => Some(Self::Excel),
            "application/vnd.ms-powerpoint" => Some(Self::PowerPoint),
            _ => None,
        }
    }
}

/// Whether Tika/POI would report the document as encrypted.
pub fn is_password_protected(src: &dyn ReadAt, kind: OfficeKind) -> Result<bool, CheckError> {
    let cfb = Cfb::open(src).map_err(|e| CheckError::invalid(e.to_string()))?;
    match kind {
        OfficeKind::Word => word(&cfb),
        OfficeKind::Excel => excel(&cfb),
        OfficeKind::PowerPoint => powerpoint(&cfb),
    }
}

// ------------------------------------------------------------ EncryptionInfo

/// Why an `EncryptionInfo` could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InfoError {
    /// POI threw `EncryptedDocumentException`.
    Encrypted(&'static str),
    /// Any other exception (short data, bad salt size, …).
    Other(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cipher {
    Rc4,
    Aes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Info {
    Xor {
        key: u16,
        verifier: u16,
    },
    BinaryRc4 {
        salt: Vec<u8>,
        verifier: Vec<u8>,
        verifier_hash: Vec<u8>,
    },
    /// `CryptoAPI` or standard (AES) encryption.
    Header {
        standard: bool,
        cipher: Cipher,
        /// `None`: POI's "none" hash algorithm (no digest available).
        hash: Option<Hash>,
        key_bits: u32,
        salt: Vec<u8>,
        verifier: Vec<u8>,
        verifier_hash: Vec<u8>,
    },
    /// Agile (XML) encryption; never verified with the default password
    /// here (it does not occur in binary Office files).
    Agile,
}

/// Little-endian reader that fails like POI's `LittleEndianInput` past
/// the end.
struct Le<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Le<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], InfoError> {
        let s = self
            .b
            .get(self.pos..self.pos.saturating_add(n))
            .ok_or(InfoError::Other("unexpected end of data"))?;
        self.pos += n;
        Ok(s)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], InfoError> {
        self.bytes(N)?
            .try_into()
            .map_err(|_| InfoError::Other("unexpected end of data"))
    }

    fn u16(&mut self) -> Result<u16, InfoError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, InfoError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn peek_i32(&self) -> Result<i32, InfoError> {
        i32_at(self.b, self.pos).ok_or(InfoError::Other("unexpected end of data"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preferred {
    None,
    Xor,
    CryptoApi,
}

const FLAG_AES: i32 = 0x20;

/// `new EncryptionInfo(LittleEndianInput, preferredMode)`.
fn parse_info(r: &mut Le<'_>, preferred: Preferred) -> Result<Info, InfoError> {
    let (major, minor) = if preferred == Preferred::Xor {
        (0, 0)
    } else {
        (r.u16()?, r.u16()?)
    };
    if (major, minor) == (0, 0) {
        let key = r.u16()?;
        let verifier = r.u16()?;
        return Ok(Info::Xor { key, verifier });
    }
    if (major, minor) == (1, 1) {
        let salt = r.bytes(16)?.to_vec();
        let verifier = r.bytes(16)?.to_vec();
        let verifier_hash = r.bytes(16)?.to_vec();
        return Ok(Info::BinaryRc4 {
            salt,
            verifier,
            verifier_hash,
        });
    }
    if (2..=4).contains(&major) && minor == 2 {
        let flags = r.i32()?;
        let standard = preferred != Preferred::CryptoApi && flags & FLAG_AES != 0;
        return parse_header_info(r, standard);
    }
    if (major, minor) == (4, 4) {
        r.i32()?;
        return Ok(Info::Agile);
    }
    r.i32()?;
    Err(InfoError::Encrypted("unknown encryption"))
}

/// The `CryptoAPI` / standard header and verifier
/// (`StandardEncryptionHeader`, `StandardEncryptionVerifier`).
fn parse_header_info(r: &mut Le<'_>, standard: bool) -> Result<Info, InfoError> {
    let _header_size = r.i32()?;
    let _flags = r.i32()?;
    let _size_extra = r.i32()?;
    let cipher = match r.i32()? {
        0x6801 => Cipher::Rc4,
        0x660E..=0x6610 => Cipher::Aes,
        _ => return Err(InfoError::Encrypted("cipher algorithm not found")),
    };
    let hash = match r.i32()? {
        0x0000 => None,
        0x8004 => Some(Hash::Sha1),
        0x800C => Some(Hash::Sha256),
        0x800D => Some(Hash::Sha384),
        0x800E => Some(Hash::Sha512),
        -1 => Some(Hash::Md5),
        _ => return Err(InfoError::Encrypted("hash algorithm not found")),
    };
    let mut key_bits = r.i32()?;
    if key_bits == 0 {
        key_bits = 0x28;
    }
    match r.i32()? {
        1 | 0x18 => {}
        _ => return Err(InfoError::Encrypted("cipher provider not found")),
    }
    r.bytes(8)?;
    // CSP name, unless the salt size follows directly.
    if r.peek_i32()? != 16 {
        loop {
            if r.u16()? == 0 {
                break;
            }
        }
    }
    let salt_size = r.i32()?;
    if salt_size != 16 {
        return Err(InfoError::Other("salt size != 16"));
    }
    let salt = r.bytes(16)?.to_vec();
    let verifier = r.bytes(16)?.to_vec();
    let _verifier_hash_size = r.i32()?;
    let hash_len = match cipher {
        Cipher::Rc4 => 20,
        Cipher::Aes => 32,
    };
    let verifier_hash = r.bytes(hash_len)?.to_vec();
    Ok(Info::Header {
        standard,
        cipher,
        hash,
        key_bits: u32::try_from(key_bits).unwrap_or(0),
        salt,
        verifier,
        verifier_hash,
    })
}

/// `Decryptor.verifyPassword(DEFAULT_PASSWORD)`; an error is POI's
/// `EncryptedDocumentException` from inside the decryptor.
fn verifies_default_password(info: &Info) -> Result<bool, InfoError> {
    match info {
        Info::Xor { key, verifier } => {
            Ok(*key == DEFAULT_XOR_KEY && *verifier == DEFAULT_XOR_VERIFIER)
        }
        Info::BinaryRc4 {
            salt,
            verifier,
            verifier_hash,
        } => {
            let h0 = Hash::Md5.digest(&[&crypto::utf16le(DEFAULT_PASSWORD)]);
            let h5 = h0.get(..5).unwrap_or_default();
            let mut parts: Vec<&[u8]> = Vec::with_capacity(32);
            for _ in 0..16 {
                parts.push(h5);
                parts.push(salt);
            }
            let h1 = Hash::Md5.digest(&parts);
            let key = Hash::Md5.digest(&[h1.get(..5).unwrap_or_default(), &0u32.to_le_bytes()]);
            let plain = crypto::rc4(&key, &[verifier.as_slice(), verifier_hash].concat());
            let (v, vh) = plain.split_at(verifier.len().min(plain.len()));
            Ok(Hash::Md5.digest(&[v]) == vh)
        }
        Info::Header {
            standard: false,
            cipher,
            hash,
            key_bits,
            salt,
            verifier,
            verifier_hash,
        } => {
            let hash = hash.ok_or(InfoError::Encrypted("hash algorithm not supported"))?;
            let h = hash.digest(&[salt, &crypto::utf16le(DEFAULT_PASSWORD)]);
            let mut key = hash.digest(&[&h, &0u32.to_le_bytes()]);
            key.resize((*key_bits / 8) as usize, 0);
            if *key_bits == 40 {
                key.resize(16, 0);
            }
            let (v, vh) = match cipher {
                Cipher::Rc4 => {
                    let plain = crypto::rc4(&key, &[verifier.as_slice(), verifier_hash].concat());
                    let (a, b) = plain.split_at(verifier.len().min(plain.len()));
                    (a.to_vec(), b.to_vec())
                }
                Cipher::Aes => (
                    crypto::aes_ecb_decrypt(&key, verifier)
                        .ok_or(InfoError::Encrypted("invalid key"))?,
                    crypto::aes_ecb_decrypt(&key, verifier_hash)
                        .ok_or(InfoError::Encrypted("invalid key"))?,
                ),
            };
            Ok(hash.digest(&[&v]) == vh)
        }
        Info::Header {
            standard: true,
            cipher,
            hash,
            key_bits,
            salt,
            verifier,
            verifier_hash,
        } => {
            let hash = hash.ok_or(InfoError::Encrypted("hash algorithm not supported"))?;
            if *cipher == Cipher::Rc4 {
                return Err(InfoError::Encrypted("invalid cipher"));
            }
            // Password hash with 50 000 iterations (`hashPassword`).
            let mut h = hash.digest(&[salt, &crypto::utf16le(DEFAULT_PASSWORD)]);
            for i in 0..STANDARD_SPIN {
                h = hash.digest(&[&i.to_le_bytes(), &h]);
            }
            let fin = hash.digest(&[&h, &0u32.to_le_bytes()]);
            // `fillAndXor` always hashes with SHA-1.
            let fill = |x: u8| {
                let mut buf = vec![x; 64];
                for (b, f) in buf.iter_mut().zip(&fin) {
                    *b ^= f;
                }
                Hash::Sha1.digest(&[&buf])
            };
            let mut key = fill(0x36);
            key.extend(fill(0x5C));
            // The header's key size picks the AES variant (an invalid
            // size fails like Java's InvalidKeyException).
            key.resize((*key_bits / 8) as usize, 0);
            let v = crypto::aes_ecb_decrypt(&key, verifier)
                .ok_or(InfoError::Encrypted("invalid key"))?;
            let vh = crypto::aes_ecb_decrypt(&key, verifier_hash)
                .ok_or(InfoError::Encrypted("invalid key"))?;
            let calc = hash.digest(&[&v]);
            Ok(vh.get(..calc.len()) == Some(calc.as_slice()))
        }
        // Agile encryption needs an XML descriptor; a binary Office file
        // with it cannot be opened with the default password.
        Info::Agile => Ok(false),
    }
}

// --------------------------------------------------------------------- Word

/// `HWPFDocumentCore.getEncryptionInfo`: the FIB's `fEncrypted` flag,
/// then the `EncryptionInfo` at the start of the table stream.
fn word(cfb: &Cfb<'_>) -> Result<bool, CheckError> {
    let root = cfb.root().ok_or_else(|| CheckError::invalid("no root"))?;
    let main = child_ci(cfb, root, "WordDocument")
        .ok_or_else(|| CheckError::invalid("no WordDocument stream"))?;
    let fib = cfb
        .read_stream(main, 68)
        .map_err(|e| CheckError::invalid(e.to_string()))?;
    let flags = u16_at(&fib, 0x0A).ok_or_else(|| CheckError::invalid("short FIB"))?;
    let l_key = i32_at(&fib, 0x0E).ok_or_else(|| CheckError::invalid("short FIB"))?;
    if flags & 0x0100 == 0 {
        return Ok(false);
    }
    let table_name = if flags & 0x0200 != 0 {
        "1Table"
    } else {
        "0Table"
    };
    let table =
        child_ci(cfb, root, table_name).ok_or_else(|| CheckError::invalid("no table stream"))?;
    // `getDocumentEntryBytes(table, -1, lKey)`.
    let want = usize::try_from(l_key.max(0)).unwrap_or(0).min(MAX_RECORD);
    let data = cfb
        .read_stream(table, want)
        .map_err(|e| CheckError::invalid(e.to_string()))?;
    let preferred = if flags & 0x8000 != 0 {
        Preferred::Xor
    } else {
        Preferred::None
    };
    // Every POI EncryptedDocumentException is reported as encrypted.
    let info = match parse_info(&mut Le::new(&data), preferred) {
        Ok(i) => i,
        Err(InfoError::Encrypted(_)) => return Ok(true),
        Err(InfoError::Other(m)) => return Err(CheckError::invalid(m)),
    };
    match verifies_default_password(&info) {
        Ok(ok) => Ok(!ok),
        Err(InfoError::Encrypted(_)) => Ok(true),
        Err(InfoError::Other(m)) => Err(CheckError::invalid(m)),
    }
}

// -------------------------------------------------------------------- Excel

const SID_BOF: u16 = 0x0809;
const SID_WRITE_PROTECT: u16 = 0x0086;
const SID_FILE_PASS: u16 = 0x002F;
const SID_EOF: u16 = 0x000A;

/// HSSF's `RecordFactoryInputStream.StreamEncryptionInfo`: BOF, an
/// optional WRITEPROTECT, then FILEPASS.
fn excel(cfb: &Cfb<'_>) -> Result<bool, CheckError> {
    let root = cfb.root().ok_or_else(|| CheckError::invalid("no root"))?;
    // `HSSFWorkbook.getWorkbookDirEntryName`.
    let book = ["Workbook", "WORKBOOK", "BOOK"]
        .iter()
        .find_map(|n| cfb.children(root).into_iter().find(|e| e.name == *n));
    let Some(book) = book else {
        if cfb.root_child("EncryptedPackage").is_some() {
            return Ok(true);
        }
        return Err(CheckError::invalid("no workbook stream"));
    };
    if book.name == "BOOK" {
        // Excel 5/95: Tika's old-Excel extractor, no decryption.
        return Ok(false);
    }
    let head = cfb
        .read_stream(book, MAX_RECORD)
        .map_err(|e| CheckError::invalid(e.to_string()))?;
    let mut records = Records { b: &head, pos: 0 };
    let Some((sid, _)) = records.next() else {
        return Err(CheckError::invalid("empty workbook stream"));
    };
    if sid != SID_BOF {
        return Ok(false);
    }
    let Some((mut sid, mut body)) = records.next() else {
        return Ok(false);
    };
    if sid == SID_WRITE_PROTECT
        && let Some(next) = records.next()
    {
        (sid, body) = next;
    }
    if sid == SID_EOF {
        return Err(CheckError::invalid("nothing between BOF and EOF"));
    }
    if sid != SID_FILE_PASS {
        return Ok(false);
    }
    let mut r = Le::new(body);
    let enc_type = r.u16().map_err(|_| CheckError::invalid("short FILEPASS"))?;
    let preferred = match enc_type {
        0 => Preferred::Xor,
        1 => Preferred::CryptoApi,
        // "invalid encryption type".
        _ => return Ok(true),
    };
    // FilePassRecord wraps every parse problem in an
    // EncryptedDocumentException, except reads past the record.
    let info = match parse_info(&mut r, preferred) {
        Ok(i) => i,
        Err(InfoError::Encrypted(_)) => return Ok(true),
        Err(InfoError::Other(m)) => return Err(CheckError::invalid(m)),
    };
    match verifies_default_password(&info) {
        Ok(ok) => Ok(!ok),
        Err(InfoError::Encrypted(_)) => Ok(true),
        Err(InfoError::Other(m)) => Err(CheckError::invalid(m)),
    }
}

/// BIFF records: `(sid, body)`.
struct Records<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for Records<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        let sid = u16_at(self.b, self.pos)?;
        let len = usize::from(u16_at(self.b, self.pos + 2)?);
        let body = self.b.get(self.pos + 4..self.pos + 4 + len)?;
        self.pos += 4 + len;
        Some((sid, body))
    }
}

// --------------------------------------------------------------- PowerPoint

const RT_USER_EDIT_ATOM: u16 = 0x0FF5;
const RT_PERSIST_PTR_HOLDER: u16 = 0x1772;
const RT_DOCUMENT_ENCRYPTION_ATOM: u16 = 0x2F14;
/// Bound on the user-edit chain walked.
const MAX_EDITS: usize = 10_000;

/// The `UserEditAtom` fields POI uses.
#[derive(Debug, Clone, Copy)]
struct UserEdit {
    last_edit: u32,
    persist_ptrs: u32,
    /// `-1` when absent.
    encrypt_session_ref: i64,
}

/// `HSLFSlideShowImpl.initRecordOffsets` + `HSLFSlideShowEncrypted`: walk
/// the user edits, find the newest one naming an encryption session, and
/// its `DocumentEncryptionAtom`. Only `EncryptedPowerPointFileException` is
/// reported as encrypted by Tika.
fn powerpoint(cfb: &Cfb<'_>) -> Result<bool, CheckError> {
    let doc = cfb
        .root_child("PowerPoint Document")
        .ok_or_else(|| CheckError::invalid("no PowerPoint Document stream"))?;
    let current = cfb
        .root_child("Current User")
        .ok_or_else(|| CheckError::invalid("no Current User stream"))?;
    let cu = cfb
        .read_stream(current, 64)
        .map_err(|e| CheckError::invalid(e.to_string()))?;
    let mut offset = u32_at(&cu, 16).ok_or_else(|| CheckError::invalid("short Current User"))?;
    let record = |off: u32| -> Result<(u16, Vec<u8>), CheckError> {
        let h = cfb
            .read_stream_range(doc, u64::from(off), 8)
            .map_err(|e| CheckError::invalid(e.to_string()))?;
        let (Some(ty), Some(len)) = (u16_at(&h, 2), u32_at(&h, 4)) else {
            return Err(CheckError::invalid("record past the end"));
        };
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        if len > MAX_RECORD {
            return Err(CheckError::invalid("record too large"));
        }
        let body = cfb
            .read_stream_range(doc, u64::from(off) + 8, len)
            .map_err(|e| CheckError::invalid(e.to_string()))?;
        if body.len() < len {
            return Err(CheckError::invalid("record past the end"));
        }
        Ok((ty, body))
    };
    // Every user edit (offset → atom), newest chain first.
    let mut edits: Vec<(u32, UserEdit)> = Vec::new();
    let mut seen_offsets: Vec<u32> = Vec::new();
    while offset != 0 {
        if edits.len() >= MAX_EDITS {
            return Err(CheckError::invalid("too many user edits"));
        }
        let (ty, body) = record(offset)?;
        if ty != RT_USER_EDIT_ATOM {
            return Err(CheckError::invalid("not a user edit atom"));
        }
        let edit = UserEdit {
            last_edit: u32_at(&body, 8).ok_or_else(|| CheckError::invalid("short user edit"))?,
            persist_ptrs: u32_at(&body, 12)
                .ok_or_else(|| CheckError::invalid("short user edit"))?,
            encrypt_session_ref: if body.len() > 28 {
                i32_at(&body, 28).map_or(-1, i64::from)
            } else {
                -1
            },
        };
        edits.push((offset, edit));
        seen_offsets.push(offset);
        seen_offsets.push(edit.persist_ptrs);
        offset = edit.last_edit;
        if offset > 0 && seen_offsets.contains(&offset) {
            // POI's repair attempt rarely applies; a loop is corrupt.
            return Err(CheckError::invalid("user edit loop"));
        }
    }
    // The UserEditAtom with the highest offset that names a session.
    let Some((_, edit)) = edits
        .iter()
        .filter(|(_, e)| e.encrypt_session_ref != -1)
        .max_by_key(|(o, _)| *o)
    else {
        return Ok(false);
    };
    let (ty, ptrs) = record(edit.persist_ptrs)?;
    if ty != RT_PERSIST_PTR_HOLDER {
        return Err(CheckError::invalid("not a persist pointer holder"));
    }
    let Some(enc_offset) = persist_lookup(&ptrs, edit.encrypt_session_ref) else {
        // "Encryption info doesn't exist anymore."
        return Ok(false);
    };
    let (ty, dea) = record(enc_offset)?;
    if ty != RT_DOCUMENT_ENCRYPTION_ATOM {
        // "Did not have a DocumentEncryptionAtom".
        return Ok(true);
    }
    let info = parse_info(&mut Le::new(&dea), Preferred::CryptoApi)
        .map_err(|_| CheckError::invalid("invalid DocumentEncryptionAtom"))?;
    match verifies_default_password(&info) {
        Ok(ok) => Ok(!ok),
        Err(_) => Err(CheckError::invalid("encryption info not usable")),
    }
}

/// `PersistPtrHolder.getSlideLocationsLookup()[id]`.
fn persist_lookup(body: &[u8], id: i64) -> Option<u32> {
    let mut pos = 0;
    let mut found = None;
    while pos + 4 <= body.len() {
        let info = u32_at(body, pos)?;
        pos += 4;
        let first = i64::from(info & 0xF_FFFF);
        let count = (info >> 20) as usize;
        for i in 0..count {
            let off = u32_at(body, pos)?;
            pos += 4;
            if first + i as i64 == id {
                found = Some(off);
            }
        }
    }
    found
}

// ------------------------------------------------------------------ helpers

/// `DirectoryNode.getEntryCaseInsensitive` for a stream.
fn child_ci<'c>(cfb: &'c Cfb<'_>, dir: &Entry, name: &str) -> Option<&'c Entry> {
    let up = name.to_uppercase();
    cfb.child_uc(dir, &up).filter(|e| e.kind == 2)
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
            .collect()
    }

    /// FILEPASS bodies written by POI 5.3 (`MakeEncrypted.java`), with
    /// POI's verdict on the default password.
    #[test]
    fn poi_vectors() {
        let vectors = [
            ("00004d01a7da", false),
            ("000059b30a9a", true),
            (
                "0100010001007e559578a68b6fa412e7707d2af755b2a777dbe8f89ffb2da903875080f9e4601e2bc640f42d98f2c7fb579041d7511f",
                false,
            ),
            (
                "010001000100bdc9819aee7b4a431040790ac1010eaa3b38b0f035aef58eca81b367bb11d7403a2889f8d47405442ddc9ed79abc9722",
                true,
            ),
        ];
        for (hex, want) in vectors {
            let body = unhex(hex);
            let mut r = Le::new(&body);
            let pref = if r.u16().unwrap() == 0 {
                Preferred::Xor
            } else {
                Preferred::CryptoApi
            };
            let info = parse_info(&mut r, pref).unwrap();
            assert_eq!(verifies_default_password(&info), Ok(want), "{hex}");
        }
    }

    #[test]
    fn unknown_versions_and_algorithms() {
        // Version 9.9: EncryptedDocumentException.
        let mut b = vec![9, 0, 9, 0];
        b.extend([0; 4]);
        assert_eq!(
            parse_info(&mut Le::new(&b), Preferred::None),
            Err(InfoError::Encrypted("unknown encryption"))
        );
        // Truncated: a plain parse error.
        assert!(matches!(
            parse_info(&mut Le::new(&[1, 0, 1, 0, 5]), Preferred::None),
            Err(InfoError::Other(_))
        ));
        // CryptoAPI header with an unknown cipher id.
        let mut b = vec![2, 0, 2, 0];
        b.extend(4i32.to_le_bytes()); // flags
        b.extend(32i32.to_le_bytes()); // header size
        b.extend(4i32.to_le_bytes()); // flags
        b.extend(0i32.to_le_bytes()); // size extra
        b.extend(0x1234i32.to_le_bytes());
        assert_eq!(
            parse_info(&mut Le::new(&b), Preferred::CryptoApi),
            Err(InfoError::Encrypted("cipher algorithm not found"))
        );
    }

    #[test]
    fn persist_pointers() {
        let mut b = Vec::new();
        b.extend((5u32 | (2 << 20)).to_le_bytes());
        b.extend(100u32.to_le_bytes());
        b.extend(200u32.to_le_bytes());
        b.extend((9u32 | (1 << 20)).to_le_bytes());
        b.extend(300u32.to_le_bytes());
        assert_eq!(persist_lookup(&b, 6), Some(200));
        assert_eq!(persist_lookup(&b, 9), Some(300));
        assert_eq!(persist_lookup(&b, 7), None);
        assert_eq!(persist_lookup(&b[..6], 5), None);
    }
}
