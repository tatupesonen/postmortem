//! Archive and compressor sniffing of Apache Commons Compress 1.27.1
//! (`ArchiveStreamFactory.detect`, `CompressorStreamFactory.detect`) as
//! Tika's `DefaultZipContainerDetector` uses it on the first 1 KiB.

/// What `ArchiveStreamFactory.detect` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Archive {
    Zip,
    Ar,
    Cpio,
    Arj,
    SevenZ,
    Dump,
    Tar,
}

fn lead(p: &[u8], n: usize) -> &[u8] {
    p.get(..n.min(p.len())).unwrap_or_default()
}

fn zip_matches(sig: &[u8]) -> bool {
    if sig.len() < 4 {
        return false;
    }
    let s = lead(sig, 4);
    s == b"PK\x03\x04" || s == b"PK\x05\x06" || s == b"PK\x07\x08" || s == b"PK00"
}

/// The archive format of `prefix` (Commons Compress' detection order).
pub(crate) fn archive(prefix: &[u8]) -> Option<Archive> {
    let sig = lead(prefix, 12);
    let n = sig.len();
    let at = |i: usize| sig.get(i).copied().unwrap_or(0);
    if zip_matches(sig) {
        return Some(Archive::Zip);
    }
    if n >= 8 && sig.starts_with(b"!<arch>\n") {
        return Some(Archive::Ar);
    }
    if n >= 6 {
        let cpio = (at(0) == 0x71 && at(1) == 0xC7)
            || (at(1) == 0x71 && at(0) == 0xC7)
            || (sig.starts_with(b"07070") && matches!(at(5), b'1' | b'2' | b'7'));
        if cpio {
            return Some(Archive::Cpio);
        }
    }
    if n >= 2 && at(0) == 0x60 && at(1) == 0xEA {
        return Some(Archive::Arj);
    }
    if n >= 6 && sig.starts_with(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(Archive::SevenZ);
    }
    let dump = lead(prefix, 32);
    if dump.len() >= 32 {
        let magic = dump
            .get(24..28)
            .and_then(|b| b.try_into().ok())
            .map_or(0, i32::from_le_bytes);
        if magic == 60012 {
            return Some(Archive::Dump);
        }
    }
    let tar = lead(prefix, 512);
    if tar_matches(tar) {
        return Some(Archive::Tar);
    }
    if tar.len() >= 512 && tar_header_ok(tar) {
        return Some(Archive::Tar);
    }
    None
}

fn tar_matches(sig: &[u8]) -> bool {
    if sig.len() < 263 + 2 {
        return false;
    }
    let magic = sig.get(257..263).unwrap_or_default();
    let version = sig.get(263..265).unwrap_or_default();
    (magic == b"ustar\0" && version == b"00")
        || (magic == b"ustar " && (version == b" \0" || version == b"0\0"))
        || (magic == b"ustar\0" && version == b"\0\0")
}

/// `TarUtils.parseOctal`; `None` where Commons Compress throws.
fn parse_octal(b: &[u8]) -> Option<i64> {
    if b.len() < 2 {
        return None;
    }
    if b.first() == Some(&0) {
        return Some(0);
    }
    let mut start = 0;
    let mut end = b.len();
    while start < end && b.get(start) == Some(&b' ') {
        start += 1;
    }
    while start < end && matches!(b.get(end - 1), Some(0 | b' ')) {
        end -= 1;
    }
    let mut v: i64 = 0;
    for &c in b.get(start..end)? {
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        v = v.wrapping_shl(3).wrapping_add(i64::from(c - b'0'));
    }
    Some(v)
}

/// `parseOctalOrBinary`.
fn parse_num(b: &[u8]) -> Option<i64> {
    let first = *b.first()?;
    if first & 0x80 == 0 {
        return parse_octal(b);
    }
    // Base-256: big-endian value of the remaining bytes (sign from 0xFF).
    let neg = first == 0xFF;
    let mut v: i64 = if neg { -1 } else { 0 };
    for (i, &x) in b.iter().enumerate() {
        let x = if i == 0 { x & 0x7F } else { x };
        v = v.wrapping_shl(8) | i64::from(x);
    }
    Some(v)
}

/// COMPRESS-117: a first header block with a valid checksum that is a
/// directory or a non-empty file.
fn tar_header_ok(h: &[u8]) -> bool {
    let field = |o: usize, n: usize| h.get(o..o + n).unwrap_or_default();
    // Fields parsed by `TarArchiveEntry.parseTarHeader` before and after
    // the checksum; any parse error rejects.
    if parse_num(field(100, 8)).is_none()
        || parse_num(field(108, 8)).is_none()
        || parse_num(field(116, 8)).is_none()
        || parse_num(field(136, 12)).is_none()
    {
        return false;
    }
    let Some(size) = parse_num(field(124, 12)) else {
        return false;
    };
    if size < 0 {
        return false;
    }
    let Some(stored) = parse_octal(field(148, 8)) else {
        return false;
    };
    let (mut unsigned, mut signed) = (0i64, 0i64);
    for (i, &b) in h.iter().enumerate().take(512) {
        let b = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += i64::from(b);
        signed += i64::from(b as i8);
    }
    if stored != unsigned && stored != signed {
        return false;
    }
    let flag = h.get(156).copied().unwrap_or(0);
    // Long names, PAX headers and sparse files need more blocks.
    if matches!(flag, b'L' | b'K' | b'x' | b'X' | b'g' | b'S') {
        return false;
    }
    if matches!(flag, b'3' | b'4')
        && (parse_num(field(329, 8)).is_none() || parse_num(field(337, 8)).is_none())
    {
        return false;
    }
    let name = field(0, 100);
    let name_end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let is_dir = flag == b'5' || name.get(..name_end).is_some_and(|n| n.ends_with(b"/"));
    is_dir || size > 0
}

/// `CompressorStreamFactory.detect` on the first 12 bytes; the media type
/// Tika's `CompressorConstants` maps it to.
pub(crate) fn compressor(prefix: &[u8]) -> Option<&'static str> {
    let s = lead(prefix, 12);
    let n = s.len();
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    if n >= 3 && s.starts_with(b"BZh") {
        return Some("application/x-bzip2");
    }
    if n >= 2 && at(0) == 0x1F && at(1) == 0x8B {
        return Some("application/gzip");
    }
    if n >= 4 && s.starts_with(&[0xCA, 0xFE, 0xD0, 0x0D]) {
        return Some("application/x-java-pack200");
    }
    if n >= 10 && s.starts_with(&[0xFF, 0x06, 0x00, 0x00, 0x73, 0x4E, 0x61, 0x50, 0x70, 0x59]) {
        return Some("application/x-snappy");
    }
    if n > 3 && at(0) == 0x1F && at(1) == 0x9D {
        return Some("application/x-compress");
    }
    if n > 3 && at(0) == 0x78 && matches!(at(1), 0x01 | 0x5E | 0x9C | 0xDA) {
        return Some("application/zlib");
    }
    if n >= 6 && s.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some("application/x-xz");
    }
    if n >= 3 && s.starts_with(&[0x5D, 0, 0]) {
        return Some("application/x-lzma");
    }
    if n >= 4 && s.starts_with(&[0x04, 0x22, 0x4D, 0x18]) {
        return Some("application/x-lz4");
    }
    if n >= 4 {
        if s.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
            return Some("application/zstd");
        }
        if at(0) & 0xF0 == 0x50 && s.get(1..4) == Some(&[0x2A, 0x4D, 0x18][..]) {
            return Some("application/zstd");
        }
    }
    None
}

/// Tika's `PackageConstants.getMediaType` for non-zip archives.
pub(crate) fn archive_media_type(a: Archive) -> &'static str {
    match a {
        Archive::Zip => "application/zip",
        Archive::Ar => "application/x-archive",
        Archive::Cpio => "application/x-cpio",
        Archive::Arj => "application/x-arj",
        Archive::SevenZ => "application/x-7z-compressed",
        Archive::Dump => "application/x-tika-unix-dump",
        Archive::Tar => "application/x-tar",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v7_tar(name: &str, size: u64, flag: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = flag;
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        h
    }

    #[test]
    fn archives() {
        assert_eq!(archive(b"PK\x03\x04rest"), Some(Archive::Zip));
        assert_eq!(archive(b"!<arch>\nfoo"), Some(Archive::Ar));
        assert_eq!(archive(b"070701xxxx"), Some(Archive::Cpio));
        assert_eq!(archive(&[0xC7, 0x71, 0, 0, 0, 0]), Some(Archive::Cpio));
        assert_eq!(
            archive(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C, 0, 4]),
            Some(Archive::SevenZ)
        );
        assert_eq!(archive(&v7_tar("a.txt", 5, b'0')), Some(Archive::Tar));
        assert_eq!(archive(&v7_tar("d/", 0, b'5')), Some(Archive::Tar));
        // Empty regular file: not enough evidence.
        assert_eq!(archive(&v7_tar("a", 0, b'0')), None);
        let mut bad = v7_tar("a.txt", 5, b'0');
        bad[148] = b'9';
        assert_eq!(archive(&bad), None);
        assert_eq!(archive(b"hello world"), None);
    }

    #[test]
    fn compressors() {
        assert_eq!(compressor(b"BZh91AY&SY"), Some("application/x-bzip2"));
        assert_eq!(compressor(&[0x1F, 0x8B, 8, 0]), Some("application/gzip"));
        assert_eq!(compressor(&[0x78, 0x9C, 1, 2]), Some("application/zlib"));
        assert_eq!(compressor(&[0x78, 0x9C, 1]), None);
        assert_eq!(compressor(&[0x5D, 0, 0, 0x80]), Some("application/x-lzma"));
        assert_eq!(
            compressor(&[0x50, 0x2A, 0x4D, 0x18]),
            Some("application/zstd")
        );
        assert_eq!(compressor(b"plain"), None);
    }
}
