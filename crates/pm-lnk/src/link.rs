//! MS-SHLLINK shell link (`.lnk`) parser.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

use crate::shell_item::{ShellItem, guid_string, parse_id_list, target_path};
use crate::{Error, Result};

const HEADER_SIZE: usize = 0x4C;
const LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];
/// Most extra-data blocks read.
const MAX_BLOCKS: usize = 64;

const HAS_ID_LIST: u32 = 0x01;
const HAS_LINK_INFO: u32 = 0x02;
const HAS_NAME: u32 = 0x04;
const HAS_RELATIVE_PATH: u32 = 0x08;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;

/// A parsed shell link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellLink {
    pub flags: u32,
    /// Target file attributes.
    pub file_attributes: u32,
    /// Target times (FILETIME) copied into the header.
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
    /// Target size (low 32 bits).
    pub file_size: u32,
    pub icon_index: i32,
    pub show_command: u32,
    pub id_list: Vec<ShellItem>,
    pub link_info: Option<LinkInfo>,
    /// Description string.
    pub name: Option<String>,
    pub relative_path: Option<String>,
    pub working_dir: Option<String>,
    pub arguments: Option<String>,
    pub icon_location: Option<String>,
    pub extra: ExtraData,
    /// Bytes the link occupies (useful when links are concatenated).
    pub len: usize,
}

/// The `LinkInfo` structure (target location).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkInfo {
    pub flags: u32,
    pub drive_type: Option<u32>,
    pub drive_serial: Option<u32>,
    pub volume_label: Option<String>,
    pub local_base_path: Option<String>,
    pub local_base_path_unicode: Option<String>,
    pub net_name: Option<String>,
    pub net_name_unicode: Option<String>,
    pub device_name: Option<String>,
    pub common_path_suffix: Option<String>,
    pub common_path_suffix_unicode: Option<String>,
}

/// Selected extra-data blocks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtraData {
    /// `EnvironmentVariableDataBlock` target.
    pub environment_target: Option<String>,
    pub tracker: Option<TrackerData>,
    /// `KnownFolderDataBlock` GUID.
    pub known_folder: Option<String>,
    /// `SpecialFolderDataBlock` CSIDL.
    pub special_folder: Option<u32>,
    /// `VistaAndAboveIDListDataBlock` items.
    pub vista_id_list: Vec<ShellItem>,
    /// `DarwinDataBlock` application identifier.
    pub darwin_id: Option<String>,
    /// `IconEnvironmentDataBlock` path.
    pub icon_environment: Option<String>,
}

/// The distributed link tracker block (`0xA0000003`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackerData {
    /// `NetBIOS` name of the machine the target was on.
    pub machine_id: String,
    pub droid_volume: Option<String>,
    pub droid_file: Option<String>,
    pub birth_droid_volume: Option<String>,
    pub birth_droid_file: Option<String>,
    /// MAC address from the (version 1 UUID) file droid.
    pub mac_address: Option<String>,
}

impl ShellLink {
    /// Parses a link from the start of `data`.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.u32_le(0).ok() != Some(HEADER_SIZE as u32)
            || data.get(4..20) != Some(&LINK_CLSID[..])
        {
            return Err(Error::NotLink);
        }
        let flags = data.u32_le(0x14)?;
        let mut l = Self {
            flags,
            file_attributes: data.u32_le(0x18)?,
            created: data.u64_le(0x1C)?,
            accessed: data.u64_le(0x24)?,
            modified: data.u64_le(0x2C)?,
            file_size: data.u32_le(0x34)?,
            icon_index: data.i32_le(0x38)?,
            show_command: data.u32_le(0x3C)?,
            ..Self::default()
        };
        let mut at = HEADER_SIZE;
        if flags & HAS_ID_LIST != 0 {
            let size = usize::from(data.u16_le(at)?);
            l.id_list = parse_id_list(data.sub(at + 2, size)?);
            at += 2 + size;
        }
        if flags & HAS_LINK_INFO != 0 {
            let size = data.u32_le(at)? as usize;
            if size < 4 {
                return Err(Error::Malformed("link info size"));
            }
            l.link_info = Some(link_info(data.sub(at, size)?));
            at += size;
        }
        let unicode = flags & IS_UNICODE != 0;
        for (flag, slot) in [
            (HAS_NAME, &mut l.name),
            (HAS_RELATIVE_PATH, &mut l.relative_path),
            (HAS_WORKING_DIR, &mut l.working_dir),
            (HAS_ARGUMENTS, &mut l.arguments),
            (HAS_ICON_LOCATION, &mut l.icon_location),
        ] {
            if flags & flag != 0 {
                let count = usize::from(data.u16_le(at)?);
                let bytes = if unicode { count * 2 } else { count };
                let raw = data.sub(at + 2, bytes)?;
                *slot = Some(if unicode {
                    utf16le_lossy(raw)
                } else {
                    ansi(raw)
                });
                at += 2 + bytes;
            }
        }
        at = extra_data(data, at, &mut l.extra);
        l.len = at;
        Ok(l)
    }

    /// Autopsy's `JLNK.getBestPath`: local base path + common suffix,
    /// else network share + suffix, else the path from the target ID list.
    pub fn best_path(&self) -> String {
        if let Some(li) = &self.link_info {
            let suffix = li
                .common_path_suffix_unicode
                .as_deref()
                .or(li.common_path_suffix.as_deref());
            let base = non_empty(li.local_base_path_unicode.as_ref())
                .or(non_empty(li.local_base_path.as_ref()));
            if let Some(base) = base {
                return format!("{base}{}", suffix.unwrap_or_default());
            }
            let net = non_empty(li.net_name_unicode.as_ref()).or(non_empty(li.net_name.as_ref()));
            if let Some(net) = net
                && let Some(s) = suffix.filter(|s| !s.is_empty())
            {
                return format!("{net}\\{s}");
            }
        }
        target_path(&self.id_list)
    }

    /// Header creation time as Unix seconds.
    pub fn created_unix(&self) -> Option<i64> {
        crate::filetime_to_unix(self.created)
    }

    /// Header access time as Unix seconds.
    pub fn accessed_unix(&self) -> Option<i64> {
        crate::filetime_to_unix(self.accessed)
    }

    /// Header modification time as Unix seconds.
    pub fn modified_unix(&self) -> Option<i64> {
        crate::filetime_to_unix(self.modified)
    }
}

fn non_empty(s: Option<&String>) -> Option<&str> {
    s.map(String::as_str).filter(|s| !s.is_empty())
}

/// 8-bit text: UTF-8 when valid, else Latin-1; stops at NUL.
fn ansi(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let b = b.get(..end).unwrap_or_default();
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => b.iter().map(|&c| char::from(c)).collect(),
    }
}

fn link_info(b: &[u8]) -> LinkInfo {
    let mut li = LinkInfo {
        flags: b.u32_le(8).unwrap_or(0),
        ..LinkInfo::default()
    };
    let header = b.u32_le(4).unwrap_or(0) as usize;
    let off = |at: usize| b.u32_le(at).ok().map(|o| o as usize).filter(|&o| o > 0);
    let ansi_at = |o: usize| b.get(o..).map(ansi);
    let uni_at = |o: usize| b.get(o..).map(utf16le_lossy);
    if li.flags & 1 != 0 {
        if let Some(v) = off(0x0C).and_then(|o| b.get(o..)) {
            li.drive_type = v.u32_le(4).ok();
            li.drive_serial = v.u32_le(8).ok();
            let label_off = v.u32_le(12).unwrap_or(0) as usize;
            li.volume_label = if label_off == 0x14 {
                v.u32_le(16)
                    .ok()
                    .and_then(|o| v.get(o as usize..))
                    .map(utf16le_lossy)
            } else {
                v.get(label_off..).map(ansi)
            };
        }
        li.local_base_path = off(0x10).and_then(ansi_at);
        if header >= 0x24 {
            li.local_base_path_unicode = off(0x1C).and_then(uni_at);
        }
    }
    if li.flags & 2 != 0
        && let Some(c) = off(0x14).and_then(|o| b.get(o..))
    {
        let net_off = c.u32_le(8).unwrap_or(0) as usize;
        li.net_name = c.get(net_off..).map(ansi).filter(|_| net_off > 0);
        let dev_off = c.u32_le(12).unwrap_or(0) as usize;
        li.device_name = c.get(dev_off..).map(ansi).filter(|_| dev_off > 0);
        if net_off > 0x14 {
            li.net_name_unicode = c
                .u32_le(20)
                .ok()
                .filter(|&o| o > 0)
                .and_then(|o| c.get(o as usize..))
                .map(utf16le_lossy);
        }
    }
    li.common_path_suffix = off(0x18).and_then(ansi_at);
    if header >= 0x24 {
        li.common_path_suffix_unicode = off(0x20).and_then(uni_at);
    }
    li
}

/// Parses extra-data blocks from `at`; returns the end offset.
fn extra_data(data: &[u8], mut at: usize, x: &mut ExtraData) -> usize {
    for _ in 0..MAX_BLOCKS {
        let Ok(size) = data.u32_le(at) else { return at };
        let size = size as usize;
        if size < 4 {
            return at + 4;
        }
        let Some(block) = data.get(at..at + size) else {
            return data.len();
        };
        let sig = block.u32_le(4).unwrap_or(0);
        match sig {
            0xA000_0001 => x.environment_target = env_block(block),
            0xA000_0003 => x.tracker = tracker(block),
            0xA000_0005 => x.special_folder = block.u32_le(8).ok(),
            0xA000_0006 => x.darwin_id = env_block(block),
            0xA000_0007 => x.icon_environment = env_block(block),
            0xA000_000B => x.known_folder = block.get(8..24).and_then(guid_string),
            0xA000_000C => x.vista_id_list = parse_id_list(block.get(8..).unwrap_or_default()),
            _ => {}
        }
        at += size;
    }
    at
}

/// Blocks holding an ANSI (260 bytes) and a Unicode (520 bytes) string.
fn env_block(b: &[u8]) -> Option<String> {
    let uni = b.get(268..788).map(utf16le_lossy).filter(|s| !s.is_empty());
    uni.or_else(|| b.get(8..268).map(ansi))
        .filter(|s| !s.is_empty())
}

fn tracker(b: &[u8]) -> Option<TrackerData> {
    let machine_id = b.get(16..32).map(ansi)?;
    let g = |at: usize| b.get(at..at + 16).and_then(guid_string);
    let droid_file = g(48);
    // A version-1 UUID ends with the node (MAC address).
    let mac_address = b.get(48..64).and_then(|u| {
        let version = u.get(7)? >> 4;
        (version == 1).then(|| {
            u.get(10..16)
                .unwrap_or_default()
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<Vec<_>>()
                .join(":")
        })
    });
    Some(TrackerData {
        machine_id,
        droid_volume: g(32),
        droid_file,
        birth_droid_volume: g(64),
        birth_droid_file: g(80),
        mac_address,
    })
}
