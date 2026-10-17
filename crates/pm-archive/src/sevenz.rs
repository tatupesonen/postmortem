//! 7z reader (`sevenz-rust2`).
//!
//! Blocks ("folders") whose coders include AES are reported as encrypted
//! unless a password is supplied; an encrypted header makes the whole
//! archive [`ArchiveError::Encrypted`]. Decoder dictionaries (LZMA, LZMA2)
//! and `PPMd` model sizes are checked against [`crate::Options::max_dictionary`]
//! before a block is decoded.

use std::io::Read;

use pm_core::io::Cursor;
use pm_core::{Bytes, ReadAt};
use sevenz_rust2::{Archive, ArchiveEntry, Block, BlockDecoder, EncoderMethod, Password};

use crate::driver::{CopyError, Driver};
use crate::io::DynSource;
use crate::{ArchiveError, Entry, MemberEnd};

fn nt_secs(t: sevenz_rust2::NtTime) -> Option<i64> {
    let ft: u64 = t.into();
    (ft != 0)
        .then_some(ft)
        .and_then(pm_core::time::from_filetime)
        .map(jiff::Timestamp::as_second)
}

fn to_entry(index: usize, f: &ArchiveEntry, encrypted: bool) -> Entry {
    Entry {
        index: index as u64,
        path: f.name.clone(),
        raw_path: None,
        is_dir: f.is_directory,
        size: Some(f.size),
        packed_size: (f.compressed_size > 0).then_some(f.compressed_size),
        encrypted: encrypted && !f.is_directory && f.has_stream,
        modified: f
            .has_last_modified_date
            .then(|| nt_secs(f.last_modified_date))
            .flatten(),
        accessed: f.has_access_date.then(|| nt_secs(f.access_date)).flatten(),
        created: f
            .has_creation_date
            .then(|| nt_secs(f.creation_date))
            .flatten(),
    }
}

/// Why a block cannot be decoded (`None`: it can).
fn block_problem(block: &Block, max_dict: u64, have_password: bool) -> Option<String> {
    for c in &block.coders {
        let id = c.encoder_method_id();
        let p = c.properties();
        if id == EncoderMethod::ID_AES256_SHA256 && !have_password {
            return Some("encrypted".into());
        }
        let dict = if id == EncoderMethod::ID_LZMA || id == EncoderMethod::ID_PPMD {
            p.get(1..5).map(|b| u64::from(b.u32_le(0).unwrap_or(0)))
        } else if id == EncoderMethod::ID_LZMA2 {
            p.first().map(|&b| {
                if b >= 40 {
                    u64::from(u32::MAX)
                } else {
                    (2 | u64::from(b & 1)) << (b / 2 + 11)
                }
            })
        } else {
            None
        };
        if let Some(dict) = dict
            && dict > max_dict
        {
            return Some(format!(
                "decoder dictionary of {dict} bytes exceeds the limit"
            ));
        }
    }
    None
}

pub(crate) fn extract(src: &dyn ReadAt, d: &mut Driver<'_>) -> Result<(), ArchiveError> {
    let password = d
        .opts
        .password
        .as_deref()
        .map_or_else(Password::empty, |p| {
            Password::new(&String::from_utf8_lossy(p))
        });
    let mut cur = Cursor::new(DynSource(src));
    let archive = match Archive::read(&mut cur, &password) {
        Ok(a) => a,
        Err(sevenz_rust2::Error::PasswordRequired | sevenz_rust2::Error::MaybeBadPassword(_)) => {
            return Err(ArchiveError::Encrypted);
        }
        Err(e) => return Err(ArchiveError::malformed("7z", e.to_string())),
    };
    if archive.files.len() as u64 > d.opts.max_members {
        return Err(ArchiveError::Bomb(crate::BombReason::TooManyMembers {
            limit: d.opts.max_members,
        }));
    }
    let have_password = !password.is_empty();
    let problems: Vec<Option<String>> = archive
        .blocks
        .iter()
        .map(|b| block_problem(b, d.opts.max_dictionary, have_password))
        .collect();
    for (i, f) in archive.files.iter().enumerate() {
        let enc = archive
            .stream_map
            .file_block_index
            .get(i)
            .copied()
            .flatten()
            .and_then(|b| problems.get(b))
            .is_some_and(|p| p.as_deref() == Some("encrypted"));
        d.check_declared(&to_entry(i, f, enc))?;
    }
    // Files of the blocks that cannot be decoded.
    let mut skipped: Vec<Vec<usize>> = vec![Vec::new(); problems.len()];
    for (i, b) in archive.stream_map.file_block_index.iter().enumerate() {
        if let Some(b) = *b
            && problems.get(b).is_some_and(Option::is_some)
            && let Some(list) = skipped.get_mut(b)
        {
            list.push(i);
        }
    }
    for (b, problem) in problems.iter().enumerate() {
        d.check_cancel()?;
        let first = archive
            .stream_map
            .block_first_file_index
            .get(b)
            .copied()
            .unwrap_or(0);
        if let Some(why) = problem {
            let encrypted = why == "encrypted";
            for &i in skipped.get(b).map(Vec::as_slice).unwrap_or_default() {
                if let Some(f) = archive.files.get(i) {
                    d.begin(&to_entry(i, f, encrypted))?;
                    d.end(&MemberEnd::Skipped(why.clone()))?;
                }
            }
            continue;
        }
        let mut stop: Option<ArchiveError> = None;
        let mut next_index = first;
        let dec = BlockDecoder::new(1, b, &archive, &password, &mut cur);
        let res = dec.for_each_entries(&mut |f: &ArchiveEntry, r: &mut dyn Read| {
            // The decoder walks the block's files in index order.
            let e = to_entry(next_index, f, false);
            next_index += 1;
            let step = (|| -> Result<(), ArchiveError> {
                let wanted = d.begin(&e)?;
                let end = if !wanted || !f.has_stream {
                    // Data must still be consumed in solid blocks.
                    let mut sink = std::io::sink();
                    match std::io::copy(r, &mut sink) {
                        Ok(_) => MemberEnd::Complete,
                        Err(err) => MemberEnd::Failed(err.to_string()),
                    }
                } else {
                    match d.copy(r, None) {
                        Ok(()) => MemberEnd::Complete,
                        Err(CopyError::Read(err)) => MemberEnd::Failed(err.to_string()),
                        Err(CopyError::Stop(err)) => return Err(err),
                    }
                };
                d.end(&end)
            })();
            match step {
                Ok(()) => Ok(true),
                Err(err) => {
                    stop = Some(err);
                    Ok(false)
                }
            }
        });
        if let Some(err) = stop {
            return Err(err);
        }
        if let Err(e) = res {
            d.end(&MemberEnd::Failed(e.to_string()))?;
            d.warn(format!("7z block {b}: {e}"));
        }
    }
    // Files without data (directories, empty files).
    for (i, f) in archive.files.iter().enumerate() {
        if archive
            .stream_map
            .file_block_index
            .get(i)
            .copied()
            .flatten()
            .is_none()
        {
            d.member(&to_entry(i, f, false), None, None)?;
        }
    }
    Ok(())
}
