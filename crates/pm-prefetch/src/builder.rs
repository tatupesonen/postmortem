//! Prefetch writer and an LZXPRESS Huffman encoder (tests, fuzz seeds).
//!
//! The encoder uses a flat code (every symbol 9 bits) and greedy LZ77
//! matching; it lays the stream out in the order the decoder reads it
//! (16-bit words interleaved with extra length bytes), so it exercises
//! every decoder path: literals, short and long matches, multi-block
//! input.

#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test-data writer; sizes are small and controlled"
)]

use std::collections::HashMap;

/// Builds an uncompressed prefetch file.
#[derive(Debug, Clone)]
pub struct PrefetchBuilder {
    pub version: u32,
    pub executable: String,
    pub hash: u32,
    pub run_count: u32,
    /// FILETIMEs, most recent first (only the first is kept for 17/23).
    pub run_times: Vec<u64>,
    pub filenames: Vec<String>,
    /// (device path, creation FILETIME, serial, directories).
    pub volumes: Vec<(String, u64, u32, Vec<String>)>,
    /// Version 30's shorter file-information layout.
    pub v30_short: bool,
}

impl PrefetchBuilder {
    /// Serialises the file.
    pub fn build(&self) -> Vec<u8> {
        let (last_run, run_count_at, info_end, vol_entry) = match self.version {
            17 => (0x78, 0x90, 0x98, 40),
            23 => (0x80, 0x98, 0xF0, 104),
            26 => (0x80, 0xD0, 0x130, 104),
            _ if self.v30_short => (0x80, 0xC8, 0x128, 96),
            _ => (0x80, 0xD0, 0x130, 96),
        };
        let mut d = vec![0u8; info_end];
        d[..4].copy_from_slice(&self.version.to_le_bytes());
        d[4..8].copy_from_slice(b"SCCA");
        d[8..12].copy_from_slice(&(if self.version == 17 { 0x0F_u32 } else { 0x11 }).to_le_bytes());
        for (i, u) in self.executable.encode_utf16().take(29).enumerate() {
            d[0x10 + i * 2..0x12 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        d[0x4C..0x50].copy_from_slice(&self.hash.to_le_bytes());
        d[0x54..0x58].copy_from_slice(&(info_end as u32).to_le_bytes());
        let slots = if self.version >= 26 { 8 } else { 1 };
        for (i, t) in self.run_times.iter().take(slots).enumerate() {
            d[last_run + i * 8..last_run + i * 8 + 8].copy_from_slice(&t.to_le_bytes());
        }
        d[run_count_at..run_count_at + 4].copy_from_slice(&self.run_count.to_le_bytes());
        // File names.
        let names_off = d.len();
        for f in &self.filenames {
            for u in f.encode_utf16() {
                d.extend(u.to_le_bytes());
            }
            d.extend([0, 0]);
        }
        let names_len = d.len() - names_off;
        d[0x64..0x68].copy_from_slice(&(names_off as u32).to_le_bytes());
        d[0x68..0x6C].copy_from_slice(&(names_len as u32).to_le_bytes());
        while d.len() % 8 != 0 {
            d.push(0);
        }
        // Volumes: entries, then device paths and directory strings.
        let vol_off = d.len();
        let mut vols = vec![0u8; vol_entry * self.volumes.len()];
        for (i, (dev, created, serial, dirs)) in self.volumes.iter().enumerate() {
            let path_off = vols.len() as u32;
            let u: Vec<u16> = dev.encode_utf16().collect();
            for c in &u {
                vols.extend(c.to_le_bytes());
            }
            vols.extend([0, 0]);
            let dir_off = vols.len() as u32;
            for dir in dirs {
                let du: Vec<u16> = dir.encode_utf16().collect();
                vols.extend((du.len() as u16).to_le_bytes());
                for c in du {
                    vols.extend(c.to_le_bytes());
                }
                vols.extend([0, 0]);
            }
            let e = &mut vols[i * vol_entry..(i + 1) * vol_entry];
            e[0..4].copy_from_slice(&path_off.to_le_bytes());
            e[4..8].copy_from_slice(&(u.len() as u32).to_le_bytes());
            e[8..16].copy_from_slice(&created.to_le_bytes());
            e[16..20].copy_from_slice(&serial.to_le_bytes());
            e[28..32].copy_from_slice(&dir_off.to_le_bytes());
            e[32..36].copy_from_slice(&(dirs.len() as u32).to_le_bytes());
        }
        d[0x6C..0x70].copy_from_slice(&(vol_off as u32).to_le_bytes());
        d[0x70..0x74].copy_from_slice(&(self.volumes.len() as u32).to_le_bytes());
        d[0x74..0x78].copy_from_slice(&(vols.len() as u32).to_le_bytes());
        d.extend(vols);
        let n = d.len() as u32;
        d[12..16].copy_from_slice(&n.to_le_bytes());
        d
    }
}

/// Wraps `data` in a `MAM\x04` container (LZXPRESS Huffman); `crc`
/// selects the variant with the checksum field (`MAM\x84`).
pub fn compress_mam(data: &[u8], crc: bool) -> Vec<u8> {
    let mut out = b"MAM".to_vec();
    out.push(if crc { 0x84 } else { 0x04 });
    out.extend((data.len() as u32).to_le_bytes());
    if crc {
        out.extend(0u32.to_le_bytes());
    }
    out.extend(xpress_compress(data));
    out
}

enum Token {
    Literal(u8),
    Match { len: usize, offset: usize },
}

enum Item {
    Word(usize),
    Byte(u8),
}

/// LZXPRESS Huffman compression with a flat 9-bit code.
pub fn xpress_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chains: HashMap<[u8; 3], Vec<usize>> = HashMap::new();
    let mut pos = 0;
    while pos < data.len() || (data.is_empty() && out.is_empty()) {
        let block_end = (pos + 65_536).min(data.len());
        // Tokens of this block.
        let mut tokens = Vec::new();
        while pos < block_end {
            let mut best = (0usize, 0usize);
            if pos + 3 <= block_end {
                let key = [data[pos], data[pos + 1], data[pos + 2]];
                if let Some(cands) = chains.get(&key) {
                    for &c in cands.iter().rev().take(16) {
                        let offset = pos - c;
                        if offset > 65_535 {
                            break;
                        }
                        let max = (block_end - pos).min(65_538);
                        let mut l = 0;
                        while l < max && data[c + l] == data[pos + l] {
                            l += 1;
                        }
                        if l > best.0 {
                            best = (l, offset);
                        }
                    }
                }
            }
            let step = if best.0 >= 3 {
                tokens.push(Token::Match {
                    len: best.0,
                    offset: best.1,
                });
                best.0
            } else {
                tokens.push(Token::Literal(data[pos]));
                1
            };
            for p in pos..pos + step {
                if p + 3 <= data.len() {
                    chains
                        .entry([data[p], data[p + 1], data[p + 2]])
                        .or_default()
                        .push(p);
                }
            }
            pos += step;
        }
        encode_block(&tokens, &mut out);
        if data.is_empty() {
            break;
        }
    }
    out
}

fn encode_block(tokens: &[Token], out: &mut Vec<u8>) {
    out.extend([0x99u8; 256]);
    let mut bits: Vec<bool> = Vec::new();
    let mut items = vec![Item::Word(0), Item::Word(1)];
    let mut words = 2usize;
    let mut extra: i32 = 16;
    let mut consume = |n: u32, items: &mut Vec<Item>| {
        extra -= n as i32;
        if extra < 0 {
            items.push(Item::Word(words));
            words += 1;
            extra += 16;
        }
    };
    let push_bits = |bits: &mut Vec<bool>, v: u32, n: u32| {
        for i in (0..n).rev() {
            bits.push((v >> i) & 1 == 1);
        }
    };
    for t in tokens {
        match *t {
            Token::Literal(b) => {
                push_bits(&mut bits, u32::from(b), 9);
                consume(9, &mut items);
            }
            Token::Match { len, offset } => {
                let ob = usize::BITS - 1 - offset.leading_zeros();
                let l = len - 3;
                let sym = 256 + ((ob as usize) << 4) + l.min(15);
                push_bits(&mut bits, sym as u32, 9);
                consume(9, &mut items);
                if l >= 15 {
                    if l - 15 < 255 {
                        items.push(Item::Byte((l - 15) as u8));
                    } else {
                        items.push(Item::Byte(255));
                        let v = l as u16;
                        items.push(Item::Byte(v.to_le_bytes()[0]));
                        items.push(Item::Byte(v.to_le_bytes()[1]));
                    }
                }
                push_bits(&mut bits, (offset - (1 << ob)) as u32, ob);
                consume(ob, &mut items);
            }
        }
    }
    bits.resize(words * 16, false);
    for it in items {
        match it {
            Item::Word(k) => {
                let mut w = 0u16;
                for b in &bits[k * 16..k * 16 + 16] {
                    w = (w << 1) | u16::from(*b);
                }
                out.extend(w.to_le_bytes());
            }
            Item::Byte(b) => out.push(b),
        }
    }
}
