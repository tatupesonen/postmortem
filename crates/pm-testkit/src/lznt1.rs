//! Reference LZNT1 compressor (greedy, brute-force matching) for building
//! compressed NTFS fixtures and round-trip tests. Not fast; test use only.

const CHUNK: usize = 4096;

/// Compresses `input` into an LZNT1 stream (terminated by a zero header).
pub fn compress(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in input.chunks(CHUNK) {
        let mut body = Vec::new();
        let mut pos = 0;
        while pos < chunk.len() {
            let flag_at = body.len();
            body.push(0u8);
            for bit in 0..8 {
                if pos >= chunk.len() {
                    break;
                }
                let (mut len_mask, mut off_shift) = (0x0FFFusize, 12u32);
                if pos > 0 {
                    let mut p = pos - 1;
                    while p >= 0x10 {
                        len_mask >>= 1;
                        off_shift -= 1;
                        p >>= 1;
                    }
                }
                let max_len = len_mask + 3;
                let max_off = 1usize << (16 - off_shift);
                let (mut best_len, mut best_off) = (0, 0);
                for off in 1..=pos.min(max_off) {
                    let mut l = 0;
                    while l < max_len
                        && pos + l < chunk.len()
                        && chunk[pos + l] == chunk[pos + l - off]
                    {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_off = off;
                    }
                }
                if best_len >= 3 {
                    body[flag_at] |= 1 << bit;
                    let token = (((best_off - 1) << off_shift) | (best_len - 3)) as u16;
                    body.extend_from_slice(&token.to_le_bytes());
                    pos += best_len;
                } else {
                    body.push(chunk[pos]);
                    pos += 1;
                }
            }
        }
        if body.len() < chunk.len() {
            let hdr = 0xB000u16 | (body.len() as u16 - 1);
            out.extend_from_slice(&hdr.to_le_bytes());
            out.extend_from_slice(&body);
        } else {
            let hdr = 0x3000u16 | (chunk.len() as u16 - 1);
            out.extend_from_slice(&hdr.to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}
