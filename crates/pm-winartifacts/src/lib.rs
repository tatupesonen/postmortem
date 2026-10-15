//! Windows OS artefact analyses.
//!
//! Pure functions from parsed evidence (registry hives, small binary
//! files) to plain structs; the ingest module maps them to artifacts.
//! They replace the RegRipper plugins and external tools Autopsy's Recent
//! Activity module runs (`ExtractRegistry`, `ExtractRecycleBin`,
//! `ExtractZoneIdentifier`, `UsbDeviceIdMapper`, …) and add analyses
//! Autopsy leaves to the RegRipper text report (`ShimCache`, Amcache,
//! `UserAssist`, network profiles, time zone, …).

pub mod recentfilecache;
pub mod recycle;
pub mod registry;
pub mod usb_ids;
pub mod zone;

/// FILETIME → Unix seconds (`None` for 0 / out of range).
pub fn filetime_unix(ft: u64) -> Option<i64> {
    pm_core::time::from_filetime(ft).map(jiff::Timestamp::as_second)
}
