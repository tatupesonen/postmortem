//! RTF: a streaming control-word scanner. Skips non-text destinations
//! (font/colour/style tables, pictures, objects, field instructions),
//! decodes `\'hh` with the document or font code page and `\uN` with the
//! `\ucN` skip count, skips `\binN` data, and reads `\info` metadata.

use std::io::BufRead;

use encoding_rs::Encoding;
use pm_core::ReadAt;

use crate::ole::codepage;
use crate::util::SrcReader;
use crate::{Metadata, Stop, TextError, TextSink};

const MAX_DEPTH: usize = 256;
const FLUSH: usize = 64 * 1024;
/// Longest control word / parameter accepted.
const MAX_WORD: usize = 32;

pub(crate) fn extract(src: &dyn ReadAt, sink: &mut dyn TextSink) -> Result<Metadata, TextError> {
    let mut meta = Metadata::new();
    let mut head = [0u8; 5];
    let n = src.read_at(0, &mut head)?;
    if head.get(..n) != Some(b"{\\rtf".as_slice()) {
        return Err(TextError::malformed("RTF", "no {\\rtf header"));
    }
    let mut p = Parser::new(sink, &mut meta);
    p.run(&mut SrcReader::new(src))?;
    p.flush()?;
    Ok(meta)
}

/// Extracts RTF held in memory (Outlook compressed-RTF bodies).
pub(crate) fn extract_bytes(
    data: &[u8],
    sink: &mut dyn TextSink,
    meta: &mut Metadata,
) -> Result<(), Stop> {
    let mut p = Parser::new(sink, meta);
    p.run(&mut std::io::Cursor::new(data))?;
    p.flush()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dest {
    Text,
    Skip,
    FontTable,
    /// An `\info` field: index into `INFO_FIELDS`, or a date field.
    Info(&'static str),
    InfoDate(&'static str),
    Info0,
}

#[derive(Clone, Copy)]
struct State {
    dest: Dest,
    /// Characters to skip after `\uN` (`\ucN`).
    uc: u32,
    /// Current font's code page (0 = document default).
    cp: u32,
    font: i32,
}

struct Parser<'a, 'm> {
    sink: &'a mut dyn TextSink,
    meta: &'m mut Metadata,
    out: String,
    stack: Vec<State>,
    st: State,
    /// Depth beyond `MAX_DEPTH` (groups counted but not saved).
    overflow: usize,
    ansi_cp: u32,
    /// Font number → code page, from `\fonttbl`.
    fonts: Vec<(i32, u32)>,
    cur_font: Option<i32>,
    /// `\'hh` bytes waiting to be decoded together (DBCS).
    pending: Vec<u8>,
    skip_chars: u32,
    /// A `\*` was seen: the next destination is ignorable.
    star: bool,
    info_text: String,
    date: [i32; 5],
    high_surrogate: Option<u16>,
}

impl<'a, 'm> Parser<'a, 'm> {
    fn new(sink: &'a mut dyn TextSink, meta: &'m mut Metadata) -> Self {
        Self {
            sink,
            meta,
            out: String::new(),
            stack: Vec::new(),
            st: State {
                dest: Dest::Text,
                uc: 1,
                cp: 0,
                font: -1,
            },
            overflow: 0,
            ansi_cp: 1252,
            fonts: Vec::new(),
            cur_font: None,
            pending: Vec::new(),
            skip_chars: 0,
            star: false,
            info_text: String::new(),
            date: [0; 5],
            high_surrogate: None,
        }
    }

    fn encoding(&self) -> &'static Encoding {
        let cp = if self.st.cp != 0 {
            self.st.cp
        } else {
            self.ansi_cp
        };
        codepage(cp)
    }

    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let (s, _, _) = self.encoding().decode(&bytes);
        let s = s.into_owned();
        self.emit_str(&s);
    }

    fn flush(&mut self) -> Result<(), Stop> {
        self.flush_pending();
        if !self.out.is_empty() {
            self.sink.push(&self.out)?;
            self.out.clear();
        }
        Ok(())
    }

    fn emit_str(&mut self, s: &str) {
        for c in s.chars() {
            self.emit(c);
        }
    }

    fn emit(&mut self, c: char) {
        match self.st.dest {
            Dest::Text => self.out.push(c),
            Dest::Info(_) if self.info_text.len() < 64 * 1024 => {
                self.info_text.push(c);
            }
            _ => {}
        }
    }

    fn group_open(&mut self) {
        self.flush_pending();
        if self.stack.len() >= MAX_DEPTH {
            self.overflow += 1;
            return;
        }
        self.stack.push(self.st);
    }

    fn group_close(&mut self) {
        self.flush_pending();
        if self.overflow > 0 {
            self.overflow -= 1;
            return;
        }
        let closing = self.st.dest;
        if let Some(s) = self.stack.pop() {
            self.st = s;
        }
        match closing {
            Dest::Info(key) if !matches!(self.st.dest, Dest::Info(k) if k == key) => {
                let v = self.info_text.trim().to_owned();
                if !v.is_empty() {
                    self.meta.insert(key.into(), v);
                }
                self.info_text.clear();
            }
            Dest::InfoDate(key) if !matches!(self.st.dest, Dest::InfoDate(k) if k == key) => {
                let [y, mo, d, h, mi] = self.date;
                if y > 0 {
                    let v = format!("{y:04}-{:02}-{:02}T{h:02}:{mi:02}:00Z", mo.max(1), d.max(1));
                    self.meta.insert(key.into(), v);
                }
                self.date = [0; 5];
            }
            Dest::FontTable => {
                if let Some(f) = self.cur_font.take() {
                    let _ = f;
                }
            }
            _ => {}
        }
        self.star = false;
    }

    fn run<R: BufRead>(&mut self, r: &mut R) -> Result<(), Stop> {
        let mut word = String::new();
        loop {
            if self.out.len() >= FLUSH {
                self.flush()?;
            }
            let Some(b) = next_byte(r) else {
                return Ok(());
            };
            match b {
                b'{' => self.group_open(),
                b'}' => self.group_close(),
                b'\\' => {
                    let Some(c) = next_byte(r) else { return Ok(()) };
                    if c.is_ascii_alphabetic() {
                        word.clear();
                        word.push(char::from(c));
                        let mut param = String::new();
                        loop {
                            match peek_byte(r) {
                                Some(x) if x.is_ascii_alphabetic() && word.len() < MAX_WORD => {
                                    word.push(char::from(x));
                                    r.consume(1);
                                }
                                _ => break,
                            }
                        }
                        if peek_byte(r) == Some(b'-') {
                            param.push('-');
                            r.consume(1);
                        }
                        while let Some(x) = peek_byte(r) {
                            if x.is_ascii_digit() && param.len() < MAX_WORD {
                                param.push(char::from(x));
                                r.consume(1);
                            } else {
                                break;
                            }
                        }
                        if peek_byte(r) == Some(b' ') {
                            r.consume(1);
                        }
                        let n: Option<i32> = if param.is_empty() || param == "-" {
                            None
                        } else {
                            param
                                .parse::<i64>()
                                .ok()
                                .map(|v| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
                        };
                        if word == "bin" {
                            let skip = n.unwrap_or(0).max(0) as u64;
                            skip_bytes(r, skip);
                            continue;
                        }
                        self.control_word(&word, n);
                    } else {
                        self.control_symbol(c, r);
                    }
                }
                b'\r' | b'\n' => {}
                _ => {
                    if self.skip_chars > 0 {
                        self.skip_chars -= 1;
                        continue;
                    }
                    if self.st.dest == Dest::FontTable && b == b';' {
                        continue;
                    }
                    if b < 0x80 {
                        self.flush_pending();
                        self.emit(char::from(b));
                    } else {
                        self.pending.push(b);
                    }
                }
            }
        }
    }

    fn control_symbol<R: BufRead>(&mut self, c: u8, r: &mut R) {
        match c {
            b'\'' => {
                let h = [next_byte(r).unwrap_or(b'0'), next_byte(r).unwrap_or(b'0')];
                if self.skip_chars > 0 {
                    self.skip_chars -= 1;
                    return;
                }
                let v = std::str::from_utf8(&h)
                    .ok()
                    .and_then(|s| u8::from_str_radix(s, 16).ok());
                if let Some(v) = v {
                    self.pending.push(v);
                }
            }
            b'*' => self.star = true,
            b'\\' | b'{' | b'}' => {
                self.flush_pending();
                self.emit(char::from(c));
            }
            b'~' => {
                self.flush_pending();
                self.emit('\u{A0}');
            }
            b'_' => {
                self.flush_pending();
                self.emit('-');
            }
            b'\r' | b'\n' => {
                self.flush_pending();
                self.emit('\n');
            }
            b'\t' => {
                self.flush_pending();
                self.emit('\t');
            }
            _ => {}
        }
    }

    fn set_dest(&mut self, d: Dest) {
        self.st.dest = d;
    }

    fn control_word(&mut self, w: &str, n: Option<i32>) {
        // Destinations.
        let skip_dest = matches!(
            w,
            "colortbl"
                | "stylesheet"
                | "listtable"
                | "listoverridetable"
                | "revtbl"
                | "rsidtbl"
                | "generator"
                | "pict"
                | "object"
                | "objdata"
                | "themedata"
                | "colorschememapping"
                | "datastore"
                | "latentstyles"
                | "xmlnstbl"
                | "fldinst"
                | "filetbl"
                | "pgdsctbl"
                | "mmathPr"
                | "wgrffmtfilter"
                | "bkmkstart"
                | "bkmkend"
                | "protusertbl"
                | "nonshppict"
                | "blipuid"
                | "pnseclvl"
                | "listtext"
                | "fontemb"
                | "falt"
                | "panose"
                | "xmlopen"
                | "xmlattrname"
                | "xmlattrvalue"
        );
        if skip_dest {
            self.flush_pending();
            self.set_dest(Dest::Skip);
            self.star = false;
            return;
        }
        let in_info = matches!(self.st.dest, Dest::Info0);
        match w {
            "fonttbl" => {
                self.set_dest(Dest::FontTable);
                return;
            }
            "info" => {
                self.set_dest(Dest::Info0);
                return;
            }
            "title" | "subject" | "author" | "operator" | "keywords" | "doccomm" | "company"
            | "manager" | "category"
                if in_info || self.star =>
            {
                let key = match w {
                    "title" => "Title",
                    "subject" => "Subject",
                    "author" => "Author",
                    "operator" => "Last-Author",
                    "keywords" => "Keywords",
                    "doccomm" => "Comments",
                    "company" => "Company",
                    "manager" => "Manager",
                    _ => "Category",
                };
                self.info_text.clear();
                self.set_dest(Dest::Info(key));
                self.star = false;
                return;
            }
            "creatim" | "revtim" | "printim" if in_info => {
                let key = match w {
                    "creatim" => "Creation-Date",
                    "revtim" => "Last-Save-Date",
                    _ => "Last-Printed",
                };
                self.date = [0; 5];
                self.set_dest(Dest::InfoDate(key));
                return;
            }
            "yr" | "mo" | "dy" | "hr" | "min" => {
                if let Dest::InfoDate(_) = self.st.dest {
                    let i = match w {
                        "yr" => 0,
                        "mo" => 1,
                        "dy" => 2,
                        "hr" => 3,
                        _ => 4,
                    };
                    if let Some(slot) = self.date.get_mut(i) {
                        *slot = n.unwrap_or(0);
                    }
                }
                return;
            }
            _ => {}
        }
        if self.star && !matches!(w, "ud" | "upr" | "shppict") {
            // Unknown ignorable destination.
            self.flush_pending();
            self.set_dest(Dest::Skip);
            self.star = false;
            return;
        }
        self.star = false;
        if matches!(self.st.dest, Dest::Info0) {
            // Other \info fields (\version, \nofpages, …).
            self.set_dest(Dest::Skip);
            return;
        }
        match w {
            "ansicpg" => self.ansi_cp = n.unwrap_or(1252).max(0) as u32,
            "f" => {
                let f = n.unwrap_or(0);
                if self.st.dest == Dest::FontTable {
                    self.cur_font = Some(f);
                } else {
                    self.flush_pending();
                    self.st.font = f;
                    self.st.cp = self
                        .fonts
                        .iter()
                        .find(|(id, _)| *id == f)
                        .map_or(0, |&(_, cp)| cp);
                }
            }
            "fcharset" if self.st.dest == Dest::FontTable => {
                if let Some(f) = self.cur_font {
                    let cp = match n.unwrap_or(0) {
                        77 => 10000,
                        128 => 932,
                        129 => 949,
                        134 => 936,
                        136 => 950,
                        161 => 1253,
                        162 => 1254,
                        163 => 1258,
                        177 => 1255,
                        178 => 1256,
                        186 => 1257,
                        204 => 1251,
                        222 => 874,
                        238 => 1250,
                        _ => 0,
                    };
                    if cp != 0 && self.fonts.len() < 4096 {
                        self.fonts.push((f, cp));
                    }
                }
            }
            "cpg" if self.st.dest == Dest::FontTable => {
                if let (Some(f), Some(cp)) = (self.cur_font, n)
                    && cp > 0
                    && self.fonts.len() < 4096
                {
                    self.fonts.push((f, cp as u32));
                }
            }
            "uc" => self.st.uc = n.unwrap_or(1).clamp(0, 16) as u32,
            "u" => {
                self.flush_pending();
                let v = n.unwrap_or(0);
                let unit = if v < 0 { (v + 65536) as u32 } else { v as u32 } as u16;
                match (self.high_surrogate.take(), unit) {
                    (_, 0xD800..=0xDBFF) => self.high_surrogate = Some(unit),
                    (Some(hi), 0xDC00..=0xDFFF) => {
                        let c = char::decode_utf16([hi, unit]).next().and_then(Result::ok);
                        self.emit(c.unwrap_or(char::REPLACEMENT_CHARACTER));
                    }
                    (_, u) => {
                        self.emit(
                            char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
                        );
                    }
                }
                self.skip_chars = self.st.uc;
            }
            "par" | "line" | "sect" | "page" | "row" | "softline" | "lbr" => {
                self.flush_pending();
                self.emit('\n');
            }
            "tab" | "cell" | "nestcell" => {
                self.flush_pending();
                self.emit('\t');
            }
            "emdash" => self.sym('—'),
            "endash" => self.sym('–'),
            "bullet" => self.sym('•'),
            "lquote" => self.sym('‘'),
            "rquote" => self.sym('’'),
            "ldblquote" => self.sym('“'),
            "rdblquote" => self.sym('”'),
            "emspace" | "enspace" | "qmspace" => self.sym(' '),
            _ => {}
        }
    }

    fn sym(&mut self, c: char) {
        self.flush_pending();
        self.emit(c);
    }
}

fn next_byte<R: BufRead>(r: &mut R) -> Option<u8> {
    let b = *r.fill_buf().ok()?.first()?;
    r.consume(1);
    Some(b)
}

fn peek_byte<R: BufRead>(r: &mut R) -> Option<u8> {
    r.fill_buf().ok()?.first().copied()
}

fn skip_bytes<R: BufRead>(r: &mut R, mut n: u64) {
    while n > 0 {
        let Ok(buf) = r.fill_buf() else { return };
        if buf.is_empty() {
            return;
        }
        let k = usize::try_from(n.min(buf.len() as u64)).unwrap_or(buf.len());
        r.consume(k);
        n -= k as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;

    fn run(rtf: &[u8]) -> (String, Metadata) {
        let src = pm_core::io::MemSource::new(rtf.to_vec());
        let mut sink = StringSink::default();
        let m = extract(&src, &mut sink).unwrap();
        (sink.text, m)
    }

    #[test]
    fn text_tables_and_skipped_destinations() {
        let (t, m) = run(
            b"{\\rtf1\\ansi\\ansicpg1252{\\fonttbl{\\f0 Times;}}{\\colortbl;\\red0;}\
              {\\info{\\title Report}{\\author Jane}{\\operator Bob}{\\creatim\\yr2020\\mo1\\dy2\\hr3\\min4}}\
              {\\*\\generator Word;}\\pard Hello \\b bold \\b0 world\\par \
              caf\\'e9 \\u8364? end\\tab x\\par\
              {\\field{\\*\\fldinst HYPERLINK \"http://x\"}{\\fldrslt link text}}\\par\
              {\\pict\\wmetafile8 0102030405}\\bin3 {}} after}",
        );
        assert_eq!(t, "Hello bold world\ncafé € end\tx\nlink text\n after");
        assert_eq!(m["Title"], "Report");
        assert_eq!(m["Author"], "Jane");
        assert_eq!(m["Last-Author"], "Bob");
        assert_eq!(m["Creation-Date"], "2020-01-02T03:04:00Z");
    }

    #[test]
    fn font_code_pages_and_unicode_skip() {
        let (t, _) = run(
            b"{\\rtf1\\ansi{\\fonttbl{\\f1\\fcharset204 Arial Cyr;}}\\f1 \\'cf\\'f0\\'e8\\par\\uc2\\u20013\\'d6\\'d0 x}",
        );
        assert_eq!(t, "При\n中 x");
    }

    #[test]
    fn not_rtf_and_garbage() {
        let src = pm_core::io::MemSource::new(b"hello".to_vec());
        let mut sink = StringSink::default();
        assert!(extract(&src, &mut sink).is_err());
        for g in [
            &b"{\\rtf"[..],
            b"{\\rtf\\",
            b"{\\rtf\\'",
            b"{\\rtf}}}}}{{{",
            b"{\\rtf\\u-99999999999",
        ] {
            let _ = run(g);
        }
    }
}
