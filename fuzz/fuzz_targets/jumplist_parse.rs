#![no_main]
//! Automatic- and custom-destinations jump lists from arbitrary bytes.

use libfuzzer_sys::fuzz_target;
use pm_lnk::jumplist::{parse_automatic, parse_custom, parse_dest_list};

fuzz_target!(|data: &[u8]| {
    let _ = parse_automatic(data);
    let _ = parse_custom(data);
    let _ = parse_dest_list(data);
});
