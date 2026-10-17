//! Program-execution caches Autopsy does not read: `Amcache.hve` (file
//! inventory with SHA-1) and `RecentFileCache.bcf` (Windows 7).

use pm_casedb::Value;
use pm_casedb::catalog::ValueType;
use pm_ingest::{JobContext, ModuleError};
use pm_winartifacts::recentfilecache;
use pm_winartifacts::registry::amcache;

use super::art::{Batch, NewArt, Types};
use super::{PathLookup, Visits, files_with_ext, open_hive, read_small};

const RFC_COMMENT: &str = "Recently executed according to RecentFileCache.bcf";

pub(crate) fn extract(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let mut types = Types::default();
    let mut paths = PathLookup::default();
    let mut batch = Batch::default();
    let am = types.artifact(ctx, "KAAMOS_AMCACHE_FILE", "Amcache File Entries");
    let link = types.attribute(ctx, "KAAMOS_LINK_DATE", "PE Link Date", ValueType::DateTime);
    for f in files_with_ext(ctx, "hve", Some("amcache.hve"), "")? {
        if ctx.is_cancelled() {
            break;
        }
        if f.stream.is_some() || f.size == 0 {
            continue;
        }
        let Some(h) = open_hive(ctx, &f) else {
            continue;
        };
        for e in amcache::files(&h) {
            let target = paths.find(ctx, &e.path);
            batch.push(
                NewArt::new(ctx, am, Some(f.id))
                    .text("KAAMOS_PATH", e.path)
                    .text_opt("KAAMOS_HASH_SHA1", e.sha1)
                    .text_opt("KAAMOS_PROG_NAME", e.product_name)
                    .text_opt("KAAMOS_ORGANIZATION", e.publisher)
                    .text_opt("KAAMOS_VERSION", e.version)
                    .time("KAAMOS_DATETIME", e.key_last_written)
                    .time("KAAMOS_DATETIME_MODIFIED", e.file_modified)
                    .time("KAAMOS_DATETIME_CREATED", e.file_created)
                    .custom(link, e.link_date.map(Value::DateTime))
                    .key(&["KAAMOS_PATH", "KAAMOS_HASH_SHA1"])
                    .associate(target),
            );
        }
    }
    for f in files_with_ext(ctx, "bcf", Some("RecentFileCache.bcf"), "")? {
        if f.stream.is_some() {
            continue;
        }
        let Some(list) = read_small(ctx, &f).and_then(|d| recentfilecache::parse(&d)) else {
            continue;
        };
        for p in list {
            let target = paths.find(ctx, &p);
            batch.push(
                NewArt::std(ctx, "KAAMOS_PROG_RUN", Some(f.id))
                    .text("KAAMOS_PROG_NAME", p)
                    .text("KAAMOS_COMMENT", RFC_COMMENT)
                    .key(&["KAAMOS_PROG_NAME", "KAAMOS_COMMENT"])
                    .associate(target),
            );
        }
    }
    batch.flush(ctx)?;
    Ok(())
}
