//! Single-stream compressors: gzip (multi-member), bzip2 (multi-stream),
//! xz (multi-stream) and zstd (multi-frame, skippable frames).
//!
//! Each yields one member. Its name is the gzip `FNAME` when present;
//! otherwise it is empty and the caller applies
//! [`crate::default_member_name`] (Autopsy's rule).

use std::io::{self, BufRead, BufReader, Read};

use pm_core::{Bytes, ReadAt};
use ruzstd::decoding::{FrameDecoder, StreamingDecoder};

use crate::detect::Format;
use crate::driver::Driver;
use crate::io::{RangeReader, read_upto};
use crate::zip::mem_limit_kb;
use crate::{ArchiveError, Entry};

pub(crate) fn extract(
    format: Format,
    src: &dyn ReadAt,
    d: &mut Driver<'_>,
) -> Result<(), ArchiveError> {
    let size = src.size();
    let input = BufReader::with_capacity(256 * 1024, RangeReader::new(src, 0, size));
    let mut entry = Entry {
        packed_size: Some(size),
        ..Entry::default()
    };
    let max_dict = d.opts.max_dictionary;
    let mut dec: Box<dyn Read + '_> = match format {
        Format::Gzip => {
            let h = gzip_header(src);
            entry.path = h.name.unwrap_or_default();
            entry.modified = h.mtime;
            // ISIZE (the size modulo 2^32 in the last four bytes) is not
            // reported: it is meaningless for truncated, concatenated or
            // carved streams, which is why Autopsy skips the declared
            // ratio check for gzip. The decoded-byte meter still applies.
            Box::new(flate2::bufread::MultiGzDecoder::new(input))
        }
        Format::Bzip2 => Box::new(bzip2::bufread::MultiBzDecoder::new(input)),
        Format::Xz => Box::new(lzma_rust2::XzReader::new_mem_limit(
            input,
            true,
            mem_limit_kb(max_dict),
        )),
        Format::Zstd => {
            if let Some(fcs) = zstd_content_size(src) {
                entry.size = Some(fcs);
            }
            Box::new(ZstdFrames::new(input, max_dict))
        }
        _ => {
            return Err(ArchiveError::Unsupported(format!(
                "{format:?} as a single stream"
            )));
        }
    };
    d.member(&entry, Some(&mut dec), None)?;
    Ok(())
}

/// Fields of a gzip member header.
#[derive(Debug, Default)]
struct GzipHeader {
    name: Option<String>,
    mtime: Option<i64>,
}

/// Parses the first gzip member header (RFC 1952).
fn gzip_header(src: &dyn ReadAt) -> GzipHeader {
    let Ok(h) = read_upto(src, 0, 64 * 1024) else {
        return GzipHeader::default();
    };
    let mut out = GzipHeader::default();
    let (Ok(flags), Ok(mtime)) = (h.u8_at(3), h.u32_le(4)) else {
        return out;
    };
    if mtime != 0 {
        out.mtime = Some(i64::from(mtime));
    }
    let mut p = 10usize;
    if flags & 0x04 != 0 {
        let xlen = usize::from(h.u16_le(p).unwrap_or(0));
        p += 2 + xlen;
    }
    if flags & 0x08 != 0
        && let Some(rest) = h.get(p..)
        && let Some(end) = memchr::memchr(0, rest)
    {
        // ISO 8859-1.
        let name: String = rest
            .get(..end)
            .unwrap_or_default()
            .iter()
            .map(|&b| char::from(b))
            .collect();
        if !name.is_empty() {
            out.name = Some(name);
        }
    }
    out
}

/// The frame content size of a single-frame zstd stream, if declared.
fn zstd_content_size(src: &dyn ReadAt) -> Option<u64> {
    let h = read_upto(src, 0, 18).ok()?;
    let fhd = h.u8_at(4).ok()?;
    let fcs_flag = fhd >> 6;
    let single = fhd & 0x20 != 0;
    let dict_flag = fhd & 0x03;
    let mut p = 5usize;
    if !single {
        p += 1;
    }
    p += match dict_flag {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    match (fcs_flag, single) {
        (0, true) => h.u8_at(p).ok().map(u64::from),
        (0, false) => None,
        (1, _) => h.u16_le(p).ok().map(|v| u64::from(v) + 256),
        (2, _) => h.u32_le(p).ok().map(u64::from),
        _ => h.u64_le(p).ok(),
    }
}

enum ZState<R: BufRead> {
    Idle(R),
    Frame(Box<StreamingDecoder<R, FrameDecoder>>),
    Done,
}

/// Decodes consecutive zstd frames (skipping skippable frames) with a
/// window-size limit.
pub(crate) struct ZstdFrames<R: BufRead> {
    state: ZState<R>,
    max_window: u64,
}

impl<R: BufRead> ZstdFrames<R> {
    pub(crate) fn new(r: R, max_window: u64) -> Self {
        Self {
            state: ZState::Idle(r),
            max_window,
        }
    }
}

impl<R: BufRead> Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match std::mem::replace(&mut self.state, ZState::Done) {
                ZState::Done => return Ok(0),
                ZState::Idle(mut r) => {
                    let head = r.fill_buf()?;
                    if head.is_empty() {
                        return Ok(0);
                    }
                    let mut magic = [0u8; 4];
                    let mut got = 0;
                    // The magic may straddle buffer refills.
                    let mut peek = Vec::with_capacity(8);
                    peek.extend_from_slice(head.get(..head.len().min(8)).unwrap_or_default());
                    if peek.len() >= 4 {
                        magic.copy_from_slice(peek.get(..4).unwrap_or(&[0; 4]));
                        got = 4;
                    }
                    let m = u32::from_le_bytes(magic);
                    if got == 4 && m & 0xFFFF_FFF0 == 0x184D_2A50 {
                        let mut hdr = [0u8; 8];
                        r.read_exact(&mut hdr)?;
                        let len = u64::from(hdr.u32_le(4).unwrap_or(0));
                        io::copy(&mut (&mut r).take(len), &mut io::sink())?;
                        self.state = ZState::Idle(r);
                        continue;
                    }
                    let dec = StreamingDecoder::new_with_max_window_size(r, self.max_window)
                        .map_err(io::Error::other)?;
                    self.state = ZState::Frame(Box::new(dec));
                }
                ZState::Frame(mut dec) => {
                    let n = dec.read(buf)?;
                    if n > 0 {
                        self.state = ZState::Frame(dec);
                        return Ok(n);
                    }
                    self.state = ZState::Idle(dec.into_inner());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_frames_and_skippable_frames() {
        let a = ruzstd::encoding::compress_to_vec(
            &b"hello "[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let b = ruzstd::encoding::compress_to_vec(
            &b"world"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let mut data = a.clone();
        data.extend([0x50, 0x2A, 0x4D, 0x18, 3, 0, 0, 0, 1, 2, 3]);
        data.extend(&b);
        let mut out = String::new();
        ZstdFrames::new(&data[..], 1 << 27)
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, "hello world");
        // Garbage fails instead of looping.
        let mut v = Vec::new();
        assert!(
            ZstdFrames::new(&b"\x28\xB5\x2F\xFDgarbage"[..], 1 << 27)
                .read_to_end(&mut v)
                .is_err()
        );
    }

    #[test]
    fn gzip_header_fields() {
        let mut gz = vec![0x1f, 0x8b, 8, 0x08, 0x10, 0x27, 0, 0, 0, 3];
        gz.extend(b"name.txt\0");
        let h = gzip_header(&pm_core::io::MemSource::new(gz));
        assert_eq!(h.name.as_deref(), Some("name.txt"));
        assert_eq!(h.mtime, Some(10_000));
    }
}
