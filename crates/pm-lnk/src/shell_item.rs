//! Shell items and item ID lists (`SHITEMID`), as stored in `.lnk` target
//! ID lists and in the registry's shellbags.
//!
//! Each item is decoded into a display name plus whatever timestamps and
//! file-system references it carries (DOS times of file entries, the
//! `0xBEEF0004` extension block with creation/access times, the long name
//! and the NTFS file reference).

use jiff::tz::TimeZone;
use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

/// Most items decoded from one ID list.
const MAX_ITEMS: usize = 512;

/// What an item describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A shell folder identified by a GUID (`0x1F`): My Computer, …
    Root,
    /// A drive (`0x2X`).
    Volume,
    /// A file or directory (`0x3X`).
    File,
    /// A network location (`0x4X`).
    Network,
    /// A URI (`0x61`).
    Uri,
    /// A control-panel item (`0x71`).
    ControlPanel,
    /// A delegate item wrapping a file entry (`0x74`, `CFSF`).
    Delegate,
    /// Anything else.
    Unknown,
}

/// A decoded shell item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellItem {
    /// The class-type byte.
    pub class_type: u8,
    pub kind: Option<ItemKind>,
    /// Name used in paths (long name when known).
    pub name: String,
    /// 8.3 name of a file entry.
    pub short_name: Option<String>,
    /// GUID of root / control-panel items, `{XXXXXXXX-…}`.
    pub guid: Option<String>,
    /// Unix seconds (DOS times are read as UTC).
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    /// NTFS file reference from the extension block.
    pub mft_entry: Option<u64>,
    pub mft_sequence: Option<u16>,
    pub file_size: Option<u32>,
    pub is_directory: bool,
}

/// Parses an ID list: items (each starting with its 16-bit size) until a
/// zero size or the end of `data`.
pub fn parse_id_list(data: &[u8]) -> Vec<ShellItem> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < MAX_ITEMS {
        let Ok(size) = data.u16_le(at) else { break };
        let size = usize::from(size);
        if size < 2 {
            break;
        }
        let Some(item) = data.get(at..at + size) else {
            break;
        };
        out.push(parse_item(item));
        at += size;
    }
    out
}

/// Decodes one item (`data` includes the leading 16-bit size).
pub fn parse_item(data: &[u8]) -> ShellItem {
    let class_type = data.get(2).copied().unwrap_or(0);
    let mut it = ShellItem {
        class_type,
        ..ShellItem::default()
    };
    match class_type {
        0x1F => root(data, &mut it),
        0x20..=0x2F => volume(data, &mut it),
        0x30..=0x3F => file_entry(data, &mut it),
        0x40..=0x4F => {
            it.kind = Some(ItemKind::Network);
            it.name = data.get(5..).map(ascii_z).unwrap_or_default();
        }
        0x61 => {
            it.kind = Some(ItemKind::Uri);
            it.name = best_string(data.get(4..).unwrap_or_default());
        }
        0x71 => {
            it.kind = Some(ItemKind::ControlPanel);
            if let Some(g) = data.get(14..30).and_then(guid_string) {
                it.name = known_folder(&g).map_or_else(|| format!("CLSID_{g}"), str::to_owned);
                it.guid = Some(g);
            }
        }
        0x74 if data.get(6..10) == Some(b"CFSF") => {
            it.kind = Some(ItemKind::Delegate);
            // The wrapped file entry starts after the CFSF header.
            if let Some(inner) = data.get(10..) {
                let mut tmp = ShellItem::default();
                file_entry_body(inner, &mut tmp, 0);
                it.short_name = tmp.short_name;
                it.modified = tmp.modified;
                it.file_size = tmp.file_size;
                it.is_directory = tmp.is_directory;
            }
            extension_block(data, &mut it);
            if it.name.is_empty() {
                it.name = it.short_name.clone().unwrap_or_default();
            }
        }
        _ => {
            it.kind = Some(ItemKind::Unknown);
            extension_block(data, &mut it);
            if it.name.is_empty() {
                it.name = best_string(data.get(3..).unwrap_or_default());
            }
            if it.name.is_empty() {
                it.name = format!("[Unknown 0x{class_type:02X}]");
            }
        }
    }
    it
}

fn root(data: &[u8], it: &mut ShellItem) {
    it.kind = Some(ItemKind::Root);
    match data.get(4..20).and_then(guid_string) {
        Some(g) => {
            it.name = known_folder(&g).map_or_else(|| format!("CLSID_{g}"), str::to_owned);
            it.guid = Some(g);
        }
        None => "[Root]".clone_into(&mut it.name),
    }
}

fn volume(data: &[u8], it: &mut ShellItem) {
    it.kind = Some(ItemKind::Volume);
    let s = data.get(3..).map(ascii_z).unwrap_or_default();
    let is_drive = s.len() >= 2
        && s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && s.as_bytes().get(1) == Some(&b':');
    if is_drive {
        it.name = s;
    } else if let Some(g) = data.get(4..20).and_then(guid_string) {
        it.name = known_folder(&g).map_or_else(|| format!("CLSID_{g}"), str::to_owned);
        it.guid = Some(g);
    } else {
        "[Volume]".clone_into(&mut it.name);
    }
}

fn file_entry(data: &[u8], it: &mut ShellItem) {
    it.kind = Some(ItemKind::File);
    file_entry_body(data, it, 0);
    extension_block(data, it);
    if it.name.is_empty() {
        it.name = it.short_name.clone().unwrap_or_default();
    }
}

/// The fixed part of a file entry at `base` (size field included).
fn file_entry_body(data: &[u8], it: &mut ShellItem, base: usize) {
    let ty = data.get(base + 2).copied().unwrap_or(0);
    it.is_directory = ty & 0x01 != 0;
    it.file_size = data.u32_le(base + 4).ok();
    it.modified = dos_time(data, base + 8);
    let name_at = base + 14;
    let short = if ty & 0x04 != 0 {
        data.get(name_at..).map(utf16le_lossy)
    } else {
        data.get(name_at..).map(ascii_z)
    };
    it.short_name = short.filter(|s| !s.is_empty());
}

/// The `0xBEEF0004` extension block: creation/access times, NTFS file
/// reference and the long name.
fn extension_block(data: &[u8], it: &mut ShellItem) {
    let Some(p) = find(data, &[0x04, 0x00, 0xEF, 0xBE]) else {
        return;
    };
    let Some(start) = p.checked_sub(4) else {
        return;
    };
    let Some(b) = data.get(start..) else { return };
    let version = b.u16_le(2).unwrap_or(0);
    it.created = dos_time(b, 8);
    it.accessed = dos_time(b, 12);
    if version >= 7
        && let Ok(r) = b.array::<8>(20)
    {
        let entry = u64::from_le_bytes([r[0], r[1], r[2], r[3], r[4], r[5], 0, 0]);
        if entry != 0 {
            it.mft_entry = Some(entry);
            it.mft_sequence = Some(u16::from_le_bytes([r[6], r[7]]));
        }
    }
    let name_at = match version {
        0..=2 => return,
        3..=6 => 20,
        7 => 38,
        8 => 42,
        _ => 46,
    };
    let name = b.get(name_at..).map(utf16le_lossy).unwrap_or_default();
    if !name.is_empty() && name.chars().all(|c| c != char::REPLACEMENT_CHARACTER) {
        it.name = name;
    }
}

/// A FAT date/time (date in the low word) as UTC Unix seconds.
fn dos_time(b: &[u8], at: usize) -> Option<i64> {
    let date = b.u16_le(at).ok()?;
    let time = b.u16_le(at + 2).ok()?;
    if date == 0 {
        return None;
    }
    let dt = pm_core::time::dos_datetime(date, time, 0)?;
    pm_core::time::local_to_timestamp(dt, &TimeZone::UTC).map(jiff::Timestamp::as_second)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn ascii_z(b: &[u8]) -> String {
    b.iter()
        .take_while(|&&c| c != 0)
        .map(|&c| char::from(c))
        .collect()
}

/// The longest printable ASCII or UTF-16 run (≥ 3 characters).
fn best_string(b: &[u8]) -> String {
    let mut best = String::new();
    let mut cur = String::new();
    for &c in b {
        if (0x20..0x7F).contains(&c) {
            cur.push(char::from(c));
        } else {
            if cur.len() > best.len() {
                best = std::mem::take(&mut cur);
            }
            cur.clear();
        }
    }
    if cur.len() > best.len() {
        best = cur;
    }
    for shift in 0..2 {
        let units: Vec<u16> = b
            .get(shift..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        for run in units.split(|&u| !(0x20..0xD800).contains(&u)) {
            if run.len() > best.chars().count() && run.len() >= 3 {
                best = String::from_utf16_lossy(run);
            }
        }
    }
    if best.chars().count() >= 3 {
        best
    } else {
        String::new()
    }
}

/// Formats a little-endian GUID as `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`
/// (`None` for all zeros).
pub fn guid_string(b: &[u8]) -> Option<String> {
    let g: [u8; 16] = b.get(..16)?.try_into().ok()?;
    if g.iter().all(|&x| x == 0) {
        return None;
    }
    Some(format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    ))
}

/// Display names of well-known shell folders.
pub fn known_folder(guid: &str) -> Option<&'static str> {
    const KNOWN: &[(&str, &str)] = &[
        ("{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "My Computer"),
        ("{450D8FBA-AD25-11D0-98A8-0800361B1103}", "My Documents"),
        (
            "{208D2C60-3AEA-1069-A2D7-08002B30309D}",
            "My Network Places",
        ),
        ("{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}", "Network"),
        ("{645FF040-5081-101B-9F08-00AA002F954E}", "Recycle Bin"),
        (
            "{871C5380-42A0-1069-A2EA-08002B30309D}",
            "Internet Explorer",
        ),
        ("{21EC2020-3AEA-1069-A2DD-08002B30309D}", "Control Panel"),
        ("{26EE0668-A00A-44D7-9371-BEB064C98683}", "Control Panel"),
        ("{5399E694-6CE5-4D6C-8FCE-1D8870FDCBA0}", "Control Panel"),
        ("{2227A280-3AEA-1069-A2DE-08002B30309D}", "Printers"),
        ("{59031A47-3F72-44A7-89C5-5595FE6B30EE}", "Users Files"),
        ("{031E4825-7B94-4DC3-B131-E946B44C8DD5}", "Libraries"),
        ("{B4BFCC3A-DB2C-424C-B029-7FE99A87C641}", "Desktop"),
        ("{D3162B92-9365-467A-956B-92703ACA08AF}", "Documents"),
        ("{FDD39AD0-238F-46AF-ADB4-6C85480369C7}", "Documents"),
        ("{088E3905-0323-4B02-9826-5D99428E115F}", "Downloads"),
        ("{374DE290-123F-4565-9164-39C4925E467B}", "Downloads"),
        ("{24AD3AD4-A569-4530-98E1-AB02F9417AA8}", "Pictures"),
        ("{3ADD1653-EB32-4CB0-BBD7-DFA0ABB5ACCA}", "Pictures"),
        ("{F86FA3AB-70D2-4FC7-9C99-FCBF05467F3A}", "Videos"),
        ("{A0953C92-50DC-43BF-BE83-3742FED03C9C}", "Videos"),
        ("{3DFDF296-DBEC-4FB4-81D1-6A3438BCF4DE}", "Music"),
        ("{1CF1260C-4DD0-4EBB-811F-33C572699FDE}", "Music"),
        ("{018D5C66-4533-4307-9B53-224DE2ED1FE6}", "OneDrive"),
        ("{679F85CB-0220-4080-B29B-5540CC05AAB6}", "Quick Access"),
        ("{22877A6D-37A1-461A-91B0-DBDA5AAEBC99}", "Recent Places"),
        (
            "{1F4DE370-D627-11D1-BA4F-00A0C91EEDBA}",
            "Search Results - Computers",
        ),
        ("{E17D4FC0-5564-11D1-83F2-00A0C90DC849}", "Search Results"),
        ("{9343812E-1C37-4A49-A12E-4B2D810D956B}", "Search Home"),
        ("{BD84B380-8CA2-1069-AB1D-08000948F534}", "Fonts"),
        (
            "{7007ACC7-3202-11D1-AAD2-00805FC1270E}",
            "Network Connections",
        ),
        (
            "{D20EA4E1-3957-11D2-A40B-0C5020524153}",
            "Administrative Tools",
        ),
        ("{4234D49B-0245-4DF3-B780-3893943456E1}", "Applications"),
        ("{C4AA340D-F20F-4863-AFEF-F87EF2E6BA25}", "Public Desktop"),
        ("{ED228FDF-9EA8-4870-83B1-96B02CFE0D52}", "My Games"),
    ];
    KNOWN
        .iter()
        .find(|(g, _)| g.eq_ignore_ascii_case(guid))
        .map(|(_, n)| *n)
}

/// Joins item names into a path (`My Computer\C:\Windows`), avoiding
/// doubled separators after drive roots.
pub fn join_path(items: &[ShellItem]) -> String {
    let mut out = String::new();
    for it in items.iter().filter(|i| !i.name.is_empty()) {
        if !out.is_empty() && !out.ends_with('\\') {
            out.push('\\');
        }
        out.push_str(&it.name);
    }
    out
}

/// The file-system path an ID list points to, as Autopsy's LNK parser
/// builds it: the drive and the names below it, without the leading
/// "My Computer" / "My Documents" shell folder.
pub fn target_path(items: &[ShellItem]) -> String {
    let start = usize::from(
        items
            .first()
            .is_some_and(|i| i.kind == Some(ItemKind::Root)),
    );
    join_path(items.get(start..).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids() {
        let b = [
            0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30,
            0x30, 0x9D,
        ];
        let g = guid_string(&b).unwrap();
        assert_eq!(g, "{20D04FE0-3AEA-1069-A2D8-08002B30309D}");
        assert_eq!(known_folder(&g), Some("My Computer"));
        assert_eq!(guid_string(&[0; 16]), None);
        assert_eq!(guid_string(&[1; 4]), None);
    }

    #[test]
    fn strings() {
        assert_eq!(best_string(b"\x01\x02hello\x00ab"), "hello");
        let u: Vec<u8> = "wide name"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(best_string(&u), "wide name");
        assert_eq!(best_string(b"ab"), "");
    }

    #[test]
    fn junk_never_panics() {
        for t in 0u8..=255 {
            let mut d = vec![0x20, 0, t];
            d.extend([0xFF; 29]);
            let _ = parse_item(&d);
            let _ = parse_item(&d[..3]);
        }
        assert!(parse_id_list(&[5, 0, 1]).is_empty());
    }
}
