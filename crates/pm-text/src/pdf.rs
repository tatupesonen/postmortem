//! PDF: `pdf-extract` page by page (its layout heuristics, our output
//! device streaming into the sink), the Info dictionary as metadata.
//! Documents encrypted with the empty user password are decrypted (by
//! `lopdf` at load); others are reported as encrypted. A page whose
//! parsing panics is skipped, the rest of the document still extracts.

use std::panic::{AssertUnwindSafe, catch_unwind};

use pdf_extract::{Document, MediaBox, Object, OutputDev, OutputError, Transform};
use pm_core::ReadAt;

use crate::util::read_all;
use crate::{Metadata, Stop, TextError, TextSink};

/// PDFs are loaded into memory (`lopdf`); larger ones are not extracted.
const MAX_PDF: u64 = 512 * 1024 * 1024;

pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let data = read_all(src, MAX_PDF)?;
    let doc = Document::load_mem(&data).map_err(|e| TextError::malformed("PDF", e))?;
    drop(data);
    if doc.is_encrypted() && !doc.was_encrypted() {
        return Err(TextError::Encrypted);
    }
    let meta = info(&doc);
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut dev = Dev::new(sink);
    for page in pages {
        let r = catch_unwind(AssertUnwindSafe(|| {
            pdf_extract::output_doc_page(&doc, &mut dev, page)
        }));
        if dev.stopped {
            return Err(TextError::Stopped);
        }
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::debug!(page, "PDF page not extracted: {e}"),
            Err(_) => tracing::debug!(page, "PDF page parser panicked"),
        }
        dev.page_break()?;
    }
    dev.flush()?;
    Ok(meta)
}

/// The Info dictionary under Tika-style names.
fn info(doc: &Document) -> Metadata {
    let mut m = Metadata::new();
    m.insert("pdf:PDFVersion".into(), doc.version.clone());
    let dict = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(id)) => doc.get_dictionary(*id).ok(),
        Ok(Object::Dictionary(d)) => Some(d),
        _ => None,
    };
    let Some(dict) = dict else {
        return m;
    };
    for (k, v) in dict {
        let key = match k.as_slice() {
            b"Title" => "Title",
            b"Author" => "Author",
            b"Subject" => "Subject",
            b"Keywords" => "Keywords",
            b"Creator" => "Creator-Tool",
            b"Producer" => "Producer",
            b"CreationDate" => "Creation-Date",
            b"ModDate" => "Last-Save-Date",
            _ => continue,
        };
        let v = match v {
            Object::Reference(id) => doc.get_object(*id).ok(),
            o => Some(o),
        };
        let Some(Object::String(bytes, _)) = v else {
            continue;
        };
        let mut s = pdf_string(bytes);
        if key.ends_with("Date") {
            match pdf_date(&s) {
                Some(d) => s = d,
                None => continue,
            }
        }
        let s = s.trim().to_owned();
        if !s.is_empty() {
            m.insert(key.into(), s);
        }
    }
    m
}

/// A PDF text string: UTF-16BE with BOM, UTF-8 with BOM, else
/// `PDFDocEncoding`.
pub(crate) fn pdf_string(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = rest.chunks_exact(2).map(|c| {
            u16::from_be_bytes([
                c.first().copied().unwrap_or(0),
                c.get(1).copied().unwrap_or(0),
            ])
        });
        return char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| pdf_doc_char(c)).collect()
}

fn pdf_doc_char(c: u8) -> char {
    const HIGH: [char; 33] = [
        '•', '†', '‡', '…', '—', '–', 'ƒ', '⁄', '‹', '›', '−', '‰', '„', '“', '”', '‘', '’', '‚',
        '™', 'ﬁ', 'ﬂ', 'Ł', 'Œ', 'Š', 'Ÿ', 'Ž', 'ı', 'ł', 'œ', 'š', 'ž', '\u{FFFD}', '€',
    ];
    match c {
        0x18 => '˘',
        0x19 => 'ˇ',
        0x1A => 'ˆ',
        0x1B => '˙',
        0x1C => '˝',
        0x1D => '˛',
        0x1E => '˚',
        0x1F => '˜',
        0x80..=0xA0 => HIGH
            .get(usize::from(c - 0x80))
            .copied()
            .unwrap_or('\u{FFFD}'),
        _ => char::from(c),
    }
}

/// `D:YYYYMMDDHHmmSSOHH'mm'` → ISO-8601 UTC (`…Z`).
pub(crate) fn pdf_date(s: &str) -> Option<String> {
    let s = s.trim();
    let s = s.strip_prefix("D:").unwrap_or(s);
    let digits: String = s
        .chars()
        .take_while(char::is_ascii_digit)
        .take(14)
        .collect();
    if digits.len() < 4 {
        return None;
    }
    let num = |a: usize, b: usize, def: i64| -> i64 {
        digits.get(a..b).and_then(|x| x.parse().ok()).unwrap_or(def)
    };
    let (y, mo, d) = (num(0, 4, 0), num(4, 6, 1), num(6, 8, 1));
    let (h, mi, sec) = (num(8, 10, 0), num(10, 12, 0), num(12, 14, 0));
    let rest = s.get(digits.len()..).unwrap_or_default();
    let mut offset_min = 0i64;
    if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
        let tz: String = rest
            .get(1..)
            .unwrap_or_default()
            .chars()
            .filter(char::is_ascii_digit)
            .take(4)
            .collect();
        let oh: i64 = tz.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
        let om: i64 = tz.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
        offset_min = (oh * 60 + om) * if sign == '-' { -1 } else { 1 };
    }
    let date = jiff::civil::date(
        i16::try_from(y).ok()?,
        i8::try_from(mo.clamp(1, 12)).ok()?,
        i8::try_from(d.clamp(1, 31)).ok()?,
    );
    let dt = date
        .at(
            i8::try_from(h.clamp(0, 23)).ok()?,
            i8::try_from(mi.clamp(0, 59)).ok()?,
            i8::try_from(sec.clamp(0, 59)).ok()?,
            0,
        )
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?;
    let ts = dt.timestamp().as_second() - offset_min * 60;
    crate::util::unix_iso(ts)
}

/// `pdf-extract`'s plain-text layout logic, writing to a sink.
struct Dev<'a> {
    sink: &'a mut dyn TextSink,
    buf: String,
    last_end: f64,
    last_y: f64,
    first_char: bool,
    page_height: f64,
    stopped: bool,
}

impl<'a> Dev<'a> {
    fn new(sink: &'a mut dyn TextSink) -> Self {
        Self {
            sink,
            buf: String::new(),
            last_end: 100_000.0,
            last_y: 0.0,
            first_char: false,
            page_height: 0.0,
            stopped: false,
        }
    }

    fn flush(&mut self) -> Result<(), Stop> {
        if !self.buf.is_empty() {
            let r = self.sink.push(&self.buf);
            self.buf.clear();
            if r.is_err() {
                self.stopped = true;
            }
            r?;
        }
        Ok(())
    }

    fn page_break(&mut self) -> Result<(), Stop> {
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
        self.buf.push('\n');
        self.flush()
    }
}

impl OutputDev for Dev<'_> {
    fn begin_page(
        &mut self,
        _page_num: u32,
        media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.page_height = media_box.ury - media_box.lly;
        self.last_end = 100_000.0;
        self.first_char = false;
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        ch: &str,
    ) -> Result<(), OutputError> {
        // Position after flipping the y axis (the page's top is y = 0).
        let x = trm.m31;
        let y = self.page_height - trm.m32;
        let vx = font_size * trm.m11 + font_size * trm.m21;
        let vy = font_size * trm.m12 + font_size * trm.m22;
        let size = (vx * vy).abs().sqrt();
        if self.first_char {
            if (y - self.last_y).abs() > size * 1.5 {
                self.buf.push('\n');
            }
            if x < self.last_end && (y - self.last_y).abs() > size * 0.5 {
                self.buf.push('\n');
            }
            if x > self.last_end + size * 0.1 {
                self.buf.push(' ');
            }
        }
        self.buf.push_str(ch);
        self.first_char = false;
        self.last_y = y;
        self.last_end = x + width * size;
        if self.buf.len() >= 64 * 1024 && self.flush().is_err() {
            return Err(OutputError::FormatError(std::fmt::Error));
        }
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        self.first_char = true;
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::StringSink;

    /// A one-page PDF showing `text` in Helvetica, with an Info
    /// dictionary.
    pub(crate) fn simple_pdf(lines: &[&str]) -> Vec<u8> {
        let mut content = String::from("BT /F1 12 Tf 72 720 Td 14 TL\n");
        for l in lines {
            content.push_str(&["(", l, ") Tj T*\n"].concat());
        }
        content.push_str("ET\n");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{}endstream", content.len(), content),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            "<< /Title (Quarterly) /Author <FEFF004A0061006E0065> /CreationDate (D:20200102030405+02'00') >>".to_owned(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn extracts_text_and_info() {
        let pdf = simple_pdf(&["Hello PDF world", "second line"]);
        let src = pm_core::io::MemSource::new(pdf);
        let mut sink = StringSink::default();
        let m = extract(&src, &mut sink).unwrap();
        assert!(sink.text.contains("Hello PDF world"), "{:?}", sink.text);
        assert!(sink.text.contains("second line"), "{:?}", sink.text);
        assert_eq!(m["Title"], "Quarterly");
        assert_eq!(m["Author"], "Jane");
        assert_eq!(m["Creation-Date"], "2020-01-02T01:04:05Z");
        assert_eq!(m["pdf:PDFVersion"], "1.4");
    }

    #[test]
    fn dates_and_strings() {
        assert_eq!(pdf_date("D:2021").as_deref(), Some("2021-01-01T00:00:00Z"));
        assert_eq!(
            pdf_date("D:20211231235959Z").as_deref(),
            Some("2021-12-31T23:59:59Z")
        );
        assert_eq!(pdf_date("garbage"), None);
        assert_eq!(pdf_string(b"caf\xE9 \x80"), "café •");
    }

    #[test]
    fn broken_pdfs_fail_cleanly() {
        for data in [
            &b"%PDF-1.4\n"[..],
            b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF",
        ] {
            let src = pm_core::io::MemSource::new(data.to_vec());
            let mut sink = StringSink::default();
            let _ = crate::extract(
                crate::Kind::Pdf,
                &src,
                &mut sink,
                &crate::ExtractOptions::default(),
            );
        }
    }
}
