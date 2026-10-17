//! Audio and video: MPEG audio (MP3), RIFF (AVI, WAV, WebP), ISO base
//! media / `QuickTime` (MP4, MOV, M4A, 3GP, HEIC).

use crate::Verdict;
use crate::window::Window;

/// Frames required before headerless MPEG audio counts as a file.
const MIN_FRAMES: u32 = 8;

/// Sample rates by version index and rate index.
const SR: [[u64; 3]; 4] = [
    [11025, 12000, 8000],
    [0, 0, 0],
    [22050, 24000, 16000],
    [44100, 48000, 32000],
];
/// MPEG-1 bit rates (kbit/s) by layer (III, II, I) and index.
const BR_V1: [[u64; 15]; 3] = [
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
    ],
    [
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
    ],
];
/// MPEG-2 and 2.5 bit rates.
const BR_V2: [[u64; 15]; 3] = [
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [
        0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
    ],
];

/// Length of the MPEG audio frame whose header is `h`, if valid.
fn mpeg_frame(h: [u8; 4]) -> Option<u64> {
    let [b0, b1, b2, _] = h;
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let version = (b1 >> 3) & 3; // 0: 2.5, 2: 2, 3: 1
    let layer = (b1 >> 1) & 3; // 1: III, 2: II, 3: I
    let br_idx = usize::from(b2 >> 4);
    let sr_idx = usize::from((b2 >> 2) & 3);
    if version == 1 || layer == 0 || br_idx == 0 || br_idx == 15 || sr_idx == 3 {
        return None;
    }
    let pad = u64::from((b2 >> 1) & 1);
    let sr = *SR.get(usize::from(version))?.get(sr_idx)?;
    let table = if version == 3 { &BR_V1 } else { &BR_V2 };
    let br = *table.get(usize::from(layer) - 1)?.get(br_idx)? * 1000;
    Some(match (layer, version) {
        (3, _) => (12 * br / sr + pad) * 4,
        (1, 0 | 2) => 72 * br / sr + pad,
        _ => 144 * br / sr + pad,
    })
}

/// Walks consecutive frames from `p`; returns the end and the count.
fn frames(w: &mut Window<'_>, mut p: u64) -> (u64, u32) {
    let mut n = 0u32;
    while let Some(h) = w.array::<4>(p) {
        let Some(len) = mpeg_frame(h).filter(|&l| l >= 4) else {
            break;
        };
        if p + len > w.end() {
            break;
        }
        p += len;
        n += 1;
    }
    (p, n)
}

/// MP3 with an `ID3v2` tag, or bare MPEG audio frames; a trailing `ID3v1`
/// tag is included.
pub(crate) fn mp3(w: &mut Window<'_>, start: u64) -> Verdict {
    let mut p = start;
    let tagged = w.get(start, 3) == Some(b"ID3");
    if tagged {
        let (Some(ver), Some(flags), Some(size)) =
            (w.u8(start + 3), w.u8(start + 5), w.array::<4>(start + 6))
        else {
            return Verdict::No;
        };
        if !(2..=4).contains(&ver) || size.iter().any(|b| b & 0x80 != 0) {
            return Verdict::No;
        }
        let len = size.iter().fold(0u64, |acc, &b| (acc << 7) | u64::from(b));
        p += 10 + len + if flags & 0x10 != 0 { 10 } else { 0 };
        // Some encoders pad after the tag.
        for _ in 0..4096 {
            match w.u8(p) {
                Some(0) => p += 1,
                _ => break,
            }
        }
    }
    let (mut end, n) = frames(w, p);
    if n < if tagged { 1 } else { MIN_FRAMES } {
        return Verdict::No;
    }
    if w.get(end, 3) == Some(b"TAG") && end + 128 <= w.end() {
        end += 128;
    }
    Verdict::Found {
        len: end - start,
        ext: "mp3",
    }
}

/// RIFF: the size in the header (AVI files over 1 GB continue in `AVIX`
/// RIFF chunks, which are included).
pub(crate) fn riff(w: &mut Window<'_>, start: u64) -> Verdict {
    let (Some(size), Some(form), Some(first)) = (
        w.u32le(start + 4),
        w.array::<4>(start + 8),
        w.array::<4>(start + 12),
    ) else {
        return Verdict::No;
    };
    let ext = match &form {
        b"AVI " => "avi",
        b"WAVE" => "wav",
        b"WEBP" => "webp",
        b"RMID" => "mid",
        b"ACON" => "ani",
        b"CDR " | b"CDRA" => "cdr",
        _ => return Verdict::No,
    };
    if size < 4 || !first.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return Verdict::No;
    }
    let mut end = start + 8 + u64::from(size);
    if ext == "avi" {
        while w.get(end, 4) == Some(b"RIFF") && w.get(end + 8, 4) == Some(b"AVIX") {
            let Some(s) = w.u32le(end + 4) else { break };
            end += 8 + u64::from(s);
        }
    }
    if end > w.end() {
        return Verdict::Truncated {
            len: w.end() - start,
            ext,
        };
    }
    Verdict::Found {
        len: end - start,
        ext,
    }
}

/// Top-level boxes of ISO base media and `QuickTime` files.
fn top_level(t: [u8; 4]) -> bool {
    matches!(
        &t,
        b"ftyp"
            | b"moov"
            | b"mdat"
            | b"free"
            | b"skip"
            | b"wide"
            | b"pnot"
            | b"uuid"
            | b"meta"
            | b"moof"
            | b"mfra"
            | b"sidx"
            | b"styp"
            | b"pdin"
            | b"junk"
            | b"PICT"
            | b"prfl"
            | b"iinf"
            | b"iloc"
    )
}

/// MP4 / `QuickTime`: top-level boxes are walked while their types are
/// known; a file needs its movie (`moov`) or item (`meta`) box and data.
pub(crate) fn mp4(w: &mut Window<'_>, start: u64) -> Verdict {
    let Some(first) = w.array::<4>(start + 4) else {
        return Verdict::No;
    };
    let mut ext = "mov";
    if &first == b"ftyp" {
        let Some(brand) = w.array::<4>(start + 8) else {
            return Verdict::No;
        };
        ext = match &brand {
            b"qt  " => "mov",
            b"M4A " | b"M4B " => "m4a",
            b"M4V " | b"M4VH" | b"M4VP" => "m4v",
            b"heic" | b"heix" | b"mif1" | b"msf1" | b"hevc" => "heic",
            b"avif" | b"avis" => "avif",
            b"crx " => "cr3",
            [b'3', b'g', ..] => "3gp",
            b if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') => "mp4",
            _ => return Verdict::No,
        };
    }
    let mut p = start;
    let (mut movie, mut data) = (false, false);
    let mut boxes = 0u32;
    while let (Some(size), Some(ty)) = (w.u32be(p), w.array::<4>(p + 4)) {
        if !top_level(ty) {
            break;
        }
        let len = match size {
            0 => break,
            1 => match w.u64be(p + 8) {
                Some(l) if l >= 16 => l,
                _ => break,
            },
            s if s < 8 => break,
            s => u64::from(s),
        };
        match &ty {
            b"moov" | b"meta" | b"moof" => movie = true,
            b"mdat" => data = true,
            _ => {}
        }
        boxes += 1;
        let Some(next) = p.checked_add(len) else {
            break;
        };
        if next > w.end() {
            return if boxes > 1 {
                Verdict::Truncated {
                    len: w.end() - start,
                    ext,
                }
            } else {
                Verdict::No
            };
        }
        p = next;
    }
    if boxes == 0 || (&first != b"ftyp" && boxes < 2) {
        return Verdict::No;
    }
    if movie && (data || ext == "heic" || ext == "avif") {
        Verdict::Found {
            len: p - start,
            ext,
        }
    } else {
        Verdict::Truncated {
            len: p - start,
            ext,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_lengths() {
        // MPEG-1 layer III, 128 kbit/s, 44.1 kHz, no padding: 417 bytes.
        assert_eq!(mpeg_frame([0xFF, 0xFB, 0x90, 0x00]), Some(417));
        // With padding: 418.
        assert_eq!(mpeg_frame([0xFF, 0xFB, 0x92, 0x00]), Some(418));
        // MPEG-2 layer III, 64 kbit/s, 22.05 kHz: 72*64000/22050 = 208.
        assert_eq!(mpeg_frame([0xFF, 0xF3, 0x80, 0x00]), Some(208));
        // Reserved version, bad bitrate, bad sample rate.
        assert_eq!(mpeg_frame([0xFF, 0xEB, 0x90, 0x00]), None);
        assert_eq!(mpeg_frame([0xFF, 0xFB, 0xF0, 0x00]), None);
        assert_eq!(mpeg_frame([0xFF, 0xFB, 0x9C, 0x00]), None);
    }
}
