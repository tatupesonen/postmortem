#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    reason = "benchmark code"
)]
//! Scale benchmarks (ADR-014), `#[ignore]`d; run with
//! `CARGO_BUILD_JOBS=2 cargo test -p pm-search --release --test bench -- --ignored --nocapture`.
//! Results go to `docs/PERF.md`. `PM_BENCH_NAMES` overrides the number of
//! names (default 10 000 000).

use std::time::Instant;

use pm_casedb::{FileKindCode, FileRecord, Known, Origin};
use pm_search::keywords::builtin_lists;
use pm_search::{
    ChunkDoc, ContentIndex, Keyword, KeywordList, Matcher, NameIndex, NameMatch, NameQuery, ObjRef,
    SearchType, TextQuery,
};

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const WORDS: &[&str] = &[
    "report", "invoice", "photo", "backup", "setup", "readme", "notes", "draft", "final", "budget",
    "scan", "img", "dsc", "document", "letter", "contract", "log", "cache", "temp", "data",
    "export", "summary", "plan", "meeting", "minutes", "agenda", "secret", "private",
];
const EXTS: &[&str] = &[
    "txt", "docx", "pdf", "jpg", "png", "exe", "dll", "log", "xml", "html", "zip", "dat", "",
];

fn record(i: i64, rng: &mut Rng) -> FileRecord {
    let w1 = WORDS[rng.below(WORDS.len() as u64) as usize];
    let w2 = WORDS[rng.below(WORDS.len() as u64) as usize];
    let ext = EXTS[rng.below(EXTS.len() as u64) as usize];
    let name = if ext.is_empty() {
        format!("{w1}_{w2}_{i}")
    } else {
        format!("{w1}_{w2}_{i}.{ext}")
    };
    let parent = format!(
        "/Users/user{}/{}/{}/",
        i % 97,
        WORDS[(i % WORDS.len() as i64) as usize],
        i % 1013
    );
    FileRecord {
        id: i,
        ds_id: 1 + i % 3,
        fs_id: None,
        parent_id: None,
        origin: Origin::FileSystem,
        inode: None,
        stream: None,
        name,
        parent_path: parent,
        kind: if ext.is_empty() {
            FileKindCode::Directory
        } else {
            FileKindCode::Regular
        },
        name_alloc: i % 17 != 0,
        meta_alloc: true,
        meta_flags: 0,
        size: rng.below(10_000_000),
        times: [None; 4],
        mode: None,
        uid: None,
        gid: None,
        seq: None,
        attributes: None,
        extension: ext.into(),
        mime: None,
        md5: None,
        sha1: None,
        sha256: None,
        known: Known::Unknown,
        derived_from: None,
        local_path: None,
    }
}

fn dir_size(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p).map_or(0, |rd| {
        rd.flatten()
            .map(|e| e.metadata().map_or(0, |m| m.len()))
            .sum()
    })
}

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    eprintln!("{label}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    r
}

#[test]
#[ignore = "benchmark: 10 M names, several minutes"]
fn name_index_scale() {
    let n: i64 = std::env::var("PM_BENCH_NAMES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000_000);
    let dir = tempfile::tempdir().unwrap();
    let ix = NameIndex::open(dir.path()).unwrap();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let t = Instant::now();
    let mut batch = Vec::with_capacity(100_000);
    for i in 1..=n {
        batch.push(record(i, &mut rng));
        if batch.len() == 100_000 {
            ix.add_new(&batch).unwrap();
            batch.clear();
        }
    }
    ix.add_new(&batch).unwrap();
    ix.commit().unwrap();
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "indexed {n} names in {secs:.1} s ({:.0} names/s), index {:.0} MB",
        n as f64 / secs,
        dir_size(dir.path()) as f64 / 1e6
    );
    assert_eq!(ix.len(), u64::try_from(n).unwrap());
    let q = |text: &str, mode: NameMatch, in_path: bool| NameQuery {
        text: text.into(),
        mode,
        in_path,
        ..NameQuery::default()
    };
    let probe = format!("_{}.", n / 2);
    for (label, query) in [
        ("rare substring", q(&probe, NameMatch::Substring, false)),
        (
            "common substring 'secret_'",
            q("secret_", NameMatch::Substring, false),
        ),
        (
            "prefix 'invoice_budget'",
            q("invoice_budget", NameMatch::Prefix, false),
        ),
        (
            "exact name",
            q(
                &format!("report_final_{}.txt", n / 3),
                NameMatch::Exact,
                false,
            ),
        ),
        ("two characters 'q_'", q("dq", NameMatch::Substring, false)),
        (
            "path substring 'user42/secret'",
            q("user42/secret", NameMatch::Substring, true),
        ),
        (
            "extension + not deleted + size",
            NameQuery {
                extensions: vec!["pdf".into()],
                exclude_deleted: true,
                min_size: Some(1_000_000),
                ..NameQuery::default()
            },
        ),
    ] {
        let page = time(&format!("{label}: first page of 100"), || {
            ix.search(&query, None, 100).unwrap()
        });
        if let Some(next) = page.next {
            time(&format!("{label}: next page"), || {
                ix.search(&query, Some(next), 100).unwrap()
            });
        }
        let count = time(&format!("{label}: count"), || ix.count(&query).unwrap());
        eprintln!("  {label}: {count} matches");
    }
}

/// Text with a Zipf-like word distribution.
fn text(rng: &mut Rng, bytes: usize, vocab: &[String]) -> String {
    let mut s = String::with_capacity(bytes + 32);
    while s.len() < bytes {
        // Squaring the uniform draw favours small indexes.
        let r = rng.below(1 << 20) as f64 / f64::from(1u32 << 20);
        let i = ((r * r) * vocab.len() as f64) as usize;
        s.push_str(&vocab[i.min(vocab.len() - 1)]);
        s.push(if rng.below(12) == 0 { '\n' } else { ' ' });
    }
    s
}

#[test]
#[ignore = "benchmark: content index throughput"]
fn content_index_throughput() {
    let mut rng = Rng(42);
    let vocab: Vec<String> = (0..50_000)
        .map(|i| {
            let len = 3 + rng.below(8) as usize;
            (0..len)
                .map(|k| char::from(b'a' + ((i * 7 + k as u64 * 13 + rng.below(26)) % 26) as u8))
                .collect::<String>()
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let ix = ContentIndex::open(dir.path()).unwrap();
    let chunk = 31 * 1024;
    let total_mb = 256;
    let chunks = total_mb * 1024 * 1024 / chunk;
    let pages: Vec<String> = (0..64).map(|_| text(&mut rng, chunk, &vocab)).collect();
    let t = Instant::now();
    for i in 0..chunks {
        let body = &pages[i % pages.len()];
        let obj = ObjRef::File(1 + (i / 8) as i64);
        ix.add_chunk(&ChunkDoc {
            obj,
            ds_id: 1,
            page: (i % 8 + 1) as u32,
            text: body,
            base: body.len(),
            base_lower: body.len(),
        })
        .unwrap();
        if i % 8 == 7 {
            ix.add_summary(obj, 1, &format!("file{i}.txt"), 8).unwrap();
        }
    }
    ix.commit().unwrap();
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "indexed {total_mb} MB ({chunks} chunks) in {secs:.1} s: {:.1} MB/s, index {:.0} MB",
        total_mb as f64 / secs,
        dir_size(dir.path()) as f64 / 1e6
    );
    let w = &vocab[10];
    let w2 = &vocab[40_000];
    for (label, term, st) in [
        ("exact common word", w.clone(), SearchType::Exact),
        ("exact rare word", w2.clone(), SearchType::Exact),
        ("substring", w2[1..].to_owned(), SearchType::Substring),
        (
            "regex with literal",
            format!("{w2}[a-z]*"),
            SearchType::Regex,
        ),
    ] {
        let q = TextQuery {
            term,
            search_type: st,
            ds_ids: vec![],
        };
        let page = time(&format!("{label}: first 100 objects"), || {
            ix.search(&q, None, 100, &|| false).unwrap()
        });
        eprintln!("  {label}: {} objects on the first page", page.hits.len());
    }
}

#[test]
#[ignore = "benchmark: inline matcher throughput"]
fn matcher_throughput() {
    let mut rng = Rng(7);
    let vocab: Vec<String> = (0..20_000).map(|i| format!("w{i:x}")).collect();
    let mut lists = builtin_lists();
    lists.push(KeywordList::new(
        "user",
        (0..100)
            .map(|i| {
                Keyword::new(
                    &format!("keyword{i}"),
                    if i % 3 == 0 {
                        SearchType::Substring
                    } else {
                        SearchType::Exact
                    },
                )
            })
            .chain([Keyword::new(r"secret[0-9]{4}", SearchType::Regex)])
            .collect(),
    ));
    let (m, bad) = Matcher::new(&lists);
    assert!(bad.is_empty());
    let pages: Vec<String> = (0..32)
        .map(|_| {
            let mut t = text(&mut rng, 31 * 1024, &vocab);
            t.push_str(" keyword7 mail a.b@example.com 192.168.0.1 ");
            t.to_lowercase()
        })
        .collect();
    let total: usize = 128 * 1024 * 1024;
    let t = Instant::now();
    let mut done = 0;
    let mut hits = Vec::new();
    while done < total {
        for p in &pages {
            hits.clear();
            m.search(p, Some(p), &mut hits);
            done += p.len();
        }
    }
    let secs = t.elapsed().as_secs_f64();
    eprintln!(
        "matcher (5 built-in lists + 101 keywords): {:.1} MB/s",
        done as f64 / 1e6 / secs
    );
}
