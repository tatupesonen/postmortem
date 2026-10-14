//! postmortem's built-in ingest modules: Rust ports of Autopsy's core
//! analysers (ADR-013), run by the `pm-ingest` framework.

mod exif;
pub mod hash_lookup;
pub mod picture_analyzer;
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
        Arc::new(hash_lookup::HashLookupFactory),
        Arc::new(picture_analyzer::PictureAnalyzerFactory),
    ]
}
