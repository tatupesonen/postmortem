//! The late data-source stage: Autopsy's `KwsDataArtifactIngestModule`
//! and `KwsAnalysisResultIngestModule` (index and search the text of
//! every artifact except keyword hits and associated-object links), then
//! the file-name index catch-up.
//!
//! Artifacts are read from the case database in id order. Results posted
//! by file modules just before this stage may still be on their way to
//! the database, so the stage waits briefly for stragglers when it runs
//! out of artifacts.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use pm_casedb::{ArtifactRecord, Value};
use pm_ingest::{DataSourceModule, DsProgress, JobContext, ModuleError};
use pm_search::ObjRef;
use pm_text::{ExtractOptions, artifact_text};

use super::Job;
use super::file::{RunError, Source, run};
use super::hits::{HitSink, Target};
use crate::util::art_type;

const BATCH: usize = 500;
/// Empty polls (and the pause between them) before the stage decides no
/// more artifacts are coming.
const IDLE_POLLS: u32 = 3;
const IDLE_PAUSE: Duration = Duration::from_millis(200);

pub(super) struct ArtifactStage {
    job: Arc<Job>,
}

impl ArtifactStage {
    pub(super) fn new(job: Arc<Job>) -> Self {
        Self { job }
    }

    fn artifacts(&self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        let db = ctx.case.db();
        let names: HashMap<i64, String> = db
            .artifact_type_names()?
            .into_iter()
            .map(|(id, _, display, _)| (id, display))
            .collect();
        let skip = [
            art_type("KAAMOS_KEYWORD_HIT"),
            art_type("KAAMOS_ASSOCIATED_OBJECT"),
        ];
        let mut after = 0i64;
        let mut idle = 0u32;
        let mut done = 0u64;
        progress.message("Indexing artifact text");
        while !ctx.is_cancelled() {
            let batch = db.artifacts_after(ctx.ds_id, after, BATCH)?;
            if batch.is_empty() {
                idle += 1;
                if idle >= IDLE_POLLS {
                    break;
                }
                std::thread::sleep(IDLE_PAUSE);
                continue;
            }
            idle = 0;
            for a in batch {
                if ctx.is_cancelled() {
                    break;
                }
                after = a.id;
                if skip.contains(&a.type_id) {
                    continue;
                }
                let label = names.get(&a.type_id).map_or("Artifact", String::as_str);
                self.index_artifact(ctx, &a, label)?;
                done += 1;
                progress.set_done(done);
            }
        }
        Ok(())
    }

    fn index_artifact(
        &self,
        ctx: &JobContext,
        a: &ArtifactRecord,
        type_label: &str,
    ) -> Result<(), ModuleError> {
        let job = &*self.job;
        let obj = ObjRef::Artifact(a.id);
        // Indexed by an earlier run.
        if job
            .index
            .content()
            .num_chunks(obj)
            .map_err(|e| ModuleError::new(e.to_string()))?
            .is_some()
        {
            return Ok(());
        }
        let attrs = ctx.case.db().attributes(a.id)?;
        let text = artifact_text(
            attrs
                .iter()
                .map(|x| (x.display_name.clone(), display_value(&x.value)))
                .collect::<Vec<_>>()
                .iter()
                .map(|(n, v)| (n.as_str(), v.as_str())),
        );
        let label = format!("{type_label}_{}", a.id);
        let target = Target {
            obj,
            ds_id: a.ds_id,
            file_id: a.file_id,
            unallocated: false,
            label: label.clone(),
        };
        let mut hits = HitSink::new(job, ctx, target);
        let empty = pm_core::io::MemSource::new(Vec::new());
        match run(
            job,
            ctx,
            obj,
            a.ds_id,
            &label,
            &empty,
            Source::Text(&text),
            &mut hits,
            &ExtractOptions::default(),
        ) {
            Ok(_) => {
                job.stats.artifacts_indexed.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(RunError::Cancelled) => Ok(()),
            Err(e) => {
                tracing::warn!("keyword search: artifact {}: {e}", a.id);
                Ok(())
            }
        }
    }
}

/// An attribute value as Autopsy's `ArtifactTextExtractor` shows it
/// (dates in UTC).
fn display_value(v: &Value) -> String {
    match v {
        Value::Text(s) | Value::Json(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Bytes(b) => b.iter().fold(String::new(), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        }),
        Value::DateTime(t) => jiff::Timestamp::from_second(*t).map_or_else(
            |_| t.to_string(),
            |ts| ts.strftime("%Y-%m-%d %H:%M:%S UTC").to_string(),
        ),
    }
}

impl DataSourceModule for ArtifactStage {
    fn process(&mut self, ctx: &JobContext, progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        if self.job.settings.index_artifacts {
            self.artifacts(ctx, progress)?;
        }
        if self.job.settings.update_name_index && !ctx.is_cancelled() {
            progress.message("Updating the file-name index");
            let n = self
                .job
                .index
                .names()
                .catch_up(ctx.case.db(), &|| ctx.is_cancelled())
                .map_err(|e| ModuleError::new(format!("file-name index: {e}")))?;
            tracing::info!("file-name index: {n} names added");
        }
        self.job
            .index
            .content()
            .commit()
            .map_err(|e| ModuleError::new(format!("keyword index: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(display_value(&Value::Text("x".into())), "x");
        assert_eq!(display_value(&Value::Int(-3)), "-3");
        assert_eq!(display_value(&Value::Bytes(vec![0xAB, 1])), "ab01");
        assert_eq!(
            display_value(&Value::DateTime(0)),
            "1970-01-01 00:00:00 UTC"
        );
    }
}
