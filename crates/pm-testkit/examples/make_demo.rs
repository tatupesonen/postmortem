//! Writes the demo disk: `cargo run -p pm-testkit --example make_demo -- OUT.dd`
#![allow(clippy::unwrap_used, reason = "dev tool")]

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "demo.dd".into());
    std::fs::write(&out, pm_testkit::demo::demo_disk()).unwrap();
}
