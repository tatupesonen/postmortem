#![no_main]
//! Keyword-list imports (Autopsy XML, EnCase UTF-16 exports) and the
//! inline matcher on arbitrary input: never panic; imported lists
//! survive an export/import round trip.

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use pm_search::Matcher;
use pm_search::keywords::{builtin_lists, parse_encase, parse_xml, to_xml};

fuzz_target!(|data: &[u8]| {
    if let Ok(lists) = parse_xml(data) {
        let again = parse_xml(to_xml(&lists, false).as_bytes()).expect("round trip");
        assert_eq!(again.len(), lists.len());
        let (m, _) = Matcher::new(&lists);
        let mut hits = Vec::new();
        m.search(&String::from_utf8_lossy(data), None, &mut hits);
    }
    let _ = parse_encase(data);
    static BUILTIN: LazyLock<Matcher> = LazyLock::new(|| Matcher::new(&builtin_lists()).0);
    let m = &*BUILTIN;
    let text = String::from_utf8_lossy(data);
    let mut hits = Vec::new();
    m.search(&text, None, &mut hits);
    for h in hits {
        assert!(h.start <= h.end && h.end <= text.len());
    }
});
