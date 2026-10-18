//! Timestamp conversions for the epochs found in forensic artefacts.
//!
//! All converters return `None` for the "unset" sentinel (usually `0`) and for
//! values outside the representable range, instead of producing bogus dates.

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

/// Seconds between 1601-01-01 and 1970-01-01 (Windows FILETIME epoch).
pub const FILETIME_UNIX_DIFF_SECS: i64 = 11_644_473_600;
/// Seconds between 1904-01-01 and 1970-01-01 (classic Mac / HFS epoch).
pub const HFS_UNIX_DIFF_SECS: i64 = 2_082_844_800;
/// Seconds between 1970-01-01 and 2001-01-01 (Cocoa / Core Data epoch).
pub const COCOA_UNIX_DIFF_SECS: i64 = 978_307_200;

fn from_secs_nanos(secs: i64, nanos: i32) -> Option<Timestamp> {
    Timestamp::new(secs, nanos).ok()
}

/// Unix seconds. `0` is treated as unset.
pub fn from_unix_secs(secs: i64) -> Option<Timestamp> {
    (secs != 0).then(|| from_secs_nanos(secs, 0)).flatten()
}

/// Unix seconds plus a nanosecond part (ext4 extra fields, `stat()`).
pub fn from_unix_secs_nanos(secs: i64, nanos: u32) -> Option<Timestamp> {
    if secs == 0 && nanos == 0 {
        return None;
    }
    from_secs_nanos(
        secs,
        i32::try_from(nanos).ok().filter(|n| *n < 1_000_000_000)?,
    )
}

/// Nanoseconds since the Unix epoch (APFS).
pub fn from_unix_nanos(nanos: i64) -> Option<Timestamp> {
    if nanos == 0 {
        return None;
    }
    from_secs_nanos(
        nanos.div_euclid(1_000_000_000),
        nanos.rem_euclid(1_000_000_000) as i32,
    )
}

/// Milliseconds since the Unix epoch (Java, JavaScript, many Android DBs).
pub fn from_unix_millis(millis: i64) -> Option<Timestamp> {
    if millis == 0 {
        return None;
    }
    Timestamp::from_millisecond(millis).ok()
}

/// Windows FILETIME: 100-nanosecond intervals since 1601-01-01 UTC.
pub fn from_filetime(ft: u64) -> Option<Timestamp> {
    if ft == 0 {
        return None;
    }
    let secs = (ft / 10_000_000) as i64 - FILETIME_UNIX_DIFF_SECS;
    let nanos = ((ft % 10_000_000) * 100) as i32;
    from_secs_nanos(secs, nanos)
}

/// Converts a timestamp back to FILETIME (saturating at the epoch bounds).
pub fn to_filetime(ts: Timestamp) -> u64 {
    let ticks =
        (ts.as_nanosecond() + i128::from(FILETIME_UNIX_DIFF_SECS) * 1_000_000_000).div_euclid(100);
    u64::try_from(ticks.max(0)).unwrap_or(u64::MAX)
}

/// WebKit/Chrome time: microseconds since 1601-01-01 UTC.
pub fn from_webkit_micros(micros: i64) -> Option<Timestamp> {
    if micros == 0 {
        return None;
    }
    let unix_micros = micros.checked_sub(FILETIME_UNIX_DIFF_SECS * 1_000_000)?;
    Timestamp::from_microsecond(unix_micros).ok()
}

/// HFS+ time: seconds since 1904-01-01 UTC.
pub fn from_hfs_secs(secs: u32) -> Option<Timestamp> {
    if secs == 0 {
        return None;
    }
    from_secs_nanos(i64::from(secs) - HFS_UNIX_DIFF_SECS, 0)
}

/// Cocoa / Core Data time: (fractional) seconds since 2001-01-01 UTC.
pub fn from_cocoa_secs(secs: f64) -> Option<Timestamp> {
    if secs == 0.0 || !secs.is_finite() {
        return None;
    }
    let whole = secs.floor();
    let nanos = ((secs - whole) * 1e9).round() as i32;
    let whole = whole as i64;
    from_secs_nanos(
        whole.checked_add(COCOA_UNIX_DIFF_SECS)?,
        nanos.min(999_999_999),
    )
}

/// MS-DOS/FAT packed date and time. These are *local* wall-clock values with
/// two-second resolution; `centis` is the optional 10 ms creation-time field.
///
/// Returns `None` for zero or out-of-range fields.
pub fn dos_datetime(date: u16, time: u16, centis: u8) -> Option<DateTime> {
    if date == 0 {
        return None;
    }
    let year = 1980 + i16::try_from(date >> 9).ok()?;
    let month = i8::try_from((date >> 5) & 0x0f).ok()?;
    let day = i8::try_from(date & 0x1f).ok()?;
    let hour = i8::try_from(time >> 11).ok()?;
    let minute = i8::try_from((time >> 5) & 0x3f).ok()?;
    let extra_secs = i8::try_from(centis / 100).ok()?;
    let second = i8::try_from((time & 0x1f) * 2).ok()? + extra_secs;
    let subsec = i32::from(centis % 100) * 10_000_000;
    let d = Date::new(year, month, day).ok()?;
    let t = Time::new(hour, minute, second, subsec).ok()?;
    Some(d.to_datetime(t))
}

/// Interprets a local wall-clock value in `tz` (e.g. FAT times with the
/// case's configured data-source time zone).
pub fn local_to_timestamp(dt: DateTime, tz: &TimeZone) -> Option<Timestamp> {
    tz.to_ambiguous_timestamp(dt).compatible().ok()
}

/// Whether two zones give the same UTC offsets over the last ten years
/// (sampled twice a month), so local times read in either agree, e.g.
/// `Europe/Helsinki` and `Europe/Kyiv`.
pub fn same_offsets(a: &TimeZone, b: &TimeZone) -> bool {
    let now = Timestamp::now().as_second();
    let step = 15 * 86_400;
    (0..240).all(|i| {
        Timestamp::from_second(now - i * step).map_or(true, |t| a.to_offset(t) == b.to_offset(t))
    })
}

/// Formats a timestamp for display: ISO-8601 with offset, second precision.
pub fn format_in(ts: Timestamp, tz: &TimeZone) -> String {
    let z: Zoned = ts.to_zoned(tz.clone());
    z.strftime("%Y-%m-%d %H:%M:%S %:z").to_string()
}

/// Formats a timestamp in UTC (`2024-05-01 12:00:00 UTC`).
pub fn format_utc(ts: Timestamp) -> String {
    ts.strftime("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn same_offsets_compares_rules_not_names() {
        let tz = |n| TimeZone::get(n).unwrap();
        assert!(same_offsets(&tz("Europe/Helsinki"), &tz("Europe/Kyiv")));
        assert!(same_offsets(&TimeZone::UTC, &tz("Etc/UTC")));
        assert!(!same_offsets(&tz("Europe/Helsinki"), &TimeZone::UTC));
        // Same standard offset, different daylight saving.
        assert!(!same_offsets(&tz("America/Phoenix"), &tz("America/Denver")));
    }

    #[test]
    fn filetime_known_values() {
        // 2009-07-25 23:00:00 UTC, a commonly cited NTFS example.
        let ts = from_filetime(128_930_364_000_000_000).unwrap();
        assert_eq!(format_utc(ts), "2009-07-25 23:00:00 UTC");
        assert_eq!(from_filetime(0), None);
        assert_eq!(
            from_filetime(116_444_736_000_000_000).unwrap().as_second(),
            0
        );
        // Out of jiff's range (year 30828) → None rather than garbage.
        assert_eq!(from_filetime(u64::MAX), None);
    }

    #[test]
    fn filetime_roundtrip_preserves_100ns() {
        for ft in [132_000_000_001_234_567u64, 1_320_000_001_234_567, 1] {
            assert_eq!(to_filetime(from_filetime(ft).unwrap()), ft);
        }
    }

    #[test]
    fn webkit_and_cocoa() {
        // 13_000_000_000_000_000 µs since 1601 = 2012-12-14 23:06:40 UTC
        let ts = from_webkit_micros(13_000_000_000_000_000).unwrap();
        assert_eq!(format_utc(ts), "2012-12-14 23:06:40 UTC");
        assert_eq!(
            format_utc(from_cocoa_secs(1.0).unwrap()),
            "2001-01-01 00:00:01 UTC"
        );
        assert_eq!(from_cocoa_secs(f64::NAN), None);
        assert_eq!(from_cocoa_secs(1e300), None);
    }

    #[test]
    fn hfs_and_unix() {
        assert_eq!(from_hfs_secs(2_082_844_800).unwrap().as_second(), 0);
        assert_eq!(from_unix_secs(0), None);
        assert_eq!(from_unix_millis(1_000).unwrap().as_second(), 1);
        assert_eq!(from_unix_nanos(-1).unwrap().as_nanosecond(), -1);
        assert_eq!(from_unix_secs_nanos(1, 2_000_000_000), None);
    }

    fn dos_date(y: u16, m: u16, d: u16) -> u16 {
        ((y - 1980) << 9) | (m << 5) | d
    }

    fn dos_time(h: u16, m: u16, s: u16) -> u16 {
        (h << 11) | (m << 5) | (s / 2)
    }

    #[test]
    fn dos_datetime_decoding() {
        let date = dos_date(2003, 3, 15);
        let time = dos_time(13, 45, 58);
        let dt = dos_datetime(date, time, 0).unwrap();
        assert_eq!(dt.to_string(), "2003-03-15T13:45:58");
        // 10 ms creation field adds up to 1.99 s.
        let dt = dos_datetime(date, dos_time(13, 45, 0), 150).unwrap();
        assert_eq!(dt.to_string(), "2003-03-15T13:45:01.5");
        assert_eq!(dos_datetime(0, 0, 0), None);
        // Month 13 is invalid.
        assert_eq!(dos_datetime(dos_date(1980, 13, 1), 0, 0), None);
    }

    #[test]
    fn local_time_in_zone() {
        let dt = dos_datetime(dos_date(2020, 6, 1), dos_time(12, 0, 0), 0).unwrap();
        let tz = TimeZone::get("Europe/Helsinki").unwrap();
        let ts = local_to_timestamp(dt, &tz).unwrap();
        assert_eq!(format_utc(ts), "2020-06-01 09:00:00 UTC");
        assert_eq!(format_in(ts, &tz), "2020-06-01 12:00:00 +03:00");
    }

    proptest! {
        #[test]
        fn converters_never_panic(a in any::<u64>(), b in any::<i64>(), c in any::<u32>(), d in any::<u16>(), e in any::<u16>(), f in any::<u8>(), g in any::<f64>()) {
            let _ = from_filetime(a);
            let _ = from_webkit_micros(b);
            let _ = from_unix_nanos(b);
            let _ = from_unix_millis(b);
            let _ = from_unix_secs(b);
            let _ = from_unix_secs_nanos(b, c);
            let _ = from_hfs_secs(c);
            let _ = dos_datetime(d, e, f);
            let _ = from_cocoa_secs(g);
        }
    }
}
