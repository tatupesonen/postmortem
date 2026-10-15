//! Legacy Windows event log (`.evt`) builders.

/// Builds one legacy `.evt` record (`EVENTLOGRECORD`).
#[allow(clippy::too_many_arguments)]
pub fn build(
    record_id: u32,
    time: u32,
    event_id: u32,
    source: &str,
    computer: &str,
    strings: &[&str],
    sid: Option<&[u8]>,
) -> Vec<u8> {
    let utf16z = |s: &str| -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    let mut var = utf16z(source);
    var.extend(utf16z(computer));
    while !(0x38 + var.len()).is_multiple_of(4) {
        var.push(0);
    }
    let sid_off = 0x38 + var.len();
    let sid_bytes = sid.unwrap_or(&[]);
    var.extend_from_slice(sid_bytes);
    let string_off = 0x38 + var.len();
    for s in strings {
        var.extend(utf16z(s));
    }
    while !(0x38 + var.len() + 4).is_multiple_of(4) {
        var.push(0);
    }
    let len = 0x38 + var.len() + 4;
    let mut r = Vec::with_capacity(len);
    let p32 = |r: &mut Vec<u8>, v: u32| r.extend_from_slice(&v.to_le_bytes());
    p32(&mut r, len as u32);
    r.extend_from_slice(b"LfLe");
    p32(&mut r, record_id);
    p32(&mut r, time);
    p32(&mut r, time);
    p32(&mut r, event_id);
    r.extend_from_slice(&8u16.to_le_bytes()); // audit success
    r.extend_from_slice(&(strings.len() as u16).to_le_bytes());
    r.extend_from_slice(&[0; 4]); // category, flags
    p32(&mut r, 0); // closing record
    p32(&mut r, string_off as u32);
    p32(&mut r, sid_bytes.len() as u32);
    p32(&mut r, sid_off as u32);
    p32(&mut r, 0); // data length
    p32(&mut r, 0); // data offset
    r.extend(var);
    p32(&mut r, len as u32);
    r
}

/// A whole `.evt` file: a 0x30-byte header followed by `records`.
pub fn log(records: &[Vec<u8>]) -> Vec<u8> {
    let mut f = vec![0u8; 0x30];
    f[0..4].copy_from_slice(&0x30u32.to_le_bytes());
    f[4..8].copy_from_slice(b"LfLe");
    f[8..12].copy_from_slice(&1u32.to_le_bytes());
    f[12..16].copy_from_slice(&1u32.to_le_bytes());
    for r in records {
        f.extend_from_slice(r);
    }
    f
}
