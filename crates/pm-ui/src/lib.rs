//! postmortem's egui user interface.
//!
//! Layout follows Autopsy's three panes: a data-source tree (left), a
//! result table (centre) and content viewers (bottom). Any viewer can be
//! popped out into its own native window (egui deferred viewports).

mod add_source;
mod analysis;
mod app;
mod case_nodes;
mod case_tree;
mod case_ui;
mod discovery;
mod file_search_ui;
mod format;
mod gallery;
mod grid;
mod hosts_ui;
mod icons;
mod index;
mod keyword_search_ui;
mod model;
mod options_ui;
mod plugins;
mod results;
mod sortable;
mod summary;
mod table;
mod tags_ui;
mod task;
mod thumbs;
mod timeline;
mod timeline_artifacts;
mod timeline_chart;
mod timeline_details;
mod viewers;
mod views;

use std::path::PathBuf;

pub use add_source::IngestPrefs;
pub use app::{Persisted, PostmortemApp};
pub use model::{Content, DataSource, FileRow, NodeId};
pub use tags_ui::TagDefinition;

/// Starts the GUI, optionally opening the given images immediately.
/// App icon (512 px PNG rendered from `assets/brand/postmortem-icon.svg`).
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../../../assets/brand/postmortem-icon-512.png");
/// Wordmark for About / welcome screens, one per theme (transparent background).
pub(crate) const WORDMARK_ON_DARK_PNG: &[u8] =
    include_bytes!("../../../assets/brand/postmortem-wordmark-on-dark.png");
pub(crate) const WORDMARK_ON_LIGHT_PNG: &[u8] =
    include_bytes!("../../../assets/brand/postmortem-wordmark-on-light.png");

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
