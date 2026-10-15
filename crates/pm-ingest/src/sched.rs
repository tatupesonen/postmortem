//! File-task scheduling for one job (Autopsy's `IngestTasksScheduler`).
//!
//! Two queues feed the file workers:
//!
//! * the **normal** queue, filled by the producer that streams the data
//!   source's files from the case database (bounded: the producer waits
//!   when it is full, so memory does not grow with the file count);
//! * the **high-priority** queue for files that modules add while the job
//!   runs (derived and carved files, Autopsy `addFilesToJob`). Workers
//!   always drain it first, and new files go to its front, so an archive's
//!   members are analysed before the job moves on.
//!
//! A worker may only stop when nothing more can arrive: both queues are
//! empty, no task is running (a running task may add derived files), and
//! no *hold* is open. The producer and every running stage-1 data-source
//! module own a hold, because they can still add work.

use std::collections::VecDeque;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use pm_casedb::FileRecord;

/// How long an idle worker sleeps before re-checking cancellation.
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Default)]
struct State {
    high: VecDeque<FileRecord>,
    normal: VecDeque<FileRecord>,
    /// Tasks taken by workers and not yet finished.
    running: usize,
    /// Open [`Hold`]s.
    holds: usize,
}

impl State {
    fn idle(&self) -> bool {
        self.high.is_empty() && self.normal.is_empty() && self.running == 0 && self.holds == 0
    }
}

#[derive(Debug)]
pub(crate) struct Scheduler {
    state: Mutex<State>,
    cv: Condvar,
    /// Maximum length of the normal queue.
    capacity: usize,
}

/// Keeps workers alive while something may still add tasks.
#[derive(Debug)]
pub(crate) struct Hold<'a>(&'a Scheduler);

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mut st = self.0.state.lock();
        st.holds = st.holds.saturating_sub(1);
        drop(st);
        self.0.cv.notify_all();
    }
}

/// A task taken by a worker; finishing (dropping) it may end the job.
#[derive(Debug)]
pub(crate) struct Task<'a> {
    sched: &'a Scheduler,
    pub(crate) file: FileRecord,
}

impl Drop for Task<'_> {
    fn drop(&mut self) {
        let mut st = self.sched.state.lock();
        st.running = st.running.saturating_sub(1);
        let wake = st.idle() || !st.high.is_empty();
        drop(st);
        if wake {
            self.sched.cv.notify_all();
        }
    }
}

impl Scheduler {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            capacity: capacity.max(1),
        }
    }

    pub(crate) fn hold(&self) -> Hold<'_> {
        self.state.lock().holds += 1;
        Hold(self)
    }

    /// Queues a file from the producer, waiting while the queue is full.
    /// Returns `false` if `cancelled` became true while waiting.
    pub(crate) fn push_normal(&self, f: FileRecord, cancelled: &dyn Fn() -> bool) -> bool {
        let mut st = self.state.lock();
        while st.normal.len() >= self.capacity {
            if cancelled() {
                return false;
            }
            self.cv.wait_for(&mut st, POLL);
        }
        st.normal.push_back(f);
        drop(st);
        self.cv.notify_one();
        true
    }

    /// Queues files ahead of everything else, keeping their order.
    pub(crate) fn push_high(&self, files: Vec<FileRecord>) {
        if files.is_empty() {
            return;
        }
        let mut st = self.state.lock();
        for f in files.into_iter().rev() {
            st.high.push_front(f);
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Files waiting in either queue.
    pub(crate) fn queued(&self) -> usize {
        let st = self.state.lock();
        st.high.len() + st.normal.len()
    }

    /// Drops every queued task (cancellation).
    pub(crate) fn clear(&self) {
        let mut st = self.state.lock();
        st.high.clear();
        st.normal.clear();
        drop(st);
        self.cv.notify_all();
    }

    /// The next task, waiting for one if work may still arrive; `None`
    /// when the phase is over or `cancelled` is true.
    pub(crate) fn next(&self, cancelled: &dyn Fn() -> bool) -> Option<Task<'_>> {
        let mut st = self.state.lock();
        loop {
            if cancelled() {
                return None;
            }
            if let Some(file) = st.high.pop_front().or_else(|| st.normal.pop_front()) {
                st.running += 1;
                drop(st);
                // The producer may be waiting for space.
                self.cv.notify_all();
                return Some(Task { sched: self, file });
            }
            if st.running == 0 && st.holds == 0 {
                drop(st);
                self.cv.notify_all();
                return None;
            }
            self.cv.wait_for(&mut st, POLL);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use pm_casedb::{FileKindCode, NewFile, Origin};

    use super::*;

    fn rec(id: i64) -> FileRecord {
        NewFile::new(
            1,
            None,
            "/",
            format!("f{id}"),
            Origin::Derived,
            FileKindCode::Regular,
        )
        .to_record(id)
    }

    const NEVER: &dyn Fn() -> bool = &|| false;

    #[test]
    fn high_priority_first_in_order() {
        let s = Scheduler::new(10);
        assert!(s.push_normal(rec(1), NEVER));
        s.push_high(vec![rec(2), rec(3)]);
        s.push_high(vec![rec(4)]);
        let order: Vec<i64> = std::iter::from_fn(|| s.next(NEVER).map(|t| t.file.id)).collect();
        assert_eq!(order, [4, 2, 3, 1]);
    }

    /// Workers keep waiting while a running task can still add files, and
    /// all stop once the last task finishes without adding any.
    #[test]
    fn running_tasks_keep_workers_alive() {
        let s = Scheduler::new(4);
        let processed = AtomicUsize::new(0);
        {
            let _producer = s.hold();
            assert!(s.push_normal(rec(1), NEVER));
        }
        std::thread::scope(|sc| {
            for _ in 0..4 {
                sc.spawn(|| {
                    while let Some(t) = s.next(NEVER) {
                        processed.fetch_add(1, Ordering::SeqCst);
                        // Each task spawns two children until depth 5.
                        if t.file.id < 32 {
                            std::thread::sleep(Duration::from_millis(2));
                            s.push_high(vec![rec(t.file.id * 2), rec(t.file.id * 2 + 1)]);
                        }
                    }
                });
            }
        });
        // 1 + 2 + 4 + 8 + 16 + 32 tasks.
        assert_eq!(processed.load(Ordering::SeqCst), 63);
        assert_eq!(s.queued(), 0);
    }

    #[test]
    fn holds_keep_workers_alive() {
        let s = Scheduler::new(4);
        let hold = s.hold();
        std::thread::scope(|sc| {
            let w = sc.spawn(|| s.next(NEVER).map(|t| t.file.id));
            std::thread::sleep(Duration::from_millis(120));
            s.push_high(vec![rec(9)]);
            assert_eq!(w.join().unwrap(), Some(9));
            let w = sc.spawn(|| s.next(NEVER).is_none());
            std::thread::sleep(Duration::from_millis(60));
            drop(hold);
            assert!(w.join().unwrap());
        });
    }

    #[test]
    fn producer_waits_for_space_and_stops_on_cancel() {
        let s = Scheduler::new(1);
        assert!(s.push_normal(rec(1), NEVER));
        let cancel = AtomicBool::new(false);
        std::thread::scope(|sc| {
            let p = sc.spawn(|| s.push_normal(rec(2), &|| cancel.load(Ordering::SeqCst)));
            std::thread::sleep(Duration::from_millis(80));
            cancel.store(true, Ordering::SeqCst);
            assert!(!p.join().unwrap());
        });
        // A cancelled job hands out nothing more.
        assert!(s.next(&|| true).is_none());
        s.clear();
        assert_eq!(s.queued(), 0);
    }
}
