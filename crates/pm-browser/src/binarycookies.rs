//! Safari `Cookies.binarycookies` (Autopsy `BinaryCookieReader`).
//!
//! Layout: big-endian header (`cook`, page count, one size per page),
//! then the pages back to back. A page starts with `00 00 01 00`, then
//! (little-endian) the cookie count, one offset per cookie and a zero
//! word. A cookie record (little-endian) holds its size, flags, the
//! offsets of URL, name, path and value (from the record start), and
//! expiry/creation times as Cocoa seconds; strings are NUL-terminated.
//! Records are read at their offsets (Autopsy reads them sequentially).

use crate::time::cocoa;

/// One cookie.
#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub url: String,
    pub name: String,
    pub path: String,
    pub value: String,
    pub expiration: Option<i64>,
    pub creation: Option<i64>,
}

/// Parse failures of the file header (pages that fail are skipped).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("not a binary cookies file")]
    NotCookies,
    #[error("truncated header")]
    Truncated,
}

const MAX_PAGES: usize = 1 << 20;

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        d.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        d.get(o..o.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn le_f64(d: &[u8], o: usize) -> Option<f64> {
    Some(f64::from_le_bytes(
        d.get(o..o.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn cstr(d: &[u8], o: usize) -> String {
    let s = d.get(o..).unwrap_or_default();
    let n = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    String::from_utf8_lossy(s.get(..n).unwrap_or_default()).into_owned()
}

fn cookie(rec: &[u8]) -> Option<Cookie> {
    let off = |i: usize| le32(rec, i).and_then(|v| usize::try_from(v).ok());
    let (url, name, path, value) = (off(16)?, off(20)?, off(24)?, off(28)?);
    Some(Cookie {
        url: cstr(rec, url),
        name: cstr(rec, name),
        path: cstr(rec, path),
        value: cstr(rec, value),
        expiration: le_f64(rec, 40).and_then(cocoa),
        creation: le_f64(rec, 48).and_then(cocoa),
    })
}

fn page(p: &[u8], out: &mut Vec<Cookie>) {
    if be32(p, 0) != Some(0x100) {
        return;
    }
    let Some(count) = le32(p, 4).and_then(|c| usize::try_from(c).ok()) else {
        return;
    };
    // Each offset takes 4 bytes; a larger count cannot be real.
    let count = count.min(p.len() / 4);
    for i in 0..count {
        let Some(o) = le32(p, 8 + i * 4).and_then(|v| usize::try_from(v).ok()) else {
            break;
        };
        let Some(size) = le32(p, o).and_then(|v| usize::try_from(v).ok()) else {
            continue;
        };
        let Some(rec) = o.checked_add(size).and_then(|end| p.get(o..end)) else {
            continue;
        };
        if let Some(c) = cookie(rec) {
            out.push(c);
        }
    }
}

/// Every cookie of a `Cookies.binarycookies` file.
pub fn parse(data: &[u8]) -> Result<Vec<Cookie>, Error> {
    if data.get(..4) != Some(b"cook".as_slice()) {
        return Err(Error::NotCookies);
    }
    let pages = be32(data, 4).ok_or(Error::Truncated)? as usize;
    let pages = pages.min(MAX_PAGES);
    let mut sizes = Vec::with_capacity(pages.min(data.len() / 4));
    for i in 0..pages {
        sizes.push(be32(data, 8 + i * 4).ok_or(Error::Truncated)? as usize);
    }
    let mut pos = 8 + pages * 4;
    let mut out = Vec::new();
    for size in sizes {
        let Some(end) = pos.checked_add(size) else {
            break;
        };
        let Some(p) = data.get(pos..end) else {
            // Short page: parse what is there.
            if let Some(p) = data.get(pos..) {
                page(p, &mut out);
            }
            break;
        };
        page(p, &mut out);
        pos = end;
    }
    Ok(out)
}

/// Builds `Cookies.binarycookies` files (tests, fuzz seeds).
#[cfg(feature = "testing")]
#[allow(
    clippy::indexing_slicing,
    clippy::missing_panics_doc,
    reason = "fixture builder for tests and fuzz seeds"
)]
pub mod testing {
    /// `(url, name, path, value, expiration, creation)` with Cocoa times.
    pub type Spec<'a> = (&'a str, &'a str, &'a str, &'a str, f64, f64);

    fn record(c: &Spec<'_>) -> Vec<u8> {
        let mut strings = Vec::new();
        let mut offs = [0u32; 4];
        for (i, s) in [c.0, c.1, c.2, c.3].iter().enumerate() {
            offs[i] = (56 + strings.len()) as u32;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
        }
        let size = 56 + strings.len();
        let mut r = Vec::with_capacity(size);
        r.extend((size as u32).to_le_bytes());
        r.extend([0u8; 12]);
        for o in offs {
            r.extend(o.to_le_bytes());
        }
        r.extend([0u8; 8]);
        r.extend(c.4.to_le_bytes());
        r.extend(c.5.to_le_bytes());
        r.extend(strings);
        r
    }

    /// A file with one page per element of `pages`.
    pub fn build(pages: &[Vec<Spec<'_>>]) -> Vec<u8> {
        let mut bodies = Vec::new();
        for cookies in pages {
            let recs: Vec<Vec<u8>> = cookies.iter().map(record).collect();
            let header = 8 + recs.len() * 4 + 4;
            let mut p = Vec::new();
            p.extend(0x100u32.to_be_bytes());
            p.extend((recs.len() as u32).to_le_bytes());
            let mut at = header;
            for r in &recs {
                p.extend((at as u32).to_le_bytes());
                at += r.len();
            }
            p.extend(0u32.to_le_bytes());
            for r in recs {
                p.extend(r);
            }
            bodies.push(p);
        }
        let mut f = b"cook".to_vec();
        f.extend((bodies.len() as u32).to_be_bytes());
        for b in &bodies {
            f.extend((b.len() as u32).to_be_bytes());
        }
        for b in bodies {
            f.extend(b);
        }
        f.extend([0u8; 8]);
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let data = testing::build(&[
            vec![
                (
                    ".example.com",
                    "sid",
                    "/",
                    "abc",
                    700_000_000.0,
                    600_000_000.5,
                ),
                ("www.x.org", "n", "/p", "", 0.0, 1.0),
            ],
            vec![("y.com", "a", "/", "b", 1.0, 2.0)],
        ]);
        let c = parse(&data).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].url, ".example.com");
        assert_eq!(c[0].value, "abc");
        assert_eq!(c[0].expiration, Some(1_678_307_200));
        assert_eq!(c[0].creation, Some(1_578_307_200));
        assert_eq!(c[2].name, "a");
        assert_eq!(parse(b"nope"), Err(Error::NotCookies));
        assert_eq!(parse(b"cook"), Err(Error::Truncated));
        // Truncated file: no panic, partial results.
        for n in 0..data.len() {
            let _ = parse(&data[..n]);
        }
    }

    proptest::proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let mut d = b"cook".to_vec();
            d.extend(data);
            let _ = parse(&d);
        }
    }
}
