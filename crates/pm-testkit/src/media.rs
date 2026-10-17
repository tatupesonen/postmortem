//! Minimal but structurally valid media and binary files for carving
//! tests: MP3, WAV, AVI, MP4/MOV, Windows PE, EVTX and TIFF.

use std::io::Cursor;

/// MPEG-1 layer III frames (128 kbit/s, 44.1 kHz, 417 bytes each),
/// optionally behind an ID3v2 tag and followed by an ID3v1 tag.
pub fn mp3(frames: usize, id3: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if id3 {
        out.extend(b"ID3\x03\x00\x00");
        // Syncsafe size 20 and 20 bytes of tag data.
        out.extend([0, 0, 0, 20]);
        out.extend(b"TIT2\x00\x00\x00\x06\x00\x00\x00Song");
        out.resize(30, 0);
    }
    for i in 0..frames {
        out.extend([0xFF, 0xFB, 0x90, 0x00]);
        out.extend((0..413).map(|k| ((i * 7 + k) % 251) as u8));
    }
    if id3 {
        let mut tag = b"TAG".to_vec();
        tag.resize(128, b' ');
        out.extend(tag);
    }
    out
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn riff(form: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = form.iter().copied().chain(chunks.concat()).collect();
    chunk(b"RIFF", &body)
}

/// A 16-bit mono PCM WAV file of `samples` samples.
pub fn wav(samples: usize) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend(1u16.to_le_bytes()); // PCM
    fmt.extend(1u16.to_le_bytes()); // mono
    fmt.extend(8000u32.to_le_bytes());
    fmt.extend(16000u32.to_le_bytes());
    fmt.extend(2u16.to_le_bytes());
    fmt.extend(16u16.to_le_bytes());
    let data: Vec<u8> = (0..samples)
        .flat_map(|i| ((i as i16).wrapping_mul(97)).to_le_bytes())
        .collect();
    riff(b"WAVE", &[chunk(b"fmt ", &fmt), chunk(b"data", &data)])
}

/// A small AVI: header list, one movie chunk, index.
pub fn avi(frame_len: usize) -> Vec<u8> {
    let mut avih = vec![0u8; 56];
    avih[0..4].copy_from_slice(&40_000u32.to_le_bytes());
    let hdrl = [b"hdrl".to_vec(), chunk(b"avih", &avih)].concat();
    let frame: Vec<u8> = (0..frame_len).map(|i| (i % 200) as u8).collect();
    let movi = [b"movi".to_vec(), chunk(b"00dc", &frame)].concat();
    let idx = vec![0u8; 16];
    riff(
        b"AVI ",
        &[
            chunk(b"LIST", &hdrl),
            chunk(b"LIST", &movi),
            chunk(b"idx1", &idx),
        ],
    )
}

fn mp4_box(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = ((data.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend(ty);
    v.extend(data);
    v
}

/// An ISO base media file: `ftyp` (with `brand`), `moov` (an `mvhd`) and
/// `mdat` of `data_len` bytes.
pub fn mp4(brand: &[u8; 4], data_len: usize) -> Vec<u8> {
    let mut ftyp = brand.to_vec();
    ftyp.extend(0u32.to_be_bytes());
    ftyp.extend(brand);
    ftyp.extend(b"isom");
    let mvhd = mp4_box(b"mvhd", &[0u8; 100]);
    let data: Vec<u8> = (0..data_len).map(|i| (i % 253) as u8).collect();
    [
        mp4_box(b"ftyp", &ftyp),
        mp4_box(b"moov", &mvhd),
        mp4_box(b"mdat", &data),
    ]
    .concat()
}

/// A minimal PE32 image with two sections of `section_len` bytes each
/// (file alignment 512) and, if `dll`, the DLL characteristic.
pub fn pe(dll: bool, section_len: u32) -> Vec<u8> {
    let raw = section_len.next_multiple_of(512);
    let headers = 0x400u32;
    let total = (headers + 2 * raw) as usize;
    let mut v = vec![0u8; total];
    v[0..2].copy_from_slice(b"MZ");
    v[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    v[0x80..0x84].copy_from_slice(b"PE\0\0");
    let coff = 0x84;
    v[coff..coff + 2].copy_from_slice(&0x14Cu16.to_le_bytes()); // i386
    v[coff + 2..coff + 4].copy_from_slice(&2u16.to_le_bytes());
    v[coff + 16..coff + 18].copy_from_slice(&224u16.to_le_bytes());
    let chars: u16 = 0x0102 | if dll { 0x2000 } else { 0 };
    v[coff + 18..coff + 20].copy_from_slice(&chars.to_le_bytes());
    let opt = coff + 20;
    v[opt..opt + 2].copy_from_slice(&0x10Bu16.to_le_bytes());
    v[opt + 32..opt + 36].copy_from_slice(&0x1000u32.to_le_bytes()); // section alignment
    v[opt + 36..opt + 40].copy_from_slice(&0x200u32.to_le_bytes()); // file alignment
    v[opt + 60..opt + 64].copy_from_slice(&headers.to_le_bytes());
    v[opt + 92..opt + 96].copy_from_slice(&16u32.to_le_bytes()); // data directories
    let table = opt + 224;
    for (i, name) in [b".text\0\0\0", b".data\0\0\0"].iter().enumerate() {
        let s = table + 40 * i;
        v[s..s + 8].copy_from_slice(*name);
        v[s + 8..s + 12].copy_from_slice(&section_len.to_le_bytes());
        v[s + 12..s + 16].copy_from_slice(&(0x1000 * (i as u32 + 1)).to_le_bytes());
        v[s + 16..s + 20].copy_from_slice(&raw.to_le_bytes());
        v[s + 20..s + 24].copy_from_slice(&(headers + raw * i as u32).to_le_bytes());
    }
    for (i, b) in v.iter_mut().enumerate().skip(headers as usize) {
        *b = (i % 241) as u8;
    }
    v
}

/// An EVTX file: header block and `chunks` 64 KiB chunks.
pub fn evtx(chunks: u16) -> Vec<u8> {
    let mut v = vec![0u8; 4096 + 65_536 * usize::from(chunks)];
    v[0..8].copy_from_slice(b"ElfFile\0");
    v[16..24].copy_from_slice(&u64::from(chunks.saturating_sub(1)).to_le_bytes());
    v[32..36].copy_from_slice(&128u32.to_le_bytes());
    v[36..38].copy_from_slice(&1u16.to_le_bytes());
    v[38..40].copy_from_slice(&3u16.to_le_bytes());
    v[40..42].copy_from_slice(&4096u16.to_le_bytes());
    v[42..44].copy_from_slice(&chunks.to_le_bytes());
    for c in 0..usize::from(chunks) {
        let at = 4096 + c * 65_536;
        v[at..at + 8].copy_from_slice(b"ElfChnk\0");
        for (k, b) in v[at + 512..at + 65_536].iter_mut().enumerate() {
            *b = ((k + c) % 199) as u8;
        }
    }
    v
}

/// A small RGB TIFF image.
pub fn tiff(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 7]));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Tiff).unwrap();
    out.into_inner()
}
