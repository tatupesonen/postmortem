//! Differential-test driver: reads file paths from stdin and prints
//! `path \t detect(stream) \t detect(stream, name)` like
//! `oracle/TikaOracle.java` does with real Tika (`oracle/run.sh` runs both).
#![allow(
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "command-line tool"
)]

use std::io::{BufRead, Write};

use pm_core::io::FileSource;

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let Ok(path) = line else { break };
        let (p1, p2) = match FileSource::open(&path) {
            Ok(src) => {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let p1 = pm_filetype::tika_detect(&src, None).unwrap_or_else(|_| "ERROR".into());
                let p2 =
                    pm_filetype::tika_detect(&src, Some(&name)).unwrap_or_else(|_| "ERROR".into());
                (p1, p2)
            }
            Err(_) => ("ERROR".into(), "ERROR".into()),
        };
        writeln!(out, "{path}\t{p1}\t{p2}").unwrap();
    }
}
