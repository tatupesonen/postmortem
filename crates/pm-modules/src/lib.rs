//! postmortem's built-in ingest modules: Rust ports of Autopsy's core
//! analysers (ADR-013), run by the `pm-ingest` framework.

pub mod data_source_integrity;
pub mod derive;
pub mod email_parser;
pub mod embedded;
pub mod encryption;
pub mod event_logs;
mod exif;
pub mod ext_mismatch;
pub mod file_type;
pub mod hash_lookup;
pub mod interesting_files;
pub mod keyword_search;
pub mod photorec;
pub mod picture_analyzer;
pub mod plugin_host;
pub mod recent_activity;
mod util;

use std::sync::Arc;

use pm_ingest::ModuleFactory;

/// The built-in modules in Autopsy's pipeline order
/// (`PipelineConfig.xml`): Hash Lookup must run before anything that
/// checks the known status, File Type Identification before every MIME
/// consumer.
pub fn core_modules() -> Vec<Arc<dyn ModuleFactory>> {
    vec![
        Arc::new(recent_activity::RecentActivityFactory),
        Arc::new(event_logs::EventLogFactory),
        Arc::new(hash_lookup::HashLookupFactory),
        Arc::new(file_type::FileTypeFactory),
        Arc::new(embedded::EmbeddedFileExtractorFactory),
        Arc::new(picture_analyzer::PictureAnalyzerFactory),
        Arc::new(keyword_search::KeywordSearchFactory),
        // Built on pm-plugin-api (ADR-022).
        Arc::new(plugin_host::PluginModule::new(Arc::new(
            email_parser::EmailParser,
        ))),
        Arc::new(ext_mismatch::ExtMismatchFactory),
        Arc::new(interesting_files::InterestingFilesFactory),
        Arc::new(photorec::PhotoRecCarverFactory),
        Arc::new(encryption::EncryptionFactory),
        Arc::new(data_source_integrity::IntegrityFactory),
    ]
}
