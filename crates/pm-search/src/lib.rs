//! Keyword and file-name search for postmortem (Autopsy's Keyword Search
//! back end, with Tantivy in place of Solr; ADR-014).
//!
//! * [`keywords`]: keyword lists — Autopsy's built-in lists and
//!   regular expressions, `keywords.xml` import/export, EnCase import.
//! * [`matcher`]: the inline matcher run on every chunk during ingest
//!   (Autopsy `InlineSearcher` semantics, compiled once per job).
//! * [`ccn`]: credit-card validation, track data, BIN lookup.
//! * [`content`]: the keyword index — chunk and summary documents, exact
//!   / substring / regex search with trigram candidates and exact
//!   verification, hit navigation for the Extracted Text viewer.
//! * [`names`]: the file-name index (substring, prefix and exact name or
//!   path search over hundreds of millions of names).
//! * [`CaseIndex`]: both indexes of a case, shared per process.

pub mod ccn;
pub mod content;
pub mod keywords;
pub mod matcher;
pub mod names;
pub mod plan;
mod tld;
pub mod tokenize;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use parking_lot::Mutex;
use thiserror::Error;

pub use ccn::{BinDb, BinInfo, CardData, ccn_hit_is_valid, is_valid_ccn, parse_track};
pub use content::{
    ChunkDoc, ContentIndex, IndexStats, ObjectHit, PageHits, PageText, SearchPage, TextQuery,
};
pub use keywords::{Keyword, KeywordList, KeywordListStore, ListError, SearchType};
pub use matcher::{Hit, InvalidKeyword, Matcher};
pub use names::{NameIndex, NameMatch, NamePage, NameQuery};
pub use tld::is_valid_tld;

/// Version of the on-disk index layout; indexes of another version are
/// deleted and rebuilt.
pub const INDEX_VERSION: u32 = 1;
/// Folder of the indexes inside a case folder.
pub const INDEX_DIR: &str = "Index";

/// Why a search operation failed.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("index error: {0}")]
    Index(String),
    #[error("case database error: {0}")]
    Db(#[from] pm_casedb::DbError),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("the index is open read-only (another process is writing it)")]
    ReadOnly,
}

impl From<tantivy::TantivyError> for SearchError {
    fn from(e: tantivy::TantivyError) -> Self {
        Self::Index(e.to_string())
    }
}

/// What a content-index document belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjRef {
    File(i64),
    Artifact(i64),
}

impl ObjRef {
    /// Index key: file id, or the negated artifact id (as TSK numbers
    /// artifacts below zero in Solr).
    pub fn key(self) -> i64 {
        match self {
            Self::File(id) => id,
            Self::Artifact(id) => id.saturating_neg(),
        }
    }

    pub fn from_key(key: i64) -> Self {
        if key < 0 {
            Self::Artifact(key.saturating_neg())
        } else {
            Self::File(key)
        }
    }
}

/// Registers postmortem's tokenizers on an index.
pub(crate) fn register_tokenizers(index: &tantivy::Index) {
    let t = index.tokenizers();
    t.register(tokenize::WORDS, tokenize::WordTokenizer::default());
    t.register(tokenize::TRIGRAMS, tokenize::TrigramTokenizer::default());
    t.register(
        tokenize::POS_TRIGRAMS,
        tokenize::PosTrigramTokenizer::default(),
    );
}

/// Both indexes of a case (`<case>/Index/content`, `<case>/Index/names`).
/// [`CaseIndex::open`] hands out one shared instance per case folder and
/// process, so the ingest module and the UI use the same writers.
#[derive(Debug)]
pub struct CaseIndex {
    dir: PathBuf,
    content: ContentIndex,
    names: NameIndex,
}

static OPEN: LazyLock<Mutex<HashMap<PathBuf, Weak<CaseIndex>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl CaseIndex {
    /// Opens (or creates) the indexes of the case in `case_dir`. Indexes
    /// written by another [`INDEX_VERSION`] are deleted first.
    pub fn open(case_dir: &Path) -> Result<Arc<Self>, SearchError> {
        let dir = case_dir.join(INDEX_DIR);
        let key = std::fs::canonicalize(case_dir).unwrap_or_else(|_| case_dir.to_path_buf());
        let mut open = OPEN.lock();
        open.retain(|_, w| w.strong_count() > 0);
        if let Some(ix) = open.get(&key).and_then(Weak::upgrade) {
            return Ok(ix);
        }
        std::fs::create_dir_all(&dir)?;
        let version_file = dir.join("VERSION");
        let current = INDEX_VERSION.to_string();
        let stale = std::fs::read_to_string(&version_file).map_or(true, |v| v.trim() != current);
        if stale {
            for sub in ["content", "names"] {
                let p = dir.join(sub);
                if p.exists() {
                    tracing::info!("removing index {} of another version", p.display());
                    std::fs::remove_dir_all(&p)?;
                }
            }
            std::fs::write(&version_file, &current)?;
        }
        let ix = Arc::new(Self {
            content: ContentIndex::open(&dir.join("content"))?,
            names: NameIndex::open(&dir.join("names"))?,
            dir,
        });
        open.insert(key, Arc::downgrade(&ix));
        Ok(ix)
    }

    pub fn content(&self) -> &ContentIndex {
        &self.content
    }

    pub fn names(&self) -> &NameIndex {
        &self.names
    }

    /// The `Index` folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Removes a data source from both indexes.
    pub fn delete_data_source(&self, ds_id: i64) -> Result<(), SearchError> {
        self.content.delete_data_source(ds_id)?;
        self.names.delete_data_source(ds_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_keys() {
        assert_eq!(ObjRef::File(5).key(), 5);
        assert_eq!(ObjRef::Artifact(5).key(), -5);
        assert_eq!(ObjRef::from_key(-5), ObjRef::Artifact(5));
        assert_eq!(ObjRef::from_key(0), ObjRef::File(0));
    }

    #[test]
    fn case_index_is_shared_and_versioned() {
        let dir = tempfile::tempdir().unwrap();
        let a = CaseIndex::open(dir.path()).unwrap();
        let b = CaseIndex::open(dir.path()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        a.content()
            .add_summary(ObjRef::File(1), 1, "kept.txt", 0)
            .unwrap();
        a.content().commit().unwrap();
        drop((a, b));
        let c = CaseIndex::open(dir.path()).unwrap();
        assert_eq!(c.content().stats().unwrap().objects, 1);
        drop(c);
        // Another version: rebuilt empty.
        std::fs::write(dir.path().join(INDEX_DIR).join("VERSION"), "0").unwrap();
        let d = CaseIndex::open(dir.path()).unwrap();
        assert_eq!(d.content().stats().unwrap().objects, 0);
        assert!(d.dir().ends_with(INDEX_DIR));
    }
}
