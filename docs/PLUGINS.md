# Writing postmortem plugins

Plugins are WebAssembly components that run sandboxed inside postmortem's
ingest pipeline, alongside the built-in modules (ADR-022). They are written
against the same API as the built-in parsers: `pm-plugin-api`.

## Quick start (Rust)

```sh
cp -r plugins/examples/big_round_files my_plugin
# edit Cargo.toml (name), src/lib.rs, plugin.toml
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
mkdir -p ~/postmortem/Plugins/my_plugin
cp plugin.toml target/wasm32-wasip2/release/my_plugin.wasm ~/postmortem/Plugins/my_plugin/
```

Set `component = "my_plugin.wasm"` in `plugin.toml`. Then open
**Tools › Plugins › Reload** in postmortem and enable the module in the
ingest module list (Add Data Source, or Run Ingest Modules).

## The API

```rust
use pm_plugin_sdk::api::*;

#[derive(Default)]
struct MyPlugin;

impl Plugin for MyPlugin {
    fn info(&self) -> PluginInfo { /* id, name, file_filter, data_source_stage */ }
    fn file_ingest(&self, settings: &serde_json::Value) -> Result<Option<Box<dyn FileIngest>>> { … }
    fn data_source_ingest(&self, settings: &serde_json::Value) -> Result<Option<Box<dyn DataSourceIngest>>> { … }
}

pm_plugin_sdk::export_plugin!(MyPlugin);
```

* **File ingest** (`FileIngest::process`) is called for each file that
  matches `PluginInfo::file_filter` (MIME types such as `image/*`,
  extensions, exact names, size cap); the host applies the filter, so
  narrow filters keep plugins fast.
* **Data-source ingest** (`DataSourceIngest::process`) runs once per job in
  the declared stage (1 runs alongside file analysis; 2+ after it).
* **Host calls**: `read` (or `HostReader` for `io::Read + Seek`), `file`,
  `find_files`, `children`, `post` (a `NewResult` with a TSK artifact type
  such as `TSK_INTERESTING_ITEM` and TSK attribute types such as
  `TSK_SET_NAME`), `add_derived` (extracted files, scheduled for analysis),
  `log`, `message` (ingest inbox), `is_cancelled`.
* **Settings** are JSON. `default_settings` provides the defaults shown in
  the ingest module list, where the examiner can edit them; `validate`
  rejects bad settings before the job starts.
* **Re-runs are idempotent**: posting the same result twice on a file adds
  it once; `add_derived` adds a file's children once.

## Limits and sandbox

`plugin.toml`:

```toml
api = "0.1"                  # the pm-plugin-api version you built against
component = "my_plugin.wasm"

[limits]
memory_mb = 256              # linear memory cap per instance
fuel_per_call = 5000000000   # roughly instructions per process call
```

A plugin has no file system, network, environment or clock access beyond
the host calls. A panic, a trap, or an exhausted fuel budget fails that
one call (recorded as a module error); the instance is recreated and the
job continues.

## Examples

* `plugins/examples/big_round_files`: Autopsy's "Big and Round File
  Finder" sample module.
* `plugins/tests/probe`: exercises every host call (used by the tests).
