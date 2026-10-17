//! `plugin.toml`, the manifest next to a plugin's component:
//!
//! ```toml
//! api = "0.1"                # pm-plugin-api version the plugin was built for
//! component = "plugin.wasm"  # path relative to the folder (default)
//!
//! [limits]                   # optional; defaults in `Limits::default()`
//! memory_mb = 256
//! fuel_per_call = 5_000_000_000
//! ```
//!
//! Everything else (id, name, filters, stage) comes from the component's
//! own `info()`, so it cannot disagree with the code.

use std::path::{Path, PathBuf};

use pm_plugin_api::Version;
use serde::Deserialize;

use crate::LoadError;

pub const FILE_NAME: &str = "plugin.toml";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `"major.minor"` of the API the plugin was built against.
    pub api: String,
    #[serde(default = "default_component")]
    pub component: PathBuf,
    #[serde(default)]
    pub limits: ManifestLimits,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestLimits {
    pub memory_mb: Option<u64>,
    pub fuel_per_call: Option<u64>,
}

fn default_component() -> PathBuf {
    PathBuf::from("plugin.wasm")
}

impl Manifest {
    /// Reads and checks `<dir>/plugin.toml`.
    pub fn read(dir: &Path) -> Result<Self, LoadError> {
        let path = dir.join(FILE_NAME);
        let fail = |reason: String| LoadError {
            path: path.clone(),
            reason,
        };
        let text = std::fs::read_to_string(&path).map_err(|e| fail(e.to_string()))?;
        let m: Self = toml::from_str(&text).map_err(|e| fail(e.to_string()))?;
        let api = m
            .api_version()
            .ok_or_else(|| fail(format!("bad api version {:?}", m.api)))?;
        if !api.runs_on(pm_plugin_api::API_VERSION) {
            return Err(fail(format!(
                "built for plugin API {api}; this postmortem provides {}",
                pm_plugin_api::API_VERSION
            )));
        }
        if m.component.is_absolute() || m.component.components().any(|c| c.as_os_str() == "..") {
            return Err(fail(
                "component must be a path inside the plugin folder".into(),
            ));
        }
        Ok(m)
    }

    pub fn api_version(&self) -> Option<Version> {
        let (major, minor) = self.api.trim().split_once('.')?;
        Some(Version {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) {
        std::fs::write(dir.join(FILE_NAME), text).unwrap();
    }

    #[test]
    fn manifests_are_checked() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "api = \"0.1\"\n[limits]\nmemory_mb = 64\n");
        let m = Manifest::read(d.path()).unwrap();
        assert_eq!(m.component, PathBuf::from("plugin.wasm"));
        assert_eq!(m.limits.memory_mb, Some(64));

        write(d.path(), "api = \"0.9\"\n");
        let e = Manifest::read(d.path()).unwrap_err();
        assert!(e.reason.contains("built for plugin API 0.9"), "{e}");
        write(d.path(), "api = \"0.1\"\ncomponent = \"../x.wasm\"\n");
        assert!(Manifest::read(d.path()).is_err(), "no escaping the folder");
        write(d.path(), "api = \"0.1\"\nunknown = 1\n");
        assert!(Manifest::read(d.path()).is_err());
    }
}
