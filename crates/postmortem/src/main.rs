//! postmortem desktop application.
//!
//! Usage: `postmortem [CASE] [IMAGE ...]` — `CASE` (a case folder or its
//! `case.pmcase`) is opened on start-up; images given after it are added
//! to that case as data sources (first segment of split/E01 sets). As in
//! Autopsy, images can only be added to an open case.
//!
//! `postmortem ingest --case <CASE> <IMAGE>…` adds images and runs the
//! ingest modules without the GUI (see `pm_ui::cli::USAGE`).
//!
//! `postmortem mcp --case <CASE>` serves the case to AI assistants over
//! the Model Context Protocol (see `pm_mcp::cli::USAGE`).

// Release builds are GUI apps on Windows: no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "mcp") {
        // stdout carries the MCP protocol: log to stderr.
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn,pm_mcp=info".into()),
            )
            .init();
        return pm_mcp::cli::run(args.get(1..).unwrap_or_default());
    }
    if args.first().is_some_and(|a| a == "ingest") {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "warn,evtx=error".into()),
            )
            .init();
        return pm_ui::cli::ingest(args.get(1..).unwrap_or_default());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu=warn,naga=warn".into()),
        )
        .init();
    let paths: Vec<PathBuf> = args.into_iter().map(PathBuf::from).collect();
    match pm_ui::run(paths) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("UI failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
