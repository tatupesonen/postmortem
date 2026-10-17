//! Plain text (Autopsy `TextFileExtractor`): the whole file decoded with
//! the detected encoding.

use pm_core::ReadAt;

use crate::{Metadata, TextError, TextSink, encoding};

pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let enc = encoding::detect_source(src)?;
    encoding::decode_to_sink(src, enc, sink)?;
    Ok(Metadata::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    #[test]
    fn decodes_utf16_text() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "Grüße".encode_utf16() {
            bytes.extend(u.to_le_bytes());
        }
        let src = pm_core::io::MemSource::new(bytes);
        let mut sink = StringSink::default();
        assert!(extract(&src, &mut sink).unwrap().is_empty());
        assert_eq!(sink.text, "Grüße");
    }
}
