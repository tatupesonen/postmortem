//! Runs modules written against `pm-plugin-api` (ADR-022) inside the
//! ingest framework: [`PluginModule`] turns any [`Plugin`] — a built-in
//! parser or a WebAssembly plugin loaded by `pm-plugin-wasm` — into a
//! [`ModuleFactory`], and [`JobHost`] implements the plugin's [`Host`]
//! over the job context.
//!
//! The host applies the plugin's file filter before calling it, posts
//! results through the job's batched writer (idempotent on re-runs) and
//! keeps the last opened content source, so the many small reads a parser
//! makes do not reopen the file.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::communications::RelationshipKind;
use pm_casedb::{FileKindCode, FileQuery, FileRecord, NewArtifact, Origin, Priority};
use pm_core::SharedSource;
use pm_ingest::{
    DataSourceModule, DsProgress, FileModule, IngestFile, IngestMessage, JobContext, MessageKind,
    ModuleError, ModuleFactory, ModuleInfo, ModuleJob,
};
use pm_plugin_api as api;
use pm_plugin_api::{FileFilter, Host, Plugin};

/// Module output folder of a plugin's derived files.
fn output_folder(id: &str) -> String {
    format!("plugin-{id}")
}

/// `ModuleInfo` holds `&'static str`s; plugin infos are owned. Each
/// distinct string is leaked once (plugins are few and loaded once).
fn intern(s: &str) -> &'static str {
    static STRINGS: LazyLock<Mutex<HashSet<&'static str>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    let mut set = STRINGS.lock();
    if let Some(&s) = set.get(s) {
        return s;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    set.insert(leaked);
    leaked
}

/// A [`Plugin`] as an ingest module.
pub struct PluginModule {
    plugin: Arc<dyn Plugin>,
    info: api::PluginInfo,
}

impl std::fmt::Debug for PluginModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginModule")
            .field("id", &self.info.id)
            .finish_non_exhaustive()
    }
}

impl PluginModule {
    pub fn new(plugin: Arc<dyn Plugin>) -> Self {
        let info = plugin.info();
        Self { plugin, info }
    }
}

impl ModuleFactory for PluginModule {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: intern(&self.info.id),
            name: intern(&self.info.name),
            description: intern(&self.info.description),
            version: intern(&self.info.version),
            enabled_by_default: self.info.enabled_by_default,
        }
    }

    fn default_settings(&self) -> serde_json::Value {
        self.plugin.default_settings()
    }

    fn start(
        &self,
        _ctx: &JobContext,
        settings: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        self.plugin
            .validate(settings)
            .map_err(|e| ModuleError(e.0))?;
        Ok(Arc::new(PluginJob {
            plugin: Arc::clone(&self.plugin),
            info: Arc::new(self.info.clone()),
            settings: settings.clone(),
        }))
    }
}

struct PluginJob {
    plugin: Arc<dyn Plugin>,
    info: Arc<api::PluginInfo>,
    settings: serde_json::Value,
}

impl ModuleJob for PluginJob {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        let filter = self.info.file_filter.clone()?;
        match self.plugin.file_ingest(&self.settings) {
            Ok(Some(inner)) => Some(Box::new(FileAdapter {
                inner,
                filter,
                info: Arc::clone(&self.info),
            })),
            Ok(None) => None,
            Err(e) => {
                tracing::error!(plugin = %self.info.id, "file ingest did not start: {e}");
                None
            }
        }
    }

    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        let stage = self.info.data_source_stage?;
        match self.plugin.data_source_ingest(&self.settings) {
            Ok(Some(inner)) => Some((
                stage,
                Box::new(DsAdapter {
                    inner,
                    info: Arc::clone(&self.info),
                }),
            )),
            Ok(None) => None,
            Err(e) => {
                tracing::error!(plugin = %self.info.id, "data source ingest did not start: {e}");
                None
            }
        }
    }
}

struct FileAdapter {
    inner: Box<dyn api::FileIngest>,
    filter: FileFilter,
    info: Arc<api::PluginInfo>,
}

impl FileModule for FileAdapter {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let mut info = file_info(&file.rec);
        // Values set earlier in this job (File Type, Hash Lookup).
        info.mime = file.mime().map(str::to_owned);
        info.md5 = file.md5().map(pm_core::decode::to_hex);
        if !self.filter.matches(&info) {
            return Ok(());
        }
        let host = JobHost::new(ctx, &self.info);
        if let Ok(src) = file.content() {
            host.remember(file.rec.id, src);
        }
        self.inner
            .process(&host, &info)
            .map_err(|e| ModuleError(e.0))
    }

    fn shut_down(&mut self, ctx: &JobContext) {
        self.inner.finish(&JobHost::new(ctx, &self.info));
    }
}

struct DsAdapter {
    inner: Box<dyn api::DataSourceIngest>,
    info: Arc<api::PluginInfo>,
}

impl DataSourceModule for DsAdapter {
    fn process(&mut self, ctx: &JobContext, _progress: &DsProgress<'_>) -> Result<(), ModuleError> {
        self.inner
            .process(&JobHost::new(ctx, &self.info))
            .map_err(|e| ModuleError(e.0))
    }
}

/// A file as the plugin API describes it.
pub fn file_info(f: &FileRecord) -> api::FileInfo {
    let kind = match (f.kind, f.origin) {
        (FileKindCode::Directory, _) => api::FileKind::Directory,
        (_, Origin::UnallocBlocks | Origin::Slack) => api::FileKind::Unallocated,
        (FileKindCode::Regular, Origin::Carved) => api::FileKind::Carved,
        (FileKindCode::Regular, Origin::Derived) => api::FileKind::Derived,
        (FileKindCode::Regular, _) => api::FileKind::File,
        _ => api::FileKind::Other,
    };
    let t = |i: usize| f.times.get(i).copied().flatten().map(|(s, _)| s);
    let mut parent_path = f.parent_path.clone();
    if !parent_path.ends_with('/') {
        parent_path.push('/');
    }
    api::FileInfo {
        id: f.id,
        ds_id: f.ds_id,
        name: f.name.clone(),
        parent_path,
        extension: f.extension.to_lowercase(),
        size: f.size,
        kind,
        deleted: f.deleted(),
        mime: f.mime.clone(),
        md5: f.md5.as_deref().map(pm_core::decode::to_hex),
        modified: t(0),
        accessed: t(1),
        changed: t(2),
        created: t(3),
    }
}

/// Custom types a module defined (ids by name).
#[derive(Debug, Default)]
struct CustomTypes {
    artifacts: std::collections::HashMap<String, i64>,
    attributes: std::collections::HashMap<String, i64>,
}

/// The plugin's view of a job. Cheap to clone: clones share the job and
/// the content cache.
#[derive(Clone)]
#[allow(missing_debug_implementations, reason = "holds the job context")]
pub struct JobHost {
    ctx: JobContext,
    info: Arc<api::PluginInfo>,
    /// Custom artifact and attribute types defined by the module, by name.
    custom: Arc<Mutex<CustomTypes>>,
    /// `add_derived` calls so far per parent: each call gets its own output
    /// folder, so a parent can receive files in several batches (e.g. one
    /// per e-mail message) and re-runs still find each batch.
    derived_calls: Arc<Mutex<std::collections::HashMap<i64, u32>>>,
    /// The last opened content (parsers read one file many times).
    open: Arc<Mutex<Option<(i64, SharedSource)>>>,
}

impl JobHost {
    pub fn new(ctx: &JobContext, info: &Arc<api::PluginInfo>) -> Self {
        Self {
            ctx: ctx.clone(),
            info: Arc::clone(info),
            custom: Arc::new(Mutex::new(CustomTypes::default())),
            derived_calls: Arc::new(Mutex::new(std::collections::HashMap::new())),
            open: Arc::new(Mutex::new(None)),
        }
    }

    fn remember(&self, id: i64, src: SharedSource) {
        *self.open.lock() = Some((id, src));
    }

    fn source(&self, id: i64) -> api::Result<SharedSource> {
        if let Some((open, src)) = &*self.open.lock()
            && *open == id
        {
            return Ok(src.clone());
        }
        let rec = self.record(id)?;
        let src = self.ctx.case.content(&rec).map_err(err)?;
        self.remember(id, src.clone());
        Ok(src)
    }

    fn record(&self, id: i64) -> api::Result<FileRecord> {
        self.ctx
            .case
            .db()
            .file(id)
            .map_err(err)?
            .ok_or_else(|| api::PluginError(format!("no file {id} in the case")))
    }
}

fn err(e: impl std::fmt::Display) -> api::PluginError {
    api::PluginError(e.to_string())
}

fn value(v: api::Value) -> pm_casedb::Value {
    match v {
        api::Value::Text(s) => pm_casedb::Value::Text(s),
        api::Value::Int(i) => pm_casedb::Value::Int(i),
        api::Value::Double(d) => pm_casedb::Value::Double(d),
        api::Value::Bytes(b) => pm_casedb::Value::Bytes(b),
        api::Value::Time(t) => pm_casedb::Value::DateTime(t),
    }
}

fn significance(s: api::Significance) -> pm_casedb::Significance {
    match s {
        api::Significance::Unknown => pm_casedb::Significance::Unknown,
        api::Significance::LikelyNone => pm_casedb::Significance::LikelyNone,
        api::Significance::LikelyNotable => pm_casedb::Significance::LikelyNotable,
        api::Significance::None => pm_casedb::Significance::None,
        api::Significance::Notable => pm_casedb::Significance::Notable,
    }
}

impl JobHost {
    /// A result as the case stores it, with its duplicate key (every
    /// attribute takes part in the identity, so re-runs add nothing).
    /// An artifact type id: standard, defined in this job, or custom in
    /// the case.
    fn artifact_type(&self, name: &str) -> api::Result<Option<i64>> {
        if let Some(id) = artifact_type_id(name) {
            return Ok(Some(id));
        }
        if let Some(id) = self.custom.lock().artifacts.get(name) {
            return Ok(Some(*id));
        }
        let id = self
            .ctx
            .case
            .db()
            .artifact_type_id_named(name)
            .map_err(err)?;
        if let Some(id) = id {
            self.custom.lock().artifacts.insert(name.to_owned(), id);
        }
        Ok(id)
    }

    /// An attribute type id, like [`Self::artifact_type`].
    fn attribute_type(&self, name: &str) -> api::Result<Option<i64>> {
        if let Some(id) = attribute_type_id(name) {
            return Ok(Some(id));
        }
        if let Some(id) = self.custom.lock().attributes.get(name) {
            return Ok(Some(*id));
        }
        let id = self
            .ctx
            .case
            .db()
            .attribute_type_id_named(name)
            .map_err(err)?;
        if let Some(id) = id {
            self.custom.lock().attributes.insert(name.to_owned(), id);
        }
        Ok(id)
    }

    fn artifact(&self, r: api::NewResult) -> api::Result<(NewArtifact, Vec<i64>)> {
        let type_id = self.artifact_type(&r.artifact_type)?.ok_or_else(|| {
            api::PluginError(format!("unknown artifact type {}", r.artifact_type))
        })?;
        let source = self.info.name.clone();
        let attributes = r
            .attributes
            .into_iter()
            .map(|a| {
                let id = self.attribute_type(&a.kind)?.ok_or_else(|| {
                    api::PluginError(format!("unknown attribute type {}", a.kind))
                })?;
                Ok((id, value(a.value), source.clone()))
            })
            .collect::<api::Result<Vec<_>>>()?;
        // Every attribute takes part in the identity except attachment
        // references, which hold object ids that a re-run does not get back.
        let attachments = attribute_type_id("TSK_ATTACHMENTS");
        let key: Vec<i64> = attributes
            .iter()
            .map(|a| a.0)
            .filter(|t| Some(*t) != attachments)
            .collect();
        Ok((
            NewArtifact {
                type_id,
                ds_id: self.ctx.ds_id,
                file_id: r.file_id,
                source_artifact_id: None,
                score: r.significance.map(|s| (significance(s), Priority::Normal)),
                conclusion: r.conclusion,
                configuration: r.configuration,
                justification: r.justification,
                created_at: self.ctx.now(),
                attributes,
            },
            key,
        ))
    }
}

impl Host for JobHost {
    fn data_source(&self) -> api::DataSourceInfo {
        let ds = self
            .ctx
            .case
            .db()
            .data_source(self.ctx.ds_id)
            .ok()
            .flatten();
        api::DataSourceInfo {
            id: self.ctx.ds_id,
            name: ds.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
            timezone: ds
                .as_ref()
                .map_or_else(|| "UTC".into(), |d| d.timezone.clone()),
            size: ds.map_or(0, |d| d.size),
        }
    }

    fn read(&self, file_id: i64, offset: u64, buf: &mut [u8]) -> api::Result<usize> {
        let src = self.source(file_id)?;
        if offset >= src.size() {
            return Ok(0);
        }
        src.read_at(offset, buf).map_err(err)
    }

    fn file(&self, file_id: i64) -> api::Result<Option<api::FileInfo>> {
        Ok(self
            .ctx
            .case
            .db()
            .file(file_id)
            .map_err(err)?
            .as_ref()
            .map(file_info))
    }

    fn find_files(&self, q: &api::FindFiles) -> api::Result<Vec<api::FileInfo>> {
        let limit = q
            .limit
            .unwrap_or(api::MAX_FIND_FILES)
            .min(api::MAX_FIND_FILES) as usize;
        let query = FileQuery {
            ds_id: Some(self.ctx.ds_id),
            regular_only: true,
            name_contains: q.name.clone(),
            extensions: q.extension.iter().cloned().collect(),
            mime: q.mime.clone(),
            ..FileQuery::default()
        };
        let path = q.path_contains.as_deref().map(str::to_lowercase);
        let mut out = Vec::new();
        let mut after = 0;
        while out.len() < limit {
            let page = self
                .ctx
                .case
                .db()
                .query_files(&query, after, 5_000)
                .map_err(err)?;
            let Some(last) = page.last() else { break };
            after = last.id;
            out.extend(
                page.iter()
                    .filter(|f| {
                        f.size > 0
                            && f.stream.is_none()
                            && q.name
                                .as_deref()
                                .is_none_or(|n| f.name.eq_ignore_ascii_case(n))
                            && path
                                .as_deref()
                                .is_none_or(|p| f.parent_path.to_lowercase().contains(p))
                    })
                    .map(file_info),
            );
        }
        out.truncate(limit);
        Ok(out)
    }

    fn children(&self, file_id: i64) -> api::Result<Vec<api::FileInfo>> {
        Ok(self
            .ctx
            .case
            .db()
            .children(file_id)
            .map_err(err)?
            .iter()
            .map(file_info)
            .collect())
    }

    fn post(&self, r: api::NewResult) -> api::Result<()> {
        let (artifact, key) = self.artifact(r)?;
        self.ctx.post_artifact(artifact, &key);
        Ok(())
    }

    fn post_communication(&self, r: api::NewResult, c: api::Communication) -> api::Result<()> {
        let (artifact, key) = self.artifact(r)?;
        let relationship = c.relationship.map(|(members, kind, time)| {
            let kind = match kind {
                api::RelationshipKind::Message => RelationshipKind::Message,
                api::RelationshipKind::CallLog => RelationshipKind::CallLog,
                api::RelationshipKind::Contact => RelationshipKind::Contact,
            };
            (members, kind, time)
        });
        self.ctx.post_communication(pm_ingest::NewCommunication {
            artifact,
            key,
            module: self.info.name.clone(),
            accounts: c
                .accounts
                .into_iter()
                .map(|a| (a.account_type, a.id))
                .collect(),
            relationship,
        });
        Ok(())
    }

    fn add_derived(&self, parent_id: i64, files: Vec<api::DerivedFile>) -> api::Result<Vec<i64>> {
        if let Some(f) = files.iter().find(|f| f.data.len() > api::MAX_DERIVED_FILE) {
            return Err(api::PluginError(format!(
                "derived file {} is larger than {} bytes",
                f.name,
                api::MAX_DERIVED_FILE
            )));
        }
        let parent = self.record(parent_id)?;
        let mut readers: Vec<std::io::Cursor<Vec<u8>>> = Vec::with_capacity(files.len());
        let mut meta = Vec::with_capacity(files.len());
        for f in files {
            let times = crate::derive::Times {
                modified: f.modified,
                accessed: f.accessed,
                created: f.created,
            };
            meta.push((f.name, times));
            readers.push(std::io::Cursor::new(f.data));
        }
        let streams = meta
            .into_iter()
            .zip(readers.iter_mut())
            .map(|((name, times), data)| crate::derive::Stream { name, times, data });
        let batch = {
            let mut calls = self.derived_calls.lock();
            let n = calls.entry(parent_id).or_insert(0);
            let b = *n;
            *n += 1;
            b
        };
        let folder = match batch {
            0 => output_folder(&self.info.id),
            n => format!("{}.{n}", output_folder(&self.info.id)),
        };
        let ids = crate::derive::add_children(&self.ctx, &parent, &folder, streams)
            .map_err(|e| api::PluginError(e.0))?;
        // `None`: not added (empty name, cancelled, or added by an earlier
        // run of the module).
        Ok(ids.into_iter().map(|i| i.unwrap_or(0)).collect())
    }

    fn log(&self, level: api::Level, message: &str) {
        let id = &self.info.id;
        match level {
            api::Level::Debug => tracing::debug!(plugin = %id, "{message}"),
            api::Level::Info => tracing::info!(plugin = %id, "{message}"),
            api::Level::Warn => tracing::warn!(plugin = %id, "{message}"),
            api::Level::Error => tracing::error!(plugin = %id, "{message}"),
        }
    }

    fn message(&self, m: api::Message) {
        let kind = match m.level {
            api::Level::Debug | api::Level::Info => MessageKind::Info,
            api::Level::Warn => MessageKind::Warning,
            api::Level::Error => MessageKind::Error,
        };
        let mut msg = IngestMessage::new(kind, &self.info.name, m.subject).details(m.details);
        if let Some(id) = m.file_id {
            msg = msg.file(id);
        }
        self.ctx.post_message(msg);
    }

    fn is_cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }

    fn define_attribute_type(
        &self,
        name: &str,
        display: &str,
        kind: api::ValueKind,
    ) -> api::Result<()> {
        use pm_casedb::catalog::ValueType as V;
        if attribute_type_id(name).is_some() {
            return Ok(());
        }
        let vt = match kind {
            api::ValueKind::String => V::String,
            api::ValueKind::Integer => V::Integer,
            api::ValueKind::Long => V::Long,
            api::ValueKind::Double => V::Double,
            api::ValueKind::Bytes => V::Bytes,
            api::ValueKind::DateTime => V::DateTime,
            api::ValueKind::Json => V::Json,
        };
        let id = self
            .ctx
            .case
            .db()
            .attribute_type(name, display, vt)
            .map_err(err)?;
        self.custom.lock().attributes.insert(name.to_owned(), id);
        Ok(())
    }

    fn define_artifact_type(
        &self,
        name: &str,
        display: &str,
        category: api::ArtifactCategory,
    ) -> api::Result<()> {
        if artifact_type_id(name).is_some() {
            return Ok(());
        }
        let cat = match category {
            api::ArtifactCategory::DataArtifact => pm_casedb::catalog::Category::DataArtifact,
            api::ArtifactCategory::AnalysisResult => pm_casedb::catalog::Category::AnalysisResult,
        };
        let id = self
            .ctx
            .case
            .db()
            .artifact_type(name, display, cat)
            .map_err(err)?;
        self.custom.lock().artifacts.insert(name.to_owned(), id);
        Ok(())
    }

    fn shared(&self) -> Arc<dyn Host + Send + Sync> {
        Arc::new(self.clone())
    }
}

/// A WebAssembly plugin loaded from the plugins folder.
#[derive(Clone)]
pub struct LoadedPlugin {
    pub info: api::PluginInfo,
    pub dir: std::path::PathBuf,
    pub limits: pm_plugin_wasm::Limits,
    pub module: Arc<dyn ModuleFactory>,
}

impl std::fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("id", &self.info.id)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// The plugins of a plugins folder: those that loaded, and why the others
/// did not.
#[derive(Debug, Clone, Default)]
pub struct PluginSet {
    pub plugins: Vec<LoadedPlugin>,
    pub errors: Vec<pm_plugin_wasm::LoadError>,
}

impl PluginSet {
    /// Loads every plugin folder below `dir` (compiling each component;
    /// run it off the UI thread). Plugins whose id collides with a
    /// built-in module or an earlier plugin are refused.
    pub fn load(dir: &std::path::Path) -> Self {
        let mut taken: HashSet<String> = crate::core_modules()
            .iter()
            .map(|m| m.info().id.to_owned())
            .collect();
        let mut set = Self::default();
        for r in pm_plugin_wasm::load_dir(dir) {
            match r {
                Ok(p) => {
                    let info = p.info();
                    if !taken.insert(info.id.clone()) {
                        set.errors.push(pm_plugin_wasm::LoadError {
                            path: p.dir().to_path_buf(),
                            reason: format!("another module already has the id {:?}", info.id),
                        });
                        continue;
                    }
                    set.plugins.push(LoadedPlugin {
                        dir: p.dir().to_path_buf(),
                        limits: p.limits(),
                        module: Arc::new(PluginModule::new(p)),
                        info,
                    });
                }
                Err(e) => set.errors.push(e),
            }
        }
        set
    }

    /// Ingest modules: the built-ins, then the plugins (Autopsy runs
    /// third-party modules after its own).
    pub fn modules(&self) -> Vec<Arc<dyn ModuleFactory>> {
        let mut m = crate::core_modules();
        m.extend(self.plugins.iter().map(|p| Arc::clone(&p.module)));
        m
    }
}
