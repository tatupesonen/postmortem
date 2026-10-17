#![no_main]
//! OLE2 compound files: open (POI-style validation), walk the directory
//! tree and read every stream.

use libfuzzer_sys::fuzz_target;
use pm_core::io::MemSource;
use pm_filetype::cfb::Cfb;

fuzz_target!(|data: &[u8]| {
    let mut bytes = pm_filetype::cfb::SIGNATURE.to_vec();
    bytes.extend_from_slice(data);
    let src = MemSource::new(bytes);
    let Ok(cfb) = Cfb::open(&src) else {
        return;
    };
    let Some(root) = cfb.root() else {
        return;
    };
    let _ = cfb.top_level_names();
    let _ = cfb.find_recursively(root, "WORDDOCUMENT", 8);
    let mut stack = vec![root];
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 10_000 {
            break;
        }
        for child in cfb.children(dir) {
            if child.is_dir() {
                stack.push(child);
            } else {
                let _ = cfb.read_stream(child, 1 << 20);
            }
        }
    }
});
