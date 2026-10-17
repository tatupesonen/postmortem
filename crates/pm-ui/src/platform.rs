//! Handing files and folders to the operating system.

use std::path::Path;

/// Opens a file or folder with the platform's default application (a
/// report in the browser, a folder in the file manager).
pub(crate) fn open(path: &Path) {
    let cmd = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    if let Err(e) = std::process::Command::new(cmd).arg(path).spawn() {
        tracing::warn!("cannot open {}: {e}", path.display());
    }
}
