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

    /// The value if [`Self::poll`] has already received it.
    pub(crate) fn peek(&self) -> Option<&T> {
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

/// A value loaded in the background and kept until it is refreshed:
/// readers always get the last good value, never a blank while reloading.
#[derive(Debug)]
pub(crate) struct Cached<T> {
    task: Option<Task<Result<T, String>>>,
    current: Option<Arc<T>>,
    stale: bool,
    error: Option<String>,
}

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self {
            task: None,
            current: None,
            stale: true,
            error: None,
        }
    }
}

impl<T: Send + Sync + 'static> Cached<T> {
    /// The last loaded value (`None` before the first load finishes);
    /// starts `load` when stale.
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        load: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Option<Arc<T>> {
        if let Some(t) = &mut self.task
            && t.poll().is_some()
            && let Some(Task::Ready(r)) = self.task.take()
        {
            match r {
                Ok(v) => {
                    self.current = Some(Arc::new(v));
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if self.stale && self.task.is_none() {
            self.stale = false;
            self.task = Some(Task::spawn(ctx, load));
        }
        self.current.clone()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Reloads on the next [`Self::get`], keeping the current value.
    pub(crate) fn refresh(&mut self) {
        self.stale = true;
    }

    /// Drops the value (e.g. the case closed).
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// A value computed in the background for a key (a query): recomputed
/// when the key changes, the previous value shown until the new one
/// arrives, so views never blank while they reload.
#[derive(Debug)]
pub(crate) struct Keyed<K, T> {
    key: Option<K>,
    task: Option<Task<Result<T, String>>>,
    current: Option<Arc<T>>,
    error: Option<String>,
}

impl<K, T> Default for Keyed<K, T> {
    fn default() -> Self {
        Self {
            key: None,
            task: None,
            current: None,
            error: None,
        }
    }
}

impl<K: PartialEq + Clone, T: Send + Sync + 'static> Keyed<K, T> {
    /// The value for `key` once loaded, else the previous one; starts
    /// `load` when the key changed.
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        key: &K,
        load: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Option<Arc<T>> {
        if self.key.as_ref() != Some(key) {
            self.key = Some(key.clone());
            self.task = Some(Task::spawn(ctx, load));
        }
        if let Some(t) = &mut self.task
            && t.poll().is_some()
            && let Some(Task::Ready(r)) = self.task.take()
        {
            match r {
                Ok(v) => {
                    self.current = Some(Arc::new(v));
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        self.current.clone()
    }

    /// Whether the value for the current key is still being computed.
    pub(crate) fn loading(&self) -> bool {
        self.task.is_some()
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Runs `f` on the pool, counted in [`running`] (for fire-and-forget jobs
/// that publish their own results).
pub(crate) fn spawn_detached(f: impl FnOnce() + Send + 'static) {
    RUNNING.fetch_add(1, Ordering::Relaxed);
    rayon::spawn(move || {
        f();
        RUNNING.fetch_sub(1, Ordering::Relaxed);
    });
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
