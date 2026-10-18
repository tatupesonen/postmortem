//! Changing a data source's time zone after it was added. FAT stores
//! local times, and exFAT does too unless an entry records its UTC offset,
//! so those times depend on the zone chosen when the data source was
//! added. Switching re-reads them from the file system opened in the new
//! zone (entries with their own offset come out unchanged); every other
//! file system stores UTC and is left alone.

use jiff::tz::TimeZone;
use pm_fs::FsKind;

use crate::{Case, CaseError, Result};

/// A data source whose system time zone (found by Recent Activity) is not
/// the zone its local file-system times were read in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneMismatch {
    pub ds_id: i64,
    pub ds_name: String,
    /// The zone as the system names it, e.g. "FLE Standard Time".
    pub system: String,
    /// Its IANA zone, e.g. "Europe/Kyiv".
    pub suggested: String,
    /// The data source's zone.
    pub current: String,
}

/// Case metadata key recording that the examiner kept or switched the
/// zone of a data source (the warning is not repeated).
fn reviewed_key(ds_id: i64) -> String {
    format!("timezone_reviewed.{ds_id}")
}

/// Rows updated per transaction.
const BATCH: usize = 10_000;

impl Case {
    /// The data source's system time zone when it differs from the
    /// data source's (compared by UTC offsets, not names), unless the
    /// examiner already reviewed it.
    pub fn time_zone_mismatch(&self, ds_id: i64) -> Result<Option<ZoneMismatch>> {
        let db = self.db();
        if db.meta(&reviewed_key(ds_id))?.is_some() {
            return Ok(None);
        }
        let Some(ds) = db.data_source(ds_id)? else {
            return Ok(None);
        };
        let current = TimeZone::get(&ds.timezone).unwrap_or(TimeZone::UTC);
        let mut first = None;
        for system in db.os_time_zones(ds_id)? {
            // Windows names map through CLDR; others may already be IANA.
            let Some(iana) = pm_core::windows_zones::iana_zone(&system)
                .map(str::to_owned)
                .or_else(|| TimeZone::get(&system).ok().map(|_| system.clone()))
            else {
                continue;
            };
            let Ok(zone) = TimeZone::get(&iana) else {
                continue;
            };
            if pm_core::time::same_offsets(&zone, &current) {
                // One installation agrees with the chosen zone.
                return Ok(None);
            }
            first.get_or_insert(ZoneMismatch {
                ds_id,
                ds_name: ds.name.clone(),
                system,
                suggested: iana,
                current: ds.timezone.clone(),
            });
        }
        Ok(first)
    }

    /// Records that the examiner kept (or switched) the data source's zone.
    pub fn mark_time_zone_reviewed(&self, ds_id: i64) -> Result<()> {
        Ok(self.db().set_meta(&reviewed_key(ds_id), "1")?)
    }

    /// Sets `ds_id`'s time zone (an IANA name) and re-reads the times of
    /// its FAT and exFAT files in it; returns the number of files updated.
    pub fn set_data_source_timezone(&self, ds_id: i64, timezone: &str) -> Result<u64> {
        jiff::tz::TimeZone::get(timezone)
            .map_err(|e| CaseError::Invalid(format!("time zone {timezone}: {e}")))?;
        self.db().set_data_source_timezone(ds_id, timezone)?;
        self.forget_source(ds_id);
        let mut updated = 0u64;
        for rec in self.db().file_systems(ds_id)? {
            let fs = match self.filesystem(rec.id) {
                Ok(fs) => fs,
                Err(e) => {
                    tracing::warn!(fs_id = rec.id, "time zone change: {e}");
                    continue;
                }
            };
            if !matches!(
                fs.info().kind,
                FsKind::Fat12 | FsKind::Fat16 | FsKind::Fat32 | FsKind::ExFat
            ) {
                continue;
            }
            let rows = self.db().file_inodes(rec.id)?;
            for chunk in rows.chunks(BATCH) {
                let times: Vec<_> = chunk
                    .iter()
                    .filter_map(|&(id, inode)| {
                        let m = fs.metadata(inode).ok()?;
                        Some((
                            id,
                            [
                                crate::load::split_time(m.times.modified),
                                crate::load::split_time(m.times.accessed),
                                crate::load::split_time(m.times.changed),
                                crate::load::split_time(m.times.created),
                            ],
                        ))
                    })
                    .collect();
                self.db().set_file_times(&times)?;
                updated += times.len() as u64;
            }
        }
        Ok(updated)
    }
}
