//! Value data types and decoders.

use pm_core::decode::utf16le_lossy;

/// Registry value data type (`REG_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueType {
    None,
    Sz,
    ExpandSz,
    Binary,
    Dword,
    DwordBigEndian,
    Link,
    MultiSz,
    ResourceList,
    FullResourceDescriptor,
    ResourceRequirementsList,
    Qword,
    /// Any other type code.
    Other(u32),
}

impl ValueType {
    /// From the on-disk type code.
    pub fn from_code(c: u32) -> Self {
        match c {
            0 => Self::None,
            1 => Self::Sz,
            2 => Self::ExpandSz,
            3 => Self::Binary,
            4 => Self::Dword,
            5 => Self::DwordBigEndian,
            6 => Self::Link,
            7 => Self::MultiSz,
            8 => Self::ResourceList,
            9 => Self::FullResourceDescriptor,
            10 => Self::ResourceRequirementsList,
            11 => Self::Qword,
            o => Self::Other(o),
        }
    }

    /// The on-disk type code.
    pub fn code(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Sz => 1,
            Self::ExpandSz => 2,
            Self::Binary => 3,
            Self::Dword => 4,
            Self::DwordBigEndian => 5,
            Self::Link => 6,
            Self::MultiSz => 7,
            Self::ResourceList => 8,
            Self::FullResourceDescriptor => 9,
            Self::ResourceRequirementsList => 10,
            Self::Qword => 11,
            Self::Other(o) => o,
        }
    }

    /// `REG_SZ`-like types holding UTF-16 text.
    pub fn is_string(self) -> bool {
        matches!(self, Self::Sz | Self::ExpandSz | Self::Link)
    }

    /// The `REG_*` name.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "REG_NONE",
            Self::Sz => "REG_SZ",
            Self::ExpandSz => "REG_EXPAND_SZ",
            Self::Binary => "REG_BINARY",
            Self::Dword => "REG_DWORD",
            Self::DwordBigEndian => "REG_DWORD_BIG_ENDIAN",
            Self::Link => "REG_LINK",
            Self::MultiSz => "REG_MULTI_SZ",
            Self::ResourceList => "REG_RESOURCE_LIST",
            Self::FullResourceDescriptor => "REG_FULL_RESOURCE_DESCRIPTOR",
            Self::ResourceRequirementsList => "REG_RESOURCE_REQUIREMENTS_LIST",
            Self::Qword => "REG_QWORD",
            Self::Other(_) => "REG_UNKNOWN",
        }
    }
}

/// Decodes `REG_SZ` data: UTF-16LE up to the first NUL. Data that is not
/// UTF-16 (odd length with ASCII bytes, as some programs write) is read as
/// 8-bit text.
pub fn decode_sz(data: &[u8]) -> String {
    let looks_ascii = data.len() >= 2
        && data.get(1).is_some_and(|&b| b != 0)
        && data.first().is_some_and(|&b| b != 0)
        && data.iter().take_while(|&&b| b != 0).all(u8::is_ascii);
    if looks_ascii && data.len() % 2 == 1 {
        let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        return data
            .get(..end)
            .map(|s| s.iter().map(|&b| char::from(b)).collect())
            .unwrap_or_default();
    }
    utf16le_lossy(data)
}

/// Decodes `REG_MULTI_SZ` data into its strings (empty trailing entries
/// dropped).
pub fn decode_multi_sz(data: &[u8]) -> Vec<String> {
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    let mut out: Vec<String> = units
        .split(|&u| u == 0)
        .map(|s| {
            char::decode_utf16(s.iter().copied())
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        })
        .collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn type_codes_roundtrip() {
        for c in 0..14 {
            assert_eq!(ValueType::from_code(c).code(), c);
        }
        assert!(ValueType::ExpandSz.is_string());
        assert_eq!(ValueType::Qword.name(), "REG_QWORD");
    }

    #[test]
    fn strings() {
        let mut d = utf16("C:\\Windows");
        d.extend([0, 0, b'x', 0]);
        assert_eq!(decode_sz(&d), "C:\\Windows");
        assert_eq!(decode_sz(b"abc"), "abc");
        let mut m = utf16("a");
        m.extend([0, 0]);
        m.extend(utf16("bc"));
        m.extend([0, 0, 0, 0]);
        assert_eq!(decode_multi_sz(&m), vec!["a", "bc"]);
        assert!(decode_multi_sz(&[]).is_empty());
    }
}
