//! Access to the content of case files.

use std::sync::Arc;

use pm_casedb::{FileRecord, Origin};
use pm_core::io::{FileSource, MemSource};
use pm_core::{ReadAt, SharedSource};
use pm_fs::{Extent, ExtentKind, ExtentSource, MetaFlags};

use crate::derived::{EncodedSource, sniff_encoding};
use crate::{Case, CaseError, Result, io_err};

/// Image byte ranges `(offset, length)` of a layout file, as stored in
/// `files.layout`.
pub fn parse_layout(json: &str) -> Option<Vec<(u64, u64)>> {
    serde_json::from_str(json).ok()
}

/// Serialises image byte ranges for `files.layout`.
pub fn layout_json(ranges: &[(u64, u64)]) -> String {
    // Infallible for a slice of integer pairs.
    serde_json::to_string(ranges).unwrap_or_else(|_| "[]".into())
}

impl Case {
    /// Opens the content of a file row (its default or alternate stream,
    /// unallocated/carved byte ranges, or a locally stored derived file).
    pub fn content(&self, f: &FileRecord) -> Result<SharedSource> {
        match f.origin {
            Origin::FileSystem | Origin::Slack => {
                let fs_id = f
                    .fs_id
                    .ok_or_else(|| CaseError::NotFound(format!("file system of file {}", f.id)))?;
                let inode = f
                    .inode
                    .ok_or_else(|| CaseError::NotFound(format!("inode of file {}", f.id)))?;
                // A name whose metadata is gone (address 0, or the entry
                // was reused by another file) has no recoverable content;
                // reading the inode would return the *other* file's data.
                let no_meta =
                    !f.meta_alloc && f.meta_flags & u32::from(MetaFlags::USED.bits()) == 0;
                if no_meta && f.stream.is_none() {
                    return Ok(Arc::new(MemSource::new(Vec::new())));
                }
                Ok(self.filesystem(fs_id)?.open(inode, f.stream)?)
            }
            Origin::UnallocBlocks | Origin::Carved => {
                let layout = self
                    .db()
                    .file_layout(f.id)?
                    .and_then(|j| parse_layout(&j))
                    .ok_or_else(|| CaseError::NotFound(format!("layout of file {}", f.id)))?;
                let image = self.image_source(f.ds_id)?;
                let mut logical = 0u64;
                let extents: Vec<Extent> = layout
                    .into_iter()
                    .map(|(physical_offset, len)| {
                        let e = Extent {
                            logical_offset: logical,
                            physical_offset,
                            len,
                            kind: ExtentKind::Allocated,
                        };
                        logical = logical.saturating_add(len);
                        e
                    })
                    .collect();
                Ok(Arc::new(ExtentSource::new(image, extents, f.size)))
            }
            Origin::Local | Origin::Derived => {
                let rel = f
                    .local_path
                    .as_deref()
                    .ok_or_else(|| CaseError::NotFound(format!("local copy of file {}", f.id)))?;
                // Local files of a logical-files data source may point at
                // their original absolute location and are read as they
                // are; derived files live in the case folder, normally
                // XOR1-encoded (recognised by the header, as Autopsy does).
                if f.origin == Origin::Local && std::path::Path::new(rel).is_absolute() {
                    return Ok(Arc::new(FileSource::open(rel).map_err(io_err(rel))?));
                }
                let path = crate::derived::resolve(self.dir(), rel);
                let src = FileSource::open(&path).map_err(io_err(&path))?;
                let mut head = [0u8; 32];
                let n = src.read_at(0, &mut head).map_err(io_err(&path))?;
                let encoding = sniff_encoding(head.get(..n).unwrap_or_default());
                Ok(Arc::new(EncodedSource::new(src, encoding)))
            }
            Origin::VirtualDir => Ok(Arc::new(MemSource::new(Vec::new()))),
        }
    }
}
