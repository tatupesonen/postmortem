//! Per-file processing: Autopsy's `KeywordSearchIngestModule.process` /
//! `searchFile` decision tree (inventory 05 §1.3), and the extraction →
//! chunk → search → index run shared with the artifact stage.

use pm_casedb::{FileKindCode, Known, Origin};
use pm_core::ReadAt;
use pm_ingest::{IngestFile, JobContext};
use pm_search::{ChunkDoc, ObjRef, SearchError};
use pm_text::{
    ARCHIVE_MIME_TYPES, Chunk, ChunkingSink, ExtractOptions, Kind, LimitSink, Metadata,
    OCTET_STREAM, Stop, TextError, TextSink, kind_for_mime, metadata_block, sniff_mime,
};

use super::hits::{HitSink, Target};
use super::metadata::metadata_attributes;
use super::{Job, NAME, Status};
use crate::util::Art;

/// Where a run's text comes from.
#[derive(Clone, Copy)]
pub(super) enum Source<'a> {
    /// A format extractor; its metadata block is appended.
    Extract(Kind),
    /// Autopsy `TextFileExtractor` fallback for `.txt` files.
    Plain,
    /// String extraction.
    Strings,
    /// Text built by the caller (artifacts).
    Text(&'a str),
}

/// Why a run produced nothing usable.
#[derive(Debug)]
pub(super) enum RunError {
    Text(TextError),
    Index(SearchError),
    /// A document format produced no text (Autopsy treats Tika's empty
    /// output as a failure).
    Empty,
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(e) => write!(f, "text extraction: {e}"),
            Self::Index(e) => write!(f, "keyword index: {e}"),
            Self::Empty => f.write_str("no text"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// Processes one file; returns its final status.
pub(super) fn process(job: &Job, ctx: &JobContext, file: &IngestFile) -> Status {
    let rec = &file.rec;
    if ctx.is_cancelled() {
        return Status::Skipped;
    }
    if rec.origin == Origin::VirtualDir || rec.kind == FileKindCode::Virtual {
        return Status::Skipped;
    }
    let obj = ObjRef::File(rec.id);
    let target = Target {
        obj,
        ds_id: rec.ds_id,
        file_id: Some(rec.id),
        unallocated: rec.origin == Origin::UnallocBlocks,
        label: rec.path(),
    };
    let mut hits = HitSink::new(job, ctx, target);
    let index_content = !(job.settings.skip_known && file.known() == Known::Known);

    // Unallocated space and carved text: strings, whatever the type.
    let carved_txt = rec.origin == Origin::Carved && rec.extension.eq_ignore_ascii_case("txt");
    if rec.origin == Origin::UnallocBlocks || carved_txt {
        return strings_only(job, ctx, file, &mut hits);
    }
    if !index_content || file.is_dir() || rec.size == 0 {
        return metadata_only(job, obj, rec.ds_id, &rec.name);
    }
    let mime = file.mime().map_or_else(
        || sniff_mime(file.head(), &rec.name).to_owned(),
        |m| m.trim().to_ascii_lowercase(),
    );
    if ARCHIVE_MIME_TYPES.contains(&mime.as_str()) {
        return metadata_only(job, obj, rec.ds_id, &rec.name);
    }
    let src = match file.content() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("keyword search: cannot read {}: {e}", rec.path());
            return Status::ErrorIo;
        }
    };
    hits.search_name(&rec.name);
    if mime == OCTET_STREAM {
        return strings_status(run(
            job,
            ctx,
            obj,
            rec.ds_id,
            &rec.name,
            &*src,
            Source::Strings,
            &mut hits,
            &ExtractOptions::default(),
        ));
    }
    let mut status = Status::ErrorText;
    let mut metadata: Option<Metadata> = None;
    if let Some(kind) = kind_for_mime(&mime, rec.size) {
        let opts = ExtractOptions {
            temp_dir: Some(ctx.case.temp_dir()),
            sqlite_wal: if kind == Kind::Sqlite {
                sqlite_wal(ctx, file)
            } else {
                None
            },
        };
        match run(
            job,
            ctx,
            obj,
            rec.ds_id,
            &rec.name,
            &*src,
            Source::Extract(kind),
            &mut hits,
            &opts,
        ) {
            Ok(meta) => {
                status = Status::Text;
                metadata = meta;
            }
            Err(RunError::Cancelled) => return Status::Skipped,
            Err(RunError::Index(e)) => {
                tracing::warn!("keyword index: {}: {e}", rec.path());
                status = Status::ErrorIndex;
            }
            Err(e) => tracing::debug!("{}: {e}", rec.path()),
        }
    }
    if status != Status::Text
        && rec.extension.eq_ignore_ascii_case("txt")
        && rec.origin != Origin::Carved
    {
        let opts = ExtractOptions::default();
        if run(
            job,
            ctx,
            obj,
            rec.ds_id,
            &rec.name,
            &*src,
            Source::Plain,
            &mut hits,
            &opts,
        )
        .is_ok()
        {
            status = Status::Text;
        }
    }
    if status != Status::Text {
        status = strings_status(run(
            job,
            ctx,
            obj,
            rec.ds_id,
            &rec.name,
            &*src,
            Source::Strings,
            &mut hits,
            &ExtractOptions::default(),
        ));
    }
    if let Some(m) = metadata {
        post_metadata(ctx, rec.id, &m);
    }
    status
}

fn strings_only<'a>(
    job: &'a Job,
    ctx: &'a JobContext,
    file: &IngestFile,
    hits: &mut HitSink<'a>,
) -> Status {
    let rec = &file.rec;
    let src = match file.content() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("keyword search: cannot read {}: {e}", rec.path());
            return Status::ErrorIo;
        }
    };
    hits.search_name(&rec.name);
    strings_status(run(
        job,
        ctx,
        ObjRef::File(rec.id),
        rec.ds_id,
        &rec.name,
        &*src,
        Source::Strings,
        hits,
        &ExtractOptions::default(),
    ))
}

fn strings_status(r: Result<Option<Metadata>, RunError>) -> Status {
    match r {
        Ok(_) => Status::Strings,
        Err(RunError::Cancelled) => Status::Skipped,
        Err(e) => {
            tracing::warn!("string extraction failed: {e}");
            Status::ErrorIndex
        }
    }
}

/// Autopsy `indexMetaDataOnly`: the name, no text, no search.
fn metadata_only(job: &Job, obj: ObjRef, ds_id: i64, name: &str) -> Status {
    let content = job.index.content();
    let r = content
        .delete_object(obj)
        .and_then(|()| content.add_summary(obj, ds_id, name, 0));
    match r {
        Ok(()) => Status::Metadata,
        Err(e) => {
            tracing::warn!("keyword index: {name}: {e}");
            Status::ErrorIndex
        }
    }
}

/// The SQLite write-ahead log next to a database (`<name>-wal`), which
/// Autopsy copies along so committed rows not yet checkpointed are seen.
fn sqlite_wal(ctx: &JobContext, file: &IngestFile) -> Option<pm_core::SharedSource> {
    let parent = file.rec.parent_id?;
    let wal = ctx
        .case
        .db()
        .child_named(parent, &format!("{}-wal", file.rec.name))
        .ok()??;
    ctx.case.content(&wal).ok()
}

/// `TSK_METADATA` on the file (de-duplicated on re-runs).
fn post_metadata(ctx: &JobContext, file_id: i64, meta: &Metadata) {
    let attrs = metadata_attributes(meta);
    if attrs.is_empty() {
        return;
    }
    let names: Vec<&str> = attrs.iter().map(|(n, _)| *n).collect();
    let mut a = Art::data(ctx, NAME, "TSK_METADATA", file_id);
    for (name, v) in attrs {
        a = a.value(name, v);
    }
    a.post(ctx, &names);
}

/// Extracts text, chunks it, searches every chunk and indexes it; then
/// adds the object's summary. Replaces anything indexed for the object
/// before. Returns the extractor's metadata.
#[allow(clippy::too_many_arguments, reason = "one call site per source kind")]
pub(super) fn run<'a>(
    job: &'a Job,
    ctx: &'a JobContext,
    obj: ObjRef,
    ds_id: i64,
    name: &str,
    src: &dyn ReadAt,
    source: Source<'_>,
    hits: &mut HitSink<'a>,
    opts: &ExtractOptions,
) -> Result<Option<Metadata>, RunError> {
    let content = job.index.content();
    content.delete_object(obj).map_err(RunError::Index)?;
    let document = matches!(source, Source::Extract(k) if k.is_document());
    let mut st = ChunkState {
        job,
        ctx,
        obj,
        ds_id,
        hits,
        page: 0,
        indexed: false,
        err: None,
        prev: None,
        keep_next: false,
    };
    let cancelled = || ctx.is_cancelled();
    let mut chunking = ChunkingSink::new(|c: Chunk| st.on_chunk(c));
    let outcome = {
        let mut limit = LimitSink::new(&mut chunking, &cancelled, 0);
        let r: Result<Option<Metadata>, TextError> = match source {
            Source::Extract(k) => pm_text::extract(k, src, &mut limit, opts).map(Some),
            Source::Plain => pm_text::extract(Kind::Plain, src, &mut limit, opts).map(|_| None),
            Source::Strings => {
                pm_text::strings::extract(src, &job.strings, &mut limit).map(|()| None)
            }
            Source::Text(t) => limit.push(t).map(|()| None).map_err(TextError::from),
        };
        match r {
            Ok(_) if document && limit.non_space == 0 => Err(None),
            Ok(meta) => {
                let pushed = match &meta {
                    Some(m) => limit.push(&metadata_block(m)),
                    None => Ok(()),
                };
                pushed.map(|()| meta).map_err(|Stop| None)
            }
            Err(e) => Err(Some(e)),
        }
    };
    let finished = match outcome {
        Ok(meta) => chunking.finish().map(|_| meta).map_err(|Stop| None),
        Err(e) => {
            drop(chunking);
            Err(e)
        }
    };
    let err = st.err.take();
    match finished {
        Ok(meta) => {
            if st.indexed {
                content
                    .add_summary(obj, ds_id, name, st.page)
                    .map_err(RunError::Index)?;
            }
            Ok(meta)
        }
        Err(e) => Err(if let Some(ie) = err {
            RunError::Index(ie)
        } else if ctx.is_cancelled() {
            RunError::Cancelled
        } else {
            match e {
                Some(te) => RunError::Text(te),
                None => RunError::Empty,
            }
        }),
    }
}

/// Per-run chunk handling.
struct ChunkState<'a, 'h> {
    job: &'a Job,
    ctx: &'a JobContext,
    obj: ObjRef,
    ds_id: i64,
    hits: &'h mut HitSink<'a>,
    /// Chunks seen (pages are numbered from 1).
    page: u32,
    /// Something was added to the index.
    indexed: bool,
    err: Option<SearchError>,
    /// Hits-only indexing: the previous chunk and whether it is indexed.
    prev: Option<(u32, Chunk, bool)>,
    /// Hits-only indexing: index the next chunk (it follows a hit).
    keep_next: bool,
}

impl ChunkState<'_, '_> {
    fn on_chunk(&mut self, c: Chunk) -> Result<(), Stop> {
        if self.ctx.is_cancelled() {
            return Err(Stop);
        }
        self.page += 1;
        let page = self.page;
        let mut found = Vec::new();
        self.job
            .matcher
            .search(&c.lower, Some(&c.lower), &mut found);
        let hit = !found.is_empty();
        for h in &found {
            self.hits.record(h, page);
        }
        if self.job.settings.index_text {
            return self.index(page, &c);
        }
        // Autopsy's "index only chunks with hits": a run of chunks with
        // hits is indexed with the chunk before and the chunk after it.
        let mut indexed = false;
        if hit {
            if let Some((p, prev, done)) = self.prev.take()
                && !done
            {
                self.index(p, &prev)?;
            }
            self.index(page, &c)?;
            indexed = true;
            self.keep_next = true;
        } else if self.keep_next {
            self.index(page, &c)?;
            indexed = true;
            self.keep_next = false;
        }
        self.prev = Some((page, c, indexed));
        Ok(())
    }

    fn index(&mut self, page: u32, c: &Chunk) -> Result<(), Stop> {
        let doc = ChunkDoc {
            obj: self.obj,
            ds_id: self.ds_id,
            page,
            text: &c.text,
            base: c.base_bytes,
            base_lower: c.base_lower_bytes,
        };
        match self.job.index.content().add_chunk(&doc) {
            Ok(()) => {
                self.indexed = true;
                Ok(())
            }
            Err(e) => {
                self.err = Some(e);
                Err(Stop)
            }
        }
    }
}
