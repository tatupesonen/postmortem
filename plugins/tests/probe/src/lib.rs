//! Test plugin for postmortem's WebAssembly host: exercises every host
//! call, and fails on purpose when asked (a panic, an endless loop).

use pm_plugin_sdk::api::{
    Attribute, DataSourceIngest, DerivedFile, FileFilter, FileInfo, FileIngest, FindFiles, Host,
    HostReader, Level, Message, NewResult, Plugin, PluginInfo, Result, Significance,
};

#[derive(Default)]
struct Probe;

struct Files {
    crash_on: Option<String>,
    spin_on: Option<String>,
}

fn opt(settings: &serde_json::Value, key: &str) -> Option<String> {
    settings.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

impl Plugin for Probe {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "probe".into(),
            name: "Probe".into(),
            description: "Exercises the plugin API".into(),
            version: "0.1".into(),
            enabled_by_default: false,
            file_filter: Some(FileFilter {
                extensions: vec!["txt".into()],
                ..FileFilter::default()
            }),
            data_source_stage: Some(2),
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn file_ingest(&self, settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> {
        Ok(Some(Box::new(Files {
            crash_on: opt(settings, "crash_on"),
            spin_on: opt(settings, "spin_on"),
        })))
    }

    fn data_source_ingest(
        &self,
        _settings: &serde_json::Value,
    ) -> Result<Option<Box<dyn DataSourceIngest>>> {
        Ok(Some(Box::new(DataSource)))
    }
}

impl FileIngest for Files {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> Result<()> {
        if self.crash_on.as_deref() == Some(file.name.as_str()) {
            panic!("probe crashes on {}", file.name);
        }
        if self.spin_on.as_deref() == Some(file.name.as_str()) {
            let mut x: u64 = 1;
            loop {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                if x == 0 {
                    return Ok(());
                }
            }
        }
        let text = HostReader::new(host, file).read_all(1 << 20)?;
        let first = String::from_utf8_lossy(&text)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        let mut hit = NewResult::new("TSK_INTERESTING_FILE_HIT", Some(file.id))
            .with(Attribute::text("TSK_SET_NAME", "Probe"))
            .with(Attribute::text("TSK_COMMENT", first.clone()));
        hit.significance = Some(Significance::LikelyNotable);
        host.post(hit)?;
        host.add_derived(
            file.id,
            vec![DerivedFile {
                name: format!("{}.first-line", file.name),
                data: first.into_bytes(),
                modified: file.modified,
                accessed: None,
                created: None,
            }],
        )?;
        Ok(())
    }
}

struct DataSource;

impl DataSourceIngest for DataSource {
    fn process(&mut self, host: &dyn Host) -> Result<()> {
        let found = host.find_files(&FindFiles {
            name: Some("passwords.txt".into()),
            ..FindFiles::default()
        })?;
        let ds = host.data_source();
        host.log(Level::Info, &format!("probe saw {} files named passwords.txt", found.len()));
        host.message(Message {
            level: Level::Info,
            subject: format!("probe: {} in {}", found.len(), ds.name),
            details: String::new(),
            file_id: found.first().map(|f| f.id),
        });
        Ok(())
    }
}

pm_plugin_sdk::export_plugin!(Probe);
