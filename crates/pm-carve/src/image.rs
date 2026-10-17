//! Picture formats: JPEG, PNG, GIF, BMP, TIFF.

use crate::Verdict;
use crate::window::Window;

/// JPEG: the marker segments are walked to the first scan, the
/// entropy-coded data is searched for the next marker (stuffed `FF 00`
/// and restart markers skipped), until End Of Image. Embedded thumbnails
/// sit inside `APP1` segments and are skipped by length.
pub(crate) fn jpeg(w: &mut Window<'_>, start: u64) -> Verdict {
    let mut p = start + 2;
    let mut segments = 0u32;
    let mut scanned = false;
    loop {
        // Fill bytes may precede a marker.
        let Some(mut b) = w.u8(p) else {
            return truncated(scanned, p - start, "jpg");
        };
        if b != 0xFF {
            return if scanned || segments > 2 {
                Verdict::Truncated {
                    len: p - start,
                    ext: "jpg",
                }
            } else {
                Verdict::No
            };
        }
        while b == 0xFF {
            p += 1;
            let Some(n) = w.u8(p) else {
                return truncated(scanned, p - start, "jpg");
            };
            b = n;
        }
        let marker = b;
        p += 1;
        if segments == 0 && !first_marker_ok(marker) {
            return Verdict::No;
        }
        segments += 1;
        match marker {
            0xD9 => {
                return Verdict::Found {
                    len: p - start,
                    ext: "jpg",
                };
            }
            0x01 | 0xD0..=0xD7 => {}
            0x00 => return truncated(scanned, p - start, "jpg"),
            _ => {
                let Some(len) = w.u16be(p) else {
                    return truncated(scanned, p - start, "jpg");
                };
                if len < 2 {
                    return truncated(scanned, p - start, "jpg");
                }
                p += u64::from(len);
                if marker == 0xDA {
                    scanned = true;
                    match next_marker(w, p) {
                        Some(m) => p = m,
                        None => return truncated(true, w.end() - start, "jpg"),
                    }
                }
            }
        }
    }
}

/// Markers that may follow Start Of Image.
fn first_marker_ok(m: u8) -> bool {
    matches!(m, 0xC0..=0xCF | 0xDB | 0xDD | 0xE0..=0xEF | 0xFE)
}

/// The next real marker in entropy-coded data at or after `from`.
fn next_marker(w: &mut Window<'_>, from: u64) -> Option<u64> {
    let mut p = from;
    loop {
        let ff = w.find_byte(p, 0xFF)?;
        let n = w.u8(ff + 1)?;
        match n {
            0x00 | 0xD0..=0xD7 | 0xFF => p = ff + 1,
            _ => return Some(ff),
        }
    }
}

fn truncated(scanned: bool, len: u64, ext: &'static str) -> Verdict {
    if scanned {
        Verdict::Truncated { len, ext }
    } else {
        Verdict::No
    }
}

/// PNG: chunk walk to `IEND`; the first chunk must be a 13-byte `IHDR`.
pub(crate) fn png(w: &mut Window<'_>, start: u64) -> Verdict {
    let mut p = start + 8;
    let mut first = true;
    loop {
        let (Some(len), Some(ty)) = (w.u32be(p), w.array::<4>(p + 4)) else {
            return truncated(!first, p - start, "png");
        };
        if !ty.iter().all(u8::is_ascii_alphabetic) || (first && (&ty != b"IHDR" || len != 13)) {
            return truncated(!first, p - start, "png");
        }
        first = false;
        p += 12 + u64::from(len);
        if &ty == b"IEND" {
            return Verdict::Found {
                len: p - start,
                ext: "png",
            };
        }
        if p > w.end() {
            return Verdict::Truncated {
                len: w.end() - start,
                ext: "png",
            };
        }
    }
}

/// GIF: header, colour tables, then extension and image blocks (their
/// data sub-blocks) up to the trailer.
pub(crate) fn gif(w: &mut Window<'_>, start: u64) -> Verdict {
    let Some(flags) = w.u8(start + 10) else {
        return Verdict::No;
    };
    let table = |f: u8| {
        if f & 0x80 != 0 {
            3u64 << ((f & 7) + 1)
        } else {
            0
        }
    };
    let mut p = start + 13 + table(flags);
    let mut images = 0u32;
    loop {
        let Some(b) = w.u8(p) else {
            return truncated(images > 0, p - start, "gif");
        };
        match b {
            0x3B => {
                return Verdict::Found {
                    len: p + 1 - start,
                    ext: "gif",
                };
            }
            0x2C => {
                let Some(f) = w.u8(p + 9) else {
                    return truncated(images > 0, p - start, "gif");
                };
                p += 10 + table(f);
                match w.u8(p) {
                    Some(1..=12) => p += 1,
                    _ => return truncated(images > 0, p - start, "gif"),
                }
                images += 1;
            }
            0x21 => p += 2,
            _ => return truncated(images > 0, p - start, "gif"),
        }
        // Data sub-blocks.
        loop {
            let Some(n) = w.u8(p) else {
                return truncated(images > 0, p - start, "gif");
            };
            p += 1 + u64::from(n);
            if n == 0 {
                break;
            }
        }
    }
}

/// BMP: the file size in the header, checked against the other fields.
pub(crate) fn bmp(w: &mut Window<'_>, start: u64) -> Verdict {
    let (Some(size), Some(reserved), Some(offset), Some(dib)) = (
        w.u32le(start + 2),
        w.u32le(start + 6),
        w.u32le(start + 10),
        w.u32le(start + 14),
    ) else {
        return Verdict::No;
    };
    let (size, offset) = (u64::from(size), u64::from(offset));
    let dib_ok = matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124);
    let (width, height) = if dib == 12 {
        (
            w.u16le(start + 18).map(u32::from),
            w.u16le(start + 20).map(u32::from),
        )
    } else {
        (w.u32le(start + 18), w.u32le(start + 22))
    };
    let planes_off = if dib == 12 { 22 } else { 26 };
    let planes = w.u16le(start + planes_off);
    if reserved != 0
        || !dib_ok
        || offset < 14 + u64::from(dib)
        || size <= offset
        || width.unwrap_or(0) == 0
        || height.unwrap_or(0) == 0
        || planes != Some(1)
    {
        return Verdict::No;
    }
    if start + size > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext: "bmp",
        };
    }
    Verdict::Found {
        len: size,
        ext: "bmp",
    }
}

/// TIFF (and TIFF-based raw formats): the extent is the end of the
/// furthest structure the IFD chain points at (IFDs, out-of-line values,
/// strips and tiles, EXIF and sub-IFDs).
pub(crate) fn tiff(w: &mut Window<'_>, start: u64) -> Verdict {
    let Some(order) = w.array::<2>(start) else {
        return Verdict::No;
    };
    let le = &order == b"II";
    let rd16 = |w: &mut Window<'_>, p: u64| if le { w.u16le(p) } else { w.u16be(p) };
    let rd32 = |w: &mut Window<'_>, p: u64| if le { w.u32le(p) } else { w.u32be(p) };
    let Some(first) = rd32(w, start + 4) else {
        return Verdict::No;
    };
    let mut end = 8u64;
    let mut queue = vec![u64::from(first)];
    let mut seen = 0u32;
    let mut strips_found = false;
    while let Some(ifd) = queue.pop() {
        seen += 1;
        if ifd < 8 || seen > 64 {
            break;
        }
        let Some(count) = rd16(w, start + ifd) else {
            return Verdict::No;
        };
        if count == 0 || count > 1000 {
            return Verdict::No;
        }
        let entries_end = ifd + 2 + 12 * u64::from(count) + 4;
        end = end.max(entries_end);
        let mut offsets: Vec<u64> = Vec::new();
        let mut counts: Vec<u64> = Vec::new();
        for i in 0..u64::from(count) {
            let e = start + ifd + 2 + 12 * i;
            let (Some(tag), Some(ty), Some(n)) = (rd16(w, e), rd16(w, e + 2), rd32(w, e + 4))
            else {
                return Verdict::No;
            };
            let unit: u64 = match ty {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 | 13 => 4,
                5 | 10 | 12 => 8,
                _ => return Verdict::No,
            };
            let bytes = unit.saturating_mul(u64::from(n));
            let value_at = if bytes > 4 {
                let Some(off) = rd32(w, e + 8) else {
                    return Verdict::No;
                };
                end = end.max(u64::from(off).saturating_add(bytes));
                start + u64::from(off)
            } else {
                e + 8
            };
            let read_values = |w: &mut Window<'_>| -> Vec<u64> {
                (0..u64::from(n).min(65_536))
                    .filter_map(|k| match unit {
                        2 => rd16(w, value_at + 2 * k).map(u64::from),
                        4 => rd32(w, value_at + 4 * k).map(u64::from),
                        _ => None,
                    })
                    .collect()
            };
            match tag {
                273 | 324 => offsets = read_values(w),
                279 | 325 => counts = read_values(w),
                330 | 34665 | 34853 => {
                    queue.extend(read_values(w));
                }
                _ => {}
            }
        }
        for (o, c) in offsets.iter().zip(&counts) {
            strips_found = true;
            end = end.max(o.saturating_add(*c));
        }
        if let Some(next) = rd32(w, start + entries_end - 4)
            && next != 0
        {
            queue.push(u64::from(next));
        }
    }
    if !strips_found {
        return Verdict::No;
    }
    if start + end > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext: "tif",
        };
    }
    Verdict::Found {
        len: end,
        ext: "tif",
    }
}
