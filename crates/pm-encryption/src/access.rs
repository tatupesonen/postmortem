//! Access (Jet/ACE) database passwords, as Autopsy finds them with
//! Jackcess 4.0.7 and its `CryptCodecProvider` (no password supplied):
//!
//! * an ACCDB encrypted with Office encryption fails to open with
//!   `InvalidCredentialsException` → protected (Autopsy returns `true`);
//! * an unsupported Office encryption provider, or any other failure to
//!   open the database, → not protected (Autopsy's catch-all returns
//!   `false`);
//! * otherwise `Database.getDatabasePassword()`: the de-obfuscated
//!   password field of the header page is not blank → protected.
//!
//! Only the header page is read; Jackcess would also parse the system
//! catalog, so a database with a password and a corrupt catalog is
//! flagged here but not by Autopsy.

use pm_core::ReadAt;

use crate::CheckError;

/// Header obfuscation mask (`JetFormat.BASE_HEADER_MASK`).
const BASE_HEADER_MASK: [u8; 128] = [
    0xB5, 0x6F, 0x03, 0x62, 0x61, 0x08, 0xC2, 0x55, 0xEB, 0xA9, 0x67, 0x72, 0x43, 0x3F, 0x00, 0x9C,
    0x7A, 0x9F, 0x90, 0xFF, 0x80, 0x9A, 0x31, 0xC5, 0x79, 0xBA, 0xED, 0x30, 0xBC, 0xDF, 0xCC, 0x9D,
    0x63, 0xD9, 0xE4, 0xC3, 0x7B, 0x42, 0xFB, 0x8A, 0xBC, 0x4E, 0x86, 0xFB, 0xEC, 0x37, 0x5D, 0x44,
    0x9C, 0xFA, 0xC6, 0x5E, 0x28, 0xE6, 0x13, 0xB6, 0x8A, 0x60, 0x54, 0x94, 0x7B, 0x36, 0xF5, 0x72,
    0xDF, 0xB1, 0x77, 0xF4, 0x13, 0x43, 0xCF, 0xAF, 0xB1, 0x33, 0x34, 0x61, 0x79, 0x5B, 0x92, 0xB5,
    0x7C, 0x2A, 0x05, 0xF1, 0x7C, 0x99, 0x01, 0x1B, 0x98, 0xFD, 0x12, 0x4F, 0x4A, 0x94, 0x6C, 0x3E,
    0x60, 0x26, 0x5F, 0x95, 0xF8, 0xD0, 0x89, 0x24, 0x85, 0x67, 0xC6, 0x1F, 0x27, 0x44, 0xD2, 0xEE,
    0xCF, 0x65, 0xED, 0xFF, 0x07, 0xC7, 0x46, 0xA1, 0x78, 0x16, 0x0C, 0xED, 0xE9, 0x2D, 0x62, 0xD4,
];
const OFFSET_VERSION: usize = 0x14;
const OFFSET_MASKED_HEADER: usize = 24;
const OFFSET_ENCODING_KEY: usize = 62;
const OFFSET_PASSWORD: usize = 66;
const OFFSET_HEADER_DATE: usize = 114;
/// Office encryption provider info in ACCDB headers.
const CRYPT_STRUCTURE_OFFSET: usize = 0x299;
const MSISAM_ENGINE: &[u8] = b"MSISAM Database";
/// `EncryptionHeader.FCRYPTO_API_FLAG`.
const FCRYPTO_API: u32 = 0x04;

/// The Jet format of a header (`JetFormat.getFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Access 97.
    Jet3,
    /// Access 2000–2003.
    Jet4,
    /// Microsoft Money.
    MsIsam,
    /// Access 2007 and later (ACCDB).
    Ace,
}

impl Format {
    fn page_size(self) -> usize {
        if self == Self::Jet3 { 2048 } else { 4096 }
    }

    fn password_len(self) -> usize {
        if self == Self::Jet3 { 20 } else { 40 }
    }

    fn mask(self) -> &'static [u8] {
        if self == Self::Jet3 {
            &BASE_HEADER_MASK[..126]
        } else {
            &BASE_HEADER_MASK
        }
    }
}

/// Whether Autopsy would report the database as password protected.
pub fn is_password_protected(src: &dyn ReadAt) -> Result<bool, CheckError> {
    let mut head = [0u8; 21];
    let n = src
        .read_at(0, &mut head)
        .map_err(|e| CheckError::invalid(e.to_string()))?;
    if n < head.len() {
        // "Empty database file": open() fails, Autopsy returns false.
        return Ok(false);
    }
    let format = match head[OFFSET_VERSION] {
        0 => Format::Jet3,
        1 if head.get(4..19) == Some(MSISAM_ENGINE) => Format::MsIsam,
        1 => Format::Jet4,
        2 | 3 | 5 | 6 => Format::Ace,
        // Unsupported version: open() fails.
        _ => return Ok(false),
    };
    let mut page = vec![0u8; format.page_size()];
    if src.read_exact_at(0, &mut page).is_err() {
        return Ok(false);
    }
    // De-obfuscate the header (`PageChannel.applyHeaderMask`).
    for (i, m) in format.mask().iter().enumerate() {
        if let Some(b) = page.get_mut(OFFSET_MASKED_HEADER + i) {
            *b ^= m;
        }
    }
    match format {
        // Money databases never reach this check (their MIME type is not
        // x-msaccess); treat them as opened without a password.
        Format::MsIsam => return Ok(false),
        Format::Ace => {
            if let Some(protected) = office_encryption(&page) {
                return Ok(protected);
            }
        }
        Format::Jet3 | Format::Jet4 => {}
    }
    Ok(has_password(&page, format))
}

/// `OfficeCryptCodecHandler.create` with no password: `None` when the
/// database is not Office-encrypted (blank encoding key), else whether
/// Autopsy reports it as protected.
fn office_encryption(page: &[u8]) -> Option<bool> {
    let key = page.get(OFFSET_ENCODING_KEY..OFFSET_ENCODING_KEY + 4)?;
    if key.iter().all(|&b| b == 0) {
        return None;
    }
    let at = CRYPT_STRUCTURE_OFFSET + 2;
    let version = |o: usize| {
        page.get(at + o..at + o + 2)
            .and_then(|b| b.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let (Some(major), Some(minor)) = (version(0), version(2)) else {
        return Some(false);
    };
    let flags = page
        .get(at + 4..at + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes);
    let supported = matches!((major, minor), (4, 4) | (1, 1))
        || ((2..=4).contains(&major) && minor == 2 && flags & FCRYPTO_API != 0);
    // Supported providers reject the missing password
    // (InvalidCredentialsException → protected); unsupported ones throw
    // UnsupportedCodecException, which Autopsy's catch-all turns into
    // "not protected".
    Some(supported)
}

/// `DatabaseImpl.getDatabasePassword() != null`.
fn has_password(page: &[u8], format: Format) -> bool {
    let len = format.password_len();
    let Some(pwd) = page.get(OFFSET_PASSWORD..OFFSET_PASSWORD + len) else {
        return false;
    };
    let mut pwd = pwd.to_vec();
    if format != Format::Jet3 {
        // Extra mask from the creation date (`getPasswordMask`).
        let date = page
            .get(OFFSET_HEADER_DATE..OFFSET_HEADER_DATE + 8)
            .and_then(|b| b.try_into().ok())
            .map_or(0.0, f64::from_le_bytes);
        // Java's (int) cast: truncation, saturation, NaN → 0.
        let mask = (date as i32).to_le_bytes();
        for (i, b) in pwd.iter_mut().enumerate() {
            *b ^= mask.get(i % 4).copied().unwrap_or(0);
        }
    }
    pwd.iter().any(|&b| b != 0)
}

/// Header pages for tests: `format_version` (0 Jet3, 1 Jet4, 2+ ACE),
/// an optional database password and an optional Office encryption
/// provider version.
#[cfg(any(test, feature = "builder"))]
#[allow(
    clippy::indexing_slicing,
    reason = "fixture builder with fixed layouts"
)]
pub mod builder {
    use super::{
        BASE_HEADER_MASK, CRYPT_STRUCTURE_OFFSET, OFFSET_ENCODING_KEY, OFFSET_HEADER_DATE,
        OFFSET_MASKED_HEADER, OFFSET_PASSWORD, OFFSET_VERSION,
    };

    /// A database file of `pages` pages whose header carries `password`
    /// (UTF-16LE for Jet4+, bytes for Jet3) and, for ACE, `office`
    /// encryption `(major, minor, flags)`.
    pub fn database(
        format_version: u8,
        password: Option<&str>,
        office: Option<(u16, u16, u32)>,
        date: f64,
    ) -> Vec<u8> {
        let jet3 = format_version == 0;
        let page_size = if jet3 { 2048 } else { 4096 };
        let mut page = vec![0u8; page_size * 2];
        page[4..19].copy_from_slice(if format_version <= 1 {
            b"Standard Jet DB"
        } else {
            b"Standard ACE DB"
        });
        page[OFFSET_VERSION] = format_version;
        page[OFFSET_HEADER_DATE..OFFSET_HEADER_DATE + 8].copy_from_slice(&date.to_le_bytes());
        if let Some(p) = password {
            let bytes: Vec<u8> = if jet3 {
                p.bytes().collect()
            } else {
                p.encode_utf16().flat_map(u16::to_le_bytes).collect()
            };
            for (i, b) in bytes.iter().enumerate() {
                page[OFFSET_PASSWORD + i] = *b;
            }
        }
        if !jet3 {
            let mask = (date as i32).to_le_bytes();
            for i in 0..40 {
                page[OFFSET_PASSWORD + i] ^= mask[i % 4];
            }
        }
        if let Some((major, minor, flags)) = office {
            page[OFFSET_ENCODING_KEY..OFFSET_ENCODING_KEY + 4].copy_from_slice(&[1, 2, 3, 4]);
            let at = CRYPT_STRUCTURE_OFFSET;
            page[at..at + 2].copy_from_slice(&64u16.to_le_bytes());
            page[at + 2..at + 4].copy_from_slice(&major.to_le_bytes());
            page[at + 4..at + 6].copy_from_slice(&minor.to_le_bytes());
            page[at + 6..at + 10].copy_from_slice(&flags.to_le_bytes());
        }
        let mask: &[u8] = if jet3 {
            &BASE_HEADER_MASK[..126]
        } else {
            &BASE_HEADER_MASK
        };
        for (i, m) in mask.iter().enumerate() {
            page[OFFSET_MASKED_HEADER + i] ^= m;
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use super::builder::database;
    use super::*;
    use pm_core::io::MemSource;

    fn check(b: Vec<u8>) -> bool {
        is_password_protected(&MemSource::new(b)).unwrap()
    }

    #[test]
    fn database_passwords() {
        let date = 45_123.25;
        for v in [0u8, 1, 2, 3, 5, 6] {
            assert!(!check(database(v, None, None, date)), "version {v}");
            assert!(
                check(database(v, Some("s3cret"), None, date)),
                "version {v}"
            );
        }
    }

    #[test]
    fn office_encryption_providers() {
        let date = 45_000.0;
        // Agile, RC4, RC4 CryptoAPI and AES standard: no password → protected.
        for p in [(4, 4, 0), (1, 1, 0), (2, 2, 0x04), (4, 2, 0x24)] {
            assert!(check(database(2, None, Some(p), date)), "{p:?}");
        }
        // Extensible or flag-less providers: unsupported → not protected.
        for p in [(4, 3, 0), (3, 2, 0)] {
            assert!(!check(database(2, None, Some(p), date)), "{p:?}");
        }
    }

    #[test]
    fn short_and_unknown() {
        assert!(!check(vec![0u8; 10]));
        let mut b = database(1, Some("x"), None, 1.0);
        b[OFFSET_VERSION] = 9;
        assert!(!check(b));
        // Truncated page: open fails.
        let b = database(1, Some("x"), None, 1.0);
        assert!(!check(b[..1000].to_vec()));
    }
}
