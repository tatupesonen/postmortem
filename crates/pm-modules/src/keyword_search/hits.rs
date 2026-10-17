//! Turning inline hits into results (Autopsy `LuceneQuery` /
//! `RegexQuery.createKeywordHitArtifact` and `createCCNAccount`).
//!
//! One result per object, keyword and distinct hit text (the first hit
//! found; Autopsy's `InlineSearcher.makeArtifacts`). Re-running the job
//! posts nothing twice: the object's existing hits are read once, when
//! its first hit is found, and credit-card instances are de-duplicated by
//! the communications manager.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use pm_casedb::communications::AccountInstanceSpec;
use pm_casedb::{KeywordHitKey, Value};
use pm_ingest::{IngestMessage, JobContext, MessageKind};
use pm_search::keywords::{SEL_CARD, SearchType};
use pm_search::results::{KeywordHitSpec, keyword_hit_artifact};
use pm_search::{Hit, ObjRef, parse_track};

use super::{Job, NAME};
use crate::util::attr;

/// Inbox messages per hit term and job (Autopsy
/// `MAX_INBOX_NOTIFICATIONS_PER_KW_TERM`).
const MAX_MESSAGES_PER_TERM: u32 = 20;
/// Hit identities remembered per object before the set is reset (the
/// database check still prevents duplicates).
const MAX_SEEN: usize = 100_000;

/// Where hits of one object are posted.
#[derive(Debug, Clone)]
pub(super) struct Target {
    pub(super) obj: ObjRef,
    pub(super) ds_id: i64,
    /// The file the results belong to (for an artifact: its source file).
    pub(super) file_id: Option<i64>,
    /// Unallocated or unused blocks (credit-card hits record the page).
    pub(super) unallocated: bool,
    /// Path or label for inbox messages.
    pub(super) label: String,
}

/// Collects one object's hits and posts each new one.
pub(super) struct HitSink<'a> {
    job: &'a Job,
    ctx: &'a JobContext,
    target: Target,
    seen: HashSet<(usize, String)>,
    existing: Option<HashSet<KeywordHitKey>>,
    pub(super) posted: u64,
}

impl<'a> HitSink<'a> {
    pub(super) fn new(job: &'a Job, ctx: &'a JobContext, target: Target) -> Self {
        Self {
            job,
            ctx,
            target,
            seen: HashSet::new(),
            existing: None,
            posted: 0,
        }
    }

    /// Searches the object's name (Autopsy searches it as "chunk 0").
    pub(super) fn search_name(&mut self, name: &str) {
        let mut found = Vec::new();
        self.job.matcher.search(name, None, &mut found);
        for h in &found {
            self.record(h, 0);
        }
    }

    /// Handles one hit found on `page`.
    pub(super) fn record(&mut self, h: &Hit, page: u32) {
        if !self.seen.insert((h.entry, h.hit.clone())) {
            return;
        }
        if self.seen.len() > MAX_SEEN {
            self.seen.clear();
        }
        let Some(e) = self.job.matcher.entries().get(h.entry) else {
            return;
        };
        if e.selector.as_deref() == Some(SEL_CARD) {
            self.card_account(h, page, &e.list_name);
        } else {
            self.keyword_hit(h);
        }
    }

    fn keyword_hit(&mut self, h: &Hit) {
        let Some(e) = self.job.matcher.entries().get(h.entry) else {
            return;
        };
        let (artifact, key) = keyword_hit_artifact(&KeywordHitSpec {
            ds_id: self.target.ds_id,
            file_id: self.target.file_id,
            associated_artifact: self.associated_artifact(),
            list_name: &e.list_name,
            term: &e.term,
            search_type: e.search_type,
            selector: e.selector.as_deref(),
            hit: &h.hit,
            snippet: &h.snippet,
            module: NAME,
            created_at: self.ctx.now(),
        });
        if let Some(file_id) = self.target.file_id {
            let existing = self.existing.get_or_insert_with(|| {
                self.ctx
                    .case
                    .db()
                    .keyword_hit_keys(file_id)
                    .unwrap_or_else(|err| {
                        tracing::warn!("keyword hits of file {file_id}: {err}");
                        HashSet::new()
                    })
            });
            if !existing.insert(key) {
                return;
            }
        }
        self.ctx.post_artifact_unchecked(artifact);
        self.posted += 1;
        self.job.stats.hits.fetch_add(1, Ordering::Relaxed);
        self.notify(h, e.search_type, &e.term, &e.list_name, e.list);
    }

    fn associated_artifact(&self) -> Option<i64> {
        match self.target.obj {
            ObjRef::Artifact(id) => Some(id),
            ObjRef::File(_) => None,
        }
    }

    /// An inbox message for lists that ask for them (first 20 per term).
    fn notify(&self, h: &Hit, st: SearchType, term: &str, list_name: &str, list: usize) {
        let wants = self
            .job
            .settings
            .lists
            .get(list)
            .is_some_and(|l| l.ingest_messages);
        if !wants {
            return;
        }
        {
            let mut n = self.job.notifications.lock();
            let c = n.entry(h.hit.to_lowercase()).or_insert(0);
            if *c >= MAX_MESSAGES_PER_TERM {
                return;
            }
            *c += 1;
        }
        let regex = st != SearchType::Exact;
        let subject = format!(
            "{}{}",
            if regex {
                "Reg Ex hit: "
            } else {
                "Keyword hit: "
            },
            h.hit
        );
        let mut details = format!(
            "Keyword: {}\nPreview: {}\nFile: {}\nList: {}",
            h.hit, h.snippet, self.target.label, list_name
        );
        if regex {
            details.push_str("\nReg Ex: ");
            details.push_str(term);
        }
        let mut msg = IngestMessage::new(MessageKind::Data, NAME, subject)
            .details(details)
            .key(h.hit.to_lowercase());
        if let Some(f) = self.target.file_id {
            msg = msg.file(f);
        }
        self.ctx.post_message(msg);
    }

    /// A credit-card account instance (no keyword hit), as Autopsy's
    /// `createCCNAccount`.
    fn card_account(&mut self, h: &Hit, page: u32, list_name: &str) {
        let Some(card) = parse_track(&h.snippet) else {
            tracing::debug!("no card number in snippet {:?}", h.snippet);
            return;
        };
        let Some(file_id) = self.target.file_id else {
            return;
        };
        let m = || NAME.to_owned();
        let text = |name: &str, v: &str| (attr(name), Value::Text(v.to_owned()), m());
        let mut extra = vec![
            text("TSK_KEYWORD", &card.raw_number),
            text("TSK_CARD_NUMBER", &card.number),
        ];
        for (name, v) in [
            ("TSK_CARD_EXPIRATION", &card.expiration),
            ("TSK_CARD_SERVICE_CODE", &card.service_code),
            ("TSK_CARD_DISCRETIONARY", &card.discretionary),
            ("TSK_CARD_LRC", &card.lrc),
            ("TSK_NAME_PERSON", &card.name),
        ] {
            if let Some(v) = v {
                extra.push(text(name, v));
            }
        }
        if let Some(bin) = self
            .job
            .bins
            .as_ref()
            .and_then(|b| b.lookup_card(&card.number))
        {
            for (name, v) in [
                ("TSK_CARD_SCHEME", &bin.scheme),
                ("TSK_CARD_TYPE", &bin.card_type),
                ("TSK_BRAND_NAME", &bin.brand),
                ("TSK_BANK_NAME", &bin.bank_name),
                ("TSK_PHONE_NUMBER", &bin.bank_phone),
                ("TSK_URL", &bin.bank_url),
                ("TSK_COUNTRY", &bin.country),
                ("TSK_CITY", &bin.bank_city),
            ] {
                if let Some(v) = v {
                    extra.push(text(name, v));
                }
            }
        }
        if self.target.unallocated {
            extra.push(text(
                "TSK_KEYWORD_SEARCH_DOCUMENT_ID",
                &pm_search::docid::document_id(self.target.obj, page),
            ));
        }
        if !list_name.trim().is_empty() {
            extra.push(text("TSK_SET_NAME", list_name));
        }
        extra.push(text("TSK_KEYWORD_PREVIEW", &h.snippet));
        if let Some(a) = self.associated_artifact() {
            extra.push((attr("TSK_ASSOCIATED_ARTIFACT"), Value::Int(a), m()));
        }
        extra.push((attr("TSK_KEYWORD_SEARCH_TYPE"), Value::Int(2), m()));
        let spec = AccountInstanceSpec {
            type_name: "CREDIT_CARD",
            id: &card.number,
            module: NAME,
            file_id,
            ds_id: self.target.ds_id,
            extra,
            now: self.ctx.now(),
        };
        match self.ctx.case.db().account_instance(spec) {
            Ok(_) => {
                self.posted += 1;
                self.job.stats.accounts.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => tracing::warn!("credit card account on file {file_id}: {e}"),
        }
    }
}
