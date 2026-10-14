//! Raw disk images with DOS/MBR (incl. extended chains) and GPT partitioning.

use uuid::Uuid;

/// A primary or logical MBR partition entry, in sectors.
#[derive(Debug, Clone, Copy)]
pub struct MbrPart {
    pub boot: bool,
    pub ptype: u8,
    pub start: u32,
    pub sectors: u32,
}

impl MbrPart {
    pub fn new(ptype: u8, start: u32, sectors: u32) -> Self {
        Self {
            boot: false,
            ptype,
            start,
            sectors,
        }
    }
}

/// A GPT partition entry, in LBAs (inclusive `last`).
#[derive(Debug, Clone)]
pub struct GptPart {
    pub type_guid: Uuid,
    pub unique_guid: Uuid,
    pub first: u64,
    pub last: u64,
    pub attrs: u64,
    pub name: String,
}

impl GptPart {
    pub fn new(type_guid: Uuid, first: u64, last: u64, name: &str) -> Self {
        Self {
            type_guid,
            unique_guid: Uuid::from_u128(
                0x1111_2222_3333_4444_5555_6666_0000_0000 | u128::from(first),
            ),
            first,
            last,
            attrs: 0,
            name: name.to_owned(),
        }
    }
}

/// Builds a raw disk image in memory.
#[derive(Debug, Clone)]
pub struct DiskBuilder {
    data: Vec<u8>,
    sector_size: usize,
}

impl DiskBuilder {
    /// A zero-filled disk of `sectors` sectors of `sector_size` bytes.
    pub fn new(sector_size: usize, sectors: u64) -> Self {
        Self {
            data: vec![0; sector_size * sectors as usize],
            sector_size,
        }
    }

    pub fn sector_size(&self) -> usize {
        self.sector_size
    }

    pub fn sectors(&self) -> u64 {
        (self.data.len() / self.sector_size) as u64
    }

    /// Writes raw bytes at a byte offset.
    pub fn write(&mut self, offset: usize, bytes: &[u8]) -> &mut Self {
        self.data[offset..offset + bytes.len()].copy_from_slice(bytes);
        self
    }

    /// Writes a DOS partition table (up to four entries) into `sector`.
    pub fn mbr_table(&mut self, sector: u64, parts: &[MbrPart]) -> &mut Self {
        assert!(parts.len() <= 4);
        let base = sector as usize * self.sector_size;
        for (i, p) in parts.iter().enumerate() {
            let e = base + 446 + i * 16;
            self.data[e] = if p.boot { 0x80 } else { 0 };
            self.data[e + 4] = p.ptype;
            self.data[e + 8..e + 12].copy_from_slice(&p.start.to_le_bytes());
            self.data[e + 12..e + 16].copy_from_slice(&p.sectors.to_le_bytes());
        }
        self.data[base + 510] = 0x55;
        self.data[base + 511] = 0xAA;
        self
    }

    /// Writes a protective MBR plus primary and backup GPT.
    pub fn gpt(&mut self, parts: &[GptPart]) -> &mut Self {
        let ss = self.sector_size;
        let last_lba = self.sectors() - 1;
        let total = u32::try_from(self.sectors() - 1).unwrap_or(u32::MAX);
        self.mbr_table(0, &[MbrPart::new(0xEE, 1, total)]);

        let num_entries = 128u32;
        let entry_size = 128u32;
        let entries_bytes = (num_entries * entry_size) as usize;
        let entry_sectors = entries_bytes.div_ceil(ss) as u64;
        let mut entries = vec![0u8; entries_bytes];
        for (i, p) in parts.iter().enumerate() {
            let e = &mut entries[i * entry_size as usize..][..entry_size as usize];
            e[0..16].copy_from_slice(&p.type_guid.to_bytes_le());
            e[16..32].copy_from_slice(&p.unique_guid.to_bytes_le());
            e[32..40].copy_from_slice(&p.first.to_le_bytes());
            e[40..48].copy_from_slice(&p.last.to_le_bytes());
            e[48..56].copy_from_slice(&p.attrs.to_le_bytes());
            for (j, u) in p.name.encode_utf16().take(36).enumerate() {
                e[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        let entries_crc = crc32fast::hash(&entries);
        let first_usable = 2 + entry_sectors;
        let last_usable = last_lba - 1 - entry_sectors;
        let disk_guid = Uuid::from_u128(0xD15C_0000_0000_0000_0000_0000_0000_0001);

        let header = |my: u64, alt: u64, entries_lba: u64| {
            let mut h = vec![0u8; ss];
            h[0..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&my.to_le_bytes());
            h[32..40].copy_from_slice(&alt.to_le_bytes());
            h[40..48].copy_from_slice(&first_usable.to_le_bytes());
            h[48..56].copy_from_slice(&last_usable.to_le_bytes());
            h[56..72].copy_from_slice(&disk_guid.to_bytes_le());
            h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
            h[80..84].copy_from_slice(&num_entries.to_le_bytes());
            h[84..88].copy_from_slice(&entry_size.to_le_bytes());
            h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
            let crc = crc32fast::hash(&h[..92]);
            h[16..20].copy_from_slice(&crc.to_le_bytes());
            h
        };
        let backup_entries_lba = last_lba - entry_sectors;
        let primary = header(1, last_lba, 2);
        let backup = header(last_lba, 1, backup_entries_lba);
        self.write(ss, &primary);
        self.write(2 * ss, &entries);
        self.write(backup_entries_lba as usize * ss, &entries);
        self.write(last_lba as usize * ss, &backup);
        self
    }

    /// Fills a sector range with a recognisable byte (to verify slicing).
    pub fn fill_sectors(&mut self, start: u64, count: u64, byte: u8) -> &mut Self {
        let s = start as usize * self.sector_size;
        let e = s + count as usize * self.sector_size;
        self.data[s..e].fill(byte);
        self
    }

    pub fn build(&self) -> Vec<u8> {
        self.data.clone()
    }
}
