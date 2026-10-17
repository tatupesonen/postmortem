//! ZIP-based documents: Office Open XML (DOCX, XLSX, PPTX), `OpenDocument`
//! (ODT, ODS, ODP, ODG) and EPUB. Parts are streamed through `quick-xml`;
//! decompression is capped per part and in total (zip bombs).

use std::collections::HashMap;
use std::io::{BufReader, Read};

use pm_core::ReadAt;
use zip::ZipArchive;

use crate::util::{Borrowed, cursor};
use crate::xml::{Ev, attr, walk};
use crate::{Metadata, Stop, TextError, TextSink};

/// Decompressed bytes read from one part at most.
const MAX_PART: u64 = 256 * 1024 * 1024;
/// Decompressed bytes read from one document at most.
const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
/// Shared strings kept for an XLSX workbook at most (bytes).
const MAX_SHARED: usize = 64 * 1024 * 1024;
/// Output is pushed in pieces of about this size.
const FLUSH: usize = 64 * 1024;

struct Archive<'a> {
    zip: ZipArchive<pm_core::io::Cursor<Borrowed<'a>>>,
    budget: u64,
}

/// Counts bytes read through it.
struct Counting<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.n += n as u64;
        Ok(n)
    }
}

impl<'a> Archive<'a> {
    fn open(src: &'a dyn ReadAt, format: &'static str) -> Result<Self, TextError> {
        let zip = ZipArchive::new(cursor(src)).map_err(|e| TextError::malformed(format, e))?;
        Ok(Self {
            zip,
            budget: MAX_TOTAL,
        })
    }

    fn names(&self) -> Vec<String> {
        self.zip.file_names().map(str::to_owned).collect()
    }

    /// Streams part `name` through `f`; `Ok(false)` if it is missing,
    /// encrypted or the budget is spent.
    fn walk_part(
        &mut self,
        name: &str,
        f: impl FnMut(Ev<'_>) -> Result<(), Stop>,
    ) -> Result<bool, Stop> {
        if self.budget == 0 {
            return Ok(false);
        }
        let Ok(file) = self.zip.by_name(name) else {
            return Ok(false);
        };
        if file.encrypted() {
            return Ok(false);
        }
        let mut counting = Counting {
            inner: file.take(MAX_PART.min(self.budget)),
            n: 0,
        };
        let r = walk(BufReader::with_capacity(64 * 1024, &mut counting), f);
        self.budget = self.budget.saturating_sub(counting.n);
        r.map(|()| true)
    }

    /// Reads a small part completely.
    fn read_part(&mut self, name: &str, limit: u64) -> Option<Vec<u8>> {
        let file = self.zip.by_name(name).ok()?;
        let mut out = Vec::new();
        file.take(limit.min(self.budget)).read_to_end(&mut out).ok()?;
        self.budget = self.budget.saturating_sub(out.len() as u64);
        Some(out)
    }
}

/// Pushes text in pieces.
struct Out<'a> {
    sink: &'a mut dyn TextSink,
    buf: String,
}

impl<'a> Out<'a> {
    fn new(sink: &'a mut dyn TextSink) -> Self {
        Self {
            sink,
            buf: String::new(),
        }
    }

    fn push(&mut self, s: &str) -> Result<(), Stop> {
        self.buf.push_str(s);
        if self.buf.len() >= FLUSH {
            self.flush()?;
        }
        Ok(())
    }

    fn ch(&mut self, c: char) {
        self.buf.push(c);
    }

    /// A line break unless the output already ends with one.
    fn newline(&mut self) {
        if !self.buf.is_empty() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
    }

    /// End of a table cell: its paragraph break becomes a tab.
    fn cell_end(&mut self) {
        if self.buf.ends_with('\n') {
            self.buf.pop();
        }
        self.buf.push('\t');
    }

    /// End of a table row: drop the last cell's tab, break the line.
    fn row_end(&mut self) {
        if self.buf.ends_with('\t') {
            self.buf.pop();
        }
        self.newline()
    }

    fn flush(&mut self) -> Result<(), Stop> {
        if !self.buf.is_empty() {
            self.sink.push(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

/// `WordprocessingML` / `DrawingML` text: `t`/`delText` runs, tabs, breaks,
/// paragraphs, table cells; field instructions and phonetic runs are not
/// text.
#[derive(Default)]
struct RunText {
    in_text: u32,
    skip: u32,
    in_tabs: u32,
}

impl RunText {
    fn event(&mut self, ev: Ev<'_>, out: &mut Out<'_>) -> Result<(), Stop> {
        match ev {
            Ev::Start(name, _) => match name {
                b"t" | b"delText" => self.in_text += 1,
                b"instrText" | b"delInstrText" | b"rPh" => self.skip += 1,
                b"tabs" => self.in_tabs += 1,
                b"tab" if self.in_tabs == 0 && self.skip == 0 => out.ch('\t'),
                b"br" | b"cr" if self.skip == 0 => out.ch('\n'),
                _ => {}
            },
            Ev::End(name) => match name {
                b"t" | b"delText" => self.in_text = self.in_text.saturating_sub(1),
                b"instrText" | b"delInstrText" | b"rPh" => self.skip = self.skip.saturating_sub(1),
                b"tabs" => self.in_tabs = self.in_tabs.saturating_sub(1),
                b"p" => out.newline(),
                b"tr" => out.row_end(),
                b"tc" => out.cell_end(),
                _ => {}
            },
            Ev::Text(t) => {
                if self.in_text > 0 && self.skip == 0 {
                    out.push(t)?;
                }
            }
        }
        Ok(())
    }
}

fn walk_run_text(z: &mut Archive<'_>, part: &str, out: &mut Out<'_>) -> Result<bool, Stop> {
    let mut rt = RunText::default();
    let found = z.walk_part(part, |ev| rt.event(ev, out))?;
    out.newline();
    Ok(found)
}

/// Numbered parts like `word/header3.xml`, sorted by number.
fn numbered(names: &[String], prefix: &str, suffix: &str) -> Vec<String> {
    let mut v: Vec<(u64, String)> = names
        .iter()
        .filter_map(|n| {
            let mid = n.strip_prefix(prefix)?.strip_suffix(suffix)?;
            let num = if mid.is_empty() { 0 } else { mid.parse().ok()? };
            Some((num, n.clone()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, n)| n).collect()
}

pub(crate) fn extract_ooxml(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let mut z = Archive::open(src, "OOXML")?;
    let names = z.names();
    let mut out = Out::new(sink);
    let has = |n: &str| names.iter().any(|x| x == n);
    if has("word/document.xml") {
        for part in numbered(&names, "word/header", ".xml") {
            walk_run_text(&mut z, &part, &mut out)?;
        }
        walk_run_text(&mut z, "word/document.xml", &mut out)?;
        for part in ["word/footnotes.xml", "word/endnotes.xml", "word/comments.xml"] {
            walk_run_text(&mut z, part, &mut out)?;
        }
        for part in numbered(&names, "word/footer", ".xml") {
            walk_run_text(&mut z, &part, &mut out)?;
        }
    } else if has("xl/workbook.xml") {
        xlsx(&mut z, &names, &mut out)?;
    } else if names.iter().any(|n| n.starts_with("ppt/slides/")) {
        for part in numbered(&names, "ppt/slides/slide", ".xml") {
            walk_run_text(&mut z, &part, &mut out)?;
        }
        for part in numbered(&names, "ppt/notesSlides/notesSlide", ".xml") {
            walk_run_text(&mut z, &part, &mut out)?;
        }
        for part in numbered(&names, "ppt/comments/comment", ".xml") {
            ppt_comments(&mut z, &part, &mut out)?;
        }
    } else {
        return Err(TextError::malformed("OOXML", "no document part"));
    }
    out.flush()?;
    let mut meta = Metadata::new();
    ooxml_core(&mut z, &mut meta);
    ooxml_app(&mut z, &mut meta);
    Ok(meta)
}

fn ppt_comments(z: &mut Archive<'_>, part: &str, out: &mut Out<'_>) -> Result<(), Stop> {
    let mut in_text = false;
    z.walk_part(part, |ev| {
        match ev {
            Ev::Start(b"text", _) => in_text = true,
            Ev::End(b"text") => {
                in_text = false;
                out.newline();
            }
            Ev::Text(t) if in_text => out.push(t)?,
            _ => {}
        }
        Ok(())
    })?;
    Ok(())
}

fn xlsx(z: &mut Archive<'_>, names: &[String], out: &mut Out<'_>) -> Result<(), Stop> {
    // Shared strings (rich-text runs joined, phonetic runs dropped).
    let mut shared: Vec<String> = Vec::new();
    let mut shared_bytes = 0usize;
    let mut cur = String::new();
    let (mut in_t, mut skip) = (0u32, 0u32);
    z.walk_part("xl/sharedStrings.xml", |ev| {
        match ev {
            Ev::Start(b"si", _) => cur.clear(),
            Ev::Start(b"t", _) => in_t += 1,
            Ev::End(b"t") => in_t = in_t.saturating_sub(1),
            Ev::Start(b"rPh", _) => skip += 1,
            Ev::End(b"rPh") => skip = skip.saturating_sub(1),
            Ev::End(b"si") => {
                shared_bytes += cur.len();
                if shared_bytes <= MAX_SHARED {
                    shared.push(std::mem::take(&mut cur));
                } else {
                    shared.push(String::new());
                }
            }
            Ev::Text(t) if in_t > 0 && skip == 0 && cur.len() < 1024 * 1024 => cur.push_str(t),
            _ => {}
        }
        Ok(())
    })?;
    // Sheet names and targets, in workbook order.
    let rels = sheet_targets(z);
    let mut sheets: Vec<(String, Option<String>)> = Vec::new();
    z.walk_part("xl/workbook.xml", |ev| {
        if let Ev::Start(b"sheet", s) = ev {
            let name = attr(s, b"name").unwrap_or_default();
            let target = attr(s, b"id").and_then(|id| rels.get(&id).cloned());
            sheets.push((name, target));
        }
        Ok(())
    })?;
    let mut done: Vec<String> = Vec::new();
    for (name, target) in &sheets {
        let Some(t) = target else { continue };
        out.push(name)?;
        out.ch('\n');
        sheet(z, t, &shared, out)?;
        done.push(t.clone());
    }
    // Sheets the workbook does not list.
    for part in numbered(names, "xl/worksheets/sheet", ".xml") {
        if !done.contains(&part) {
            sheet(z, &part, &shared, out)?;
        }
    }
    Ok(())
}

/// `xl/_rels/workbook.xml.rels`: relationship id → part name.
fn sheet_targets(z: &mut Archive<'_>) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let _ = z.walk_part("xl/_rels/workbook.xml.rels", |ev| {
        if let Ev::Start(b"Relationship", s) = ev
            && let (Some(id), Some(target)) = (attr(s, b"Id"), attr(s, b"Target")) {
                let target = target.trim_start_matches('/');
                let full = if target.starts_with("xl/") {
                    target.to_owned()
                } else {
                    format!("xl/{target}")
                };
                m.insert(id, full);
            }
        Ok(())
    });
    m
}

fn sheet(z: &mut Archive<'_>, part: &str, shared: &[String], out: &mut Out<'_>) -> Result<(), Stop> {
    let mut cell_type = String::new();
    let mut in_v = false;
    let mut in_is = false;
    let mut first = true;
    let mut value = String::new();
    z.walk_part(part, |ev| {
        match ev {
            Ev::Start(b"row", _) => first = true,
            Ev::End(b"row") => out.ch('\n'),
            Ev::Start(b"c", s) => {
                cell_type = attr(s, b"t").unwrap_or_default();
                value.clear();
            }
            Ev::End(b"c") => {
                let text = match cell_type.as_str() {
                    "s" => value
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i))
                        .map_or("", String::as_str),
                    _ => value.as_str(),
                };
                if !text.is_empty() {
                    if !first {
                        out.ch('\t');
                    }
                    first = false;
                    out.push(text)?;
                }
            }
            Ev::Start(b"v", _) => in_v = true,
            Ev::End(b"v") => in_v = false,
            Ev::Start(b"is", _) => in_is = true,
            Ev::End(b"is") => in_is = false,
            Ev::Text(t) if (in_v || in_is) && value.len() < 1024 * 1024 => value.push_str(t),
            _ => {}
        }
        Ok(())
    })?;
    out.newline()
}

/// Simple text of the elements of a small XML part, by local name.
fn collect_simple(z: &mut Archive<'_>, part: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut cur: Option<String> = None;
    let mut text = String::new();
    let _ = z.walk_part(part, |ev| {
        match ev {
            Ev::Start(name, _) => {
                cur = Some(String::from_utf8_lossy(name).into_owned());
                text.clear();
            }
            Ev::Text(t) => {
                if text.len() < 64 * 1024 {
                    text.push_str(t);
                }
            }
            Ev::End(name) => {
                if cur.as_deref().map(str::as_bytes) == Some(name) {
                    let t = text.trim();
                    if !t.is_empty() {
                        v.push((String::from_utf8_lossy(name).into_owned(), t.to_owned()));
                    }
                }
                cur = None;
                text.clear();
            }
        }
        Ok(())
    });
    v
}

/// `docProps/core.xml` → Tika-style metadata names.
fn ooxml_core(z: &mut Archive<'_>, meta: &mut Metadata) {
    for (k, v) in collect_simple(z, "docProps/core.xml") {
        let key = match k.as_str() {
            "creator" => "Author",
            "lastModifiedBy" => "Last-Author",
            "created" => "Creation-Date",
            "modified" => "Last-Save-Date",
            "title" => "Title",
            "subject" => "Subject",
            "keywords" => "Keywords",
            "description" => "Comments",
            "lastPrinted" => "Last-Printed",
            "revision" => "Revision-Number",
            "version" => "cp:version",
            "category" => "Category",
            "contentStatus" => "Content-Status",
            _ => continue,
        };
        meta.insert(key.into(), v);
    }
}

/// `docProps/app.xml` → Tika-style metadata names.
fn ooxml_app(z: &mut Archive<'_>, meta: &mut Metadata) {
    for (k, v) in collect_simple(z, "docProps/app.xml") {
        let key = match k.as_str() {
            "Application" => "Application-Name",
            "AppVersion" => "Application-Version",
            "Company" => "Company",
            "Manager" => "Manager",
            "Template" => "Template",
            "Pages" => "Page-Count",
            "Words" => "Word-Count",
            "Characters" => "Character Count",
            "Slides" => "Slide-Count",
            "TotalTime" => "Total-Time",
            _ => continue,
        };
        meta.insert(key.into(), v);
    }
}

pub(crate) fn extract_odf(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let mut z = Archive::open(src, "ODF")?;
    let mut out = Out::new(sink);
    let mut in_body = 0u32;
    let mut skip = 0u32;
    let found = z.walk_part("content.xml", |ev| {
        match ev {
            Ev::Start(name, s) => match name {
                b"body" => in_body += 1,
                b"s" if in_body > 0 => {
                    let n = attr(s, b"c").and_then(|c| c.parse::<usize>().ok()).unwrap_or(1);
                    out.push(&" ".repeat(n.min(1000)))?;
                }
                b"tab" if in_body > 0 => out.ch('\t'),
                b"line-break" if in_body > 0 => out.ch('\n'),
                // Tracked changes' deleted text and note citations' numbers
                // are not document text.
                b"tracked-changes" | b"note-citation" => skip += 1,
                _ => {}
            },
            Ev::End(name) => match name {
                b"body" => in_body = in_body.saturating_sub(1),
                b"p" | b"h" | b"list-item" => out.newline(),
                b"table-row" => out.row_end(),
                b"table-cell" => out.cell_end(),
                b"tracked-changes" | b"note-citation" => skip = skip.saturating_sub(1),
                _ => {}
            },
            Ev::Text(t) => {
                if in_body > 0 && skip == 0 {
                    out.push(t)?;
                }
            }
        }
        Ok(())
    })?;
    if !found {
        return Err(TextError::malformed("ODF", "no content.xml"));
    }
    out.newline();
    out.flush()?;
    let mut meta = Metadata::new();
    for (k, v) in collect_simple(&mut z, "meta.xml") {
        let key = match k.as_str() {
            "initial-creator" => "Author",
            "creator" => "Last-Author",
            "creation-date" => "Creation-Date",
            "date" => "Last-Save-Date",
            "title" => "Title",
            "subject" => "Subject",
            "keyword" => "Keywords",
            "description" => "Comments",
            "generator" => "Application-Name",
            "print-date" => "Last-Printed",
            "editing-cycles" => "Revision-Number",
            _ => continue,
        };
        meta.insert(key.into(), v);
    }
    Ok(meta)
}

pub(crate) fn extract_epub(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let mut z = Archive::open(src, "EPUB")?;
    let names = z.names();
    // META-INF/container.xml → the package document.
    let mut opf: Option<String> = None;
    let _ = z.walk_part("META-INF/container.xml", |ev| {
        if let Ev::Start(b"rootfile", s) = ev
            && opf.is_none() {
                opf = attr(s, b"full-path");
            }
        Ok(())
    });
    let mut meta = Metadata::new();
    let mut order: Vec<String> = Vec::new();
    if let Some(opf) = &opf {
        let base = opf.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
        let mut manifest: HashMap<String, String> = HashMap::new();
        let mut spine: Vec<String> = Vec::new();
        let _ = z.walk_part(opf, |ev| {
            match ev {
                Ev::Start(b"item", s) => {
                    if let (Some(id), Some(href)) = (attr(s, b"id"), attr(s, b"href")) {
                        manifest.insert(id, format!("{base}{href}"));
                    }
                }
                Ev::Start(b"itemref", s) => {
                    if let Some(id) = attr(s, b"idref") {
                        spine.push(id);
                    }
                }
                _ => {}
            }
            Ok(())
        });
        order = spine.iter().filter_map(|id| manifest.get(id).cloned()).collect();
        for (k, v) in collect_simple(&mut z, opf) {
            let key = match k.as_str() {
                "title" => "Title",
                "creator" => "Author",
                "publisher" => "Publisher",
                "date" => "Creation-Date",
                "language" => "Language",
                _ => continue,
            };
            meta.entry(key.into()).or_insert(v);
        }
    }
    if order.is_empty() {
        order = names
            .iter()
            .filter(|n| {
                let l = n.to_ascii_lowercase();
                l.ends_with(".xhtml") || l.ends_with(".html") || l.ends_with(".htm")
            })
            .cloned()
            .collect();
    }
    for part in order {
        let Some(bytes) = z.read_part(&part, MAX_PART) else {
            continue;
        };
        let html = String::from_utf8_lossy(&bytes);
        crate::html::render_str(&html, sink)?;
        sink.push("\n")?;
    }
    Ok(meta)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;
    use crate::StringSink;

    /// Builds a ZIP archive from `(name, content)` pairs (stored).
    pub(crate) fn zip(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in parts {
            w.start_file(*name, opts).unwrap();
            w.write_all(content.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn run(f: fn(&dyn ReadAt, &mut dyn TextSink) -> Result<Metadata, TextError>, data: Vec<u8>) -> (String, Metadata) {
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        let m = f(&src, &mut sink).unwrap();
        (sink.text, m)
    }

    const W: &str = "xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"";

    #[test]
    fn docx_text_and_metadata() {
        let doc = format!(
            "<w:document {W}><w:body>\
             <w:p><w:pPr><w:tabs><w:tab w:val=\"left\"/></w:tabs></w:pPr>\
             <w:r><w:t>Hello</w:t></w:r><w:r><w:tab/><w:t xml:space=\"preserve\"> big </w:t></w:r>\
             <w:r><w:fldChar/><w:instrText>HYPERLINK x</w:instrText><w:t>world</w:t></w:r></w:p>\
             <w:tbl><w:tr><w:tc><w:p><w:r><w:t>c1</w:t></w:r></w:p></w:tc>\
             <w:tc><w:p><w:r><w:t>c2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>\
             <w:p><w:r><w:t>a&amp;b</w:t><w:br/><w:delText>gone</w:delText></w:r></w:p>\
             </w:body></w:document>"
        );
        let header = format!("<w:hdr {W}><w:p><w:r><w:t>Head</w:t></w:r></w:p></w:hdr>");
        let core = "<cp:coreProperties xmlns:cp=\"x\" xmlns:dc=\"d\" xmlns:dcterms=\"t\">\
             <dc:creator>Jane</dc:creator><cp:lastModifiedBy>Bob</cp:lastModifiedBy>\
             <dcterms:created>2020-01-02T03:04:05Z</dcterms:created><dc:title>Report</dc:title>\
             </cp:coreProperties>";
        let app = "<Properties><Application>Microsoft Office Word</Application>\
                   <Company>ACME</Company></Properties>";
        let data = zip(&[
            ("[Content_Types].xml", "<Types/>"),
            ("word/document.xml", &doc),
            ("word/header1.xml", &header),
            ("docProps/core.xml", core),
            ("docProps/app.xml", app),
        ]);
        let (t, m) = run(extract_ooxml, data);
        assert_eq!(t, "Head\nHello\t big world\nc1\tc2\na&b\ngone\n");
        assert_eq!(m["Author"], "Jane");
        assert_eq!(m["Last-Author"], "Bob");
        assert_eq!(m["Creation-Date"], "2020-01-02T03:04:05Z");
        assert_eq!(m["Title"], "Report");
        assert_eq!(m["Application-Name"], "Microsoft Office Word");
        assert_eq!(m["Company"], "ACME");
    }

    #[test]
    fn xlsx_cells() {
        let wb = "<workbook><sheets><sheet name=\"First\" sheetId=\"1\" r:id=\"rId1\" xmlns:r=\"r\"/></sheets></workbook>";
        let rels = "<Relationships><Relationship Id=\"rId1\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
        let sst = "<sst><si><t>alpha</t></si><si><r><t>be</t></r><r><t>ta</t></r><rPh><t>x</t></rPh></si></sst>";
        let sheet = "<worksheet><sheetData><row><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>42.5</v></c></row>\
                     <row><c t=\"inlineStr\"><is><t>inline</t></is></c><c t=\"s\"><v>1</v></c><c t=\"s\"><v>99</v></c></row></sheetData></worksheet>";
        let data = zip(&[
            ("xl/workbook.xml", wb),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", sst),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        let (t, _) = run(extract_ooxml, data);
        assert_eq!(t, "First\nalpha\t42.5\ninline\tbeta\n");
    }

    #[test]
    fn pptx_slides_in_order() {
        let s = |x: &str| format!("<p:sld><a:p><a:r><a:t>{x}</a:t></a:r></a:p></p:sld>");
        let (s1, s2, s10) = (s("one"), s("two"), s("ten"));
        let data = zip(&[
            ("ppt/slides/slide10.xml", &s10),
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
        ]);
        let (t, _) = run(extract_ooxml, data);
        assert_eq!(t, "one\ntwo\nten\n");
    }

    #[test]
    fn odt_text_and_meta() {
        let content = "<office:document-content xmlns:office=\"o\" xmlns:text=\"t\" xmlns:table=\"b\">\
             <office:automatic-styles><style>no</style></office:automatic-styles>\
             <office:body><office:text><text:h>Title</text:h><text:p>a<text:s text:c=\"3\"/>b<text:tab/>c\
             <text:line-break/>d</text:p><table:table><table:table-row><table:table-cell><text:p>x</text:p>\
             </table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>";
        let meta = "<office:document-meta><office:meta><meta:initial-creator>Ann</meta:initial-creator>\
                    <dc:title>Doc</dc:title><meta:generator>LibreOffice</meta:generator></office:meta></office:document-meta>";
        let data = zip(&[("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content), ("meta.xml", meta)]);
        let (t, m) = run(extract_odf, data);
        assert_eq!(t, "Title\na   b\tc\nd\nx\n");
        assert_eq!(m["Author"], "Ann");
        assert_eq!(m["Title"], "Doc");
        assert_eq!(m["Application-Name"], "LibreOffice");
    }

    #[test]
    fn epub_spine_order() {
        let container = "<container><rootfiles><rootfile full-path=\"OEBPS/content.opf\"/></rootfiles></container>";
        let opf = "<package><metadata><dc:title>Book</dc:title><dc:creator>Me</dc:creator></metadata>\
                   <manifest><item id=\"b\" href=\"b.xhtml\"/><item id=\"a\" href=\"a.xhtml\"/></manifest>\
                   <spine><itemref idref=\"a\"/><itemref idref=\"b\"/></spine></package>";
        let data = zip(&[
            ("mimetype", "application/epub+zip"),
            ("META-INF/container.xml", container),
            ("OEBPS/content.opf", opf),
            ("OEBPS/b.xhtml", "<html><body><p>second</p></body></html>"),
            ("OEBPS/a.xhtml", "<html><body><p>first</p></body></html>"),
        ]);
        let (t, m) = run(extract_epub, data);
        let first = t.find("first").unwrap();
        let second = t.find("second").unwrap();
        assert!(first < second, "{t:?}");
        assert_eq!(m["Title"], "Book");
        assert_eq!(m["Author"], "Me");
    }

    #[test]
    fn not_a_zip() {
        let src = pm_core::io::MemSource::new(b"PK\x03\x04garbage".to_vec());
        let mut sink = StringSink::default();
        assert!(extract_ooxml(&src, &mut sink).is_err());
    }
}
