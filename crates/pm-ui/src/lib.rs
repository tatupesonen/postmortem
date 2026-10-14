//! postmortem's egui user interface.
//!
//! Layout follows Autopsy's three panes: a data-source tree (left), a
//! result table (centre) and content viewers (bottom). Any viewer can be
//! popped out into its own native window (egui deferred viewports).

mod app;
mod format;
mod model;
mod table;
mod task;
mod viewers;

use std::path::PathBuf;

pub use app::{Persisted, PostmortemApp};
pub use model::{Content, DataSource, FileRow, NodeId};

/// Starts the GUI, optionally opening the given images immediately.
pub fn run(open: Vec<PathBuf>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
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
