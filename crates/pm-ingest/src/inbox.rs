//! Ingest inbox messages (Autopsy's `IngestMessage`).

use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;

/// Errors and warnings kept per case; later ones collapse into one notice.
pub const MAX_ERROR_MESSAGES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Data,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestMessage {
    pub id: u64,
    pub kind: MessageKind,
    /// Module display name (`None`: the framework).
    pub source: Option<String>,
    pub subject: String,
    /// Plain-text details (Autopsy uses HTML; the UI renders this as text).
    pub details: String,
    /// Grouping key for data messages.
    pub unique_key: Option<String>,
    /// Related file, if any.
    pub file_id: Option<i64>,
    pub posted_at: i64,
}

impl IngestMessage {
    pub fn new(kind: MessageKind, source: &str, subject: impl Into<String>) -> Self {
        Self {
            id: 0,
            kind,
            source: Some(source.to_owned()),
            subject: subject.into(),
            details: String::new(),
            unique_key: None,
            file_id: None,
            posted_at: jiff::Timestamp::now().as_second(),
        }
    }

    #[must_use]
    pub fn details(mut self, d: impl Into<String>) -> Self {
        self.details = d.into();
        self
    }

    #[must_use]
    pub fn key(mut self, k: impl Into<String>) -> Self {
        self.unique_key = Some(k.into());
        self
    }

    #[must_use]
    pub fn file(mut self, id: i64) -> Self {
        self.file_id = Some(id);
        self
    }
}

/// The case's inbox, shared by all jobs.
#[derive(Debug, Default)]
pub struct Inbox {
    next_id: AtomicU64,
    messages: Mutex<Vec<IngestMessage>>,
    errors: Mutex<usize>,
}

impl Inbox {
    pub fn post(&self, mut msg: IngestMessage) {
        if matches!(msg.kind, MessageKind::Error | MessageKind::Warning) {
            let mut n = self.errors.lock();
            *n += 1;
            if *n > MAX_ERROR_MESSAGES + 1 {
                return;
            }
            if *n == MAX_ERROR_MESSAGES + 1 {
                msg = IngestMessage {
                    source: None,
                    subject: "Error message limit reached".into(),
                    details: format!(
                        "More than {MAX_ERROR_MESSAGES} errors and warnings were posted; \
                         further ones are only written to the log."
                    ),
                    kind: MessageKind::Error,
                    unique_key: None,
                    file_id: None,
                    ..msg
                };
            }
        }
        msg.id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.messages.lock().push(msg);
    }

    /// Messages posted after `after_id`, oldest first.
    pub fn since(&self, after_id: u64) -> Vec<IngestMessage> {
        self.messages
            .lock()
            .iter()
            .filter(|m| m.id > after_id)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.messages.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.messages.lock().clear();
        *self.errors.lock() = 0;
    }
}
