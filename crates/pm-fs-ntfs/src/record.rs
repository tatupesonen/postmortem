//! MFT records: fixups, header and raw attribute decoding.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

pub(crate) const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub(crate) const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub(crate) const ATTR_FILE_NAME: u32 = 0x30;
pub(crate) const ATTR_VOLUME_NAME: u32 = 0x60;
pub(crate) const ATTR_DATA: u32 = 0x80;
pub(crate) const ATTR_INDEX_ROOT: u32 = 0x90;
pub(crate) const ATTR_INDEX_ALLOCATION: u32 = 0xA0;
pub(crate) const ATTR_BITMAP: u32 = 0xB0;
pub(crate) const ATTR_REPARSE_POINT: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

pub(crate) const FLAG_COMPRESSED: u16 = 0x0001;
pub(crate) const FLAG_ENCRYPTED: u16 = 0x4000;
pub(crate) const FLAG_SPARSE: u16 = 0x8000;

/// Applies the update-sequence-array fixups in place. Returns `false` if a
/// sector's check value did not match (torn write); data is still fixed up.
pub(crate) fn apply_fixups(buf: &mut [u8], stride: usize) -> Result<bool, String> {
    let usa_off = usize::from(buf.u16_le(4).map_err(|e| e.to_string())?);
    let usa_count = usize::from(buf.u16_le(6).map_err(|e| e.to_string())?);
    if usa_count == 0 {
        return Ok(true);
    }
    if usa_count - 1 > buf.len() / stride || usa_off + usa_count * 2 > buf.len() {
        return Err(format!(
            "invalid update sequence array ({usa_count} entries at {usa_off})"
        ));
    }
    let usn = buf.u16_le(usa_off).map_err(|e| e.to_string())?;
    let mut intact = true;
    for i in 1..usa_count {
        let pos = i * stride - 2;
        let replacement = buf.u16_le(usa_off + i * 2).map_err(|e| e.to_string())?;
        let Some(slot) = buf.get_mut(pos..pos + 2) else {
            return Err("fixup position outside record".into());
        };
        if *slot != usn.to_le_bytes() {
            intact = false;
        }
        slot.copy_from_slice(&replacement.to_le_bytes());
    }
    Ok(intact)
}

/// One decoded run of a non-resident attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) vcn: u64,
    pub(crate) len: u64,
    /// `None` for sparse runs.
    pub(crate) lcn: Option<u64>,
}

/// Decodes a mapping-pairs (runlist) array.
pub(crate) fn decode_runs(b: &[u8], start_vcn: u64) -> Result<Vec<Run>, String> {
    let mut runs = Vec::new();
    let mut pos = 0usize;
    let mut vcn = start_vcn;
    let mut lcn: i64 = 0;
    while let Ok(header) = b.u8_at(pos) {
        if header == 0 {
            break;
        }
        let len_bytes = usize::from(header & 0x0F);
        let off_bytes = usize::from(header >> 4);
        if len_bytes == 0 || len_bytes > 8 || off_bytes > 8 {
            return Err(format!("invalid run header {header:#04x}"));
        }
        let len = b.uint_le(pos + 1, len_bytes).map_err(|e| e.to_string())?;
        if len == 0 || len > (1 << 48) {
            return Err(format!("invalid run length {len}"));
        }
        let this_lcn = if off_bytes == 0 {
            None
        } else {
            let delta = b
                .int_le(pos + 1 + len_bytes, off_bytes)
                .map_err(|e| e.to_string())?;
            lcn = lcn.checked_add(delta).ok_or("run offset overflow")?;
            Some(u64::try_from(lcn).map_err(|_| "negative LCN")?)
        };
        runs.push(Run {
            vcn,
            len,
            lcn: this_lcn,
        });
        vcn = vcn.checked_add(len).ok_or("VCN overflow")?;
        pos += 1 + len_bytes + off_bytes;
        if runs.len() > 1_000_000 {
            return Err("runlist too long".into());
        }
    }
    Ok(runs)
}

/// Body of an attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttrBody {
    Resident(Vec<u8>),
    NonResident {
        start_vcn: u64,
        runs: Vec<Run>,
        alloc_size: u64,
        real_size: u64,
        init_size: u64,
        /// log2(clusters per compression unit); 0 = not compressed.
        compression_unit: u16,
    },
}

/// A raw attribute record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attr {
    pub(crate) ty: u32,
    pub(crate) name: Option<String>,
    pub(crate) id: u16,
    pub(crate) flags: u16,
    pub(crate) body: AttrBody,
}

impl Attr {
    pub(crate) fn resident(&self) -> Option<&[u8]> {
        match &self.body {
            AttrBody::Resident(v) => Some(v),
            AttrBody::NonResident { .. } => None,
        }
    }

    pub(crate) fn real_size(&self) -> u64 {
        match &self.body {
            AttrBody::Resident(v) => v.len() as u64,
            AttrBody::NonResident { real_size, .. } => *real_size,
        }
    }
}

/// A decoded MFT record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) number: u64,
    pub(crate) seq: u16,
    pub(crate) link_count: u16,
    pub(crate) in_use: bool,
    pub(crate) is_dir: bool,
    /// Base record reference (non-zero for extension records).
    pub(crate) base_ref: u64,
    pub(crate) attrs: Vec<Attr>,
    pub(crate) torn: bool,
}

/// Parses one MFT record buffer (fixups are applied here).
pub(crate) fn parse_record(
    mut buf: Vec<u8>,
    number: u64,
    stride: usize,
) -> Result<Option<Record>, String> {
    match buf.get(..4) {
        Some(b"FILE") => {}
        Some(b"BAAD") => return Err(format!("record {number} marked BAAD")),
        _ => return Ok(None), // never used / zeroed
    }
    let intact = apply_fixups(&mut buf, stride)?;
    let e = |t: pm_core::Truncated| t.to_string();
    let seq = buf.u16_le(16).map_err(e)?;
    let link_count = buf.u16_le(18).map_err(e)?;
    let attrs_off = usize::from(buf.u16_le(20).map_err(e)?);
    let flags = buf.u16_le(22).map_err(e)?;
    let used = (buf.u32_le(24).map_err(e)? as usize).min(buf.len());
    let base_ref = buf.u64_le(32).map_err(e)?;
    let mut attrs = Vec::new();
    let mut off = attrs_off;
    while off + 8 <= used {
        let ty = buf.u32_le(off).map_err(e)?;
        if ty == ATTR_END {
            break;
        }
        let len = buf.u32_le(off + 4).map_err(e)? as usize;
        if len < 16 || off + len > used || !len.is_multiple_of(8) {
            break; // corrupt tail: keep what we have
        }
        let a = buf.sub(off, len).map_err(e)?;
        if let Ok(attr) = parse_attr(a) {
            attrs.push(attr);
        }
        off += len;
        if attrs.len() > 4096 {
            break;
        }
    }
    Ok(Some(Record {
        number,
        seq,
        link_count,
        in_use: flags & 0x01 != 0,
        is_dir: flags & 0x02 != 0,
        base_ref: base_ref & 0x0000_FFFF_FFFF_FFFF,
        attrs,
        torn: !intact,
    }))
}

fn parse_attr(a: &[u8]) -> Result<Attr, String> {
    let e = |t: pm_core::Truncated| t.to_string();
    let ty = a.u32_le(0).map_err(e)?;
    let non_resident = a.u8_at(8).map_err(e)? != 0;
    let name_len = usize::from(a.u8_at(9).map_err(e)?);
    let name_off = usize::from(a.u16_le(10).map_err(e)?);
    let flags = a.u16_le(12).map_err(e)?;
    let id = a.u16_le(14).map_err(e)?;
    let name = if name_len > 0 {
        Some(utf16le_lossy(a.sub(name_off, name_len * 2).map_err(e)?))
    } else {
        None
    };
    let body = if non_resident {
        let start_vcn = a.u64_le(16).map_err(e)?;
        let runs_off = usize::from(a.u16_le(32).map_err(e)?);
        let compression_unit = a.u16_le(34).map_err(e)?;
        let runs = decode_runs(a.get(runs_off..).unwrap_or_default(), start_vcn)?;
        AttrBody::NonResident {
            start_vcn,
            runs,
            alloc_size: a.u64_le(40).map_err(e)?,
            real_size: a.u64_le(48).map_err(e)?,
            init_size: a.u64_le(56).map_err(e)?,
            compression_unit: if compression_unit > 16 {
                0
            } else {
                compression_unit
            },
        }
    } else {
        let vlen = a.u32_le(16).map_err(e)? as usize;
        let voff = usize::from(a.u16_le(20).map_err(e)?);
        AttrBody::Resident(a.sub(voff, vlen).map_err(e)?.to_vec())
    };
    Ok(Attr {
        ty,
        name,
        id,
        flags,
        body,
    })
}

/// `$FILE_NAME` attribute contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileName {
    pub(crate) parent: u64,
    pub(crate) parent_seq: u16,
    pub(crate) times: [u64; 4],
    pub(crate) real_size: u64,
    pub(crate) flags: u32,
    pub(crate) namespace: u8,
    pub(crate) name: String,
}

pub(crate) const NS_DOS: u8 = 2;

impl FileName {
    pub(crate) fn parse(b: &[u8]) -> Option<Self> {
        let pref = b.u64_le(0).ok()?;
        let name_len = usize::from(b.u8_at(64).ok()?);
        let namespace = b.u8_at(65).ok()?;
        if namespace > 3 {
            return None;
        }
        Some(Self {
            parent: pref & 0x0000_FFFF_FFFF_FFFF,
            parent_seq: (pref >> 48) as u16,
            times: [
                b.u64_le(8).ok()?,
                b.u64_le(16).ok()?,
                b.u64_le(24).ok()?,
                b.u64_le(32).ok()?,
            ],
            real_size: b.u64_le(48).ok()?,
            flags: b.u32_le(56).ok()?,
            namespace,
            name: utf16le_lossy(b.sub(66, name_len * 2).ok()?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runlist_decoding() {
        // 0x21: 1-byte length, 2-byte offset. Run 1: 0x18 clusters at 0x5634.
        // Run 2: 0x11 → 1-byte length 0x10, offset -0x10 (0xF0).
        // Run 3: sparse 0x01 → len 0x20, no offset.
        let b = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x01, 0x20, 0x00];
        let runs = decode_runs(&b, 0).unwrap();
        assert_eq!(
            runs,
            vec![
                Run {
                    vcn: 0,
                    len: 0x18,
                    lcn: Some(0x5634)
                },
                Run {
                    vcn: 0x18,
                    len: 0x10,
                    lcn: Some(0x5624)
                },
                Run {
                    vcn: 0x28,
                    len: 0x20,
                    lcn: None
                },
            ]
        );
    }

    #[test]
    fn runlist_rejects_garbage() {
        assert!(decode_runs(&[0x90, 0, 0], 0).is_err());
        assert!(decode_runs(&[0x11, 0x05, 0x80], 0).is_err(), "negative LCN");
        assert_eq!(decode_runs(&[], 0).unwrap(), vec![]);
    }

    #[test]
    fn fixups() {
        let mut rec = vec![0u8; 1024];
        rec[4..6].copy_from_slice(&48u16.to_le_bytes());
        rec[6..8].copy_from_slice(&3u16.to_le_bytes());
        rec[48..50].copy_from_slice(&0x0007u16.to_le_bytes()); // USN
        rec[50..52].copy_from_slice(&0xAAAAu16.to_le_bytes());
        rec[52..54].copy_from_slice(&0xBBBBu16.to_le_bytes());
        rec[510..512].copy_from_slice(&0x0007u16.to_le_bytes());
        rec[1022..1024].copy_from_slice(&0x0007u16.to_le_bytes());
        assert!(apply_fixups(&mut rec, 512).unwrap());
        assert_eq!(&rec[510..512], &0xAAAAu16.to_le_bytes());
        assert_eq!(&rec[1022..1024], &0xBBBBu16.to_le_bytes());
        rec[1022..1024].copy_from_slice(&0x0009u16.to_le_bytes());
        rec[510..512].copy_from_slice(&0x0007u16.to_le_bytes());
        assert!(
            !apply_fixups(&mut rec, 512).unwrap(),
            "torn sector detected"
        );
    }
}
