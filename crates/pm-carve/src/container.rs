//! Documents and containers: PDF, zip (and Office Open XML, `OpenDocument`,
//! Java and Android packages), OLE compound files, 7z, gzip.

use std::io::Read;

use pm_core::ReadAt;

use crate::Verdict;
use crate::window::Window;

/// PDF: up to the `%%EOF` that is not followed by an incremental update.
pub(crate) fn pdf(w: &mut Window<'_>, start: u64) -> Verdict {
    match w.get(start + 5, 3) {
        Some([b'1' | b'2', b'.', d]) if d.is_ascii_digit() => {}
        _ => return Verdict::No,
    }
    let mut from = start + 8;
    while let Some(at) = w.find(from, b"%%EOF") {
        let mut end = at + 5;
        // The end-of-line after the marker belongs to the file.
        for _ in 0..2 {
            match w.u8(end) {
                Some(b'\r' | b'\n') => end += 1,
                _ => break,
            }
        }
        if !continues(w, end) {
            return Verdict::Found {
                len: end - start,
                ext: "pdf",
            };
        }
        from = end;
    }
    Verdict::No
}

/// Whether an incremental update follows the end of a PDF revision: an
/// object (`12 0 obj`), a cross-reference section or stream, or a
/// comment, after optional white space.
fn continues(w: &mut Window<'_>, at: u64) -> bool {
    let mut p = at;
    for _ in 0..64 {
        match w.u8(p) {
            Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0C) => p += 1,
            _ => break,
        }
    }
    let avail = usize::try_from(w.end().saturating_sub(p))
        .unwrap_or(24)
        .min(24);
    let Some(head) = w.get(p, avail).map(<[u8]>::to_vec) else {
        return false;
    };
    if head.starts_with(b"xref") || (head.starts_with(b"%") && !head.starts_with(b"%PDF")) {
        return true;
    }
    // `<num> <num> obj`
    let s = String::from_utf8_lossy(&head);
    let mut it = s.split_ascii_whitespace();
    matches!(
        (it.next(), it.next(), it.next()),
        (Some(a), Some(b), Some(o))
            if a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
                && o.starts_with("obj")
    )
}

/// Zip: the end-of-central-directory record whose directory offset and
/// size point back at this archive's start (records of stored inner zips
/// do not). Without one, the local headers are walked as far as they go.
pub(crate) fn zip(w: &mut Window<'_>, src: &dyn ReadAt, start: u64) -> Verdict {
    // A plausible first local header.
    let (Some(ver), Some(method), Some(name_len)) =
        (w.u16le(start + 4), w.u16le(start + 8), w.u16le(start + 26))
    else {
        return Verdict::No;
    };
    if ver > 100 || name_len == 0 || name_len > 1024 || !matches!(method, 0..=20 | 93..=99) {
        return Verdict::No;
    }
    let mut from = start + 30;
    while let Some(at) = w.find(from, b"PK\x05\x06") {
        from = at + 1;
        let (Some(cd_size), Some(cd_off), Some(comment)) =
            (w.u32le(at + 12), w.u32le(at + 16), w.u16le(at + 20))
        else {
            break;
        };
        let (mut cd_size, mut cd_off) = (u64::from(cd_size), u64::from(cd_off));
        let mut record = at;
        if (cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF)
            && at >= 20
            && w.get(at - 20, 4) == Some(b"PK\x06\x07")
            && let Some(rec64) = w.u64le(at - 20 + 8)
            && w.get(start + rec64, 4) == Some(b"PK\x06\x06")
            && let (Some(s), Some(o)) = (w.u64le(start + rec64 + 40), w.u64le(start + rec64 + 48))
        {
            cd_size = s;
            cd_off = o;
            record = start + rec64;
        }
        if start
            .checked_add(cd_off)
            .and_then(|x| x.checked_add(cd_size))
            == Some(record)
            && (cd_size == 0 || w.get(start + cd_off, 4) == Some(b"PK\x01\x02"))
        {
            let len = at + 22 + u64::from(comment) - start;
            if start + len > w.end() {
                return Verdict::Truncated {
                    len: w.end() - start,
                    ext: "zip",
                };
            }
            return Verdict::Found {
                len,
                ext: zip_kind(src, start, len),
            };
        }
    }
    // No directory: walk the local headers.
    let mut p = start;
    let mut entries = 0u32;
    while w.get(p, 4) == Some(b"PK\x03\x04") {
        let (Some(flags), Some(csize), Some(n), Some(m)) = (
            w.u16le(p + 6),
            w.u32le(p + 18),
            w.u16le(p + 26),
            w.u16le(p + 28),
        ) else {
            break;
        };
        if flags & 8 != 0 && csize == 0 {
            break;
        }
        p += 30 + u64::from(n) + u64::from(m) + u64::from(csize);
        entries += 1;
    }
    if entries == 0 || p > w.end() {
        return Verdict::No;
    }
    Verdict::Truncated {
        len: p - start,
        ext: "zip",
    }
}

/// The extension of a carved zip: Office Open XML by its parts,
/// `OpenDocument` by its `mimetype` member, Java and Android packages by
/// their manifests.
fn zip_kind(src: &dyn ReadAt, start: u64, len: u64) -> &'static str {
    let slice = Slice { src, start, len };
    match pm_archive::detect(&slice) {
        Some(pm_archive::Format::Docx) => return "docx",
        Some(pm_archive::Format::Xlsx) => return "xlsx",
        Some(pm_archive::Format::Pptx) => return "pptx",
        _ => {}
    }
    let mut w = Window::new(&slice, len);
    let n = w.u16le(26).unwrap_or(0);
    let name = w
        .get(30, usize::from(n))
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if name == b"mimetype" {
        let m = w.u16le(28).unwrap_or(0);
        let data = w
            .get(30 + u64::from(n) + u64::from(m), 46)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let s = String::from_utf8_lossy(&data);
        for (key, ext) in [
            (".text", "odt"),
            (".spreadsheet", "ods"),
            (".presentation", "odp"),
            (".graphics", "odg"),
        ] {
            if s.contains(key) {
                return ext;
            }
        }
    }
    if w.find(0, b"AndroidManifest.xml").is_some() && w.find(0, b"classes.dex").is_some() {
        return "apk";
    }
    if name == b"META-INF/" || name == b"META-INF/MANIFEST.MF" {
        return "jar";
    }
    "zip"
}

/// A byte range of a source.
pub(crate) struct Slice<'a> {
    pub(crate) src: &'a dyn ReadAt,
    pub(crate) start: u64,
    pub(crate) len: u64,
}

impl std::fmt::Debug for Slice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slice")
            .field("start", &self.start)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl ReadAt for Slice<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.len.saturating_sub(offset);
        let n = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        if n == 0 {
            return Ok(0);
        }
        self.src
            .read_at(self.start + offset, buf.get_mut(..n).unwrap_or_default())
    }

    fn size(&self) -> u64 {
        self.len
    }
}

/// OLE compound file: one sector past the highest sector in use according
/// to the FAT (plus the header sector).
pub(crate) fn ole(w: &mut Window<'_>, src: &dyn ReadAt, start: u64) -> Verdict {
    let (Some(shift), Some(n_fat), Some(difat_first), Some(n_difat)) = (
        w.u16le(start + 0x1E),
        w.u32le(start + 0x2C),
        w.u32le(start + 0x44),
        w.u32le(start + 0x48),
    ) else {
        return Verdict::No;
    };
    if !matches!(shift, 9 | 12)
        || n_fat == 0
        || n_fat > 1 << 20
        || w.u16le(start + 0x1C) != Some(0xFFFE)
    {
        return Verdict::No;
    }
    let ss = 1u64 << shift;
    let sector = |n: u32| start + (u64::from(n) + 1) * ss;
    // FAT sector numbers: 109 in the header, the rest in the DIFAT chain.
    let mut fat_sectors: Vec<u32> = Vec::new();
    for i in 0..109u64 {
        match w.u32le(start + 0x4C + 4 * i) {
            Some(s) if s < 0xFFFF_FFFA => fat_sectors.push(s),
            _ => break,
        }
    }
    let per = (ss / 4) as u32;
    let mut d = difat_first;
    let mut max_used = 0u64;
    for _ in 0..n_difat.min(1 << 16) {
        if d >= 0xFFFF_FFFA {
            break;
        }
        max_used = max_used.max(u64::from(d));
        for i in 0..per - 1 {
            match w.u32le(sector(d) + 4 * u64::from(i)) {
                Some(s) if s < 0xFFFF_FFFA => fat_sectors.push(s),
                _ => {}
            }
        }
        d = w
            .u32le(sector(d) + 4 * u64::from(per - 1))
            .unwrap_or(0xFFFF_FFFF);
    }
    fat_sectors.truncate(n_fat as usize);
    if fat_sectors.is_empty() {
        return Verdict::No;
    }
    for (k, &fs) in fat_sectors.iter().enumerate() {
        max_used = max_used.max(u64::from(fs));
        // The highest entry of this FAT sector that is not free.
        let base = sector(fs);
        let Some(page) = w.get(base, ss as usize).map(<[u8]>::to_vec) else {
            return Verdict::No;
        };
        if let Some(i) = page
            .chunks_exact(4)
            .rposition(|e| e != [0xFF, 0xFF, 0xFF, 0xFF])
        {
            max_used = max_used.max(k as u64 * u64::from(per) + i as u64);
        }
    }
    let len = (max_used + 2) * ss;
    if start + len > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext: "doc",
        };
    }
    let slice = Slice { src, start, len };
    let ext = match pm_archive::detect(&slice) {
        Some(pm_archive::Format::Xls) => "xls",
        Some(pm_archive::Format::Ppt) => "ppt",
        _ => "doc",
    };
    Verdict::Found { len, ext }
}

/// 7z: the start header names the end of the archive (its CRC must
/// match).
pub(crate) fn sevenz(w: &mut Window<'_>, start: u64) -> Verdict {
    let (Some(crc), Some(hdr)) = (
        w.u32le(start + 8),
        w.get(start + 12, 20).map(<[u8]>::to_vec),
    ) else {
        return Verdict::No;
    };
    if crc32fast::hash(&hdr) != crc {
        return Verdict::No;
    }
    let (Some(off), Some(size)) = (w.u64le(start + 12), w.u64le(start + 20)) else {
        return Verdict::No;
    };
    let Some(len) = off.checked_add(size).and_then(|x| x.checked_add(32)) else {
        return Verdict::No;
    };
    if start.saturating_add(len) > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext: "7z",
        };
    }
    Verdict::Found { len, ext: "7z" }
}

/// Most bytes decoded to find the end of one gzip stream.
const MAX_GZIP_DECODED: u64 = 1 << 31;

/// gzip: the deflate stream is decoded to find its end; the trailer's
/// CRC-32 and size must match.
pub(crate) fn gzip(w: &mut Window<'_>, src: &dyn ReadAt, start: u64) -> Verdict {
    let Some(flags) = w.u8(start + 3) else {
        return Verdict::No;
    };
    if flags & 0xE0 != 0 {
        return Verdict::No;
    }
    let mut p = start + 10;
    if flags & 4 != 0 {
        let Some(x) = w.u16le(p) else {
            return Verdict::No;
        };
        p += 2 + u64::from(x);
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            match w.find_byte(p, 0) {
                Some(z) if z - p < 1024 => p = z + 1,
                _ => return Verdict::No,
            }
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    let slice = Slice {
        src,
        start: p,
        len: w.end().saturating_sub(p),
    };
    let input =
        std::io::BufReader::with_capacity(64 * 1024, pm_core::io::Cursor::new(SliceRef(&slice)));
    let mut dec = flate2::bufread::DeflateDecoder::new(input);
    let mut crc = crc32fast::Hasher::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match dec.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                crc.update(buf.get(..n).unwrap_or_default());
                total += n as u64;
                if total > MAX_GZIP_DECODED {
                    return Verdict::No;
                }
            }
            Err(_) => return Verdict::No,
        }
    }
    let end = p + dec.total_in();
    let (Some(c), Some(isize)) = (w.u32le(end), w.u32le(end + 4)) else {
        return Verdict::No;
    };
    if c != crc.finalize() || isize != total as u32 {
        return Verdict::No;
    }
    Verdict::Found {
        len: end + 8 - start,
        ext: "gz",
    }
}

/// A sized wrapper so a borrowed slice works with generic adapters.
#[derive(Debug, Clone, Copy)]
struct SliceRef<'a>(&'a Slice<'a>);

impl ReadAt for SliceRef<'_> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_at(offset, buf)
    }

    fn size(&self) -> u64 {
        self.0.size()
    }
}
