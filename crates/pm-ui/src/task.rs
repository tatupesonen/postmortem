//! Background work for the UI.
//!
//! The UI thread never performs evidence IO beyond tiny reads. Anything that
//! can take time (opening images, listing directories, extracting strings,
//! decoding pictures) runs on the rayon pool as a [`Task`]; the UI polls
//! it each frame and egui is woken when the result arrives.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;

static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Number of background tasks currently running (for the status bar).
pub(crate) fn running() -> usize {
    RUNNING.load(Ordering::Relaxed)
}

/// A value computed in the background.
#[derive(Debug)]
pub(crate) enum Task<T> {
    Pending(Arc<Mutex<Option<T>>>),
    Ready(T),
}

impl<T: Send + 'static> Task<T> {
    /// Runs `f` on the thread pool and repaints `ctx` when done.
    pub(crate) fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let out = Arc::clone(&slot);
        let ctx = ctx.clone();
        // Wake the window that asked (pop-outs are separate viewports).
        let viewport = ctx.viewport_id();
        RUNNING.fetch_add(1, Ordering::Relaxed);
        rayon::spawn(move || {
            let v = f();
            *out.lock() = Some(v);
            RUNNING.fetch_sub(1, Ordering::Relaxed);
            ctx.request_repaint_of(viewport);
            ctx.request_repaint();
        });
        Self::Pending(slot)
    }

    /// Returns the value once available.
    pub(crate) fn poll(&mut self) -> Option<&T> {
        if let Self::Pending(slot) = self {
            let v = slot.lock().take();
            if let Some(v) = v {
                *self = Self::Ready(v);
            }
        }
        match self {
            Self::Ready(v) => Some(v),
            Self::Pending(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_ready(&mut self) -> bool {
        self.poll().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_completes() {
        let ctx = egui::Context::default();
        let mut t = Task::spawn(&ctx, || 41 + 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !t.is_ready() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(t.poll(), Some(&42));
    }
}
