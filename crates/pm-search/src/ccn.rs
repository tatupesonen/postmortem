//! Credit card numbers (Autopsy `CreditCardValidator`, `RegexQuery`
//! track parsing and `CreditCards` BIN lookup).
//!
//! * [`is_valid_ccn`]: one separator kind (spaces or dashes), IIN length
//!   table with Guava `RangeMap` "last insert wins" semantics, digit
//!   grouping, Luhn.
//! * [`ccn_hit_is_valid`]: the filter applied to "Credit Card Numbers"
//!   regex hits (a card number inside the hit, shrinking the region).
//! * [`parse_track`]: card number, expiry, service code, discretionary
//!   data, LRC and holder name from magnetic-stripe track 1/2 text.
//! * [`BinDb`]: Bank Identification Numbers from a binlist `ranges.csv`.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

/// Allowed lengths per IIN range, in Autopsy's insertion order (later
/// ranges override earlier overlapping ones). The `{16, 17, 18, 29}` set
/// keeps Autopsy's typo for 19 (parity: Autopsy's own vectors depend on
/// the override order, not on the typo).
const IIN_RANGES: &[(u32, u32, &[u8])] = &[
    (34_000_000, 35_000_000, &[15]),
    (37_000_000, 38_000_000, &[15]),
    (40_000_000, 50_000_000, TWELVE_TO_19),
    (40_260_000, 40_270_000, &[16]),
    (41_750_000, 41_750_100, &[16]),
    (44_050_000, 44_060_000, &[16]),
    (45_080_000, 45_090_000, &[16]),
    (48_440_000, 48_450_000, &[16]),
    (49_130_000, 49_140_000, &[16]),
    (49_170_000, 49_180_000, &[16]),
    (62_000_000, 63_000_000, SIXTEEN_TO_19),
    (51_000_000, 56_000_000, &[16]),
    (22_210_000, 27_210_000, &[16]),
    (50_609_900, 50_619_900, &[16, 19]),
    (65_000_200, 65_002_700, &[16, 19]),
    (50_000_000, 50_100_000, TWELVE_TO_19),
    (56_000_000, 59_000_000, TWELVE_TO_19),
    (60_000_000, 70_000_000, TWELVE_TO_19),
    (63_900_000, 63_910_000, TWELVE_TO_19),
    (67_000_000, 68_000_000, TWELVE_TO_19),
    (30_000_000, 30_600_000, SIXTEEN_TO_19),
    (30_950_000, 30_960_000, SIXTEEN_TO_19),
    (36_000_000, 37_000_000, FOURTEEN_TO_19),
    (38_000_000, 40_000_000, SIXTEEN_TO_19),
    (54_000_000, 56_000_000, FOURTEEN_TO_19),
    (60_110_000, 60_120_000, SIXTEEN_TO_19),
    (62_212_600, 62_292_600, SIXTEEN_TO_19),
    (64_400_000, 66_000_000, SIXTEEN_TO_19),
    (35_280_000, 35_900_000, SIXTEEN_TO_19),
    (50_190_000, 50_200_000, SIXTEEN_TO_19),
    (63_600_000, 63_700_000, SIXTEEN_TO_19),
];
const TWELVE_TO_19: &[u8] = &[12, 13, 14, 15, 16, 17, 18, 19];
const FOURTEEN_TO_19: &[u8] = &[14, 15, 16, 17, 18, 19];
const SIXTEEN_TO_19: &[u8] = &[16, 17, 18, 29];

/// Lengths allowed for a BIN (first eight digits); `None` if no range
/// covers it (any length is fine then).
fn allowed_lengths(bin: u32) -> Option<&'static [u8]> {
    IIN_RANGES
        .iter()
        .rev()
        .find(|(lo, hi, _)| (*lo..*hi).contains(&bin))
        .map(|(_, _, l)| *l)
}

/// Autopsy `CreditCardValidator.isValidCCN`.
pub fn is_valid_ccn(raw: &str) -> bool {
    let space = raw.contains(' ');
    let dash = raw.contains('-');
    if space && dash {
        return false;
    }
    let sep = if space {
        Some(' ')
    } else if dash {
        Some('-')
    } else {
        None
    };
    let (canonical, groups): (String, Vec<&str>) = match sep {
        Some(s) => (
            raw.chars().filter(|&c| c != s).collect(),
            java_split(raw, s),
        ),
        None => (raw.to_owned(), vec![raw]),
    };
    if !canonical.bytes().all(|b| b.is_ascii_digit()) || canonical.len() < 8 {
        return false;
    }
    let Some(bin) = canonical.get(..8).and_then(|b| b.parse::<u32>().ok()) else {
        return false;
    };
    if let Some(lens) = allowed_lengths(bin)
        && !lens.iter().any(|&l| usize::from(l) == canonical.len())
    {
        return false;
    }
    let lens: Vec<usize> = groups.iter().map(|g| g.len()).collect();
    let grouping_ok = match canonical.len() {
        _ if lens.len() == 1 => true,
        15 => lens == [4, 6, 5],
        16 => lens == [4, 4, 4, 4],
        19 => lens == [6, 13] || lens == [4, 4, 4, 4, 3],
        _ => lens.first() == Some(&4),
    };
    grouping_ok && luhn(&canonical)
}

/// Java `String.split(sep)`: trailing empty strings are dropped.
fn java_split(s: &str, sep: char) -> Vec<&str> {
    let mut v: Vec<&str> = s.split(sep).collect();
    while v.len() > 1 && v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// commons-validator `LuhnCheckDigit.isValid`: digits only, non-empty,
/// a non-zero digit sum, and a weighted sum divisible by ten.
pub fn luhn(digits: &str) -> bool {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut total = 0u32;
    let mut raw_sum = 0u32;
    for (i, b) in digits.bytes().rev().enumerate() {
        let d = u32::from(b - b'0');
        raw_sum += d;
        let v = if i % 2 == 1 {
            let x = d * 2;
            if x > 9 { x - 9 } else { x }
        } else {
            d
        };
        total += v;
    }
    raw_sum != 0 && total.is_multiple_of(10)
}

/// Autopsy `RegexQuery.CREDIT_CARD_NUM_PATTERN`.
static CCN_NUM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?<ccn>[2-6]([ -]?[0-9]){11,18})").unwrap_or_else(|_| unreachable!())
});

/// Autopsy's filter for "Credit Card Numbers" hits: some prefix of the
/// hit (at least 12 characters, longest first) contains a number that
/// [`is_valid_ccn`] accepts.
pub fn ccn_hit_is_valid(hit: &str) -> bool {
    let mut end = hit.len();
    while end >= 12 {
        if hit.is_char_boundary(end)
            && let Some(prefix) = hit.get(..end)
            && let Some(c) = CCN_NUM.captures(prefix)
            && let Some(m) = c.name("ccn")
            && is_valid_ccn(m.as_str())
        {
            return true;
        }
        end -= 1;
    }
    false
}

/// Card data parsed from a hit's snippet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardData {
    /// The number as found (with separators): Autopsy's `KAAMOS_KEYWORD`.
    pub raw_number: String,
    /// Digits only: `KAAMOS_CARD_NUMBER` and the account id.
    pub number: String,
    pub expiration: Option<String>,
    pub service_code: Option<String>,
    pub discretionary: Option<String>,
    pub lrc: Option<String>,
    /// Card holder (track 1 only).
    pub name: Option<String>,
}

/// Autopsy `CREDIT_CARD_TRACK1_PATTERN`.
static TRACK1: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?:%?B)?(?<accountNumber>[2-6]([ -]?[0-9]){11,18})\^(?<name>[^^]{2,26})",
        r"(?:\^(?:(?:\^|(?<expiration>\d{4}))(?:(?:\^|(?<serviceCode>\d{3}))",
        r"(?:(?<discretionary>[^?]*)(?:\?(?<LRC>.)?)?)?)?)?)?"
    ))
    .unwrap_or_else(|_| unreachable!())
});

/// Autopsy `CREDIT_CARD_TRACK2_PATTERN`.
static TRACK2: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"[:;<=>?]?(?<accountNumber>[2-6]([ -]?[0-9]){11,18})",
        r"(?:[:;<=>?](?:(?<expiration>\d{4})(?:(?<serviceCode>\d{3})",
        r"(?:(?<discretionary>[^:;<=>?]*)(?:[:;<=>?](?<LRC>.)?)?)?)?)?)?"
    ))
    .unwrap_or_else(|_| unreachable!())
});

/// Autopsy `createCCNAccount` parsing: track 1, then track 2; the first
/// value found for each field wins. `None` without a card number.
pub fn parse_track(snippet: &str) -> Option<CardData> {
    let mut d = CardData::default();
    let mut have_number = false;
    for (track1, re) in [(true, &*TRACK1), (false, &*TRACK2)] {
        let Some(c) = re.captures(snippet) else {
            continue;
        };
        let get = |n: &str| {
            c.name(n)
                .map(|m| m.as_str().to_owned())
                .filter(|s| !s.trim().is_empty())
        };
        if !have_number && let Some(raw) = c.name("accountNumber").map(|m| m.as_str().to_owned()) {
            let number: String = raw.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
            if !number.is_empty() {
                d.raw_number = raw;
                d.number = number;
                have_number = true;
            }
        }
        if track1 {
            d.name = d.name.take().or_else(|| get("name"));
        }
        d.expiration = d.expiration.take().or_else(|| get("expiration"));
        d.service_code = d.service_code.take().or_else(|| get("serviceCode"));
        d.discretionary = d.discretionary.take().or_else(|| get("discretionary"));
        d.lrc = d.lrc.take().or_else(|| get("LRC"));
    }
    have_number.then_some(d)
}

/// What a BIN range says about a card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BinInfo {
    pub start: u32,
    pub end: u32,
    pub number_length: Option<u32>,
    pub scheme: Option<String>,
    pub brand: Option<String>,
    pub card_type: Option<String>,
    pub country: Option<String>,
    pub bank_name: Option<String>,
    pub bank_url: Option<String>,
    pub bank_phone: Option<String>,
    pub bank_city: Option<String>,
}

/// Bank Identification Numbers (Autopsy `CreditCards`), loaded from a
/// binlist `ranges.csv` (`iin_start, iin_end, number_length, scheme,
/// brand, type, country, bank_name, bank_url, bank_phone, bank_city`).
/// Ranges are closed; later rows override earlier overlapping ones.
#[derive(Debug, Clone, Default)]
pub struct BinDb {
    /// In file order.
    ranges: Vec<BinInfo>,
}

impl BinDb {
    /// Parses `ranges.csv` content. Rows with unparsable numbers are
    /// skipped (Autopsy logs them).
    pub fn from_csv(data: &[u8]) -> Result<Self, csv::Error> {
        let mut r = csv::ReaderBuilder::new().flexible(true).from_reader(data);
        let headers = r.headers()?.clone();
        let col = |name: &str| headers.iter().position(|h| h.trim() == name);
        let cols = [
            "iin_start",
            "iin_end",
            "number_length",
            "scheme",
            "brand",
            "type",
            "country",
            "bank_name",
            "bank_url",
            "bank_phone",
            "bank_city",
        ]
        .map(col);
        let mut ranges = Vec::new();
        for rec in r.records() {
            let rec = rec?;
            let field = |i: usize| {
                cols.get(i)
                    .copied()
                    .flatten()
                    .and_then(|c| rec.get(c))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            };
            let Some(start_raw) = field(0) else {
                continue;
            };
            // Start padded with 0s, end (or the start) with 9s, to eight
            // digits. Autopsy pads a blank end from the already-padded
            // start, so 6-digit BINs only cover `XXXXXX00`; we pad the raw
            // start instead (documented fix).
            let end_raw = field(1).unwrap_or_else(|| start_raw.clone());
            let (Some(start), Some(end)) = (pad(&start_raw, '0'), pad(&end_raw, '9')) else {
                tracing::debug!("bad BIN range {start_raw}-{end_raw}");
                continue;
            };
            let number_length = match field(2) {
                Some(n) => match n.parse() {
                    Ok(n) => Some(n),
                    Err(_) => continue,
                },
                None => None,
            };
            ranges.push(BinInfo {
                start,
                end,
                number_length,
                scheme: field(3),
                brand: field(4),
                card_type: field(5),
                country: field(6),
                bank_name: field(7),
                bank_url: field(8),
                bank_phone: field(9),
                bank_city: field(10),
            });
        }
        Ok(Self { ranges })
    }

    /// Loads a `ranges.csv` file.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let data = std::fs::read(path)?;
        Self::from_csv(&data).map_err(std::io::Error::other)
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// The range covering an eight-digit BIN (the last matching row).
    pub fn lookup(&self, bin: u32) -> Option<&BinInfo> {
        self.ranges
            .iter()
            .rev()
            .find(|r| (r.start..=r.end).contains(&bin))
    }

    /// The range for a card number (its first eight digits).
    pub fn lookup_card(&self, number: &str) -> Option<&BinInfo> {
        let bin = number.get(..8)?.parse().ok()?;
        self.lookup(bin)
    }
}

/// Right-pads a digit string to eight digits; `None` if it is not
/// digits or longer than eight.
fn pad(s: &str, with: char) -> Option<u32> {
    if s.is_empty() || s.len() > 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut p = s.to_owned();
    while p.len() < 8 {
        p.push(with);
    }
    p.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cases: &[(&str, bool)]) {
        for &(n, want) in cases {
            assert_eq!(is_valid_ccn(n), want, "{n}");
        }
    }

    /// Autopsy `CreditCardValidatorTest.testLengthMatchesBin`.
    #[test]
    fn length_matches_bin() {
        check(&[
            // Amex
            ("3431 136294 58529", true),
            ("3431-136294-5850", false),
            ("34311362945850", false),
            ("3431 1362 9458 5034", false),
            // Verve
            ("506099002418342", false),
            ("5060990024183426", true),
            ("50609900241834267", false),
            ("506099002418342675", false),
            ("5060990024183426759", true),
            ("50609900241834267591", false),
            // Visa
            ("4000 0000 000", false),
            ("4000 0000 0002", true),
            ("4000 0000 0002 2", true),
            ("4000 0000 0002 26", true),
            ("4000 0000 0002 267", false),
            ("4000 000000 02267", true),
            ("4000 0000 0002 2675", true),
            ("4000 0000 0002 2675 9", true),
            ("4000 0000 0002 2675 91", true),
            ("4000 0000 0002 2675 918", true),
            ("4000 0000 0002 2675 9183", false),
            // Visa Electron
            ("4026 1200 006", false),
            ("4026 0000 0065", false),
            ("4026 0000 0065 9", false),
            ("4026 0000 0065 91", false),
            ("4026 0000 0065 918", false),
            ("4026 0000 0065 9187", true),
            ("4026 0000 0065 9187 0", false),
            ("4026 0000 0065 9183 00", false),
            ("4026 0000 0065 9183 000", false),
            ("4026 0000 0065 9183 0000", false),
        ]);
    }

    /// Autopsy `CreditCardValidatorTest.testIsValidCCN16` … `12`.
    #[test]
    fn is_valid_ccn_vectors() {
        check(&[
            ("1234567890318342", true),
            ("1234-5678-9031-8342", true),
            ("1234 5678 9031 8342", true),
            ("1234567890318341", false),
            ("1234-5678-9031 8342", false),
            ("1234-5678-90-318342", false),
            ("1234 5678 90 318342", false),
            ("1-2-3-4-5-6-7-8-9-0-3-1-8-3-4-2", false),
            // 15
            ("3431 136294 58529", true),
            ("3431-136294-58529", true),
            ("343113629458529", true),
            ("343113629458528", false),
            ("3431 13629458 529", false),
            ("3431 136294-58529", false),
            ("377585291285489", true),
            ("3775-852912-85489", true),
            ("3775 852912 85489", true),
            ("377585291285488", false),
            ("3775-852912 85489", false),
            ("37-7585-29-1285489", false),
            ("377585 29128548 9", false),
            // 19
            ("4539747947839518654", true),
            ("4539-7479-4783-9518-654", true),
            ("4539 7479 4783 9518 654", true),
            ("4539747947839518653", false),
            ("4539-7479 4783 9518 654", false),
            ("45374 79 4783 9518 654", false),
            ("6239747947839518659", true),
            ("623974 7947839518659", true),
            ("623974-7947839518659", true),
            ("6239747947839518658", false),
            ("623974-79478395 18659", false),
            ("62397-47947839518659", false),
            // 18
            ("123456789031834267", true),
            ("1234 5678 9031 8342 67", true),
            ("1234-56789031834-267", true),
            ("123456789031834266", false),
            ("123 456789031834267", false),
            ("1234-56789 031834267", false),
            // 17
            ("12345678903183426", true),
            ("1234 5678 9031 8342 6", true),
            ("1234-56789031834-26", true),
            ("12345678903183425", false),
            ("123 45678903183426", false),
            ("1234-56789 03183426", false),
            // 14
            ("12345678903183", true),
            ("1234 5678 9031 83", true),
            ("1234-5678903183", true),
            ("12345678903182", false),
            ("123 45678903183", false),
            ("1234-56789 03183", false),
            // 13
            ("1234567890318", true),
            ("1234 5678 9031 8", true),
            ("1234-567890318", true),
            ("1234567890317", false),
            ("123 4567890318", false),
            ("1234-56789 0318", false),
            // 12
            ("123456789031", true),
            ("1234 5678 9031", true),
            ("1234-56789031", true),
            ("123456789030", false),
            ("123 456789031", false),
            ("1234-56789 031", false),
        ]);
    }

    #[test]
    fn last_insert_wins_and_edge_cases() {
        // Maestro (60000000–70000000) overrides UnionPay's typo'd set.
        assert_eq!(allowed_lengths(62_397_479), Some(TWELVE_TO_19));
        // Discover (inserted after Maestro) overrides it again.
        assert_eq!(allowed_lengths(64_500_000), Some(SIXTEEN_TO_19));
        // Diners US/CA overrides MasterCard.
        assert_eq!(allowed_lengths(54_100_000), Some(FOURTEEN_TO_19));
        assert_eq!(allowed_lengths(12_345_678), None);
        assert!(!is_valid_ccn(""));
        assert!(!is_valid_ccn("1234567"));
        assert!(!is_valid_ccn("abcdefghijkl"));
        assert!(!is_valid_ccn("000000000000"));
        assert!(!luhn("0000"));
        assert!(luhn("79927398713"));
    }

    #[test]
    fn hit_filter() {
        assert!(ccn_hit_is_valid("4539747947839518654"));
        assert!(ccn_hit_is_valid("%b4539747947839518654^"));
        // Trailing garbage is cut by shrinking the region.
        assert!(ccn_hit_is_valid("4111111111111111 1"));
        assert!(!is_valid_ccn("4111111111111111 1"));
        // The first digit must be 2–6.
        assert!(!ccn_hit_is_valid("1234567890318342"));
        assert!(!ccn_hit_is_valid("12345"));
        assert!(!ccn_hit_is_valid("ééééééééééééééé"));
    }

    #[test]
    fn tracks() {
        let t1 = parse_track("x %B4111111111111111^DOE/JOHN^2512101abc?Z y").unwrap();
        assert_eq!(t1.number, "4111111111111111");
        assert_eq!(t1.name.as_deref(), Some("DOE/JOHN"));
        assert_eq!(t1.expiration.as_deref(), Some("2512"));
        assert_eq!(t1.service_code.as_deref(), Some("101"));
        assert_eq!(t1.discretionary.as_deref(), Some("abc"));
        assert_eq!(t1.lrc.as_deref(), Some("Z"));

        let t2 = parse_track(";4111-1111-1111-1111=2512101999?1").unwrap();
        assert_eq!(t2.raw_number, "4111-1111-1111-1111");
        assert_eq!(t2.number, "4111111111111111");
        assert_eq!(t2.expiration.as_deref(), Some("2512"));
        assert_eq!(t2.service_code.as_deref(), Some("101"));
        assert_eq!(t2.discretionary.as_deref(), Some("999"));
        assert_eq!(t2.lrc.as_deref(), Some("1"));
        assert_eq!(t2.name, None);

        // A bare number matches track 2.
        let bare = parse_track("card «4111111111111111« end").unwrap();
        assert_eq!(bare.number, "4111111111111111");
        assert_eq!(bare.expiration, None);
        assert!(parse_track("no digits here").is_none());
    }

    #[test]
    fn bin_database() {
        let csv = "iin_start,iin_end,number_length,number_luhn,scheme,brand,type,prepaid,country,bank_name,bank_logo,bank_url,bank_phone,bank_city\n\
                   411111,,16,true,visa,Classic,debit,,US,Test Bank,,www.test.example,+1 555 0100,Springfield\n\
                   40000000,49999999,,,visa,,,,,,,,,\n\
                   bad,,,,,,,,,,,,,\n\
                   5100,5199,x,,mastercard,,,,,,,,,\n";
        let db = BinDb::from_csv(csv.as_bytes()).unwrap();
        assert_eq!(db.len(), 2);
        // The broad range comes later in the file and wins.
        assert_eq!(db.lookup(41_111_100).unwrap().brand, None);
        let specific = BinDb::from_csv(
            "iin_start,iin_end,number_length,scheme,brand,type,country,bank_name,bank_url,bank_phone,bank_city\n\
             40000000,49999999,,visa,,,,,,,\n\
             411111,,16,visa,Classic,debit,US,Test Bank,www.test.example,+1 555 0100,Springfield\n"
                .as_bytes(),
        )
        .unwrap();
        let b = specific.lookup_card("4111111111111111").unwrap();
        assert_eq!(b.start, 41_111_100);
        assert_eq!(b.end, 41_111_199);
        assert_eq!(b.number_length, Some(16));
        assert_eq!(b.bank_name.as_deref(), Some("Test Bank"));
        assert_eq!(b.bank_city.as_deref(), Some("Springfield"));
        assert_eq!(b.country.as_deref(), Some("US"));
        assert!(specific.lookup_card("3111111111111111").is_none());
        assert!(specific.lookup_card("41").is_none());
    }
}
