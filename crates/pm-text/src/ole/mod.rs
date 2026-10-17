//! OLE2 compound documents (Tika's POI-based parsers): Word 97+, Excel
//! BIFF8 (and BIFF5 labels), `PowerPoint` 97+, Outlook `.msg`, plus the
//! `SummaryInformation` / `DocumentSummaryInformation` properties of any
//! compound file. Encrypted documents are reported as
//! [`TextError::Encrypted`].

mod excel;
mod msg;
mod ppt;
mod props;
mod word;

use std::io::Read;

use pm_core::ReadAt;

use crate::util::{Borrowed, cursor};
use crate::{Metadata, TextError, TextSink};

/// A stream is read into memory up to this size.
const MAX_STREAM: u64 = 256 * 1024 * 1024;

pub(crate) type Cfb<'a> = cfb::CompoundFile<pm_core::io::Cursor<Borrowed<'a>>>;

#[cfg(test)]
pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    extract_depth(src, sink, 0)
}

pub(crate) fn extract_depth(
    src: &dyn ReadAt,
    sink: &mut dyn TextSink,
    depth: u32,
) -> Result<Metadata, TextError> {
    let mut cf = cfb::CompoundFile::open(cursor(src)).map_err(|e| TextError::malformed("OLE", e))?;
    if cf.is_stream("/EncryptedPackage") || cf.is_stream("/EncryptionInfo") {
        // Encrypted OOXML (agile or standard encryption).
        return Err(TextError::Encrypted);
    }
    let mut meta = props::read(&mut cf);
    if cf.is_stream("/WordDocument") {
        word::extract(&mut cf, sink)?;
    } else if cf.is_stream("/Workbook") || cf.is_stream("/Book") {
        excel::extract(&mut cf, sink)?;
    } else if cf.is_stream("/PowerPoint Document") {
        if cf.is_stream("/EncryptedSummary") {
            return Err(TextError::Encrypted);
        }
        ppt::extract(&mut cf, sink)?;
    } else if msg::is_msg(&cf) {
        msg::extract(&mut cf, sink, &mut meta, depth)?;
    }
    Ok(meta)
}

/// Reads a whole stream (capped).
pub(crate) fn read_stream(cf: &mut Cfb<'_>, path: &str) -> Result<Vec<u8>, TextError> {
    let s = cf.open_stream(path).map_err(|e| TextError::malformed("OLE", e))?;
    let len = s.len();
    if len > MAX_STREAM {
        return Err(TextError::TooLarge {
            size: len,
            limit: MAX_STREAM,
        });
    }
    let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    s.take(MAX_STREAM).read_to_end(&mut out)?;
    Ok(out)
}

/// The text encoding of a Windows code page.
pub(crate) fn codepage(cp: u32) -> &'static encoding_rs::Encoding {
    use encoding_rs::{WINDOWS_874, SHIFT_JIS, GBK, EUC_KR, BIG5, UTF_16LE, UTF_16BE, WINDOWS_1250, WINDOWS_1251, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256, WINDOWS_1257, WINDOWS_1258, MACINTOSH, KOI8_R, KOI8_U, ISO_8859_2, ISO_8859_5, ISO_8859_7, ISO_8859_8, ISO_2022_JP, EUC_JP, GB18030, UTF_8, WINDOWS_1252};
    match cp {
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1200 => UTF_16LE,
        1201 => UTF_16BE,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        10000 => MACINTOSH,
        20866 => KOI8_R,
        21866 => KOI8_U,
        28592 => ISO_8859_2,
        28595 => ISO_8859_5,
        28597 => ISO_8859_7,
        28598 => ISO_8859_8,
        50220..=50222 => ISO_2022_JP,
        51932 => EUC_JP,
        54936 => GB18030,
        65001 => UTF_8,
        _ => WINDOWS_1252,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    /// Builds a compound file with the given streams.
    pub(crate) fn cfb_file(streams: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cf = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        for (path, data) in streams {
            if let Some((dir, _)) = path.rsplit_once('/')
                && !dir.is_empty() && !cf.exists(dir) {
                    cf.create_storage_all(dir).unwrap();
                }
            let mut s = cf.create_stream(path).unwrap();
            s.write_all(data).unwrap();
        }
        cf.flush().unwrap();
        cf.into_inner().into_inner()
    }

    #[test]
    fn generic_ole_has_only_metadata() {
        let si = super::props::tests::summary_info(&[(2, "Title here"), (4, "Ann")]);
        let data = cfb_file(&[("/\u{5}SummaryInformation", &si), ("/Other", b"x")]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = crate::StringSink::default();
        let m = super::extract(&src, &mut sink).unwrap();
        assert_eq!(sink.text, "");
        assert_eq!(m["Title"], "Title here");
        assert_eq!(m["Author"], "Ann");
    }

    #[test]
    fn encrypted_ooxml_is_reported() {
        let data = cfb_file(&[("/EncryptionInfo", b"x"), ("/EncryptedPackage", b"y")]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = crate::StringSink::default();
        assert!(matches!(
            super::extract(&src, &mut sink),
            Err(crate::TextError::Encrypted)
        ));
    }
}
