//! Human-readable sizes. SI (decimal, metric) prefixes are the default;
//! binary (IEC) prefixes are available where sector/cluster maths matters.

const SI: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
const IEC: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

fn format(bytes: u64, base: f64, units: &[&str; 7]) -> String {
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= base && idx < units.len() - 1 {
        value /= base;
        idx += 1;
    }
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!(
        "{value:.decimals$} {}",
        units.get(idx).copied().unwrap_or("B")
    )
}

/// Formats a byte count with SI prefixes: `1 500 000` → `1.50 MB`.
pub fn format_size(bytes: u64) -> String {
    format(bytes, 1000.0, &SI)
}

/// Formats a byte count with IEC prefixes: `1 048 576` → `1.00 MiB`.
pub fn format_size_iec(bytes: u64) -> String {
    format(bytes, 1024.0, &IEC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case(0, "0 B")]
    #[case(999, "999 B")]
    #[case(1000, "1.00 kB")]
    #[case(1_500_000, "1.50 MB")]
    #[case(25_000_000_000, "25.0 GB")]
    #[case(512_000_000_000_000, "512 TB")]
    #[case(u64::MAX, "18.4 EB")]
    fn si(#[case] n: u64, #[case] want: &str) {
        assert_eq!(format_size(n), want);
    }

    #[rstest]
    #[case(1023, "1023 B")]
    #[case(1024, "1.00 KiB")]
    #[case(1_048_576, "1.00 MiB")]
    fn iec(#[case] n: u64, #[case] want: &str) {
        assert_eq!(format_size_iec(n), want);
    }
}
