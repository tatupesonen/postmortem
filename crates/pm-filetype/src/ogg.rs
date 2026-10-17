//! vorbis-java-tika 0.8 `OggDetector`: walks every Ogg page and packet
//! and names the stream types.

use std::collections::HashMap;

use pm_core::ReadAt;

use crate::Failure;
use crate::stream::SeqReader;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    General,
    Audio,
    Video,
    Metadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StreamType {
    mime: &'static str,
    kind: Kind,
}

const fn st(mime: &'static str, kind: Kind) -> StreamType {
    StreamType { mime, kind }
}

const UNKNOWN: StreamType = st("application/octet-stream", Kind::General);
const SKELETON: StreamType = st("application/annodex", Kind::Metadata);
const CMML: StreamType = st("text/x-cmml", Kind::Metadata);
const KATE: StreamType = st("application/kate", Kind::Metadata);
const VORBIS: StreamType = st("audio/vorbis", Kind::Audio);
const SPEEX: StreamType = st("audio/speex", Kind::Audio);
const OPUS: StreamType = st("audio/opus", Kind::Audio);
const FLAC: StreamType = st("audio/x-oggflac", Kind::Audio);
const PCM: StreamType = st("audio/x-oggpcm", Kind::Audio);
const THEORA: StreamType = st("video/theora", Kind::Video);
const DAALA: StreamType = st("video/daala", Kind::Video);
const DIRAC: StreamType = st("video/x-dirac", Kind::Video);
const OGM: StreamType = st("video/x-ogm", Kind::Video);
const UVS: StreamType = st("video/x-ogguvs", Kind::Video);
const YUV: StreamType = st("video/x-oggyuv", Kind::Video);
const RGB: StreamType = st("video/x-oggrgb", Kind::Video);

const OGG_GENERAL: &str = "application/ogg";
const OGG_AUDIO: &str = "audio/ogg";
const OGG_VIDEO: &str = "video/ogg";

/// A packet (`OggPacket`); `None` sid for continued data.
#[derive(Debug)]
struct Packet {
    sid: i32,
    data: Vec<u8>,
    bos: bool,
}

/// One page and the packets still to hand out (`OggPacketIterator`).
#[derive(Debug, Default)]
struct PageIter {
    sid: i32,
    bos: bool,
    lvs: Vec<u8>,
    data: Vec<u8>,
    current_lv: usize,
    current_offset: usize,
    prev: Option<Vec<u8>>,
}

enum Next {
    Packet(Packet),
    /// Data continuing on the next page.
    Leftover(Vec<u8>),
}

impl PageIter {
    fn has_next(&self) -> bool {
        self.current_lv < self.lvs.len() || (self.current_lv == 0 && self.lvs.is_empty())
    }

    fn next(&mut self) -> Next {
        let n = self.lvs.len();
        let mut continues = false;
        let mut packet_lvs = 0;
        let mut size = 0usize;
        for i in self.current_lv..n {
            let s = usize::from(self.lvs.get(i).copied().unwrap_or(0));
            size += s;
            packet_lvs += 1;
            if s < 255 {
                break;
            }
            if i == n - 1 && s == 255 {
                continues = true;
            }
        }
        let mut pd = self
            .data
            .get(self.current_offset..self.current_offset + size)
            .unwrap_or_default()
            .to_vec();
        if let Some(mut prev) = self.prev.take() {
            prev.append(&mut pd);
            pd = prev;
        }
        let out = if continues {
            Next::Leftover(pd)
        } else {
            Next::Packet(Packet {
                sid: self.sid,
                data: pd,
                bos: self.bos && self.current_lv == 0,
            })
        };
        self.current_lv += packet_lvs;
        self.current_offset += size;
        if self.current_lv == 0 {
            self.current_lv = 1;
        }
        out
    }
}

/// `OggPacketReader`.
struct PacketReader<'a> {
    r: SeqReader<'a>,
    it: Option<PageIter>,
}

impl PacketReader<'_> {
    fn next_packet(&mut self) -> Result<Option<Packet>, Failure> {
        let mut leftover = None;
        loop {
            if let Some(it) = self.it.as_mut().filter(|i| i.has_next()) {
                match it.next() {
                    Next::Packet(p) => return Ok(Some(p)),
                    Next::Leftover(d) => leftover = Some(d),
                }
            }
            // Find the next "OggS" within 64 KiB.
            let mut searched = 0usize;
            let mut pos: i32 = -1;
            let mut found = false;
            while searched < 65536 && !found {
                let Some(b) = self.r.byte() else {
                    return Ok(None);
                };
                pos = match (pos, b) {
                    (-1, b'O') => 0,
                    (-1, _) => -1,
                    (0, b'g') => 1,
                    (1, b'g') => 2,
                    (2, b'S') => {
                        found = true;
                        2
                    }
                    _ => -1,
                };
                if !found {
                    searched += 1;
                }
            }
            if !found {
                // IOException: "Next ogg packet header not found".
                return Err(Failure("ogg: no page header within 64 KiB".into()));
            }
            let mut hdr = [0u8; 23];
            if !self.r.read_exact(&mut hdr) {
                return Ok(None);
            }
            if hdr[0] != 0 {
                // IllegalArgumentException: unsupported version.
                return Err(Failure("ogg: page version".into()));
            }
            let flags = hdr[1];
            let sid = i32::from_le_bytes([hdr[10], hdr[11], hdr[12], hdr[13]]);
            let nlv = usize::from(hdr[22]);
            let mut lvs = vec![0u8; nlv];
            if !self.r.read_exact(&mut lvs) {
                return Ok(None);
            }
            let size: usize = lvs.iter().map(|&b| usize::from(b)).sum();
            let mut data = vec![0u8; size];
            if !self.r.read_exact(&mut data) {
                return Ok(None);
            }
            self.it = Some(PageIter {
                sid,
                bos: flags & 0x02 != 0,
                lvs,
                data,
                current_lv: 0,
                current_offset: 0,
                prev: leftover.take(),
            });
        }
    }
}

fn starts(d: &[u8], magic: &[u8]) -> bool {
    d.starts_with(magic)
}

/// `OggStreamIdentifier.identifyType`.
fn identify(d: &[u8]) -> StreamType {
    if d.len() <= 10 {
        return UNKNOWN;
    }
    let at = |i: usize| d.get(i).copied().unwrap_or(0);
    if d.len() >= 52 && (starts(d, b"fishead\0") || starts(d, b"fisbone\0")) {
        return SKELETON;
    }
    if starts(d, b"Annodex\0") {
        return SKELETON;
    }
    if starts(d, b"CMML\0\0\0\0") {
        return CMML;
    }
    if starts(d, b"\x80kate\0\0\0") {
        return KATE;
    }
    if d.len() >= 16 && matches!(at(0), 1 | 3 | 5) && d.get(1..7) == Some(b"vorbis") {
        return VORBIS;
    }
    if d.len() >= 72 && starts(d, b"Speex   ") {
        return SPEEX;
    }
    if d.len() >= 12 && (starts(d, b"OpusHead") || starts(d, b"OpusTags")) {
        return OPUS;
    }
    if at(0) == 0x7F && d.get(1..5) == Some(b"FLAC") {
        return FLAC;
    }
    if starts(d, b"PCM     ") {
        return PCM;
    }
    if d.len() >= 16 && matches!(at(0), 0x80..=0x82) && d.get(1..7) == Some(b"theora") {
        return THEORA;
    }
    if starts(d, b"\x80daala\0\0") {
        return DAALA;
    }
    if starts(d, b"BBCD") {
        return DIRAC;
    }
    if starts(d, b"video") {
        return OGM;
    }
    if starts(d, b"UVS ") {
        return UVS;
    }
    if starts(d, b"\x01YUV") {
        return YUV;
    }
    if starts(d, b"\x01GBP") {
        return RGB;
    }
    UNKNOWN
}

/// `SkeletonPacketFactory.create`: only the failures matter.
fn check_skeleton_packet(d: &[u8]) -> Result<(), Failure> {
    let bad = || Failure("ogg: bad skeleton packet".into());
    let le = |o: usize, n: usize| -> Result<u64, Failure> {
        let b = d.get(o..o + n).ok_or_else(bad)?;
        Ok(b.iter().rev().fold(0u64, |a, &x| (a << 8) | u64::from(x)))
    };
    if d.len() >= 52 && starts(d, b"fishead\0") {
        let major = le(8, 2)?;
        if !(3..=4).contains(&major) {
            return Err(bad());
        }
        le(36, 8)?;
        if !(d.get(44) == Some(&0) && d.get(45) == Some(&0)) && d.len() < 64 {
            return Err(bad());
        }
        if major == 4 {
            le(72, 8)?;
        }
    } else if d.len() >= 52 && starts(d, b"fisbone\0") {
        if le(8, 4)? != 44 {
            return Err(bad());
        }
        let headers = String::from_utf8_lossy(d.get(52..).unwrap_or_default());
        if !headers.contains("Content-Type") {
            return Err(bad());
        }
        for line in headers.split(['\r', '\n']).filter(|l| !l.is_empty()) {
            if !line.contains(": ") {
                return Err(bad());
            }
        }
    }
    Ok(())
}

/// Detects an Ogg file; `None` when it does not start with `OggS`.
pub(crate) fn detect(src: &dyn ReadAt) -> Result<Option<&'static str>, Failure> {
    let mut sig = [0u8; 4];
    if src.read_at(0, &mut sig).unwrap_or(0) < 4 || &sig != b"OggS" {
        return Ok(None);
    }
    let mut reader = PacketReader {
        r: SeqReader::new(src),
        it: None,
    };
    let mut total = 0usize;
    let mut streams: HashMap<StreamType, usize> = HashMap::new();
    let mut order: Vec<StreamType> = Vec::new();
    let mut skeleton_sids: Vec<i32> = Vec::new();
    while let Some(p) = reader.next_packet()? {
        if p.bos {
            total += 1;
            let t = identify(&p.data);
            if t == SKELETON {
                check_skeleton_packet(&p.data)?;
                skeleton_sids.push(p.sid);
            }
            let e = streams.entry(t).or_insert(0);
            if *e == 0 {
                order.push(t);
            }
            *e += 1;
        } else if skeleton_sids.contains(&p.sid) {
            check_skeleton_packet(&p.data)?;
        }
    }
    let to_mime = |t: StreamType| if t == UNKNOWN { OGG_GENERAL } else { t.mime };
    if total == 1 {
        return Ok(Some(order.first().map_or(OGG_GENERAL, |&t| to_mime(t))));
    }
    let count = |t: &StreamType| streams.get(t).copied().unwrap_or(0);
    let non_meta: usize = order
        .iter()
        .filter(|t| t.kind != Kind::Metadata)
        .map(count)
        .sum();
    if non_meta == 0 {
        return Ok(Some(OGG_GENERAL));
    }
    if non_meta == 1
        && let Some(&t) = order.iter().find(|t| t.kind != Kind::Metadata)
    {
        return Ok(Some(to_mime(t)));
    }
    let video: Vec<StreamType> = order
        .iter()
        .copied()
        .filter(|t| t.kind == Kind::Video)
        .collect();
    let audio: Vec<StreamType> = order
        .iter()
        .copied()
        .filter(|t| t.kind == Kind::Audio)
        .collect();
    let video_count: usize = video.iter().map(count).sum();
    let audio_count: usize = audio.iter().map(count).sum();
    let kate = order.contains(&KATE);
    if video_count == 1 {
        return Ok(Some(video.first().map_or(OGG_GENERAL, |&t| to_mime(t))));
    }
    if video_count == 0 && audio_count > 1 {
        return Ok(Some(if audio.len() == 1 {
            audio.first().map_or(OGG_AUDIO, |&t| to_mime(t))
        } else {
            OGG_AUDIO
        }));
    }
    if video_count > 1 {
        return Ok(Some(if video.len() == 1 {
            video.first().map_or(OGG_VIDEO, |&t| to_mime(t))
        } else {
            OGG_VIDEO
        }));
    }
    if kate {
        return Ok(Some(KATE.mime));
    }
    Ok(Some(OGG_GENERAL))
}

/// Builds an Ogg page (test fixtures).
#[cfg(test)]
pub(crate) fn page(sid: i32, bos: bool, packets: &[&[u8]]) -> Vec<u8> {
    let mut lvs = Vec::new();
    let mut data = Vec::new();
    for p in packets {
        let mut n = p.len();
        loop {
            let s = n.min(255);
            lvs.push(s as u8);
            n -= s;
            if s < 255 {
                break;
            }
        }
        data.extend_from_slice(p);
    }
    let mut out = b"OggS\0".to_vec();
    out.push(if bos { 2 } else { 0 });
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&sid.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out.push(lvs.len() as u8);
    out.extend_from_slice(&lvs);
    out.extend_from_slice(&data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_core::io::MemSource;

    fn vorbis_head() -> Vec<u8> {
        let mut v = b"\x01vorbis".to_vec();
        v.resize(30, 0);
        v
    }

    fn theora_head() -> Vec<u8> {
        let mut v = b"\x80theora".to_vec();
        v.resize(42, 0);
        v
    }

    #[test]
    fn single_and_multiplexed() {
        let mut f = page(1, true, &[&vorbis_head()]);
        f.extend(page(1, false, &[b"audio data"]));
        assert_eq!(
            detect(&MemSource::new(f.clone())).unwrap(),
            Some("audio/vorbis")
        );
        let mut v = page(1, true, &[&vorbis_head()]);
        v.extend(page(2, true, &[&theora_head()]));
        assert_eq!(detect(&MemSource::new(v)).unwrap(), Some("video/theora"));
        let mut two = page(1, true, &[&vorbis_head()]);
        two.extend(page(2, true, &[&vorbis_head()]));
        assert_eq!(detect(&MemSource::new(two)).unwrap(), Some("audio/vorbis"));
        assert_eq!(detect(&MemSource::new(b"not ogg".to_vec())).unwrap(), None);
    }

    #[test]
    fn failures() {
        // Version byte other than 0.
        let mut bad = page(1, true, &[&vorbis_head()]);
        bad[4] = 1;
        assert!(detect(&MemSource::new(bad)).is_err());
        // More than 64 KiB of junk before the next page.
        let mut junk = page(1, true, &[&vorbis_head()]);
        junk.extend(vec![b'x'; 70_000]);
        assert!(detect(&MemSource::new(junk)).is_err());
        // Truncated page: just stops.
        let mut cut = page(1, true, &[&vorbis_head()]);
        cut.truncate(20);
        assert_eq!(
            detect(&MemSource::new(cut)).unwrap(),
            Some("application/ogg")
        );
    }
}
