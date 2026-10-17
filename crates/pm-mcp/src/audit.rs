//! The examiner-facing audit log: one JSON line per tool call in
//! `<case>/Log/mcp-audit.jsonl`.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

/// File name of the audit log inside the case's log folder.
pub const AUDIT_FILE: &str = "mcp-audit.jsonl";

/// One audited call.
#[derive(Debug, Clone)]
pub struct AuditEntry<'a> {
    pub client: &'a str,
    pub tool: &'a str,
    pub arguments: &'a Map<String, Value>,
    pub rows: usize,
    pub error: Option<String>,
    pub millis: u128,
}

/// Appends audit entries; failures are logged, never fatal.
#[derive(Debug)]
pub struct AuditLog {
    path: Option<PathBuf>,
    file: Mutex<Option<File>>,
}

impl AuditLog {
    /// An audit log in `log_dir` (created when missing).
    pub fn new(log_dir: &Path) -> Self {
        Self {
            path: Some(log_dir.join(AUDIT_FILE)),
            file: Mutex::new(None),
        }
    }

    /// A log that records nothing.
    pub fn disabled() -> Self {
        Self {
            path: None,
            file: Mutex::new(None),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn record(&self, e: &AuditEntry<'_>) {
        let Some(path) = &self.path else { return };
        let line = json!({
            "time": jiff::Timestamp::now().to_string(),
            "client": e.client,
            "tool": e.tool,
            "arguments": e.arguments,
            "rows": e.rows,
            "ok": e.error.is_none(),
            "error": e.error,
            "ms": e.millis,
        });
        let mut guard = self.file.lock();
        if guard.is_none() {
            if let Some(dir) = path.parent()
                && let Err(err) = std::fs::create_dir_all(dir)
            {
                tracing::warn!("cannot create {}: {err}", dir.display());
                return;
            }
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => *guard = Some(f),
                Err(err) => {
                    tracing::warn!("cannot open audit log {}: {err}", path.display());
                    return;
                }
            }
        }
        if let Some(f) = guard.as_mut() {
            let mut bytes = line.to_string().into_bytes();
            bytes.push(b'\n');
            if let Err(err) = f.write_all(&bytes) {
                tracing::warn!("cannot write audit log {}: {err}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(&dir.path().join("Log"));
        let args = Map::from_iter([("q".to_owned(), json!("x"))]);
        for tool in ["a", "b"] {
            log.record(&AuditEntry {
                client: "test",
                tool,
                arguments: &args,
                rows: 2,
                error: None,
                millis: 1,
            });
        }
        let text = std::fs::read_to_string(log.path().unwrap()).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["tool"], "b");
        assert_eq!(lines[0]["arguments"]["q"], "x");
        AuditLog::disabled().record(&AuditEntry {
            client: "",
            tool: "",
            arguments: &args,
            rows: 0,
            error: None,
            millis: 0,
        });
    }
}
