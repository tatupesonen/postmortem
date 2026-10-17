//! Tool errors (reported to the client as tool results with `isError`).

use thiserror::Error;

/// Why a tool call failed.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The arguments are missing or malformed.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The referenced object does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The call is not allowed by the server configuration.
    #[error("refused: {0}")]
    Refused(String),
    /// No such tool.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    /// A needed part of the case is unavailable (e.g. no keyword index).
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("case database error: {0}")]
    Db(#[from] pm_casedb::DbError),
    #[error("case error: {0}")]
    Case(#[from] pm_case::CaseError),
    #[error("search error: {0}")]
    Search(#[from] pm_search::SearchError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl ToolError {
    pub(crate) fn arg(msg: impl Into<String>) -> Self {
        Self::InvalidArgument(msg.into())
    }
}

/// Result of a tool handler.
pub type ToolResult<T> = Result<T, ToolError>;
