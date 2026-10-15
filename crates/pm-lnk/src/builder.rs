//! Writers for shell items, shell links, compound files and jump lists
//! (tests, fuzz seeds, fixtures).

#![allow(
    clippy::cast_possible_truncation,
    clippy::missing_panics_doc,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "test-data writer; sizes are small and controlled"
)]

/// FAT date/time words for a UTC civil time.
pub fn dos(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> (u16, u16) {
    (
        ((y - 1980) << 9) | (mo << 5) | d,
        (h << 11) | (mi << 5) | (s / 2),
    )
}

/// GUID text `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX` → on-disk bytes.
pub fn guid_bytes(g: &str) -> [u8; 16] {
    let h: String = g.chars().filter(char::is_ascii_hexdigit).collect();
    let byte = |i: usize| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap();
    let mut b = [0u8; 16];
    for (dst, src) in [
        (0, 3),
        (1, 2),
        (2, 1),
        (3, 0),
        (4, 5),
        (5, 4),
        (6, 7),
        (7, 6),
    ] {
        b[dst] = byte(src);
    }
    for (i, slot) in b.iter_mut().enumerate().skip(8) {
        *slot = byte(i);
    }
    b
}

fn with_size(mut body: Vec<u8>) -> Vec<u8> {
    let n = (body.len() + 2) as u16;
    let mut v = n.to_le_bytes().to_vec();
    v.append(&mut body);
    v
}

/// A root-folder item (`0x1F`) for a shell-folder GUID.
pub fn root_item(guid: &str) -> Vec<u8> {
    let mut b = vec![0x1F, 0x50];
    b.extend(guid_bytes(guid));
    with_size(b)
}

/// "My Computer".
pub fn my_computer() -> Vec<u8> {
    root_item("20D04FE0-3AEA-1069-A2D8-08002B30309D")
}

/// A drive item (`0x2F`, e.g. `"C:\\"`).
pub fn volume_item(drive: &str) -> Vec<u8> {
    let mut b = vec![0x2F];
    let mut name = drive.as_bytes().to_vec();
    name.resize(22, 0);
    b.extend(name);
    with_size(b)
}

/// Options for a file-entry item.
#[derive(Debug, Clone)]
pub struct FileItem<'a> {
    pub short_name: &'a str,
    pub long_name: &'a str,
    pub is_dir: bool,
    pub size: u32,
    pub modified: (u16, u16),
    pub created: (u16, u16),
    pub accessed: (u16, u16),
    /// Extension-block version (3 = XP, 7 = Vista, 8 = 7, 9 = 8+).
    pub version: u16,
    /// NTFS file reference (entry, sequence), written for version ≥ 7.
    pub mft: (u64, u16),
}

/// A file-entry item (`0x31`/`0x32`) with a `0xBEEF0004` extension block.
pub fn file_item(f: &FileItem<'_>) -> Vec<u8> {
    let mut b = vec![if f.is_dir { 0x31 } else { 0x32 }, 0];
    b.extend(f.size.to_le_bytes());
    b.extend(f.modified.0.to_le_bytes());
    b.extend(f.modified.1.to_le_bytes());
    b.extend((if f.is_dir { 0x10u16 } else { 0x20 }).to_le_bytes());
    b.extend(f.short_name.as_bytes());
    b.push(0);
    if b.len() % 2 == 1 {
        b.push(0);
    }
    let ext_start = b.len() + 2;
    let mut e = Vec::new();
    e.extend(0u16.to_le_bytes()); // size, patched
    e.extend(f.version.to_le_bytes());
    e.extend(0xBEEF_0004u32.to_le_bytes());
    e.extend(f.created.0.to_le_bytes());
    e.extend(f.created.1.to_le_bytes());
    e.extend(f.accessed.0.to_le_bytes());
    e.extend(f.accessed.1.to_le_bytes());
    e.extend(0x14u16.to_le_bytes());
    if f.version >= 7 {
        e.extend(0u16.to_le_bytes());
        e.extend(&f.mft.0.to_le_bytes()[..6]);
        e.extend(f.mft.1.to_le_bytes());
        e.extend([0u8; 8]);
    }
    e.extend(0u16.to_le_bytes()); // long string size
    if f.version >= 9 {
        e.extend([0u8; 4]);
    }
    if f.version >= 8 {
        e.extend([0u8; 4]);
    }
    for u in f.long_name.encode_utf16() {
        e.extend(u.to_le_bytes());
    }
    e.extend([0, 0]);
    e.extend((ext_start as u16).to_le_bytes());
    let n = e.len() as u16;
    e[..2].copy_from_slice(&n.to_le_bytes());
    b.extend(e);
    with_size(b)
}

/// A network-location item (`0x41`).
pub fn network_item(location: &str) -> Vec<u8> {
    let mut b = vec![0x41, 0, 0x02];
    b.extend(location.as_bytes());
    b.extend([0, 0, 0]);
    with_size(b)
}

/// An ID list: the items plus the zero terminator.
pub fn id_list(items: &[Vec<u8>]) -> Vec<u8> {
    let mut v: Vec<u8> = items.concat();
    v.extend([0, 0]);
    v
}

/// Builds a `.lnk` file.
#[derive(Debug, Clone, Default)]
pub struct LnkBuilder {
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
    pub file_size: u32,
    pub attributes: u32,
    /// Raw ID list (see [`id_list`]).
    pub id_list: Option<Vec<u8>>,
    pub local_base_path: Option<String>,
    /// Also write Unicode copies of the base path / suffix.
    pub unicode_paths: bool,
    pub net_name: Option<String>,
    pub common_path_suffix: String,
    pub volume_label: String,
    pub drive_serial: u32,
    pub description: Option<String>,
    pub relative_path: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    /// Machine id and file droid (a version-1 UUID string).
    pub tracker: Option<(String, String)>,
    pub environment_target: Option<String>,
}

impl LnkBuilder {
    /// Serialises the link.
    pub fn build(&self) -> Vec<u8> {
        let mut flags = 0x80u32; // unicode strings
        let mut out = vec![0u8; 0x4C];
        out[..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        out[4..20].copy_from_slice(&[
            0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x46,
        ]);
        out[0x18..0x1C].copy_from_slice(&self.attributes.to_le_bytes());
        out[0x1C..0x24].copy_from_slice(&self.created.to_le_bytes());
        out[0x24..0x2C].copy_from_slice(&self.accessed.to_le_bytes());
        out[0x2C..0x34].copy_from_slice(&self.modified.to_le_bytes());
        out[0x34..0x38].copy_from_slice(&self.file_size.to_le_bytes());
        out[0x3C..0x40].copy_from_slice(&1u32.to_le_bytes());
        if let Some(ids) = &self.id_list {
            flags |= 0x01;
            out.extend((ids.len() as u16).to_le_bytes());
            out.extend(ids);
        }
        if self.local_base_path.is_some() || self.net_name.is_some() {
            flags |= 0x02;
            out.extend(self.link_info());
        }
        for (flag, s) in [
            (0x04, &self.description),
            (0x08, &self.relative_path),
            (0x10, &self.working_dir),
            (0x20, &self.arguments),
        ] {
            if let Some(s) = s {
                flags |= flag;
                let u: Vec<u16> = s.encode_utf16().collect();
                out.extend((u.len() as u16).to_le_bytes());
                for c in u {
                    out.extend(c.to_le_bytes());
                }
            }
        }
        if let Some(t) = &self.environment_target {
            let mut blk = vec![0u8; 788];
            blk[..4].copy_from_slice(&788u32.to_le_bytes());
            blk[4..8].copy_from_slice(&0xA000_0001u32.to_le_bytes());
            blk[8..8 + t.len()].copy_from_slice(t.as_bytes());
            for (i, u) in t.encode_utf16().enumerate() {
                blk[268 + i * 2..270 + i * 2].copy_from_slice(&u.to_le_bytes());
            }
            out.extend(blk);
        }
        if let Some((machine, droid)) = &self.tracker {
            let mut blk = vec![0u8; 0x60];
            blk[..4].copy_from_slice(&0x60u32.to_le_bytes());
            blk[4..8].copy_from_slice(&0xA000_0003u32.to_le_bytes());
            blk[8..12].copy_from_slice(&0x58u32.to_le_bytes());
            blk[16..16 + machine.len()].copy_from_slice(machine.as_bytes());
            let g = guid_bytes(droid);
            blk[32..48].copy_from_slice(&g);
            blk[48..64].copy_from_slice(&g);
            blk[64..80].copy_from_slice(&g);
            blk[80..96].copy_from_slice(&g);
            out.extend(blk);
        }
        out.extend([0u8; 4]);
        out[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        out
    }

    fn link_info(&self) -> Vec<u8> {
        let header = if self.unicode_paths { 0x24usize } else { 0x1C };
        let mut body = Vec::new();
        let mut li_flags = 0u32;
        let mut vol_off = 0;
        let mut base_off = 0;
        let mut cnrl_off = 0;
        let mut base_u_off = 0;
        if let Some(base) = &self.local_base_path {
            li_flags |= 1;
            vol_off = header + body.len();
            let mut v = vec![0u8; 0x10];
            v[4..8].copy_from_slice(&3u32.to_le_bytes());
            v[8..12].copy_from_slice(&self.drive_serial.to_le_bytes());
            v[12..16].copy_from_slice(&0x10u32.to_le_bytes());
            v.extend(self.volume_label.as_bytes());
            v.push(0);
            let n = v.len() as u32;
            v[..4].copy_from_slice(&n.to_le_bytes());
            body.extend(v);
            base_off = header + body.len();
            body.extend(base.as_bytes());
            body.push(0);
            if self.unicode_paths {
                base_u_off = header + body.len();
                for u in base.encode_utf16() {
                    body.extend(u.to_le_bytes());
                }
                body.extend([0, 0]);
            }
        }
        if let Some(net) = &self.net_name {
            li_flags |= 2;
            cnrl_off = header + body.len();
            let mut c = vec![0u8; 0x14];
            c[4..8].copy_from_slice(&1u32.to_le_bytes());
            c[8..12].copy_from_slice(&0x14u32.to_le_bytes());
            c.extend(net.as_bytes());
            c.push(0);
            let n = c.len() as u32;
            c[..4].copy_from_slice(&n.to_le_bytes());
            body.extend(c);
        }
        let suffix_off = header + body.len();
        body.extend(self.common_path_suffix.as_bytes());
        body.push(0);
        let mut suffix_u_off = 0;
        if self.unicode_paths {
            suffix_u_off = header + body.len();
            for u in self.common_path_suffix.encode_utf16() {
                body.extend(u.to_le_bytes());
            }
            body.extend([0, 0]);
        }
        let mut h = vec![0u8; header];
        let total = (header + body.len()) as u32;
        h[..4].copy_from_slice(&total.to_le_bytes());
        h[4..8].copy_from_slice(&(header as u32).to_le_bytes());
        h[8..12].copy_from_slice(&li_flags.to_le_bytes());
        h[12..16].copy_from_slice(&(vol_off as u32).to_le_bytes());
        h[16..20].copy_from_slice(&(base_off as u32).to_le_bytes());
        h[20..24].copy_from_slice(&(cnrl_off as u32).to_le_bytes());
        h[24..28].copy_from_slice(&(suffix_off as u32).to_le_bytes());
        if self.unicode_paths {
            h[28..32].copy_from_slice(&(base_u_off as u32).to_le_bytes());
            h[32..36].copy_from_slice(&(suffix_u_off as u32).to_le_bytes());
        }
        h.extend(body);
        h
    }
}

/// A version-3 compound file (512-byte sectors) holding `streams` under
/// the root storage; streams under 4096 bytes go to the mini stream.
pub fn compound_file(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const SS: usize = 512;
    const END: u32 = 0xFFFF_FFFE;
    let mut sectors: Vec<Vec<u8>> = Vec::new();
    let mut fat: Vec<u32> = Vec::new();
    let alloc_chain = |data: &[u8], sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<u32>| -> u32 {
        if data.is_empty() {
            return END;
        }
        let start = sectors.len() as u32;
        let chunks: Vec<&[u8]> = data.chunks(SS).collect();
        for (i, c) in chunks.iter().enumerate() {
            let mut s = c.to_vec();
            s.resize(SS, 0);
            sectors.push(s);
            fat.push(if i + 1 == chunks.len() {
                END
            } else {
                start + i as u32 + 1
            });
        }
        start
    };
    // Mini stream.
    let mut mini = Vec::new();
    let mut minifat: Vec<u32> = Vec::new();
    let mut starts = Vec::new();
    for (_, data) in streams {
        if data.len() < 4096 && !data.is_empty() {
            let start = (mini.len() / 64) as u32;
            let n = data.len().div_ceil(64);
            for i in 0..n {
                minifat.push(if i + 1 == n {
                    END
                } else {
                    start + i as u32 + 1
                });
            }
            let mut d = data.clone();
            d.resize(n * 64, 0);
            mini.extend(d);
            starts.push(start);
        } else {
            starts.push(u32::MAX);
        }
    }
    for (i, (_, data)) in streams.iter().enumerate() {
        if starts[i] == u32::MAX {
            starts[i] = alloc_chain(data, &mut sectors, &mut fat);
        }
    }
    let mini_start = alloc_chain(&mini, &mut sectors, &mut fat);
    let mf_bytes: Vec<u8> = minifat.iter().flat_map(|x| x.to_le_bytes()).collect();
    let minifat_start = alloc_chain(&mf_bytes, &mut sectors, &mut fat);
    // Directory: root, then streams linked through `right`.
    let mut dir = Vec::new();
    let entry = |name: &str, kind: u8, child: u32, right: u32, start: u32, size: u64| {
        let mut e = vec![0u8; 128];
        let u: Vec<u16> = name.encode_utf16().collect();
        for (i, c) in u.iter().enumerate() {
            e[i * 2..i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
        e[64..66].copy_from_slice(&(((u.len() + 1) * 2) as u16).to_le_bytes());
        e[66] = kind;
        e[67] = 1;
        e[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
        e[72..76].copy_from_slice(&right.to_le_bytes());
        e[76..80].copy_from_slice(&child.to_le_bytes());
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..128].copy_from_slice(&size.to_le_bytes());
        e
    };
    let first_child = if streams.is_empty() { u32::MAX } else { 1 };
    dir.extend(entry(
        "Root Entry",
        5,
        first_child,
        u32::MAX,
        mini_start,
        mini.len() as u64,
    ));
    for (i, (name, data)) in streams.iter().enumerate() {
        let right = if i + 1 < streams.len() {
            i as u32 + 2
        } else {
            u32::MAX
        };
        dir.extend(entry(
            name,
            2,
            u32::MAX,
            right,
            starts[i],
            data.len() as u64,
        ));
    }
    let dir_start = alloc_chain(&dir, &mut sectors, &mut fat);
    // FAT sectors (enough to describe themselves).
    let mut fat_count = 1;
    while (sectors.len() + fat_count) > fat_count * (SS / 4) {
        fat_count += 1;
    }
    let fat_first = sectors.len() as u32;
    fat.extend(std::iter::repeat_n(0xFFFF_FFFD, fat_count));
    fat.resize(fat_count * SS / 4, 0xFFFF_FFFF);
    let fat_bytes: Vec<u8> = fat.iter().flat_map(|x| x.to_le_bytes()).collect();
    for c in fat_bytes.chunks(SS) {
        sectors.push(c.to_vec());
    }
    let mut h = vec![0u8; 512];
    h[..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    h[24..26].copy_from_slice(&0x3Eu16.to_le_bytes());
    h[26..28].copy_from_slice(&3u16.to_le_bytes());
    h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes());
    h[30..32].copy_from_slice(&9u16.to_le_bytes());
    h[32..34].copy_from_slice(&6u16.to_le_bytes());
    h[44..48].copy_from_slice(&(fat_count as u32).to_le_bytes());
    h[48..52].copy_from_slice(&dir_start.to_le_bytes());
    h[56..60].copy_from_slice(&4096u32.to_le_bytes());
    h[60..64].copy_from_slice(&minifat_start.to_le_bytes());
    h[64..68].copy_from_slice(&(minifat.len().div_ceil(128) as u32).to_le_bytes());
    h[68..72].copy_from_slice(&END.to_le_bytes());
    for i in 0..109 {
        let v = if i < fat_count {
            fat_first + i as u32
        } else {
            u32::MAX
        };
        h[76 + i * 4..80 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    let mut out = h;
    for s in sectors {
        out.extend(s);
    }
    out
}

/// One `DestList` record to write.
#[derive(Debug, Clone)]
pub struct DestEntry<'a> {
    pub entry_number: u32,
    pub hostname: &'a str,
    pub last_access: u64,
    pub pinned: bool,
    pub access_count: u32,
    pub path: &'a str,
}

/// A `DestList` stream (`win10` selects the version-3 layout).
pub fn dest_list(entries: &[DestEntry<'_>], win10: bool) -> Vec<u8> {
    let mut out = vec![0u8; 32];
    out[..4].copy_from_slice(&(if win10 { 3u32 } else { 1 }).to_le_bytes());
    out[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    let (len_at, fixed, trailer) = if win10 {
        (0x80, 0x82, 4)
    } else {
        (0x70, 0x72, 0)
    };
    for e in entries {
        let mut r = vec![0u8; fixed];
        r[0x48..0x48 + e.hostname.len()].copy_from_slice(e.hostname.as_bytes());
        r[0x58..0x5C].copy_from_slice(&e.entry_number.to_le_bytes());
        r[0x60..0x68].copy_from_slice(&e.last_access.to_le_bytes());
        r[0x68..0x6C].copy_from_slice(&(if e.pinned { 0i32 } else { -1 }).to_le_bytes());
        if win10 {
            r[0x74..0x78].copy_from_slice(&e.access_count.to_le_bytes());
        }
        let u: Vec<u16> = e.path.encode_utf16().collect();
        r[len_at..len_at + 2].copy_from_slice(&(u.len() as u16).to_le_bytes());
        for c in u {
            r.extend(c.to_le_bytes());
        }
        r.extend(vec![0u8; trailer]);
        out.extend(r);
    }
    out
}

/// A `*.customDestinations-ms` file holding `links`.
pub fn custom_destinations(links: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(2u32.to_le_bytes());
    out.extend(1u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend((links.len() as u32).to_le_bytes());
    for l in links {
        out.extend(l);
    }
    out.extend(0xBABF_FBABu32.to_le_bytes());
    out
}
