//! WebAssembly plugins (ADR-022) end to end: the example and test plugins
//! under `plugins/` are built for `wasm32-wasip2`, loaded by
//! `pm-plugin-wasm` and run through ingest on the demo case.
//!
//! Skipped (with a message) when the `wasm32-wasip2` target is missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    reason = "test code"
)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::DemoCase;
use pm_modules::plugin_host::PluginModule;
use pm_plugin_api::Plugin;
use pm_plugin_wasm::WasmPlugin;

/// Builds the plugin crate at `plugins/<rel>` and installs it (component +
/// manifest) in a fresh plugin folder under `into`; `None` without the
/// wasm target.
fn install(rel: &str, into: &Path) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(rel);
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("plugins");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args([
            "build",
            "--release",
            "--quiet",
            "--target",
            "wasm32-wasip2",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .env_remove("RUSTFLAGS")
        .output()
        .unwrap();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("wasm32-wasip2") && err.contains("target") {
            eprintln!("wasm32-wasip2 target not installed; skipping: {err}");
            return None;
        }
        panic!("building {rel} failed: {err}");
    }
    let manifest = std::fs::read_to_string(root.join("plugin.toml")).unwrap();
    let component = manifest
        .lines()
        .find_map(|l| l.strip_prefix("component = "))
        .map(|c| c.trim_matches('"').to_owned())
        .unwrap();
    let dir = into.join(rel.rsplit('/').next().unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::copy(
        target.join("wasm32-wasip2/release").join(&component),
        dir.join(&component),
    )
    .unwrap();
    Some(dir)
}

#[test]
fn example_plugin_finds_big_round_files() {
    let plugins = tempfile::tempdir().unwrap();
    let Some(dir) = install("examples/big_round_files", plugins.path()) else {
        return;
    };
    let plugin = Arc::new(WasmPlugin::load(&dir).unwrap());
    let info = plugin.info();
    assert_eq!(info.name, "Big and Round File Finder");
    assert_eq!(plugin.limits().memory, 32 * 1024 * 1024, "manifest limits");
    assert_eq!(plugin.default_settings()["multiple"], 4096);

    let case = DemoCase::new();
    // The demo disk has no 10 MiB files: lower the threshold.
    let (summary, _) = case.run_one(
        Arc::new(PluginModule::new(plugin.clone())),
        serde_json::json!({ "min_size": 1000, "multiple": 512 }),
    );
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    let expected: Vec<i64> = case
        .files()
        .iter()
        .filter(|f| {
            f.kind == pm_casedb::FileKindCode::Regular
                && !matches!(
                    f.origin,
                    pm_casedb::Origin::UnallocBlocks | pm_casedb::Origin::Slack
                )
                && f.size > 1000
                && f.size % 512 == 0
                // NTFS's sparse bad-cluster stream is never scheduled.
                && !f.name.starts_with("$BadClus")
        })
        .map(|f| f.id)
        .collect();
    let mut got: Vec<i64> = case
        .artifacts("TSK_INTERESTING_ITEM")
        .into_iter()
        .filter(|(_, a)| {
            a.get("TSK_SET_NAME").map(String::as_str) == Some("Big and Round File Finder")
        })
        .filter_map(|(a, _)| a.file_id)
        .collect();
    got.sort_unstable();
    got.dedup();
    let mut expected = expected;
    expected.sort_unstable();
    assert!(!expected.is_empty());
    assert_eq!(got, expected);

    // Invalid settings fail validation inside the plugin.
    let e = plugin
        .validate(&serde_json::json!({ "multiple": 0 }))
        .unwrap_err();
    assert!(e.0.contains("multiple must be at least 1"), "{e}");
}

#[test]
fn probe_plugin_uses_every_host_call_and_survives_traps() {
    let plugins = tempfile::tempdir().unwrap();
    let Some(_) = install("tests/probe", plugins.path()) else {
        return;
    };
    let loaded = pm_plugin_wasm::load_dir(plugins.path());
    assert_eq!(loaded.len(), 1);
    let plugin = loaded.into_iter().next().unwrap().unwrap();
    let case = DemoCase::new();
    let (summary, inbox) = case.run_one(
        Arc::new(PluginModule::new(plugin)),
        serde_json::json!({ "crash_on": "meeting notes.txt", "spin_on": "project_plan.txt" }),
    );

    // The panic and the endless loop fail their own files only.
    let errors = summary.errors.join("\n");
    assert!(errors.contains("trapped"), "{errors}");
    assert!(errors.contains("fuel"), "{errors}");

    // Other text files were processed: results, derived files, reads.
    let pw = case.file("/Users/alice/Documents/passwords.txt");
    let hits = case.artifacts("TSK_INTERESTING_FILE_HIT");
    assert!(
        hits.iter().any(|(a, attrs)| a.file_id == Some(pw.id)
            && attrs.get("TSK_COMMENT").map(String::as_str) == Some("router: hunter2")),
        "{hits:?}"
    );
    let child = case
        .files()
        .into_iter()
        .find(|f| f.name == "passwords.txt.first-line")
        .expect("derived file");
    assert_eq!(child.parent_id, Some(pw.id));
    assert_eq!(
        common::read_all(&*case.case.content(&child).unwrap()),
        b"router: hunter2"
    );
    // The data-source stage found the file and posted a message.
    assert!(
        inbox
            .since(0)
            .iter()
            .any(|m| m.subject == "probe: 1 in demo.dd" && m.file_id == Some(pw.id))
    );
}
