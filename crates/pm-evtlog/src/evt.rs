//! Legacy `.evt` logs (`EVENTLOGRECORD`, Windows NT 4 – Server 2003).
//!
//! Logs are circular, so rather than following the header's offsets the
//! parser scans for record signatures (`LfLe` at offset 4) in bounded
//! windows and validates each candidate (lengths at both ends, sane
//! offsets and timestamps). This also recovers records left in slack after
//! the log wrapped.

use pm_core::ReadAt;

use crate::sid::format_sid;
use crate::{EventRecord, EvtLogError, LogFormat};

const SIGNATURE: &[u8; 4] = b"LfLe";
/// Fixed part of a record.
const FIXED: usize = 0x38;
/// Largest record accepted (Windows caps records well below this).
const MAX_RECORD: usize = 0x4_0000;
/// Bytes examined per window (records may straddle windows; see below).
const STEP: usize = 4 << 20;
/// 1990-01-01 … 2100-01-01, to reject garbage in slack.
const MIN_TIME: u32 = 631_152_000;
const MAX_TIME: u32 = 4_102_444_800;

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// A NUL-terminated UTF-16LE string at `o`, and the offset after it.
fn utf16z(b: &[u8], o: usize, end: usize) -> Option<(String, usize)> {
    let mut units = Vec::new();
    let mut p = o;
    while p + 2 <= end {
        let u = u16_at(b, p)?;
        p += 2;
        if u == 0 {
            return Some((String::from_utf16_lossy(&units), p));
        }
        units.push(u);
    }
    None
}

/// Parses one record at `off` of `b` (`None` if it is not a valid record).
fn record(b: &[u8], off: usize) -> Option<EventRecord> {
    let len = usize::try_from(u32_at(b, off)?).ok()?;
    if !(FIXED..=MAX_RECORD).contains(&len) || !len.is_multiple_of(4) {
        return None;
    }
    if b.get(off + 4..off + 8)? != SIGNATURE {
        return None;
    }
    let end = off.checked_add(len)?;
    if usize::try_from(u32_at(b, end - 4)?).ok()? != len {
        return None;
    }
    let time = u32_at(b, off + 12)?;
    if !(MIN_TIME..MAX_TIME).contains(&time) {
        return None;
    }
    let event_code = u32_at(b, off + 20)?;
    let num_strings = usize::from(u16_at(b, off + 26)?);
    let string_off = usize::try_from(u32_at(b, off + 36)?).ok()?;
    let sid_len = usize::try_from(u32_at(b, off + 40)?).ok()?;
    let sid_off = usize::try_from(u32_at(b, off + 44)?).ok()?;
    let (provider, after) = utf16z(b, off + FIXED, end)?;
    let (computer, _) = utf16z(b, after, end)?;
    let user_sid = (sid_len > 0 && sid_off + sid_len <= len)
        .then(|| {
            b.get(off + sid_off..off + sid_off + sid_len)
                .and_then(format_sid)
        })
        .flatten();
    let mut data = Vec::with_capacity(num_strings);
    if string_off >= FIXED && string_off < len {
        let mut p = off + string_off;
        for i in 1..=num_strings.min(256) {
            let Some((s, next)) = utf16z(b, p, end - 4) else {
                break;
            };
            data.push((i.to_string(), s));
            p = next;
        }
    }
    Some(EventRecord {
        format: LogFormat::Evt,
        channel: String::new(),
        provider,
        event_id: event_code & 0xFFFF,
        record_id: u64::from(u32_at(b, off + 8)?),
        time: i64::from(time),
        computer,
        user_sid,
        data,
        xml: None,
    })
}

/// Parses the records of an in-memory log.
pub fn parse_evt_bytes(b: &[u8], f: &mut dyn FnMut(EventRecord) -> bool) {
    let mut off = 0;
    while off + FIXED <= b.len() {
        match record(b, off) {
            Some(r) => {
                let len = u32_at(b, off)
                    .and_then(|l| usize::try_from(l).ok())
                    .unwrap_or(4);
                if !f(r) {
                    return;
                }
                off += len;
            }
            None => off += 4,
        }
    }
}

/// Parses a log of any size, reading it in bounded windows. Records are
/// emitted once each, in file order.
pub fn parse_evt(
    src: &dyn ReadAt,
    f: &mut dyn FnMut(EventRecord) -> bool,
) -> Result<(), EvtLogError> {
    let size = src.size();
    let mut start = 0u64;
    let mut buf = Vec::new();
    while start < size {
        // A window covers STEP bytes of record starts plus room for the
        // largest record that starts near its end.
        let want = usize::try_from((size - start).min((STEP + MAX_RECORD) as u64)).unwrap_or(0);
        buf.resize(want, 0);
        let got = src.read_at(start, &mut buf)?;
        buf.truncate(got);
        if got == 0 {
            break;
        }
        let limit = STEP.min(got);
        let mut off = 0;
        let mut stopped = false;
        while off < limit && off + FIXED <= buf.len() {
            match record(&buf, off) {
                Some(r) => {
                    let len = u32_at(&buf, off)
                        .and_then(|l| usize::try_from(l).ok())
                        .unwrap_or(4);
                    if !f(r) {
                        stopped = true;
                        break;
                    }
                    off += len;
                }
                None => off += 4,
            }
        }
        if stopped {
            break;
        }
        // Continue where record scanning stopped (it may be past STEP when
        // the last record straddled the boundary).
        start += off.max(4) as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use pm_testkit::evt::build;

    #[test]
    fn parses_records_and_skips_garbage() {
        let mut log = vec![0u8; 0x30];
        log[0..4].copy_from_slice(&0x30u32.to_le_bytes());
        log[4..8].copy_from_slice(SIGNATURE); // file header: too short to be a record
        let sid = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        log.extend(build(
            1,
            1_093_532_400,
            528,
            "Security",
            "N-1A9ODN6ZXK4LQ",
            &["Mr. Evil", "N-1A9ODN6ZXK4LQ", "(0x0,0x1)", "2"],
            Some(&sid),
        ));
        log.extend([0xAB; 12]); // junk
        log.extend(build(
            2,
            1_093_532_500,
            0xC000_0000 | 0x21A, // 538
            "Security",
            "PC",
            &["Mr. Evil"],
            None,
        ));
        let mut out = Vec::new();
        parse_evt_bytes(&log, &mut |r| {
            out.push(r);
            true
        });
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event_id, 528);
        assert_eq!(out[0].provider, "Security");
        assert_eq!(out[0].nth(1), Some("Mr. Evil"));
        assert_eq!(out[0].nth(4), Some("2"));
        assert_eq!(out[0].user_sid.as_deref(), Some("S-1-5-18"));
        assert_eq!(out[1].event_id, 538);
        assert_eq!(out[1].time, 1_093_532_500);
    }

    proptest::proptest! {
        #[test]
        fn never_panics_on_arbitrary_bytes(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            parse_evt_bytes(&data, &mut |_| true);
        }

        #[test]
        fn never_panics_on_mutated_records(flip in 0usize..400, byte in proptest::prelude::any::<u8>()) {
            let mut log = build(1, 1_093_532_400, 528, "Security", "PC", &["a", "b", "c"], None);
            log.extend(build(2, 1_093_532_401, 538, "Security", "PC", &["a"], None));
            let i = flip % log.len();
            log[i] = byte;
            parse_evt_bytes(&log, &mut |_| true);
        }
    }

    #[test]
    fn windowed_parse_matches_in_memory_parse() {
        let mut log = Vec::new();
        for i in 0..60_000u32 {
            log.extend(build(
                i,
                1_093_532_400 + i,
                528,
                "Security",
                "PC",
                &["user", "dom"],
                None,
            ));
        }
        let src = pm_core::io::MemSource::new(log.clone());
        let mut windowed = 0;
        parse_evt(&src, &mut |_| {
            windowed += 1;
            true
        })
        .unwrap();
        assert_eq!(windowed, 60_000);
        assert!(log.len() > STEP, "test must cross a window boundary");
    }
}
