//! Embedded content of Office documents (Autopsy
//! `DocumentEmbeddedContentExtractor`).
//!
//! * **Office Open XML** (docx/xlsx/pptx): the parts under `…/media/` and
//!   `…/embeddings/`, named by their file name (duplicates skipped), as
//!   Tika's embedded-document extractor hands them to Autopsy.
//! * **Word 97–2003**: every picture BLIP record (MS-ODRAW) in the `Data`
//!   and `WordDocument` streams, named `image_<n>.<ext>` (POI's
//!   `PicturesTable.getAllPictures`).
//! * **Excel 97–2003**: the BLIPs of the drawing group (`MsoDrawingGroup`
//!   records and their `Continue` records, reassembled), `image_<n>.<ext>`.
//! * **`PowerPoint` 97–2003**: the `Pictures` stream; like Autopsy only JPEG,
//!   PNG, WMF, EMF and PICT pictures are kept.
//!
//! Metafile BLIPs are inflated; WMF gets an Aldus placeable header and
//! PICT a 512-byte header, DIB a bitmap file header (as POI's `getData`
//! does), so the extracted files open in ordinary viewers.

use std::collections::HashSet;
use std::io::{Read, Write};

use pm_core::{Bytes, ReadAt};

use crate::cfb::Cfb;
use crate::detect::Format;
use crate::driver::{CopyError, Driver};
use crate::io::{RangeReader, read_upto};
use crate::{ArchiveError, Entry, MemberEnd};

/// Refines a zip into an Office Open XML document.
pub(crate) fn ooxml_kind(src: &dyn ReadAt) -> Option<Format> {
    let names = crate::zip::names(src, 4000);
    if !names.iter().any(|n| n == b"[Content_Types].xml") {
        return None;
    }
    let has = |p: &[u8]| names.iter().any(|n| n.starts_with(p));
    if has(b"word/") {
        Some(Format::Docx)
    } else if has(b"xl/") {
        Some(Format::Xlsx)
    } else if has(b"ppt/") {
        Some(Format::Pptx)
    } else {
        None
    }
}

/// Refines a compound file into a Word, Excel or `PowerPoint` document.
pub(crate) fn ole_kind(src: &dyn ReadAt) -> Option<Format> {
    let cfb = Cfb::open(src).ok()?;
    if cfb.stream("WordDocument").is_some() {
        Some(Format::Doc)
    } else if cfb.stream("Workbook").is_some() || cfb.stream("Book").is_some() {
        Some(Format::Xls)
    } else if cfb.stream("PowerPoint Document").is_some() {
        Some(Format::Ppt)
    } else {
        None
    }
}

/// Media and embeddings of an Office Open XML package.
pub(crate) fn extract_ooxml(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut filter = |e: &Entry| -> Option<Entry> {
        if e.is_dir {
            return None;
        }
        let p = e.path.replace('\\', "/");
        let keep = p.contains("/media/")
            || p.contains("/embeddings/")
            || p.starts_with("media/")
            || p.starts_with("embeddings/");
        if !keep {
            return None;
        }
        let name = p.rsplit('/').next().unwrap_or_default().to_owned();
        if name.is_empty() || !seen.insert(name.clone()) {
            return None;
        }
        Some(Entry {
            path: name,
            raw_path: None,
            ..e.clone()
        })
    };
    crate::zip::extract(src, d, Some(&mut filter))
}

/// BLIP record types (MS-ODRAW 2.2.23).
const EMF: u16 = 0xF01A;
const WMF: u16 = 0xF01B;
const PICT: u16 = 0xF01C;
const JPEG: u16 = 0xF01D;
const PNG: u16 = 0xF01E;
const DIB: u16 = 0xF01F;
const TIFF: u16 = 0xF029;
const JPEG_CMYK: u16 = 0xF02A;

/// The record instances (one-UID form; `+1` adds a second UID) per type.
fn instances(ty: u16) -> &'static [u16] {
    match ty {
        EMF => &[0x3D4],
        WMF => &[0x216],
        PICT => &[0x542],
        JPEG => &[0x46A, 0x6E2],
        PNG => &[0x6E0],
        DIB => &[0x7A8],
        TIFF => &[0x6E4],
        JPEG_CMYK => &[0x6E2],
        _ => &[],
    }
}

/// A BLIP found in a stream.
#[derive(Debug, Clone)]
struct Blip {
    ty: u16,
    /// Offset of the image data (after the BLIP header).
    data: u64,
    /// Bytes of image data in the record.
    len: u64,
    uid: [u8; 16],
    /// Metafiles: compressed with deflate.
    deflated: bool,
    /// Metafiles: uncompressed size, bounds.
    meta_size: u64,
    bounds: [i32; 4],
}

impl Blip {
    fn ext(&self, jpeg: &'static str) -> &'static str {
        match self.ty {
            EMF => "emf",
            WMF => "wmf",
            PICT => "pict",
            PNG => "png",
            DIB => "dib",
            TIFF => "tiff",
            _ => jpeg,
        }
    }
}

/// Parses and validates a BLIP record header at `at` (`None`: not a BLIP).
fn parse_blip(src: &dyn ReadAt, at: u64) -> Option<(Blip, u64)> {
    let h = read_upto(src, at, 8 + 32 + 34 + 8).ok()?;
    let ver_inst = h.u16_le(0).ok()?;
    let ty = h.u16_le(2).ok()?;
    let rec_len = u64::from(h.u32_le(4).ok()?);
    if ver_inst & 0x0F != 0 {
        return None;
    }
    let inst = ver_inst >> 4;
    let base = inst & !1;
    if !instances(ty).contains(&base) {
        return None;
    }
    let end = at.checked_add(8)?.checked_add(rec_len)?;
    if end > src.size() {
        return None;
    }
    let uids = if inst & 1 == 1 { 2 } else { 1 };
    let mut uid = [0u8; 16];
    uid.copy_from_slice(h.get(8..24)?);
    let after_uids = 8 + 16 * uids;
    let mut b = Blip {
        ty,
        data: 0,
        len: 0,
        uid,
        deflated: false,
        meta_size: 0,
        bounds: [0; 4],
    };
    if matches!(ty, EMF | WMF | PICT) {
        let m = h.get(after_uids..after_uids + 34)?;
        let cb_size = u64::from(m.u32_le(0).ok()?);
        let cb_save = u64::from(m.u32_le(28).ok()?);
        let compression = m.u8_at(32).ok()?;
        let data = at + after_uids as u64 + 34;
        if data.checked_add(cb_save)? > end || compression != 0 && compression != 0xFE {
            return None;
        }
        b.data = data;
        b.len = cb_save;
        b.deflated = compression == 0;
        b.meta_size = cb_size;
        for (i, v) in b.bounds.iter_mut().enumerate() {
            *v = m.i32_le(4 + i * 4).ok()?;
        }
        let first = read_upto(src, data, 4).ok()?;
        let ok = if b.deflated {
            first.first() == Some(&0x78)
        } else {
            match ty {
                EMF => first.u32_le(0).ok() == Some(1),
                WMF => {
                    first.u32_le(0).ok() == Some(0x9AC6_CDD7)
                        || matches!(first.u16_le(0).ok(), Some(1 | 2))
                }
                _ => true,
            }
        };
        ok.then_some((b, end))
    } else {
        let data = at + after_uids as u64 + 1;
        if data > end {
            return None;
        }
        b.data = data;
        b.len = end - data;
        let sig = read_upto(src, data, 8).ok()?;
        let ok = match ty {
            JPEG | JPEG_CMYK => sig.starts_with(&[0xFF, 0xD8, 0xFF]),
            PNG => sig.starts_with(&[0x89, b'P', b'N', b'G']),
            DIB => matches!(sig.u32_le(0).ok(), Some(12 | 40 | 52 | 56 | 108 | 124)),
            TIFF => sig.starts_with(b"II*\0") || sig.starts_with(b"MM\0*"),
            _ => false,
        };
        ok.then_some((b, end))
    }
}

/// Finds BLIP records anywhere in a stream.
fn scan_blips(src: &dyn ReadAt, start: u64, d: &Driver<'_>) -> Result<Vec<Blip>, ArchiveError> {
    const WIN: usize = 1 << 20;
    let mut out = Vec::new();
    let mut pos = start;
    let size = src.size();
    while pos + 8 <= size {
        d.check_cancel()?;
        let buf = read_upto(src, pos, WIN)?;
        if buf.len() < 8 {
            break;
        }
        let mut next_pos = pos + (buf.len() - 7) as u64;
        let mut i = 0usize;
        while let Some(off) = memchr::memchr(0xF0, buf.get(i..).unwrap_or_default()) {
            let p = i + off;
            i = p + 1;
            // The record type's high byte is at offset 3.
            if p < 3 || p + 5 > buf.len() {
                continue;
            }
            let lo = buf.get(p - 1).copied().unwrap_or(0);
            if !(0x1A..=0x2A).contains(&lo) {
                continue;
            }
            let at = pos + (p - 3) as u64;
            if let Some((b, end)) = parse_blip(src, at) {
                out.push(b);
                if end > pos + buf.len() as u64 {
                    next_pos = end;
                    break;
                }
                i = (end - pos) as usize;
            }
        }
        pos = next_pos.max(pos + 1);
    }
    Ok(out)
}

/// The `PowerPoint` `Pictures` stream: consecutive BLIP records.
fn pictures_stream(src: &dyn ReadAt, d: &Driver<'_>) -> Result<Vec<Blip>, ArchiveError> {
    let mut out = Vec::new();
    let mut pos = 0u64;
    while pos + 8 <= src.size() {
        d.check_cancel()?;
        match parse_blip(src, pos) {
            Some((b, end)) => {
                out.push(b);
                pos = end;
            }
            None => {
                // Damaged or unknown record: fall back to scanning the rest.
                out.extend(scan_blips(src, pos, d)?);
                break;
            }
        }
    }
    Ok(out)
}

/// Reassembles the Excel drawing group (`MsoDrawingGroup` + `Continue`).
fn xls_drawing_group(src: &dyn ReadAt, max: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0u64;
    let mut in_group = false;
    while pos + 4 <= src.size() {
        let Ok(h) = read_upto(src, pos, 4) else {
            break;
        };
        let (Ok(ty), Ok(len)) = (h.u16_le(0), h.u16_le(2)) else {
            break;
        };
        let body = pos + 4;
        match ty {
            0x00EB => in_group = true,
            0x003C if in_group => {}
            _ => in_group = false,
        }
        if in_group {
            if out.len() as u64 + u64::from(len) > max {
                break;
            }
            if let Ok(b) = read_upto(src, body, usize::from(len)) {
                out.extend_from_slice(&b);
            }
        }
        pos = body + u64::from(len);
    }
    out
}

/// Pictures of a Word, Excel or `PowerPoint` 97–2003 document.
pub(crate) fn extract_ole(
    format: Format,
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
) -> Result<(), ArchiveError> {
    let cfb =
        Cfb::open(src).map_err(|e| ArchiveError::malformed("compound file", e.to_string()))?;
    let open = |name: &str| cfb.stream(name).and_then(|e| cfb.open_stream(e).ok());
    let mut index = 0u64;
    match format {
        Format::Doc => {
            let mut seen = HashSet::new();
            for name in ["Data", "WordDocument"] {
                let Some(s) = open(name) else { continue };
                for b in scan_blips(&s, 0, d)? {
                    if seen.insert(b.uid) {
                        emit(&s, d, &b, &mut index, "jpg")?;
                    }
                }
            }
        }
        Format::Xls => {
            let Some(s) = open("Workbook").or_else(|| open("Book")) else {
                return Ok(());
            };
            let group = xls_drawing_group(&s, d.opts.max_in_memory);
            let mem = pm_core::io::MemSource::new(group);
            for b in scan_blips(&mem, 0, d)? {
                emit(&mem, d, &b, &mut index, "jpeg")?;
            }
        }
        Format::Ppt => {
            let Some(s) = open("Pictures") else {
                return Ok(());
            };
            for b in pictures_stream(&s, d)? {
                if matches!(b.ty, JPEG | PNG | WMF | EMF | PICT) {
                    emit(&s, d, &b, &mut index, "jpg")?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Delivers one picture as `image_<n>.<ext>`.
fn emit(
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
    b: &Blip,
    index: &mut u64,
    jpeg_ext: &'static str,
) -> Result<(), ArchiveError> {
    let entry = Entry {
        index: *index,
        path: format!("image_{index}.{}", b.ext(jpeg_ext)),
        size: Some(if b.deflated { b.meta_size } else { b.len }),
        packed_size: Some(b.len),
        ..Entry::default()
    };
    *index += 1;
    if !d.begin(&entry)? {
        return d.end(&MemberEnd::Complete);
    }
    let mut prefix = Vec::new();
    match b.ty {
        WMF => {
            let first = read_upto(src, b.data, 2).unwrap_or_default();
            // Raw (undeflated) placeable files already have the header.
            if b.deflated || first != [0xD7, 0xCD] {
                prefix = placeable_header(b.bounds);
            }
        }
        PICT => prefix = vec![0u8; 512],
        DIB => {
            let h = read_upto(src, b.data, 40).unwrap_or_default();
            prefix = bmp_file_header(&h, b.len);
        }
        _ => {}
    }
    d.write(&prefix)?;
    let mut raw = RangeReader::new(src, b.data, b.len);
    let mut reader: Box<dyn Read> = if b.deflated {
        Box::new(flate2::read::ZlibDecoder::new(&mut raw))
    } else {
        Box::new(&mut raw)
    };
    let end = match d.copy(&mut reader, None) {
        Ok(()) => MemberEnd::Complete,
        Err(CopyError::Read(e)) => MemberEnd::Failed(e.to_string()),
        Err(CopyError::Stop(e)) => return Err(e),
    };
    d.end(&end)
}

/// Aldus placeable metafile header for WMF data.
fn placeable_header(bounds: [i32; 4]) -> Vec<u8> {
    let clamp = |v: i32| i16::try_from(v).unwrap_or(if v < 0 { i16::MIN } else { i16::MAX });
    let mut h = Vec::with_capacity(22);
    h.write_all(&0x9AC6_CDD7u32.to_le_bytes()).ok();
    h.write_all(&0u16.to_le_bytes()).ok();
    for v in bounds {
        h.write_all(&clamp(v).to_le_bytes()).ok();
    }
    h.write_all(&1440u16.to_le_bytes()).ok();
    h.write_all(&0u32.to_le_bytes()).ok();
    let checksum = h
        .chunks_exact(2)
        .fold(0u16, |acc, c| acc ^ c.u16_le(0).unwrap_or(0));
    h.write_all(&checksum.to_le_bytes()).ok();
    h
}

/// `BITMAPFILEHEADER` for a DIB of `len` bytes.
fn bmp_file_header(info: &[u8], len: u64) -> Vec<u8> {
    let total = u32::try_from(len + 14).unwrap_or(u32::MAX);
    let header_size = info.u32_le(0).unwrap_or(40);
    let bits = info.u16_le(14).unwrap_or(24);
    let clr_used = info.u32_le(32).unwrap_or(0);
    let compression = info.u32_le(16).unwrap_or(0);
    let palette = if clr_used > 0 {
        clr_used
    } else if bits <= 8 {
        1u32 << bits
    } else {
        0
    };
    let masks = if compression == 3 && header_size == 40 {
        12
    } else {
        0
    };
    let offset = 14u32
        .saturating_add(header_size)
        .saturating_add(palette.saturating_mul(4))
        .saturating_add(masks);
    let mut h = Vec::with_capacity(14);
    h.extend_from_slice(b"BM");
    h.extend_from_slice(&total.to_le_bytes());
    h.extend_from_slice(&[0, 0, 0, 0]);
    h.extend_from_slice(&offset.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn blip_png(second_uid: bool) -> Vec<u8> {
        let png = [0x89, b'P', b'N', b'G', 13, 10, 26, 10, 1, 2, 3];
        let inst: u16 = 0x6E0 | u16::from(second_uid);
        let uids = if second_uid { 32 } else { 16 };
        let mut r = Vec::new();
        r.extend((inst << 4).to_le_bytes());
        r.extend(PNG.to_le_bytes());
        r.extend(((uids + 1 + png.len()) as u32).to_le_bytes());
        r.extend(vec![0xAB; uids]);
        r.push(0xFF);
        r.extend(png);
        r
    }

    #[test]
    fn finds_blips_in_noise() {
        let mut data = vec![0x1Au8, 0xF0, 0xF0, 0x1E, 0xF0];
        data.extend(blip_png(false));
        data.extend([0u8; 37]);
        data.extend(blip_png(true));
        let src = MemSource::new(data);
        let opts = crate::Options::default();
        let mut nul = Nul;
        let d = Driver::new(src.size(), &opts, &mut nul);
        let found = scan_blips(&src, 0, &d).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].len, 11);
        assert_eq!(found[1].len, 11);
        assert_eq!(found[0].ext("jpg"), "png");
    }

    #[test]
    fn headers() {
        let h = placeable_header([0, 0, 100, 200]);
        assert_eq!(h.len(), 22);
        assert_eq!(&h[..4], &[0xD7, 0xCD, 0xC6, 0x9A]);
        let mut info = vec![0u8; 40];
        info[..4].copy_from_slice(&40u32.to_le_bytes());
        info[14..16].copy_from_slice(&8u16.to_le_bytes());
        let f = bmp_file_header(&info, 1000);
        assert_eq!(&f[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes([f[10], f[11], f[12], f[13]]),
            14 + 40 + 1024
        );
    }

    struct Nul;
    impl crate::Sink for Nul {
        fn begin(&mut self, _: &Entry) -> std::io::Result<bool> {
            Ok(true)
        }
        fn write(&mut self, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn end(&mut self, _: &MemberEnd) -> std::io::Result<()> {
            Ok(())
        }
    }
}
