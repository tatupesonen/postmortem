//! Word 97–2003 (`WordDocument` stream, [MS-DOC]): text from the piece
//! table in the table stream's `Clx`.

use super::{Cfb, read_stream};
use crate::util::{cp1252, le16, le32};
use crate::{Stop, TextError, TextSink};

/// Characters decoded at most (the document's CP count is untrusted).
const MAX_CHARS: u64 = 512 * 1024 * 1024;
const MAX_PIECES: usize = 1 << 20;

/// Field-code state of one nesting level.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    /// Between the field begin (0x13) and separator (0x14): hidden.
    Instruction,
    /// Between separator and end (0x15): the field's result, shown.
    Result,
}

pub(super) fn extract(cf: &mut Cfb<'_>, sink: &mut dyn TextSink) -> Result<(), TextError> {
    let doc = read_stream(cf, "/WordDocument")?;
    if le16(&doc, 0) != Some(0xA5EC) {
        return Err(TextError::malformed("Word", "bad FIB signature"));
    }
    let n_fib = le16(&doc, 2).unwrap_or(0);
    if n_fib < 101 {
        // Word 6/95: not supported (no piece table in this form).
        return Err(TextError::malformed(
            "Word",
            format!("unsupported nFib {n_fib}"),
        ));
    }
    let flags = le16(&doc, 0x0A).unwrap_or(0);
    if flags & 0x0100 != 0 {
        return Err(TextError::Encrypted);
    }
    let table_name = if flags & 0x0200 != 0 {
        "/1Table"
    } else {
        "/0Table"
    };
    let table = read_stream(cf, table_name)?;
    // FIB: base (32) | csw | fibRgW | cslw | fibRgLw | cbRgFcLcb | fibRgFcLcb.
    let csw = usize::from(le16(&doc, 32).unwrap_or(0));
    let lw_start = 34 + csw * 2;
    let cslw = usize::from(le16(&doc, lw_start).unwrap_or(0));
    let fclcb_start = lw_start + 2 + cslw * 4 + 2;
    let fc_clx = le32(&doc, fclcb_start + 33 * 8).unwrap_or(0) as usize;
    let lcb_clx = le32(&doc, fclcb_start + 33 * 8 + 4).unwrap_or(0) as usize;
    let clx = table
        .get(fc_clx..fc_clx.saturating_add(lcb_clx))
        .ok_or_else(|| TextError::malformed("Word", "Clx outside the table stream"))?;
    let pieces = piece_table(clx)?;
    let mut out = Writer::new(sink);
    let mut total = 0u64;
    for p in pieces {
        let count = u64::from(p.cp_end.saturating_sub(p.cp_start));
        total += count;
        if total > MAX_CHARS {
            break;
        }
        let count = usize::try_from(count).unwrap_or(0);
        if p.compressed {
            let start = p.fc as usize / 2;
            let bytes = doc
                .get(start..start.saturating_add(count))
                .unwrap_or_default();
            out.text(&cp1252(bytes))?;
        } else {
            let start = p.fc as usize;
            let bytes = doc
                .get(start..start.saturating_add(count * 2))
                .unwrap_or_default();
            out.text(&crate::util::utf16le(bytes))?;
        }
    }
    out.finish()?;
    Ok(())
}

struct Piece {
    cp_start: u32,
    cp_end: u32,
    fc: u32,
    compressed: bool,
}

/// Parses the `Clx`: skips `Prc` entries, then reads the `PlcPcd`.
fn piece_table(clx: &[u8]) -> Result<Vec<Piece>, TextError> {
    let mut i = 0usize;
    while clx.get(i) == Some(&0x01) {
        let cb = usize::from(le16(clx, i + 1).unwrap_or(0));
        i += 3 + cb;
    }
    if clx.get(i) != Some(&0x02) {
        return Err(TextError::malformed("Word", "no piece table"));
    }
    let lcb = le32(clx, i + 1).unwrap_or(0) as usize;
    let plc = clx
        .get(i + 5..(i + 5).saturating_add(lcb))
        .ok_or_else(|| TextError::malformed("Word", "truncated piece table"))?;
    if plc.len() < 4 {
        return Ok(Vec::new());
    }
    let n = ((plc.len() - 4) / 12).min(MAX_PIECES);
    let mut v = Vec::with_capacity(n);
    for k in 0..n {
        let cp_start = le32(plc, k * 4).unwrap_or(0);
        let cp_end = le32(plc, (k + 1) * 4).unwrap_or(0);
        let pcd = (n + 1) * 4 + k * 8;
        let fc_raw = le32(plc, pcd + 2).unwrap_or(0);
        v.push(Piece {
            cp_start,
            cp_end,
            fc: fc_raw & 0x3FFF_FFFF,
            compressed: fc_raw & 0x4000_0000 != 0,
        });
    }
    Ok(v)
}

/// Maps Word's special characters and hides field instructions.
struct Writer<'a> {
    sink: &'a mut dyn TextSink,
    buf: String,
    fields: Vec<Field>,
}

impl<'a> Writer<'a> {
    fn new(sink: &'a mut dyn TextSink) -> Self {
        Self {
            sink,
            buf: String::new(),
            fields: Vec::new(),
        }
    }

    fn visible(&self) -> bool {
        self.fields.iter().all(|f| *f == Field::Result)
    }

    fn text(&mut self, s: &str) -> Result<(), Stop> {
        for c in s.chars() {
            match c {
                '\u{13}' => {
                    if self.fields.len() < 64 {
                        self.fields.push(Field::Instruction);
                    }
                }
                '\u{14}' => {
                    if let Some(f) = self.fields.last_mut() {
                        *f = Field::Result;
                    }
                }
                '\u{15}' => {
                    self.fields.pop();
                }
                _ if !self.visible() => {}
                '\r' | '\u{B}' | '\u{C}' | '\u{E}' => self.buf.push('\n'),
                '\u{7}' | '\t' => self.buf.push('\t'),
                '\u{1E}' => self.buf.push('-'),
                c if (c as u32) < 0x20 => {}
                c => self.buf.push(c),
            }
        }
        if self.buf.len() >= 64 * 1024 {
            self.sink.push(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    fn finish(self) -> Result<(), Stop> {
        if !self.buf.is_empty() {
            self.sink.push(&self.buf)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::StringSink;
    use crate::ole::tests::cfb_file;

    /// A minimal Word 97 document: FIB with a Clx pointing at a one-piece
    /// table; `compressed` text is cp1252, otherwise UTF-16LE.
    pub(crate) fn word_doc(text: &str, compressed: bool, encrypted: bool) -> Vec<u8> {
        let mut doc = vec![0u8; 0x800];
        doc[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        doc[2..4].copy_from_slice(&193u16.to_le_bytes());
        let mut flags = 0x0200u16; // 1Table
        if encrypted {
            flags |= 0x0100;
        }
        doc[0x0A..0x0C].copy_from_slice(&flags.to_le_bytes());
        // csw = 14, cslw = 22, cbRgFcLcb = 93.
        doc[32..34].copy_from_slice(&14u16.to_le_bytes());
        doc[62..64].copy_from_slice(&22u16.to_le_bytes());
        doc[152..154].copy_from_slice(&93u16.to_le_bytes());
        let text_fc = 0x400usize;
        let chars: Vec<u16> = text.encode_utf16().collect();
        let fc_field = if compressed {
            for (i, c) in text.bytes().enumerate() {
                doc[text_fc + i] = c;
            }
            ((text_fc * 2) as u32) | 0x4000_0000
        } else {
            for (i, c) in chars.iter().enumerate() {
                doc[text_fc + i * 2..text_fc + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
            }
            text_fc as u32
        };
        let n = chars.len() as u32;
        let mut plc = Vec::new();
        plc.extend(0u32.to_le_bytes());
        plc.extend(n.to_le_bytes());
        plc.extend(0u16.to_le_bytes());
        plc.extend(fc_field.to_le_bytes());
        plc.extend(0u16.to_le_bytes());
        let mut clx = vec![0x01, 2, 0, 0xAA, 0xBB, 0x02];
        clx.extend((plc.len() as u32).to_le_bytes());
        clx.extend(plc);
        let mut table = vec![0u8; 16];
        let fc_clx = table.len() as u32;
        table.extend(&clx);
        let entry = 154 + 33 * 8;
        doc[entry..entry + 4].copy_from_slice(&fc_clx.to_le_bytes());
        doc[entry + 4..entry + 8].copy_from_slice(&(clx.len() as u32).to_le_bytes());
        cfb_file(&[("/WordDocument", &doc), ("/1Table", &table)])
    }

    fn run(data: Vec<u8>) -> Result<String, TextError> {
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        crate::ole::extract(&src, &mut sink).map(|_| sink.text)
    }

    #[test]
    fn unicode_and_compressed_pieces() {
        assert_eq!(
            run(word_doc("Grüße\rzwei\u{7}x", false, false)).unwrap(),
            "Grüße\nzwei\tx"
        );
        assert_eq!(
            run(word_doc("plain text\r", true, false)).unwrap(),
            "plain text\n"
        );
    }

    #[test]
    fn fields_show_results_only() {
        let t = "see \u{13} HYPERLINK \"http://x\" \u{14}the link\u{15} now";
        assert_eq!(run(word_doc(t, false, false)).unwrap(), "see the link now");
        let nested = "a\u{13}IF \u{13}PAGE\u{14}1\u{15}\u{14}yes\u{15}b";
        assert_eq!(run(word_doc(nested, false, false)).unwrap(), "ayesb");
    }

    #[test]
    fn encrypted_word() {
        assert!(matches!(
            run(word_doc("secret", false, true)),
            Err(TextError::Encrypted)
        ));
    }
}
