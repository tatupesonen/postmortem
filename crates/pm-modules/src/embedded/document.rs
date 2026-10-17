//! Embedded content of Office and PDF documents (Autopsy
//! `DocumentEmbeddedContentExtractor`): pictures, embedded objects and
//! attachments become flat derived children of the document.
//!
//! Names follow Autopsy: Office Open XML parts keep their file name
//! (escaped), legacy Office pictures are `image_<n>.<ext>`, PDF resources
//! keep their name and are stored as `extract_<n>[.<ext>]`. No timestamps
//! are recorded (Autopsy passes zeros).

use std::io;

use pm_archive::{ArchiveError, Entry, Format, MemberEnd, Options, Sink};
use pm_case::derived::EncodedWriter;
use pm_ingest::{IngestFile, JobContext, ModuleError};

use super::{Job, OUTPUT_FOLDER, Root};
use crate::derive::{DerivedTree, Existing, NodeId, NodeKind, Times, escape_file_name};

struct Current {
    node: NodeId,
    rel: String,
    writer: EncodedWriter,
}

struct FlatSink<'a> {
    ctx: &'a JobContext,
    pdf: bool,
    tree: DerivedTree,
    cur: Option<Current>,
    /// PDF resources written so far (`extract_<n>`).
    count: u64,
}

impl FlatSink<'_> {
    fn abandon(&mut self) {
        if let Some(c) = self.cur.take() {
            drop(c.writer);
            let _ = std::fs::remove_file(self.tree.output().absolute(&c.rel));
            self.tree.set(c.node, NodeKind::NoContent, Times::default());
        }
    }
}

/// The extension of a resource name (`FilenameUtils.getExtension`).
fn extension(name: &str) -> Option<&str> {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    file.rfind('.').and_then(|i| file.get(i + 1..))
}

impl Sink for FlatSink<'_> {
    fn begin(&mut self, e: &Entry) -> io::Result<bool> {
        self.abandon();
        if e.is_dir || e.encrypted || e.path.is_empty() {
            return Ok(false);
        }
        let (name, local) = if self.pdf {
            let mut local = format!("extract_{}", self.count);
            self.count += 1;
            if let Some(ext) = extension(&e.path).filter(|x| !x.is_empty()) {
                local = format!("{local}.{}", escape_file_name(ext));
            }
            (e.path.clone(), local)
        } else {
            let name = escape_file_name(&e.path);
            (name.clone(), name)
        };
        if name.is_empty() || local.is_empty() {
            return Ok(false);
        }
        let root = self.tree.root();
        let node = self.tree.child(root, &name, None);
        let (rel, writer) = self.tree.create(&local)?;
        self.cur = Some(Current { node, rel, writer });
        Ok(true)
    }

    fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some(c) = self.cur.as_mut() {
            io::Write::write_all(&mut c.writer, data)?;
        }
        Ok(())
    }

    fn end(&mut self, end: &MemberEnd) -> io::Result<()> {
        let Some(c) = self.cur.take() else {
            return Ok(());
        };
        let size = c.writer.finish()?;
        if !matches!(end, MemberEnd::Complete) {
            tracing::debug!("embedded item {}: {end:?}", self.tree.name(c.node));
        }
        self.tree.set(
            c.node,
            NodeKind::File {
                size,
                local_path: c.rel,
            },
            Times::default(),
        );
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        self.ctx.is_cancelled()
    }
}

/// Extracts the embedded content of one document.
pub(super) fn extract(
    job: &Job,
    ctx: &JobContext,
    file: &IngestFile,
    format: Format,
) -> Result<(), ModuleError> {
    let existing = Existing::load(&ctx.case, OUTPUT_FOLDER, file.rec.id)?;
    if !existing.is_empty() {
        // Already extracted by an earlier run (Autopsy intends the same
        // skip; its check is commented out and it adds duplicates).
        return Ok(());
    }
    let src = file.content()?;
    let opts = Options {
        temp_dir: Some(ctx.case.dir().join("Temp").join(OUTPUT_FOLDER)),
        ..Options::default()
    };
    let mut sink = FlatSink {
        ctx,
        pdf: format == Format::Pdf,
        tree: DerivedTree::new(ctx.case.dir(), OUTPUT_FOLDER, &file.rec),
        cur: None,
        count: 0,
    };
    let result = pm_archive::extract(format, &*src, &opts, &mut sink);
    sink.abandon();
    match result {
        Ok(_) => {}
        Err(ArchiveError::Cancelled) => return Ok(()),
        Err(ArchiveError::Bomb(reason)) => {
            // Office Open XML files are zip files: the same limits apply.
            let root = Root {
                id: file.rec.id,
                path: file.rec.path(),
            };
            let details = format!(
                "Compression ratio is {}, skipping items in {}.",
                reason.ratio().unwrap_or(0),
                escape_file_name(&root.path)
            );
            job.flag_zip_bomb(ctx, &root, file, &details);
        }
        Err(e) => {
            // Autopsy logs unreadable documents and moves on.
            tracing::warn!(
                "error while parsing {} for embedded content, skipping: {e}",
                file.rec.path()
            );
        }
    }
    if ctx.is_cancelled() || sink.tree.is_empty() {
        return Ok(());
    }
    sink.tree.commit(ctx, &existing, &mut |_, _| {})?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(extension("a.pdf"), Some("pdf"));
        assert_eq!(extension("dir.x/file"), None);
        assert_eq!(extension("file."), Some(""));
        assert_eq!(extension("noext"), None);
    }
}
