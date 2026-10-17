//! Prefetch (Autopsy `ExtractPrefetch`, which runs the external
//! `mm_artifact_parser`): one `KAAMOS_PROG_RUN` per distinct last-run time
//! of each `*.pf` under `\Windows\Prefetch`, parsed natively (versions 17–31,
//! Windows 10+ compression included).

use std::collections::BTreeSet;

use pm_ingest::{JobContext, ModuleError};

use super::art::{Batch, NewArt};
use super::{PathLookup, Visits, files_with_ext, read_small};

const COMMENT: &str = "Prefetch File";

pub(crate) fn extract(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let mut paths = PathLookup::default();
    let mut batch = Batch::default();
    for f in files_with_ext(ctx, "pf", None, "/windows/prefetch")? {
        if ctx.is_cancelled() {
            break;
        }
        if f.stream.is_some() {
            continue;
        }
        let Some(data) = read_small(ctx, &f) else {
            continue;
        };
        let p = match pm_prefetch::parse(&data) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(file = %f.path(), %e, "unreadable prefetch file");
                continue;
            }
        };
        let dir = p.executable_dir();
        let target = paths.find(ctx, &format!("{dir}/{}", p.executable));
        let times: BTreeSet<i64> = p.last_run_unix().into_iter().filter(|&t| t > 0).collect();
        for t in times {
            batch.push(
                NewArt::std(ctx, "KAAMOS_PROG_RUN", Some(f.id))
                    .text("KAAMOS_PROG_NAME", p.executable.clone())
                    .text("KAAMOS_PATH", dir.clone())
                    .time("KAAMOS_DATETIME", Some(t))
                    .int("KAAMOS_COUNT", i64::from(p.run_count))
                    .text("KAAMOS_COMMENT", COMMENT)
                    .key(&["KAAMOS_PROG_NAME", "KAAMOS_DATETIME"])
                    .associate(target),
            );
        }
    }
    batch.flush(ctx)?;
    Ok(())
}
