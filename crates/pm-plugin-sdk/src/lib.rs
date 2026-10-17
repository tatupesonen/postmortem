//! Write postmortem plugins in Rust.
//!
//! A plugin implements [`pm_plugin_api::Plugin`] (and its `FileIngest` /
//! `DataSourceIngest` instances) exactly like a built-in module, then
//! exports it:
//!
//! ```ignore
//! #[derive(Default)]
//! struct MyPlugin;
//! impl pm_plugin_sdk::api::Plugin for MyPlugin { /* … */ }
//! pm_plugin_sdk::export_plugin!(MyPlugin);
//! ```
//!
//! and builds a component with `cargo build --release --target
//! wasm32-wasip2` (crate type `cdylib`). Put the `.wasm` file and a
//! `plugin.toml` in a folder under postmortem's plugins directory.
//!
//! Inside the component, the [`Host`](api::Host) a module receives calls
//! the host through the `postmortem:plugin` WIT imports.

pub use pm_plugin_api as api;

use std::cell::RefCell;
use std::sync::Arc;

#[doc(hidden)]
#[allow(
    unsafe_code,
    unreachable_pub,
    missing_docs,
    clippy::all,
    clippy::pedantic
)]
pub mod bindings {
    wit_bindgen::generate!({
        world: "plugin",
        path: "../pm-plugin-api/wit",
        pub_export_macro: true,
        default_bindings_module: "pm_plugin_sdk::bindings",
    });
}

use bindings::postmortem::plugin::{host as wit_host, types as wit};

/// The host as seen from inside the component.
#[derive(Debug, Clone, Copy, Default)]
pub struct GuestHost;

fn err(e: String) -> api::PluginError {
    api::PluginError(e)
}

impl api::Host for GuestHost {
    fn data_source(&self) -> api::DataSourceInfo {
        let d = wit_host::data_source();
        api::DataSourceInfo {
            id: d.id,
            name: d.name,
            timezone: d.timezone,
            size: d.size,
        }
    }

    fn read(&self, file_id: i64, offset: u64, buf: &mut [u8]) -> api::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let data = wit_host::read(file_id, offset, len).map_err(err)?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }

    fn file(&self, file_id: i64) -> api::Result<Option<api::FileInfo>> {
        Ok(wit_host::file(file_id).map_err(err)?.map(file_from_wit))
    }

    fn find_files(&self, q: &api::FindFiles) -> api::Result<Vec<api::FileInfo>> {
        let q = wit::FileQuery {
            name: q.name.clone(),
            extension: q.extension.clone(),
            path_contains: q.path_contains.clone(),
            mime: q.mime.clone(),
            limit: q.limit,
        };
        Ok(wit_host::find_files(&q)
            .map_err(err)?
            .into_iter()
            .map(file_from_wit)
            .collect())
    }

    fn children(&self, file_id: i64) -> api::Result<Vec<api::FileInfo>> {
        Ok(wit_host::children(file_id)
            .map_err(err)?
            .into_iter()
            .map(file_from_wit)
            .collect())
    }

    fn post(&self, r: api::NewResult) -> api::Result<()> {
        wit_host::post(&result_to_wit(r)).map_err(err)
    }

    fn add_derived(&self, parent_id: i64, files: Vec<api::DerivedFile>) -> api::Result<Vec<i64>> {
        let files: Vec<wit::DerivedFile> = files
            .into_iter()
            .map(|f| wit::DerivedFile {
                name: f.name,
                data: f.data,
                modified: f.modified,
                accessed: f.accessed,
                created: f.created,
            })
            .collect();
        wit_host::add_derived(parent_id, &files).map_err(err)
    }

    fn log(&self, level: api::Level, message: &str) {
        wit_host::log(level_to_wit(level), message);
    }

    fn message(&self, m: api::Message) {
        wit_host::post_message(&wit::Message {
            level: level_to_wit(m.level),
            subject: m.subject,
            details: m.details,
            file_id: m.file_id,
        });
    }

    fn is_cancelled(&self) -> bool {
        wit_host::is_cancelled()
    }

    fn shared(&self) -> Arc<dyn api::Host + Send + Sync> {
        Arc::new(Self)
    }
}

fn level_to_wit(l: api::Level) -> wit::Level {
    match l {
        api::Level::Debug => wit::Level::Debug,
        api::Level::Info => wit::Level::Info,
        api::Level::Warn => wit::Level::Warn,
        api::Level::Error => wit::Level::Error,
    }
}

fn file_from_wit(f: wit::FileInfo) -> api::FileInfo {
    api::FileInfo {
        id: f.id,
        ds_id: f.ds_id,
        name: f.name,
        parent_path: f.parent_path,
        extension: f.extension,
        size: f.size,
        kind: match f.kind {
            wit::FileKind::File => api::FileKind::File,
            wit::FileKind::Directory => api::FileKind::Directory,
            wit::FileKind::Unallocated => api::FileKind::Unallocated,
            wit::FileKind::Carved => api::FileKind::Carved,
            wit::FileKind::Derived => api::FileKind::Derived,
            wit::FileKind::Other => api::FileKind::Other,
        },
        deleted: f.deleted,
        mime: f.mime,
        md5: f.md5,
        modified: f.modified,
        accessed: f.accessed,
        created: f.created,
        changed: f.changed,
    }
}

fn result_to_wit(r: api::NewResult) -> wit::NewResult {
    wit::NewResult {
        artifact_type: r.artifact_type,
        file_id: r.file_id,
        significance: r.significance.map(|s| match s {
            api::Significance::Unknown => wit::Significance::Unknown,
            api::Significance::LikelyNone => wit::Significance::LikelyNone,
            api::Significance::LikelyNotable => wit::Significance::LikelyNotable,
            api::Significance::None => wit::Significance::None,
            api::Significance::Notable => wit::Significance::Notable,
        }),
        conclusion: r.conclusion,
        configuration: r.configuration,
        justification: r.justification,
        attributes: r
            .attributes
            .into_iter()
            .map(|a| wit::Attribute {
                kind: a.kind,
                value: match a.value {
                    api::Value::Text(s) => wit::Value::Text(s),
                    api::Value::Int(i) => wit::Value::Int(i),
                    api::Value::Double(d) => wit::Value::Double(d),
                    api::Value::Bytes(b) => wit::Value::Bytes(b),
                    api::Value::Time(t) => wit::Value::Time(t),
                },
            })
            .collect(),
    }
}

fn info_to_wit(i: api::PluginInfo) -> wit::PluginInfo {
    wit::PluginInfo {
        id: i.id,
        name: i.name,
        description: i.description,
        version: i.version,
        enabled_by_default: i.enabled_by_default,
        file_filter: i.file_filter.map(|f| wit::FileFilter {
            mime_types: f.mime_types,
            extensions: f.extensions,
            names: f.names,
            include_unallocated: f.include_unallocated,
            include_directories: f.include_directories,
            max_size: f.max_size,
        }),
        data_source_stage: i.data_source_stage,
    }
}

/// The module instances of this component instance (one job, one worker).
#[derive(Default)]
struct Started {
    files: Option<Box<dyn api::FileIngest>>,
    data_source: Option<Box<dyn api::DataSourceIngest>>,
}

thread_local! {
    static STARTED: RefCell<Started> = RefCell::new(Started::default());
}

fn settings(json: &str) -> api::Result<serde_json::Value> {
    serde_json::from_str(json).map_err(|e| api::PluginError(format!("bad settings: {e}")))
}

/// The component's exports for a plugin (used by [`export_plugin!`]).
#[doc(hidden)]
pub mod guest {
    use super::{GuestHost, STARTED, Started, api, info_to_wit, settings, wit};

    pub fn info(p: &dyn api::Plugin) -> wit::PluginInfo {
        info_to_wit(p.info())
    }

    pub fn default_settings(p: &dyn api::Plugin) -> String {
        p.default_settings().to_string()
    }

    pub fn validate(p: &dyn api::Plugin, json: &str) -> Result<(), String> {
        p.validate(&settings(json).map_err(|e| e.0)?)
            .map_err(|e| e.0)
    }

    pub fn start(p: &dyn api::Plugin, json: &str) -> Result<(), String> {
        let s = settings(json).map_err(|e| e.0)?;
        let info = p.info();
        let files = if info.file_filter.is_some() {
            p.file_ingest(&s).map_err(|e| e.0)?
        } else {
            None
        };
        let data_source = if info.data_source_stage.is_some() {
            p.data_source_ingest(&s).map_err(|e| e.0)?
        } else {
            None
        };
        STARTED.with(|st| *st.borrow_mut() = Started { files, data_source });
        Ok(())
    }

    pub fn process_file(file: wit::FileInfo) -> Result<(), String> {
        let file = super::file_from_wit(file);
        STARTED.with(|st| match st.borrow_mut().files.as_mut() {
            Some(f) => f.process(&GuestHost, &file).map_err(|e| e.0),
            None => Err("the plugin was not started for files".into()),
        })
    }

    pub fn finish_files() {
        STARTED.with(|st| {
            if let Some(f) = st.borrow_mut().files.as_mut() {
                f.finish(&GuestHost);
            }
        });
    }

    pub fn process_data_source() -> Result<(), String> {
        STARTED.with(|st| match st.borrow_mut().data_source.as_mut() {
            Some(d) => d.process(&GuestHost).map_err(|e| e.0),
            None => Err("the plugin was not started for the data source".into()),
        })
    }
}

/// Exports a [`api::Plugin`] type (which must implement `Default`) as the
/// component's `postmortem:plugin/module`.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        struct __PmPluginExports;

        fn __pm_plugin() -> &'static $plugin {
            static PLUGIN: ::std::sync::OnceLock<$plugin> = ::std::sync::OnceLock::new();
            PLUGIN.get_or_init(<$plugin as ::std::default::Default>::default)
        }

        impl $crate::bindings::exports::postmortem::plugin::module::Guest for __PmPluginExports {
            fn info() -> $crate::bindings::postmortem::plugin::types::PluginInfo {
                $crate::guest::info(__pm_plugin())
            }
            fn default_settings() -> ::std::string::String {
                $crate::guest::default_settings(__pm_plugin())
            }
            fn validate(settings: ::std::string::String) -> ::std::result::Result<(), ::std::string::String> {
                $crate::guest::validate(__pm_plugin(), &settings)
            }
            fn start(settings: ::std::string::String) -> ::std::result::Result<(), ::std::string::String> {
                $crate::guest::start(__pm_plugin(), &settings)
            }
            fn process_file(
                file: $crate::bindings::postmortem::plugin::types::FileInfo,
            ) -> ::std::result::Result<(), ::std::string::String> {
                $crate::guest::process_file(file)
            }
            fn finish_files() {
                $crate::guest::finish_files()
            }
            fn process_data_source() -> ::std::result::Result<(), ::std::string::String> {
                $crate::guest::process_data_source()
            }
        }

        $crate::bindings::export!(__PmPluginExports with_types_in $crate::bindings);
    };
}
