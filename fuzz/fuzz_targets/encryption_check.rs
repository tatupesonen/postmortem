#![no_main]
//! Password-protection checks: the first byte routes the rest to the
//! OLE2 (Word/Excel/PowerPoint), PDF or Access checker.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_encryption::office::{OfficeKind, is_password_protected};

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else {
        return;
    };
    let src = MemSource::new(rest.to_vec());
    match k % 5 {
        0 => drop(is_password_protected(&src, OfficeKind::Word)),
        1 => drop(is_password_protected(&src, OfficeKind::Excel)),
        2 => drop(is_password_protected(&src, OfficeKind::PowerPoint)),
        3 => drop(pm_encryption::pdf::is_password_protected(&src)),
        _ => drop(pm_encryption::access::is_password_protected(&src)),
    }
});
