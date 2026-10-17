//! Browser timestamp conversions to Unix seconds, truncating like Autopsy's
//! Java long arithmetic.

/// Seconds between 1601-01-01 and 1970-01-01.
pub const WEBKIT_EPOCH_DIFF: i64 = 11_644_473_600;
/// Seconds between 1970-01-01 and 2001-01-01 (Cocoa / Mac absolute time).
pub const COCOA_EPOCH_DIFF: i64 = 978_307_200;

/// Chromium/WebKit microseconds since 1601 → Unix seconds
/// (`v / 1000000 - 11644473600`, Java integer division).
pub fn webkit(micros: i64) -> i64 {
    micros / 1_000_000 - WEBKIT_EPOCH_DIFF
}

/// Like [`webkit`], `None` for 0 / negative (unset) values.
pub fn webkit_opt(micros: Option<i64>) -> Option<i64> {
    micros.filter(|&v| v > 0).map(webkit)
}

/// Windows FILETIME (100 ns since 1601) → Unix seconds; `None` for 0.
pub fn filetime(ft: u64) -> Option<i64> {
    if ft == 0 {
        return None;
    }
    i64::try_from(ft / 10_000_000)
        .ok()
        .map(|s| s - WEBKIT_EPOCH_DIFF)
}

/// Cocoa seconds since 2001 → Unix seconds (Java `(long) d + 978307200`).
pub fn cocoa(secs: f64) -> Option<i64> {
    if !secs.is_finite() {
        return None;
    }
    // `as` saturates, like Java's (long) cast of large values.
    Some((secs as i64).saturating_add(COCOA_EPOCH_DIFF))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(webkit(13_000_000_000_000_000), 1_355_526_400);
        assert_eq!(webkit_opt(Some(0)), None);
        assert_eq!(filetime(0), None);
        assert_eq!(filetime(116_444_736_000_000_000), Some(0));
        // pasco2 TestFileTime: 0x01C5D2C6C6418F30 = 2005-10-17T02:59:22.147Z.
        assert_eq!(filetime(0x01C5_D2C6_C641_8F30), Some(1_129_517_962));
        assert_eq!(cocoa(0.5), Some(978_307_200));
        assert_eq!(cocoa(f64::NAN), None);
    }
}
