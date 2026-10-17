//! Autopsy's sample file ingest module ("Big and Round File Finder",
//! `pythonExamples/July2015FileTutorial_BigRound`) as a postmortem plugin:
//! files larger than 10 MiB whose size is a multiple of 4096 bytes (a hint
//! of an encrypted container) get an Interesting Item result.
//!
//! Settings (JSON): `{"min_size": 10485760, "multiple": 4096}`.

use pm_plugin_sdk::api::{
    Attribute, FileFilter, FileInfo, FileIngest, Host, NewResult, Plugin, PluginError,
    PluginInfo, Result, Significance,
};

const NAME: &str = "Big and Round File Finder";

#[derive(Default)]
struct BigRoundFiles;

struct Finder {
    min_size: u64,
    multiple: u64,
}

fn setting(settings: &serde_json::Value, key: &str, default: u64) -> u64 {
    settings.get(key).and_then(serde_json::Value::as_u64).unwrap_or(default)
}

impl Plugin for BigRoundFiles {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "big_round_files".into(),
            name: NAME.into(),
            description: "Sample module that finds large files that are a multiple of 4096.".into(),
            version: "1.0".into(),
            enabled_by_default: false,
            // The host only calls `process` for regular files.
            file_filter: Some(FileFilter::all_files()),
            data_source_stage: None,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::json!({ "min_size": 10_485_760, "multiple": 4096 })
    }

    fn validate(&self, settings: &serde_json::Value) -> Result<()> {
        if setting(settings, "multiple", 4096) == 0 {
            return Err(PluginError::new("multiple must be at least 1"));
        }
        Ok(())
    }

    fn file_ingest(&self, settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> {
        Ok(Some(Box::new(Finder {
            min_size: setting(settings, "min_size", 10_485_760),
            multiple: setting(settings, "multiple", 4096).max(1),
        })))
    }
}

impl FileIngest for Finder {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> Result<()> {
        if file.size <= self.min_size || file.size % self.multiple != 0 {
            return Ok(());
        }
        let mut hit = NewResult::new("TSK_INTERESTING_ITEM", Some(file.id))
            .with(Attribute::text("TSK_SET_NAME", NAME));
        hit.significance = Some(Significance::LikelyNotable);
        hit.configuration = Some("Big and Round Files".into());
        host.post(hit)
    }
}

pm_plugin_sdk::export_plugin!(BigRoundFiles);
