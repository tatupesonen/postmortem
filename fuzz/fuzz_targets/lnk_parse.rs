#![no_main]
//! Shell links and shell-item ID lists from arbitrary bytes.

use libfuzzer_sys::fuzz_target;
use pm_lnk::ShellLink;
use pm_lnk::shell_item::{join_path, parse_id_list};

fuzz_target!(|data: &[u8]| {
    if let Ok(l) = ShellLink::parse(data) {
        let _ = l.best_path();
    }
    let _ = join_path(&parse_id_list(data));
});
