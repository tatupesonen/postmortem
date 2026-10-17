//! The keyword (content) index: Autopsy's Solr core, in Tantivy.
//!
//! Documents (Autopsy's Solr document model, made smaller):
//! * one **chunk document** per chunk of extracted text (`page` 1…N):
//!   the text indexed as words with positions (exact match) and as
//!   distinct lower-cased trigrams (substring and regex candidates), and
//!   stored once (compressed) for verification, the viewer and snippets;
//! * one **summary document** per object (`page` 0): the file name (or
//!   artifact label), indexed the same way, and the number of chunks.
//!
//! Queries find candidate documents in the inverted index and verify
//! them with the same [`Matcher`] the ingest module uses, so ad-hoc and
//! ingest results agree. Results are one row per object (its lowest
//! page with a hit), keyset-paged by object key.
//!
//! Objects are files (key = file id) or artifacts (key = −artifact id).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use tantivy::collector::{Collector, Count, SegmentCollector};
use tantivy::columnar::Column;
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing,
    TextOptions, Value,
};
use tantivy::{
    DocAddress, DocId, Index, IndexReader, IndexWriter, ReloadPolicy, Score, SegmentOrdinal,
    SegmentReader, TantivyDocument, Term,
};

use crate::keywords::SearchType;
use crate::matcher::{Hit, Matcher};
use crate::plan::Plan;
use crate::tokenize::{TRIGRAMS, WORDS, word_terms};
use crate::{ObjRef, SearchError, register_tokenizers};

/// Writer heap (shared by its threads).
const WRITER_HEAP: usize = 256 * 1024 * 1024;
const WRITER_THREADS: usize = 4;
/// `maybe_commit` commits at most this often.
const COMMIT_INTERVAL: Duration = Duration::from_mins(1);
/// Distinct hit terms kept per object in search results.
const MAX_HITS_PER_OBJECT: usize = 100;
/// Upper bound on objects collected per search pass.
const MAX_BATCH: usize = 65_536;

#[derive(Debug, Clone, Copy)]
struct Fields {
    obj: Field,
    page: Field,
    ds: Field,
    text: Field,
    tri: Field,
    body: Field,
    base: Field,
    base_lower: Field,
    name: Field,
    chunks: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let obj = b.add_i64_field("obj", INDEXED | FAST | STORED);
    let page = b.add_u64_field("page", INDEXED | FAST | STORED);
    let ds = b.add_i64_field("ds", INDEXED | FAST);
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(WORDS)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let text = b.add_text_field("text", words);
    let tri_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TRIGRAMS)
            .set_index_option(IndexRecordOption::Basic),
    );
    let tri = b.add_text_field("tri", tri_opts);
    let body = b.add_text_field("body", STORED);
    let base = b.add_u64_field("base", STORED);
    let base_lower = b.add_u64_field("base_lower", STORED);
    let name = b.add_text_field("name", STRING | STORED);
    let chunks = b.add_u64_field("chunks", STORED);
    (
        b.build(),
        Fields {
            obj,
            page,
            ds,
            text,
            tri,
            body,
            base,
            base_lower,
            name,
            chunks,
        },
    )
}

/// A chunk to index.
#[derive(Debug, Clone, Copy)]
pub struct ChunkDoc<'a> {
    pub obj: ObjRef,
    pub ds_id: i64,
    /// 1-based chunk number.
    pub page: u32,
    pub text: &'a str,
    /// Bytes of `text` before the overlap window (hits starting there or
    /// later are reported by the next chunk).
    pub base: usize,
    /// The same in the lower-cased text.
    pub base_lower: usize,
}

/// An ad-hoc query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextQuery {
    pub term: String,
    pub search_type: SearchType,
    /// Restrict to these data sources (empty: all).
    pub ds_ids: Vec<i64>,
}

impl TextQuery {
    /// The query that finds a keyword hit again (Extracted Text viewer):
    /// the regular expression if the hit has one, else the hit text as
    /// an exact (search type 0) or substring match. Arguments are the
    /// hit's `TSK_KEYWORD`, `TSK_KEYWORD_SEARCH_TYPE` and
    /// `TSK_KEYWORD_REGEXP`.
    pub fn for_keyword_hit(keyword: &str, search_type: Option<i64>, regexp: Option<&str>) -> Self {
        let (term, search_type) = match (regexp, search_type) {
            (Some(r), _) if !r.is_empty() => (r.to_owned(), SearchType::Regex),
            (_, Some(1)) => (keyword.to_owned(), SearchType::Substring),
            _ => (keyword.to_owned(), SearchType::Exact),
        };
        Self {
            term,
            search_type,
            ds_ids: Vec::new(),
        }
    }
}

/// One result row: an object with at least one verified hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHit {
    pub obj: ObjRef,
    /// Lowest page with a hit (0 = the name).
    pub page: u32,
    /// Snippet of the first hit on that page (Autopsy's "Keyword
    /// Preview").
    pub preview: String,
    /// Distinct hit terms on all pages (at most 100), first first.
    pub hits: Vec<String>,
}

/// A page of results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchPage {
    pub hits: Vec<ObjectHit>,
    /// Pass as `after` to continue; `None` when nothing is left.
    pub next: Option<i64>,
}

/// Highlight ranges on one page of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHits {
    pub page: u32,
    /// Byte ranges in the page's original text.
    pub ranges: Vec<std::ops::Range<usize>>,
}

/// A stored page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    /// For page 0, the object's name or label.
    pub text: String,
    /// Byte length of the part before the overlap window.
    pub base: usize,
}

/// Counts shown by Autopsy as "Files in keyword index" / "Chunks in
/// keyword index".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndexStats {
    pub objects: u64,
    pub chunks: u64,
}

/// The content index of a case.
pub struct ContentIndex {
    reader: IndexReader,
    /// `None` when another process holds the writer lock (read-only).
    writer: RwLock<Option<IndexWriter>>,
    f: Fields,
    last_commit: Mutex<Instant>,
    uncommitted: AtomicU64,
}

impl std::fmt::Debug for ContentIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentIndex")
            .field("uncommitted", &self.uncommitted.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl ContentIndex {
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
                tracing::warn!("content index {} is locked: read-only", dir.display());
                None
            }
            Err(e) => return Err(e.into()),
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            reader,
            writer: RwLock::new(writer),
            f,
            last_commit: Mutex::new(Instant::now()),
            uncommitted: AtomicU64::new(0),
        })
    }

    fn add(&self, doc: TantivyDocument) -> Result<(), SearchError> {
        let w = self.writer.read();
        let w = w.as_ref().ok_or(SearchError::ReadOnly)?;
        w.add_document(doc)?;
        self.uncommitted.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Removes everything indexed for an object (call before re-indexing
    /// it; takes effect at the next commit, before later additions).
    pub fn delete_object(&self, obj: ObjRef) -> Result<(), SearchError> {
        let w = self.writer.read();
        let w = w.as_ref().ok_or(SearchError::ReadOnly)?;
        w.delete_term(Term::from_field_i64(self.f.obj, obj.key()));
        Ok(())
    }

    /// Removes a data source (Autopsy `deleteDataSource`).
    pub fn delete_data_source(&self, ds_id: i64) -> Result<(), SearchError> {
        {
            let w = self.writer.read();
            let w = w.as_ref().ok_or(SearchError::ReadOnly)?;
            w.delete_term(Term::from_field_i64(self.f.ds, ds_id));
        }
        self.commit()
    }

    /// Indexes one chunk.
    pub fn add_chunk(&self, c: &ChunkDoc<'_>) -> Result<(), SearchError> {
        let mut d = TantivyDocument::default();
        d.add_i64(self.f.obj, c.obj.key());
        d.add_u64(self.f.page, u64::from(c.page));
        d.add_i64(self.f.ds, c.ds_id);
        d.add_text(self.f.text, c.text);
        d.add_text(self.f.tri, c.text);
        d.add_text(self.f.body, c.text);
        d.add_u64(self.f.base, c.base as u64);
        d.add_u64(self.f.base_lower, c.base_lower as u64);
        self.add(d)
    }

    /// Indexes an object's summary: its name (searchable like text) and
    /// chunk count. Metadata-only objects have zero chunks.
    pub fn add_summary(
        &self,
        obj: ObjRef,
        ds_id: i64,
        name: &str,
        chunks: u32,
    ) -> Result<(), SearchError> {
        let mut d = TantivyDocument::default();
        d.add_i64(self.f.obj, obj.key());
        d.add_u64(self.f.page, 0);
        d.add_i64(self.f.ds, ds_id);
        d.add_text(self.f.text, name);
        d.add_text(self.f.tri, name);
        d.add_text(self.f.body, name);
        d.add_text(self.f.name, name.to_lowercase());
        d.add_u64(self.f.base, name.len() as u64);
        d.add_u64(self.f.base_lower, name.to_lowercase().len() as u64);
        d.add_u64(self.f.chunks, u64::from(chunks));
        self.add(d)
    }

    /// Makes everything added so far searchable.
    pub fn commit(&self) -> Result<(), SearchError> {
        {
            let mut w = self.writer.write();
            if let Some(w) = w.as_mut() {
                w.commit()?;
            }
        }
        self.uncommitted.store(0, Ordering::Relaxed);
        *self.last_commit.lock() = Instant::now();
        self.reader.reload()?;
        Ok(())
    }

    /// Commits if there are uncommitted documents and the last commit
    /// was more than a minute ago (Autopsy flushes every 10 minutes).
    pub fn maybe_commit(&self) -> Result<(), SearchError> {
        if self.uncommitted.load(Ordering::Relaxed) > 0
            && self.last_commit.lock().elapsed() >= COMMIT_INTERVAL
        {
            self.commit()?;
        }
        Ok(())
    }

    /// Picks up commits made by other handles or processes.
    pub fn reload(&self) -> Result<(), SearchError> {
        self.reader.reload()?;
        Ok(())
    }

    /// Objects and chunks in the index.
    pub fn stats(&self) -> Result<IndexStats, SearchError> {
        let s = self.reader.searcher();
        let summaries = s.search(
            &TermQuery::new(
                Term::from_field_u64(self.f.page, 0),
                IndexRecordOption::Basic,
            ),
            &Count,
        )? as u64;
        Ok(IndexStats {
            objects: summaries,
            chunks: s.num_docs().saturating_sub(summaries),
        })
    }

    fn object_docs(&self, obj: ObjRef) -> Result<Vec<(u32, DocAddress)>, SearchError> {
        let s = self.reader.searcher();
        let q = TermQuery::new(
            Term::from_field_i64(self.f.obj, obj.key()),
            IndexRecordOption::Basic,
        );
        let found = s.search(&q, &Candidates::new(None, usize::MAX))?;
        Ok(found.objs.into_values().next().unwrap_or_default())
    }

    /// The number of chunks of an indexed object (`None`: not indexed).
    pub fn num_chunks(&self, obj: ObjRef) -> Result<Option<u32>, SearchError> {
        let s = self.reader.searcher();
        for (page, addr) in self.object_docs(obj)? {
            if page == 0 {
                let d: TantivyDocument = s.doc(addr)?;
                let n = d
                    .get_first(self.f.chunks)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                return Ok(Some(u32::try_from(n).unwrap_or(u32::MAX)));
            }
        }
        Ok(None)
    }

    /// The stored text of a page (0 = name).
    pub fn page(&self, obj: ObjRef, page: u32) -> Result<Option<PageText>, SearchError> {
        let s = self.reader.searcher();
        for (p, addr) in self.object_docs(obj)? {
            if p == page {
                let d: TantivyDocument = s.doc(addr)?;
                return Ok(Some(self.page_text(&d)));
            }
        }
        Ok(None)
    }

    fn page_text(&self, d: &TantivyDocument) -> PageText {
        let text = d
            .get_first(self.f.body)
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let base = d
            .get_first(self.f.base)
            .and_then(|v| v.as_u64())
            .map_or(text.len(), |b| usize::try_from(b).unwrap_or(usize::MAX))
            .min(text.len());
        PageText { text, base }
    }

    /// Candidate query for an ad-hoc search.
    fn candidate_query(&self, q: &TextQuery) -> Result<Box<dyn Query>, SearchError> {
        let base: Box<dyn Query> = match q.search_type {
            SearchType::Exact => {
                let mut terms: Vec<Term> = word_terms(&q.term)
                    .iter()
                    .map(|t| Term::from_field_text(self.f.text, t))
                    .collect();
                if terms.len() > 1 {
                    Box::new(PhraseQuery::new(terms))
                } else if let Some(t) = terms.pop() {
                    Box::new(TermQuery::new(t, IndexRecordOption::Basic))
                } else {
                    return Err(SearchError::InvalidQuery("the keyword has no words".into()));
                }
            }
            SearchType::Substring => {
                let plan = Plan::for_literal(&q.term);
                if plan == Plan::All {
                    // One or two characters: trigrams containing them,
                    // and names that short.
                    let esc = regex::escape(&crate::tokenize::lower_chars(&q.term));
                    let pattern = format!(".*{esc}.*");
                    Box::new(BooleanQuery::union(vec![
                        Box::new(RegexQuery::from_pattern(&pattern, self.f.tri)?),
                        Box::new(RegexQuery::from_pattern(&pattern, self.f.name)?),
                    ]))
                } else {
                    self.plan_query(&plan)
                }
            }
            SearchType::Regex => {
                let plan = Plan::for_regex(&crate::keywords::java_regex(&q.term));
                self.plan_query(&plan)
            }
        };
        if q.ds_ids.is_empty() {
            return Ok(base);
        }
        let ds: Vec<Box<dyn Query>> = q
            .ds_ids
            .iter()
            .map(|&d| {
                Box::new(TermQuery::new(
                    Term::from_field_i64(self.f.ds, d),
                    IndexRecordOption::Basic,
                )) as Box<dyn Query>
            })
            .collect();
        Ok(Box::new(BooleanQuery::new(vec![
            (Occur::Must, base),
            (Occur::Must, Box::new(BooleanQuery::union(ds))),
        ])))
    }

    fn plan_query(&self, plan: &Plan) -> Box<dyn Query> {
        match plan {
            Plan::All => Box::new(AllQuery),
            Plan::Trigram(t) => Box::new(TermQuery::new(
                Term::from_field_text(self.f.tri, t),
                IndexRecordOption::Basic,
            )),
            Plan::And(v) => Box::new(BooleanQuery::intersection(
                v.iter().map(|p| self.plan_query(p)).collect(),
            )),
            Plan::Or(v) => Box::new(BooleanQuery::union(
                v.iter().map(|p| self.plan_query(p)).collect(),
            )),
        }
    }

    /// Ad-hoc search: objects with a verified hit, ascending by object
    /// key, after `after`. Stops early (returning `next`) when
    /// `cancelled` says so.
    pub fn search(
        &self,
        q: &TextQuery,
        after: Option<i64>,
        limit: usize,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<SearchPage, SearchError> {
        let matcher = Matcher::single(&q.term, q.search_type)
            .map_err(|e| SearchError::InvalidQuery(e.error))?;
        let query = self.candidate_query(q)?;
        let searcher = self.reader.searcher();
        let mut out = SearchPage::default();
        let mut cursor = after;
        let limit = limit.max(1);
        // Objects collected per pass; doubled while candidates keep
        // failing verification, so a scan is not quadratic.
        let mut batch = (limit * 4).max(64);
        loop {
            if cancelled() {
                out.next = cursor;
                return Ok(out);
            }
            let windowed = self.after_query(query.box_clone(), cursor);
            let found = searcher.search(&*windowed, &Candidates::new(cursor, batch))?;
            let objs: Vec<(i64, Vec<(u32, DocAddress)>)> = found.objs.into_iter().collect();
            if objs.is_empty() {
                out.next = None;
                return Ok(out);
            }
            let verified: Vec<Option<ObjectHit>> = objs
                .par_iter()
                .map(|(key, pages)| {
                    if cancelled() {
                        return None;
                    }
                    self.verify(&searcher, &matcher, *key, pages)
                })
                .collect();
            for ((key, _), hit) in objs.iter().zip(verified) {
                cursor = Some(*key);
                if let Some(h) = hit {
                    out.hits.push(h);
                    if out.hits.len() >= limit {
                        out.next = cursor;
                        return Ok(out);
                    }
                }
            }
            if !found.truncated {
                out.next = None;
                return Ok(out);
            }
            batch = (batch * 2).min(MAX_BATCH);
        }
    }

    fn after_query(&self, q: Box<dyn Query>, after: Option<i64>) -> Box<dyn Query> {
        match after {
            None => q,
            Some(a) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, q),
                (
                    Occur::Must,
                    Box::new(RangeQuery::new(
                        Bound::Excluded(Term::from_field_i64(self.f.obj, a)),
                        Bound::Unbounded,
                    )),
                ),
            ])),
        }
    }

    /// Verifies one object's candidate pages.
    fn verify(
        &self,
        searcher: &tantivy::Searcher,
        matcher: &Matcher,
        key: i64,
        pages: &[(u32, DocAddress)],
    ) -> Option<ObjectHit> {
        let mut first: Option<(u32, String)> = None;
        let mut terms: Vec<String> = Vec::new();
        let mut seen = BTreeSet::new();
        for &(page, addr) in pages {
            let Ok(d) = searcher.doc::<TantivyDocument>(addr) else {
                continue;
            };
            let hits = self.page_hits(&d, matcher);
            if let Some(h) = hits.first()
                && first.is_none()
            {
                first = Some((page, h.snippet.clone()));
            }
            for h in hits {
                if terms.len() < MAX_HITS_PER_OBJECT && seen.insert(h.hit.clone()) {
                    terms.push(h.hit);
                }
            }
        }
        let (page, preview) = first?;
        Some(ObjectHit {
            obj: ObjRef::from_key(key),
            page,
            preview,
            hits: terms,
        })
    }

    /// Hits on a stored page, leaving out those that start in the overlap
    /// window (the next page reports them; a last page has no overlap).
    fn page_hits(&self, d: &TantivyDocument, matcher: &Matcher) -> Vec<Hit> {
        let text = d
            .get_first(self.f.body)
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let lower = text.to_lowercase();
        let base = d
            .get_first(self.f.base_lower)
            .and_then(|v| v.as_u64())
            .map_or(lower.len(), |b| usize::try_from(b).unwrap_or(usize::MAX));
        let mut hits = Vec::new();
        matcher.search(&lower, Some(&lower), &mut hits);
        hits.retain(|h| h.start < base);
        hits
    }

    /// Every hit of `q` in an object, page by page, as ranges in the
    /// original text (for the Extracted Text viewer's hit navigation).
    pub fn object_hits(&self, obj: ObjRef, q: &TextQuery) -> Result<Vec<PageHits>, SearchError> {
        let matcher = Matcher::single(&q.term, q.search_type)
            .map_err(|e| SearchError::InvalidQuery(e.error))?;
        let searcher = self.reader.searcher();
        let docs = self.object_docs(obj)?;
        let mut out = Vec::new();
        for (page, addr) in docs {
            let d: TantivyDocument = searcher.doc(addr)?;
            let text = d
                .get_first(self.f.body)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let base = d
                .get_first(self.f.base_lower)
                .and_then(|v| v.as_u64())
                .map_or(usize::MAX, |b| usize::try_from(b).unwrap_or(usize::MAX));
            let (lower, map) = lower_with_map(text);
            let mut ranges = Vec::new();
            for e in 0..matcher.entries().len() {
                for h in matcher.occurrences(e, &lower) {
                    if h.start >= base {
                        continue;
                    }
                    ranges.push(map.to_original(h.start, h.end));
                }
            }
            if !ranges.is_empty() {
                ranges.sort_by_key(|r| (r.start, r.end));
                ranges.dedup();
                out.push(PageHits { page, ranges });
            }
        }
        Ok(out)
    }
}

/// Maps byte offsets of a lower-cased text back to the original.
struct OffsetMap {
    /// `(lower start, original start, original end)` per original char.
    chars: Vec<(usize, usize, usize)>,
    lower_len: usize,
    orig_len: usize,
}

impl OffsetMap {
    fn to_original(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        let find = |l: usize| {
            let i = self.chars.partition_point(|c| c.0 <= l);
            i.checked_sub(1).and_then(|i| self.chars.get(i))
        };
        let s = if start >= self.lower_len {
            self.orig_len
        } else {
            find(start).map_or(0, |c| c.1)
        };
        let e = if end == 0 {
            0
        } else {
            find(end - 1).map_or(self.orig_len, |c| c.2)
        };
        s..e.max(s)
    }
}

/// `text.to_lowercase()` and the offset map back to `text`. Lower-casing
/// a character never depends on its neighbours except for the Greek final
/// sigma, which has the same length either way.
fn lower_with_map(text: &str) -> (String, OffsetMap) {
    let lower = text.to_lowercase();
    let mut chars = Vec::with_capacity(text.len());
    let mut l = 0usize;
    for (o, c) in text.char_indices() {
        chars.push((l, o, o + c.len_utf8()));
        l += c.to_lowercase().map(char::len_utf8).sum::<usize>();
    }
    let map = OffsetMap {
        chars,
        lower_len: lower.len(),
        orig_len: text.len(),
    };
    (lower, map)
}

/// Collects candidate documents grouped by object: only the smallest
/// `max_objs` object keys after `after` are kept.
struct Candidates {
    after: Option<i64>,
    max_objs: usize,
}

impl Candidates {
    fn new(after: Option<i64>, max_objs: usize) -> Self {
        Self { after, max_objs }
    }
}

#[derive(Debug, Default)]
struct Found {
    objs: BTreeMap<i64, Vec<(u32, DocAddress)>>,
    /// Objects were dropped to respect the limit.
    truncated: bool,
}

impl Found {
    fn insert(&mut self, key: i64, page: u32, addr: DocAddress, max: usize) {
        if self.objs.len() >= max
            && !self.objs.contains_key(&key)
            && self.objs.last_key_value().is_some_and(|(k, _)| *k < key)
        {
            self.truncated = true;
            return;
        }
        self.objs.entry(key).or_default().push((page, addr));
        while self.objs.len() > max {
            self.objs.pop_last();
            self.truncated = true;
        }
    }
}

struct CandidateSegment {
    seg: SegmentOrdinal,
    obj: Column<i64>,
    page: Column<u64>,
    after: Option<i64>,
    max_objs: usize,
    found: Found,
}

impl Collector for Candidates {
    type Fruit = Found;
    type Child = CandidateSegment;

    fn for_segment(
        &self,
        seg: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<CandidateSegment> {
        let ff = reader.fast_fields();
        Ok(CandidateSegment {
            seg,
            obj: ff.i64("obj")?,
            page: ff.u64("page")?,
            after: self.after,
            max_objs: self.max_objs,
            found: Found::default(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Found>) -> tantivy::Result<Found> {
        let mut all = Found::default();
        for f in fruits {
            all.truncated |= f.truncated;
            for (k, pages) in f.objs {
                for (p, a) in pages {
                    all.insert(k, p, a, self.max_objs);
                }
            }
        }
        for pages in all.objs.values_mut() {
            pages.sort_by_key(|p| p.0);
        }
        Ok(all)
    }
}

impl SegmentCollector for CandidateSegment {
    type Fruit = Found;

    fn collect(&mut self, doc: DocId, _score: Score) {
        let Some(key) = self.obj.first(doc) else {
            return;
        };
        if self.after.is_some_and(|a| key <= a) {
            return;
        }
        let page = self
            .page
            .first(doc)
            .map_or(0, |p| u32::try_from(p).unwrap_or(u32::MAX));
        self.found
            .insert(key, page, DocAddress::new(self.seg, doc), self.max_objs);
    }

    fn harvest(self) -> Found {
        self.found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> (tempfile::TempDir, ContentIndex) {
        let dir = tempfile::tempdir().unwrap();
        let ix = ContentIndex::open(&dir.path().join("content")).unwrap();
        (dir, ix)
    }

    /// A chunk whose overlap window starts at `base` (`usize::MAX`: no
    /// overlap, the last chunk).
    fn chunk(ix: &ContentIndex, obj: ObjRef, ds: i64, page: u32, text: &str, base: usize) {
        let base = base.min(text.len());
        ix.add_chunk(&ChunkDoc {
            obj,
            ds_id: ds,
            page,
            text,
            base,
            base_lower: text.get(..base).unwrap().to_lowercase().len(),
        })
        .unwrap();
    }

    const ALL: usize = usize::MAX;

    fn q(term: &str, t: SearchType) -> TextQuery {
        TextQuery {
            term: term.into(),
            search_type: t,
            ds_ids: vec![],
        }
    }

    fn run(ix: &ContentIndex, query: &TextQuery) -> Vec<(i64, u32, Vec<String>)> {
        let page = ix.search(query, None, 100, &|| false).unwrap();
        page.hits
            .into_iter()
            .map(|h| (h.obj.key(), h.page, h.hits))
            .collect()
    }

    fn sample() -> (tempfile::TempDir, ContentIndex) {
        let (d, ix) = index();
        let f1 = ObjRef::File(1);
        chunk(
            &ix,
            f1,
            10,
            1,
            "The Quick brown fox. Contact: John.Doe@example.com",
            ALL,
        );
        chunk(
            &ix,
            f1,
            10,
            2,
            "jumps over the lazy dog; secret42 here",
            ALL,
        );
        ix.add_summary(f1, 10, "fox.txt", 2).unwrap();
        let f2 = ObjRef::File(2);
        chunk(&ix, f2, 11, 1, "Hello yellow fellow, no foxes", ALL);
        ix.add_summary(f2, 11, "notes.doc", 1).unwrap();
        let a = ObjRef::Artifact(7);
        chunk(&ix, a, 10, 1, "URL : http://fox.example/secret99", ALL);
        ix.add_summary(a, 10, "Web History_7", 1).unwrap();
        ix.add_summary(ObjRef::File(3), 10, "ab", 0).unwrap();
        ix.commit().unwrap();
        (d, ix)
    }

    #[test]
    fn exact_substring_regex() {
        let (_d, ix) = sample();
        // "fox.txt" and "fox.example" are single words (UAX #29, as in
        // Lucene), "foxes" is another word.
        assert_eq!(
            run(&ix, &q("fox", SearchType::Exact)),
            [(1, 1, vec!["fox".to_owned()])]
        );
        assert_eq!(
            run(&ix, &q("fox.txt", SearchType::Exact)),
            [(1, 0, vec!["fox.txt".to_owned()])]
        );
        assert_eq!(
            run(&ix, &q("lazy dog", SearchType::Exact)),
            [(1, 2, vec!["lazy dog".to_owned()])]
        );
        assert_eq!(
            run(&ix, &q("ell", SearchType::Substring)),
            [(
                2,
                1,
                vec!["hello".to_owned(), "yellow".to_owned(), "fellow".to_owned()]
            )]
        );
        assert_eq!(
            run(&ix, &q(r"secret\d+", SearchType::Regex)),
            [
                (-7, 1, vec!["secret99".to_owned()]),
                (1, 2, vec!["secret42".to_owned()])
            ]
        );
        // Unplannable regex scans everything.
        assert_eq!(run(&ix, &q(r"\d\d", SearchType::Regex)).len(), 2);
        // One and two character substrings.
        assert_eq!(run(&ix, &q("ab", SearchType::Substring)).len(), 1);
        assert!(run(&ix, &q("zz", SearchType::Substring)).is_empty());
        // Data-source filter.
        let mut only11 = q("o", SearchType::Substring);
        only11.ds_ids = vec![11];
        assert_eq!(run(&ix, &only11).len(), 1);
        assert!(
            ix.search(&q("(", SearchType::Regex), None, 10, &|| false)
                .is_err()
        );
    }

    #[test]
    fn keyset_paging_and_cancel() {
        let (_d, ix) = index();
        for i in 1..=50 {
            chunk(
                &ix,
                ObjRef::File(i),
                1,
                1,
                &format!("needle number {i}"),
                ALL,
            );
        }
        ix.commit().unwrap();
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let p = ix
                .search(&q("needle", SearchType::Exact), after, 7, &|| false)
                .unwrap();
            seen.extend(p.hits.iter().map(|h| h.obj.key()));
            match p.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(seen, (1..=50).collect::<Vec<_>>());
        let p = ix
            .search(&q("needle", SearchType::Exact), None, 7, &|| true)
            .unwrap();
        assert!(p.hits.is_empty());
        assert_eq!(p.next, None);
    }

    #[test]
    fn overlap_hits_belong_to_the_next_page() {
        let (_d, ix) = index();
        let f = ObjRef::File(5);
        // "needle" sits in page 1's overlap and again at page 2's start.
        chunk(&ix, f, 1, 1, "aaaa bbbb needle", 10);
        chunk(&ix, f, 1, 2, "needle cccc", ALL);
        ix.add_summary(f, 1, "x", 2).unwrap();
        ix.commit().unwrap();
        assert_eq!(
            run(&ix, &q("needle", SearchType::Exact)),
            [(5, 2, vec!["needle".to_owned()])]
        );
        let hits = ix.object_hits(f, &q("needle", SearchType::Exact)).unwrap();
        assert_eq!(
            hits,
            [PageHits {
                page: 2,
                ranges: std::iter::once(0..6).collect()
            }]
        );
    }

    #[test]
    fn pages_counts_and_deletes() {
        let (_d, ix) = sample();
        assert_eq!(ix.num_chunks(ObjRef::File(1)).unwrap(), Some(2));
        assert_eq!(ix.num_chunks(ObjRef::File(3)).unwrap(), Some(0));
        assert_eq!(ix.num_chunks(ObjRef::File(99)).unwrap(), None);
        let p = ix.page(ObjRef::File(1), 2).unwrap().unwrap();
        assert!(p.text.starts_with("jumps"));
        assert_eq!(
            ix.page(ObjRef::File(1), 0).unwrap().unwrap().text,
            "fox.txt"
        );
        assert_eq!(
            ix.stats().unwrap(),
            IndexStats {
                objects: 4,
                chunks: 4
            }
        );
        ix.delete_object(ObjRef::File(1)).unwrap();
        ix.commit().unwrap();
        assert_eq!(ix.num_chunks(ObjRef::File(1)).unwrap(), None);
        ix.delete_data_source(10).unwrap();
        assert_eq!(
            ix.stats().unwrap(),
            IndexStats {
                objects: 1,
                chunks: 1
            }
        );
    }

    #[test]
    fn highlight_ranges_map_to_original_text() {
        let (_d, ix) = index();
        let f = ObjRef::File(1);
        // 'İ' lower-cases to two characters: offsets shift.
        chunk(&ix, f, 1, 1, "İİ Needle and NEEDLE", ALL);
        ix.add_summary(f, 1, "n", 1).unwrap();
        ix.commit().unwrap();
        let hits = ix.object_hits(f, &q("needle", SearchType::Exact)).unwrap();
        let text = "İİ Needle and NEEDLE";
        let got: Vec<&str> = hits[0].ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(got, ["Needle", "NEEDLE"]);
    }

    #[test]
    fn queries_for_keyword_hits() {
        let q = TextQuery::for_keyword_hit("hello", Some(1), Some("ell"));
        assert_eq!((q.term.as_str(), q.search_type), ("ell", SearchType::Regex));
        let q = TextQuery::for_keyword_hit("hello", Some(1), None);
        assert_eq!(q.search_type, SearchType::Substring);
        let q = TextQuery::for_keyword_hit("jean", Some(0), Some(""));
        assert_eq!(
            (q.term.as_str(), q.search_type),
            ("jean", SearchType::Exact)
        );
    }

    #[test]
    fn reopen_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        {
            let ix = ContentIndex::open(dir.path()).unwrap();
            chunk(&ix, ObjRef::File(1), 1, 1, "persistent words", ALL);
            ix.commit().unwrap();
        }
        let ix = ContentIndex::open(dir.path()).unwrap();
        assert_eq!(run(&ix, &q("persistent", SearchType::Exact)).len(), 1);
    }
}
