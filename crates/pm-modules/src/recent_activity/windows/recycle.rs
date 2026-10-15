//! Recycle Bin analyzer (Autopsy `ExtractRecycleBin`): `$I`/`$R` pairs in
//! `$Recycle.Bin\<SID>` (Vista+), plus the XP/2000 `INFO2` index in
//! `RECYCLER\<SID>` / `RECYCLED` that Autopsy does not read.
//!
//! `TSK_RECYCLE_BIN` goes on the file holding the content (`$R…`,
//! `Dc<n>…`); when that file is gone the record is still reported, on the
//! index file, with a comment. The `$R` is looked up in its own folder
//! (Autopsy searches the whole data source by name).

use std::collections::HashMap;

use pm_casedb::FileRecord;
use pm_ingest::{JobContext, ModuleError};
use pm_winartifacts::recycle;

use super::art::{Batch, NewArt, Types};
use super::{Visits, read_small, user_map};

const CONTENT_MISSING: &str = "Content file not found in the Recycle Bin";
const REMOVED: &str = "Removed from the Recycle Bin (restored or emptied)";

pub(crate) fn extract(ctx: &JobContext, _v: &mut Visits) -> Result<(), ModuleError> {
    let db = ctx.case.db();
    let mut types = Types::default();
    let ty = types.artifact(ctx, "TSK_RECYCLE_BIN", "Recycle Bin");
    let users = user_map(ctx);
    let mut batch = Batch::default();
    for fs in db.file_systems(ctx.ds_id)? {
        let Some(root) = db.fs_root(fs.id)? else {
            continue;
        };
        if let Some(bin) = db.child_named_nocase(root.id, "$Recycle.Bin")? {
            for dir in db.children(bin.id)? {
                dollar_i_dir(ctx, ty, &users, &dir, &mut batch)?;
            }
        }
        for name in ["RECYCLER", "RECYCLED"] {
            let Some(bin) = db.child_named_nocase(root.id, name)? else {
                continue;
            };
            info2_dir(ctx, ty, &users, &bin, &mut batch)?;
            for dir in db.children(bin.id)? {
                info2_dir(ctx, ty, &users, &dir, &mut batch)?;
            }
        }
        if ctx.is_cancelled() {
            break;
        }
    }
    batch.flush(ctx)?;
    Ok(())
}

fn sid_user(users: &HashMap<String, String>, dir: &FileRecord) -> String {
    users.get(&dir.name).cloned().unwrap_or_default()
}

fn dollar_i_dir(
    ctx: &JobContext,
    ty: i64,
    users: &HashMap<String, String>,
    dir: &FileRecord,
    batch: &mut Batch,
) -> Result<(), ModuleError> {
    // Autopsy skips `$I` files whose folder has no SID ('-').
    if !dir.name.contains('-') {
        return Ok(());
    }
    let user = sid_user(users, dir);
    let kids = ctx.case.db().children(dir.id)?;
    for i in kids
        .iter()
        .filter(|f| f.name.starts_with("$I") && f.stream.is_none())
    {
        let Some(meta) = read_small(ctx, i).and_then(|d| recycle::parse_dollar_i(&d)) else {
            continue;
        };
        let r = recycle::r_name(&i.name).and_then(|rn| {
            kids.iter().find(|k| {
                k.name.eq_ignore_ascii_case(&rn) && k.stream.is_none() && k.deleted() == i.deleted()
            })
        });
        let target = r.unwrap_or(i);
        batch.push(
            NewArt::new(ctx, ty, Some(target.id))
                .text("TSK_PATH", meta.original_path)
                .time("TSK_DATETIME_DELETED", meta.deleted)
                .text("TSK_USER_NAME", user.clone())
                .text_opt("TSK_COMMENT", r.is_none().then_some(CONTENT_MISSING))
                .key(&["TSK_PATH", "TSK_DATETIME_DELETED"]),
        );
    }
    Ok(())
}

fn info2_dir(
    ctx: &JobContext,
    ty: i64,
    users: &HashMap<String, String>,
    dir: &FileRecord,
    batch: &mut Batch,
) -> Result<(), ModuleError> {
    let db = ctx.case.db();
    let Some(info2) = db.child_named_nocase(dir.id, "INFO2")? else {
        return Ok(());
    };
    let Some(data) = read_small(ctx, &info2) else {
        return Ok(());
    };
    let user = sid_user(users, dir);
    for rec in recycle::parse_info2(&data) {
        let content = if rec.removed {
            None
        } else {
            db.child_named_nocase(dir.id, &rec.content_name())?
        };
        let comment = if rec.removed {
            Some(REMOVED)
        } else if content.is_none() {
            Some(CONTENT_MISSING)
        } else {
            None
        };
        batch.push(
            NewArt::new(ctx, ty, Some(content.as_ref().map_or(info2.id, |c| c.id)))
                .text("TSK_PATH", rec.original_path)
                .time("TSK_DATETIME_DELETED", rec.deleted)
                .text("TSK_USER_NAME", user.clone())
                .text_opt("TSK_COMMENT", comment)
                .key(&["TSK_PATH", "TSK_DATETIME_DELETED"]),
        );
    }
    Ok(())
}
