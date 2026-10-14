//! Creates a case and adds a disk image, printing progress and timings.
//!
//! `cargo run --release -p pm-case --example add_image -- <new case dir> <image>`
#![allow(clippy::print_stdout, clippy::print_stderr, reason = "CLI example")]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(image)) = (args.next(), args.next()) else {
        eprintln!("usage: add_image <new case dir> <image>");
        std::process::exit(2);
    };
    let case = Case::create(&dir, CaseDetails::default())?;
    let progress = Arc::new(LoadProgress::default());
    let t = Instant::now();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ticker = {
        let (p, done) = (Arc::clone(&progress), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
                eprintln!(
                    "  {:>8} rows {:>7} dirs  {}",
                    p.files.load(Ordering::Relaxed),
                    p.dirs.load(Ordering::Relaxed),
                    p.stage()
                );
            }
        })
    };
    let result = add_image(&case, image.as_ref(), &LoadOptions::default(), &progress);
    done.store(true, Ordering::Relaxed);
    let _ = ticker.join();
    let s = result?;
    let dt = t.elapsed();
    println!(
        "data source {}: {} rows in {dt:.2?} ({:.0} rows/s)",
        s.ds_id,
        s.files,
        s.files as f64 / dt.as_secs_f64()
    );
    for fs in case.db().file_systems(s.ds_id)? {
        println!(
            "  fs {} {} at byte {} label {:?}",
            fs.id, fs.kind, fs.byte_offset, fs.label
        );
    }
    for w in s.warnings.iter().take(20) {
        println!("  warning: {w}");
    }
    println!("  {} warnings", s.warnings.len());
    Ok(())
}
