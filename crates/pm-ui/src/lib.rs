//! postmortem's egui user interface.
//!
//! Layout follows Autopsy's three panes: a data-source tree (left), a
//! result table (centre) and content viewers (bottom). Any viewer can be
//! popped out into its own native window (egui deferred viewports).

mod app;
mod format;
mod gallery;
mod index;
mod model;
mod summary;
mod table;
mod task;
mod timeline;
mod viewers;

use std::path::PathBuf;

pub use app::{Persisted, PostmortemApp};
pub use model::{Content, DataSource, FileRow, NodeId};

/// Starts the GUI, optionally opening the given images immediately.
/// App icon (512 px PNG rendered from `assets/brand/postmortem-icon.svg`).
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../../../assets/brand/postmortem-icon-512.png");
/// Wordmark for About / welcome screens.
pub(crate) const WORDMARK_PNG: &[u8] =
    include_bytes!("../../../assets/brand/postmortem-wordmark.png");

pub fn run(open: Vec<PathBuf>) -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default();
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("postmortem")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "postmortem",
        options,
        Box::new(move |cc| Ok(Box::new(PostmortemApp::new(cc, open)))),
    )
}
