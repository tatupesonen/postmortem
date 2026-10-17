//! Keyword Search (Autopsy `KeywordSearchIngestModule`, inventory 05 §1).
//!
//! Disabled by default, like enabling Autopsy's module by hand. Per file
//! it follows Autopsy's decision tree: pick a text extractor from the MIME
//! type (`pm-text`), append the metadata block, chunk the text with
//! Autopsy's `Chunker`, search every chunk inline for the job's keyword
//! lists and add the chunks to the case's keyword index (`pm-search`).
//! Unallocated space, unknown binaries and failed extractions get string
//! extraction; known (NSRL) files, directories, empty files and archives
//! get a metadata-only entry.
//!
//! Hits become `KAAMOS_KEYWORD_HIT` analysis results (score Likely Notable,
//! configuration = list name) or, for the credit-card list, `KAAMOS_ACCOUNT`
//! credit-card account instances. Extractor metadata becomes
//! `KAAMOS_METADATA`. A late data-source stage indexes and searches the text
//! of the data source's artifacts and brings the file-name index up to
//! date. Results are posted per file as it completes (Autopsy keeps them
//! in memory until the job ends and drops them on cancellation).

mod artifacts;
mod file;
mod hits;
mod metadata;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use pm_ingest::{
    DataSourceModule, FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError,
    ModuleFactory, ModuleInfo, ModuleJob,
};
use pm_search::keywords::{builtin_lists, enabled_by_default};
use pm_search::{BinDb, CaseIndex, KeywordList, Matcher};
use pm_text::strings::{Script, StringsConfig};
use serde::{Deserialize, Serialize};

pub use metadata::metadata_attributes;

/// Display name (attribute source and `KAAMOS_SET_NAME` module).
pub const NAME: &str = "Keyword Search";
/// Settings id.
pub const ID: &str = "keyword_search";
/// Stage of the artifact-text and name-index pass: after file analysis
/// and every earlier data-source stage.
pub const ARTIFACT_STAGE: u32 = 9;

/// Per-job settings (Autopsy `KeywordSearchJobSettings` plus the global
/// string-extraction options, which postmortem keeps per job).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordSearchSettings {
    /// The lists searched during ingest, in full (so a job is
    /// reproducible). Default: the built-in lists Autopsy enables by
    /// default ("Email Addresses").
    #[serde(default = "default_lists")]
    pub lists: Vec<KeywordList>,
    /// Autopsy "Add text to Solr Index": index all text. When off, only
    /// chunks with hits (and their neighbours) are indexed.
    #[serde(default = "yes")]
    pub index_text: bool,
    /// Autopsy `SkipKnown`: known (NSRL) files get a metadata-only entry.
    #[serde(default = "yes")]
    pub skip_known: bool,
    /// String extraction: UTF-8 and UTF-16 (LE and BE).
    #[serde(default = "yes")]
    pub extract_utf8: bool,
    #[serde(default = "yes")]
    pub extract_utf16: bool,
    /// Scripts for string extraction (Autopsy `SCRIPT` names; default
    /// `LATIN_1`, which selects the fast English-only scanner).
    #[serde(default = "default_scripts")]
    pub scripts: Vec<String>,
    /// A binlist `ranges.csv` for credit-card BIN details (optional).
    #[serde(default)]
    pub bin_database: Option<PathBuf>,
    /// Index and search the text of the data source's artifacts.
    #[serde(default = "yes")]
    pub index_artifacts: bool,
    /// Bring the file-name index up to date at the end of the job.
    #[serde(default = "yes")]
    pub update_name_index: bool,
}

fn yes() -> bool {
    true
}

fn default_lists() -> Vec<KeywordList> {
    builtin_lists()
        .into_iter()
        .filter(|l| enabled_by_default(&l.name))
        .collect()
}

fn default_scripts() -> Vec<String> {
    vec![Script::Latin1.name().to_owned()]
}

impl Default for KeywordSearchSettings {
    fn default() -> Self {
        Self {
            lists: default_lists(),
            index_text: true,
            skip_known: true,
            extract_utf8: true,
            extract_utf16: true,
            scripts: default_scripts(),
            bin_database: None,
            index_artifacts: true,
            update_name_index: true,
        }
    }
}

impl KeywordSearchSettings {
    fn strings_config(&self) -> StringsConfig {
        let mut scripts: Vec<Script> = self
            .scripts
            .iter()
            .filter_map(|s| Script::from_name(s))
            .collect();
        if scripts.is_empty() {
            scripts.push(Script::Latin1);
        }
        StringsConfig {
            utf8: self.extract_utf8,
            utf16: self.extract_utf16,
            scripts,
        }
    }
}

#[derive(Debug, Default)]
pub struct KeywordSearchFactory;

impl ModuleFactory for KeywordSearchFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: ID,
            name: NAME,
            description: "Performs file indexing and search using selected keyword lists.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: false,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(KeywordSearchSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let settings: KeywordSearchSettings = if settings.is_null() {
            KeywordSearchSettings::default()
        } else {
            serde_json::from_value(settings.clone())
                .map_err(|e| ModuleError::new(format!("invalid settings: {e}")))?
        };
        let index = CaseIndex::open(ctx.case.dir()).map_err(|e| {
            ModuleError::new(format!(
                "The index could not be opened or does not exist. ({e})"
            ))
        })?;
        let (matcher, invalid) = Matcher::new(&settings.lists);
        for bad in &invalid {
            ctx.post_message(
                IngestMessage::new(
                    MessageKind::Warning,
                    NAME,
                    format!(
                        "Keyword {:?} of list {:?} is not searched",
                        bad.term, bad.list
                    ),
                )
                .details(bad.error.clone()),
            );
        }
        if !settings.index_text {
            ctx.post_message(
                IngestMessage::new(MessageKind::Warning, NAME, "Text indexing is disabled.")
                    .details(
                        "Only chunks with keyword hits (and the chunks around them) are added to \
                     the keyword index, so ad-hoc keyword search finds text near ingest hits only.",
                    ),
            );
        } else if matcher.is_empty() {
            ctx.post_message(
                IngestMessage::new(MessageKind::Warning, NAME, "No keywords in keyword list.")
                    .details(
                        "Only indexing will be done and keyword search will be skipped (you can \
                         still search the index ad hoc).",
                    ),
            );
        }
        let bins = settings
            .bin_database
            .as_ref()
            .and_then(|p| match BinDb::load(p) {
                Ok(db) => Some(db),
                Err(e) => {
                    ctx.post_message(
                        IngestMessage::new(
                            MessageKind::Warning,
                            NAME,
                            "There was an error loading Bank Identification Number information. \
                         Accounts will not have their BINs identified.",
                        )
                        .details(format!("{}: {e}", p.display())),
                    );
                    None
                }
            });
        let strings = settings.strings_config();
        Ok(Arc::new(JobRef(Arc::new(Job {
            settings,
            matcher,
            index,
            strings,
            bins,
            stats: Stats::default(),
            notifications: Mutex::new(HashMap::new()),
        }))))
    }
}

/// Final status of a file (Autopsy `IngestStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Text,
    Strings,
    Metadata,
    ErrorText,
    ErrorIndex,
    ErrorIo,
    /// Not processed (virtual directories, cancellation).
    Skipped,
}

#[derive(Debug, Default)]
pub(crate) struct Stats {
    text: AtomicU64,
    strings: AtomicU64,
    metadata: AtomicU64,
    error_text: AtomicU64,
    error_index: AtomicU64,
    error_io: AtomicU64,
    pub(crate) hits: AtomicU64,
    pub(crate) accounts: AtomicU64,
    pub(crate) artifacts_indexed: AtomicU64,
}

impl Stats {
    fn record(&self, s: Status) {
        let c = match s {
            Status::Text => &self.text,
            Status::Strings => &self.strings,
            Status::Metadata => &self.metadata,
            Status::ErrorText => &self.error_text,
            Status::ErrorIndex => &self.error_index,
            Status::ErrorIo => &self.error_io,
            Status::Skipped => return,
        };
        c.fetch_add(1, Ordering::Relaxed);
    }
}

/// State shared by all instances of one job.
pub(crate) struct Job {
    pub(crate) settings: KeywordSearchSettings,
    pub(crate) matcher: Matcher,
    pub(crate) index: Arc<CaseIndex>,
    pub(crate) strings: StringsConfig,
    pub(crate) bins: Option<BinDb>,
    pub(crate) stats: Stats,
    /// Inbox messages sent per hit term (at most 20 each).
    pub(crate) notifications: Mutex<HashMap<String, u32>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("keywords", &self.matcher.entries().len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Job {
    /// Summary message (Autopsy `postIndexSummary`).
    fn finish(&self, ctx: &JobContext) {
        if let Err(e) = self.index.content().commit() {
            ctx.post_message(
                IngestMessage::new(MessageKind::Error, NAME, "Could not save the keyword index")
                    .details(e.to_string()),
            );
        }
        let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
        let s = &self.stats;
        ctx.post_message(
            IngestMessage::new(MessageKind::Info, NAME, "Keyword Indexing Results").details(
                format!(
                    "Files with known types: {}\nFiles with general strings extracted: {}\n\
                 Metadata only was indexed: {}\nError (indexer): {}\n\
                 Error (text extraction): {}\nError (I/O): {}\nKeyword hits: {}\n\
                 Credit card accounts: {}\nArtifacts indexed: {}",
                    n(&s.text),
                    n(&s.strings),
                    n(&s.metadata),
                    n(&s.error_index),
                    n(&s.error_text),
                    n(&s.error_io),
                    n(&s.hits),
                    n(&s.accounts),
                    n(&s.artifacts_indexed),
                ),
            ),
        );
        if n(&s.error_index) > 0 {
            ctx.post_message(
                IngestMessage::new(MessageKind::Error, NAME, "Keyword Indexing Errors").details(
                    format!(
                        "Keyword index service had errors ingesting {} files.",
                        n(&s.error_index)
                    ),
                ),
            );
        } else if n(&s.error_io) + n(&s.error_text) > 0 {
            ctx.post_message(
                IngestMessage::new(MessageKind::Warning, NAME, "Keyword Indexing Warning").details(
                    "Keyword index service had errors reading files and extracting text. \
                     Could have been from corrupt media or files.",
                ),
            );
        }
    }
}

struct JobRef(Arc<Job>);

impl ModuleJob for JobRef {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(Worker(Arc::clone(&self.0))))
    }

    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        Some((
            ARTIFACT_STAGE,
            Box::new(artifacts::ArtifactStage::new(Arc::clone(&self.0))),
        ))
    }

    fn finish(&self, ctx: &JobContext) {
        self.0.finish(ctx);
    }
}

struct Worker(Arc<Job>);

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let status = file::process(&self.0, ctx, file);
        self.0.stats.record(status);
        if let Err(e) = self.0.index.content().maybe_commit() {
            tracing::warn!("keyword index commit: {e}");
        }
        Ok(())
    }

    fn shut_down(&mut self, _ctx: &JobContext) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_follow_autopsy() {
        let f = KeywordSearchFactory;
        assert!(!f.info().enabled_by_default);
        let s: KeywordSearchSettings = serde_json::from_value(f.default_settings()).unwrap();
        assert_eq!(s, KeywordSearchSettings::default());
        let names: Vec<&str> = s.lists.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Email Addresses"]);
        assert!(s.index_text && s.skip_known && s.extract_utf8 && s.extract_utf16);
        assert!(s.strings_config().is_latin_basic_only());
        // Missing fields take the defaults.
        let s: KeywordSearchSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, KeywordSearchSettings::default());
        let s: KeywordSearchSettings =
            serde_json::from_str(r#"{"scripts": ["CYRILLIC", "nonsense"]}"#).unwrap();
        assert_eq!(s.strings_config().scripts, [Script::Cyrillic]);
        let s: KeywordSearchSettings = serde_json::from_str(r#"{"scripts": []}"#).unwrap();
        assert_eq!(s.strings_config().scripts, [Script::Latin1]);
    }
}
