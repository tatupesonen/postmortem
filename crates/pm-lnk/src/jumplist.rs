//! Jump lists.
//!
//! * `*.automaticDestinations-ms`: an OLE compound file with one stream per
//!   entry (a shell link named by the entry number in hex) plus the
//!   `DestList` stream (MRU order, last access time, pin status, access
//!   count, `NetBIOS` host name, link-tracker droids).
//! * `*.customDestinations-ms`: shell links concatenated after a small
//!   header; links are found by their header signature.
//!
//! The file-name prefix is the application id (a CRC-64 of the app's
//! path); [`app_name`] knows the common ones.

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

use crate::cfb::CompoundFile;
use crate::shell_item::guid_string;
use crate::{Result, ShellLink};

/// Most `DestList` entries decoded.
const MAX_DEST: usize = 100_000;
/// Most links taken from a custom-destinations file.
const MAX_CUSTOM: usize = 10_000;
const LINK_HEADER: [u8; 20] = [
    0x4C, 0, 0, 0, 0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x46,
];

/// One `DestList` record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DestListEntry {
    /// Matches the stream name (hex).
    pub entry_number: u32,
    /// `NetBIOS` name of the host the target was on.
    pub hostname: String,
    /// FILETIME of the last access through the jump list.
    pub last_access: u64,
    pub pinned: bool,
    /// Interaction count (Windows 10 format only).
    pub access_count: Option<u32>,
    pub path: String,
    pub droid_volume: Option<String>,
    pub droid_file: Option<String>,
}

impl DestListEntry {
    /// Last access as Unix seconds.
    pub fn last_access_unix(&self) -> Option<i64> {
        crate::filetime_to_unix(self.last_access)
    }
}

/// One link stream with its `DestList` record, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct JumpListEntry {
    pub stream_name: String,
    pub link: ShellLink,
    pub dest: Option<DestListEntry>,
}

/// A parsed automatic-destinations jump list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AutomaticDestinations {
    pub entries: Vec<JumpListEntry>,
    /// All `DestList` records (also those whose stream is gone).
    pub dest_list: Vec<DestListEntry>,
    pub dest_list_version: Option<u32>,
    /// Streams that could not be parsed as links (besides `DestList`).
    pub unparsed_streams: Vec<String>,
}

/// Parses an `*.automaticDestinations-ms` file.
pub fn parse_automatic(data: &[u8]) -> Result<AutomaticDestinations> {
    let cf = CompoundFile::parse(data)?;
    let mut out = AutomaticDestinations::default();
    let streams = cf.root_streams();
    if let Some(d) = streams.iter().find(|e| e.name == "DestList")
        && let Ok(bytes) = cf.read_stream(d)
    {
        let (v, list) = parse_dest_list(&bytes);
        out.dest_list_version = v;
        out.dest_list = list;
    }
    let mut named: Vec<_> = streams
        .into_iter()
        .filter(|e| e.name != "DestList")
        .collect();
    named.sort_by_key(|e| u64::from_str_radix(&e.name, 16).unwrap_or(u64::MAX));
    for e in named {
        if e.size == 0 {
            continue;
        }
        match cf
            .read_stream(e)
            .ok()
            .and_then(|b| ShellLink::parse(&b).ok())
        {
            Some(link) => {
                let num = u32::from_str_radix(&e.name, 16).ok();
                let dest = out
                    .dest_list
                    .iter()
                    .find(|d| Some(d.entry_number) == num)
                    .cloned();
                out.entries.push(JumpListEntry {
                    stream_name: e.name.clone(),
                    link,
                    dest,
                });
            }
            None => out.unparsed_streams.push(e.name.clone()),
        }
    }
    Ok(out)
}

/// Decodes a `DestList` stream: `(version, entries)`.
pub fn parse_dest_list(b: &[u8]) -> (Option<u32>, Vec<DestListEntry>) {
    let Ok(version) = b.u32_le(0) else {
        return (None, Vec::new());
    };
    let count = b.u32_le(4).unwrap_or(0) as usize;
    let win10 = version >= 2;
    let (len_at, fixed, trailer) = if win10 {
        (0x80, 0x82, 4)
    } else {
        (0x70, 0x72, 0)
    };
    // Version 3+ (Windows 10) adds a 4-byte float before the time:
    // entry number 0x58, float 0x60, FILETIME 0x64, pin status 0x6C,
    // access count 0x74, path size 0x80. Version 1 (Windows 7): float
    // 0x5C, FILETIME 0x60, pin status 0x68, path size 0x70.
    let time_at = if win10 { 0x64 } else { 0x60 };
    let mut out = Vec::new();
    let mut at = 32usize;
    while out.len() < count.min(MAX_DEST) {
        let Some(e) = b.get(at..) else { break };
        let Ok(chars) = e.u16_le(len_at) else { break };
        let path_len = usize::from(chars) * 2;
        let Some(path) = e.get(fixed..fixed + path_len) else {
            break;
        };
        out.push(DestListEntry {
            entry_number: e.u32_le(0x58).unwrap_or(0),
            hostname: e
                .get(0x48..0x58)
                .map(|h| {
                    h.iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| char::from(c))
                        .collect()
                })
                .unwrap_or_default(),
            last_access: e.u64_le(time_at).unwrap_or(0),
            pinned: e.i32_le(time_at + 8).is_ok_and(|p| p >= 0),
            access_count: if win10 { e.u32_le(0x74).ok() } else { None },
            path: utf16le_lossy(path),
            droid_volume: e.get(0x08..0x18).and_then(guid_string),
            droid_file: e.get(0x18..0x28).and_then(guid_string),
        });
        at += fixed + path_len + trailer;
    }
    (Some(version), out)
}

/// Shell links in a `*.customDestinations-ms` file.
pub fn parse_custom(data: &[u8]) -> Vec<ShellLink> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < MAX_CUSTOM {
        let Some(rest) = data.get(at..) else { break };
        let Some(p) = rest
            .windows(LINK_HEADER.len())
            .position(|w| w == LINK_HEADER)
        else {
            break;
        };
        let start = at + p;
        match data.get(start..).map(ShellLink::parse) {
            Some(Ok(l)) => {
                at = start + l.len.max(LINK_HEADER.len());
                out.push(l);
            }
            _ => at = start + LINK_HEADER.len(),
        }
    }
    out
}

/// The application id of a jump-list file name (`<appid>.xxxDestinations-ms`).
pub fn app_id(file_name: &str) -> &str {
    file_name.split('.').next().unwrap_or_default()
}

/// Names of common jump-list application ids.
pub fn app_name(app_id: &str) -> Option<&'static str> {
    const IDS: &[(&str, &str)] = &[
        ("1b4dd67f29cb1962", "Windows Explorer"),
        ("f01b4d95cf55d32a", "Windows Explorer (Windows 8+)"),
        ("5f7b5f1e01b83767", "Quick Access"),
        ("7e4dca80246863e3", "Control Panel"),
        ("9b9cdc69c1c24e2b", "Notepad (64-bit)"),
        ("918e0ecb43d17e23", "Notepad (32-bit)"),
        ("adecfb853d77462a", "Microsoft Word 2007"),
        ("a7bd71699cd38d1c", "Microsoft Word 2010"),
        ("fb3b0dbfee58fac8", "Microsoft Word 365"),
        ("a8c43ef36da523b1", "Microsoft Word 2003"),
        ("d38adec6953449ba", "Microsoft OneNote"),
        ("9839aec31243a928", "Microsoft Excel 2010"),
        ("f0275e8685d95486", "Microsoft Excel"),
        ("b8ab77100df80ab2", "Microsoft Excel 365"),
        ("9c7cc110ff56d1bd", "Microsoft PowerPoint 2010"),
        ("d00655d2aa12ff6d", "Microsoft PowerPoint 365"),
        ("5d696d521de238c3", "Google Chrome"),
        ("9d1f905ce5044aee", "Microsoft Edge"),
        ("5c450709f7ae4396", "Mozilla Firefox"),
        ("28c8b86deab549a1", "Internet Explorer"),
        ("6d2bac8f1edf6668", "Microsoft Outlook"),
        ("12dc1ea8e34b5a6", "Microsoft Paint"),
        ("1bc392b8e104a00e", "Remote Desktop Connection"),
        ("290532160612e071", "WinRAR"),
        ("23646679aaccfae0", "Adobe Reader"),
        ("de48a32edcbe79e4", "Acrobat Reader DC"),
        ("b74736c2bd8cc8a5", "WinZip"),
        ("4cb9c5750d51c07f", "Windows Media Player"),
        ("9fda41b86ddcf1db", "VLC media player"),
        ("ccba5a5986c77e43", "Skype"),
        ("bc0c37e84e063727", "Windows Command Processor"),
    ];
    IDS.iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(app_id))
        .map(|(_, n)| *n)
}
