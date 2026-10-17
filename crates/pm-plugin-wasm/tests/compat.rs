//! Plugins built against an older API minor version still load
//! (`PM_PLUGIN_DIR` = a folder of installed plugins).
#![allow(clippy::unwrap_used, clippy::print_stderr, reason = "test code")]

#[test]
#[ignore = "needs installed plugins"]
fn installed_plugins_load() {
    let Some(dir) = std::env::var_os("PM_PLUGIN_DIR") else {
        return;
    };
    for r in pm_plugin_wasm::load_dir(std::path::Path::new(&dir)) {
        let p = r.unwrap();
        eprintln!("loaded {}", p.dir().display());
    }
}
