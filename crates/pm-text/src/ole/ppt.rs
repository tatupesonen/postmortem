//! `PowerPoint` 97–2003 (`PowerPoint Document` stream): the text atoms of
//! slides, notes and drawings, in stream order.

use super::{Cfb, read_stream};
use crate::util::{le16, le32, utf16le};
use crate::{Stop, TextError, TextSink};

const TEXT_CHARS_ATOM: u16 = 0x0FA0;
const TEXT_BYTES_ATOM: u16 = 0x0FA8;
const CRYPT_SESSION: u16 = 0x2F14;
const MAX_DEPTH: u32 = 32;
const MAX_RECORDS: usize = 10_000_000;

pub(super) fn extract(cf: &mut Cfb<'_>, sink: &mut dyn TextSink) -> Result<(), TextError> {
    let stream = read_stream(cf, "/PowerPoint Document")?;
    let mut w = Walker {
        sink,
        buf: String::new(),
        records: 0,
        encrypted: false,
    };
    w.walk(&stream, 0)?;
    if w.encrypted {
        return Err(TextError::Encrypted);
    }
    if !w.buf.is_empty() {
        w.sink.push(&w.buf)?;
    }
    Ok(())
}

struct Walker<'a> {
    sink: &'a mut dyn TextSink,
    buf: String,
    records: usize,
    encrypted: bool,
}

impl Walker<'_> {
    fn walk(&mut self, data: &[u8], depth: u32) -> Result<(), Stop> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let mut pos = 0usize;
        while pos + 8 <= data.len() {
            self.records += 1;
            if self.records > MAX_RECORDS {
                return Ok(());
            }
            let ver_inst = le16(data, pos).unwrap_or(0);
            let ty = le16(data, pos + 2).unwrap_or(0);
            let len = le32(data, pos + 4).unwrap_or(0) as usize;
            let body_start = pos + 8;
            let body_end = body_start.saturating_add(len).min(data.len());
            let body = data.get(body_start..body_end).unwrap_or_default();
            match ty {
                CRYPT_SESSION => self.encrypted = true,
                TEXT_CHARS_ATOM => self.text(&utf16le(body))?,
                TEXT_BYTES_ATOM => {
                    let s: String = body.iter().map(|&b| char::from(b)).collect();
                    self.text(&s)?;
                }
                _ if ver_inst & 0x000F == 0x000F => self.walk(body, depth + 1)?,
                _ => {}
            }
            if len == 0 && ver_inst & 0x000F != 0x000F && ty == 0 {
                // Zero padding: nothing more to read here.
                break;
            }
            pos = body_end;
        }
        Ok(())
    }

    fn text(&mut self, s: &str) -> Result<(), Stop> {
        for c in s.chars() {
            match c {
                '\r' | '\u{B}' => self.buf.push('\n'),
                '\t' => self.buf.push('\t'),
                c if (c as u32) < 0x20 => {}
                c => self.buf.push(c),
            }
        }
        if !self.buf.ends_with('\n') {
            self.buf.push('\n');
        }
        if self.buf.len() >= 64 * 1024 {
            self.sink.push(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::StringSink;
    use crate::ole::tests::cfb_file;

    fn rec(ver: u16, ty: u16, body: &[u8]) -> Vec<u8> {
        let mut v = ver.to_le_bytes().to_vec();
        v.extend(ty.to_le_bytes());
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn text_atoms_in_containers() {
        let chars: Vec<u8> = "Título\rdos".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut inner = rec(0, super::TEXT_CHARS_ATOM, &chars);
        inner.extend(rec(0, super::TEXT_BYTES_ATOM, b"bytes atom"));
        let mut doc = rec(0x000F, 0x03E8, &inner);
        doc.extend(rec(0, 0x0FF5, &[1, 2, 3, 4]));
        let data = cfb_file(&[("/PowerPoint Document", &doc), ("/Current User", b"x")]);
        let src = pm_core::io::MemSource::new(data);
        let mut sink = StringSink::default();
        crate::ole::extract(&src, &mut sink).unwrap();
        assert_eq!(sink.text, "Título\ndos\nbytes atom\n");
    }
}
