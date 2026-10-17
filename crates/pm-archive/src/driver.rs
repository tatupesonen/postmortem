//! Drives a [`Sink`]: counts members, meters decoded bytes and enforces the
//! zip-bomb rules and limits for every format reader.

use std::fmt;
use std::io::Read;

use crate::{ArchiveError, Entry, MemberEnd, Options, Sink};

/// Size of the chunks member data is copied in.
pub(crate) const CHUNK: usize = 64 * 1024;

/// Observes member data as it is copied (checksums).
pub(crate) type Verify<'v> = Option<&'v mut dyn FnMut(&[u8])>;

/// Why a container was judged a possible zip bomb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BombReason {
    /// Autopsy's rule: an item declares an uncompressed size of at least
    /// `ratio_min_size` with `size / packed ≥ max_ratio`.
    DeclaredRatio { path: String, ratio: u64 },
    /// The bytes decoded for one member reached the minimum size with a
    /// ratio above the limit.
    MemberRatio { path: String, ratio: u64 },
    /// All bytes decoded from the container, relative to its size.
    ContainerRatio { ratio: u64 },
    /// More members than [`Options::max_members`].
    TooManyMembers { limit: u64 },
}

impl BombReason {
    /// The compression ratio that triggered, if the rule is a ratio.
    pub fn ratio(&self) -> Option<u64> {
        match self {
            Self::DeclaredRatio { ratio, .. }
            | Self::MemberRatio { ratio, .. }
            | Self::ContainerRatio { ratio } => Some(*ratio),
            Self::TooManyMembers { .. } => None,
        }
    }

    /// The member that triggered, if any.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::DeclaredRatio { path, .. } | Self::MemberRatio { path, .. } => Some(path),
            _ => None,
        }
    }
}

impl fmt::Display for BombReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeclaredRatio { path, ratio } => {
                write!(f, "item {path} declares a compression ratio of {ratio}")
            }
            Self::MemberRatio { path, ratio } => {
                write!(f, "item {path} decoded with a compression ratio of {ratio}")
            }
            Self::ContainerRatio { ratio } => {
                write!(f, "the archive decoded with a compression ratio of {ratio}")
            }
            Self::TooManyMembers { limit } => write!(f, "more than {limit} members"),
        }
    }
}

/// Outcome of a completed extraction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Members announced (directories included).
    pub members: u64,
    /// Members whose data was delivered completely.
    pub complete: u64,
    /// Encrypted members (not decoded).
    pub encrypted: u64,
    /// Members that failed to decode or were skipped.
    pub failed: u64,
    /// Bytes delivered to the sink.
    pub decoded_bytes: u64,
    /// Non-fatal problems (logged by the caller).
    pub warnings: Vec<String>,
}

/// State of the member being delivered.
#[derive(Debug, Default)]
struct Current {
    path: String,
    packed: Option<u64>,
    decoded: u64,
    wanted: bool,
    open: bool,
}

/// Wraps the caller's sink for one extraction.
pub(crate) struct Driver<'a> {
    pub(crate) opts: &'a Options,
    sink: &'a mut dyn Sink,
    container_size: u64,
    cur: Current,
    summary: Summary,
}

impl fmt::Debug for Driver<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Driver")
            .field("container_size", &self.container_size)
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

/// `a / b` rounded down, or `None` when `b` is zero.
fn ratio(a: u64, b: u64) -> Option<u64> {
    a.checked_div(b)
}

impl<'a> Driver<'a> {
    pub(crate) fn new(container_size: u64, opts: &'a Options, sink: &'a mut dyn Sink) -> Self {
        Self {
            opts,
            sink,
            container_size,
            cur: Current::default(),
            summary: Summary::default(),
        }
    }

    pub(crate) fn finish(self) -> Summary {
        self.summary
    }

    pub(crate) fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::debug!("{msg}");
        if self.summary.warnings.len() < 100 {
            self.summary.warnings.push(msg);
        }
    }

    pub(crate) fn check_cancel(&self) -> Result<(), ArchiveError> {
        if self.sink.is_cancelled() {
            Err(ArchiveError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Autopsy's declared-size rule for one item (run before any data is
    /// decoded, for formats with an index).
    pub(crate) fn check_declared(&self, e: &Entry) -> Result<(), ArchiveError> {
        if !self.opts.check_declared || e.is_dir {
            return Ok(());
        }
        let (Some(size), Some(packed)) = (e.size, e.packed_size) else {
            return Ok(());
        };
        if size < self.opts.ratio_min_size || packed == 0 {
            return Ok(());
        }
        match ratio(size, packed) {
            Some(r) if r >= self.opts.max_ratio => {
                Err(ArchiveError::Bomb(BombReason::DeclaredRatio {
                    path: e.path.clone(),
                    ratio: r,
                }))
            }
            _ => Ok(()),
        }
    }

    /// Announces a member. Returns whether its data is wanted.
    pub(crate) fn begin(&mut self, e: &Entry) -> Result<bool, ArchiveError> {
        self.check_cancel()?;
        if self.cur.open {
            self.end(&MemberEnd::Failed("member not finished".into()))?;
        }
        if self.summary.members >= self.opts.max_members {
            return Err(ArchiveError::Bomb(BombReason::TooManyMembers {
                limit: self.opts.max_members,
            }));
        }
        self.check_declared(e)?;
        self.summary.members += 1;
        if e.encrypted {
            self.summary.encrypted += 1;
        }
        let wanted = self.sink.begin(e).map_err(ArchiveError::Sink)?;
        self.cur = Current {
            path: e.path.clone(),
            packed: e.packed_size,
            decoded: 0,
            wanted: wanted && !e.is_dir && !e.encrypted,
            open: true,
        };
        Ok(self.cur.wanted)
    }

    /// Delivers decoded bytes of the current member, metering them.
    pub(crate) fn write(&mut self, data: &[u8]) -> Result<(), ArchiveError> {
        if data.is_empty() {
            return Ok(());
        }
        self.check_cancel()?;
        let n = data.len() as u64;
        self.cur.decoded = self.cur.decoded.saturating_add(n);
        self.summary.decoded_bytes = self.summary.decoded_bytes.saturating_add(n);
        self.meter()?;
        if self.cur.wanted {
            self.sink.write(data).map_err(ArchiveError::Sink)?;
        }
        Ok(())
    }

    fn meter(&self) -> Result<(), ArchiveError> {
        let o = self.opts;
        if self.cur.decoded >= o.ratio_min_size
            && let Some(r) = self.cur.packed.and_then(|p| ratio(self.cur.decoded, p))
            && r >= o.max_ratio
        {
            return Err(ArchiveError::Bomb(BombReason::MemberRatio {
                path: self.cur.path.clone(),
                ratio: r,
            }));
        }
        let total = self.summary.decoded_bytes;
        if total >= o.ratio_min_size
            && let Some(r) = ratio(total, self.container_size.max(1))
            && r >= o.max_ratio
        {
            return Err(ArchiveError::Bomb(BombReason::ContainerRatio { ratio: r }));
        }
        Ok(())
    }

    /// Ends the current member.
    pub(crate) fn end(&mut self, end: &MemberEnd) -> Result<(), ArchiveError> {
        if !self.cur.open {
            return Ok(());
        }
        self.cur.open = false;
        match end {
            MemberEnd::Complete | MemberEnd::ChecksumMismatch => {
                if self.cur.wanted || self.cur.decoded > 0 {
                    self.summary.complete += 1;
                }
            }
            MemberEnd::Failed(_) | MemberEnd::Skipped(_) => self.summary.failed += 1,
        }
        if let MemberEnd::ChecksumMismatch = end {
            let msg = format!("{}: checksum mismatch", self.cur.path);
            self.warn(msg);
        }
        if let MemberEnd::Failed(why) = end {
            let msg = format!("{}: {why}", self.cur.path);
            self.warn(msg);
        }
        self.sink.end(end).map_err(ArchiveError::Sink)
    }

    /// Announces a member, copies `reader` into it and ends it. Decode
    /// errors end the member as failed; limit, cancellation and sink errors
    /// propagate.
    pub(crate) fn member(
        &mut self,
        e: &Entry,
        reader: Option<&mut dyn Read>,
        verify: Verify<'_>,
    ) -> Result<MemberEnd, ArchiveError> {
        let wanted = self.begin(e)?;
        let end = match reader {
            _ if e.encrypted => MemberEnd::Skipped("encrypted".into()),
            Some(r) if wanted => match self.copy(r, verify) {
                Ok(()) => MemberEnd::Complete,
                Err(CopyError::Read(err)) => MemberEnd::Failed(err.to_string()),
                Err(CopyError::Stop(err)) => return Err(err),
            },
            _ => MemberEnd::Complete,
        };
        self.end(&end)?;
        Ok(end)
    }

    /// Copies a decoder's output into the current member.
    pub(crate) fn copy(
        &mut self,
        r: &mut dyn Read,
        mut verify: Verify<'_>,
    ) -> Result<(), CopyError> {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // Decoders report our own stop requests as I/O errors
                    // when they come through a callback; unwrap them.
                    return Err(CopyError::Read(e));
                }
            };
            let chunk = buf.get(..n).unwrap_or_default();
            if let Some(v) = verify.as_mut() {
                v(chunk);
            }
            self.write(chunk).map_err(CopyError::Stop)?;
        }
    }
}

/// Failure while copying member data.
#[derive(Debug)]
pub(crate) enum CopyError {
    /// The decoder failed (corrupt data): the member fails, the walk goes on.
    Read(std::io::Error),
    /// A limit, cancellation or sink error: the walk stops.
    Stop(ArchiveError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec {
        bytes: u64,
        ends: Vec<MemberEnd>,
    }

    impl Sink for Rec {
        fn begin(&mut self, _: &Entry) -> std::io::Result<bool> {
            Ok(true)
        }
        fn write(&mut self, d: &[u8]) -> std::io::Result<()> {
            self.bytes += d.len() as u64;
            Ok(())
        }
        fn end(&mut self, e: &MemberEnd) -> std::io::Result<()> {
            self.ends.push(e.clone());
            Ok(())
        }
    }

    fn small() -> Options {
        Options {
            ratio_min_size: 1000,
            max_ratio: 10,
            ..Options::default()
        }
    }

    #[test]
    fn declared_ratio_follows_autopsy() {
        let o = small();
        let mut rec = Rec::default();
        let mut d = Driver::new(1_000_000, &o, &mut rec);
        let mut e = Entry {
            path: "big".into(),
            size: Some(1000),
            packed_size: Some(100),
            ..Entry::default()
        };
        assert!(matches!(
            d.begin(&e),
            Err(ArchiveError::Bomb(BombReason::DeclaredRatio {
                ratio: 10,
                ..
            }))
        ));
        e.size = Some(999);
        assert!(d.begin(&e).is_ok());
        e.size = Some(1000);
        e.packed_size = Some(0);
        assert!(d.begin(&e).is_ok(), "unknown packed size is not checked");
        let o2 = Options {
            check_declared: false,
            ..small()
        };
        let mut rec2 = Rec::default();
        let mut d2 = Driver::new(1_000_000, &o2, &mut rec2);
        e.packed_size = Some(1);
        assert!(d2.begin(&e).is_ok());
    }

    #[test]
    fn decoded_bytes_are_metered_per_member_and_container() {
        let o = small();
        let mut rec = Rec::default();
        let mut d = Driver::new(1_000_000, &o, &mut rec);
        let e = Entry {
            path: "liar".into(),
            size: Some(1),
            packed_size: Some(50),
            ..Entry::default()
        };
        assert!(d.begin(&e).unwrap());
        d.write(&[0; 999]).unwrap();
        let err = d.write(&[0; 1]).unwrap_err();
        assert!(matches!(
            err,
            ArchiveError::Bomb(BombReason::MemberRatio { ratio: 20, .. })
        ));

        // Container: many members, each fine on its own.
        let mut rec = Rec::default();
        let mut d = Driver::new(10, &o, &mut rec);
        let mut stopped = None;
        for i in 0..100 {
            let e = Entry {
                path: format!("m{i}"),
                packed_size: Some(10),
                ..Entry::default()
            };
            d.begin(&e).unwrap();
            if let Err(err) = d.write(&[0; 50]) {
                stopped = Some(err);
                break;
            }
            d.end(&MemberEnd::Complete).unwrap();
        }
        assert!(matches!(
            stopped,
            Some(ArchiveError::Bomb(BombReason::ContainerRatio {
                ratio: 100
            }))
        ));
    }

    #[test]
    fn member_cap() {
        let o = Options {
            max_members: 2,
            ..Options::default()
        };
        let mut rec = Rec::default();
        let mut d = Driver::new(10, &o, &mut rec);
        let e = Entry::default();
        d.member(&e, None, None).unwrap();
        d.member(&e, None, None).unwrap();
        assert!(matches!(
            d.member(&e, None, None),
            Err(ArchiveError::Bomb(BombReason::TooManyMembers { limit: 2 }))
        ));
    }

    #[test]
    fn read_errors_fail_the_member_only() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("corrupt"))
            }
        }
        let o = Options::default();
        let mut rec = Rec::default();
        let mut d = Driver::new(10, &o, &mut rec);
        let end = d
            .member(&Entry::default(), Some(&mut Broken), None)
            .unwrap();
        assert!(matches!(end, MemberEnd::Failed(_)));
        let s = d.finish();
        assert_eq!(s.failed, 1);
        assert_eq!(s.warnings.len(), 1);
    }
}
