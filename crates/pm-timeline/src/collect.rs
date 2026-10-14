//! Builds file-system timeline events (MAC + created times) by walking
//! filesystems, as Autopsy's data-source ingest does via TSK.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pm_fs::walk::{WalkOptions, walk};
use pm_fs::{FileKind, FileSystem};
use rayon::prelude::*;

use crate::{Event, EventType, FileCategory, Subject, Timeline};

/// A filesystem to collect from.
#[derive(Debug, Clone)]
pub struct Source {
    pub ds: u32,
    pub fs_index: u32,
    pub fs: Arc<dyn FileSystem>,
}

/// Shared progress counters (read by the UI while collecting).
#[derive(Debug, Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub events: AtomicU64,
    pub cancelled: AtomicBool,
}

fn seconds(t: Option<jiff::Timestamp>) -> Option<i64> {
    // Autopsy skips zero/unset times; negative (pre-1970) times are kept.
    t.map(jiff::Timestamp::as_second).filter(|&s| s != 0)
}

/// Walks one filesystem, producing subjects and (subject-local) events.
fn collect_one(src: &Source, progress: &Progress) -> (Vec<Subject>, Vec<(u32, i64, EventType)>) {
    let mut subjects = Vec::new();
    let mut events = Vec::new();
    let mut emit = |path: String, meta: &pm_fs::Metadata, deleted: bool, inode: u64| {
        let idx = subjects.len() as u32;
        let category = FileCategory::from_name(&path);
        let t = meta.times;
        for (time, kind) in [
            (t.modified, EventType::FileModified),
            (t.accessed, EventType::FileAccessed),
            (t.created, EventType::FileCreated),
            (t.changed, EventType::FileChanged),
        ] {
            if let Some(s) = seconds(time) {
                events.push((idx, s, kind));
            }
        }
        subjects.push(Subject {
            ds: src.ds,
            fs: src.fs_index,
            inode,
            path: Arc::from(path),
            deleted,
            size: meta.size,
            category,
        });
        progress.files.fetch_add(1, Ordering::Relaxed);
    };
    walk(&*src.fs, &WalkOptions::default(), |item| {
        if progress.cancelled.load(Ordering::Relaxed) {
            return ControlFlow::Break(());
        }
        if let Ok(meta) = &item.meta
            && meta.kind != FileKind::Virtual
        {
            let deleted = !item.entry.allocated || !meta.is_allocated();
            emit(item.path(), meta, deleted, item.entry.inode);
        }
        ControlFlow::Continue(())
    });
    if let Ok(orphans) = src.fs.orphans() {
        for o in orphans {
            if let Ok(meta) = src.fs.metadata(o.inode) {
                emit(format!("/$OrphanFiles/{}", o.name), &meta, true, o.inode);
            }
        }
    }
    progress
        .events
        .fetch_add(events.len() as u64, Ordering::Relaxed);
    (subjects, events)
}

/// Collects events from all `sources` in parallel and builds a timeline.
pub fn collect(
    sources: &[Source],
    data_source_names: Vec<String>,
    progress: &Progress,
) -> Timeline {
    let parts: Vec<_> = sources
        .par_iter()
        .map(|s| collect_one(s, progress))
        .collect();
    let mut subjects = Vec::new();
    let mut events = Vec::new();
    for (subs, evs) in parts {
        let base = subjects.len() as u32;
        events.extend(evs.into_iter().map(|(i, time, kind)| Event {
            time,
            kind,
            subject: base + i,
        }));
        subjects.extend(subs);
    }
    tracing::info!(
        subjects = subjects.len(),
        events = events.len(),
        "timeline collected"
    );
    Timeline::new(events, subjects, data_source_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;
    use pm_testkit::ntfs::{Deleted, FileOpts, NtfsBuilder};

    #[test]
    fn collects_macb_events_from_ntfs() {
        let mut b = NtfsBuilder::new(4);
        b.file("a.txt", b"a");
        b.file_with(
            "gone.txt",
            b"g",
            FileOpts {
                deleted: Deleted::Unindexed,
                ..Default::default()
            },
        );
        let src: pm_core::SharedSource = Arc::new(MemSource::new(b.build()));
        let fs: Arc<dyn FileSystem> = Arc::new(pm_fs_ntfs_shim::open(src));
        let progress = Progress::default();
        let t = collect(
            &[Source {
                ds: 0,
                fs_index: 0,
                fs,
            }],
            vec!["img".into()],
            &progress,
        );
        // Every file contributes M, A, B (created) and C ($MFT modified).
        let a = t
            .subjects()
            .iter()
            .position(|s| &*s.path == "/a.txt")
            .unwrap() as u32;
        let kinds: Vec<_> = t
            .range(i64::MIN, i64::MAX)
            .iter()
            .filter(|e| e.subject == a)
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds.len(), 4);
        let gone = t
            .subjects()
            .iter()
            .find(|s| &*s.path == "/gone.txt")
            .unwrap();
        assert!(gone.deleted);
        assert!(progress.files.load(Ordering::Relaxed) >= 2);
        // All builder timestamps are 2021-03-04 10:20:30 UTC.
        assert!(
            t.range(i64::MIN, i64::MAX)
                .iter()
                .all(|e| e.time == 1_614_853_230)
        );
    }

    /// Tiny indirection so this crate does not depend on pm-fs-ntfs outside tests.
    mod pm_fs_ntfs_shim {
        pub(super) fn open(src: pm_core::SharedSource) -> impl pm_fs::FileSystem {
            pm_fs_ntfs::NtfsFs::open(src).unwrap()
        }
    }
}
