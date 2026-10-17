//! The Data Sources tree from the case database (Autopsy's data-source
//! hierarchy: data source → volumes → file system root → directories).
//!
//! The structure above the root directories is small and loaded in one
//! background query; directory children are listed per node on demand
//! (`CaseDb::children`), so the tree never walks the evidence.

use std::sync::Arc;
use std::time::{Duration, Instant};

use pm_case::Case;
use pm_casedb::{DataSourceRecord, FileRecord, FileSystemRecord, VolumeRecord};

use crate::task::Task;

/// How often the tree refreshes while data sources are being added.
const REFRESH: Duration = Duration::from_secs(2);

/// A file system and its root directory row.
#[derive(Debug, Clone)]
pub(crate) struct FsRoot {
    pub(crate) fs: FileSystemRecord,
    pub(crate) root: FileRecord,
}

/// A volume-system slot with the file systems found in it.
#[derive(Debug, Clone)]
pub(crate) struct VolumeNode {
    pub(crate) volume: VolumeRecord,
    pub(crate) roots: Vec<FsRoot>,
}

impl VolumeNode {
    pub(crate) fn unallocated(&self) -> bool {
        self.volume.kind == 1
    }

    /// Autopsy's volume label: `vol2 (NTFS / exFAT: 2048-1050623)`.
    pub(crate) fn label(&self) -> String {
        format!(
            "vol{} ({}: {}-{})",
            self.volume.addr,
            self.volume.description,
            self.volume.start_sector,
            (self.volume.start_sector + self.volume.sector_count).saturating_sub(1)
        )
    }
}

/// One data source of the case.
#[derive(Debug, Clone)]
pub(crate) struct DsTree {
    pub(crate) ds: DataSourceRecord,
    /// Volume-system slots except meta slots, in address order.
    pub(crate) volumes: Vec<VolumeNode>,
    /// File systems not inside a volume (no volume system).
    pub(crate) loose: Vec<FsRoot>,
}

/// Loads the tree structure of every data source.
pub(crate) fn load(case: &Case) -> Result<Vec<DsTree>, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut out = Vec::new();
    for ds in db.data_sources().map_err(err)? {
        let fss = db.file_systems(ds.id).map_err(err)?;
        let mut roots: Vec<FsRoot> = Vec::with_capacity(fss.len());
        for fs in fss {
            if let Some(root) = db.fs_root(fs.id).map_err(err)? {
                roots.push(FsRoot { fs, root });
            }
        }
        let mut volumes: Vec<VolumeNode> = db
            .volumes(ds.id)
            .map_err(err)?
            .into_iter()
            .filter(|v| v.kind != 2)
            .map(|volume| VolumeNode {
                volume,
                roots: Vec::new(),
            })
            .collect();
        volumes.sort_by_key(|v| v.volume.addr);
        let mut loose = Vec::new();
        for r in roots {
            match volumes
                .iter_mut()
                .find(|v| Some(v.volume.id) == r.fs.volume_id)
            {
                Some(v) => v.roots.push(r),
                None => loose.push(r),
            }
        }
        out.push(DsTree { ds, volumes, loose });
    }
    Ok(out)
}

/// The tree structure, reloaded in the background when stale.
#[derive(Debug, Default)]
pub(crate) struct Sources {
    task: Option<Task<Result<Vec<DsTree>, String>>>,
    current: Arc<Vec<DsTree>>,
    refreshed: Option<Instant>,
    error: Option<String>,
}

impl Sources {
    /// Latest structure; refreshes every couple of seconds while `live`
    /// (data sources being added).
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<Case>>,
        live: bool,
    ) -> Arc<Vec<DsTree>> {
        if let Some(t) = &mut self.task
            && let Some(r) = t.poll()
        {
            match r {
                Ok(v) => {
                    self.current = Arc::new(v.clone());
                    self.error = None;
                }
                Err(e) => self.error = Some(e.clone()),
            }
            self.task = None;
            self.refreshed = Some(Instant::now());
        }
        let stale = self
            .refreshed
            .is_none_or(|t| live && t.elapsed() >= REFRESH);
        if self.task.is_none()
            && stale
            && let Some(case) = case
        {
            let case = Arc::clone(case);
            self.task = Some(Task::spawn(ctx, move || load(&case)));
        }
        Arc::clone(&self.current)
    }

    /// Whether the first load is still running.
    pub(crate) fn loading(&self) -> bool {
        self.refreshed.is_none() && self.task.is_some()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Reloads in the background, keeping the current structure meanwhile
    /// (analysis finished: new derived files, data sources).
    pub(crate) fn refresh(&mut self) {
        self.refreshed = None;
    }

    /// Forces a reload (case changed, data source added or removed).
    pub(crate) fn invalidate(&mut self) {
        self.refreshed = None;
        self.task = None;
        self.current = Arc::new(Vec::new());
    }

    /// The data source with `id` in the last loaded structure.
    pub(crate) fn find(&self, id: i64) -> Option<&DsTree> {
        self.current.iter().find(|d| d.ds.id == id)
    }
}
