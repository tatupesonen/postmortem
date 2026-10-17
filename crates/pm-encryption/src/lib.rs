//! Encryption and password-protection checks for Autopsy's Encryption
//! Detection module (inventory 03 §2.7):
//!
//! * [`entropy`]: Autopsy's byte-entropy calculation;
//! * [`volume`]: BitLocker (Autopsy's rule), LUKS and Core Storage volume
//!   signatures;
//! * [`office`]: legacy Word/Excel/PowerPoint password protection with
//!   Apache POI 5.3's semantics (default password `VelvetSweatshop`);
//! * [`pdf`]: PDF standard security handler with `PDFBox` 3.0.3 semantics
//!   (empty owner/user password);
//! * [`access`]: Access database passwords and encryption with Jackcess
//!   4.0.7's semantics.
//!
//! Each password check answers what Autopsy learns from Tika/Jackcess:
//! `Ok(true)` (an encrypted-document exception), `Ok(false)`, or
//! [`CheckError`] for any other parse failure, which Autopsy logs without
//! flagging the file. No data is ever decrypted beyond password
//! verifiers.

pub mod access;
mod crypto;
pub mod entropy;
pub mod office;
pub mod pdf;
pub mod volume;

/// The document could not be parsed far enough to decide (Autopsy:
/// `ProcessResult.ERROR`, nothing posted).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error("invalid document: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl CheckError {
    pub(crate) fn invalid(m: impl Into<String>) -> Self {
        Self::Invalid(m.into())
    }

    pub(crate) fn unsupported(m: impl Into<String>) -> Self {
        Self::Unsupported(m.into())
    }
}
