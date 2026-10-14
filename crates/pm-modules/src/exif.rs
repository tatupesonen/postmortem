//! A small EXIF reader for the Picture Analyzer.
//!
//! Reads exactly what Autopsy's `EXIFProcessor` uses (via drew
//! metadata-extractor): IFD0 `Make`/`Model`, Exif sub-IFD
//! `DateTimeOriginal`, and the GPS position and altitude. It works over a
//! [`ReadAt`] with random access, so a TIFF whose IFDs sit at the end of a
//! multi-gigabyte file costs a few small reads instead of reading the file
//! (the reason `kamadak-exif`, which needs the whole TIFF in memory, was not
//! adopted; ADR-009).
//!
//! Evidence is hostile: every offset is bounds-checked, value sizes are
//! capped and the JPEG segment walk is bounded.

#![allow(clippy::many_single_char_names, reason = "binary field names")]

use std::io;

use pm_core::ReadAt;

/// Largest value read for one tag (strings, rational arrays).
const MAX_VALUE: usize = 64 * 1024;
/// Largest number of entries read from one IFD.
const MAX_ENTRIES: u16 = 2048;
/// JPEG segments examined before giving up.
const MAX_SEGMENTS: usize = 4096;

const TAG_MAKE: u16 = 0x010F;
const TAG_MODEL: u16 = 0x0110;
const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_DATETIME_ORIGINAL: u16 = 0x9003;
const TAG_GPS_LATITUDE_REF: u16 = 0x0001;
const TAG_GPS_LATITUDE: u16 = 0x0002;
const TAG_GPS_LONGITUDE_REF: u16 = 0x0003;
const TAG_GPS_LONGITUDE: u16 = 0x0004;
const TAG_GPS_ALTITUDE: u16 = 0x0006;

/// A TIFF rational (`RATIONAL` or `SRATIONAL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rational {
    pub(crate) num: i64,
    pub(crate) den: i64,
}

impl Rational {
    /// metadata-extractor's `Rational.doubleValue()`: zero numerators give
    /// 0, zero denominators give ±infinity.
    pub(crate) fn to_f64(self) -> f64 {
        if self.num == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }
}

/// IFD0 (metadata-extractor's `ExifIFD0Directory`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Ifd0 {
    pub(crate) make: Option<String>,
    pub(crate) model: Option<String>,
}

/// The Exif sub-IFD (`ExifSubIFDDirectory`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SubIfd {
    /// `DateTimeOriginal` as stored (`"2021:03:04 10:20:30"`).
    pub(crate) date_time_original: Option<String>,
}

/// The GPS IFD (`GpsDirectory`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Gps {
    pub(crate) latitude_ref: Option<String>,
    /// Degrees, minutes, seconds (only when stored as three rationals).
    pub(crate) latitude: Option<Vec<Rational>>,
    pub(crate) longitude_ref: Option<String>,
    pub(crate) longitude: Option<Vec<Rational>>,
    /// `GPSAltitude` (a single rational or integer).
    pub(crate) altitude: Option<Rational>,
}

impl Gps {
    /// Decimal degrees (`GpsDirectory.getGeoLocation()`): needs three
    /// rationals and a reference for both axes; `S`/`W` are negative.
    pub(crate) fn geo_location(&self) -> Option<(f64, f64)> {
        let lat = dms(
            self.latitude.as_deref()?,
            self.latitude_ref.as_deref()?.eq_ignore_ascii_case("S"),
        )?;
        let lon = dms(
            self.longitude.as_deref()?,
            self.longitude_ref.as_deref()?.eq_ignore_ascii_case("W"),
        )?;
        Some((lat, lon))
    }
}

/// `GeoLocation.degreesMinutesSecondsToDecimal`.
fn dms(v: &[Rational], negative: bool) -> Option<f64> {
    let [d, m, s] = v else { return None };
    let decimal = d.to_f64().abs() + m.to_f64() / 60.0 + s.to_f64() / 3600.0;
    if decimal.is_nan() {
        return None;
    }
    Some(if negative { -decimal } else { decimal })
}

/// The directories found in a file (first of each kind wins, like
/// metadata-extractor's `getFirstDirectoryOfType`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ExifData {
    pub ifd0: Option<Ifd0>,
    pub(crate) sub_ifd: Option<SubIfd>,
    pub(crate) gps: Option<Gps>,
}

impl ExifData {
    fn merge(&mut self, other: Self) {
        if self.ifd0.is_none() {
            self.ifd0 = other.ifd0;
        }
        if self.sub_ifd.is_none() {
            self.sub_ifd = other.sub_ifd;
        }
        if self.gps.is_none() {
            self.gps = other.gps;
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.ifd0.is_none() && self.sub_ifd.is_none() && self.gps.is_none()
    }
}

/// Reads the EXIF of a JPEG: every `APP1` segment that starts with
/// `Exif\0\0`, up to the start of the scan data.
pub(crate) fn read_jpeg(src: &dyn ReadAt) -> io::Result<ExifData> {
    let mut out = ExifData::default();
    let mut soi = [0u8; 2];
    if src.read_at(0, &mut soi)? < 2 || soi != [0xFF, 0xD8] {
        return Ok(out);
    }
    let size = src.size();
    let mut off = 2u64;
    for _ in 0..MAX_SEGMENTS {
        let mut hdr = [0u8; 4];
        if off >= size || src.read_at(off, &mut hdr)? < 2 || hdr[0] != 0xFF {
            break;
        }
        // Fill bytes: any number of 0xFF before the marker code.
        let mut marker = hdr[1];
        while marker == 0xFF {
            off += 1;
            if src.read_at(off, &mut hdr)? < 2 {
                return Ok(out);
            }
            marker = hdr[1];
        }
        match marker {
            0xD9 | 0xDA => break, // EOI / start of scan
            0x01 | 0xD0..=0xD8 => {
                off += 2; // no length field
                continue;
            }
            _ => {}
        }
        let Some(len_bytes) = read_array::<2>(src, off + 2)? else {
            break;
        };
        let len = u64::from(u16::from_be_bytes(len_bytes));
        if len < 2 {
            break;
        }
        if marker == 0xE1 && len >= 8 {
            let payload = src.read_vec_at(off + 4, (len - 2) as usize);
            if let Ok(p) = payload
                && let Some(tiff) = p.strip_prefix(b"Exif\0\0")
            {
                let mem = pm_core::io::MemSource::new(tiff.to_vec());
                out.merge(read_tiff(&mem)?);
            }
        }
        off += 2 + len;
    }
    Ok(out)
}

/// Reads the EXIF directories of a TIFF structure (a `.tif` file or the
/// payload of a JPEG `APP1` segment).
pub(crate) fn read_tiff(src: &dyn ReadAt) -> io::Result<ExifData> {
    let mut out = ExifData::default();
    let Some(hdr) = read_array::<8>(src, 0)? else {
        return Ok(out);
    };
    let big = match &hdr[..2] {
        b"MM" => true,
        b"II" => false,
        _ => return Ok(out),
    };
    let t = Tiff { src, big };
    // Standard TIFF, plus the variants metadata-extractor accepts
    // (Olympus ORF `IIRO`/`IIRS`, Panasonic RW2).
    let marker = t.u16(&hdr[2..4]);
    if !matches!(marker, 0x2A | 0x4F52 | 0x5352 | 0x55) {
        return Ok(out);
    }
    let ifd0_off = u64::from(t.u32(&hdr[4..8]));
    let Some(ifd0) = t.ifd(ifd0_off)? else {
        return Ok(out);
    };
    out.ifd0 = Some(Ifd0 {
        make: t.ascii(ifd0.get(TAG_MAKE))?,
        model: t.ascii(ifd0.get(TAG_MODEL))?,
    });
    if let Some(off) = t.pointer(ifd0.get(TAG_EXIF_IFD))
        && off != ifd0_off
        && let Some(sub) = t.ifd(off)?
    {
        out.sub_ifd = Some(SubIfd {
            date_time_original: t.ascii(sub.get(TAG_DATETIME_ORIGINAL))?,
        });
    }
    if let Some(off) = t.pointer(ifd0.get(TAG_GPS_IFD))
        && off != ifd0_off
        && let Some(gps) = t.ifd(off)?
    {
        out.gps = Some(Gps {
            latitude_ref: t.ascii(gps.get(TAG_GPS_LATITUDE_REF))?,
            latitude: t.rational_array(gps.get(TAG_GPS_LATITUDE))?,
            longitude_ref: t.ascii(gps.get(TAG_GPS_LONGITUDE_REF))?,
            longitude: t.rational_array(gps.get(TAG_GPS_LONGITUDE))?,
            altitude: t.rational(gps.get(TAG_GPS_ALTITUDE))?,
        });
    }
    Ok(out)
}

/// Reads `N` bytes at `off`, `None` past the end.
fn read_array<const N: usize>(src: &dyn ReadAt, off: u64) -> io::Result<Option<[u8; N]>> {
    let mut b = [0u8; N];
    match src.read_exact_at(off, &mut b) {
        Ok(()) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u16,
    ty: u16,
    count: u32,
    /// The 4-byte value/offset field, as stored.
    field: [u8; 4],
}

struct Ifd(Vec<Entry>);

impl Ifd {
    /// First entry with `tag`.
    fn get(&self, tag: u16) -> Option<Entry> {
        self.0.iter().find(|e| e.tag == tag).copied()
    }
}

struct Tiff<'a> {
    src: &'a dyn ReadAt,
    big: bool,
}

/// Size in bytes of one value of a TIFF type.
fn type_size(ty: u16) -> Option<usize> {
    Some(match ty {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

impl Tiff<'_> {
    fn u16(&self, b: &[u8]) -> u16 {
        let a = [
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ];
        if self.big {
            u16::from_be_bytes(a)
        } else {
            u16::from_le_bytes(a)
        }
    }

    fn u32(&self, b: &[u8]) -> u32 {
        let mut a = [0u8; 4];
        for (d, s) in a.iter_mut().zip(b) {
            *d = *s;
        }
        if self.big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        }
    }

    /// The entries of the IFD at `off` (`None` if it cannot be read).
    fn ifd(&self, off: u64) -> io::Result<Option<Ifd>> {
        let Some(n) = read_array::<2>(self.src, off)? else {
            return Ok(None);
        };
        let n = self.u16(&n).min(MAX_ENTRIES);
        let mut entries = Vec::with_capacity(usize::from(n));
        for i in 0..u64::from(n) {
            let Some(e) = read_array::<12>(self.src, off + 2 + i * 12)? else {
                break; // truncated directory: keep what was read
            };
            entries.push(Entry {
                tag: self.u16(&e[0..2]),
                ty: self.u16(&e[2..4]),
                count: self.u32(&e[4..8]),
                field: [e[8], e[9], e[10], e[11]],
            });
        }
        Ok(Some(Ifd(entries)))
    }

    /// The raw bytes of an entry's value (inline or at its offset).
    fn value(&self, e: Entry) -> io::Result<Option<Vec<u8>>> {
        let Some(size) = type_size(e.ty).and_then(|s| s.checked_mul(e.count as usize)) else {
            return Ok(None);
        };
        if size > MAX_VALUE {
            return Ok(None);
        }
        if size <= 4 {
            return Ok(Some(e.field.get(..size).unwrap_or_default().to_vec()));
        }
        let off = u64::from(self.u32(&e.field));
        match self.src.read_vec_at(off, size) {
            Ok(v) => Ok(Some(v)),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// An `ASCII` value up to its first NUL (metadata-extractor's
    /// `getNullTerminatedStringValue`).
    fn ascii(&self, e: Option<Entry>) -> io::Result<Option<String>> {
        let Some(e) = e.filter(|e| e.ty == 2) else {
            return Ok(None);
        };
        let Some(v) = self.value(e)? else {
            return Ok(None);
        };
        let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
        let bytes = v.get(..end).unwrap_or_default();
        Ok(Some(match std::str::from_utf8(bytes) {
            Ok(s) => s.to_owned(),
            // Not UTF-8: decode as Latin-1.
            Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
        }))
    }

    /// A sub-IFD offset (`LONG` or `IFD` type).
    fn pointer(&self, e: Option<Entry>) -> Option<u64> {
        e.filter(|e| matches!(e.ty, 4 | 13) && e.count >= 1)
            .map(|e| u64::from(self.u32(&e.field)))
    }

    fn rationals(&self, e: Entry) -> io::Result<Option<Vec<Rational>>> {
        let signed = match e.ty {
            5 => false,
            10 => true,
            _ => return Ok(None),
        };
        let Some(v) = self.value(e)? else {
            return Ok(None);
        };
        Ok(Some(
            v.chunks_exact(8)
                .map(|c| {
                    let (n, d) = (self.u32(c), self.u32(c.get(4..).unwrap_or_default()));
                    if signed {
                        Rational {
                            num: i64::from(n as i32),
                            den: i64::from(d as i32),
                        }
                    } else {
                        Rational {
                            num: i64::from(n),
                            den: i64::from(d),
                        }
                    }
                })
                .collect(),
        ))
    }

    /// A rational array (`getRationalArray`: only multi-valued rationals).
    fn rational_array(&self, e: Option<Entry>) -> io::Result<Option<Vec<Rational>>> {
        match e {
            Some(e) if e.count > 1 => self.rationals(e),
            _ => Ok(None),
        }
    }

    /// A single rational, or an integer as `n/1` (`getRational`).
    fn rational(&self, e: Option<Entry>) -> io::Result<Option<Rational>> {
        let Some(e) = e.filter(|e| e.count == 1) else {
            return Ok(None);
        };
        if matches!(e.ty, 5 | 10) {
            return Ok(self.rationals(e)?.and_then(|v| v.first().copied()));
        }
        let Some(v) = self.value(e)? else {
            return Ok(None);
        };
        let num = match e.ty {
            1 => v.first().map(|&b| i64::from(b)),
            6 => v.first().map(|&b| i64::from(b as i8)),
            3 => Some(i64::from(self.u16(&v))),
            8 => Some(i64::from(self.u16(&v) as i16)),
            4 => Some(i64::from(self.u32(&v))),
            9 => Some(i64::from(self.u32(&v) as i32)),
            _ => None,
        };
        Ok(num.map(|num| Rational { num, den: 1 }))
    }
}

/// metadata-extractor's date patterns, in the order it tries them.
const DATE_PATTERNS: [&str; 12] = [
    "yyyy:MM:dd HH:mm:ss",
    "yyyy:MM:dd HH:mm",
    "yyyy-MM-dd HH:mm:ss",
    "yyyy-MM-dd HH:mm",
    "yyyy.MM.dd HH:mm:ss",
    "yyyy.MM.dd HH:mm",
    "yyyy-MM-dd'T'HH:mm:ss",
    "yyyy-MM-dd'T'HH:mm",
    "yyyy-MM-dd",
    "yyyy-MM",
    "yyyyMMdd", // as used in IPTC data
    "yyyy",
];

/// Parses an EXIF date string as metadata-extractor's `Directory.getDate`
/// does: an optional fractional-second part and trailing zone offset are
/// split off, then the first pattern that parses a prefix of the string
/// wins. Returns local fields and, if the string carried one, its UTC
/// offset in seconds.
///
/// Unlike Java's lenient `SimpleDateFormat`, out-of-range fields (such as
/// the all-zero "unknown date" `0000:00:00 00:00:00`) give `None` instead
/// of a date rolled into another year.
pub(crate) fn parse_exif_date(s: &str) -> Option<(jiff::civil::DateTime, Option<i32>)> {
    let mut s = s.to_owned();
    // `(\d\d:\d\d:\d\d)(\.\d+)` → drop the fraction (sub-second precision
    // is lost when Autopsy divides by 1000 anyway).
    if let Some(dot) = find_subsecond(&s) {
        let end = s.get(dot + 1..).map_or(s.len(), |r| {
            dot + 1 + r.bytes().take_while(u8::is_ascii_digit).count()
        });
        s.replace_range(dot..end, "");
    }
    // `(Z|[+-]\d\d:\d\d|[+-]\d\d\d\d)$` → explicit zone.
    let mut offset = None;
    let b = s.as_bytes();
    let n = b.len();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    let num = |r: std::ops::Range<usize>| -> i32 {
        b.get(r).map_or(0, |d| {
            d.iter().fold(0, |a, &c| a * 10 + i32::from(c - b'0'))
        })
    };
    let sign = |i: usize| match b.get(i) {
        Some(b'+') => Some(1),
        Some(b'-') => Some(-1),
        _ => None,
    };
    if n >= 6
        && let Some(sg) = sign(n - 6)
        && digits(n - 5..n - 3)
        && b.get(n - 3) == Some(&b':')
        && digits(n - 2..n)
    {
        offset = Some(sg * (num(n - 5..n - 3) * 3600 + num(n - 2..n) * 60));
        s.truncate(n - 6);
    } else if n >= 5
        && let Some(sg) = sign(n - 5)
        && digits(n - 4..n)
    {
        offset = Some(sg * (num(n - 4..n - 2) * 3600 + num(n - 2..n) * 60));
        s.truncate(n - 5);
    } else if s.ends_with('Z') {
        offset = Some(0);
        s.truncate(n - 1);
    }
    DATE_PATTERNS
        .iter()
        .find_map(|p| parse_with(p, &s))
        .and_then(|f| {
            let [y, mo, d, h, mi, sec] = f;
            jiff::civil::DateTime::new(
                i16::try_from(y).ok()?,
                i8::try_from(mo).ok()?,
                i8::try_from(d).ok()?,
                i8::try_from(h).ok()?,
                i8::try_from(mi).ok()?,
                i8::try_from(sec).ok()?,
                0,
            )
            .ok()
            .filter(|_| y >= 1)
        })
        .map(|dt| (dt, offset))
}

/// Position of the `.` in the first `dd:dd:dd.d+` of `s`.
fn find_subsecond(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    b.windows(10)
        .position(|w| {
            matches!(w, [a, b, b':', c, d, b':', e, f, b'.', g]
                if [a, b, c, d, e, f, g].iter().all(|x| x.is_ascii_digit()))
        })
        .map(|i| i + 8)
}

/// `SimpleDateFormat`-style prefix parse of `s` with pattern `p` (only the
/// letters used above). Fields: year, month, day, hour, minute, second.
fn parse_with(p: &str, s: &str) -> Option<[i64; 6]> {
    let mut f = [0i64, 1, 1, 0, 0, 0];
    let pat: Vec<char> = p.replace("'T'", "T").chars().collect();
    let txt = s.as_bytes();
    let (mut i, mut pos) = (0usize, 0usize);
    while let Some(&c) = pat.get(i) {
        let run = pat.get(i..)?.iter().take_while(|&&x| x == c).count();
        let idx = match c {
            'y' => Some(0),
            'M' => Some(1),
            'd' => Some(2),
            'H' => Some(3),
            'm' => Some(4),
            's' => Some(5),
            _ => None,
        };
        if let Some(idx) = idx {
            // Abutting numeric fields (`yyyyMMdd`) take exactly their
            // width; otherwise a field takes all its digits.
            let abutting = pat
                .get(i + run)
                .is_some_and(|n| matches!(n, 'y' | 'M' | 'd' | 'H' | 'm' | 's'));
            let avail = txt
                .get(pos..)?
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            let take = if abutting { run.min(avail) } else { avail };
            if take == 0 || (abutting && take < run) {
                return None;
            }
            let v = std::str::from_utf8(txt.get(pos..pos + take)?)
                .ok()?
                .parse::<i64>()
                .ok()?;
            if let Some(slot) = f.get_mut(idx) {
                *slot = v;
            }
            pos += take;
        } else {
            for _ in 0..run {
                if txt.get(pos) != Some(&(c as u8)) {
                    return None;
                }
                pos += 1;
            }
        }
        i += run;
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    /// Builds a TIFF structure with IFD0 (`Make`, `Model`, pointers), an
    /// Exif sub-IFD (`DateTimeOriginal`) and a GPS IFD.
    pub(crate) struct TiffBuilder {
        big: bool,
        ifd0: Vec<(u16, u16, u32, Vec<u8>)>,
        sub: Vec<(u16, u16, u32, Vec<u8>)>,
        gps: Vec<(u16, u16, u32, Vec<u8>)>,
    }

    impl TiffBuilder {
        fn new(big: bool) -> Self {
            Self {
                big,
                ifd0: Vec::new(),
                sub: Vec::new(),
                gps: Vec::new(),
            }
        }
        fn u16b(&self, v: u16) -> [u8; 2] {
            if self.big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        }
        fn u32b(&self, v: u32) -> [u8; 4] {
            if self.big {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        }
        fn ascii(s: &str) -> (u16, u32, Vec<u8>) {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            (2, v.len() as u32, v)
        }
        fn rats(&self, r: &[(u32, u32)]) -> (u16, u32, Vec<u8>) {
            let mut v = Vec::new();
            for &(n, d) in r {
                v.extend(self.u32b(n));
                v.extend(self.u32b(d));
            }
            (5, r.len() as u32, v)
        }
        fn make(mut self, s: &str) -> Self {
            let (t, c, v) = Self::ascii(s);
            self.ifd0.push((TAG_MAKE, t, c, v));
            self
        }
        fn model(mut self, s: &str) -> Self {
            let (t, c, v) = Self::ascii(s);
            self.ifd0.push((TAG_MODEL, t, c, v));
            self
        }
        fn date(mut self, s: &str) -> Self {
            let (t, c, v) = Self::ascii(s);
            self.sub.push((TAG_DATETIME_ORIGINAL, t, c, v));
            self
        }
        fn gps_tag(mut self, tag: u16, val: (u16, u32, Vec<u8>)) -> Self {
            self.gps.push((tag, val.0, val.1, val.2));
            self
        }
        fn position(self, lat: (&str, [(u32, u32); 3]), lon: (&str, [(u32, u32); 3])) -> Self {
            let la = self.rats(&lat.1);
            let lo = self.rats(&lon.1);
            self.gps_tag(TAG_GPS_LATITUDE_REF, Self::ascii(lat.0))
                .gps_tag(TAG_GPS_LATITUDE, la)
                .gps_tag(TAG_GPS_LONGITUDE_REF, Self::ascii(lon.0))
                .gps_tag(TAG_GPS_LONGITUDE, lo)
        }
        fn altitude(self, n: u32, d: u32) -> Self {
            let a = self.rats(&[(n, d)]);
            self.gps_tag(TAG_GPS_ALTITUDE, a)
        }

        /// Serialises the IFD at `at` and returns its bytes (values after
        /// the entries).
        fn ifd_bytes(&self, at: u32, mut entries: Vec<(u16, u16, u32, Vec<u8>)>) -> Vec<u8> {
            entries.sort_by_key(|e| e.0);
            let n = entries.len() as u32;
            let mut data_off = at + 2 + n * 12 + 4;
            let mut head = Vec::new();
            let mut data = Vec::new();
            head.extend(self.u16b(n as u16));
            for (tag, ty, count, val) in entries {
                head.extend(self.u16b(tag));
                head.extend(self.u16b(ty));
                head.extend(self.u32b(count));
                if val.len() <= 4 {
                    let mut f = val.clone();
                    f.resize(4, 0);
                    head.extend(f);
                } else {
                    head.extend(self.u32b(data_off));
                    data_off += val.len() as u32;
                    data.extend(val);
                }
            }
            head.extend([0; 4]); // no next IFD
            head.extend(data);
            head
        }

        pub(crate) fn build(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend(if self.big { b"MM" } else { b"II" });
            out.extend(self.u16b(42));
            out.extend(self.u32b(8));
            // Layout: IFD0 at 8, then sub-IFD, then GPS IFD.
            let mut ifd0 = self.ifd0.clone();
            let sizes = |e: &Vec<(u16, u16, u32, Vec<u8>)>, extra: u32| -> u32 {
                let n = e.len() as u32 + extra;
                2 + n * 12
                    + 4
                    + e.iter()
                        .filter(|x| x.3.len() > 4)
                        .map(|x| x.3.len() as u32)
                        .sum::<u32>()
            };
            let extra = u32::from(!self.sub.is_empty()) + u32::from(!self.gps.is_empty());
            let ifd0_len = sizes(&ifd0, extra);
            let sub_at = 8 + ifd0_len;
            let sub_len = if self.sub.is_empty() {
                0
            } else {
                sizes(&self.sub, 0)
            };
            let gps_at = sub_at + sub_len;
            if !self.sub.is_empty() {
                ifd0.push((TAG_EXIF_IFD, 4, 1, self.u32b(sub_at).to_vec()));
            }
            if !self.gps.is_empty() {
                ifd0.push((TAG_GPS_IFD, 4, 1, self.u32b(gps_at).to_vec()));
            }
            out.extend(self.ifd_bytes(8, ifd0));
            if !self.sub.is_empty() {
                out.extend(self.ifd_bytes(sub_at, self.sub.clone()));
            }
            if !self.gps.is_empty() {
                out.extend(self.ifd_bytes(gps_at, self.gps.clone()));
            }
            out
        }
    }

    fn jpeg_with(app1: Option<&[u8]>) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        // APP0 JFIF first, as cameras and editors write it.
        v.extend([0xFF, 0xE0, 0x00, 0x10]);
        v.extend(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        if let Some(t) = app1 {
            let len = (t.len() + 8) as u16;
            v.extend([0xFF, 0xE1]);
            v.extend(len.to_be_bytes());
            v.extend(b"Exif\0\0");
            v.extend(t);
        }
        v.extend([0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6]);
        v.extend([0xFF, 0xE1, 0x00, 0x04, 0, 0]); // after SOS: ignored
        v.extend([0xFF, 0xD9]);
        v
    }

    fn sample(big: bool) -> TiffBuilder {
        TiffBuilder::new(big)
            .make("Canon")
            .model("Canon EOS 5D")
            .date("2021:03:04 10:20:30")
            .position(
                ("N", [(60, 1), (10, 1), (3000, 100)]),
                ("W", [(24, 1), (56, 1), (15, 1)]),
            )
            .altitude(125, 10)
    }

    #[test]
    fn reads_all_fields_both_byte_orders() {
        for big in [false, true] {
            let tiff = sample(big).build();
            let d = read_tiff(&MemSource::new(tiff.clone())).unwrap();
            assert_eq!(d.ifd0.as_ref().unwrap().make.as_deref(), Some("Canon"));
            assert_eq!(
                d.ifd0.as_ref().unwrap().model.as_deref(),
                Some("Canon EOS 5D")
            );
            assert_eq!(
                d.sub_ifd.as_ref().unwrap().date_time_original.as_deref(),
                Some("2021:03:04 10:20:30")
            );
            let gps = d.gps.as_ref().unwrap();
            let (lat, lon) = gps.geo_location().unwrap();
            assert!((lat - (60.0 + 10.0 / 60.0 + 30.0 / 3600.0)).abs() < 1e-9);
            assert!((lon + (24.0 + 56.0 / 60.0 + 15.0 / 3600.0)).abs() < 1e-9);
            assert!((gps.altitude.unwrap().to_f64() - 12.5).abs() < 1e-12);

            let jpeg = jpeg_with(Some(&tiff));
            let j = read_jpeg(&MemSource::new(jpeg)).unwrap();
            assert_eq!(j, d);
        }
    }

    #[test]
    fn jpeg_without_exif_and_non_jpeg() {
        let d = read_jpeg(&MemSource::new(jpeg_with(None))).unwrap();
        assert!(d.is_empty());
        let d = read_jpeg(&MemSource::new(b"GIF89a....".to_vec())).unwrap();
        assert!(d.is_empty());
        let d = read_tiff(&MemSource::new(b"XX*\0\x08\0\0\0".to_vec())).unwrap();
        assert!(d.is_empty());
    }

    #[test]
    fn first_exif_segment_wins_per_directory() {
        let first = TiffBuilder::new(false).make("First").build();
        let second = TiffBuilder::new(true)
            .make("Second")
            .date("2020:01:02 03:04:05")
            .build();
        let mut v = vec![0xFF, 0xD8];
        for t in [&first, &second] {
            v.extend([0xFF, 0xE1]);
            v.extend(((t.len() + 8) as u16).to_be_bytes());
            v.extend(b"Exif\0\0");
            v.extend(t.iter());
        }
        // Fill bytes before a marker are allowed.
        v.extend([0xFF, 0xFF, 0xD9]);
        let d = read_jpeg(&MemSource::new(v)).unwrap();
        assert_eq!(d.ifd0.unwrap().make.as_deref(), Some("First"));
        assert_eq!(
            d.sub_ifd.unwrap().date_time_original.as_deref(),
            Some("2020:01:02 03:04:05")
        );
    }

    #[test]
    fn geo_location_rules() {
        let r = |n, d| Rational { num: n, den: d };
        let mut g = Gps {
            latitude_ref: Some("s".into()),
            latitude: Some(vec![r(-33, 1), r(30, 1), r(0, 1)]),
            longitude_ref: Some("E".into()),
            longitude: Some(vec![r(151, 1), r(0, 0), r(0, 1)]),
            altitude: None,
        };
        // abs(degrees), lower-case reference; 0/0 is 0.
        assert_eq!(g.geo_location(), Some((-33.5, 151.0)));
        g.longitude_ref = None;
        assert_eq!(g.geo_location(), None);
        g.longitude_ref = Some("E".into());
        g.latitude = Some(vec![r(1, 1), r(2, 1)]);
        assert_eq!(g.geo_location(), None);
        assert!(r(5, 0).to_f64().is_infinite());
    }

    #[test]
    fn integer_altitude_and_single_rational_position() {
        let t = TiffBuilder::new(false)
            .gps_tag(TAG_GPS_ALTITUDE, (3, 1, 1234u16.to_le_bytes().to_vec()))
            .gps_tag(TAG_GPS_LATITUDE_REF, TiffBuilder::ascii("N"))
            .gps_tag(TAG_GPS_LATITUDE, (5, 1, [10, 0, 0, 0, 1, 0, 0, 0].to_vec()))
            .build();
        let d = read_tiff(&MemSource::new(t)).unwrap();
        let g = d.gps.unwrap();
        assert_eq!(g.altitude, Some(Rational { num: 1234, den: 1 }));
        // A single rational is not an array (getRationalArray → null).
        assert_eq!(g.latitude, None);
        assert!(d.ifd0.unwrap().make.is_none());
    }

    #[test]
    fn strings_stop_at_nul_and_decode_latin1() {
        let t = TiffBuilder::new(false)
            .make("NIKON\0garbage")
            .model("Caf\u{e9}")
            .build();
        let d = read_tiff(&MemSource::new(t)).unwrap();
        let ifd0 = d.ifd0.unwrap();
        assert_eq!(ifd0.make.as_deref(), Some("NIKON"));
        assert_eq!(ifd0.model.as_deref(), Some("Caf\u{e9}"));
        // Latin-1 bytes.
        let mut raw = TiffBuilder::new(false).make("Caf_").build();
        let pos = raw.windows(4).position(|w| w == b"Caf_").unwrap();
        raw[pos + 3] = 0xE9;
        let d = read_tiff(&MemSource::new(raw)).unwrap();
        assert_eq!(d.ifd0.unwrap().make.as_deref(), Some("Caf\u{e9}"));
    }

    #[test]
    fn corrupt_offsets_do_not_fail() {
        let mut t = sample(false).build();
        // Point the GPS IFD past the end and loop the Exif IFD to itself.
        let n = t.len();
        let gps_entry = t
            .windows(2)
            .position(|w| w == TAG_GPS_IFD.to_le_bytes())
            .unwrap();
        t[gps_entry + 8..gps_entry + 12].copy_from_slice(&(n as u32 + 100).to_le_bytes());
        let exif_entry = t
            .windows(2)
            .position(|w| w == TAG_EXIF_IFD.to_le_bytes())
            .unwrap();
        t[exif_entry + 8..exif_entry + 12].copy_from_slice(&8u32.to_le_bytes());
        let d = read_tiff(&MemSource::new(t)).unwrap();
        assert!(d.gps.is_none());
        assert!(d.sub_ifd.is_none());
        assert_eq!(d.ifd0.unwrap().make.as_deref(), Some("Canon"));
    }

    #[test]
    fn date_patterns() {
        let p = |s: &str| parse_exif_date(s).map(|(d, o)| (d.to_string(), o));
        assert_eq!(
            p("2021:03:04 10:20:30"),
            Some(("2021-03-04T10:20:30".into(), None))
        );
        assert_eq!(
            p("2021:03:04 10:20"),
            Some(("2021-03-04T10:20:00".into(), None))
        );
        assert_eq!(
            p("2021-03-04T10:20:30+02:00"),
            Some(("2021-03-04T10:20:30".into(), Some(7200)))
        );
        assert_eq!(
            p("2021:03:04 10:20:30.123-0130"),
            Some(("2021-03-04T10:20:30".into(), Some(-5400)))
        );
        assert_eq!(
            p("2021.03.04 10:20:30Z"),
            Some(("2021-03-04T10:20:30".into(), Some(0)))
        );
        assert_eq!(p("20210304"), Some(("2021-03-04T00:00:00".into(), None)));
        // Only the year parses: Java's last pattern "yyyy" wins.
        assert_eq!(p("2021:03:04"), Some(("2021-01-01T00:00:00".into(), None)));
        assert_eq!(p("0000:00:00 00:00:00"), None);
        assert_eq!(p("    :  :     :  :  "), None);
        assert_eq!(p(""), None);
        assert_eq!(p("2021:13:01 00:00:00"), None);
    }

    proptest::proptest! {
        #[test]
        fn never_panics(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            let _ = read_tiff(&MemSource::new(data.clone()));
            let mut j = vec![0xFF, 0xD8];
            j.extend(&data);
            let _ = read_jpeg(&MemSource::new(j));
        }

        #[test]
        fn mutated_tiff_never_panics(pos in 0usize..400, byte in proptest::prelude::any::<u8>()) {
            let mut t = sample(pos % 2 == 0).build();
            let i = pos % t.len();
            t[i] = byte;
            let _ = read_tiff(&MemSource::new(t));
        }

        #[test]
        fn date_parser_never_panics(s in "\\PC{0,40}") {
            let _ = parse_exif_date(&s);
        }
    }
}
