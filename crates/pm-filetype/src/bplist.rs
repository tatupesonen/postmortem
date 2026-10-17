//! Tika's `BPListDetector`: parses a binary property list like dd-plist
//! 1.28 (any parse failure makes Tika throw) and specialises it by the
//! keys of the top-level dictionary.

use std::collections::HashMap;

use pm_core::ReadAt;

use crate::Failure;

/// Largest property list parsed (dd-plist reads the whole file).
const MAX_PLIST: u64 = 256 * 1024 * 1024;

/// A parsed object, reduced to what detection needs.
#[derive(Debug, Clone)]
enum Obj {
    Null,
    Str(String),
    Dict(Vec<String>),
    Other,
}

struct Parser<'a> {
    b: &'a [u8],
    obj_ref_size: i32,
    offset_size: i32,
    num_objects: i32,
    offset_table: i32,
    parsed: HashMap<i32, Obj>,
}

fn fail(msg: &str) -> Failure {
    Failure(format!("bplist: {msg}"))
}

/// dd-plist `parseUnsignedInt` (masked to 32 bits). An empty range is 0
/// without touching the array, whatever its (even negative) position.
fn uint(b: &[u8], start: i64, end: i64) -> Result<i64, Failure> {
    if end <= start {
        return Ok(0);
    }
    let (s, e) = (usize::try_from(start), usize::try_from(end));
    let (Ok(s), Ok(e)) = (s, e) else {
        return Err(fail("index"));
    };
    let bytes = b.get(s..e).ok_or_else(|| fail("index"))?;
    let mut l: i64 = 0;
    for &x in bytes {
        l = l.wrapping_shl(8) | i64::from(x);
    }
    Ok(l & 0xFFFF_FFFF)
}

impl Parser<'_> {
    fn byte(&self, i: i32) -> Result<u8, Failure> {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.b.get(i).copied())
            .ok_or_else(|| fail("index"))
    }

    fn object_offset(&self, obj: i32) -> Result<i32, Failure> {
        if obj >= self.num_objects {
            return Err(fail("object id"));
        }
        let start = i64::from(
            self.offset_table
                .wrapping_add(obj.wrapping_mul(self.offset_size)),
        );
        Ok(uint(self.b, start, start + i64::from(self.offset_size))? as i32)
    }

    fn reference(&self, base: i32, index: i32) -> Result<i32, Failure> {
        let off = base.wrapping_add(index.wrapping_mul(self.obj_ref_size));
        if i64::from(off) + i64::from(self.obj_ref_size) >= self.b.len() as i64 {
            return Err(fail("object reference"));
        }
        Ok(uint(
            self.b,
            i64::from(off),
            i64::from(off) + i64::from(self.obj_ref_size),
        )? as i32)
    }

    /// `readLengthAndOffset`.
    fn length_and_offset(&self, info: i32, offset: i32) -> Result<(i32, i32), Failure> {
        if info != 0xF {
            return Ok((info, 1));
        }
        let t = i32::from(self.byte(offset.wrapping_add(1))? as i8);
        let int_info = t & 0x0F;
        let int_len = 1i32 << int_info;
        let start = i64::from(offset) + 2;
        let len = if int_len < 3 {
            uint(self.b, start, start + i64::from(int_len))? as i32
        } else {
            // BigInteger(bytes).intValue(): the low 32 bits.
            let s = usize::try_from(start).map_err(|_| fail("index"))?;
            let e = s
                .checked_add(usize::try_from(int_len).map_err(|_| fail("index"))?)
                .ok_or_else(|| fail("index"))?;
            let bytes = self.b.get(s..e).ok_or_else(|| fail("index"))?;
            let mut v: u32 = 0;
            for &x in bytes {
                v = (v << 8) | u32::from(x);
            }
            v as i32
        };
        Ok((len, 2 + int_len))
    }

    fn parse(&mut self, stack: &mut Vec<i32>, obj: i32) -> Result<Obj, Failure> {
        // ParsedObjectStack.push: a cycle on the current path fails.
        if stack.contains(&obj) {
            return Err(fail("cycle"));
        }
        if stack.len() > 10_000 {
            return Err(fail("depth"));
        }
        if let Some(o) = self.parsed.get(&obj) {
            return Ok(o.clone());
        }
        stack.push(obj);
        let r = self.parse_inner(stack, obj);
        stack.pop();
        let o = r?;
        self.parsed.insert(obj, o.clone());
        Ok(o)
    }

    fn parse_inner(&mut self, stack: &mut Vec<i32>, obj: i32) -> Result<Obj, Failure> {
        let offset = self.object_offset(obj)?;
        let ty = self.byte(offset)?;
        let obj_type = ty >> 4;
        let info = i32::from(ty & 0x0F);
        let len_b = self.b.len() as i64;
        let off = i64::from(offset);
        match obj_type {
            0x0 => match info {
                0x0 => Ok(Obj::Null),
                0x8 | 0x9 => Ok(Obj::Other),
                0xE => {
                    if off + 1 + 16 >= len_b {
                        return Err(fail("uid"));
                    }
                    Ok(Obj::Other)
                }
                _ => Err(fail("simple type")),
            },
            0x1 | 0x2 => {
                let len = 1i64 << info;
                if off + 1 + len > len_b {
                    return Err(fail("number"));
                }
                if obj_type == 0x2 && len != 4 && len != 8 {
                    return Err(fail("real"));
                }
                Ok(Obj::Other)
            }
            0x3 => {
                if info != 3 || off + 9 > len_b {
                    return Err(fail("date"));
                }
                Ok(Obj::Other)
            }
            0x4 => {
                let (len, o) = self.length_and_offset(info, offset)?;
                let start = off + i64::from(o);
                if start + i64::from(len) > len_b || len < 0 {
                    return Err(fail("data"));
                }
                Ok(Obj::Other)
            }
            0x5..=0x7 => {
                let (n, o) = self.length_and_offset(info, offset)?;
                let start = off + i64::from(o);
                let len = match obj_type {
                    0x5 => i64::from(n),
                    0x6 => 2 * i64::from(n),
                    _ => self.utf8_len(start, n),
                };
                if start + len > len_b || len < 0 {
                    return Err(fail("string"));
                }
                let s = usize::try_from(start).map_err(|_| fail("string"))?;
                let e = usize::try_from(start + len).map_err(|_| fail("string"))?;
                let bytes = self.b.get(s..e).ok_or_else(|| fail("string"))?;
                let text = match obj_type {
                    0x5 => bytes
                        .iter()
                        .map(|&c| if c < 0x80 { char::from(c) } else { '\u{FFFD}' })
                        .collect(),
                    0x6 => {
                        let units: Vec<u16> = bytes
                            .chunks_exact(2)
                            .map(|c| {
                                u16::from_be_bytes([
                                    c.first().copied().unwrap_or(0),
                                    c.get(1).copied().unwrap_or(0),
                                ])
                            })
                            .collect();
                        String::from_utf16_lossy(&units)
                    }
                    _ => String::from_utf8_lossy(bytes).into_owned(),
                };
                Ok(Obj::Str(text))
            }
            0x8 => {
                if off + 1 + i64::from(info + 1) >= len_b {
                    return Err(fail("uid"));
                }
                Ok(Obj::Other)
            }
            0xA..=0xC => {
                let (n, o) = self.length_and_offset(info, offset)?;
                let base = offset.wrapping_add(o);
                let mut seen = Vec::new();
                for i in 0..n.max(0) {
                    let r = self.reference(base, i)?;
                    if obj_type != 0xA {
                        if seen.contains(&r) {
                            continue;
                        }
                        seen.push(r);
                    }
                    self.parse(stack, r)?;
                }
                Ok(Obj::Other)
            }
            0xD => {
                let (n, o) = self.length_and_offset(info, offset)?;
                let keys_at = offset.wrapping_add(o);
                let vals_at = keys_at.wrapping_add(n.wrapping_mul(self.obj_ref_size));
                let mut keys = Vec::new();
                for i in 0..n.max(0) {
                    let k = self.reference(keys_at, i)?;
                    let v = self.reference(vals_at, i)?;
                    let key = self.parse(stack, k)?;
                    if matches!(key, Obj::Null) {
                        return Err(fail("null key"));
                    }
                    self.parse(stack, v)?;
                    if let Obj::Str(s) = key {
                        keys.push(s);
                    }
                }
                Ok(Obj::Dict(keys))
            }
            _ => Err(fail("type")),
        }
    }

    /// `calculateUtf8StringLength`.
    fn utf8_len(&self, offset: i64, chars: i32) -> i64 {
        let mut length: i64 = 0;
        for _ in 0..chars.max(0) {
            let t = offset + length;
            let Some(&c) = usize::try_from(t).ok().and_then(|t| self.b.get(t)) else {
                return i64::from(chars);
            };
            if c & 0x80 == 0 {
                length += 1;
                continue;
            }
            let n = if c & 0xC0 == 0x80 {
                return i64::from(chars);
            } else if c & 0xE0 == 0xC0 {
                1
            } else if c & 0xF0 == 0xE0 {
                2
            } else if c & 0xF8 == 0xF0 {
                3
            } else {
                0
            };
            let ok = (1..=n).all(|i| {
                usize::try_from(t + i)
                    .ok()
                    .and_then(|p| self.b.get(p))
                    .is_some_and(|&x| x & 0xC0 == 0x80)
            });
            if ok {
                // dd-plist's quirk: a multi-byte character counts 2.
                length += 2;
            } else {
                return i64::from(chars);
            }
        }
        length
    }
}

/// `BPListDetector.detectOnKeys`.
fn on_keys(keys: &[String]) -> &'static str {
    let has = |k: &str| keys.iter().any(|x| x == k);
    if has("nodes") && has("edges") && has("graphEncodingVersion") {
        "application/x-bplist-memgraph"
    } else if has("WebMainResource") {
        "application/x-bplist-webarchive"
    } else if has("Playlists") && has("Tracks") && has("Music Folder") {
        "application/x-bplist-itunes"
    } else {
        "application/x-bplist"
    }
}

/// Parses a whole binary plist; the detected type or the failure.
pub(crate) fn detect_bytes(b: &[u8]) -> Result<&'static str, Failure> {
    if b.len() < 8 {
        return Err(fail("short"));
    }
    let magic = b.get(..8).unwrap_or_default();
    if !magic.starts_with(b"bplist")
        || !magic
            .get(6..8)
            .is_some_and(|v| v.iter().all(u8::is_ascii_digit))
    {
        return Err(fail("header"));
    }
    if magic.get(6) != Some(&b'0') {
        return Err(fail("version"));
    }
    if b.len() < 40 {
        return Err(fail("offset table"));
    }
    let tr = b.len() as i64 - 32;
    let offset_size = uint(b, tr + 6, tr + 7)? as i32;
    let obj_ref_size = uint(b, tr + 7, tr + 8)? as i32;
    let num_objects = uint(b, tr + 8, tr + 16)? as i32;
    let top = uint(b, tr + 16, tr + 24)? as i32;
    let offset_table = uint(b, tr + 24, tr + 32)? as i32;
    let end = offset_table.wrapping_add(num_objects.wrapping_add(1).wrapping_mul(offset_size));
    if i64::from(end) > b.len() as i64 || i64::from(top) >= b.len() as i64 - 32 {
        return Err(fail("corrupted offset table"));
    }
    let mut p = Parser {
        b,
        obj_ref_size,
        offset_size,
        num_objects,
        offset_table,
        parsed: HashMap::new(),
    };
    match p.parse(&mut Vec::new(), top)? {
        Obj::Dict(keys) => Ok(on_keys(&keys)),
        _ => Ok("application/x-bplist"),
    }
}

/// Detects a binary plist; `None` when the file does not start with
/// `bplist`.
pub(crate) fn detect(src: &dyn ReadAt) -> Result<Option<&'static str>, Failure> {
    let mut head = [0u8; 8];
    let n = src.read_at(0, &mut head).unwrap_or(0);
    if n < 6 || !head.starts_with(b"bplist") {
        return Ok(None);
    }
    if src.size() > MAX_PLIST {
        tracing::debug!("binary plist larger than {MAX_PLIST} bytes; not parsed");
        return Ok(Some("application/x-bplist"));
    }
    let len = usize::try_from(src.size()).map_err(|_| fail("size"))?;
    let data = src.read_vec_at(0, len).map_err(|e| fail(&e.to_string()))?;
    detect_bytes(&data).map(Some)
}

/// Builds a binary plist dictionary with string keys and values (tests).
#[cfg(test)]
pub(crate) fn build_dict(keys: &[&str]) -> Vec<u8> {
    let mut out = b"bplist00".to_vec();
    let n = keys.len();
    let mut offsets = Vec::new();
    // Object 0: dict; objects 1..=n: keys; n+1: a shared value string.
    offsets.push(out.len());
    out.push(0xD0 | n as u8);
    for i in 0..n {
        out.push((i + 1) as u8);
    }
    for _ in 0..n {
        out.push((n + 1) as u8);
    }
    for k in keys {
        offsets.push(out.len());
        if k.len() < 15 {
            out.push(0x50 | k.len() as u8);
        } else {
            out.extend_from_slice(&[0x5F, 0x10, k.len() as u8]);
        }
        out.extend_from_slice(k.as_bytes());
    }
    offsets.push(out.len());
    out.extend_from_slice(b"\x51v");
    let table = out.len();
    for o in &offsets {
        out.push(*o as u8);
    }
    let mut trailer = [0u8; 32];
    trailer[6] = 1;
    trailer[7] = 1;
    trailer[15] = offsets.len() as u8;
    trailer[23] = 0;
    trailer[31] = table as u8;
    out.extend_from_slice(&trailer);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries() {
        assert_eq!(
            detect_bytes(&build_dict(&["a", "b"])).unwrap(),
            "application/x-bplist"
        );
        assert_eq!(
            detect_bytes(&build_dict(&["WebMainResource"])).unwrap(),
            "application/x-bplist-webarchive"
        );
        assert_eq!(
            detect_bytes(&build_dict(&["nodes", "edges", "graphEncodingVersion"])).unwrap(),
            "application/x-bplist-memgraph"
        );
    }

    #[test]
    fn corrupt() {
        let mut b = build_dict(&["a"]);
        let n = b.len();
        b[n - 1] = 0xFF;
        assert!(detect_bytes(&b).is_err());
        assert!(detect_bytes(b"bplist00").is_err());
        assert!(detect_bytes(b"bplist15aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        // A dictionary containing itself.
        let mut c = build_dict(&["a"]);
        c[10] = 0;
        assert!(detect_bytes(&c).is_err());
    }

    /// dd-plist's `github-issue72.plist`: a trailer shifted by one byte
    /// gives offset and reference sizes of 0, so every offset reads as 0
    /// and the "top object" is the `b` of the header (a UTF-16 string).
    /// dd-plist accepts it; so does Tika.
    #[test]
    fn zero_offset_size() {
        let mut b = b"bplist00".to_vec();
        let mut trailer = [0u8; 32];
        trailer[15] = 1;
        // A table offset far outside the file is never dereferenced.
        trailer[27] = 0xBD;
        b.extend_from_slice(&trailer);
        assert_eq!(detect_bytes(&b).unwrap(), "application/x-bplist");
    }
}
