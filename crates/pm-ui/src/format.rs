//! Display formatting shared by all views.

use jiff::Timestamp;
use jiff::tz::TimeZone;

/// Display preferences (later persisted in Options).
#[derive(Debug, Clone)]
pub(crate) struct DisplayPrefs {
    pub timezone: TimeZone,
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self {
            timezone: TimeZone::UTC,
        }
    }
}

impl DisplayPrefs {
    pub(crate) fn time(&self, ts: Option<Timestamp>) -> String {
        ts.map_or_else(String::new, |t| {
            if self.timezone == TimeZone::UTC {
                pm_core::time::format_utc(t)
            } else {
                pm_core::time::format_in(t, &self.timezone)
            }
        })
    }
}

/// `1 234 567 B (1.23 MB)` style size with SI units.
pub(crate) fn size_long(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{bytes} B")
    } else {
        format!(
            "{} ({})",
            grouped(bytes),
            pm_core::units::format_size(bytes)
        )
    }
}

/// Thousands-grouped integer with narrow spaces (ISO 31-0 style).
pub(crate) fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{202F}');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1\u{202F}000");
        assert_eq!(grouped(1_234_567), "1\u{202F}234\u{202F}567");
        assert_eq!(size_long(512), "512 B");
        assert_eq!(size_long(1_500_000), "1\u{202F}500\u{202F}000 (1.50 MB)");
    }

    #[test]
    fn time_formatting() {
        let p = DisplayPrefs::default();
        assert_eq!(p.time(None), "");
        assert_eq!(
            p.time(Some(Timestamp::from_second(0).unwrap())),
            "1970-01-01 00:00:00 UTC"
        );
    }
}
