//! Primitives for password verification: RC4, hashes and AES block
//! operations (only ever used to *check* a password, never to protect
//! data).

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha384, Sha512};

/// RC4 keystream applied to `data` (encryption and decryption are the
/// same). An empty key is treated as a single zero byte.
#[allow(
    clippy::indexing_slicing,
    reason = "u8 and 0..256 indices into a 256-entry table"
)]
pub(crate) fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let key: &[u8] = if key.is_empty() { &[0] } else { key };
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        let k = key.get(i % key.len()).copied().unwrap_or(0);
        j = j.wrapping_add(s[i]).wrapping_add(k);
        s.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[usize::from(i)]);
            s.swap(usize::from(i), usize::from(j));
            let t = s[usize::from(i)].wrapping_add(s[usize::from(j)]);
            b ^ s[usize::from(t)]
        })
        .collect()
}

/// Hash algorithms used by the Office and PDF password schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hash {
    Md5,
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// The digest of the concatenated parts.
    pub(crate) fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut d = D::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        match self {
            Self::Md5 => run::<Md5>(parts),
            Self::Sha1 => run::<Sha1>(parts),
            Self::Sha256 => run::<Sha256>(parts),
            Self::Sha384 => run::<Sha384>(parts),
            Self::Sha512 => run::<Sha512>(parts),
        }
    }
}

/// UTF-16LE bytes of a password (`StringUtil.getToUnicodeLE`).
pub(crate) fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// AES-ECB decryption of whole 16-byte blocks (a trailing partial block
/// is dropped); `None` for an invalid key length.
pub(crate) fn aes_ecb_decrypt(key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    macro_rules! run {
        ($t:ty) => {{
            let c = <$t>::new_from_slice(key).ok()?;
            let mut out = Vec::with_capacity(data.len());
            for chunk in data.chunks_exact(16) {
                let mut b = aes::Block::default();
                b.copy_from_slice(chunk);
                c.decrypt_block(&mut b);
                out.extend_from_slice(&b);
            }
            Some(out)
        }};
    }
    match key.len() {
        16 => run!(aes::Aes128),
        24 => run!(aes::Aes192),
        32 => run!(aes::Aes256),
        _ => None,
    }
}

/// AES-128-CBC encryption without padding (whole blocks only).
pub(crate) fn aes128_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let c = aes::Aes128::new_from_slice(key).ok()?;
    let mut prev = aes::Block::default();
    if iv.len() != 16 {
        return None;
    }
    prev.copy_from_slice(iv);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut b = aes::Block::default();
        for (d, (x, p)) in b.iter_mut().zip(chunk.iter().zip(prev.iter())) {
            *d = x ^ p;
        }
        c.encrypt_block(&mut b);
        out.extend_from_slice(&b);
        prev = b;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_vectors() {
        // RFC 6229 / Wikipedia test vectors.
        assert_eq!(
            rc4(b"Key", b"Plaintext"),
            [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
        assert_eq!(rc4(b"Wiki", b"pedia"), [0x10, 0x21, 0xBF, 0x04, 0x20]);
        let c = rc4(b"Secret", b"Attack at dawn");
        assert_eq!(rc4(b"Secret", &c), b"Attack at dawn");
    }

    #[test]
    fn hashes() {
        assert_eq!(
            Hash::Md5.digest(&[b"a", b"bc"]),
            Hash::Md5.digest(&[b"abc"])
        );
        assert_eq!(Hash::Sha1.digest(&[]).len(), 20);
        assert_eq!(Hash::Sha384.digest(&[b"x"]).len(), 48);
        assert_eq!(utf16le("Ab"), [b'A', 0, b'b', 0]);
    }

    #[test]
    fn aes_fips197() {
        // FIPS-197 C.1: AES-128.
        let key: Vec<u8> = (0u8..16).collect();
        let pt = [
            0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        let ct = aes128_cbc_encrypt(&key, &[0; 16], &pt).unwrap();
        assert_eq!(
            ct,
            [
                0x69, 0xC4, 0xE0, 0xD8, 0x6A, 0x7B, 0x04, 0x30, 0xD8, 0xCD, 0xB7, 0x80, 0x70, 0xB4,
                0xC5, 0x5A
            ]
        );
        assert_eq!(aes_ecb_decrypt(&key, &ct).unwrap(), pt);
        assert!(aes_ecb_decrypt(&[0; 5], &ct).is_none());
    }
}
