//! Windows prefetch (`*.pf`) parser.
//!
//! Supports format versions 17 (XP/2003), 23 (Vista/7), 26 (8.1),
//! 30 (Windows 10, both file-information layouts) and 31 (Windows 11),
//! including Windows 10+ `MAM\x04` files compressed with LZXPRESS Huffman
//! (decoded natively in [`xpress`]). Extracts the executable name, the
//! prefetch hash, the run count, up to eight last-run times, the file
//! names the program loaded and the volumes (device path, serial number,
//! creation time, directories).

#[cfg(feature = "builder")]
pub mod builder;
pub mod xpress;

use pm_core::Bytes;
use pm_core::decode::utf16le_lossy;

/// Largest decompressed prefetch accepted.
const MAX_UNCOMPRESSED: usize = 64 << 20;
/// Most file names / volumes / directories read.
const MAX_ITEMS: usize = 100_000;

/// Parse errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a prefetch file")]
    NotPrefetch,
    #[error("unsupported prefetch version {0}")]
    Version(u32),
    #[error("corrupt data: {0}")]
    Corrupt(&'static str),
    #[error("truncated structure: {0}")]
    Truncated(#[from] pm_core::Truncated),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A volume the program touched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Volume {
    /// `\VOLUME{…}` or `\DEVICE\HARDDISKVOLUME1`.
    pub device_path: String,
    /// FILETIME.
    pub created: u64,
    pub serial: u32,
    pub directories: Vec<String>,
}

/// A parsed prefetch file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefetch {
    pub version: u32,
    /// Whether the file was MAM-compressed.
    pub compressed: bool,
    /// Executable name from the header (upper case, e.g. `CMD.EXE`).
    pub executable: String,
    pub hash: u32,
    pub run_count: u32,
    /// Non-zero last-run FILETIMEs, most recent first (≤ 8).
    pub last_run_times: Vec<u64>,
    /// Files loaded during the first seconds of execution.
    pub filenames: Vec<String>,
    pub volumes: Vec<Volume>,
}

/// Offsets of the version-specific file-information fields.
struct Layout {
    last_run: usize,
    run_times: usize,
    run_count: usize,
    volume_entry: usize,
}

/// Parses a prefetch file (compressed or not).
pub fn parse(data: &[u8]) -> Result<Prefetch> {
    if data.get(..3) == Some(b"MAM") {
        let plain = decompress_mam(data)?;
        let mut p = parse_plain(&plain)?;
        p.compressed = true;
        return Ok(p);
    }
    parse_plain(data)
}

/// Decompresses a `MAM` container (format 4 = LZXPRESS Huffman).
pub fn decompress_mam(data: &[u8]) -> Result<Vec<u8>> {
    let flags = data.u8_at(3)?;
    if flags & 0x0F != 4 {
        return Err(Error::Corrupt("unsupported MAM compression format"));
    }
    let size = data.u32_le(4)? as usize;
    if size > MAX_UNCOMPRESSED {
        return Err(Error::Corrupt("decompressed size too large"));
    }
    let start = if flags & 0x80 != 0 { 12 } else { 8 };
    xpress::decompress(data.get(start..).unwrap_or_default(), size)
}

#[allow(clippy::many_single_char_names, reason = "short names for raw buffers")]
fn parse_plain(d: &[u8]) -> Result<Prefetch> {
    if d.get(4..8) != Some(b"SCCA") {
        return Err(Error::NotPrefetch);
    }
    let version = d.u32_le(0)?;
    let layout = match version {
        17 => Layout {
            last_run: 0x78,
            run_times: 1,
            run_count: 0x90,
            volume_entry: 40,
        },
        23 => Layout {
            last_run: 0x80,
            run_times: 1,
            run_count: 0x98,
            volume_entry: 104,
        },
        26 => Layout {
            last_run: 0x80,
            run_times: 8,
            run_count: 0xD0,
            volume_entry: 104,
        },
        30 | 31 => Layout {
            last_run: 0x80,
            run_times: 8,
            // The shorter file-information variant moves the run count.
            run_count: if d.u32_le(0x54)? == 0x128 { 0xC8 } else { 0xD0 },
            volume_entry: 96,
        },
        v => return Err(Error::Version(v)),
    };
    let mut p = Prefetch {
        version,
        executable: utf16le_lossy(d.sub(0x10, 60)?),
        hash: d.u32_le(0x4C)?,
        run_count: d.u32_le(layout.run_count)?,
        ..Prefetch::default()
    };
    for i in 0..layout.run_times {
        let t = d.u64_le(layout.last_run + i * 8)?;
        if t != 0 {
            p.last_run_times.push(t);
        }
    }
    // File names: NUL-separated UTF-16.
    let names_off = d.u32_le(0x64)? as usize;
    let names_len = d.u32_le(0x68)? as usize;
    if let Some(names) = d.get(names_off..names_off.saturating_add(names_len)) {
        let units: Vec<u16> = names
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        p.filenames = units
            .split(|&u| u == 0)
            .filter(|s| !s.is_empty())
            .take(MAX_ITEMS)
            .map(String::from_utf16_lossy)
            .collect();
    }
    // Volumes.
    let vol_off = d.u32_le(0x6C)? as usize;
    let vol_count = (d.u32_le(0x70)? as usize).min(MAX_ITEMS);
    let vol_size = d.u32_le(0x74)? as usize;
    let vols = d
        .get(vol_off..vol_off.saturating_add(vol_size))
        .unwrap_or_default();
    for i in 0..vol_count {
        let Some(e) = vols.get(i * layout.volume_entry..) else {
            break;
        };
        let (Ok(path_off), Ok(path_len)) = (e.u32_le(0), e.u32_le(4)) else {
            break;
        };
        let device_path = vols
            .get(path_off as usize..)
            .and_then(|s| s.get(..(path_len as usize).saturating_mul(2)))
            .map(utf16le_lossy)
            .unwrap_or_default();
        let mut directories = Vec::new();
        let dir_off = e.u32_le(28).unwrap_or(0) as usize;
        let dir_count = (e.u32_le(32).unwrap_or(0) as usize).min(MAX_ITEMS);
        let mut at = dir_off;
        for _ in 0..dir_count {
            let Ok(n) = vols.u16_le(at) else { break };
            let len = usize::from(n) * 2;
            let Some(s) = vols.get(at + 2..at + 2 + len) else {
                break;
            };
            directories.push(utf16le_lossy(s));
            at += 2 + len + 2;
        }
        p.volumes.push(Volume {
            device_path,
            created: e.u64_le(8).unwrap_or(0),
            serial: e.u32_le(16).unwrap_or(0),
            directories,
        });
    }
    Ok(p)
}

impl Prefetch {
    /// Last-run times as Unix seconds (most recent first).
    pub fn last_run_unix(&self) -> Vec<i64> {
        self.last_run_times
            .iter()
            .filter_map(|&t| pm_core::time::from_filetime(t).map(jiff::Timestamp::as_second))
            .collect()
    }

    /// The loaded-file entry that is the executable itself.
    pub fn executable_path(&self) -> Option<&str> {
        let exe = self.executable.to_uppercase();
        self.filenames
            .iter()
            .find(|f| {
                f.rsplit('\\')
                    .next()
                    .is_some_and(|n| n.to_uppercase() == exe)
            })
            .map(String::as_str)
    }

    /// Directory of the executable with the volume device removed and
    /// `/` separators (`/WINDOWS/SYSTEM32`), as Autopsy's prefetch tool
    /// reports it; `""` when unknown.
    pub fn executable_dir(&self) -> String {
        let Some(path) = self.executable_path() else {
            return String::new();
        };
        let dir = path.rsplit_once('\\').map_or("", |(d, _)| d);
        strip_device(dir).replace('\\', "/")
    }
}

/// Removes a `\DEVICE\HARDDISKVOLUMEn` or `\VOLUME{…}` prefix.
pub fn strip_device(path: &str) -> String {
    let comps: Vec<&str> = path.split('\\').collect();
    let second = comps.get(1).copied().unwrap_or_default();
    let skip = if comps.first() != Some(&"") {
        return path.to_owned();
    } else if second.eq_ignore_ascii_case("DEVICE") {
        3
    } else if second.to_uppercase().starts_with("VOLUME{") {
        2
    } else {
        return path.to_owned();
    };
    comps
        .get(skip..)
        .unwrap_or_default()
        .iter()
        .fold(String::new(), |mut acc, c| {
            acc.push('\\');
            acc.push_str(c);
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_prefixes() {
        assert_eq!(
            strip_device("\\DEVICE\\HARDDISKVOLUME1\\WINDOWS\\SYSTEM32"),
            "\\WINDOWS\\SYSTEM32"
        );
        assert_eq!(
            strip_device("\\VOLUME{01d2-abcd}\\PROGRAM FILES"),
            "\\PROGRAM FILES"
        );
        assert_eq!(strip_device("\\DEVICE\\HARDDISKVOLUME1"), "");
        assert_eq!(strip_device("C:\\x"), "C:\\x");
    }

    #[test]
    fn rejects_junk() {
        assert!(matches!(parse(b"hello world"), Err(Error::NotPrefetch)));
        assert!(parse(b"MAM").is_err());
        let mut v = vec![0u8; 300];
        v[..4].copy_from_slice(&99u32.to_le_bytes());
        v[4..8].copy_from_slice(b"SCCA");
        assert!(matches!(parse(&v), Err(Error::Version(99))));
    }
}
