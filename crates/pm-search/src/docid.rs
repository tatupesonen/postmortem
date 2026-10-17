//! Autopsy's chunk document ids (`<objId>_<chunk>`, mini-chunks
//! `<objId>_<chunk>_mini`), as stored in `TSK_KEYWORD_SEARCH_DOCUMENT_ID`
//! by the credit-card module (Autopsy `MiniChunkHelper`).

use crate::ObjRef;

const MINI_SUFFIX: &str = "_mini";

/// The id of page `page` of `obj` (`page` 0 is the object itself).
pub fn document_id(obj: ObjRef, page: u32) -> String {
    if page == 0 {
        obj.key().to_string()
    } else {
        format!("{}_{page}", obj.key())
    }
}

/// Autopsy `MiniChunkHelper.isMiniChunkID`.
pub fn is_mini_chunk_id(id: &str) -> bool {
    id.ends_with(MINI_SUFFIX)
}

/// Autopsy `MiniChunkHelper.getBaseChunkID`: only a trailing `_mini` is
/// removed.
pub fn base_chunk_id(id: &str) -> &str {
    id.strip_suffix(MINI_SUFFIX).unwrap_or(id)
}

/// The object and page of a document id (mini-chunk ids give their base
/// chunk); `None` if it is not one.
pub fn parse_document_id(id: &str) -> Option<(ObjRef, u32)> {
    let base = base_chunk_id(id.trim());
    let (obj, page) = match base.rsplit_once('_') {
        Some((o, p)) => (o, p.parse().ok()?),
        None => (base, 0),
    };
    Some((ObjRef::from_key(obj.parse().ok()?), page))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Autopsy `MiniChunkHelperTest`.
    #[test]
    fn mini_chunk_helper() {
        assert!(is_mini_chunk_id("1_1_mini"));
        assert!(!is_mini_chunk_id("1_1"));
        assert!(!is_mini_chunk_id("1"));
        assert_eq!(base_chunk_id("1_1_mini"), "1_1");
        assert_eq!(base_chunk_id("1_1"), "1_1");
        assert_eq!(base_chunk_id("1"), "1");
    }

    #[test]
    fn round_trip() {
        assert_eq!(document_id(ObjRef::File(12), 3), "12_3");
        assert_eq!(document_id(ObjRef::Artifact(5), 1), "-5_1");
        assert_eq!(document_id(ObjRef::File(12), 0), "12");
        for (obj, page) in [
            (ObjRef::File(12), 3),
            (ObjRef::Artifact(5), 1),
            (ObjRef::File(9), 0),
        ] {
            assert_eq!(
                parse_document_id(&document_id(obj, page)),
                Some((obj, page))
            );
        }
        assert_eq!(parse_document_id("7_2_mini"), Some((ObjRef::File(7), 2)));
        assert_eq!(parse_document_id("x_1"), None);
        assert_eq!(parse_document_id("7_x"), None);
        assert_eq!(parse_document_id(""), None);
    }
}
