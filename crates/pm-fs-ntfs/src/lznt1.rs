//! LZNT1 decompression (NTFS compressed attributes).
//!
//! A compression unit is a sequence of chunks, each decompressing to at most
//! 4096 bytes. A chunk starts with a 16-bit header: bit 15 = compressed,
//! bits 0–11 = stored length − 3 (header excluded). A zero header ends the
//! stream. Compressed chunks are groups of a flag byte followed by eight
//! tokens: literal bytes (flag bit 0) or 16-bit back-references (bit 1)
//! whose length/offset split depends on the position within the chunk.

const CHUNK: usize = 4096;

/// Decompresses `input` into `out` (appending), stopping once `limit` bytes
/// have been produced or the stream ends. Corrupt input stops early instead
/// of failing: forensic readers return what can be recovered.
pub(crate) fn decompress(input: &[u8], out: &mut Vec<u8>, limit: usize) {
    let mut pos = 0usize;
    while out.len() < limit {
        let Some(hdr) = input
            .get(pos..pos + 2)
            .and_then(|h| h.try_into().ok())
            .map(u16::from_le_bytes)
        else {
            break;
        };
        if hdr == 0 {
            break;
        }
        let stored = usize::from(hdr & 0x0FFF) + 1;
        pos += 2;
        let Some(data) = input.get(pos..(pos + stored).min(input.len())) else {
            break;
        };
        pos += stored;
        let chunk_start = out.len();
        if hdr & 0x8000 == 0 {
            out.extend_from_slice(data);
        } else {
            decompress_chunk(data, out, chunk_start);
        }
        // Every chunk except the last represents a full 4 KiB of output.
        let produced = out.len() - chunk_start;
        if produced < CHUNK && pos < input.len() && input.get(pos..pos + 2) != Some(&[0, 0]) {
            out.resize(chunk_start + CHUNK, 0);
        }
    }
    out.truncate(limit);
}

fn decompress_chunk(data: &[u8], out: &mut Vec<u8>, chunk_start: usize) {
    let mut i = 0usize;
    while i < data.len() {
        let Some(&flags) = data.get(i) else { return };
        i += 1;
        for bit in 0..8 {
            if i >= data.len() {
                return;
            }
            let written = out.len() - chunk_start;
            if written >= CHUNK {
                return;
            }
            if flags & (1 << bit) == 0 {
                let Some(&lit) = data.get(i) else { return };
                out.push(lit);
                i += 1;
            } else {
                let Some(token) = data
                    .get(i..i + 2)
                    .and_then(|t| t.try_into().ok())
                    .map(u16::from_le_bytes)
                else {
                    return;
                };
                i += 2;
                if written == 0 {
                    return; // a back-reference cannot precede any output
                }
                let (mut len_mask, mut off_shift) = (0x0FFFu16, 12u32);
                let mut p = written - 1;
                while p >= 0x10 {
                    len_mask >>= 1;
                    off_shift -= 1;
                    p >>= 1;
                }
                let length = usize::from(token & len_mask) + 3;
                let offset = usize::from(token >> off_shift) + 1;
                if offset > written {
                    return;
                }
                for _ in 0..length.min(CHUNK - written) {
                    let Some(&b) = out.get(out.len() - offset) else {
                        return;
                    };
                    out.push(b);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_testkit::lznt1::compress;
    use proptest::prelude::*;

    #[test]
    fn known_vector() {
        // "abcabcabcabc" → literals a,b,c + back-reference (offset 3, len 9).
        let mut out = Vec::new();
        let token: u16 = (2 << 12) | (9 - 3);
        let mut chunk = vec![0b0000_1000u8, b'a', b'b', b'c'];
        chunk.extend_from_slice(&token.to_le_bytes());
        let mut input = (0xB000u16 | (chunk.len() as u16 - 1))
            .to_le_bytes()
            .to_vec();
        input.extend_from_slice(&chunk);
        decompress(&input, &mut out, 4096);
        assert_eq!(out, b"abcabcabcabc");
    }

    #[test]
    fn uncompressed_chunk_passthrough() {
        let data = vec![7u8; 4096];
        let mut input = (0x3000u16 | 4095).to_le_bytes().to_vec();
        input.extend_from_slice(&data);
        let mut out = Vec::new();
        decompress(&input, &mut out, 8192);
        assert_eq!(out, data);
    }

    proptest! {
        #[test]
        fn roundtrip(data in proptest::collection::vec(prop_oneof![Just(b'a'), Just(b'b'), any::<u8>()], 0..10_000)) {
            let c = compress(&data);
            let mut out = Vec::new();
            decompress(&c, &mut out, data.len());
            prop_assert_eq!(out, data);
        }

        #[test]
        fn garbage_never_panics(data in proptest::collection::vec(any::<u8>(), 0..9000)) {
            let mut out = Vec::new();
            decompress(&data, &mut out, 65_536);
            prop_assert!(out.len() <= 65_536);
        }
    }
}
