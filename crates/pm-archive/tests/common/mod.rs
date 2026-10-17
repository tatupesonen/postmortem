//! Shared helpers: a sink that collects every member.
#![allow(dead_code, unreachable_pub, reason = "shared by several test crates")]

use pm_archive::{ArchiveError, Entry, Format, MemberEnd, Options, Sink, Summary};
use pm_core::io::MemSource;

/// A collected member.
#[derive(Debug, Clone)]
pub struct Member {
    pub entry: Entry,
    pub data: Vec<u8>,
    pub end: MemberEnd,
}

#[derive(Debug, Default)]
pub struct Collect {
    pub members: Vec<Member>,
    cur: Option<(Entry, Vec<u8>)>,
}

impl Sink for Collect {
    fn begin(&mut self, entry: &Entry) -> std::io::Result<bool> {
        self.cur = Some((entry.clone(), Vec::new()));
        Ok(true)
    }
    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        if let Some(c) = self.cur.as_mut() {
            c.1.extend_from_slice(data);
        }
        Ok(())
    }
    fn end(&mut self, end: &MemberEnd) -> std::io::Result<()> {
        if let Some((entry, data)) = self.cur.take() {
            self.members.push(Member {
                entry,
                data,
                end: end.clone(),
            });
        }
        Ok(())
    }
}

/// Extracts `data` as `format` with `opts`.
pub fn run_with(
    format: Format,
    data: &[u8],
    opts: &Options,
) -> (Result<Summary, ArchiveError>, Vec<Member>) {
    let src = MemSource::new(data.to_vec());
    let mut c = Collect::default();
    let r = pm_archive::extract(format, &src, opts, &mut c);
    (r, c.members)
}

/// Extracts with default options and expects success.
pub fn run(format: Format, data: &[u8]) -> (Summary, Vec<Member>) {
    let (r, m) = run_with(format, data, &Options::default());
    (r.expect("extraction succeeds"), m)
}

/// The member at `path`.
pub fn member<'a>(ms: &'a [Member], path: &str) -> &'a Member {
    ms.iter()
        .find(|m| m.entry.path == path)
        .unwrap_or_else(|| panic!("no member {path}; have {:?}", paths(ms)))
}

pub fn paths(ms: &[Member]) -> Vec<String> {
    ms.iter().map(|m| m.entry.path.clone()).collect()
}

/// Format detected from content.
pub fn detect(data: &[u8]) -> Option<Format> {
    pm_archive::detect(&MemSource::new(data.to_vec()))
}
