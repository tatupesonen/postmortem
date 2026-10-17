//! `postmortem ingest`: adds images to a case and runs the ingest modules
//! without the GUI (Autopsy's command-line ingest), with the same module
//! defaults and plugins as the Add Data Source wizard. Progress, module
//! times and peak memory go to stderr.
#![allow(clippy::print_stderr, reason = "command-line front end")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress};
use pm_ingest::{Inbox, IngestProgress};

pub const USAGE: &str = "\
usage: postmortem ingest --case <CASE DIR> [--enable ID,..] [--disable ID,..]
                         [--threads N] [IMAGE ...]

Creates the case if the folder does not exist, adds each image (first
segment of split or E01 sets) and runs the ingest modules on it. Without
images the case is only opened, which upgrades a case made by an older
postmortem.
Module ids: postmortem ingest --list-modules";

#[derive(Debug, Default)]
struct Args {
    case: Option<PathBuf>,
    images: Vec<PathBuf>,
    enable: Vec<String>,
    disable: Vec<String>,
    threads: usize,
    list: bool,
}

fn parse(args: &[OsString]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.iter();
    let list = |v: Option<&OsString>| -> Result<Vec<String>, String> {
        let v = v.ok_or("missing module ids")?.to_string_lossy();
        Ok(v.split(',').map(|s| s.trim().to_owned()).collect())
    };
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--case") => a.case = Some(it.next().ok_or("--case needs a folder")?.into()),
            Some("--enable") => a.enable.extend(list(it.next())?),
            Some("--disable") => a.disable.extend(list(it.next())?),
            Some("--threads") => {
                a.threads = it
                    .next()
                    .and_then(|n| n.to_str()?.parse().ok())
                    .ok_or("--threads needs a number")?;
            }
            Some("--list-modules") => a.list = true,
            Some("-h" | "--help") => return Err(String::new()),
            Some(s) if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => a.images.push(arg.into()),
        }
    }
    Ok(a)
}

/// Memory in MiB (Linux; zeros elsewhere).
#[derive(Debug, Clone, Copy)]
struct Memory {
    /// Heap and other anonymous memory: what the process really uses.
    anon: u64,
    /// File pages mapped (images, indexes): reclaimable page cache.
    file: u64,
    /// Peak resident (anonymous + file).
    peak: u64,
}

fn memory() -> Memory {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map_or(0, |kb| kb / 1024)
    };
    Memory {
        anon: field("RssAnon:"),
        file: field("RssFile:"),
        peak: field("VmHWM:"),
    }
}

/// Highest heap seen by [`sample`].
static PEAK_ANON: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`memory`], recording the peak heap.
fn sample() -> Memory {
    let m = memory();
    PEAK_ANON.fetch_max(m.anon, std::sync::atomic::Ordering::Relaxed);
    m
}

fn clock(d: Duration) -> String {
    crate::analysis::clock(d)
}

pub fn ingest(args: &[OsString]) -> ExitCode {
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let plugins = pm_modules::plugin_host::PluginSet::load(&crate::plugins::plugins_dir());
    for e in &plugins.errors {
        eprintln!("plugin not loaded: {e}");
    }
    let modules = plugins.modules();
    if a.list {
        for m in &modules {
            let i = m.info();
            eprintln!(
                "{:<28} {}{}",
                i.id,
                i.name,
                if i.enabled_by_default {
                    ""
                } else {
                    " (off by default)"
                }
            );
        }
        return ExitCode::SUCCESS;
    }
    let Some(dir) = a.case else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let case = if dir.join(pm_case::CASE_FILE).is_file() {
        Case::open(&dir)
    } else {
        let name = dir
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Case::create(
            &dir,
            CaseDetails {
                name,
                ..CaseDetails::default()
            },
        )
    };
    let case = match case {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("{}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    let prefs = crate::IngestPrefs {
        enabled: a.enable,
        disabled: a.disable,
        ..crate::IngestPrefs::default()
    };
    let mut settings = prefs.job_settings_for(&modules);
    settings.threads = a.threads;
    let mut failed = false;
    for image in &a.images {
        failed |= !ingest_one(&case, image, &settings, &modules);
    }
    let now = sample();
    eprintln!(
        "peak resident {} MiB (peak heap {} MiB)",
        now.peak,
        PEAK_ANON.load(std::sync::atomic::Ordering::Relaxed)
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn ingest_one(
    case: &Arc<Case>,
    image: &std::path::Path,
    settings: &pm_ingest::JobSettings,
    modules: &[Arc<dyn pm_ingest::ModuleFactory>],
) -> bool {
    let t = Instant::now();
    let load = Arc::new(LoadProgress::default());
    let loaded = {
        let l = Arc::clone(&load);
        let (case, image) = (Arc::clone(case), image.to_path_buf());
        let h = std::thread::spawn(move || {
            pm_case::add_image(&case, &image, &LoadOptions::default(), &l)
        });
        while !h.is_finished() {
            std::thread::sleep(Duration::from_secs(2));
            let mem = sample();
            eprintln!(
                "[{}] adding: {} rows, {} | heap {} MiB, mapped {} MiB",
                clock(t.elapsed()),
                load.files.load(std::sync::atomic::Ordering::Relaxed),
                load.stage(),
                mem.anon,
                mem.file
            );
        }
        h.join()
            .map_err(|_| "loader panicked".to_owned())
            .and_then(|r| r.map_err(|e| e.to_string()))
    };
    let loaded = match loaded {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{}: {e}", image.display());
            return false;
        }
    };
    eprintln!(
        "[{}] {}: {} rows added, {} warnings",
        clock(t.elapsed()),
        image.display(),
        loaded.files,
        loaded.warnings.len()
    );
    let progress = Arc::new(IngestProgress::default());
    let inbox = Arc::new(Inbox::default());
    let job = {
        let (case, settings, modules) = (Arc::clone(case), settings.clone(), modules.to_vec());
        let (p, inbox) = (Arc::clone(&progress), Arc::clone(&inbox));
        let ds = loaded.ds_id;
        std::thread::spawn(move || pm_ingest::run_job(&case, ds, &settings, &modules, &p, &inbox))
    };
    while !job.is_finished() {
        std::thread::sleep(Duration::from_secs(5));
        let s = progress.snapshot();
        let mem = sample();
        eprintln!(
            "[{}] {}/{} files, {} results, {} errors, {:.0} files/s | heap {} MiB, mapped {} MiB | {}",
            clock(t.elapsed()),
            s.files_done,
            s.files_total,
            s.artifacts,
            s.errors,
            s.rate,
            mem.anon,
            mem.file,
            s.activity_line()
        );
    }
    let summary = match job.join() {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            eprintln!("ingest failed: {e}");
            return false;
        }
        Err(_) => {
            eprintln!("ingest panicked");
            return false;
        }
    };
    let s = progress.snapshot();
    eprintln!(
        "[{}] done: {} files, {} results, {} errors{}",
        clock(t.elapsed()),
        summary.files,
        summary.artifacts,
        summary.errors.len(),
        if summary.cancelled {
            " (cancelled)"
        } else {
            ""
        }
    );
    eprintln!("module times (summed over workers):");
    for (m, d) in s.module_times.iter().take(20) {
        eprintln!("  {:<32} {}", m, clock(*d));
    }
    for e in summary.errors.iter().take(20) {
        eprintln!("  error: {e}");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options() {
        let args: Vec<OsString> = ["--case", "c", "--disable", "a,b", "--threads", "3", "x.E01"]
            .iter()
            .map(OsString::from)
            .collect();
        let a = parse(&args).unwrap();
        assert_eq!(a.case, Some(PathBuf::from("c")));
        assert_eq!(a.disable, ["a", "b"]);
        assert_eq!(a.threads, 3);
        assert_eq!(a.images, [PathBuf::from("x.E01")]);
        assert!(parse(&[OsString::from("--bogus")]).is_err());
    }
}
