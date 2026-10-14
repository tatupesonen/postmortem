//! Calendar binning (Autopsy's `RangeDivision`).

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};

/// Granularity of the time axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeUnit {
    Seconds,
    Minutes,
    Hours,
    Days,
    Months,
    Years,
}

impl TimeUnit {
    pub const ALL: [Self; 6] = [
        Self::Years,
        Self::Months,
        Self::Days,
        Self::Hours,
        Self::Minutes,
        Self::Seconds,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Years => "Years",
            Self::Months => "Months",
            Self::Days => "Days",
            Self::Hours => "Hours",
            Self::Minutes => "Minutes",
            Self::Seconds => "Seconds",
        }
    }

    /// Autopsy's rule: the coarsest unit of which the range spans more
    /// than three.
    pub fn for_range(start: i64, end: i64) -> Self {
        const DAY: i64 = 86_400;
        let d = end.saturating_sub(start);
        if d > 3 * 365 * DAY {
            Self::Years
        } else if d > 3 * 30 * DAY {
            Self::Months
        } else if d > 3 * DAY {
            Self::Days
        } else if d > 3 * 3600 {
            Self::Hours
        } else if d > 3 * 60 {
            Self::Minutes
        } else {
            Self::Seconds
        }
    }

    /// Next finer unit (Seconds stays Seconds).
    #[must_use]
    pub fn finer(self) -> Self {
        match self {
            Self::Years => Self::Months,
            Self::Months => Self::Days,
            Self::Days => Self::Hours,
            Self::Hours => Self::Minutes,
            Self::Minutes | Self::Seconds => Self::Seconds,
        }
    }

    /// Truncates a wall-clock time to the start of its unit.
    fn floor_dt(self, dt: DateTime) -> DateTime {
        let (y, mo, d, h, mi, s) = (
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
        );
        let mk = |y, mo, d, h, mi, s| {
            Date::new(y, mo, d)
                .ok()
                .and_then(|dd| Time::new(h, mi, s, 0).ok().map(|t| dd.to_datetime(t)))
                .unwrap_or(dt)
        };
        match self {
            Self::Years => mk(y, 1, 1, 0, 0, 0),
            Self::Months => mk(y, mo, 1, 0, 0, 0),
            Self::Days => mk(y, mo, d, 0, 0, 0),
            Self::Hours => mk(y, mo, d, h, 0, 0),
            Self::Minutes => mk(y, mo, d, h, mi, 0),
            Self::Seconds => mk(y, mo, d, h, mi, s),
        }
    }

    fn step(self, dt: DateTime) -> Option<DateTime> {
        let span = match self {
            Self::Years => 1.year(),
            Self::Months => 1.month(),
            Self::Days => 1.day(),
            Self::Hours => 1.hour(),
            Self::Minutes => 1.minute(),
            Self::Seconds => 1.second(),
        };
        dt.checked_add(span).ok()
    }

    /// Start (unix seconds) of the unit containing `t`, in `tz`.
    pub fn floor(self, t: i64, tz: &TimeZone) -> i64 {
        let Ok(ts) = Timestamp::from_second(t) else {
            return t;
        };
        let dt = self.floor_dt(ts.to_zoned(tz.clone()).datetime());
        tz.to_ambiguous_timestamp(dt)
            .compatible()
            .map_or(t, jiff::Timestamp::as_second)
    }

    /// Start of the unit after the one containing `t`.
    pub fn next(self, t: i64, tz: &TimeZone) -> i64 {
        let Ok(ts) = Timestamp::from_second(t) else {
            return t.saturating_add(1);
        };
        let dt = self.floor_dt(ts.to_zoned(tz.clone()).datetime());
        let next = self
            .step(dt)
            .and_then(|n| tz.to_ambiguous_timestamp(n).compatible().ok());
        next.map_or(t.saturating_add(1), jiff::Timestamp::as_second)
            .max(t.saturating_add(1))
    }

    /// Bin label in Autopsy's format.
    pub fn label(self, t: i64, tz: &TimeZone) -> String {
        let Ok(ts) = Timestamp::from_second(t) else {
            return t.to_string();
        };
        let z = ts.to_zoned(tz.clone());
        let fmt = match self {
            Self::Years => "%Y",
            Self::Months => "%Y-%B",
            Self::Days => "%Y-%B-%d",
            Self::Hours => "%Y-%m-%d %H",
            Self::Minutes => "%Y-%m-%d %H:%M",
            Self::Seconds => "%Y-%m-%d %H:%M:%S",
        };
        z.strftime(fmt).to_string()
    }
}

/// Bin boundaries covering `[start, end)` in `unit` steps (calendar-aware).
/// The first bin starts at `floor(start)`; at most `max_bins` are produced.
pub fn bins(
    start: i64,
    end: i64,
    unit: TimeUnit,
    tz: &TimeZone,
    max_bins: usize,
) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut t = unit.floor(start, tz);
    while t < end && out.len() < max_bins {
        let n = unit.next(t, tz);
        out.push((t, n));
        t = n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn autopsy_unit_rule() {
        assert_eq!(TimeUnit::for_range(0, 4 * 365 * DAY), TimeUnit::Years);
        assert_eq!(TimeUnit::for_range(0, 365 * DAY), TimeUnit::Months);
        assert_eq!(TimeUnit::for_range(0, 10 * DAY), TimeUnit::Days);
        assert_eq!(TimeUnit::for_range(0, DAY), TimeUnit::Hours);
        assert_eq!(TimeUnit::for_range(0, 3600), TimeUnit::Minutes);
        assert_eq!(TimeUnit::for_range(0, 100), TimeUnit::Seconds);
    }

    #[test]
    fn month_bins_are_calendar_aligned() {
        // 2021-01-15 .. 2021-04-02 UTC → Jan, Feb, Mar, Apr.
        let tz = TimeZone::UTC;
        let start = 1_610_668_800; // 2021-01-15
        let end = 1_617_321_600; // 2021-04-02
        let b = bins(start, end, TimeUnit::Months, &tz, 100);
        let labels: Vec<_> = b
            .iter()
            .map(|(s, _)| TimeUnit::Months.label(*s, &tz))
            .collect();
        assert_eq!(
            labels,
            ["2021-January", "2021-February", "2021-March", "2021-April"]
        );
        assert_eq!(b[1].1 - b[1].0, 28 * DAY, "February 2021 has 28 days");
    }

    #[test]
    fn bins_respect_time_zone() {
        let tz = TimeZone::get("Europe/Helsinki").unwrap();
        // 2021-03-04 10:20:30 UTC = 12:20:30 local → day starts 22:00 UTC prev day.
        let day = TimeUnit::Days.floor(1_614_853_230, &tz);
        assert_eq!(
            pm_core::time::format_utc(Timestamp::from_second(day).unwrap()),
            "2021-03-03 22:00:00 UTC"
        );
        assert_eq!(TimeUnit::Hours.label(1_614_853_230, &tz), "2021-03-04 12");
    }

    #[test]
    fn dst_day_is_23_hours() {
        let tz = TimeZone::get("Europe/Helsinki").unwrap();
        // DST starts 2021-03-28 in Finland.
        let start = TimeUnit::Days.floor(1_616_932_800, &tz); // 2021-03-28 12:00 UTC
        assert_eq!(TimeUnit::Days.next(start, &tz) - start, 23 * 3600);
    }

    #[test]
    fn bin_cap() {
        assert_eq!(
            bins(0, 1_000_000, TimeUnit::Seconds, &TimeZone::UTC, 50).len(),
            50
        );
    }
}
