//! Virtual disk container builders: VHD (fixed, dynamic, differencing),
//! VHDX (dynamic, differencing) and VMDK (monolithic sparse with an
//! embedded descriptor), built from raw disk bytes.
//!
//! These cover what `qemu-img` cannot create (differencing VHD/VHDX) and let
//! tests run without `qemu-img`. Layouts follow the published
//! specifications; see `pm-image` for the readers.

const SECTOR: usize = 512;
const MIB: usize = 1 << 20;

/// Which sectors of a differencing disk are stored in the child.
pub type Present<'a> = &'a dyn Fn(u64) -> bool;

fn put(buf: &mut [u8], off: usize, bytes: &[u8]) {
    buf[off..off + bytes.len()].copy_from_slice(bytes);
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

fn is_zero(b: &[u8]) -> bool {
    b.iter().all(|&x| x == 0)
}

// ------------------------------------------------------------------ VHD

fn vhd_footer(size: u64, disk_type: u32, data_offset: u64, id: [u8; 16]) -> Vec<u8> {
    let mut f = vec![0u8; SECTOR];
    put(&mut f, 0, b"conectix");
    put(&mut f, 8, &2u32.to_be_bytes());
    put(&mut f, 12, &0x0001_0000u32.to_be_bytes());
    put(&mut f, 16, &data_offset.to_be_bytes());
    put(&mut f, 28, b"pmtk");
    put(&mut f, 36, b"Wi2k");
    put(&mut f, 40, &size.to_be_bytes());
    put(&mut f, 48, &size.to_be_bytes());
    put(&mut f, 56, &[0x04, 0x00, 0x10, 0x3F]);
    put(&mut f, 60, &disk_type.to_be_bytes());
    put(&mut f, 68, &id);
    let sum: u32 = f.iter().fold(0u32, |a, &b| a.wrapping_add(u32::from(b)));
    put(&mut f, 64, &(!sum).to_be_bytes());
    f
}

/// A fixed VHD: the raw bytes (padded to a sector) plus a footer.
pub fn vhd_fixed(raw: &[u8]) -> Vec<u8> {
    let mut out = raw.to_vec();
    out.resize(raw.len().next_multiple_of(SECTOR), 0);
    let size = out.len() as u64;
    out.extend(vhd_footer(size, 2, u64::MAX, [0x11; 16]));
    out
}

/// A differencing VHD's parent reference.
#[derive(Debug, Clone)]
pub struct VhdParent<'a> {
    /// Relative path stored in the `W2ru` locator (Windows separators ok).
    pub relative: &'a str,
    /// Absolute path stored in the `W2ku` locator.
    pub absolute: &'a str,
    pub parent_id: [u8; 16],
}

/// A dynamic VHD holding `raw` (all-zero blocks left unallocated).
pub fn vhd_dynamic(raw: &[u8], block_size: u32) -> Vec<u8> {
    vhd_sparse(raw, block_size, None, [0x22; 16])
}

/// A differencing VHD: sectors for which `present` is true are stored
/// (with their bits set in the block bitmap); the rest come from the parent.
pub fn vhd_differencing(
    raw: &[u8],
    block_size: u32,
    present: Present<'_>,
    parent: &VhdParent<'_>,
) -> Vec<u8> {
    vhd_sparse(raw, block_size, Some((present, parent)), [0x33; 16])
}

fn vhd_sparse(
    raw: &[u8],
    block_size: u32,
    diff: Option<(Present<'_>, &VhdParent<'_>)>,
    id: [u8; 16],
) -> Vec<u8> {
    let size = raw.len().next_multiple_of(SECTOR) as u64;
    let bs = block_size as usize;
    let blocks = (size as usize).div_ceil(bs);
    let bat_off = 1536usize;
    let bat_len = (blocks * 4).next_multiple_of(SECTOR);
    let mut out = vec![0u8; bat_off + bat_len];
    let mut bat = vec![0xFFu8; bat_len];
    // Parent locators.
    let mut loc_entries: Vec<([u8; 4], u64, u32)> = Vec::new();
    if let Some((_, p)) = diff {
        for (code, text) in [(b"W2ru", p.relative), (b"W2ku", p.absolute)] {
            let data = utf16le(text);
            let off = out.len() as u64;
            let mut padded = data.clone();
            padded.resize(data.len().next_multiple_of(SECTOR).max(SECTOR), 0);
            out.extend(&padded);
            loc_entries.push((*code, off, data.len() as u32));
        }
    }
    let bitmap_len = (bs / SECTOR).div_ceil(8).next_multiple_of(SECTOR);
    for b in 0..blocks {
        let start = b * bs;
        let end = (start + bs).min(raw.len());
        let chunk = raw.get(start..end).unwrap_or_default();
        let mut bitmap = vec![0u8; bitmap_len];
        let mut any = false;
        for s in 0..bs / SECTOR {
            let sector = (start / SECTOR + s) as u64;
            let on = match diff {
                Some((present, _)) => sector < size / SECTOR as u64 && present(sector),
                None => {
                    let a = (s * SECTOR).min(chunk.len());
                    let z = ((s + 1) * SECTOR).min(chunk.len());
                    !is_zero(&chunk[a..z])
                }
            };
            if on {
                bitmap[s / 8] |= 0x80 >> (s % 8);
                any = true;
            }
        }
        if !any {
            continue;
        }
        if diff.is_none() {
            // Like Virtual PC/qemu: a written block marks all its sectors.
            bitmap.fill(0xFF);
        }
        let sector = (out.len() / SECTOR) as u32;
        put(&mut bat, b * 4, &sector.to_be_bytes());
        out.extend(&bitmap);
        let mut data = chunk.to_vec();
        data.resize(bs, 0);
        out.extend(&data);
    }
    put(&mut out, bat_off, &bat);
    let disk_type = if diff.is_some() { 4 } else { 3 };
    let footer = vhd_footer(size, disk_type, 512, id);
    put(&mut out, 0, &footer);
    let mut h = vec![0u8; 1024];
    put(&mut h, 0, b"cxsparse");
    put(&mut h, 8, &u64::MAX.to_be_bytes());
    put(&mut h, 16, &(bat_off as u64).to_be_bytes());
    put(&mut h, 24, &0x0001_0000u32.to_be_bytes());
    put(&mut h, 28, &(blocks as u32).to_be_bytes());
    put(&mut h, 32, &block_size.to_be_bytes());
    if let Some((_, p)) = diff {
        put(&mut h, 40, &p.parent_id);
        let name = p.absolute.rsplit(['\\', '/']).next().unwrap_or_default();
        let n = utf16be(name);
        put(&mut h, 64, &n[..n.len().min(512)]);
        for (i, (code, off, len)) in loc_entries.iter().enumerate() {
            let e = 576 + i * 24;
            put(&mut h, e, code);
            put(&mut h, e + 4, &(*len).next_multiple_of(512).to_be_bytes());
            put(&mut h, e + 8, &len.to_be_bytes());
            put(&mut h, e + 16, &off.to_be_bytes());
        }
    }
    let sum: u32 = h.iter().fold(0u32, |a, &b| a.wrapping_add(u32::from(b)));
    put(&mut h, 36, &(!sum).to_be_bytes());
    put(&mut out, 512, &h);
    out.extend(footer);
    out
}

// ----------------------------------------------------------------- VHDX

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5],
        d4[6], d4[7],
    ]
}

const BAT_REGION: [u8; 16] = guid(
    0x2DC2_7766,
    0xF623,
    0x4200,
    [0x9D, 0x64, 0x11, 0x5E, 0x9B, 0xFD, 0x4A, 0x08],
);
const METADATA_REGION: [u8; 16] = guid(
    0x8B7C_A206,
    0x4790,
    0x4B9A,
    [0xB8, 0xFE, 0x57, 0x5F, 0x05, 0x0F, 0x88, 0x6E],
);
const FILE_PARAMETERS: [u8; 16] = guid(
    0xCAA1_6737,
    0xFA36,
    0x4D43,
    [0xB3, 0xB6, 0x33, 0xF0, 0xAA, 0x44, 0xE7, 0x6B],
);
const VIRTUAL_DISK_SIZE: [u8; 16] = guid(
    0x2FA5_4224,
    0xCD1B,
    0x4876,
    [0xB2, 0x11, 0x5D, 0xBE, 0xD8, 0x3B, 0xF4, 0xB8],
);
const VIRTUAL_DISK_ID: [u8; 16] = guid(
    0xBECA_12AB,
    0xB2E6,
    0x4523,
    [0x93, 0xEF, 0xC3, 0x09, 0xE0, 0x00, 0xC7, 0x46],
);
const LOGICAL_SECTOR_SIZE: [u8; 16] = guid(
    0x8141_BF1D,
    0xA96F,
    0x4709,
    [0xBA, 0x47, 0xF2, 0x33, 0xA8, 0xFA, 0xAB, 0x5F],
);
const PHYSICAL_SECTOR_SIZE: [u8; 16] = guid(
    0xCDA3_48C7,
    0x445D,
    0x4471,
    [0x9C, 0xC9, 0xE9, 0x88, 0x52, 0x51, 0xC5, 0x56],
);
const PARENT_LOCATOR: [u8; 16] = guid(
    0xA8D3_5F2D,
    0xB30B,
    0x454D,
    [0xAB, 0xF7, 0xD3, 0xD8, 0x48, 0x34, 0xAB, 0x0C],
);
const VHDX_LOCATOR_TYPE: [u8; 16] = guid(
    0xB04A_EFB7,
    0xD19E,
    0x4A81,
    [0xB7, 0x89, 0x25, 0xB8, 0xE9, 0x44, 0x59, 0x13],
);

/// Options for [`vhdx`].
#[derive(Clone, Copy)]
pub struct VhdxOptions<'a> {
    pub block_size: u32,
    pub logical_sector: u32,
    /// Differencing: sectors stored in the child, and the parent's
    /// `relative_path` locator value.
    pub parent: Option<(Present<'a>, &'a str)>,
    /// Set a log GUID (as if the disk was not closed cleanly).
    pub dirty_log: bool,
}

impl std::fmt::Debug for VhdxOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VhdxOptions")
            .field("block_size", &self.block_size)
            .field("differencing", &self.parent.is_some())
            .finish()
    }
}

impl Default for VhdxOptions<'_> {
    fn default() -> Self {
        Self {
            block_size: MIB as u32,
            logical_sector: 512,
            parent: None,
            dirty_log: false,
        }
    }
}

/// A VHDX holding `raw`. Without a parent, all-zero blocks are left
/// `NOT_PRESENT`; with one, fully stored blocks are `FULLY_PRESENT`,
/// partially stored ones `PARTIALLY_PRESENT` with a sector bitmap.
pub fn vhdx(raw: &[u8], o: &VhdxOptions<'_>) -> Vec<u8> {
    let lss = o.logical_sector as usize;
    let size = raw.len().next_multiple_of(lss);
    let bs = o.block_size as usize;
    let blocks = size.div_ceil(bs);
    let chunk_ratio = ((1usize << 23) * lss) / bs;
    let chunks = blocks.div_ceil(chunk_ratio);
    let bat_entries = chunks * (chunk_ratio + 1);
    let (log_off, meta_off, bat_off) = (MIB, 2 * MIB, 3 * MIB);
    let bat_len = (bat_entries * 8).next_multiple_of(MIB);
    let mut out = vec![0u8; bat_off + bat_len];
    put(&mut out, 0, b"vhdxfile");
    put(&mut out, 8, &utf16le("pm-testkit"));
    // Headers.
    for (i, at) in [64 * 1024usize, 128 * 1024].into_iter().enumerate() {
        let mut h = vec![0u8; 4096];
        put(&mut h, 0, b"head");
        put(&mut h, 8, &(i as u64 + 1).to_le_bytes());
        put(&mut h, 16, &[0xA1; 16]);
        put(&mut h, 32, &[0xB2; 16]);
        if o.dirty_log {
            put(&mut h, 48, &[0xC3; 16]);
        }
        put(&mut h, 66, &1u16.to_le_bytes());
        put(&mut h, 68, &(MIB as u32).to_le_bytes());
        put(&mut h, 72, &(log_off as u64).to_le_bytes());
        let c = crc32c(&h);
        put(&mut h, 4, &c.to_le_bytes());
        put(&mut out, at, &h);
    }
    // Region tables.
    let mut rt = vec![0u8; 64 * 1024];
    put(&mut rt, 0, b"regi");
    put(&mut rt, 8, &2u32.to_le_bytes());
    for (i, (id, off, len)) in [
        (BAT_REGION, bat_off, bat_len),
        (METADATA_REGION, meta_off, MIB),
    ]
    .into_iter()
    .enumerate()
    {
        let e = 16 + i * 32;
        put(&mut rt, e, &id);
        put(&mut rt, e + 16, &(off as u64).to_le_bytes());
        put(&mut rt, e + 24, &(len as u32).to_le_bytes());
        put(&mut rt, e + 28, &1u32.to_le_bytes());
    }
    let c = crc32c(&rt);
    put(&mut rt, 4, &c.to_le_bytes());
    put(&mut out, 192 * 1024, &rt);
    put(&mut out, 256 * 1024, &rt);
    // Metadata.
    let mut items: Vec<([u8; 16], Vec<u8>, u32)> = vec![
        (
            FILE_PARAMETERS,
            {
                let mut v = o.block_size.to_le_bytes().to_vec();
                let flags: u32 = if o.parent.is_some() { 2 } else { 0 };
                v.extend(flags.to_le_bytes());
                v
            },
            4,
        ),
        (VIRTUAL_DISK_SIZE, (size as u64).to_le_bytes().to_vec(), 6),
        (VIRTUAL_DISK_ID, vec![0xD4; 16], 6),
        (
            LOGICAL_SECTOR_SIZE,
            o.logical_sector.to_le_bytes().to_vec(),
            6,
        ),
        (PHYSICAL_SECTOR_SIZE, 4096u32.to_le_bytes().to_vec(), 6),
    ];
    if let Some((_, rel)) = o.parent {
        let kv = [
            ("parent_linkage", "{b2b2b2b2-b2b2-b2b2-b2b2-b2b2b2b2b2b2}"),
            ("relative_path", rel),
            ("absolute_win32_path", r"C:\nowhere\missing-parent.vhdx"),
        ];
        let mut pl = VHDX_LOCATOR_TYPE.to_vec();
        pl.extend(0u16.to_le_bytes());
        pl.extend((kv.len() as u16).to_le_bytes());
        let mut strings: Vec<u8> = Vec::new();
        let base = 20 + kv.len() * 12;
        let mut entries: Vec<u8> = Vec::new();
        for (k, v) in kv {
            let (k, v) = (utf16le(k), utf16le(v));
            let ko = base + strings.len();
            strings.extend(&k);
            let vo = base + strings.len();
            strings.extend(&v);
            entries.extend((ko as u32).to_le_bytes());
            entries.extend((vo as u32).to_le_bytes());
            entries.extend((k.len() as u16).to_le_bytes());
            entries.extend((v.len() as u16).to_le_bytes());
        }
        pl.extend(entries);
        pl.extend(strings);
        items.push((PARENT_LOCATOR, pl, 4));
    }
    let mut md = vec![0u8; MIB];
    put(&mut md, 0, b"metadata");
    put(&mut md, 10, &(items.len() as u16).to_le_bytes());
    let mut data_at = 64 * 1024;
    for (i, (id, data, flags)) in items.iter().enumerate() {
        let e = 32 + i * 32;
        put(&mut md, e, id);
        put(&mut md, e + 16, &(data_at as u32).to_le_bytes());
        put(&mut md, e + 20, &(data.len() as u32).to_le_bytes());
        put(&mut md, e + 24, &flags.to_le_bytes());
        put(&mut md, data_at, data);
        data_at += data.len().next_multiple_of(8);
    }
    put(&mut out, meta_off, &md);
    // Payload blocks and sector bitmaps.
    let sectors_per_block = bs / lss;
    let mut bat = vec![0u8; bat_len];
    let mut bitmaps: Vec<Option<Vec<u8>>> = vec![None; chunks];
    for b in 0..blocks {
        let start = b * bs;
        let end = (start + bs).min(raw.len());
        let chunk = raw.get(start..end).unwrap_or_default();
        let state: u64 = match o.parent {
            None => {
                if is_zero(chunk) {
                    0
                } else {
                    6
                }
            }
            Some((present, _)) => {
                let first = (start / lss) as u64;
                let n = (0..sectors_per_block as u64)
                    .filter(|s| (first + s) * (lss as u64) < size as u64 && present(first + s))
                    .count();
                if n == 0 {
                    0
                } else if n == sectors_per_block {
                    6
                } else {
                    let c = b / chunk_ratio;
                    let bm = bitmaps[c].get_or_insert_with(|| vec![0u8; MIB]);
                    let base = (b % chunk_ratio) * sectors_per_block;
                    for s in 0..sectors_per_block {
                        if present(first + s as u64) {
                            let bit = base + s;
                            bm[bit / 8] |= 1 << (bit % 8);
                        }
                    }
                    7
                }
            }
        };
        let idx = b + b / chunk_ratio;
        let mut entry = state;
        if state != 0 {
            let off = out.len();
            let mut data = chunk.to_vec();
            data.resize(bs, 0);
            out.extend(&data);
            entry |= off as u64;
        }
        put(&mut bat, idx * 8, &entry.to_le_bytes());
    }
    for (c, bm) in bitmaps.into_iter().enumerate() {
        if let Some(bm) = bm {
            let off = out.len();
            out.extend(&bm);
            let idx = c * (chunk_ratio + 1) + chunk_ratio;
            put(&mut bat, idx * 8, &(6u64 | off as u64).to_le_bytes());
        }
    }
    put(&mut out, bat_off, &bat);
    out
}

// ----------------------------------------------------------------- VMDK

/// A monolithic sparse VMDK (embedded descriptor, 64 KiB grains, 512-entry
/// grain tables) holding `raw`; all-zero grains are left unallocated.
/// `parent` makes it a delta link to the named parent VMDK.
pub fn vmdk_sparse(raw: &[u8], name: &str, parent: Option<&str>) -> Vec<u8> {
    let grain = 128usize; // sectors
    let gtes = 512usize;
    let capacity = raw.len().div_ceil(SECTOR) as u64;
    let grains = (capacity as usize).div_ceil(grain);
    let gts = grains.div_ceil(gtes);
    let desc_off = 1usize; // sectors
    let desc_size = 20usize;
    let mut desc = format!(
        "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID={}\ncreateType=\"monolithicSparse\"\n",
        if parent.is_some() {
            "12345678"
        } else {
            "ffffffff"
        }
    );
    if let Some(p) = parent {
        desc.push_str(&format!("parentFileNameHint=\"{p}\"\n"));
    }
    desc.push_str(&format!(
        "\n# Extent description\nRW {capacity} SPARSE \"{name}\"\n"
    ));
    assert!(desc.len() <= desc_size * SECTOR);
    let gd_off = desc_off + desc_size;
    let gd_sectors = (gts * 4).div_ceil(SECTOR);
    let gt_off = gd_off + gd_sectors;
    let gt_sectors = (gts * gtes * 4).div_ceil(SECTOR);
    let overhead = (gt_off + gt_sectors).next_multiple_of(grain);
    let mut out = vec![0u8; overhead * SECTOR];
    put(&mut out, 0, b"KDMV");
    put(&mut out, 4, &1u32.to_le_bytes());
    put(&mut out, 8, &3u32.to_le_bytes());
    put(&mut out, 12, &capacity.to_le_bytes());
    put(&mut out, 20, &(grain as u64).to_le_bytes());
    put(&mut out, 28, &(desc_off as u64).to_le_bytes());
    put(&mut out, 36, &(desc_size as u64).to_le_bytes());
    put(&mut out, 44, &(gtes as u32).to_le_bytes());
    put(&mut out, 48, &0u64.to_le_bytes());
    put(&mut out, 56, &(gd_off as u64).to_le_bytes());
    put(&mut out, 64, &(overhead as u64).to_le_bytes());
    put(&mut out, 73, b"\n \r\n");
    put(&mut out, desc_off * SECTOR, desc.as_bytes());
    for t in 0..gts {
        let gt_sector = (gt_off + t * gtes * 4 / SECTOR) as u32;
        put(&mut out, gd_off * SECTOR + t * 4, &gt_sector.to_le_bytes());
    }
    for g in 0..grains {
        let start = g * grain * SECTOR;
        let end = (start + grain * SECTOR).min(raw.len());
        let chunk = &raw[start..end];
        if is_zero(chunk) {
            continue;
        }
        let sector = (out.len() / SECTOR) as u32;
        let mut data = chunk.to_vec();
        data.resize(grain * SECTOR, 0);
        out.extend(&data);
        put(&mut out, gt_off * SECTOR + g * 4, &sector.to_le_bytes());
    }
    out
}
