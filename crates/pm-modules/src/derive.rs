//! Derived files of one source file: a reusable builder for modules that
//! extract content (archive members, embedded pictures, e-mail
//! attachments, …).
//!
//! The extracted bytes are written to
//! `ModuleOutput/<module folder>/<source id>/…` in the case folder,
//! XOR1-encoded like Autopsy's `EncodedFileOutputStream`; the rows are
//! Autopsy's `addDerivedFile` rows (origin derived, `derived_from` = the
//! source file), inserted parents before children through the job's
//! writer, with ids taken from the shared allocator. New and updated
//! files are then scheduled into the running job (`addFilesToJob`), so
//! they pass through the whole file pipeline, recursively.
//!
//! For a flat list of streams (e-mail attachments, embedded objects) use
//! [`add_children`]:
//!
//! ```text
//! let added = add_children(ctx, &file.rec, "EmailParser", vec![
//!     Stream { name: "invoice.pdf".into(), times: Times::default(), data: &mut reader },
//! ])?;
//! // `added[i]` is the file id of stream `i` (`None`: not added).
//! ```
//!
//! For trees (archives) build a [`DerivedTree`] node by node and
//! [`DerivedTree::commit`] it.
//!
//! Re-extraction follows Autopsy's `SevenZipExtractor`: a file that is
//! already in the case (same parent path and name) is left alone unless
//! the new copy is larger, in which case its row is updated (size, times,
//! local path; hashes cleared; a MIME type of `application/octet-stream`
//! reset) and it is analysed again.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use pm_archive::names;
use pm_case::Case;
use pm_case::derived::{EncodedWriter, Encoding, OutputDir};
use pm_casedb::{DerivedUpdate, FileKindCode, FileRecord, NewFile, Origin};
use pm_ingest::{JobContext, ModuleError};

/// Rows inserted per transaction (Autopsy commits every 1000 inserts).
const BATCH: usize = 1000;

/// A node of a [`DerivedTree`] (the root is the source file itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

/// Timestamps of a derived file (Unix seconds).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Times {
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    pub created: Option<i64>,
}

impl Times {
    fn row(self) -> [Option<(i64, u32)>; 4] {
        let t = |v: Option<i64>| v.filter(|&s| s != 0).map(|s| (s, 0));
        // modified, accessed, changed (never known: Autopsy passes 0), created.
        [t(self.modified), t(self.accessed), None, t(self.created)]
    }
}

/// What a node stands for once extraction is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A directory (explicit, or implied by a member path).
    Dir,
    /// A file whose content is stored at `local_path` (relative to the
    /// case folder).
    File { size: u64, local_path: String },
    /// A member listed by the container whose content could not be
    /// extracted (encrypted). Autopsy adds such members without content
    /// (`isFile` false, size 0); so does postmortem.
    NoContent,
}

#[derive(Debug)]
struct Node {
    parent: u32,
    name: String,
    /// Undecoded name, for the per-directory charset repair.
    raw: Option<Vec<u8>>,
    kind: NodeKind,
    times: Times,
    children: Vec<u32>,
}

/// Derived files already in the case for one source (re-extraction).
#[derive(Debug, Default)]
pub struct Existing {
    by_path: HashMap<String, FileRecord>,
}

impl Existing {
    /// Loads the files derived from `source_id` when it was extracted
    /// before: it has children and its output folder exists (Autopsy
    /// `getAlreadyExtractedFiles`).
    pub fn load(case: &Case, module_folder: &str, source_id: i64) -> Result<Self, ModuleError> {
        let db = case.db();
        if !db.has_children(source_id)?
            || !OutputDir::new(case.dir(), module_folder, source_id).exists()
        {
            return Ok(Self::default());
        }
        let mut by_path = HashMap::new();
        let mut after = 0;
        loop {
            let page = db.derived_files_of(source_id, after, 2000)?;
            let Some(last) = page.last() else { break };
            after = last.id;
            for f in page {
                by_path.insert(f.path(), f);
            }
        }
        Ok(Self { by_path })
    }

    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_path.len()
    }
}

/// Outcome of [`DerivedTree::commit`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Committed {
    /// New rows.
    pub added: u64,
    /// Existing rows replaced by a larger copy.
    pub updated: u64,
    /// Existing rows left as they were.
    pub unchanged: u64,
}

/// The derived files of one source file, built up while extracting and
/// written to the case database by [`Self::commit`].
#[derive(Debug)]
pub struct DerivedTree {
    ds_id: i64,
    source_id: i64,
    source_path: String,
    out: OutputDir,
    nodes: Vec<Node>,
    lookup: HashMap<(u32, String), u32>,
}

impl DerivedTree {
    /// A tree below `source`, storing content in
    /// `ModuleOutput/<module_folder>/<source id>/`.
    pub fn new(case_dir: &Path, module_folder: &str, source: &FileRecord) -> Self {
        Self {
            ds_id: source.ds_id,
            source_id: source.id,
            source_path: source.path(),
            out: OutputDir::new(case_dir, module_folder, source.id),
            nodes: vec![Node {
                parent: 0,
                name: source.name.clone(),
                raw: None,
                kind: NodeKind::Dir,
                times: Times::default(),
                children: Vec::new(),
            }],
            lookup: HashMap::new(),
        }
    }

    /// The source file.
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Nodes below the root.
    pub fn len(&self) -> usize {
        self.nodes.len() - 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The output folder of the source.
    pub fn output(&self) -> &OutputDir {
        &self.out
    }

    /// A new child of `parent`, even when a sibling has the same name
    /// (two attachments called `image.png` stay two files).
    pub fn new_child(&mut self, parent: NodeId, name: &str) -> NodeId {
        let id = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.nodes.push(Node {
            parent: parent.0,
            name: name.to_owned(),
            raw: None,
            kind: NodeKind::Dir,
            times: Times::default(),
            children: Vec::new(),
        });
        if let Some(p) = self.nodes.get_mut(parent.0 as usize) {
            p.children.push(id);
        }
        NodeId(id)
    }

    /// The child of `parent` called `name`, created as a directory if it
    /// does not exist yet (Autopsy `UnpackedTree.addNode`: a repeated
    /// name refers to the same node).
    pub fn child(&mut self, parent: NodeId, name: &str, raw: Option<&[u8]>) -> NodeId {
        let key = (parent.0, name.to_owned());
        if let Some(&id) = self.lookup.get(&key) {
            return NodeId(id);
        }
        let id = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.nodes.push(Node {
            parent: parent.0,
            name: name.to_owned(),
            raw: raw.map(<[u8]>::to_vec),
            kind: NodeKind::Dir,
            times: Times::default(),
            children: Vec::new(),
        });
        if let Some(p) = self.nodes.get_mut(parent.0 as usize) {
            p.children.push(id);
        }
        self.lookup.insert(key, id);
        NodeId(id)
    }

    /// The node at a path of `(name, raw name)` components below the
    /// root, creating missing directories.
    pub fn path<'p>(
        &mut self,
        components: impl IntoIterator<Item = (String, Option<&'p [u8]>)>,
    ) -> NodeId {
        let mut cur = self.root();
        for (name, raw) in components {
            cur = self.child(cur, &name, raw);
        }
        cur
    }

    /// Sets what a node is.
    pub fn set(&mut self, node: NodeId, kind: NodeKind, times: Times) {
        if let Some(n) = self.nodes.get_mut(node.0 as usize) {
            n.kind = kind;
            n.times = times;
        }
    }

    /// The name of a node (as decoded so far).
    pub fn name(&self, node: NodeId) -> &str {
        self.nodes
            .get(node.0 as usize)
            .map_or("", |n| n.name.as_str())
    }

    /// Creates the local file `local_name` (may contain `/`) in the
    /// output folder: returns its case-relative path and an XOR1 writer.
    pub fn create(&self, local_name: &str) -> io::Result<(String, EncodedWriter)> {
        let rel = self.out.relative(local_name);
        let w = EncodedWriter::create(&self.out.absolute(&rel), Encoding::Xor1)?;
        Ok((rel, w))
    }

    /// Copies `data` into the local file `local_name` and makes `node` a
    /// file with that content. Returns the size.
    pub fn write_file(
        &mut self,
        node: NodeId,
        local_name: &str,
        data: &mut dyn Read,
        times: Times,
    ) -> io::Result<u64> {
        let (rel, mut w) = self.create(local_name)?;
        io::copy(data, &mut w)?;
        let size = w.finish()?;
        self.set(
            node,
            NodeKind::File {
                size,
                local_path: rel,
            },
            times,
        );
        Ok(size)
    }

    /// Re-decodes the names of a directory's children when their raw
    /// bytes are in a detectable code page (Autopsy's charset fix).
    fn repair_names(&mut self, dir: u32) {
        let Some(children) = self.nodes.get(dir as usize).map(|n| n.children.clone()) else {
            return;
        };
        let raws: Vec<&[u8]> = children
            .iter()
            .filter_map(|&c| self.nodes.get(c as usize)?.raw.as_deref())
            .collect();
        if raws.is_empty() {
            return;
        }
        let Some(enc) = names::repair(&raws) else {
            return;
        };
        for c in children {
            if let Some(n) = self.nodes.get_mut(c as usize)
                && let Some(raw) = &n.raw
            {
                n.name = names::decode_with(enc, raw);
            }
        }
    }

    /// Writes the tree to the case database (parents first, in batches),
    /// then schedules new and updated files into the job. `on_id` is told
    /// each node's file id before its batch is scheduled (e.g. to record
    /// the nesting depth of archives found inside). Stops early, without
    /// error, when the job is cancelled.
    pub fn commit(
        mut self,
        ctx: &JobContext,
        existing: &Existing,
        on_id: &mut dyn FnMut(NodeId, i64),
    ) -> Result<Committed, ModuleError> {
        let mut stats = Committed::default();
        // (node, file id, path) of nodes already written; the root is the
        // source file.
        let mut ids: Vec<i64> = vec![0; self.nodes.len()];
        let mut paths: Vec<Option<String>> = vec![None; self.nodes.len()];
        if let (Some(i), Some(p)) = (ids.first_mut(), paths.first_mut()) {
            *i = self.source_id;
            *p = Some(self.source_path.clone());
        }
        let mut batch: Vec<(u32, NewFile)> = Vec::new();
        let mut rescheduled: Vec<FileRecord> = Vec::new();
        // Pre-order walk: a node is visited after its parent.
        let mut stack: Vec<u32> = vec![0];
        while let Some(n) = stack.pop() {
            if ctx.is_cancelled() {
                return Ok(stats);
            }
            if n != 0 {
                let (row, existing_rec) = self.row(n, &ids, &paths, existing)?;
                let path = format!("{}{}", row.parent_path, row.name);
                let id = match existing_rec {
                    None => {
                        let id = ctx.reserve_file_ids(1).start;
                        let mut row = row;
                        row.id = Some(id);
                        batch.push((n, row));
                        stats.added += 1;
                        id
                    }
                    Some(old) => {
                        if old.size < row.size {
                            let rec = Self::update(ctx, old, &row)?;
                            rescheduled.push(rec);
                            stats.updated += 1;
                        } else {
                            stats.unchanged += 1;
                        }
                        on_id(NodeId(n), old.id);
                        old.id
                    }
                };
                if let (Some(i), Some(p)) = (ids.get_mut(n as usize), paths.get_mut(n as usize)) {
                    *i = id;
                    *p = Some(path);
                }
                if batch.len() >= BATCH {
                    flush(ctx, &mut batch, on_id)?;
                }
            }
            self.repair_names(n);
            if let Some(node) = self.nodes.get(n as usize) {
                // Reverse, so children are visited in archive order.
                stack.extend(node.children.iter().rev());
            }
        }
        flush(ctx, &mut batch, on_id)?;
        ctx.add_files_to_job(rescheduled);
        Ok(stats)
    }

    /// The row for node `n` and the existing file it replaces, if any.
    fn row<'e>(
        &self,
        n: u32,
        ids: &[i64],
        paths: &[Option<String>],
        existing: &'e Existing,
    ) -> Result<(NewFile, Option<&'e FileRecord>), ModuleError> {
        let node = self
            .nodes
            .get(n as usize)
            .ok_or_else(|| ModuleError::new("derived tree: bad node"))?;
        let parent_id = ids.get(node.parent as usize).copied().unwrap_or(0);
        let parent_path = paths
            .get(node.parent as usize)
            .cloned()
            .flatten()
            .ok_or_else(|| ModuleError::new("derived tree: parent not written"))?;
        let kind = match node.kind {
            NodeKind::File { .. } => FileKindCode::Regular,
            NodeKind::Dir | NodeKind::NoContent => FileKindCode::Directory,
        };
        let mut row = NewFile::new(
            self.ds_id,
            Some(parent_id),
            format!("{parent_path}/"),
            node.name.clone(),
            Origin::Derived,
            kind,
        );
        row.derived_from = Some(self.source_id);
        row.times = node.times.row();
        if let NodeKind::File { size, local_path } = &node.kind {
            row.size = *size;
            row.local_path = Some(local_path.clone());
        }
        let old = existing
            .by_path
            .get(&format!("{}{}", row.parent_path, row.name));
        Ok((row, old))
    }

    /// Autopsy `updateDerivedFile`: the new copy is larger.
    fn update(
        ctx: &JobContext,
        old: &FileRecord,
        row: &NewFile,
    ) -> Result<FileRecord, ModuleError> {
        let reset_mime = old
            .mime
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("application/octet-stream"));
        let u = DerivedUpdate {
            size: row.size,
            times: row.times,
            local_path: row.local_path.clone(),
            reset_mime,
        };
        ctx.case.db().update_derived_file(old.id, &u)?;
        let mut rec = old.clone();
        rec.size = row.size;
        rec.times = row.times;
        rec.local_path.clone_from(&row.local_path);
        rec.md5 = None;
        rec.sha1 = None;
        rec.sha256 = None;
        if reset_mime {
            rec.mime = None;
        }
        Ok(rec)
    }
}

/// Inserts a batch, reports the ids, then schedules the files.
fn flush(
    ctx: &JobContext,
    batch: &mut Vec<(u32, NewFile)>,
    on_id: &mut dyn FnMut(NodeId, i64),
) -> Result<(), ModuleError> {
    if batch.is_empty() {
        return Ok(());
    }
    let nodes: Vec<u32> = batch.iter().map(|b| b.0).collect();
    let rows: Vec<NewFile> = batch.drain(..).map(|b| b.1).collect();
    let recs = ctx.insert_derived_files(rows)?;
    for (n, r) in nodes.into_iter().zip(&recs) {
        on_id(NodeId(n), r.id);
    }
    ctx.add_files_to_job(recs);
    Ok(())
}

/// One byte stream to store as a derived file.
pub struct Stream<'r> {
    /// File name shown in the case (any characters; the local copy is
    /// named by position).
    pub name: String,
    pub times: Times,
    pub data: &'r mut dyn Read,
}

impl std::fmt::Debug for Stream<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("name", &self.name)
            .field("times", &self.times)
            .finish_non_exhaustive()
    }
}

/// Stores `streams` as derived files directly below `parent`
/// (`ModuleOutput/<module_folder>/<parent id>/<n>`, XOR1), inserts them
/// and schedules them into the job, so they are analysed like any other
/// file. Returns the file id of each stream, in order (`None` when it was
/// not added: empty name or the job was cancelled).
///
/// Idempotent across ingest runs: when `parent` already has files from
/// this module (an earlier run), nothing is written and every entry is
/// `None`.
pub fn add_children<'r>(
    ctx: &JobContext,
    parent: &FileRecord,
    module_folder: &str,
    streams: impl IntoIterator<Item = Stream<'r>>,
) -> Result<Vec<Option<i64>>, ModuleError> {
    let existing = Existing::load(&ctx.case, module_folder, parent.id)?;
    let mut tree = DerivedTree::new(ctx.case.dir(), module_folder, parent);
    let mut nodes: Vec<Option<NodeId>> = Vec::new();
    for (i, s) in streams.into_iter().enumerate() {
        if !existing.is_empty() || s.name.is_empty() || ctx.is_cancelled() {
            nodes.push(None);
            continue;
        }
        let node = tree.new_child(tree.root(), &s.name);
        tree.write_file(node, &i.to_string(), s.data, s.times)?;
        nodes.push(Some(node));
    }
    let mut ids: HashMap<NodeId, i64> = HashMap::new();
    if !tree.is_empty() {
        tree.commit(ctx, &Existing::default(), &mut |n, id| {
            ids.insert(n, id);
        })?;
    }
    Ok(nodes
        .into_iter()
        .map(|n| n.and_then(|n| ids.get(&n).copied()))
        .collect())
}

/// Autopsy `FileUtil.escapeFileName`: every run of control characters and
/// `/:"*?<>|` becomes one `_` (`\` is kept), then surrounding white space
/// is trimmed. Used for Autopsy's message texts and local file names.
pub fn escape_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.chars() {
        if c.is_control() || matches!(c, '/' | ':' | '"' | '*' | '?' | '<' | '>' | '|') {
            if !in_run {
                out.push('_');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out.trim().to_owned()
}

/// Splits a member path into components on `/` and `\` (Autopsy
/// `UnpackedTree.addNode`), each with its undecoded bytes when the
/// container stores names as bytes. Empty and `.` components are dropped.
///
/// With raw bytes, the components come from the bytes and are decoded one
/// by one with [`names::decode_default`] (as the archive readers decode
/// whole paths). A `\` byte may be the second byte of a Shift-JIS
/// character, so it only separates components in pure-ASCII names.
pub fn split_path<'a>(path: &str, raw: Option<&'a [u8]>) -> Vec<(String, Option<&'a [u8]>)> {
    match raw {
        Some(r) => {
            let ascii = r.is_ascii();
            r.split(|&b| b == b'/' || (ascii && b == b'\\'))
                .filter(|t| !t.is_empty() && *t != b".")
                .map(|t| (names::decode_default(t), Some(t)))
                .collect()
        }
        None => path
            .split(['/', '\\'])
            .filter(|t| !t.is_empty() && *t != ".")
            .map(|t| (t.to_owned(), None))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_follows_autopsy() {
        assert_eq!(escape_file_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(escape_file_name("normal name.doc"), "normal name.doc");
        assert_eq!(escape_file_name("x\u{1}y"), "x_y");
        assert_eq!(escape_file_name("/img/dir/a.zip"), "_img_dir_a.zip");
        assert_eq!(escape_file_name(" a:/?b\\c "), "a_b\\c");
    }

    #[test]
    fn paths_split_with_raw_bytes() {
        let names = |v: Vec<(String, Option<&[u8]>)>| -> Vec<String> {
            v.into_iter().map(|p| p.0).collect()
        };
        assert_eq!(names(split_path("a/b\\c.txt", None)), ["a", "b", "c.txt"]);
        assert_eq!(names(split_path("//./x/", None)), ["x"]);
        let raw = b"d\x81r/f\x94.txt";
        let parts = split_path("", Some(raw));
        assert_eq!(parts[0], ("d\u{fc}r".to_owned(), Some(&b"d\x81r"[..])));
        assert_eq!(parts[1].0, "f\u{f6}.txt");
        assert_eq!(names(split_path("", Some(b"a\\b/c"))), ["a", "b", "c"]);
        // Shift-JIS 0x5C trail byte: one component, repaired later.
        let sjis = encoding_rs::SHIFT_JIS.encode("\u{8868}.txt").0.into_owned();
        let parts = split_path("", Some(&sjis));
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].1, Some(&sjis[..]));
    }

    #[test]
    fn times_skip_zero() {
        let t = Times {
            modified: Some(5),
            accessed: Some(0),
            created: None,
        };
        assert_eq!(t.row(), [Some((5, 0)), None, None, None]);
    }
}
