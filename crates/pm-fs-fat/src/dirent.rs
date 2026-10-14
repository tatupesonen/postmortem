//! 32-byte directory entries and long-file-name assembly.

use pm_core::Bytes;

pub(crate) const ENTRY_SIZE: usize = 32;
pub(crate) const ATTR_READ_ONLY: u8 = 0x01;
pub(crate) const ATTR_VOLUME_ID: u8 = 0x08;
pub(crate) const ATTR_DIRECTORY: u8 = 0x10;
pub(crate) const ATTR_LFN: u8 = 0x0F;
const DELETED: u8 = 0xE5;

/// A decoded short (8.3) directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShortEntry {
    pub(crate) raw_name: [u8; 11],
    pub(crate) attr: u8,
    pub(crate) case_flags: u8,
    pub(crate) crt_tenths: u8,
    pub(crate) crt_time: u16,
    pub(crate) crt_date: u16,
    pub(crate) acc_date: u16,
    pub(crate) wrt_time: u16,
    pub(crate) wrt_date: u16,
    pub(crate) cluster: u32,
    pub(crate) size: u32,
    pub(crate) deleted: bool,
}

impl ShortEntry {
    pub(crate) fn parse(e: &[u8], fat32: bool) -> Option<Self> {
        let raw_name: [u8; 11] = e.array(0).ok()?;
        let hi = if fat32 {
            u32::from(e.u16_le(20).ok()?)
        } else {
            0
        };
        Some(Self {
            raw_name,
            attr: e.u8_at(11).ok()?,
            case_flags: e.u8_at(12).ok()?,
            crt_tenths: e.u8_at(13).ok()?,
            crt_time: e.u16_le(14).ok()?,
            crt_date: e.u16_le(16).ok()?,
            acc_date: e.u16_le(18).ok()?,
            wrt_time: e.u16_le(22).ok()?,
            wrt_date: e.u16_le(24).ok()?,
            cluster: (hi << 16) | u32::from(e.u16_le(26).ok()?),
            size: e.u32_le(28).ok()?,
            deleted: raw_name[0] == DELETED,
        })
    }

    pub(crate) fn is_dir(&self) -> bool {
        self.attr & ATTR_DIRECTORY != 0
    }

    pub(crate) fn is_volume_label(&self) -> bool {
        self.attr & ATTR_VOLUME_ID != 0 && self.attr & ATTR_LFN != ATTR_LFN
    }

    pub(crate) fn is_dot(&self) -> bool {
        self.raw_name[0] == b'.'
            && (self.raw_name[1] == b' ' || (self.raw_name[1] == b'.' && self.raw_name[2] == b' '))
    }

    /// 8.3 name as displayed. Deleted entries show `_` for the lost first
    /// character (TSK convention); `0x05` encodes a literal `0xE5`.
    pub(crate) fn short_name(&self) -> String {
        let mut base: [u8; 8] = [0; 8];
        base.copy_from_slice(&self.raw_name[..8]);
        if self.deleted {
            base[0] = b'_';
        } else if base[0] == 0x05 {
            base[0] = 0xE5;
        }
        let decode = |b: &[u8], lower: bool| {
            let s: String = b
                .iter()
                .map(|&c| cp437(c))
                .collect::<String>()
                .trim_end()
                .to_owned();
            if lower { s.to_lowercase() } else { s }
        };
        let base = decode(&base, self.case_flags & 0x08 != 0);
        let ext = decode(&self.raw_name[8..], self.case_flags & 0x10 != 0);
        if self.is_volume_label() {
            return format!("{base}{ext}");
        }
        if ext.is_empty() {
            base
        } else {
            format!("{base}.{ext}")
        }
    }

    /// Checksum of the 11-byte short name used to bind LFN entries.
    pub(crate) fn lfn_checksum(&self) -> u8 {
        let mut name = self.raw_name;
        if self.deleted {
            // The original first byte is lost; the checksum cannot be
            // recomputed exactly, so callers use `lfn_checksum_matches`.
            name[0] = DELETED;
        }
        checksum(&name)
    }
}

pub(crate) fn checksum(name: &[u8; 11]) -> u8 {
    name.iter()
        .fold(0u8, |sum, &c| sum.rotate_right(1).wrapping_add(c))
}

/// One LFN slot.
#[derive(Debug, Clone)]
pub(crate) struct LfnEntry {
    pub(crate) seq: u8,
    pub(crate) checksum: u8,
    pub(crate) chars: [u16; 13],
}

impl LfnEntry {
    pub(crate) fn parse(e: &[u8]) -> Option<Self> {
        let mut chars = [0u16; 13];
        let offsets = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        for (c, off) in chars.iter_mut().zip(offsets) {
            *c = e.u16_le(off).ok()?;
        }
        Some(Self {
            seq: e.u8_at(0).ok()?,
            checksum: e.u8_at(13).ok()?,
            chars,
        })
    }
}

/// Accumulates LFN slots preceding a short entry.
#[derive(Debug, Default)]
pub(crate) struct LfnBuilder {
    parts: Vec<LfnEntry>,
}

impl LfnBuilder {
    pub(crate) fn push(&mut self, e: LfnEntry) {
        // A new "last" slot (0x40) starts a fresh name.
        if e.seq & 0x40 != 0 && e.seq != DELETED {
            self.parts.clear();
        }
        self.parts.push(e);
    }

    pub(crate) fn clear(&mut self) {
        self.parts.clear();
    }

    /// Completes the name for `short`, if the collected slots belong to it.
    pub(crate) fn take(&mut self, short: &ShortEntry) -> Option<String> {
        let parts = std::mem::take(&mut self.parts);
        if parts.is_empty() {
            return None;
        }
        let matches = |sum: u8| {
            if short.deleted {
                // Try every possible first byte of the lost short name.
                let mut n = short.raw_name;
                (0u8..=255).any(|b| {
                    n[0] = b;
                    checksum(&n) == sum
                })
            } else {
                short.lfn_checksum() == sum
            }
        };
        let sum = parts.first()?.checksum;
        if !parts.iter().all(|p| p.checksum == sum) || !matches(sum) {
            return None;
        }
        // Slots are stored last-first on disk.
        let units: Vec<u16> = parts
            .iter()
            .rev()
            .flat_map(|p| p.chars)
            .take_while(|&u| u != 0x0000)
            .filter(|&u| u != 0xFFFF)
            .collect();
        let name: String = char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        (!name.is_empty()).then_some(name)
    }
}

/// Heuristic validity check for entries found after the end-of-directory
/// marker or in deleted directories (avoids treating garbage as files).
pub(crate) fn plausible(e: &ShortEntry, cluster_count: u32) -> bool {
    if e.attr & 0xC0 != 0 {
        return false;
    }
    let name_ok = e.raw_name.iter().enumerate().all(|(i, &c)| {
        (i == 0 && (c == DELETED || c == 0x05)) || (c >= 0x20 && !b"\"*+,/:;<=>?[\\]|".contains(&c))
    });
    let cluster_ok = e.cluster == 0 || e.cluster < cluster_count + 2;
    let date_ok =
        |d: u16| d == 0 || (((d >> 5) & 0xF) as u8 >= 1 && ((d >> 5) & 0xF) <= 12 && d & 0x1F >= 1);
    name_ok && cluster_ok && date_ok(e.wrt_date) && date_ok(e.crt_date)
}

/// Whether a sector found in unallocated space looks like part of a
/// directory: at least one plausible short entry and no implausible ones.
pub(crate) fn sector_is_dir(sector: &[u8], cluster_count: u32) -> bool {
    let mut good = 0;
    for e in sector.as_chunks::<ENTRY_SIZE>().0 {
        if e[0] == 0 {
            continue;
        }
        if e[11] & ATTR_LFN == ATTR_LFN {
            // LFN slot: sequence number (or deletion mark) must be sane.
            let seq = e[0];
            let index = seq & 0x1F;
            if seq != DELETED && (index == 0 || index > 20 || seq & 0xA0 != 0) {
                return false;
            }
            continue;
        }
        match ShortEntry::parse(e, true) {
            Some(s) if plausible(&s, cluster_count) => {
                if !s.is_dot() {
                    good += 1;
                }
            }
            _ => return false,
        }
    }
    good > 0
}

/// Maps a code page 437 byte to Unicode (ASCII range passes through).
fn cp437(b: u8) -> char {
    const HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
    if b < 0x80 {
        char::from(b)
    } else {
        HIGH.chars().nth(usize::from(b - 0x80)).unwrap_or('?')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short(name: &[u8; 11], attr: u8) -> ShortEntry {
        let mut e = [0u8; 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        ShortEntry::parse(&e, true).unwrap()
    }

    #[test]
    fn short_names() {
        assert_eq!(short(b"README  TXT", 0).short_name(), "README.TXT");
        assert_eq!(short(b"NOEXT      ", 0).short_name(), "NOEXT");
        assert_eq!(short(b"\xE5ELETED TXT", 0).short_name(), "_ELETED.TXT");
        assert_eq!(short(b"\x05BC     TXT", 0).short_name(), "\u{3c3}BC.TXT");
        assert_eq!(
            short(b"MY LABEL   ", ATTR_VOLUME_ID).short_name(),
            "MY LABEL"
        );
        assert!(short(b".          ", ATTR_DIRECTORY).is_dot());
        assert!(short(b"..         ", ATTR_DIRECTORY).is_dot());
        assert!(!short(b"..A        ", 0).is_dot());
    }

    #[test]
    fn case_flags_lowercase() {
        let mut e = short(b"README  TXT", 0);
        e.case_flags = 0x18;
        assert_eq!(e.short_name(), "readme.txt");
    }

    #[test]
    fn checksum_known_value() {
        // Checksum of "FOO     BAR" computed with the reference algorithm.
        let mut sum: u8 = 0;
        for &c in b"FOO     BAR" {
            sum = (if sum & 1 == 1 { 0x80 } else { 0 }) + (sum >> 1);
            sum = sum.wrapping_add(c);
        }
        assert_eq!(checksum(b"FOO     BAR"), sum);
    }

    #[test]
    fn plausibility() {
        assert!(plausible(&short(b"README  TXT", 0x20), 100));
        assert!(!plausible(&short(b"BAD*NAMETXT", 0), 100));
        assert!(!plausible(&short(b"README  TXT", 0xC0), 100));
    }
}
