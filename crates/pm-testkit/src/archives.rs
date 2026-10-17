//! Archive and document builders for embedded-file tests: zip (stored,
//! deflate, `ZipCrypto`, fake AES, lying sizes, data descriptors, no
//! central directory, prepended data), tar (ustar, GNU long names, pax),
//! gzip / bzip2 / xz / zstd, 7z (`sevenz-rust2` writer, AES), RAR
//! (`rars` writer), ISO 9660 (`fstool`), Office Open XML, legacy Office
//! with BLIP pictures, PDF with attachments and images, and small valid
//! images.

use std::io::{Cursor, Read, Write};
use std::path::Path;

/// Fixed DOS timestamp of zip entries: 2021-03-04 10:20:30.
pub const DOS_DATE: u16 = ((2021 - 1980) << 9) | (3 << 5) | 4;
pub const DOS_TIME: u16 = (10 << 11) | (20 << 5) | (30 / 2);
/// The same moment as Unix seconds (UTC).
pub const MTIME: i64 = 1_614_853_230;

// ---------------------------------------------------------------- zip ---

/// Compression of a zip entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Stored,
    Deflate,
    Bzip2,
    Zstd,
    Xz,
}

/// One zip member.
#[derive(Debug, Clone)]
pub struct ZipEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub method: Method,
    pub dir: bool,
    /// Set the UTF-8 name flag.
    pub utf8: bool,
    /// Encrypt with `ZipCrypto`.
    pub password: Option<Vec<u8>>,
    /// Mark as WinZip-AES encrypted (method 99; data is random, not real
    /// AES — enough for detection).
    pub fake_aes: bool,
    /// Uncompressed size written to the headers instead of the real one.
    pub declared_size: Option<u32>,
    /// Sizes and CRC in a data descriptor after the data (flag bit 3).
    pub descriptor: bool,
    /// Info-ZIP extended timestamp (UT) with this mtime.
    pub unix_mtime: Option<i32>,
}

impl ZipEntry {
    pub fn new(name: impl AsRef<[u8]>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.as_ref().to_vec(),
            data: data.into(),
            method: Method::Deflate,
            dir: false,
            utf8: false,
            password: None,
            fake_aes: false,
            declared_size: None,
            descriptor: false,
            unix_mtime: None,
        }
    }

    pub fn dir(name: &str) -> Self {
        let mut n = name.to_owned();
        if !n.ends_with('/') {
            n.push('/');
        }
        Self {
            dir: true,
            method: Method::Stored,
            ..Self::new(n, Vec::new())
        }
    }

    pub fn method(mut self, m: Method) -> Self {
        self.method = m;
        self
    }
    pub fn password(mut self, p: &str) -> Self {
        self.password = Some(p.as_bytes().to_vec());
        self
    }
    pub fn fake_aes(mut self) -> Self {
        self.fake_aes = true;
        self
    }
    pub fn declared_size(mut self, s: u32) -> Self {
        self.declared_size = Some(s);
        self
    }
    pub fn descriptor(mut self) -> Self {
        self.descriptor = true;
        self
    }
    pub fn utf8(mut self) -> Self {
        self.utf8 = true;
        self
    }
    pub fn unix_mtime(mut self, t: i32) -> Self {
        self.unix_mtime = Some(t);
        self
    }
}

/// Builds zip files.
#[derive(Debug, Clone, Default)]
pub struct ZipBuilder {
    pub entries: Vec<ZipEntry>,
    /// Bytes before the first local header (SFX stub).
    pub prefix: Vec<u8>,
    /// Leave out the central directory and end record.
    pub omit_central: bool,
}

/// CRC-32 (IEEE), computed independently of the reader under test.
pub fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

fn crc_byte(crc: u32, b: u8) -> u32 {
    let mut c = (crc ^ u32::from(b)) & 0xFF;
    for _ in 0..8 {
        c = if c & 1 != 0 {
            0xEDB8_8320 ^ (c >> 1)
        } else {
            c >> 1
        };
    }
    c ^ (crc >> 8)
}

/// Traditional PKWARE (`ZipCrypto`) encryption of `header ++ data`.
pub fn zipcrypto(password: &[u8], header: [u8; 12], data: &[u8]) -> Vec<u8> {
    let mut k = [0x1234_5678u32, 0x2345_6789, 0x3456_7890];
    let update = |k: &mut [u32; 3], b: u8| {
        k[0] = crc_byte(k[0], b);
        k[1] = k[1]
            .wrapping_add(k[0] & 0xFF)
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        k[2] = crc_byte(k[2], (k[1] >> 24) as u8);
    };
    for &b in password {
        update(&mut k, b);
    }
    header
        .iter()
        .chain(data)
        .map(|&p| {
            let t = (k[2] | 2) & 0xFFFF;
            let c = p ^ ((t.wrapping_mul(t ^ 1) >> 8) as u8);
            update(&mut k, p);
            c
        })
        .collect()
}

/// Raw deflate.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

impl ZipBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, e: ZipEntry) -> &mut Self {
        self.entries.push(e);
        self
    }

    /// A deflated file.
    pub fn file(&mut self, name: &str, data: &[u8]) -> &mut Self {
        self.add(ZipEntry::new(name, data))
    }

    pub fn dir(&mut self, name: &str) -> &mut Self {
        self.add(ZipEntry::dir(name))
    }

    pub fn build(&self) -> Vec<u8> {
        let mut out = self.prefix.clone();
        let mut central = Vec::new();
        for e in &self.entries {
            let crc = crc32(&e.data);
            let (method, mut payload) = match e.method {
                Method::Stored => (0u16, e.data.clone()),
                Method::Deflate => (8, deflate(&e.data)),
                Method::Bzip2 => (12, bzip2_bytes(&e.data)),
                Method::Zstd => (93, zstd_bytes(&e.data)),
                Method::Xz => (95, xz_bytes(&e.data)),
            };
            let mut flags: u16 = if e.utf8 { 1 << 11 } else { 0 };
            let mut method = method;
            let mut extra = Vec::new();
            if let Some(t) = e.unix_mtime {
                extra.extend([0x55, 0x54, 5, 0, 1]);
                extra.extend(t.to_le_bytes());
            }
            if let Some(pw) = &e.password {
                flags |= 1;
                let mut hdr = [0x5Au8; 12];
                hdr[11] = if e.descriptor {
                    (DOS_TIME >> 8) as u8
                } else {
                    (crc >> 24) as u8
                };
                payload = zipcrypto(pw, hdr, &payload);
            }
            if e.fake_aes {
                flags |= 1;
                extra.extend([0x01, 0x99, 7, 0, 2, 0, b'A', b'E', 3]);
                extra.extend(method.to_le_bytes());
                method = 99;
                payload = (0..payload.len() + 28)
                    .map(|i| (i * 31 % 251) as u8)
                    .collect();
            }
            if e.descriptor {
                flags |= 1 << 3;
            }
            let usize = e.declared_size.unwrap_or(e.data.len() as u32);
            let csize = payload.len() as u32;
            // Offsets are relative to the archive start, as SFX tools
            // write them before the stub is prepended.
            let offset = (out.len() - self.prefix.len()) as u32;
            let (l_crc, l_c, l_u) = if e.descriptor {
                (0, 0, 0)
            } else {
                (crc, csize, usize)
            };
            out.extend(b"PK\x03\x04");
            out.extend(20u16.to_le_bytes());
            out.extend(flags.to_le_bytes());
            out.extend(method.to_le_bytes());
            out.extend(DOS_TIME.to_le_bytes());
            out.extend(DOS_DATE.to_le_bytes());
            out.extend(l_crc.to_le_bytes());
            out.extend(l_c.to_le_bytes());
            out.extend(l_u.to_le_bytes());
            out.extend((e.name.len() as u16).to_le_bytes());
            out.extend((extra.len() as u16).to_le_bytes());
            out.extend(&e.name);
            out.extend(&extra);
            out.extend(&payload);
            if e.descriptor {
                out.extend(b"PK\x07\x08");
                out.extend(crc.to_le_bytes());
                out.extend(csize.to_le_bytes());
                out.extend(usize.to_le_bytes());
            }
            central.extend(b"PK\x01\x02");
            central.extend(0x0314u16.to_le_bytes()); // made by Unix, 2.0
            central.extend(20u16.to_le_bytes());
            central.extend(flags.to_le_bytes());
            central.extend(method.to_le_bytes());
            central.extend(DOS_TIME.to_le_bytes());
            central.extend(DOS_DATE.to_le_bytes());
            central.extend(crc.to_le_bytes());
            central.extend(csize.to_le_bytes());
            central.extend(usize.to_le_bytes());
            central.extend((e.name.len() as u16).to_le_bytes());
            central.extend((extra.len() as u16).to_le_bytes());
            central.extend(0u16.to_le_bytes()); // comment
            central.extend(0u16.to_le_bytes()); // disk
            central.extend(0u16.to_le_bytes()); // internal attrs
            let ext: u32 = if e.dir {
                (0o40755 << 16) | 0x10
            } else {
                0o100644 << 16
            };
            central.extend(ext.to_le_bytes());
            central.extend(offset.to_le_bytes());
            central.extend(&e.name);
            central.extend(&extra);
        }
        if self.omit_central {
            return out;
        }
        let cd_offset = (out.len() - self.prefix.len()) as u32;
        let cd_size = central.len() as u32;
        out.extend(&central);
        out.extend(b"PK\x05\x06");
        out.extend([0u8; 4]);
        out.extend((self.entries.len() as u16).to_le_bytes());
        out.extend((self.entries.len() as u16).to_le_bytes());
        out.extend(cd_size.to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }
}

/// A zip of deflated files.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = ZipBuilder::new();
    for (n, d) in files {
        b.file(n, d);
    }
    b.build()
}

// ---------------------------------------------------------------- tar ---

/// One tar member.
#[derive(Debug, Clone)]
pub enum TarEntry {
    File(String, Vec<u8>),
    Dir(String),
    /// A file whose name goes into a GNU `L` record.
    GnuLong(String, Vec<u8>),
    /// A file whose name goes into a pax `x` record.
    Pax(String, Vec<u8>),
    /// A symbolic link (no data).
    Symlink(String, String),
}

fn octal(field: &mut [u8], v: u64) {
    let s = format!("{:0width$o}\0", v, width = field.len() - 1);
    field.copy_from_slice(s.as_bytes());
}

fn tar_header(name: &str, size: u64, typ: u8, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    let nb = name.as_bytes();
    h[..nb.len().min(100)].copy_from_slice(&nb[..nb.len().min(100)]);
    octal(&mut h[100..108], 0o644);
    octal(&mut h[108..116], 1000);
    octal(&mut h[116..124], 1000);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], MTIME as u64);
    h[156] = typ;
    let lb = link.as_bytes();
    h[157..157 + lb.len().min(100)].copy_from_slice(&lb[..lb.len().min(100)]);
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

fn tar_data(out: &mut Vec<u8>, data: &[u8]) {
    out.extend(data);
    let pad = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// A tar archive (ends with two zero blocks).
pub fn tar(entries: &[TarEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            TarEntry::File(n, d) => {
                out.extend(tar_header(n, d.len() as u64, b'0', ""));
                tar_data(&mut out, d);
            }
            TarEntry::Dir(n) => {
                let n = if n.ends_with('/') {
                    n.clone()
                } else {
                    format!("{n}/")
                };
                out.extend(tar_header(&n, 0, b'5', ""));
            }
            TarEntry::GnuLong(n, d) => {
                let mut name = n.as_bytes().to_vec();
                name.push(0);
                let mut h = tar_header("././@LongLink", name.len() as u64, b'L', "");
                h[257..265].copy_from_slice(b"ustar  \0");
                h[148..156].copy_from_slice(b"        ");
                let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
                h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
                out.extend(h);
                tar_data(&mut out, &name);
                out.extend(tar_header(&n[..n.len().min(99)], d.len() as u64, b'0', ""));
                tar_data(&mut out, d);
            }
            TarEntry::Pax(n, d) => {
                let body = |len: usize| format!("{len} path={n}\n");
                let mut len = body(0).len();
                while body(len).len() != len {
                    len = body(len).len();
                }
                let rec = body(len);
                out.extend(tar_header("PaxHeaders/x", rec.len() as u64, b'x', ""));
                tar_data(&mut out, rec.as_bytes());
                out.extend(tar_header("short", d.len() as u64, b'0', ""));
                tar_data(&mut out, d);
            }
            TarEntry::Symlink(n, t) => out.extend(tar_header(n, 0, b'2', t)),
        }
    }
    out.extend([0u8; 1024]);
    out
}

// ---------------------------------------------------- single streams ---

/// gzip with an optional stored file name.
pub fn gzip(name: Option<&str>, data: &[u8]) -> Vec<u8> {
    let mut b = flate2::GzBuilder::new().mtime(MTIME as u32);
    if let Some(n) = name {
        b = b.filename(n);
    }
    let mut e = b.write(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

pub fn bzip2_bytes(data: &[u8]) -> Vec<u8> {
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

pub fn xz_bytes(data: &[u8]) -> Vec<u8> {
    let mut w =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap()
}

pub fn zstd_bytes(data: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest)
}

// ----------------------------------------------------------------- 7z ---

/// A 7z archive; `solid` puts all files into one block; `password`
/// encrypts the data (and, with `encrypt_header`, the header too).
pub fn sevenz(
    files: &[(&str, &[u8])],
    dirs: &[&str],
    solid: bool,
    password: Option<&str>,
    encrypt_header: bool,
) -> Vec<u8> {
    use sevenz_rust2::{
        ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, Password,
        encoder_options::AesEncoderOptions,
    };
    let mut w = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    let mut methods = Vec::new();
    if let Some(p) = password {
        methods.push(AesEncoderOptions::new(Password::new(p)).into());
        w.set_encrypt_header(encrypt_header);
    }
    methods.push(EncoderConfiguration::new(EncoderMethod::LZMA2));
    w.set_content_methods(methods);
    for d in dirs {
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(d), None)
            .unwrap();
    }
    if solid {
        let entries: Vec<ArchiveEntry> = files
            .iter()
            .map(|(n, _)| ArchiveEntry::new_file(n))
            .collect();
        let readers: Vec<sevenz_rust2::SourceReader<&[u8]>> =
            files.iter().map(|(_, d)| (*d).into()).collect();
        w.push_archive_entries(entries, readers).unwrap();
    } else {
        for (n, d) in files {
            w.push_archive_entry(ArchiveEntry::new_file(n), Some(*d))
                .unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

// ---------------------------------------------------------------- rar ---

/// A RAR archive of the given version (`rars` writer).
pub fn rar(
    version: rars::ArchiveVersion,
    files: &[(&str, &[u8])],
    password: Option<&str>,
) -> Vec<u8> {
    let mut b = rars::Builder::new(version).password(password.map(|p| p.as_bytes().to_vec()));
    for (n, d) in files {
        b.add_bytes(n.as_bytes().to_vec(), d.to_vec(), None, None)
            .unwrap();
    }
    b.to_bytes().unwrap()
}

// ---------------------------------------------------------------- iso ---

/// An ISO 9660 image with Joliet and Rock Ridge names (`fstool`).
/// `files` paths may contain directories (`a/b.txt`).
pub fn iso(files: &[(&str, &[u8])], joliet: bool, rock_ridge: bool) -> Vec<u8> {
    use fstool::block::MemoryBackend;
    use fstool::fs::iso9660::{FormatOpts, Iso9660};
    use fstool::fs::{FileMeta, FileSource, Filesystem};
    let total: usize = files.iter().map(|f| f.1.len() + 4096).sum::<usize>() + (1 << 20);
    let mut dev = MemoryBackend::new(total as u64);
    let opts = FormatOpts {
        volume_id: "PMTEST".into(),
        joliet,
        rock_ridge,
        create_date: MTIME as u32,
        ..FormatOpts::default()
    };
    let mut fs = Iso9660::format(&mut dev, &opts).unwrap();
    let meta = FileMeta {
        mode: 0o644,
        uid: 0,
        gid: 0,
        mtime: MTIME as u32,
        atime: MTIME as u32,
        ctime: MTIME as u32,
    };
    for (p, d) in files {
        fs.create_file(
            &mut dev,
            &Path::new("/").join(p),
            FileSource::Reader {
                reader: Box::new(Cursor::new(d.to_vec())),
                len: d.len() as u64,
            },
            meta,
        )
        .unwrap();
    }
    fs.flush(&mut dev).unwrap();
    let mut img = dev.as_slice().to_vec();
    // Trim trailing unused space to the written length.
    while img.len() > 64 * 1024 && img.ends_with(&[0u8; 2048]) {
        img.truncate(img.len() - 2048);
    }
    // `fstool` leaves the recording dates of directory records zero; give
    // every record (primary and Joliet trees) the fixture time.
    let date = iso_record_date(MTIME);
    for vd in 16..32usize {
        let at = vd * 2048;
        match img.get(at) {
            Some(1 | 2) => {
                let root = img[at + 156..at + 156 + 34].to_vec();
                iso_date_tree(&mut img, &root, &date, 0);
            }
            _ => break,
        }
    }
    img
}

/// ISO 9660 7-byte recording date (UTC) of Unix seconds.
fn iso_record_date(t: i64) -> [u8; 7] {
    let ts = jiff::Timestamp::from_second(t).unwrap();
    let dt = ts.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    [
        (dt.year() - 1900) as u8,
        dt.month() as u8,
        dt.day() as u8,
        dt.hour() as u8,
        dt.minute() as u8,
        dt.second() as u8,
        0,
    ]
}

/// Sets the date of every record below the directory record `dir`.
fn iso_date_tree(img: &mut [u8], dir: &[u8], date: &[u8; 7], depth: u32) {
    if depth > 16 {
        return;
    }
    let lba = u32::from_le_bytes(dir[2..6].try_into().unwrap()) as usize;
    let len = u32::from_le_bytes(dir[10..14].try_into().unwrap()) as usize;
    let (start, end) = (lba * 2048, (lba * 2048 + len).min(img.len()));
    let mut pos = start;
    let mut subdirs = Vec::new();
    while pos < end {
        let rlen = usize::from(img[pos]);
        if rlen == 0 {
            // Records do not cross sectors: continue in the next one.
            pos = (pos / 2048 + 1) * 2048;
            continue;
        }
        img[pos + 18..pos + 25].copy_from_slice(date);
        let name_len = usize::from(img[pos + 32]);
        let dot = name_len == 1 && img[pos + 33] <= 1;
        if img[pos + 25] & 2 != 0 && !dot {
            subdirs.push(img[pos..pos + rlen].to_vec());
        }
        pos += rlen;
    }
    for s in subdirs {
        iso_date_tree(img, &s, date, depth + 1);
    }
}

// ------------------------------------------------------------- images ---

/// A small valid PNG (gradient).
pub fn png(w: u32, h: u32) -> Vec<u8> {
    let img =
        image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 8) as u8, (y * 8) as u8, 128]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A small valid baseline JPEG (gradient).
pub fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 4) as u8, 64, (y * 4) as u8]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

/// A small valid GIF.
pub fn gif(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, _| image::Rgba([(x * 16) as u8, 0, 0, 255]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Gif).unwrap();
    out.into_inner()
}

// ------------------------------------------------------------- office ---

/// Office Open XML package with `media` (`word/media/<name>`) and
/// `embeddings` parts; `kind` is "word", "xl" or "ppt".
pub fn ooxml(kind: &str, media: &[(&str, &[u8])], embeddings: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = ZipBuilder::new();
    z.file(
        "[Content_Types].xml",
        br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#,
    );
    z.file("_rels/.rels", b"<Relationships/>");
    let main = match kind {
        "xl" => "xl/workbook.xml",
        "ppt" => "ppt/presentation.xml",
        _ => "word/document.xml",
    };
    z.file(main, b"<document/>");
    for (n, d) in media {
        z.file(&format!("{kind}/media/{n}"), d);
    }
    for (n, d) in embeddings {
        z.file(&format!("{kind}/embeddings/{n}"), d);
    }
    z.build()
}

/// BLIP record types.
pub const BLIP_EMF: u16 = 0xF01A;
pub const BLIP_WMF: u16 = 0xF01B;
pub const BLIP_PICT: u16 = 0xF01C;
pub const BLIP_JPEG: u16 = 0xF01D;
pub const BLIP_PNG: u16 = 0xF01E;
pub const BLIP_DIB: u16 = 0xF01F;

/// An OfficeArt BLIP record holding `data` (metafiles are deflated, as
/// Office writes them).
pub fn blip(ty: u16, data: &[u8], uid: u8) -> Vec<u8> {
    let inst: u16 = match ty {
        BLIP_EMF => 0x3D4,
        BLIP_WMF => 0x216,
        BLIP_PICT => 0x542,
        BLIP_JPEG => 0x46A,
        BLIP_PNG => 0x6E0,
        BLIP_DIB => 0x7A8,
        _ => panic!("unknown BLIP type"),
    };
    let mut body = vec![uid; 16];
    if matches!(ty, BLIP_EMF | BLIP_WMF | BLIP_PICT) {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(data).unwrap();
        let comp = z.finish().unwrap();
        body.extend((data.len() as u32).to_le_bytes());
        for v in [0i32, 0, 100, 100] {
            body.extend(v.to_le_bytes());
        }
        body.extend(1000i32.to_le_bytes());
        body.extend(1000i32.to_le_bytes());
        body.extend((comp.len() as u32).to_le_bytes());
        body.extend([0u8, 0xFE]);
        body.extend(comp);
    } else {
        body.push(0xFF);
        body.extend(data);
    }
    let mut r = Vec::new();
    r.extend((inst << 4).to_le_bytes());
    r.extend(ty.to_le_bytes());
    r.extend((body.len() as u32).to_le_bytes());
    r.extend(body);
    r
}

/// A Word 97 document whose `Data` stream holds the BLIPs.
pub fn doc(blips: &[Vec<u8>]) -> Vec<u8> {
    let mut data = vec![0u8; 68]; // PICF header stand-in
    for b in blips {
        data.extend(b);
        data.extend([0u8; 13]);
    }
    let mut word = vec![0xEC, 0xA5, 0xC1, 0x00];
    word.resize(1024, 0);
    pm_lnk::builder::compound_file(&[
        ("WordDocument", word),
        ("1Table", vec![0u8; 512]),
        ("Data", data),
    ])
}

/// An Excel 97 workbook whose drawing group holds the BLIPs (split over
/// `Continue` records when longer than a BIFF record).
pub fn xls(blips: &[Vec<u8>]) -> Vec<u8> {
    let rec = |ty: u16, body: &[u8]| {
        let mut r = Vec::new();
        r.extend(ty.to_le_bytes());
        r.extend((body.len() as u16).to_le_bytes());
        r.extend(body);
        r
    };
    let mut bstore = Vec::new();
    for b in blips {
        // FBSE header (36 bytes) followed by the embedded BLIP.
        let mut fbse_body = vec![0u8; 36];
        fbse_body.extend(b);
        bstore.extend(0x0002u16.to_le_bytes());
        bstore.extend(0xF007u16.to_le_bytes());
        bstore.extend((fbse_body.len() as u32).to_le_bytes());
        bstore.extend(fbse_body);
    }
    let mut dgg = Vec::new();
    dgg.extend(0x000Fu16.to_le_bytes());
    dgg.extend(0xF001u16.to_le_bytes());
    dgg.extend((bstore.len() as u32).to_le_bytes());
    dgg.extend(bstore);
    let mut group = Vec::new();
    group.extend(0x000Fu16.to_le_bytes());
    group.extend(0xF000u16.to_le_bytes());
    group.extend((dgg.len() as u32).to_le_bytes());
    group.extend(dgg);
    let mut wb = rec(0x0809, &[0, 6, 5, 0, 0, 0, 0, 0]);
    let mut chunks = group.chunks(8224);
    if let Some(first) = chunks.next() {
        wb.extend(rec(0x00EB, first));
    }
    for c in chunks {
        wb.extend(rec(0x003C, c));
    }
    wb.extend(rec(0x000A, &[]));
    pm_lnk::builder::compound_file(&[("Workbook", wb)])
}

/// A PowerPoint 97 presentation whose `Pictures` stream holds the BLIPs.
pub fn ppt(blips: &[Vec<u8>]) -> Vec<u8> {
    pm_lnk::builder::compound_file(&[
        ("PowerPoint Document", vec![0u8; 600]),
        ("Current User", vec![0u8; 40]),
        ("Pictures", blips.concat()),
    ])
}

// ---------------------------------------------------------------- pdf ---

/// An image placed in a PDF.
#[derive(Debug, Clone)]
pub enum PdfImage {
    Jpeg(Vec<u8>),
    /// 8-bit gray pixels, flate-compressed.
    Gray {
        w: u32,
        h: u32,
        pixels: Vec<u8>,
    },
}

/// A PDF with attachments (`/EmbeddedFiles` name tree, file
/// specifications with `/UF`) and image XObjects.
pub fn pdf(attachments: &[(&str, &[u8])], images: &[PdfImage]) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = Vec::new();
    fn push(objs: &mut Vec<Vec<u8>>, o: Vec<u8>) -> usize {
        objs.push(o);
        objs.len()
    }
    let zlib = |d: &[u8]| {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(d).unwrap();
        z.finish().unwrap()
    };
    let stream = |dict: String, data: &[u8]| {
        let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        o.extend(data);
        o.extend(b"\nendstream");
        o
    };
    let mut names = Vec::new();
    for (n, d) in attachments {
        let ef = push(
            &mut objs,
            stream(
                format!(
                    "/Type /EmbeddedFile /Filter /FlateDecode /Params << /Size {} >>",
                    d.len()
                ),
                &zlib(d),
            ),
        );
        let utf16: String = std::iter::once("FEFF".to_owned())
            .chain(n.encode_utf16().map(|u| format!("{u:04X}")))
            .collect();
        let fs = push(
            &mut objs,
            format!(
                "<< /Type /Filespec /F ({n}) /UF <{utf16}> /EF << /F {ef} 0 R /UF {ef} 0 R >> >>"
            )
            .into_bytes(),
        );
        names.push(format!("({n}) {fs} 0 R"));
    }
    let mut xobjects = Vec::new();
    for (i, img) in images.iter().enumerate() {
        let id = match img {
            PdfImage::Jpeg(j) => push(&mut objs, stream(
                "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode".into(),
                j,
            )),
            PdfImage::Gray { w, h, pixels } => push(&mut objs, stream(
                format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode"),
                &zlib(pixels),
            )),
        };
        xobjects.push(format!("/Im{i} {id} 0 R"));
    }
    let content = push(&mut objs, stream(String::new(), b"q Q"));
    let names_tree = push(
        &mut objs,
        format!("<< /Names [{}] >>", names.join(" ")).into_bytes(),
    );
    // The page is the next object and its parent the one after.
    let parent = objs.len() + 2;
    let page = push(
        &mut objs,
        format!(
            "<< /Type /Page /MediaBox [0 0 100 100] /Contents {content} 0 R /Resources << /XObject << {} >> >> /Parent {parent} 0 R >>",
            xobjects.join(" "),
        )
        .into_bytes(),
    );
    let pages = push(
        &mut objs,
        format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>").into_bytes(),
    );
    let catalog = push(
        &mut objs,
        format!(
            "<< /Type /Catalog /Pages {pages} 0 R /Names << /EmbeddedFiles {names_tree} 0 R >> >>"
        )
        .into_bytes(),
    );
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root {catalog} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Reads everything from a reader (test helper).
pub fn read_all(mut r: impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    v
}
