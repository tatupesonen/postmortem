//! Archive unpacking (Autopsy `SevenZipExtractor.unpack`).

use std::collections::HashSet;
use std::io;
use std::sync::Arc;

use pm_archive::{ArchiveError, BombReason, Entry, Format, MemberEnd, Options, Sink};
use pm_case::derived::EncodedWriter;
use pm_casedb::Significance;
use pm_ingest::{IngestFile, IngestMessage, JobContext, MessageKind, ModuleError};

use super::{ENCRYPTION_FILE_LEVEL, ENCRYPTION_FULL, Job, MAX_DEPTH, NAME, Nesting, OUTPUT_FOLDER};
use crate::derive::{DerivedTree, Existing, NodeId, NodeKind, Times, escape_file_name, split_path};
use crate::util::Art;

/// Bytes of each member kept to recognise nested archives.
const SNIFF: usize = 4096;

/// The member being written.
struct Current {
    node: NodeId,
    rel: String,
    writer: EncodedWriter,
    times: Times,
    head: Vec<u8>,
}

/// Receives the members of one archive into a [`DerivedTree`].
struct TreeSink<'a> {
    ctx: &'a JobContext,
    archive_name: String,
    tree: DerivedTree,
    cur: Option<Current>,
    /// Nodes whose content looks like an archive (Autopsy sniffs the
    /// first chunk with Tika for its depth map).
    nested: Vec<NodeId>,
    has_encrypted: bool,
    /// Every item is encrypted (directories count as not encrypted, as in
    /// Autopsy).
    full_encryption: bool,
}

fn times(e: &Entry) -> Times {
    Times {
        modified: e.modified,
        accessed: e.accessed,
        created: e.created,
    }
}

impl TreeSink<'_> {
    /// Drops a member that was not finished (the extraction stopped).
    fn abandon(&mut self) {
        if let Some(c) = self.cur.take() {
            drop(c.writer);
            let _ = std::fs::remove_file(self.tree.output().absolute(&c.rel));
            self.tree.set(c.node, NodeKind::NoContent, c.times);
        }
    }
}

impl Sink for TreeSink<'_> {
    fn begin(&mut self, e: &Entry) -> io::Result<bool> {
        self.abandon();
        let path = if e.path.is_empty() {
            pm_archive::default_member_name(&self.archive_name, e.index)
        } else {
            e.path.clone()
        };
        let mut parts = split_path(&path, e.raw_path.as_deref());
        if parts.is_empty() {
            parts = split_path(
                &pm_archive::default_member_name(&self.archive_name, e.index),
                None,
            );
        }
        let node = self.tree.path(parts);
        if e.is_dir {
            self.full_encryption = false;
            self.tree.set(node, NodeKind::Dir, times(e));
            return Ok(false);
        }
        if e.encrypted {
            self.has_encrypted = true;
            tracing::debug!("skipping encrypted file in archive: {path}");
            self.tree.set(node, NodeKind::NoContent, times(e));
            return Ok(false);
        }
        self.full_encryption = false;
        // Autopsy's local layout: 1000 files per sub-folder.
        let local = format!("{}/{}", e.index / 1000, e.index);
        let (rel, writer) = self.tree.create(&local)?;
        self.cur = Some(Current {
            node,
            rel,
            writer,
            times: times(e),
            head: Vec::new(),
        });
        Ok(true)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some(c) = self.cur.as_mut() {
            if c.head.len() < SNIFF {
                let n = (SNIFF - c.head.len()).min(data.len());
                c.head.extend_from_slice(data.get(..n).unwrap_or_default());
            }
            io::Write::write_all(&mut c.writer, data)?;
        }
        Ok(())
    }

    fn end(&mut self, end: &MemberEnd) -> io::Result<()> {
        let Some(c) = self.cur.take() else {
            return Ok(());
        };
        if !matches!(end, MemberEnd::Complete) {
            tracing::debug!(
                "extraction of {} in {}: {end:?}",
                self.tree.name(c.node),
                self.archive_name
            );
        }
        let size = c.writer.finish()?;
        if pm_archive::detect_head(&c.head).is_some_and(Format::is_archive) {
            self.nested.push(c.node);
        }
        self.tree.set(
            c.node,
            NodeKind::File {
                size,
                local_path: c.rel,
            },
            c.times,
        );
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }
}

/// Unpacks one archive into derived files.
pub(super) fn unpack(
    job: &Job,
    ctx: &JobContext,
    file: &IngestFile,
    format: Format,
) -> Result<(), ModuleError> {
    let nesting = job.take_nesting(file);
    if job.is_flagged(nesting.root.id) {
        // The root archive is already a possible zip bomb.
        return Ok(());
    }
    if nesting.depth == MAX_DEPTH {
        let details = format!(
            "The archive is {} levels deep, skipping processing of {}",
            nesting.depth,
            escape_file_name(&nesting.root.path)
        );
        job.flag_zip_bomb(ctx, &nesting.root, file, &details);
        return Ok(());
    }
    if ctx.is_cancelled() {
        return Ok(());
    }
    let existing = Existing::load(&ctx.case, OUTPUT_FOLDER, file.rec.id)?;
    let src = file.content()?;
    let opts = Options {
        // Autopsy skips the ratio check for deleted archives (damaged
        // headers give absurd ratios) and for `application/x-gzip`.
        check_declared: file.rec.meta_alloc
            && !file
                .mime()
                .is_some_and(|m| m.eq_ignore_ascii_case("application/x-gzip")),
        temp_dir: Some(ctx.case.dir().join("Temp").join(OUTPUT_FOLDER)),
        ..Options::default()
    };
    let mut sink = TreeSink {
        ctx,
        archive_name: file.rec.name.clone(),
        tree: DerivedTree::new(ctx.case.dir(), OUTPUT_FOLDER, &file.rec),
        cur: None,
        nested: Vec::new(),
        has_encrypted: false,
        full_encryption: true,
    };
    let result = pm_archive::extract(format, &*src, &opts, &mut sink);
    sink.abandon();
    let mut header_encrypted = false;
    match result {
        Ok(summary) => {
            for w in &summary.warnings {
                tracing::debug!("{}: {w}", file.rec.path());
            }
        }
        Err(ArchiveError::Cancelled) => return Ok(()),
        Err(ArchiveError::Bomb(reason)) => {
            let details = bomb_details(&reason, &nesting.root.path);
            job.flag_zip_bomb(ctx, &nesting.root, file, &details);
        }
        Err(ArchiveError::Encrypted) => header_encrypted = true,
        Err(e) => {
            tracing::warn!("error unpacking {}: {e}", file.rec.path());
            if file.rec.meta_alloc {
                ctx.post_message(
                    IngestMessage::new(
                        MessageKind::Error,
                        NAME,
                        format!("Error unpacking {}", file.rec.name),
                    )
                    .details(format!(
                        "Error unpacking  {}. {e}",
                        escape_file_name(&file.rec.path())
                    ))
                    .file(file.rec.id),
                );
            }
        }
    }
    if ctx.is_cancelled() {
        return Ok(());
    }
    let TreeSink {
        tree,
        nested,
        has_encrypted,
        full_encryption,
        ..
    } = sink;
    if !tree.is_empty() {
        let child = Nesting {
            depth: nesting.depth + 1,
            root: Arc::clone(&nesting.root),
        };
        let nested: HashSet<NodeId> = nested.into_iter().collect();
        let stats = tree.commit(ctx, &existing, &mut |node, id| {
            if nested.contains(&node) {
                job.add_nested(id, child.clone());
            }
        })?;
        tracing::debug!("{}: {stats:?}", file.rec.path());
    }
    if has_encrypted || header_encrypted {
        let kind = if header_encrypted || full_encryption {
            ENCRYPTION_FULL
        } else {
            ENCRYPTION_FILE_LEVEL
        };
        Art::result(
            ctx,
            NAME,
            "TSK_ENCRYPTION_DETECTED",
            file.rec.id,
            Significance::Notable,
        )
        .justification(kind)
        .text("TSK_COMMENT", kind)
        .post(ctx, &["TSK_COMMENT"]);
        ctx.post_message(
            IngestMessage::new(
                MessageKind::Warning,
                NAME,
                "Encrypted files in archive detected.",
            )
            .details(format!(
                "Some files in archive: {} are encrypted. {NAME} extractor was unable to extract all files from this archive.",
                file.rec.name
            ))
            .file(file.rec.id),
        );
    }
    Ok(())
}

/// Autopsy's comment for a ratio bomb (`isZipBombCheck.warnDetails`), and
/// postmortem's for the member cap.
fn bomb_details(reason: &BombReason, root_path: &str) -> String {
    let root = escape_file_name(root_path);
    match reason.ratio() {
        Some(r) => format!("Compression ratio is {r}, skipping items in {root}."),
        None => format!("The archive has {reason}, skipping items in {root}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bomb_texts() {
        let r = BombReason::DeclaredRatio {
            path: "x".into(),
            ratio: 1000,
        };
        assert_eq!(
            bomb_details(&r, "/img/a.zip"),
            "Compression ratio is 1000, skipping items in _img_a.zip."
        );
        let r = BombReason::TooManyMembers { limit: 5 };
        assert_eq!(
            bomb_details(&r, "/a.zip"),
            "The archive has more than 5 members, skipping items in _a.zip."
        );
    }
}
