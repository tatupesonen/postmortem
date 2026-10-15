//! `RecentFileCache.bcf` (Windows 7 / 2008 R2): executables recently run
//! from removable or non-system locations, recorded by the Application
//! Experience service. Layout from `RecentFileCacheParser`
//! (`RecentFileCache/RecentFileCacheFile.cs`, MIT).

use pm_core::Bytes;

const MAGIC: u32 = 0xFFEE_FFFE;
const MAX_ENTRIES: usize = 100_000;

/// File paths listed in the cache, in file order; `None` if the header
/// does not match.
pub fn parse(b: &[u8]) -> Option<Vec<String>> {
    if b.u32_le(0).ok()? != MAGIC {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 0x14usize;
    while out.len() < MAX_ENTRIES {
        let Ok(chars) = b.u32_le(at) else { break };
        let len = (chars as usize).checked_mul(2)?;
        let Some(s) = b.get(at + 4..at + 4 + len) else {
            break;
        };
        out.push(pm_core::decode::utf16le_lossy(s));
        at += 4 + len + 2;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entries() {
        let mut b = MAGIC.to_le_bytes().to_vec();
        b.resize(0x14, 0);
        for p in ["e:\\tools\\nc.exe", "c:\\users\\x\\a.exe"] {
            let u: Vec<u8> = p.encode_utf16().flat_map(u16::to_le_bytes).collect();
            b.extend((p.len() as u32).to_le_bytes());
            b.extend(u);
            b.extend([0, 0]);
        }
        assert_eq!(
            parse(&b).unwrap(),
            vec!["e:\\tools\\nc.exe", "c:\\users\\x\\a.exe"]
        );
        assert!(parse(b"nope").is_none());
    }
}
