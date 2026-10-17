//! Signature-based file carving (a native, PhotoRec-like carver).
//!
//! [`carve`] makes one pass over a byte source (normally an
//! unallocated-space file): it reads large aligned chunks and, like
//! PhotoRec, looks for file headers at the start of every block
//! (512 bytes by default), using a first-byte dispatch table. A matching
//! header is handed to its format's validator and length finder, which
//! walks the format's structure (JPEG markers, PNG chunks, zip
//! directory, OLE allocation table, MP4 boxes, …) to find where the file
//! ends. The scan then continues after the carved file, so its bytes are
//! not scanned again.
//!
//! Carved files are reported as byte ranges; nothing is copied.
//! [`map_to_image`] turns a range of an unallocated-space file into
//! ranges of the disk image (Autopsy `convertToImgRanges`), and
//! [`photorec_name`] names files the way PhotoRec does (`f<sector>.<ext>`).
//!
//! Supported PhotoRec families (`fileopt` names): `jpg`, `png`, `gif`,
//! `bmp`, `tif`, `pdf`, `zip` (also docx/xlsx/pptx, odt/ods/odp/odg, jar,
//! apk), `doc` (OLE: doc, xls, ppt), `7z`, `gz`, `mp3`, `riff` (avi, wav,
//! webp, …), `mov` (mp4, mov, m4a, m4v, 3gp, heic, avif, cr3), `sqlite`,
//! `exe` (exe, dll), `evtx`.

#![allow(
    clippy::many_single_char_names,
    reason = "binary-format code uses short names"
)]

mod container;
mod image;
mod known;
mod media;
mod misc;
mod window;

use std::collections::HashSet;

pub use known::KNOWN_EXTENSIONS;
use pm_core::ReadAt;
use window::Window;

/// What a length finder concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Not a file of this type.
    No,
    /// A complete file of `len` bytes.
    Found { len: u64, ext: &'static str },
    /// A valid start whose structure breaks off after `len` bytes
    /// (PhotoRec's "corrupted" files, kept only on request).
    Truncated { len: u64, ext: &'static str },
}

/// How a format is recognised and measured.
pub(crate) enum Finder {
    Plain(fn(&mut Window<'_>, u64) -> Verdict),
    /// Needs the source itself (sub-format refinement, decoding).
    WithSource(fn(&mut Window<'_>, &dyn ReadAt, u64) -> Verdict),
}

/// One file signature.
pub(crate) struct Spec {
    /// PhotoRec family (`fileopt` name) the format belongs to.
    pub(crate) family: &'static str,
    /// Header bytes, at `at` from the start of a block.
    pub(crate) magic: &'static [u8],
    pub(crate) at: usize,
    /// Largest file accepted.
    pub(crate) max: u64,
    pub(crate) finder: Finder,
}

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// Every signature, in the order they are tried at a block.
fn specs() -> Vec<Spec> {
    use Finder::{Plain, WithSource};
    vec![
        Spec {
            family: "jpg",
            magic: &[0xFF, 0xD8, 0xFF],
            at: 0,
            max: 200 * MIB,
            finder: Plain(image::jpeg),
        },
        Spec {
            family: "png",
            magic: b"\x89PNG\r\n\x1a\n",
            at: 0,
            max: 200 * MIB,
            finder: Plain(image::png),
        },
        Spec {
            family: "gif",
            magic: b"GIF87a",
            at: 0,
            max: 100 * MIB,
            finder: Plain(image::gif),
        },
        Spec {
            family: "gif",
            magic: b"GIF89a",
            at: 0,
            max: 100 * MIB,
            finder: Plain(image::gif),
        },
        Spec {
            family: "bmp",
            magic: b"BM",
            at: 0,
            max: 500 * MIB,
            finder: Plain(image::bmp),
        },
        Spec {
            family: "tif",
            magic: b"II*\0",
            at: 0,
            max: GIB,
            finder: Plain(image::tiff),
        },
        Spec {
            family: "tif",
            magic: b"MM\0*",
            at: 0,
            max: GIB,
            finder: Plain(image::tiff),
        },
        Spec {
            family: "pdf",
            magic: b"%PDF-",
            at: 0,
            max: 512 * MIB,
            finder: Plain(container::pdf),
        },
        Spec {
            family: "zip",
            magic: b"PK\x03\x04",
            at: 0,
            max: 4 * GIB,
            finder: WithSource(container::zip),
        },
        Spec {
            family: "doc",
            magic: &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1],
            at: 0,
            max: 4 * GIB,
            finder: WithSource(container::ole),
        },
        Spec {
            family: "7z",
            magic: &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C],
            at: 0,
            max: 64 * GIB,
            finder: Plain(container::sevenz),
        },
        Spec {
            family: "gz",
            magic: &[0x1F, 0x8B, 0x08],
            at: 0,
            max: 4 * GIB,
            finder: WithSource(container::gzip),
        },
        Spec {
            family: "mp3",
            magic: b"ID3",
            at: 0,
            max: GIB,
            finder: Plain(media::mp3),
        },
        Spec {
            family: "mp3",
            magic: &[0xFF],
            at: 0,
            max: GIB,
            finder: Plain(media::mp3),
        },
        Spec {
            family: "riff",
            magic: b"RIFF",
            at: 0,
            max: 64 * GIB,
            finder: Plain(media::riff),
        },
        Spec {
            family: "mov",
            magic: b"ftyp",
            at: 4,
            max: 64 * GIB,
            finder: Plain(media::mp4),
        },
        Spec {
            family: "mov",
            magic: b"moov",
            at: 4,
            max: 64 * GIB,
            finder: Plain(media::mp4),
        },
        Spec {
            family: "mov",
            magic: b"mdat",
            at: 4,
            max: 64 * GIB,
            finder: Plain(media::mp4),
        },
        Spec {
            family: "mov",
            magic: b"wide",
            at: 4,
            max: 64 * GIB,
            finder: Plain(media::mp4),
        },
        Spec {
            family: "sqlite",
            magic: b"SQLite format 3\0",
            at: 0,
            max: 64 * GIB,
            finder: Plain(misc::sqlite),
        },
        Spec {
            family: "exe",
            magic: b"MZ",
            at: 0,
            max: 4 * GIB,
            finder: Plain(misc::exe),
        },
        Spec {
            family: "evtx",
            magic: b"ElfFile\0",
            at: 0,
            max: 4 * GIB,
            finder: Plain(misc::evtx),
        },
    ]
}

/// The PhotoRec families this carver implements.
pub fn supported_families() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = specs().iter().map(|s| s.family).collect();
    v.dedup();
    v.sort_unstable();
    v.dedup();
    v
}

/// Which families to carve (Autopsy's extension filter).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Filter {
    /// Everything.
    #[default]
    All,
    /// Only these `fileopt` names.
    Include(HashSet<String>),
    /// Everything but these.
    Exclude(HashSet<String>),
}

impl Filter {
    fn accepts(&self, family: &str) -> bool {
        match self {
            Self::All => true,
            Self::Include(s) => s.contains(family),
            Self::Exclude(s) => !s.contains(family),
        }
    }
}

/// Carving settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Headers are looked for at multiples of this (PhotoRec: the sector
    /// size).
    pub block_size: u64,
    /// Keep files whose structure breaks off (PhotoRec
    /// `keep_corrupted_file`).
    pub keep_corrupted: bool,
    pub filter: Filter,
    /// Bytes read per scan step.
    pub chunk: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            block_size: 512,
            keep_corrupted: false,
            filter: Filter::All,
            chunk: 4 << 20,
        }
    }
}

/// A carved file: a byte range of the scanned source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carved {
    pub offset: u64,
    pub len: u64,
    /// File extension (`jpg`, `docx`, …).
    pub ext: &'static str,
    /// The PhotoRec family that found it.
    pub family: &'static str,
    /// The structure breaks off (kept because of `keep_corrupted`).
    pub corrupted: bool,
}

/// Outcome of a scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    /// Bytes scanned for headers (carved files' bytes are skipped).
    pub scanned: u64,
    pub cancelled: bool,
}

/// Errors that stop a scan.
#[derive(Debug, thiserror::Error)]
pub enum CarveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid options: {0}")]
    Options(String),
}

/// Specs grouped by the byte that starts their magic, plus those whose
/// magic is not at the block start.
struct Table {
    specs: Vec<Spec>,
    by_first: Vec<Vec<usize>>,
    offset: Vec<usize>,
}

impl Table {
    fn new(filter: &Filter) -> Self {
        let specs: Vec<Spec> = specs()
            .into_iter()
            .filter(|s| filter.accepts(s.family))
            .collect();
        let mut by_first = vec![Vec::new(); 256];
        let mut offset = Vec::new();
        for (i, s) in specs.iter().enumerate() {
            match (s.at, s.magic.first()) {
                (0, Some(&b)) => {
                    if let Some(v) = by_first.get_mut(usize::from(b)) {
                        v.push(i);
                    }
                }
                _ => offset.push(i),
            }
        }
        Self {
            specs,
            by_first,
            offset,
        }
    }

    /// The specs whose magic matches the block starting with `head`.
    fn candidates<'t>(&'t self, head: &'t [u8]) -> impl Iterator<Item = &'t Spec> + 't {
        let first = head
            .first()
            .and_then(|&b| self.by_first.get(usize::from(b)))
            .map(Vec::as_slice)
            .unwrap_or_default();
        first
            .iter()
            .chain(self.offset.iter())
            .filter_map(|&i| self.specs.get(i))
            .filter(move |s| head.get(s.at..s.at + s.magic.len()) == Some(s.magic))
    }
}

/// Scans `src` and reports every carved file to `found` (in offset
/// order). `cancel` is polled between chunks.
pub fn carve(
    src: &dyn ReadAt,
    opts: &Options,
    cancel: &dyn Fn() -> bool,
    found: &mut dyn FnMut(Carved),
) -> Result<Stats, CarveError> {
    if opts.block_size == 0 || !opts.block_size.is_power_of_two() || opts.block_size > 1 << 20 {
        return Err(CarveError::Options(format!(
            "block size {} is not a power of two up to 1 MiB",
            opts.block_size
        )));
    }
    let table = Table::new(&opts.filter);
    let size = src.size();
    let bs = opts.block_size;
    let chunk = (opts.chunk as u64).max(bs) / bs * bs;
    // Room for the longest magic plus its offset past a chunk's last block.
    let tail = 32u64;
    let mut stats = Stats::default();
    let mut buf = Vec::new();
    let mut pos = 0u64;
    'chunks: while pos < size {
        if cancel() {
            stats.cancelled = true;
            break;
        }
        let want = usize::try_from((chunk + tail).min(size - pos)).unwrap_or(0);
        buf.resize(want, 0);
        let got = read_full(src, pos, &mut buf)?;
        buf.truncate(got);
        if got == 0 {
            break;
        }
        let scan_end = (chunk.min(got as u64)).min(size - pos);
        let mut i = 0u64;
        while i < scan_end {
            let head = buf.get(i as usize..).unwrap_or_default();
            let head = head
                .get(..head.len().min(tail as usize))
                .unwrap_or_default();
            let start = pos + i;
            let mut next = None;
            for spec in table.candidates(head) {
                let mut w = Window::new(src, start.saturating_add(spec.max));
                let verdict = match spec.finder {
                    Finder::Plain(f) => f(&mut w, start),
                    Finder::WithSource(f) => f(&mut w, src, start),
                };
                let (len, ext, corrupted) = match verdict {
                    Verdict::No => continue,
                    Verdict::Found { len, ext } => (len, ext, false),
                    Verdict::Truncated { len, ext } => {
                        if !opts.keep_corrupted {
                            continue;
                        }
                        (len, ext, true)
                    }
                };
                if len == 0 {
                    continue;
                }
                let len = len.min(size - start);
                found(Carved {
                    offset: start,
                    len,
                    ext,
                    family: spec.family,
                    corrupted,
                });
                stats.files += 1;
                next = Some(align_up(start + len, bs));
                break;
            }
            match next {
                Some(n) if n >= pos + scan_end => {
                    stats.scanned += i;
                    pos = n;
                    continue 'chunks;
                }
                Some(n) => i = n - pos,
                None => i += bs,
            }
        }
        stats.scanned += scan_end;
        pos += scan_end;
    }
    Ok(stats)
}

fn align_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a).saturating_mul(a)
}

fn read_full(src: &dyn ReadAt, pos: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let Some(rest) = buf.get_mut(got..) else {
            break;
        };
        match src.read_at(pos + got as u64, rest) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// PhotoRec's name for a file found at byte `offset` of the scanned
/// source: `f` and the 512-byte sector number, at least 7 digits.
pub fn photorec_name(offset: u64, ext: &str) -> String {
    format!("f{:07}.{ext}", offset / 512)
}

/// Maps `[offset, offset + len)` of a file made of the image byte ranges
/// `layout` (in order) to image ranges, splitting where the file's ranges
/// are not contiguous (Autopsy `AbstractFile.convertToImgRanges`). Parts
/// past the end of the layout are dropped.
pub fn map_to_image(layout: &[(u64, u64)], offset: u64, len: u64) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let end = offset.saturating_add(len);
    let mut logical = 0u64;
    for &(img, run) in layout {
        let run_end = logical.saturating_add(run);
        let lo = offset.max(logical);
        let hi = end.min(run_end);
        if lo < hi {
            let start = img + (lo - logical);
            match out.last_mut() {
                Some(last) if last.0 + last.1 == start => last.1 += hi - lo,
                _ => out.push((start, hi - lo)),
            }
        }
        if run_end >= end {
            break;
        }
        logical = run_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_photorec() {
        assert_eq!(photorec_name(0, "jpg"), "f0000000.jpg");
        assert_eq!(photorec_name(512 * 1234, "png"), "f0001234.png");
        assert_eq!(photorec_name(512 * 123_456_789, "doc"), "f123456789.doc");
    }

    #[test]
    fn ranges_map_through_the_layout() {
        let layout = [(10_000, 100), (50_000, 100), (50_100, 50)];
        assert_eq!(map_to_image(&layout, 0, 50), [(10_000, 50)]);
        assert_eq!(map_to_image(&layout, 90, 20), [(10_090, 10), (50_000, 10)]);
        // Adjacent image runs merge.
        assert_eq!(map_to_image(&layout, 150, 100), [(50_050, 100)]);
        assert_eq!(map_to_image(&layout, 240, 100), [(50_140, 10)]);
        assert!(map_to_image(&layout, 400, 10).is_empty());
        assert!(map_to_image(&[], 0, 10).is_empty());
    }

    #[test]
    fn families_are_photorec_names() {
        for f in supported_families() {
            assert!(KNOWN_EXTENSIONS.contains(&f), "{f}");
        }
    }

    #[test]
    fn bad_block_size_is_refused() {
        let src = pm_core::io::MemSource::new(vec![0; 10]);
        let opts = Options {
            block_size: 500,
            ..Options::default()
        };
        assert!(carve(&src, &opts, &|| false, &mut |_| {}).is_err());
    }
}
