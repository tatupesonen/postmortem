//! Plugins (ADR-022): the process-wide set of WebAssembly plugins loaded
//! from the plugins folder, and Tools › Plugins.
//!
//! Plugins are compiled on a background thread at start-up and on
//! "Reload"; until then the module lists hold only the built-ins, so the
//! UI never waits for a compiler.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;
use pm_ingest::ModuleFactory;
use pm_modules::plugin_host::PluginSet;

static PLUGINS: LazyLock<RwLock<Arc<PluginSet>>> =
    LazyLock::new(|| RwLock::new(Arc::new(PluginSet::default())));

/// `$POSTMORTEM_PLUGINS`, or `~/postmortem/Plugins` (next to the default
/// case folder).
pub(crate) fn plugins_dir() -> PathBuf {
    std::env::var_os("POSTMORTEM_PLUGINS").map_or_else(
        || {
            std::env::home_dir()
                .unwrap_or_default()
                .join("postmortem")
                .join("Plugins")
        },
        PathBuf::from,
    )
}

/// The loaded plugins.
pub(crate) fn current() -> Arc<PluginSet> {
    Arc::clone(&PLUGINS.read())
}

/// Every ingest module: the built-ins and the loaded plugins.
pub(crate) fn modules() -> Vec<Arc<dyn ModuleFactory>> {
    current().modules()
}

/// Loads the plugins folder in the background and swaps the set in.
pub(crate) fn reload(ctx: &egui::Context) {
    let ctx = ctx.clone();
    let dir = plugins_dir();
    let spawned = std::thread::Builder::new()
        .name("pm-plugins".into())
        .spawn(move || {
            let set = PluginSet::load(&dir);
            for e in &set.errors {
                tracing::warn!("plugin not loaded: {e}");
            }
            *PLUGINS.write() = Arc::new(set);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        tracing::error!("cannot load plugins: {e}");
    }
}

/// Tools › Plugins; `false` when the window was closed.
pub(crate) fn window(ctx: &egui::Context) -> bool {
    {
        let mut open = true;
        let set = current();
        let dir = plugins_dir();
        egui::Window::new("Plugins")
            .open(&mut open)
            .default_size([640.0, 420.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Plugins folder:");
                    ui.monospace(dir.display().to_string());
                });
                ui.horizontal(|ui| {
                    if ui.button("Open Plugins Folder").clicked() {
                        if let Err(e) = std::fs::create_dir_all(&dir) {
                            tracing::warn!("cannot create {}: {e}", dir.display());
                        }
                        crate::platform::open(&dir);
                    }
                    if ui.button("Reload").clicked() {
                        reload(ctx);
                    }
                });
                ui.weak(
                    "Each plugin is a folder with a plugin.toml and a WebAssembly component. \
                     Enable plugins per job in the ingest module list.",
                );
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if set.plugins.is_empty() && set.errors.is_empty() {
                        ui.weak("No plugins installed.");
                    }
                    for p in &set.plugins {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.strong(&p.info.name);
                                ui.weak(format!("{} · {}", p.info.version, p.info.id));
                            });
                            if !p.info.description.is_empty() {
                                ui.label(&p.info.description);
                            }
                            let mut kinds = Vec::new();
                            if p.info.file_filter.is_some() {
                                kinds.push("file ingest".to_owned());
                            }
                            if let Some(s) = p.info.data_source_stage {
                                kinds.push(format!("data source ingest (stage {s})"));
                            }
                            ui.weak(format!(
                                "{} · memory {} · {}",
                                kinds.join(", "),
                                pm_core::units::format_size(p.limits.memory as u64),
                                p.dir.display()
                            ));
                        });
                    }
                    for e in &set.errors {
                        ui.colored_label(ui.visuals().error_fg_color, format!("Not loaded: {e}"));
                    }
                });
            });
        open
    }
}
