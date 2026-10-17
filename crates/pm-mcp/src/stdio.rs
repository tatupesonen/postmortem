//! The stdio transport (`postmortem mcp --case <dir>`).

use std::sync::Arc;

use rmcp::ServiceExt as _;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::handler::McpHandler;
use crate::server::McpServer;

/// Why serving stopped with an error.
#[derive(Debug, Error)]
pub enum ServeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("MCP session failed: {0}")]
    Session(String),
}

/// Serves MCP over any byte stream pair until the client disconnects.
pub async fn serve_io<R, W>(server: Arc<McpServer>, reader: R, writer: W) -> Result<(), ServeError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let running = McpHandler::new(server)
        .serve((reader, writer))
        .await
        .map_err(|e| ServeError::Session(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| ServeError::Session(e.to_string()))?;
    Ok(())
}

/// Serves MCP on this process's stdin/stdout until the client closes it
/// (blocking; builds its own Tokio runtime). Logs must go to stderr.
pub fn serve_stdio(server: Arc<McpServer>) -> Result<(), ServeError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(serve_io(server, tokio::io::stdin(), tokio::io::stdout()))
}
