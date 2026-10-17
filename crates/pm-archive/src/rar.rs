//! RAR reader (`rars`: RAR 1.3 through RAR 7, pure Rust, MIT/Apache).
//!
//! `rars` parses from memory or from a path, so archives above
//! [`crate::Options::max_in_memory`] are spooled to a temporary file. Its
//! extraction API pushes member data into writers it owns; extraction runs
//! on a scoped thread that streams chunks through a bounded channel, so the
//! sink still sees one member at a time and back-pressure keeps memory
//! bounded. Stopping (zip-bomb meter, cancellation) drops the channel,
//! which aborts the decoder.
//!
//! Encrypted members cannot be skipped by `rars`' streaming extractor:
//! without a password, members before the first encrypted one are
//! delivered, encrypted members are reported as encrypted, and the
//! remaining unencrypted members are reported as failed.

use std::io::{self, Write};

use crossbeam_channel::{Receiver, Sender, bounded};
use pm_core::ReadAt;
use rars::{ArchiveReadOptions, ArchiveReader, ExtractedEntryMeta};

use crate::driver::{CHUNK, Driver};
use crate::io::{read_all_bounded, spool};
use crate::names::decode_default;
use crate::{ArchiveError, Entry, MemberEnd};

enum Msg {
    Begin(ExtractedEntryMeta),
    Data(Vec<u8>),
    End,
}

/// The writer handed to `rars` for one member.
struct ChanWriter {
    tx: Sender<Msg>,
    buf: Vec<u8>,
}

impl ChanWriter {
    fn flush_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let data = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        self.tx
            .send(Msg::Data(data))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "extraction stopped"))
    }
}

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.flush_buf()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ChanWriter {
    fn drop(&mut self) {
        let _ = self.flush_buf();
        let _ = self.tx.send(Msg::End);
    }
}

fn dos_secs(t: u32) -> Option<i64> {
    let dt = pm_core::time::dos_datetime((t >> 16) as u16, (t & 0xFFFF) as u16, 0)?;
    dt.to_zoned(jiff::tz::TimeZone::UTC)
        .ok()
        .map(|z| z.timestamp().as_second())
}

fn name_entry(index: u64, name: &[u8], is_dir: bool) -> Entry {
    Entry {
        index,
        path: decode_default(name),
        raw_path: std::str::from_utf8(name).is_err().then(|| name.to_vec()),
        is_dir,
        ..Entry::default()
    }
}

pub(crate) fn extract(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let password = d.opts.password.clone();
    let mut options = match password.as_deref() {
        Some(p) => ArchiveReadOptions::with_password(p),
        None => ArchiveReadOptions::new(),
    };
    options = options.with_rar50_buffered_decode_limit(d.opts.max_in_memory);
    let mut _spooled = None;
    let parsed = match read_all_bounded(src, d.opts.max_in_memory)? {
        Some(bytes) => ArchiveReader::read_owned_with_options(bytes, options),
        None => {
            let tmp = spool(src, d.opts.temp_dir.as_deref(), &|| false)?;
            let r = ArchiveReader::read_path_with_options(tmp.path(), options);
            _spooled = Some(tmp);
            r
        }
    };
    let archive = match parsed {
        Ok(a) => a,
        Err(rars::Error::NeedPassword | rars::Error::WrongPasswordOrCorruptData) => {
            return Err(ArchiveError::Encrypted);
        }
        Err(e) => return Err(ArchiveError::malformed("RAR", e.to_string())),
    };

    // Index: declared sizes, encryption and dictionary limits.
    let mut entries = Vec::new();
    for (i, m) in archive.members().enumerate() {
        if i as u64 >= d.opts.max_members {
            return Err(ArchiveError::Bomb(crate::BombReason::TooManyMembers {
                limit: d.opts.max_members,
            }));
        }
        let meta = &m.meta;
        let mut e = name_entry(i as u64, &meta.name, meta.is_directory);
        e.size = Some(meta.unpacked_size);
        e.packed_size = Some(meta.packed_size);
        e.encrypted = meta.is_encrypted && password.is_none() && !meta.is_directory;
        e.modified = meta.file_time.and_then(dos_secs);
        d.check_declared(&e)?;
        entries.push(e);
    }
    if let Some(r5) = archive.as_rar50() {
        for f in r5.files() {
            if f.is_stored() {
                continue;
            }
            if let Ok(ci) = f.decoded_compression_info() {
                let need = ci.dictionary_size.min(f.unpacked_size.max(1));
                if need > d.opts.max_dictionary {
                    return Err(ArchiveError::Limit(format!(
                        "RAR dictionary of {} bytes exceeds the limit",
                        ci.dictionary_size
                    )));
                }
            }
        }
    }

    let (tx, rx) = bounded::<Msg>(8);
    let outcome = std::thread::scope(|s| {
        let producer = s.spawn(move || {
            archive.extract_to_with_options(options, |meta| {
                tx.send(Msg::Begin(meta.clone()))
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "extraction stopped"))?;
                Ok(Box::new(ChanWriter {
                    tx: tx.clone(),
                    buf: Vec::with_capacity(CHUNK),
                }) as Box<dyn Write>)
            })
        });
        let consumed = consume(rx, d, &entries);
        let produced = producer.join();
        (consumed, produced)
    });
    let (consumed, produced) = outcome;
    let delivered = consumed?;
    let error = match produced {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("RAR decoder panicked".to_owned()),
    };
    if let Some(why) = error {
        // Members not delivered: encrypted ones are reported as such, the
        // rest as failed.
        for e in entries.iter().skip(delivered) {
            d.begin(e)?;
            let end = if e.encrypted {
                MemberEnd::Skipped("encrypted".into())
            } else if e.is_dir {
                MemberEnd::Complete
            } else {
                MemberEnd::Failed(why.clone())
            };
            d.end(&end)?;
        }
        if delivered == 0 && entries.iter().all(|e| !e.encrypted) {
            d.warn(format!("RAR: {why}"));
        }
    }
    Ok(())
}

/// Drives the sink from the producer's messages; returns how many members
/// were completed.
fn consume(
    rx: Receiver<Msg>,
    d: &mut Driver<'_>,
    entries: &[Entry],
) -> Result<usize, ArchiveError> {
    let mut started = 0usize;
    let mut done = 0usize;
    let mut open = false;
    let mut encrypted = false;
    let ended = |encrypted: bool| {
        if encrypted {
            MemberEnd::Skipped("encrypted".into())
        } else {
            MemberEnd::Complete
        }
    };
    for msg in rx {
        match msg {
            Msg::Begin(meta) => {
                if open {
                    d.end(&ended(encrypted))?;
                    done += 1;
                }
                // Members arrive in archive order; fall back to the
                // decoder's metadata if the index disagrees.
                let e = entries
                    .get(started)
                    .filter(|e| e.path == decode_default(&meta.name))
                    .cloned()
                    .unwrap_or_else(|| name_entry(started as u64, &meta.name, meta.is_directory));
                started += 1;
                encrypted = e.encrypted;
                d.begin(&e)?;
                open = true;
            }
            Msg::Data(data) => {
                if open && !encrypted {
                    d.write(&data)?;
                }
            }
            Msg::End => {
                if open {
                    d.end(&ended(encrypted))?;
                    done += 1;
                    open = false;
                }
            }
        }
    }
    if open {
        // The decoder failed in the middle of this member.
        d.end(&MemberEnd::Failed("decoding stopped".into()))?;
        done += 1;
    }
    Ok(done)
}
