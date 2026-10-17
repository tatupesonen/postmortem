//! A module written against `pm-plugin-api` runs in the ingest framework
//! through `plugin_host::PluginModule` (ADR-022): host-side file filter,
//! content reads, results, derived files, data-source work, messages and
//! idempotent re-runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::DemoCase;
use pm_modules::plugin_host::PluginModule;
use pm_plugin_api::{
    Attribute, DataSourceIngest, DerivedFile, FileFilter, FileInfo, FileIngest, FindFiles, Host,
    HostReader, Level, Message, NewResult, Plugin, PluginError, PluginInfo, Significance,
};

/// Flags text files containing a word; lists `passwords.txt` files.
#[derive(Default)]
struct Secrets {
    calls: Arc<AtomicUsize>,
}

struct SecretFiles {
    word: String,
    calls: Arc<AtomicUsize>,
}

impl FileIngest for SecretFiles {
    fn process(&mut self, host: &dyn Host, file: &FileInfo) -> pm_plugin_api::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(file.extension, "txt", "the host applies the filter");
        let text = HostReader::new(host, file).read_all(1 << 20)?;
        let text = String::from_utf8_lossy(&text);
        let Some(line) = text.lines().find(|l| l.contains(&self.word)) else {
            return Ok(());
        };
        host.post(
            NewResult::new("TSK_INTERESTING_FILE_HIT", Some(file.id))
                .with(Attribute::text("TSK_SET_NAME", "Secrets"))
                .with(Attribute::text("TSK_COMMENT", line.trim())),
        )?;
        let mut hit = NewResult::new("TSK_INTERESTING_FILE_HIT", Some(file.id));
        hit.significance = Some(Significance::LikelyNotable);
        hit.attributes = vec![Attribute::text("TSK_SET_NAME", "Secrets (scored)")];
        host.post(hit)?;
        host.add_derived(
            file.id,
            vec![DerivedFile {
                name: format!("{}.secret", file.name),
                data: line.as_bytes().to_vec(),
                modified: Some(1_600_000_000),
                accessed: None,
                created: None,
            }],
        )?;
        Ok(())
    }
}

struct PasswordFiles;

impl DataSourceIngest for PasswordFiles {
    fn process(&mut self, host: &dyn Host) -> pm_plugin_api::Result<()> {
        let found = host.find_files(&FindFiles {
            name: Some("PASSWORDS.TXT".into()),
            ..FindFiles::default()
        })?;
        host.log(Level::Info, &format!("{} password files", found.len()));
        host.message(Message {
            level: Level::Info,
            subject: format!(
                "{} password files in {}",
                found.len(),
                host.data_source().name
            ),
            details: found
                .iter()
                .map(FileInfo::path)
                .collect::<Vec<_>>()
                .join("\n"),
            file_id: found.first().map(|f| f.id),
        });
        Ok(())
    }
}

impl Plugin for Secrets {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: "test_secrets".into(),
            name: "Secrets".into(),
            description: "Finds secrets in text files".into(),
            version: "1.0".into(),
            enabled_by_default: false,
            file_filter: Some(FileFilter {
                extensions: vec!["txt".into()],
                ..FileFilter::default()
            }),
            data_source_stage: Some(2),
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        serde_json::json!({ "word": "hunter2" })
    }

    fn validate(&self, settings: &serde_json::Value) -> pm_plugin_api::Result<()> {
        if settings
            .get("word")
            .and_then(|w| w.as_str())
            .is_none_or(str::is_empty)
        {
            return Err(PluginError::new("a word to look for is required"));
        }
        Ok(())
    }

    fn file_ingest(
        &self,
        settings: &serde_json::Value,
    ) -> pm_plugin_api::Result<Option<Box<dyn FileIngest>>> {
        Ok(Some(Box::new(SecretFiles {
            word: settings["word"].as_str().unwrap_or_default().to_owned(),
            calls: Arc::clone(&self.calls),
        })))
    }

    fn data_source_ingest(
        &self,
        _settings: &serde_json::Value,
    ) -> pm_plugin_api::Result<Option<Box<dyn DataSourceIngest>>> {
        Ok(Some(Box::new(PasswordFiles)))
    }
}

#[test]
fn native_plugin_runs_as_an_ingest_module() {
    let case = DemoCase::new();
    let plugin = Arc::new(Secrets::default());
    let calls = Arc::clone(&plugin.calls);
    let module = Arc::new(PluginModule::new(plugin));
    let settings = serde_json::json!({ "word": "hunter2" });

    let (summary, inbox) = case.run_one(module.clone(), settings.clone());
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let txt = case
        .files()
        .iter()
        .filter(|f| f.extension.eq_ignore_ascii_case("txt") && f.size > 0)
        .count();
    let called = calls.load(Ordering::Relaxed);
    assert!(
        called >= 1 && called <= txt + 1,
        "{called} calls for {txt} text files"
    );

    let hits = case.artifacts("TSK_INTERESTING_FILE_HIT");
    assert_eq!(hits.len(), 2, "{hits:?}");
    let pw = case.file("/Users/alice/Documents/passwords.txt");
    assert!(hits.iter().all(|(a, _)| a.file_id == Some(pw.id)));
    assert!(
        hits.iter()
            .any(|(a, _)| { a.significance == Some(pm_casedb::Significance::LikelyNotable) })
    );
    assert!(
        hits.iter()
            .any(|(_, attrs)| attrs.get("TSK_COMMENT").map(String::as_str)
                == Some("router: hunter2"))
    );

    // The derived file is a child of the source file, with the line.
    let child = case
        .files()
        .into_iter()
        .find(|f| f.name == "passwords.txt.secret")
        .expect("derived file");
    assert_eq!(child.parent_id, Some(pw.id));
    let content = common::read_all(&*case.case.content(&child).unwrap());
    assert_eq!(content, b"router: hunter2");

    let messages = inbox.since(0);
    assert!(
        messages
            .iter()
            .any(|m| m.subject == "1 password files in demo.dd" && m.file_id == Some(pw.id)),
        "{:?}",
        messages.iter().map(|m| &m.subject).collect::<Vec<_>>()
    );

    // Re-running adds nothing.
    let files_before = case.files().len();
    case.run_one(module.clone(), settings);
    assert_eq!(case.artifacts("TSK_INTERESTING_FILE_HIT").len(), 2);
    assert_eq!(case.files().len(), files_before);

    // Invalid settings stop the job before any work (Autopsy startUp).
    let before = calls.load(Ordering::Relaxed);
    let job = pm_ingest::JobSettings {
        modules: vec![pm_ingest::ModuleSetting {
            id: "test_secrets".into(),
            enabled: true,
            settings: serde_json::json!({ "word": "" }),
        }],
        ..pm_ingest::JobSettings::default()
    };
    let err = pm_ingest::run_job(
        &case.case,
        case.ds_id,
        &job,
        &[module as Arc<dyn pm_ingest::ModuleFactory>],
        &Arc::new(pm_ingest::IngestProgress::default()),
        &Arc::new(pm_ingest::Inbox::default()),
    )
    .unwrap_err();
    assert!(err.to_string().contains("a word to look for"), "{err}");
    assert_eq!(calls.load(Ordering::Relaxed), before);
}

#[test]
fn unknown_result_types_are_reported_to_the_module() {
    struct Bad;
    impl FileIngest for Bad {
        fn process(&mut self, host: &dyn Host, file: &FileInfo) -> pm_plugin_api::Result<()> {
            let e = host
                .post(NewResult::new("TSK_NOT_A_TYPE", Some(file.id)))
                .unwrap_err();
            assert!(e.0.contains("unknown artifact type"), "{e}");
            let e = host
                .post(
                    NewResult::new("TSK_INTERESTING_FILE_HIT", Some(file.id))
                        .with(Attribute::text("TSK_NOPE", "x")),
                )
                .unwrap_err();
            assert!(e.0.contains("unknown attribute type"), "{e}");
            Err(PluginError::new("bad plugin"))
        }
    }
    struct BadPlugin;
    impl Plugin for BadPlugin {
        fn info(&self) -> PluginInfo {
            PluginInfo {
                id: "test_bad".into(),
                name: "Bad".into(),
                description: String::new(),
                version: "0".into(),
                enabled_by_default: false,
                file_filter: Some(FileFilter {
                    names: vec!["passwords.txt".into()],
                    ..FileFilter::default()
                }),
                data_source_stage: None,
            }
        }
        fn file_ingest(
            &self,
            _: &serde_json::Value,
        ) -> pm_plugin_api::Result<Option<Box<dyn FileIngest>>> {
            Ok(Some(Box::new(Bad)))
        }
    }
    let case = DemoCase::new();
    let (summary, _) = case.run_one(
        Arc::new(PluginModule::new(Arc::new(BadPlugin))),
        serde_json::Value::Null,
    );
    assert!(
        summary.errors.iter().any(|e| e.contains("bad plugin")),
        "module errors are recorded and the job continues: {:?}",
        summary.errors
    );
}
