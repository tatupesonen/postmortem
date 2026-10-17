//! The wasmtime side: engine, per-worker instances, limits and the host
//! interface implementation.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use pm_plugin_api as api;
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::manifest::Manifest;
use crate::{LoadError, convert};

wasmtime::component::bindgen!({
    world: "plugin",
    path: "../pm-plugin-api/wit",
});

use exports::postmortem::plugin::module::Guest as ModuleExports;
use postmortem::plugin::{host as wit_host, types as wit};

/// The most bytes one `host.read` call returns.
const MAX_READ: u32 = 16 * 1024 * 1024;

/// Resource limits of a plugin instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Linear memory cap, in bytes.
    pub memory: usize,
    /// Fuel (roughly, WebAssembly instructions) per call into the plugin.
    pub fuel_per_call: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: 256 * 1024 * 1024,
            fuel_per_call: 5_000_000_000,
        }
    }
}

pub(crate) fn engine() -> &'static Engine {
    static ENGINE: LazyLock<Engine> = LazyLock::new(|| {
        let mut c = Config::new();
        c.wasm_component_model(true).consume_fuel(true);
        // The default configuration is valid; a failure here is a build
        // problem, so fall back to the defaults rather than abort.
        Engine::new(&c).unwrap_or_default()
    });
    &ENGINE
}

/// Store data: WASI (locked down), limits, and the job's host while a call
/// is in progress.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    host: Option<Arc<dyn api::Host + Send + Sync>>,
}

impl State {
    fn new(limits: Limits) -> Self {
        let wasi = WasiCtxBuilder::new()
            .allow_tcp(false)
            .allow_udp(false)
            .allow_ip_name_lookup(false)
            .socket_addr_check(|_, _| Box::pin(async { false }))
            .build();
        Self {
            wasi,
            table: ResourceTable::new(),
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory)
                .instances(16)
                .tables(64)
                .memories(16)
                .build(),
            host: None,
        }
    }

    fn host(&self) -> Result<&Arc<dyn api::Host + Send + Sync>, String> {
        self.host
            .as_ref()
            .ok_or_else(|| "the host is only available while the plugin is processing".to_owned())
    }
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl wit::Host for State {}

impl wit_host::Host for State {
    fn data_source(&mut self) -> wit::DataSourceInfo {
        match &self.host {
            Some(h) => convert::ds_to_wit(&h.data_source()),
            None => convert::ds_to_wit(&api::DataSourceInfo {
                id: 0,
                name: String::new(),
                timezone: "UTC".into(),
                size: 0,
            }),
        }
    }

    fn read(&mut self, file_id: i64, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        let h = self.host()?;
        let mut buf = vec![0; len.min(MAX_READ) as usize];
        let n = h.read(file_id, offset, &mut buf).map_err(|e| e.0)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn file(&mut self, file_id: i64) -> Result<Option<wit::FileInfo>, String> {
        Ok(self
            .host()?
            .file(file_id)
            .map_err(|e| e.0)?
            .as_ref()
            .map(convert::file_to_wit))
    }

    fn find_files(&mut self, q: wit::FileQuery) -> Result<Vec<wit::FileInfo>, String> {
        let files = self
            .host()?
            .find_files(&convert::find_from_wit(q))
            .map_err(|e| e.0)?;
        Ok(files.iter().map(convert::file_to_wit).collect())
    }

    fn children(&mut self, file_id: i64) -> Result<Vec<wit::FileInfo>, String> {
        let files = self.host()?.children(file_id).map_err(|e| e.0)?;
        Ok(files.iter().map(convert::file_to_wit).collect())
    }

    fn post(&mut self, r: wit::NewResult) -> Result<(), String> {
        self.host()?
            .post(convert::result_from_wit(r))
            .map_err(|e| e.0)
    }

    fn add_derived(
        &mut self,
        parent_id: i64,
        files: Vec<wit::DerivedFile>,
    ) -> Result<Vec<i64>, String> {
        let files = files.into_iter().map(convert::derived_from_wit).collect();
        self.host()?.add_derived(parent_id, files).map_err(|e| e.0)
    }

    fn log(&mut self, level: wit::Level, message: String) {
        if let Some(h) = &self.host {
            h.log(convert::level_from_wit(level), &message);
        } else {
            tracing::info!(target: "pm_plugin", "{message}");
        }
    }

    fn post_message(&mut self, m: wit::Message) {
        if let Some(h) = &self.host {
            h.message(convert::message_from_wit(m));
        }
    }

    fn is_cancelled(&mut self) -> bool {
        self.host.as_ref().is_some_and(|h| h.is_cancelled())
    }
}

/// A loaded plugin component.
pub struct WasmPlugin {
    dir: std::path::PathBuf,
    handle: Arc<Handle>,
    info: api::PluginInfo,
    default_settings: serde_json::Value,
}

impl std::fmt::Debug for WasmPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmPlugin")
            .field("dir", &self.dir)
            .field("id", &self.info.id)
            .finish_non_exhaustive()
    }
}

/// One instance of the plugin (one per worker).
struct Instance {
    store: Store<State>,
    bindings: Plugin,
    fuel: u64,
}

impl Instance {
    fn new(h: &Handle) -> Result<Self, String> {
        let mut store = Store::new(engine(), State::new(h.limits));
        store.limiter(|s| &mut s.limits);
        store
            .set_fuel(h.limits.fuel_per_call)
            .map_err(|e| e.to_string())?;
        let bindings = Plugin::instantiate(&mut store, &h.component, &h.linker)
            .map_err(|e| format!("cannot instantiate the plugin: {e:#}"))?;
        Ok(Self {
            store,
            bindings,
            fuel: h.limits.fuel_per_call,
        })
    }

    /// Runs one call with a fresh fuel budget and `host` available.
    fn call<R>(
        &mut self,
        host: Option<&dyn api::Host>,
        f: impl FnOnce(&ModuleExports, &mut Store<State>) -> wasmtime::Result<R>,
    ) -> Result<R, String> {
        self.store.set_fuel(self.fuel).map_err(|e| e.to_string())?;
        self.store.data_mut().host = host.map(api::Host::shared);
        let r = f(self.bindings.postmortem_plugin_module(), &mut self.store);
        self.store.data_mut().host = None;
        r.map_err(|e| {
            let fuel_out = self.store.get_fuel().is_ok_and(|f| f == 0);
            if fuel_out {
                "the plugin used up its fuel budget for this call".to_owned()
            } else {
                format!("the plugin trapped: {e:#}")
            }
        })
    }
}

impl WasmPlugin {
    /// Loads and checks the plugin in `dir` (its manifest and component),
    /// and asks the component for its info.
    pub fn load(dir: &Path) -> Result<Self, LoadError> {
        let fail = |reason: String| LoadError {
            path: dir.to_path_buf(),
            reason,
        };
        let manifest = Manifest::read(dir)?;
        let mut limits = Limits::default();
        if let Some(mb) = manifest.limits.memory_mb {
            limits.memory = usize::try_from(mb.saturating_mul(1024 * 1024)).unwrap_or(usize::MAX);
        }
        if let Some(f) = manifest.limits.fuel_per_call {
            limits.fuel_per_call = f;
        }
        let path = dir.join(&manifest.component);
        let component = Component::from_file(engine(), &path)
            .map_err(|e| fail(format!("{}: {e:#}", path.display())))?;
        let mut linker = Linker::new(engine());
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(|e| fail(e.to_string()))?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)
            .map_err(|e| fail(e.to_string()))?;
        let handle = Arc::new(Handle {
            component,
            linker: Arc::new(linker),
            limits,
        });
        let mut inst = Instance::new(&handle).map_err(&fail)?;
        let mut plugin = Self {
            dir: dir.to_path_buf(),
            handle,
            info: api::PluginInfo {
                id: String::new(),
                name: String::new(),
                description: String::new(),
                version: String::new(),
                enabled_by_default: false,
                file_filter: None,
                data_source_stage: None,
            },
            default_settings: serde_json::Value::Null,
        };
        let info = inst.call(None, |m, s| m.call_info(s)).map_err(&fail)?;
        let defaults = inst
            .call(None, |m, s| m.call_default_settings(s))
            .map_err(&fail)?;
        plugin.info = convert::info_from_wit(info);
        if plugin.info.id.is_empty() {
            return Err(fail("the plugin reports an empty id".into()));
        }
        plugin.default_settings = serde_json::from_str(&defaults).unwrap_or_default();
        Ok(plugin)
    }

    /// The folder the plugin was loaded from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn limits(&self) -> Limits {
        self.handle.limits
    }

    fn worker(&self, settings: &serde_json::Value) -> api::Result<Worker> {
        Ok(Worker {
            plugin: Arc::clone(&self.handle),
            settings: settings.clone(),
            inst: Some(self.handle.instance(settings)?),
        })
    }
}

impl api::Plugin for WasmPlugin {
    fn info(&self) -> api::PluginInfo {
        self.info.clone()
    }

    fn default_settings(&self) -> serde_json::Value {
        self.default_settings.clone()
    }

    fn validate(&self, settings: &serde_json::Value) -> api::Result<()> {
        let mut inst = Instance::new(&self.handle).map_err(api::PluginError)?;
        let json = settings.to_string();
        inst.call(None, |m, s| m.call_validate(s, &json))
            .map_err(api::PluginError)?
            .map_err(api::PluginError)
    }

    fn file_ingest(
        &self,
        settings: &serde_json::Value,
    ) -> api::Result<Option<Box<dyn api::FileIngest>>> {
        if self.info.file_filter.is_none() {
            return Ok(None);
        }
        Ok(Some(Box::new(self.worker(settings)?)))
    }

    fn data_source_ingest(
        &self,
        settings: &serde_json::Value,
    ) -> api::Result<Option<Box<dyn api::DataSourceIngest>>> {
        if self.info.data_source_stage.is_none() {
            return Ok(None);
        }
        Ok(Some(Box::new(self.worker(settings)?)))
    }
}

/// The compiled component and its linker: makes instances.
struct Handle {
    component: Component,
    linker: Arc<Linker<State>>,
    limits: Limits,
}

impl Handle {
    /// A new instance, started for a job with `settings`.
    fn instance(&self, settings: &serde_json::Value) -> api::Result<Instance> {
        let mut inst = Instance::new(self).map_err(api::PluginError)?;
        let json = settings.to_string();
        inst.call(None, |m, s| m.call_start(s, &json))
            .map_err(api::PluginError)?
            .map_err(api::PluginError)?;
        Ok(inst)
    }
}

/// A started instance on one worker; recreated after a trap (the guest's
/// state is undefined after one).
struct Worker {
    plugin: Arc<Handle>,
    settings: serde_json::Value,
    inst: Option<Instance>,
}

impl Worker {
    fn instance(&mut self) -> api::Result<&mut Instance> {
        if self.inst.is_none() {
            self.inst = Some(self.plugin.instance(&self.settings)?);
        }
        self.inst
            .as_mut()
            .ok_or_else(|| api::PluginError("no plugin instance".into()))
    }

    fn run<R>(
        &mut self,
        host: &dyn api::Host,
        f: impl FnOnce(&ModuleExports, &mut Store<State>) -> wasmtime::Result<Result<R, String>>,
    ) -> api::Result<R> {
        let inst = self.instance()?;
        match inst.call(Some(host), f) {
            Ok(r) => r.map_err(api::PluginError),
            Err(trap) => {
                self.inst = None;
                Err(api::PluginError(trap))
            }
        }
    }
}

impl api::FileIngest for Worker {
    fn process(&mut self, host: &dyn api::Host, file: &api::FileInfo) -> api::Result<()> {
        let f = convert::file_to_wit(file);
        self.run(host, |m, s| m.call_process_file(s, &f))
    }

    fn finish(&mut self, host: &dyn api::Host) {
        if let Some(inst) = self.inst.as_mut()
            && let Err(e) = inst.call(Some(host), |m, s| m.call_finish_files(s))
        {
            host.log(api::Level::Warn, &e);
        }
    }
}

impl api::DataSourceIngest for Worker {
    fn process(&mut self, host: &dyn api::Host) -> api::Result<()> {
        self.run(host, |m, s| m.call_process_data_source(s))
    }
}
