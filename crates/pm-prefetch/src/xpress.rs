//! LZXPRESS Huffman decompression (MS-XCA §2.2.4), used by Windows 10+
//! compressed prefetch files (`MAM\x04`).
//!
//! The input is a series of blocks, each producing up to 64 KiB of
//! output: a 256-byte table of 512 four-bit code lengths, then a bit
//! stream read as little-endian 16-bit words (literals 0–255, match
//! symbols 256–511 carrying a length nibble and the bit length of the
//! offset; long lengths continue in extra bytes).

use crate::{Error, Result};

const TABLE_BITS: u32 = 15;
const BLOCK: usize = 65_536;

/// Canonical Huffman decoding table for one block.
struct Table {
    /// Next 15 bits → symbol (`u16::MAX` = no code).
    lookup: Vec<u16>,
    lengths: [u8; 512],
}

impl Table {
    fn build(raw: &[u8]) -> Result<Self> {
        let mut lengths = [0u8; 512];
        for (i, &b) in raw.iter().take(256).enumerate() {
            if let Some(l) = lengths.get_mut(2 * i) {
                *l = b & 0x0F;
            }
            if let Some(l) = lengths.get_mut(2 * i + 1) {
                *l = b >> 4;
            }
        }
        let mut lookup = vec![u16::MAX; 1 << TABLE_BITS];
        let mut code: u32 = 0;
        for len in 1..=TABLE_BITS {
            for (sym, _) in lengths
                .iter()
                .enumerate()
                .filter(|(_, l)| u32::from(**l) == len)
            {
                let span = 1u32 << (TABLE_BITS - len);
                let start = code << (TABLE_BITS - len);
                let slots = lookup
                    .get_mut(start as usize..(start + span) as usize)
                    .ok_or(Error::Corrupt("Huffman table oversubscribed"))?;
                slots.fill(sym as u16);
                code += 1;
            }
            code <<= 1;
        }
        Ok(Self { lookup, lengths })
    }
}

/// Bit reader with the spec's 32-bit window and two-word look-ahead.
struct Bits<'a> {
    input: &'a [u8],
    pos: usize,
    window: u32,
    extra: i32,
}

impl<'a> Bits<'a> {
    fn new(input: &'a [u8], pos: usize) -> Self {
        let mut b = Self {
            input,
            pos,
            window: 0,
            extra: 16,
        };
        b.window = (u32::from(b.word()) << 16) | u32::from(b.word());
        b
    }

    /// The next 16-bit word (zero past the end, which only matters for
    /// look-ahead the encoder padded anyway).
    fn word(&mut self) -> u16 {
        let w = self
            .input
            .get(self.pos..self.pos + 2)
            .and_then(|b| b.try_into().ok())
            .map_or(0, u16::from_le_bytes);
        self.pos += 2;
        w
    }

    fn peek(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.window >> (32 - n) }
    }

    fn consume(&mut self, n: u32) {
        if n == 0 {
            return;
        }
        self.window = if n >= 32 { 0 } else { self.window << n };
        self.extra -= n as i32;
        if self.extra < 0 {
            let w = u32::from(self.word());
            self.window |= w << (-self.extra) as u32;
            self.extra += 16;
        }
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .input
            .get(self.pos)
            .ok_or(Error::Corrupt("compressed data truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16> {
        let lo = self.byte()?;
        let hi = self.byte()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
}

/// Decompresses `input` into exactly `out_len` bytes.
pub fn decompress(input: &[u8], out_len: usize) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(out_len.min(64 << 20));
    let mut in_pos = 0usize;
    while out.len() < out_len {
        let raw = input
            .get(in_pos..in_pos + 256)
            .ok_or(Error::Corrupt("compressed data truncated"))?;
        let table = Table::build(raw)?;
        let mut bits = Bits::new(input, in_pos + 256);
        let block_end = (out.len() + BLOCK).min(out_len);
        while out.len() < block_end {
            let sym = *table
                .lookup
                .get(bits.peek(TABLE_BITS) as usize)
                .unwrap_or(&u16::MAX);
            if sym == u16::MAX {
                return Err(Error::Corrupt("invalid Huffman code"));
            }
            let len = u32::from(*table.lengths.get(usize::from(sym)).unwrap_or(&0));
            bits.consume(len);
            if sym < 256 {
                out.push(sym as u8);
                continue;
            }
            let s = usize::from(sym - 256);
            let mut match_len = s & 0x0F;
            let offset_bits = (s >> 4) as u32;
            if match_len == 15 {
                match_len = usize::from(bits.byte()?);
                if match_len == 255 {
                    match_len = usize::from(bits.u16()?);
                    match_len = match_len
                        .checked_sub(15)
                        .ok_or(Error::Corrupt("bad match length"))?;
                }
                match_len += 15;
            }
            match_len += 3;
            let offset = (bits.peek(offset_bits) as usize) + (1usize << offset_bits);
            bits.consume(offset_bits);
            let start = out
                .len()
                .checked_sub(offset)
                .ok_or(Error::Corrupt("match before start of output"))?;
            for i in 0..match_len {
                if out.len() >= out_len {
                    break;
                }
                let b = *out.get(start + i).ok_or(Error::Corrupt("bad match"))?;
                out.push(b);
            }
        }
        in_pos = bits.pos;
    }
    Ok(out)
}
