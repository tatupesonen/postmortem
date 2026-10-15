//! `Zone.Identifier` alternate data streams (mark of the web), as
//! Autopsy's `ExtractZoneIdentifier` reads them.

/// Parsed stream content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneIdentifier {
    pub zone_id: Option<u32>,
    pub referrer_url: Option<String>,
    pub host_url: Option<String>,
    pub last_writer_package: Option<String>,
}

/// Parses the INI-style text (`[ZoneTransfer]`, `ZoneId=3`, …).
pub fn parse(data: &[u8]) -> ZoneIdentifier {
    let text = if data.starts_with(&[0xFF, 0xFE]) {
        pm_core::decode::utf16le_lossy(data.get(2..).unwrap_or_default())
    } else {
        String::from_utf8_lossy(data).into_owned()
    };
    let mut z = ZoneIdentifier::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(['=', ':']) else {
            continue;
        };
        let v = v.trim().to_owned();
        match k.trim() {
            "ZoneId" => z.zone_id = v.parse().ok(),
            "ReferrerUrl" => z.referrer_url = Some(v),
            "HostUrl" => z.host_url = Some(v),
            "LastWriterPackageFamilyName" => z.last_writer_package = Some(v),
            _ => {}
        }
    }
    z
}

/// Autopsy's zone names.
pub fn zone_name(id: u32) -> &'static str {
    match id {
        0 => "Local Machine Zone",
        1 => "Local Intranet Zone",
        2 => "Trusted Sites Zone",
        3 => "Internet Zone",
        4 => "Restricted Sites Zone",
        _ => "Unknown Zone",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mark_of_the_web() {
        let z = parse(b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.com/\r\nHostUrl=https://example.com/setup.exe\r\n");
        assert_eq!(z.zone_id, Some(3));
        assert_eq!(z.host_url.as_deref(), Some("https://example.com/setup.exe"));
        assert_eq!(z.referrer_url.as_deref(), Some("https://example.com/"));
        assert_eq!(zone_name(3), "Internet Zone");
        assert_eq!(parse(b"garbage").zone_id, None);
    }
}
