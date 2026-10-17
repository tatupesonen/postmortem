//! Top-level domain check for e-mail hits (commons-validator
//! `DomainValidator.getInstance(true).isValidTld`).
//!
//! `data/tlds.txt` is IANA's `tlds-alpha-by-domain.txt` (version
//! 2026101700), lower-cased, without the punycode (`xn--`) entries, which
//! the e-mail regex cannot match. The local TLDs `localhost` and
//! `localdomain` are allowed, as with `allowLocal = true`.

use std::collections::HashSet;
use std::sync::LazyLock;

static TLDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("../data/tlds.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .chain(["localhost", "localdomain"])
        .collect()
});

/// Whether `tld` (with or without a leading dot, any case) is a known
/// top-level domain.
pub fn is_valid_tld(tld: &str) -> bool {
    let t = tld.strip_prefix('.').unwrap_or(tld).to_ascii_lowercase();
    TLDS.contains(t.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_and_unknown() {
        for t in ["com", ".COM", "fi", "org", "museum", "localhost"] {
            assert!(is_valid_tld(t), "{t}");
        }
        for t in ["", ".", "c", "invalidtld", "inva"] {
            assert!(!is_valid_tld(t), "{t}");
        }
    }
}
