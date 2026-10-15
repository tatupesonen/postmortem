//! Binary security identifiers.

/// Formats a binary SID as `S-1-5-21-…` (`None` if malformed).
pub fn format_sid(b: &[u8]) -> Option<String> {
    let revision = *b.first()?;
    let count = usize::from(*b.get(1)?);
    let auth = b.get(2..8)?;
    if b.len() < 8 + count * 4 {
        return None;
    }
    let authority = auth.iter().fold(0u64, |acc, &x| (acc << 8) | u64::from(x));
    let mut s = format!("S-{revision}-{authority}");
    for i in 0..count {
        let v: [u8; 4] = b.get(8 + i * 4..12 + i * 4)?.try_into().ok()?;
        s.push('-');
        s.push_str(&u32::from_le_bytes(v).to_string());
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_and_domain_sids() {
        // S-1-5-18 (LocalSystem)
        assert_eq!(
            format_sid(&[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]).as_deref(),
            Some("S-1-5-18")
        );
        let mut b = vec![1, 2, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0];
        b.extend_from_slice(&1004u32.to_le_bytes());
        assert_eq!(format_sid(&b).as_deref(), Some("S-1-5-21-1004"));
        assert_eq!(format_sid(&[1, 3, 0, 0]), None);
    }
}
