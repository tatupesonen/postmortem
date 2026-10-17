//! The file-name index (ADR-014: substring search over hundreds of
//! millions of names without `LIKE` scans).
//!
//! Every file's name and full path are indexed as positional trigrams of
//! `\u{2}` + lower-cased text + `\u{3}`. A substring query is then a
//! phrase of the query's trigrams at fixed offsets, which is exact: no
//! verification pass. Prefix and exact-name queries anchor on the
//! markers; one- and two-character queries use the trigram dictionary.
//! Results are file ids in ascending order, keyset-paged.
//!
//! [`NameIndex::catch_up`] indexes the case database's files above the
//! last indexed id (stored in the commit payload), so it is cheap to call
//! after every data source or ingest job.

use std::ops::Bound;
use std::path::Path;

use parking_lot::Mutex;
use pm_casedb::{CaseDb, FileKindCode, FileQuery, FileRecord};
use serde::{Deserialize, Serialize};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::{Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term};

use crate::tokenize::{NAME_END, NAME_START, POS_TRIGRAMS};
use crate::{SearchError, register_tokenizers};

const WRITER_HEAP: usize = 256 * 1024 * 1024;
const WRITER_THREADS: usize = 4;
/// Files read from the case database per batch.
const CATCH_UP_BATCH: usize = 20_000;
/// Commit (and record progress) after this many names.
const COMMIT_EVERY: u64 = 1_000_000;

#[derive(Debug, Clone, Copy)]
struct Fields {
    id: Field,
    ds: Field,
    name: Field,
    path: Field,
    ext: Field,
    size: Field,
    deleted: Field,
    dir: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let id = b.add_i64_field("id", INDEXED | FAST | STORED);
    let ds = b.add_i64_field("ds", INDEXED | FAST);
    let tri = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(POS_TRIGRAMS)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let name = b.add_text_field("name", tri.clone());
    let path = b.add_text_field("path", tri);
    let ext = b.add_text_field("ext", STRING);
    let size = b.add_u64_field("size", INDEXED | FAST);
    let deleted = b.add_u64_field("deleted", INDEXED);
    let dir = b.add_u64_field("dir", INDEXED);
    (
        b.build(),
        Fields {
            id,
            ds,
            name,
            path,
            ext,
            size,
            deleted,
            dir,
        },
    )
}

/// How the query text must match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameMatch {
    /// Anywhere in the name (or path).
    #[default]
    Substring,
    /// At the start of the name (or path).
    Prefix,
    /// The whole name (or path).
    Exact,
}

/// A file-name query. Matching ignores case.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameQuery {
    /// Text to find; empty matches every file (filters only).
    pub text: String,
    #[serde(default)]
    pub mode: NameMatch,
    /// Match against the full path (`/dir/name`) instead of the name.
    #[serde(default)]
    pub in_path: bool,
    /// Data sources (empty: all).
    #[serde(default)]
    pub ds_ids: Vec<i64>,
    /// Leave out deleted files.
    #[serde(default)]
    pub exclude_deleted: bool,
    /// Leave out directories.
    #[serde(default)]
    pub exclude_dirs: bool,
    /// Lower-case extensions without the dot (empty: any).
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub min_size: Option<u64>,
    #[serde(default)]
    pub max_size: Option<u64>,
}

/// A page of name-search results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamePage {
    /// Matching file ids, ascending.
    pub ids: Vec<i64>,
    /// Pass as `after` to continue; `None` when nothing is left.
    pub next: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Payload {
    /// Highest file id indexed.
    last_id: i64,
}

/// The file-name index of a case.
pub struct NameIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<Option<IndexWriter>>,
    f: Fields,
}

impl std::fmt::Debug for NameIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameIndex").finish_non_exhaustive()
    }
}

impl NameIndex {
    /// Opens or creates the index in `dir`.
    pub fn open(dir: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(dir)?;
        let (schema, f) = schema();
        let mmap = tantivy::directory::MmapDirectory::open(dir)
            .map_err(|e| SearchError::Index(e.to_string()))?;
        let index = Index::open_or_create(mmap, schema)?;
        register_tokenizers(&index);
        let writer = match index.writer_with_num_threads(WRITER_THREADS, WRITER_HEAP) {
            Ok(w) => Some(w),
            Err(tantivy::TantivyError::LockFailure(..)) => {
                tracing::warn!("name index {} is locked: read-only", dir.display());
                None
            }
            Err(e) => return Err(e.into()),
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            reader,
            writer: Mutex::new(writer),
            f,
        })
    }

    /// The highest file id indexed so far (0: none).
    pub fn last_indexed_id(&self) -> Result<i64, SearchError> {
        let meta = self.index.load_metas()?;
        Ok(meta
            .payload
            .as_deref()
            .and_then(|p| serde_json::from_str::<Payload>(p).ok())
            .map_or(0, |p| p.last_id))
    }

    fn doc(&self, f: &FileRecord) -> TantivyDocument {
        let mut d = TantivyDocument::default();
        d.add_i64(self.f.id, f.id);
        d.add_i64(self.f.ds, f.ds_id);
        d.add_text(self.f.name, &f.name);
        d.add_text(self.f.path, f.path());
        d.add_text(self.f.ext, f.extension.to_lowercase());
        d.add_u64(self.f.size, f.size);
        d.add_u64(self.f.deleted, u64::from(f.deleted()));
        d.add_u64(self.f.dir, u64::from(f.kind == FileKindCode::Directory));
        d
    }

    fn indexable(f: &FileRecord) -> bool {
        f.name != "." && f.name != ".."
    }

    /// Indexes files the case database has above the last indexed id;
    /// returns how many were added. `cancelled` is polled per batch;
    /// progress is committed every million names and at the end.
    pub fn catch_up(&self, db: &CaseDb, cancelled: &dyn Fn() -> bool) -> Result<u64, SearchError> {
        let added = {
            let mut guard = self.writer.lock();
            let w = guard.as_mut().ok_or(SearchError::ReadOnly)?;
            self.catch_up_with(w, db, cancelled)?
        };
        if added > 0 {
            self.reader.reload()?;
        }
        Ok(added)
    }

    fn catch_up_with(
        &self,
        w: &mut IndexWriter,
        db: &CaseDb,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<u64, SearchError> {
        let mut last = self.last_indexed_id()?;
        let mut added = 0u64;
        let mut since_commit = 0u64;
        loop {
            if cancelled() {
                break;
            }
            let files = db.query_files(&FileQuery::default(), last, CATCH_UP_BATCH)?;
            let Some(tail) = files.last() else {
                break;
            };
            last = tail.id;
            for f in files.iter().filter(|f| Self::indexable(f)) {
                w.add_document(self.doc(f))?;
                added += 1;
                since_commit += 1;
            }
            if since_commit >= COMMIT_EVERY {
                commit_with(w, last)?;
                since_commit = 0;
            }
        }
        if added > 0 || since_commit > 0 {
            commit_with(w, last)?;
        }
        Ok(added)
    }

    /// Indexes files directly (e.g. derived files outside a catch-up);
    /// commit with [`NameIndex::commit`]. Files at or below the last
    /// indexed id are not added twice by a later catch-up.
    pub fn add(&self, files: &[FileRecord]) -> Result<(), SearchError> {
        let w = self.writer.lock();
        let w = w.as_ref().ok_or(SearchError::ReadOnly)?;
        for f in files.iter().filter(|f| Self::indexable(f)) {
            w.delete_term(Term::from_field_i64(self.f.id, f.id));
            w.add_document(self.doc(f))?;
        }
        Ok(())
    }

    /// Commits added files.
    pub fn commit(&self) -> Result<(), SearchError> {
        let last = self.last_indexed_id()?;
        {
            let mut w = self.writer.lock();
            let w = w.as_mut().ok_or(SearchError::ReadOnly)?;
            commit_with(w, last)?;
        }
        self.reader.reload()?;
        Ok(())
    }

    /// Removes a data source's files.
    pub fn delete_data_source(&self, ds_id: i64) -> Result<(), SearchError> {
        {
            let w = self.writer.lock();
            let w = w.as_ref().ok_or(SearchError::ReadOnly)?;
            w.delete_term(Term::from_field_i64(self.f.ds, ds_id));
        }
        self.commit()
    }

    /// Picks up commits made by other handles or processes.
    pub fn reload(&self) -> Result<(), SearchError> {
        self.reader.reload()?;
        Ok(())
    }

    fn text_query(&self, q: &NameQuery) -> Result<Option<Box<dyn Query>>, SearchError> {
        let field = if q.in_path { self.f.path } else { self.f.name };
        let lower: String = q.text.chars().flat_map(char::to_lowercase).collect();
        if lower.is_empty() {
            return Ok(None);
        }
        let mut chars: Vec<char> = Vec::with_capacity(lower.len() + 2);
        if q.mode != NameMatch::Substring {
            chars.push(NAME_START);
        }
        chars.extend(lower.chars());
        if q.mode == NameMatch::Exact {
            chars.push(NAME_END);
        }
        if chars.len() < 3 {
            // Trigrams containing the text (one or two characters, or a
            // one-character prefix).
            let pattern = match q.mode {
                NameMatch::Prefix => {
                    format!("{}.*", regex::escape(&chars.iter().collect::<String>()))
                }
                _ => format!(".*{}.*", regex::escape(&lower)),
            };
            return Ok(Some(Box::new(RegexQuery::from_pattern(&pattern, field)?)));
        }
        // Non-overlapping trigrams at offsets 0, 3, 6, … plus the last
        // one cover every character, so the phrase is an exact match.
        let n = chars.len() - 2;
        let mut offsets: Vec<usize> = (0..n).step_by(3).collect();
        if offsets.last() != Some(&(n - 1)) {
            offsets.push(n - 1);
        }
        let terms: Vec<(usize, Term)> = offsets
            .into_iter()
            .map(|o| {
                let t: String = chars.get(o..o + 3).unwrap_or_default().iter().collect();
                (o, Term::from_field_text(field, &t))
            })
            .collect();
        if terms.len() == 1 {
            let (_, t) = terms
                .into_iter()
                .next()
                .unwrap_or((0, Term::from_field_text(field, "")));
            return Ok(Some(Box::new(TermQuery::new(t, IndexRecordOption::Basic))));
        }
        Ok(Some(Box::new(PhraseQuery::new_with_offset(terms))))
    }

    fn query(&self, q: &NameQuery, after: Option<i64>) -> Result<Box<dyn Query>, SearchError> {
        let mut must: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if let Some(t) = self.text_query(q)? {
            must.push((Occur::Must, t));
        }
        let term_q =
            |t: Term| -> Box<dyn Query> { Box::new(TermQuery::new(t, IndexRecordOption::Basic)) };
        if !q.ds_ids.is_empty() {
            must.push((
                Occur::Must,
                Box::new(BooleanQuery::union(
                    q.ds_ids
                        .iter()
                        .map(|&d| term_q(Term::from_field_i64(self.f.ds, d)))
                        .collect(),
                )),
            ));
        }
        if q.exclude_deleted {
            must.push((Occur::Must, term_q(Term::from_field_u64(self.f.deleted, 0))));
        }
        if q.exclude_dirs {
            must.push((Occur::Must, term_q(Term::from_field_u64(self.f.dir, 0))));
        }
        if !q.extensions.is_empty() {
            must.push((
                Occur::Must,
                Box::new(BooleanQuery::union(
                    q.extensions
                        .iter()
                        .map(|e| {
                            let e = e.trim_start_matches('.').to_lowercase();
                            term_q(Term::from_field_text(self.f.ext, &e))
                        })
                        .collect(),
                )),
            ));
        }
        if q.min_size.is_some() || q.max_size.is_some() {
            let lo = q.min_size.map_or(Bound::Unbounded, |v| {
                Bound::Included(Term::from_field_u64(self.f.size, v))
            });
            let hi = q.max_size.map_or(Bound::Unbounded, |v| {
                Bound::Included(Term::from_field_u64(self.f.size, v))
            });
            must.push((Occur::Must, Box::new(RangeQuery::new(lo, hi))));
        }
        if let Some(a) = after {
            must.push((
                Occur::Must,
                Box::new(RangeQuery::new(
                    Bound::Excluded(Term::from_field_i64(self.f.id, a)),
                    Bound::Unbounded,
                )),
            ));
        }
        if must.is_empty() {
            return Ok(Box::new(AllQuery));
        }
        Ok(Box::new(BooleanQuery::new(must)))
    }

    /// Matching file ids after `after`, ascending, at most `limit`.
    pub fn search(
        &self,
        q: &NameQuery,
        after: Option<i64>,
        limit: usize,
    ) -> Result<NamePage, SearchError> {
        let limit = limit.max(1);
        let query = self.query(q, after)?;
        let s = self.reader.searcher();
        let top = s.search(
            &*query,
            &TopDocs::with_limit(limit + 1).order_by_fast_field::<i64>("id", Order::Asc),
        )?;
        let mut ids: Vec<i64> = top.into_iter().filter_map(|(id, _)| id).collect();
        let more = ids.len() > limit;
        ids.truncate(limit);
        let next = if more { ids.last().copied() } else { None };
        Ok(NamePage { ids, next })
    }

    /// The number of matching files.
    pub fn count(&self, q: &NameQuery) -> Result<u64, SearchError> {
        let query = self.query(q, None)?;
        Ok(self.reader.searcher().search(&*query, &Count)? as u64)
    }

    /// Names in the index.
    pub fn len(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn commit_with(w: &mut IndexWriter, last_id: i64) -> Result<(), SearchError> {
    let payload = serde_json::to_string(&Payload { last_id }).unwrap_or_default();
    let mut c = w.prepare_commit()?;
    c.set_payload(&payload);
    c.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: i64, ds: i64, parent: &str, name: &str) -> FileRecord {
        FileRecord {
            id,
            ds_id: ds,
            fs_id: None,
            parent_id: None,
            origin: pm_casedb::Origin::FileSystem,
            inode: None,
            stream: None,
            name: name.into(),
            parent_path: parent.into(),
            kind: if name.contains('.') {
                FileKindCode::Regular
            } else {
                FileKindCode::Directory
            },
            name_alloc: id % 5 != 0,
            meta_alloc: true,
            meta_flags: 0,
            size: u64::try_from(id).unwrap_or(0) * 100,
            times: [None; 4],
            mode: None,
            uid: None,
            gid: None,
            seq: None,
            attributes: None,
            extension: name
                .rsplit_once('.')
                .map(|(_, e)| e.to_owned())
                .unwrap_or_default(),
            mime: None,
            md5: None,
            sha1: None,
            sha256: None,
            known: pm_casedb::Known::Unknown,
            derived_from: None,
            local_path: None,
        }
    }

    fn sample() -> (tempfile::TempDir, NameIndex) {
        let dir = tempfile::tempdir().unwrap();
        let ix = NameIndex::open(dir.path()).unwrap();
        ix.add(&[
            rec(1, 1, "/", "Users"),
            rec(2, 1, "/Users/", "alice"),
            rec(3, 1, "/Users/alice/", "Report_2020.DOCX"),
            rec(4, 1, "/Users/alice/", "notes.txt"),
            rec(5, 1, "/Users/alice/", "secret report.pdf"),
            rec(6, 2, "/", "ab"),
            rec(7, 2, "/", "a.b"),
            rec(8, 2, "/", "."),
        ])
        .unwrap();
        ix.commit().unwrap();
        (dir, ix)
    }

    fn ids(ix: &NameIndex, q: &NameQuery) -> Vec<i64> {
        ix.search(q, None, 100).unwrap().ids
    }

    fn text(t: &str, mode: NameMatch) -> NameQuery {
        NameQuery {
            text: t.into(),
            mode,
            ..NameQuery::default()
        }
    }

    #[test]
    fn substring_prefix_exact() {
        let (_d, ix) = sample();
        assert_eq!(ids(&ix, &text("report", NameMatch::Substring)), [3, 5]);
        assert_eq!(
            ids(&ix, &text("REPORT_2020.docx", NameMatch::Substring)),
            [3]
        );
        assert_eq!(ids(&ix, &text("rt_20", NameMatch::Substring)), [3]);
        assert_eq!(ids(&ix, &text("report", NameMatch::Prefix)), [3]);
        assert_eq!(ids(&ix, &text("notes.txt", NameMatch::Exact)), [4]);
        assert_eq!(
            ids(&ix, &text("notes", NameMatch::Exact)),
            Vec::<i64>::new()
        );
        assert_eq!(ids(&ix, &text("ab", NameMatch::Exact)), [6]);
        assert_eq!(ids(&ix, &text("b", NameMatch::Substring)), [6, 7]);
        assert_eq!(ids(&ix, &text("a", NameMatch::Prefix)), [2, 6, 7]);
        assert_eq!(ids(&ix, &text("ab", NameMatch::Substring)), [6]);
        assert_eq!(ids(&ix, &text("t r", NameMatch::Substring)), [5]);
        // "." entries are not indexed.
        assert_eq!(ix.len(), 7);
    }

    #[test]
    fn paths_and_filters() {
        let (_d, ix) = sample();
        let mut q = text("alice/", NameMatch::Substring);
        q.in_path = true;
        assert_eq!(ids(&ix, &q), [3, 4, 5]);
        q.text = "/users/alice".into();
        q.mode = NameMatch::Exact;
        assert_eq!(ids(&ix, &q), [2]);
        let mut q = NameQuery {
            extensions: vec![".DOCX".into(), "pdf".into()],
            ..NameQuery::default()
        };
        assert_eq!(ids(&ix, &q), [3, 5]);
        q.exclude_deleted = true;
        assert_eq!(ids(&ix, &q), [3]);
        let mut q = NameQuery {
            exclude_dirs: true,
            ds_ids: vec![1],
            min_size: Some(400),
            max_size: Some(500),
            ..NameQuery::default()
        };
        assert_eq!(ids(&ix, &q), [4, 5]);
        q.ds_ids = vec![2];
        assert_eq!(ids(&ix, &q), Vec::<i64>::new());
        assert_eq!(ix.count(&NameQuery::default()).unwrap(), 7);
    }

    #[test]
    fn keyset_paging() {
        let (_d, ix) = sample();
        let q = NameQuery::default();
        let p1 = ix.search(&q, None, 3).unwrap();
        assert_eq!(p1.ids, [1, 2, 3]);
        assert_eq!(p1.next, Some(3));
        let p2 = ix.search(&q, p1.next, 3).unwrap();
        assert_eq!(p2.ids, [4, 5, 6]);
        let p3 = ix.search(&q, p2.next, 3).unwrap();
        assert_eq!(p3.ids, [7]);
        assert_eq!(p3.next, None);
    }

    #[test]
    fn re_adding_replaces() {
        let (_d, ix) = sample();
        ix.add(&[rec(4, 1, "/Users/alice/", "renamed.txt")])
            .unwrap();
        ix.commit().unwrap();
        assert!(ids(&ix, &text("notes", NameMatch::Substring)).is_empty());
        assert_eq!(ids(&ix, &text("renamed", NameMatch::Substring)), [4]);
        ix.delete_data_source(2).unwrap();
        assert_eq!(ix.len(), 5);
    }
}
