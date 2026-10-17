//! Tika's `POIFSContainerDetector` and `MiscOLEDetector`.

use std::collections::HashSet;

use pm_core::ReadAt;

use crate::cfb::{Cfb, Entry, SIGNATURE};

const OLE: &str = "application/x-tika-msoffice";
/// `POIFSContainerDetector.markLimit`: larger files are not opened.
const POIFS_LIMIT: u64 = 128 * 1024 * 1024;
/// Largest `\x01CompObj` stream read.
const MAX_COMP_OBJ: usize = 16 * 1024 * 1024;

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// `processCompObjFormatType`.
fn comp_obj_type(cfb: &Cfb<'_>, root: &Entry) -> &'static str {
    let Some(e) = cfb.child_uc(root, "\u{1}COMPOBJ") else {
        return OLE;
    };
    if e.kind != 2 {
        return OLE;
    }
    let Ok(bytes) = cfb.read_stream(e, MAX_COMP_OBJ) else {
        return OLE;
    };
    if contains_bytes(&bytes, b"MSGraph.Chart") {
        "application/vnd.ms-graph"
    } else if contains_bytes(&bytes, b"StarDraw") {
        "application/vnd.stardivision.draw"
    } else if contains_bytes(&bytes, b"StarImpress") {
        "application/vnd.stardivision.impress"
    } else if contains_bytes(&bytes, b"Quill96") {
        "application/vnd.ms-works"
    } else {
        OLE
    }
}

/// `\s\s\s\d+` (Java `matches`).
fn mpp_name(n: &str) -> bool {
    let mut it = n.chars();
    for _ in 0..3 {
        if !it
            .next()
            .is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r'))
        {
            return false;
        }
    }
    let rest: Vec<char> = it.collect();
    !rest.is_empty() && rest.iter().all(char::is_ascii_digit)
}

/// `POIFSContainerDetector.detect(names, root)`.
fn poifs(names: &[String], cfb: &Cfb<'_>, root: &Entry) -> String {
    if names.is_empty() {
        return OLE.into();
    }
    let uc: HashSet<String> = names.iter().map(|n| n.to_uppercase()).collect();
    let has = |n: &str| uc.contains(n);
    // checkEncrypted.
    if has("\u{6}DATASPACES") && cfb.find_recursively(root, "\tDRMDATASPACE", 10) {
        return "application/x-tika-ole-drm-encrypted".into();
    }
    if has("ENCRYPTEDPACKAGE") {
        if has("ENCRYPTIONINFO") {
            return "application/x-tika-ooxml-protected".into();
        }
        if has("\u{6}DATASPACES") && cfb.find_recursively(root, "DRMENCRYPTEDDATASPACE", 10) {
            return "application/x-tika-ole-drm-encrypted".into();
        }
    }
    if has("WORKBOOK") || has("BOOK") {
        return if comp_obj_type(cfb, root) == "application/vnd.ms-graph" {
            "application/vnd.ms-graph".into()
        } else {
            "application/vnd.ms-excel".into()
        };
    }
    let t: &str = if has("SWDOCCONTENTMGR") && has("SWDOCMGRTEMPSTORAGE") {
        "application/sldworks"
    } else if has("STARCALCDOCUMENT") {
        "application/vnd.stardivision.calc"
    } else if has("STARWRITERDOCUMENT") {
        "application/vnd.stardivision.writer"
    } else if has("STARDRAWDOCUMENT3") {
        comp_obj_type(cfb, root)
    } else if has("WKSSSWORKBOOK") {
        "application/x-tika-msworks-spreadsheet"
    } else if has("WORDDOCUMENT") {
        "application/msword"
    } else if has("QUILL") {
        "application/x-mspublisher"
    } else if has("POWERPOINT DOCUMENT") {
        "application/vnd.ms-powerpoint"
    } else if has("VISIODOCUMENT") {
        "application/vnd.visio"
    } else if has("\u{1}OLE10NATIVE") {
        "application/x-tika-msoffice-embedded; format=ole10_native"
    } else if has("MATOST") || (has("CONTENTS") && has("SPELLING")) {
        "application/vnd.ms-works"
    } else if has("EQUATION NATIVE") {
        "application/vnd.ms-equation"
    } else if has("\u{3}OCXNAME") {
        OLE
    } else if has("CONTENTS") && has("\u{3}OBJINFO") {
        "application/x-tika-msoffice-embedded; format=comp_obj"
    } else if has("CONTENTS") && has("\u{1}COMPOBJ") {
        if comp_obj_type(cfb, root) == "application/vnd.ms-works" {
            "application/vnd.ms-works"
        } else {
            "application/x-tika-msoffice-embedded; format=comp_obj"
        }
    } else if has("CONTENTS") {
        OLE
    } else if has("\u{1}COMPOBJ") && (has("PROPS") || has("PROPS9") || has("PROPS12")) {
        if uc.iter().any(|n| mpp_name(n)) {
            "application/vnd.ms-project"
        } else {
            OLE
        }
    } else if has("LAYER") {
        "application/x-esri-layer"
    } else if has("DGN~MF") && has("DGN~S") && has("DGN~H") {
        "image/vnd.dgn; version=8"
    } else if uc.iter().any(|n| n.starts_with("__SUBSTG1.0_")) {
        "application/vnd.ms-outlook"
    } else {
        OLE
    };
    t.into()
}

/// `MiscOLEDetector.detect(names)` (case-sensitive names).
fn misc(names: &[String]) -> String {
    let has = |n: &str| names.iter().any(|x| x == n);
    if names.is_empty() {
        OLE.into()
    } else if has("\u{5}HwpSummaryInformation") {
        "application/x-hwp-v5".into()
    } else if has("PerfectOffice_MAIN") {
        if has("SlideShow") {
            "application/x-corelpresentations".into()
        } else if has("PerfectOffice_OBJECTS") {
            "application/x-quattro-pro; version=7-8".into()
        } else {
            OLE.into()
        }
    } else if has("NativeContent_MAIN") {
        "application/x-quattro-pro; version=9".into()
    } else {
        OLE.into()
    }
}

/// Runs both OLE detectors: `None` when the file is not OLE2.
pub(crate) fn detect(src: &dyn ReadAt, prefix: &[u8]) -> Option<(String, String)> {
    if !prefix.starts_with(&SIGNATURE) {
        return None;
    }
    if src.size() >= POIFS_LIMIT {
        return Some((OLE.into(), OLE.into()));
    }
    match Cfb::open(src) {
        Ok(cfb) => {
            let names = cfb.top_level_names();
            let root = cfb.root().cloned();
            let p = root.map_or_else(|| OLE.to_owned(), |r| poifs(&names, &cfb, &r));
            Some((p, misc(&names)))
        }
        Err(e) => {
            tracing::debug!("OLE2 file not opened: {e}");
            Some((OLE.into(), OLE.into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfb::builder::{Stream, build};
    use pm_core::io::MemSource;

    fn det(streams: &[Stream]) -> (String, String) {
        let f = build(streams);
        let src = MemSource::new(f.clone());
        detect(&src, &f).unwrap()
    }

    #[test]
    fn office_types() {
        assert_eq!(
            det(&[Stream::root("WordDocument", b"x")]).0,
            "application/msword"
        );
        assert_eq!(
            det(&[Stream::root("Workbook", b"x")]).0,
            "application/vnd.ms-excel"
        );
        assert_eq!(
            det(&[Stream::root("PowerPoint Document", b"x")]).0,
            "application/vnd.ms-powerpoint"
        );
        assert_eq!(
            det(&[
                Stream::root("EncryptionInfo", b"x"),
                Stream::root("EncryptedPackage", b"y")
            ])
            .0,
            "application/x-tika-ooxml-protected"
        );
        assert_eq!(
            det(&[Stream::root("__substg1.0_0037001F", b"x")]).0,
            "application/vnd.ms-outlook"
        );
        assert_eq!(
            det(&[
                Stream::root("\u{1}CompObj", b"..MSGraph.Chart.8.."),
                Stream::root("Workbook", b"x")
            ])
            .0,
            "application/vnd.ms-graph"
        );
        assert_eq!(
            det(&[Stream::root("\u{5}HwpSummaryInformation", b"x")]).1,
            "application/x-hwp-v5"
        );
        assert_eq!(det(&[Stream::root("Other", b"x")]).0, OLE);
        assert!(mpp_name("   112"));
        assert!(!mpp_name("  112"));
    }

    #[test]
    fn broken_files_are_generic_ole() {
        let mut f = build(&[Stream::root("WordDocument", b"x")]);
        f[30] = 3;
        let src = MemSource::new(f.clone());
        assert_eq!(detect(&src, &f).unwrap(), (OLE.into(), OLE.into()));
        assert!(detect(&src, b"not ole").is_none());
    }
}
