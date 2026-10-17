//! Data source hashes for Data Source Integrity (Autopsy `Image.setMD5` /
//! `setSha1` / `setSha256`): stored hashes from E01 headers or entered by
//! the user, and hashes the module computes.

use crate::{CaseDb, DbError, Result};

/// The hash algorithms a data source can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageHash {
    Md5,
    Sha1,
    Sha256,
}

impl ImageHash {
    pub const ALL: [Self; 3] = [Self::Md5, Self::Sha1, Self::Sha256];

    /// Autopsy's name (`MD5`, `SHA-1`, `SHA-256`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
        }
    }

    /// Hex digits of a hash value.
    pub fn hex_len(self) -> usize {
        match self {
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
}

/// A valid hash value: the right number of hex digits (stored lower
/// case).
pub fn normalize_hash(algo: ImageHash, value: &str) -> Option<String> {
    let v = value.trim();
    (v.len() == algo.hex_len() && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| v.to_ascii_lowercase())
}

impl CaseDb {
    /// The stored hashes of a data source (`Image.getMd5()` …), empty
    /// values left out.
    pub fn data_source_hashes(&self, ds_id: i64) -> Result<Vec<(ImageHash, String)>> {
        let ds = self
            .data_source(ds_id)?
            .ok_or_else(|| DbError::Invalid(format!("no data source {ds_id}")))?;
        Ok([
            (ImageHash::Md5, ds.md5),
            (ImageHash::Sha1, ds.sha1),
            (ImageHash::Sha256, ds.sha256),
        ]
        .into_iter()
        .filter_map(|(a, v)| v.filter(|v| !v.is_empty()).map(|v| (a, v)))
        .collect())
    }

    /// Stores a hash of a data source when none is stored yet (Autopsy refuses
    /// to overwrite one: an exception). Returns whether it was
    /// stored. The value must be hex of the algorithm's length.
    pub fn set_data_source_hash(&self, ds_id: i64, algo: ImageHash, value: &str) -> Result<bool> {
        let v = normalize_hash(algo, value).ok_or_else(|| {
            DbError::Invalid(format!("not a valid {} hash: {value}", algo.name()))
        })?;
        let col = algo.column();
        self.write(|tx| {
            let n = tx.execute(
                &format!(
                    "UPDATE data_sources SET {col} = ?2 WHERE id = ?1 AND ({col} IS NULL OR {col} = '')"
                ),
                rusqlite::params![ds_id, v],
            )?;
            Ok(n > 0)
        })
    }
}
