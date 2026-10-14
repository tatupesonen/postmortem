//! The shared file index used by all analysis tools.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;
use pm_index::{FileIndex, Progress, Source};

use crate::model::DataSource;

#[derive(Debug)]
pub(crate) enum IndexStatus {
    Building {
        progress: Arc<Progress>,
        sources: usize,
    },
    Ready(Arc<FileIndex>),
}

/// Index state shared between the main window and tool windows.
#[derive(Debug, Clone)]
pub(crate) struct SharedIndex(Arc<Mutex<IndexStatus>>);

impl SharedIndex {
    /// Starts building an index of `sources` in the background.
    pub(crate) fn build(ctx: &egui::Context, sources: &[DataSource]) -> Self {
        let progress = Arc::new(Progress::default());
        let shared = Self(Arc::new(Mutex::new(IndexStatus::Building {
            progress: Arc::clone(&progress),
            sources: sources.len(),
        })));
        let fs_sources: Vec<Source> = sources
            .iter()
            .flat_map(|ds| {
                ds.evidence
                    .filesystems
                    .iter()
                    .enumerate()
                    .map(move |(i, f)| Source {
                        ds: ds.id as u32,
                        fs_index: i as u32,
                        fs: Arc::clone(&f.fs),
                    })
            })
            .collect();
        let names: Vec<String> = sources.iter().map(|d| d.name.clone()).collect();
        let out = shared.clone();
        let ctx = ctx.clone();
        crate::task::spawn_detached(move || {
            let idx = Arc::new(FileIndex::build(&fs_sources, names, &progress));
            *out.0.lock() = IndexStatus::Ready(idx);
            ctx.request_repaint();
        });
        shared
    }

    pub(crate) fn ready(&self) -> Option<Arc<FileIndex>> {
        match &*self.0.lock() {
            IndexStatus::Ready(i) => Some(Arc::clone(i)),
            IndexStatus::Building { .. } => None,
        }
    }

    /// Files examined so far while building (`None` once ready).
    pub(crate) fn building_count(&self) -> Option<u64> {
        match &*self.0.lock() {
            IndexStatus::Building { progress, .. } => Some(progress.files.load(Ordering::Relaxed)),
            IndexStatus::Ready(_) => None,
        }
    }

    /// Number of data sources this index covers.
    pub(crate) fn covers(&self) -> usize {
        match &*self.0.lock() {
            IndexStatus::Building { sources, .. } => *sources,
            IndexStatus::Ready(i) => i.data_sources.len(),
        }
    }

    /// Shows a "still indexing" placeholder; returns the index when ready.
    pub(crate) fn ready_or_progress(&self, ui: &mut egui::Ui) -> Option<Arc<FileIndex>> {
        if let Some(i) = self.ready() {
            return Some(i);
        }
        let n = self.building_count().unwrap_or(0);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.spinner();
            ui.heading("Indexing data sources…");
            ui.label(format!("{} files examined", crate::format::grouped(n)));
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(200));
        None
    }
}
