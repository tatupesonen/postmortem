//! Excel 97–2003 workbooks (BIFF8 `Workbook` stream; BIFF5 `Book` labels):
//! sheet names and cell values, one row per line, cells separated by tabs.

use std::collections::HashMap;

use super::{Cfb, read_stream};
use crate::util::{cp1252, le16, le32, lef64, utf16le};
use crate::{Stop, TextError, TextSink};

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const BOUNDSHEET: u16 = 0x0085;
const SST: u16 = 0x00FC;
const CONTINUE: u16 = 0x003C;
const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;
const BOOLERR: u16 = 0x0205;
const FILEPASS: u16 = 0x002F;
/// Shared strings kept at most (bytes).
const MAX_SST: usize = 64 * 1024 * 1024;

/// One BIFF record.
struct Rec<'a> {
    ty: u16,
    data: &'a [u8],
    /// Offset of the record header in the stream.
    pos: usize,
}

fn records(s: &[u8]) -> impl Iterator<Item = Rec<'_>> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let ty = le16(s, pos)?;
        let len = usize::from(le16(s, pos + 2)?);
        let data = s.get(pos + 4..pos + 4 + len)?;
        let r = Rec { ty, data, pos };
        pos += 4 + len;
        Some(r)
    })
}

pub(super) fn extract(cf: &mut Cfb<'_>, sink: &mut dyn TextSink) -> Result<(), TextError> {
    let stream = if cf.is_stream("/Workbook") {
        read_stream(cf, "/Workbook")?
    } else {
        read_stream(cf, "/Book")?
    };
    // Records are walked lazily: a stream of empty records would
    // otherwise cost 32 bytes of memory per 4 bytes of input.
    let mut recs = records(&stream).peekable();
    let mut sheet_names: HashMap<usize, String> = HashMap::new();
    let mut sst: Vec<String> = Vec::new();
    let mut biff8 = true;
    let mut out = Out::new(sink);
    let mut in_sheet = false;
    let mut pending_string = false;
    while let Some(r) = recs.next() {
        match r.ty {
            BOF => {
                let vers = le16(r.data, 0).unwrap_or(0);
                let dt = le16(r.data, 2).unwrap_or(0);
                biff8 = vers >= 0x0600;
                if dt == 0x0010 {
                    in_sheet = true;
                    if let Some(name) = sheet_names.get(&r.pos) {
                        out.line(name);
                    }
                }
            }
            EOF => {
                if in_sheet {
                    out.end_row()?;
                    out.cur_row = None;
                }
                in_sheet = false;
            }
            FILEPASS => return Err(TextError::Encrypted),
            BOUNDSHEET => {
                let pos = le32(r.data, 0).unwrap_or(0) as usize;
                let name = if biff8 {
                    short_unicode(r.data.get(6..).unwrap_or_default())
                } else {
                    let n = usize::from(r.data.get(6).copied().unwrap_or(0));
                    cp1252(r.data.get(7..7 + n).unwrap_or_default())
                };
                sheet_names.insert(pos, name);
            }
            SST => {
                let mut segs = vec![r.data];
                while let Some(c) = recs.next_if(|c| c.ty == CONTINUE) {
                    segs.push(c.data);
                }
                sst = parse_sst(&segs);
            }
            LABELSST if in_sheet => {
                let idx = le32(r.data, 6).unwrap_or(u32::MAX) as usize;
                let text = sst.get(idx).cloned().unwrap_or_default();
                out.cell(r.data, &text)?;
            }
            LABEL if in_sheet => {
                let text = if biff8 {
                    unicode_string(r.data.get(6..).unwrap_or_default())
                } else {
                    let n = usize::from(le16(r.data, 6).unwrap_or(0));
                    cp1252(r.data.get(8..8 + n).unwrap_or_default())
                };
                out.cell(r.data, &text)?;
            }
            NUMBER if in_sheet => {
                let v = lef64(r.data, 6).unwrap_or(0.0);
                out.cell(r.data, &number(v))?;
            }
            RK if in_sheet => {
                let v = rk(le32(r.data, 6).unwrap_or(0));
                out.cell(r.data, &number(v))?;
            }
            MULRK if in_sheet => {
                let row = le16(r.data, 0).unwrap_or(0);
                let n = r.data.len().saturating_sub(6) / 6;
                for k in 0..n {
                    let v = rk(le32(r.data, 4 + k * 6 + 2).unwrap_or(0));
                    out.cell_at(row, &number(v))?;
                }
            }
            FORMULA if in_sheet => {
                let row = le16(r.data, 0).unwrap_or(0);
                let res = r.data.get(6..14).unwrap_or_default();
                if res.get(6..8) == Some(&[0xFF, 0xFF]) {
                    match res.first() {
                        Some(0) => pending_string = true,
                        Some(1) => out.cell_at(
                            row,
                            if res.get(2) == Some(&1) {
                                "TRUE"
                            } else {
                                "FALSE"
                            },
                        )?,
                        _ => {}
                    }
                } else {
                    out.cell_at(row, &number(lef64(r.data, 6).unwrap_or(0.0)))?;
                }
            }
            STRING if in_sheet && pending_string => {
                pending_string = false;
                let mut data = r.data.to_vec();
                while let Some(c) = recs.next_if(|c| c.ty == CONTINUE) {
                    // A continued string restarts with a flags byte.
                    data.extend(c.data.get(1..).unwrap_or_default());
                }
                let text = unicode_string(&data);
                let row = out.cur_row.unwrap_or(0);
                out.cell_at(row, &text)?;
            }
            BOOLERR if in_sheet && r.data.get(7) == Some(&0) => {
                let v = if r.data.get(6) == Some(&1) {
                    "TRUE"
                } else {
                    "FALSE"
                };
                out.cell(r.data, v)?;
            }
            _ => {}
        }
    }
    out.finish()?;
    Ok(())
}

/// Formats a number like a spreadsheet would show it without a format:
/// integers without a decimal point.
fn number(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Decodes an RK number.
fn rk(v: u32) -> f64 {
    let div100 = v & 1 != 0;
    let int = v & 2 != 0;
    let n = if int {
        f64::from((v as i32) >> 2)
    } else {
        f64::from_bits(u64::from(v & 0xFFFF_FFFC) << 32)
    };
    if div100 { n / 100.0 } else { n }
}

/// `ShortXLUnicodeString`: cch (u8), flags, characters.
fn short_unicode(b: &[u8]) -> String {
    let n = usize::from(b.first().copied().unwrap_or(0));
    let high = b.get(1).is_some_and(|f| f & 1 != 0);
    chars(b.get(2..).unwrap_or_default(), n, high)
}

/// `XLUnicodeString`: cch (u16), flags, characters.
fn unicode_string(b: &[u8]) -> String {
    let n = usize::from(le16(b, 0).unwrap_or(0));
    let high = b.get(2).is_some_and(|f| f & 1 != 0);
    chars(b.get(3..).unwrap_or_default(), n, high)
}

fn chars(b: &[u8], n: usize, high: bool) -> String {
    if high {
        utf16le(b.get(..n * 2).unwrap_or(b))
    } else {
        // "Compressed": the low bytes of UTF-16 (Latin-1).
        b.get(..n)
            .unwrap_or(b)
            .iter()
            .map(|&c| char::from(c))
            .collect()
    }
}

/// Reads the shared-string table from the SST record and its CONTINUE
/// records (strings may cross record boundaries; the remainder of a
/// string's characters then starts with a new flags byte).
fn parse_sst(segs: &[&[u8]]) -> Vec<String> {
    let mut r = SegReader {
        segs,
        seg: 0,
        pos: 8,
    };
    let unique = le32(segs.first().copied().unwrap_or_default(), 4).unwrap_or(0) as usize;
    let mut out = Vec::new();
    let mut total = 0usize;
    for _ in 0..unique.min(10_000_000) {
        let Some(cch) = r.u16() else { break };
        let Some(flags) = r.u8() else { break };
        let rich = flags & 0x08 != 0;
        let ext = flags & 0x04 != 0;
        let runs = if rich {
            usize::from(r.u16().unwrap_or(0))
        } else {
            0
        };
        let ext_len = if ext {
            r.u32().unwrap_or(0) as usize
        } else {
            0
        };
        let mut s = String::new();
        let mut high = flags & 1 != 0;
        let mut left = usize::from(cch);
        while left > 0 {
            if r.at_seg_end() {
                // Continued in the next record: new flags byte.
                if !r.next_seg() {
                    break;
                }
                high = r.u8().is_some_and(|f| f & 1 != 0);
            }
            let avail = r.remaining_in_seg();
            let width = if high { 2 } else { 1 };
            let take = left.min(avail / width);
            if take == 0 {
                break;
            }
            let bytes = r.take(take * width);
            if high {
                s.push_str(&utf16le(&bytes));
            } else {
                s.extend(bytes.iter().map(|&c| char::from(c)));
            }
            left -= take;
        }
        r.skip(runs * 4 + ext_len);
        total += s.len();
        out.push(if total <= MAX_SST { s } else { String::new() });
    }
    out
}

struct SegReader<'a> {
    segs: &'a [&'a [u8]],
    seg: usize,
    pos: usize,
}

impl SegReader<'_> {
    fn cur(&self) -> &[u8] {
        self.segs.get(self.seg).copied().unwrap_or_default()
    }

    fn at_seg_end(&self) -> bool {
        self.pos >= self.cur().len()
    }

    fn remaining_in_seg(&self) -> usize {
        self.cur().len().saturating_sub(self.pos)
    }

    fn next_seg(&mut self) -> bool {
        if self.seg + 1 >= self.segs.len() {
            return false;
        }
        self.seg += 1;
        self.pos = 0;
        true
    }

    /// Takes up to `n` bytes, crossing record boundaries (for fixed-size
    /// fields, which are never split with a flags byte).
    fn take(&mut self, n: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            if self.at_seg_end() && !self.next_seg() {
                break;
            }
            let k = (n - v.len()).min(self.remaining_in_seg());
            v.extend(self.cur().get(self.pos..self.pos + k).unwrap_or_default());
            self.pos += k;
        }
        v
    }

    fn skip(&mut self, n: usize) {
        let mut left = n;
        while left > 0 {
            if self.at_seg_end() && !self.next_seg() {
                return;
            }
            let k = left.min(self.remaining_in_seg());
            self.pos += k;
            left -= k;
        }
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2);
        le16(&b, 0)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4);
        le32(&b, 0)
    }
}

/// Rows and cells.
struct Out<'a> {
    sink: &'a mut dyn TextSink,
    buf: String,
    cur_row: Option<u16>,
}

impl<'a> Out<'a> {
    fn new(sink: &'a mut dyn TextSink) -> Self {
        Self {
            sink,
            buf: String::new(),
            cur_row: None,
        }
    }

    fn line(&mut self, s: &str) {
        self.buf.push_str(s);
        self.buf.push('\n');
    }

    fn end_row(&mut self) -> Result<(), Stop> {
        if self.cur_row.is_some() && !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
        if self.buf.len() >= 64 * 1024 {
            self.sink.push(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    fn cell(&mut self, rec: &[u8], text: &str) -> Result<(), Stop> {
        self.cell_at(le16(rec, 0).unwrap_or(0), text)
    }

    fn cell_at(&mut self, row: u16, text: &str) -> Result<(), Stop> {
        if self.cur_row == Some(row) {
            self.buf.push('\t');
        } else {
            self.end_row()?;
            self.cur_row = Some(row);
        }
        self.buf.push_str(text);
        Ok(())
    }

    fn finish(mut self) -> Result<(), Stop> {
        self.end_row()?;
        if !self.buf.is_empty() {
            self.sink.push(&self.buf)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StringSink;
    use crate::ole::tests::cfb_file;

    fn rec(ty: u16, data: &[u8]) -> Vec<u8> {
        let mut v = ty.to_le_bytes().to_vec();
        v.extend((data.len() as u16).to_le_bytes());
        v.extend(data);
        v
    }

    fn cell_hdr(row: u16, col: u16) -> Vec<u8> {
        let mut v = row.to_le_bytes().to_vec();
        v.extend(col.to_le_bytes());
        v.extend(0u16.to_le_bytes());
        v
    }

    /// Globals (BOUNDSHEET + SST split by CONTINUE) and one sheet.
    pub(crate) fn workbook() -> Vec<u8> {
        let bof = |dt: u16| {
            let mut d = 0x0600u16.to_le_bytes().to_vec();
            d.extend(dt.to_le_bytes());
            d.extend([0u8; 12]);
            rec(BOF, &d)
        };
        // SST: "hello" (compressed) and "wörld" (UTF-16) split across a
        // CONTINUE with a flags byte.
        let mut sst = 2u32.to_le_bytes().to_vec();
        sst.extend(2u32.to_le_bytes());
        sst.extend(5u16.to_le_bytes());
        sst.push(0);
        sst.extend(b"hello");
        sst.extend(5u16.to_le_bytes());
        sst.push(1);
        for c in "wö".encode_utf16() {
            sst.extend(c.to_le_bytes());
        }
        let mut cont = vec![0u8]; // compressed remainder
        cont.extend(b"rld");
        let mut globals = bof(0x0005);
        let boundsheet = |pos: u32| {
            let mut bs = pos.to_le_bytes().to_vec();
            bs.extend([0, 0]);
            bs.extend([6, 0]);
            bs.extend(b"Sheet1");
            rec(BOUNDSHEET, &bs)
        };
        let sheet_pos = (globals.len()
            + boundsheet(0).len()
            + rec(SST, &sst).len()
            + rec(CONTINUE, &cont).len()
            + rec(EOF, &[]).len()) as u32;
        globals.extend(boundsheet(sheet_pos));
        globals.extend(rec(SST, &sst));
        globals.extend(rec(CONTINUE, &cont));
        globals.extend(rec(EOF, &[]));
        assert_eq!(globals.len(), sheet_pos as usize);
        let mut sheet = bof(0x0010);
        let mut l = cell_hdr(0, 0);
        l.extend(0u32.to_le_bytes());
        sheet.extend(rec(LABELSST, &l));
        let mut n = cell_hdr(0, 1);
        n.extend(3.5f64.to_le_bytes());
        sheet.extend(rec(NUMBER, &n));
        let mut l2 = cell_hdr(1, 0);
        l2.extend(1u32.to_le_bytes());
        sheet.extend(rec(LABELSST, &l2));
        let mut r = cell_hdr(1, 1);
        r.extend(((42u32) << 2 | 2).to_le_bytes());
        sheet.extend(rec(RK, &r));
        sheet.extend(rec(EOF, &[]));
        globals.extend(sheet);
        globals
    }

    #[test]
    fn biff8_cells() {
        let data = cfb_file(&[("/Workbook", &workbook())]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        crate::ole::extract(&src, &mut sink).unwrap();
        assert_eq!(sink.text, "Sheet1\nhello\t3.5\nwörld\t42\n");
    }

    #[test]
    fn rk_values() {
        assert_eq!(rk((42u32 << 2) | 2).to_bits(), 42.0f64.to_bits());
        assert_eq!(rk((1234u32 << 2) | 3).to_bits(), 12.34f64.to_bits());
        let bits = (1.5f64.to_bits() >> 32) as u32 & 0xFFFF_FFFC;
        assert_eq!(rk(bits).to_bits(), 1.5f64.to_bits());
        assert_eq!(number(42.0), "42");
        assert_eq!(number(2.25), "2.25");
    }

    #[test]
    fn filepass_is_encrypted() {
        let mut d = 0x0600u16.to_le_bytes().to_vec();
        d.extend(5u16.to_le_bytes());
        let mut s = rec(BOF, &d);
        s.extend(rec(FILEPASS, &[1, 0]));
        let data = cfb_file(&[("/Workbook", &s)]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        assert!(matches!(
            crate::ole::extract(&src, &mut sink),
            Err(TextError::Encrypted)
        ));
    }
}
