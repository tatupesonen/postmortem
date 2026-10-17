//! Internet Explorer text stores (Autopsy `ExtractIE`): Favorites
//! (`.url` internet shortcuts) and cookie text files. History is in
//! [`crate::index_dat`].

/// The target of an internet shortcut: the text after `=` on the first
/// line starting with `URL` (`""` when none).
pub fn favorite_url(data: &[u8]) -> String {
    let text = decode(data);
    text.lines()
        .find(|l| l.starts_with("URL"))
        .and_then(|l| l.split_once('='))
        .map(|(_, u)| u.to_owned())
        .unwrap_or_default()
}

/// One IE cookie file: Autopsy reads only the first cookie (lines 0–2:
/// name, value, host/path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub url: String,
}

/// The first cookie of a cookie text file.
pub fn cookie(data: &[u8]) -> Cookie {
    let text = decode(data);
    let values: Vec<&str> = text.split('\n').collect();
    let get = |i: usize| {
        values
            .get(i)
            .map_or("", |v| v.trim_end_matches('\r'))
            .to_owned()
    };
    Cookie {
        name: get(0),
        value: get(1),
        url: get(2),
    }
}

/// Bytes as text: UTF-8 when valid (with a BOM stripped), UTF-16LE with a
/// BOM, else Windows-1252-ish (bytes as code points).
fn decode(data: &[u8]) -> String {
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        return pm_core::decode::utf16le_lossy(rest);
    }
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    match std::str::from_utf8(data) {
        Ok(s) => s.to_owned(),
        Err(_) => data.iter().map(|&b| char::from(b)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_and_cookies() {
        assert_eq!(
            favorite_url(
                b"[InternetShortcut]\r\nURL=http://www.example.com/?a=b\r\nIconIndex=0\r\n"
            ),
            "http://www.example.com/?a=b"
        );
        assert_eq!(favorite_url(b"[DEFAULT]\nBASEURL=x\n"), "");
        assert_eq!(favorite_url(b"no url"), "");
        let c = cookie(b"SID\nabc123\ngoogle.com/\n1536\n2850000000\n30000000\n*\nPREF\n");
        assert_eq!(
            c,
            Cookie {
                name: "SID".into(),
                value: "abc123".into(),
                url: "google.com/".into()
            }
        );
        assert_eq!(cookie(b"").name, "");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "URL=http://u.com/"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(favorite_url(&utf16), "http://u.com/");
    }
}
