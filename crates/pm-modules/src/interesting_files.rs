//! Interesting Files Identifier (Autopsy `modules/interestingitems`,
//! inventory 03 §2.8), over the reusable rule engine in `pm-rules`.
//!
//! Every enabled files set is matched against every file (slack files
//! excepted); a match posts `TSK_INTERESTING_ITEM` (score Likely Notable,
//! configuration and `TSK_SET_NAME` = the set, `TSK_CATEGORY` = the rule)
//! once per file and set, and an "Interesting File Match" inbox message.
//! The sets are the job's (Autopsy's global "Interesting Files" options);
//! by default Autopsy's standard sets.

use std::sync::Arc;

use pm_casedb::{FileKindCode, FileRecord, Known, Origin, Significance};
use pm_ingest::{
    FileModule, IngestFile, IngestMessage, JobContext, MessageKind, ModuleError, ModuleFactory,
    ModuleInfo, ModuleJob,
};
use pm_rules::{CompiledSet, FileFacts, FileMeta, FileOrigin, FilesSet};
use serde::{Deserialize, Serialize};

use crate::util::{Art, file_result_exists};

pub const NAME: &str = "Interesting Files Identifier";

/// Per-job settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterestingFilesSettings {
    /// The defined sets; `None` uses Autopsy's standard sets.
    #[serde(default)]
    pub sets: Option<Vec<FilesSet>>,
    /// Sets switched off for this job (`FilesIdentifierIngestJobSettings`:
    /// everything not listed is enabled).
    #[serde(default)]
    pub disabled_sets: Vec<String>,
}

/// The facts `pm-rules` matches, from a file record.
pub fn file_facts<'a>(rec: &'a FileRecord, mime: Option<&'a str>) -> FileFacts<'a> {
    let meta = match (rec.kind, rec.origin) {
        (FileKindCode::Directory, Origin::VirtualDir) => FileMeta::VirtualDirectory,
        (FileKindCode::Directory, _) => FileMeta::Directory,
        (FileKindCode::Regular, _) => FileMeta::Regular,
        _ => FileMeta::Other,
    };
    let origin = match rec.origin {
        Origin::UnallocBlocks => FileOrigin::Unallocated,
        Origin::Slack => FileOrigin::Slack,
        _ => FileOrigin::Normal,
    };
    let time = |i: usize| rec.times.get(i).copied().flatten().map_or(0, |(s, _)| s);
    FileFacts {
        name: &rec.name,
        extension: &rec.extension,
        parent_path: &rec.parent_path,
        mime_type: mime,
        size: rec.size,
        meta,
        origin,
        known: rec.known == Known::Known,
        // times: modified, accessed, changed, created.
        crtime: time(3),
        mtime: time(0),
    }
}

#[derive(Debug, Default)]
pub struct InterestingFilesFactory;

impl ModuleFactory for InterestingFilesFactory {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "interesting_files",
            name: NAME,
            description: "Identifies interesting items as defined by interesting item rule sets.",
            version: env!("CARGO_PKG_VERSION"),
            enabled_by_default: true,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::to_value(InterestingFilesSettings::default()).unwrap_or_default()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        let s: InterestingFilesSettings = if settings.is_null() {
            InterestingFilesSettings::default()
        } else {
            serde_json::from_value(settings.clone()).map_err(|e| {
                ModuleError::new(format!(
                    "Error getting interesting files sets from file. ({e})"
                ))
            })?
        };
        let sets = s
            .sets
            .unwrap_or_else(pm_rules::standard::standard_sets)
            .into_iter()
            .filter(|set| !s.disabled_sets.contains(&set.name))
            .map(|set| set.compile())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                ModuleError::new(format!(
                    "Error getting interesting files sets from file. ({e})"
                ))
            })?;
        Ok(Arc::new(Job(Arc::new(sets))))
    }
}

struct Job(Arc<Vec<CompiledSet>>);

impl ModuleJob for Job {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        if self.0.is_empty() {
            return None;
        }
        Some(Box::new(Worker(Arc::clone(&self.0))))
    }
}

struct Worker(Arc<Vec<CompiledSet>>);

impl FileModule for Worker {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        if file.rec.origin == Origin::Slack {
            return Ok(());
        }
        let now = ctx.now();
        let mime = file.mime().map(str::to_owned);
        let rec = &file.rec;
        let facts = file_facts(rec, mime.as_deref());
        for set in self.0.iter() {
            let Some(rule) = set.member_rule(&facts, now) else {
                continue;
            };
            let set_name = &set.set().name;
            // `artifactExists`: an earlier job already reported it.
            if file_result_exists(
                ctx,
                rec.id,
                "TSK_INTERESTING_ITEM",
                &[("TSK_SET_NAME", set_name), ("TSK_CATEGORY", rule)],
            ) {
                continue;
            }
            Art::result(
                ctx,
                NAME,
                "TSK_INTERESTING_ITEM",
                rec.id,
                Significance::LikelyNotable,
            )
            .configuration(set_name.clone())
            .text("TSK_SET_NAME", set_name.clone())
            .text("TSK_CATEGORY", rule)
            .post(ctx, &["TSK_SET_NAME", "TSK_CATEGORY"]);
            ctx.post_message(
                IngestMessage::new(
                    MessageKind::Data,
                    NAME,
                    format!("Interesting File Match: {set_name}({})", rec.name),
                )
                .details(format!("File: {}\nRule Set: {set_name}", rec.path()))
                .key(rec.name.clone())
                .file(rec.id),
            );
        }
        Ok(())
    }
}
