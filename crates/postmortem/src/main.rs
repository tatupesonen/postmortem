//! postmortem desktop application.
//!
//! Usage: `postmortem [IMAGE ...]` — any image paths given are opened as
//! data sources on start-up (first segment of split/E01 sets).

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu=warn,naga=warn".into()),
        )
        .init();
    let images: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    match pm_ui::run(images) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("UI failed: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
