//! MCP server for AI assistants (docs/BEYOND-AUTOPSY.md §8).
//!
//! * [`McpServer`]: the transport-independent server: Autopsy's 17 query
//!   tools under their names, plus search, content, pivot, timeline,
//!   communications and coverage tools, opt-in write tools, privacy
//!   switches ([`McpConfig`]) and an audit log.
//! * Transports: stdio ([`stdio::serve_stdio`],
//!   `postmortem mcp --case <dir>`) and streamable HTTP on 127.0.0.1 with a bearer token
//!   ([`http::HttpServer`], started and stopped by the GUI).
//!
//! Conventions: stable references (`file:123`, `artifact:45`, …), keyset
//! cursors, pages of 50 (max 500) rows, responses capped at 256 KiB,
//! ISO 8601 UTC times with Unix seconds, and evidence-derived text only
//! inside `evidence` fields.
#![forbid(unsafe_code)]

pub mod args;
pub mod audit;
pub mod cli;
pub mod config;
pub mod error;
pub mod handler;
pub mod http;
pub mod prompts;
pub mod response;
pub mod rows;
pub mod server;
pub mod stdio;
pub mod tools;

pub use config::McpConfig;
pub use error::ToolError;
pub use handler::McpHandler;
pub use server::{CallOutcome, INSTRUCTIONS, McpServer, SERVER_NAME};
