#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]
//! End-to-end Keyword Search ingest on a generated FAT image.

use std::collections::BTreeSet;
use std::io::{Cursor, Write as _};
use std::sync::Arc;

use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{
    FileAnalysis, FileQuery, FileRecord, Known, NewArtifact, Origin, Significance, Value,
};
use pm_ingest::{
    Inbox, IngestError, IngestProgress, JobSettings, MessageKind, ModuleFactory, ModuleSetting,
    run_job,
};
use pm_modules::keyword_search::{KeywordSearchFactory, KeywordSearchSettings};
use pm_search::keywords::{CREDIT_CARD_NUMBERS, EMAIL_ADDRESSES, PHONE_NUMBERS, builtin_lists};
use pm_search::{
    CaseIndex, Keyword, KeywordList, NameMatch, NameQuery, ObjRef, SearchType, TextQuery,
};

fn docx() -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let parts = [
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
        ),
        (
            "word/document.xml",
            "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>Uranium enrichment schedule</w:t></w:r></w:p></w:body></w:document>",
        ),
        (
            "docProps/core.xml",
            "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\"><dc:creator>Alice Author</dc:creator><cp:lastModifiedBy>Bob Editor</cp:lastModifiedBy><dcterms:created>2020-01-02T03:04:05Z</dcterms:created><dc:title>Plans</dc:title></cp:coreProperties>",
        ),
    ];
    for (name, body) in parts {
        z.start_file(name, opts).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn stored_zip(name: &str, body: &[u8]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    z.start_file(name, opts).unwrap();
    z.write_all(body).unwrap();
    z.finish().unwrap().into_inner()
}

/// A FAT32 volume with the test files.
fn image() -> Vec<u8> {
    let mut cur = Cursor::new(vec![0u8; 40 * 1024 * 1024]);
    fatfs::format_volume(
        &mut cur,
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32),
    )
    .unwrap();
    {
        let fs = fatfs::FileSystem::new(&mut cur, fatfs::FsOptions::new()).unwrap();
        let root = fs.root_dir();
        let put = |name: &str, data: &[u8]| {
            root.create_file(name).unwrap().write_all(data).unwrap();
        };
        put(
            "notes.txt",
            b"Meet Jean at the bomb site.\nMail john.doe@example.com or call (555) 123-4567 today.\n",
        );
        put(
            "page.html",
            b"<html><head><title>x</title></head><body><p>Molotov recipe</p><a href=\"http://evil.example/\">link</a></body></html>",
        );
        put("plans.docx", &docx());
        let mut bin = vec![0u8, 1, 2, 3, 0xFF, 0xFE];
        bin.extend_from_slice(b"card 4539747947839518654 here");
        bin.extend_from_slice(&[0, 0, 7, 8]);
        put("cards.bin", &bin);
        put(
            "archive.zip",
            &stored_zip("inner.txt", b"bomb inside an archive"),
        );
        put("known.txt", b"a known file that mentions the bomb\n");
        put("secret bomb plans.dat", &[0u8; 16]);
        put("empty.txt", b"");
        put("plain.txt", b"nothing of interest here\n");
        put("deleted.txt", b"nuclear launch codes in deleted space\n");
        root.remove("deleted.txt").unwrap();
        drop(root);
        fs.unmount().unwrap();
    }
    cur.into_inner()
}

struct Fixture {
    _dir: tempfile::TempDir,
    case: Arc<Case>,
    ds_id: i64,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("kws.dd");
        std::fs::write(&img, image()).unwrap();
        let case = Case::create(dir.path().join("case"), CaseDetails::default()).unwrap();
        let opts = LoadOptions {
            process_unallocated: true,
            ..LoadOptions::default()
        };
        let s = add_image(&case, &img, &opts, &LoadProgress::default()).unwrap();
        Self {
            _dir: dir,
            case: Arc::new(case),
            ds_id: s.ds_id,
        }
    }

    fn files(&self) -> Vec<FileRecord> {
        self.case
            .db()
            .query_files(
                &FileQuery {
                    ds_id: Some(self.ds_id),
                    ..FileQuery::default()
                },
                0,
                100_000,
            )
            .unwrap()
    }

    fn file(&self, name: &str) -> FileRecord {
        self.files()
            .into_iter()
            .find(|f| f.name == name && f.origin == Origin::FileSystem)
            .unwrap_or_else(|| panic!("{name}"))
    }

    fn run(&self, settings: &KeywordSearchSettings) -> Arc<Inbox> {
        let job = JobSettings {
            modules: vec![ModuleSetting {
                id: "keyword_search".into(),
                enabled: true,
                settings: serde_json::to_value(settings).unwrap(),
            }],
            ..JobSettings::default()
        };
        let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(KeywordSearchFactory)];
        let inbox = Arc::new(Inbox::default());
        run_job(
            &self.case,
            self.ds_id,
            &job,
            &modules,
            &Arc::new(IngestProgress::default()),
            &inbox,
        )
        .unwrap();
        inbox
    }

    /// `(file name, TSK_KEYWORD, TSK_SET_NAME, search type)` of every hit.
    fn hits(&self) -> BTreeSet<(String, String, String, i64)> {
        let db = self.case.db();
        let kwh = artifact_type_id("TSK_KEYWORD_HIT").unwrap();
        let mut out = BTreeSet::new();
        for a in db
            .artifacts_of_type(kwh, Some(self.ds_id), 0, 10_000)
            .unwrap()
        {
            let attrs = db.attributes(a.id).unwrap();
            let get = |n: &str| attrs.iter().find(|x| x.name == n).map(|x| x.value.clone());
            let text = |n: &str| match get(n) {
                Some(Value::Text(s)) => s,
                _ => String::new(),
            };
            let st = match get("TSK_KEYWORD_SEARCH_TYPE") {
                Some(Value::Int(i)) => i,
                _ => -1,
            };
            let file = a
                .file_id
                .and_then(|f| db.file(f).unwrap())
                .map_or_else(String::new, |f| f.name);
            assert_eq!(a.significance, Some(Significance::LikelyNotable));
            assert_eq!(
                a.configuration.as_deref(),
                Some(text("TSK_SET_NAME").as_str())
            );
            assert!(text("TSK_KEYWORD_PREVIEW").contains('\u{AB}'));
            out.insert((file, text("TSK_KEYWORD"), text("TSK_SET_NAME"), st));
        }
        out
    }

    fn count(&self, ty: &str) -> usize {
        self.case
            .db()
            .artifacts_of_type(artifact_type_id(ty).unwrap(), Some(self.ds_id), 0, 10_000)
            .unwrap()
            .len()
    }
}

fn settings() -> KeywordSearchSettings {
    let mut lists: Vec<KeywordList> = builtin_lists()
        .into_iter()
        .filter(|l| {
            [EMAIL_ADDRESSES, PHONE_NUMBERS, CREDIT_CARD_NUMBERS].contains(&l.name.as_str())
        })
        .collect();
    lists.push(KeywordList::new(
        "notable",
        vec![
            Keyword::new("bomb", SearchType::Exact),
            Keyword::new("Jean", SearchType::Exact),
            Keyword::new("molotov", SearchType::Exact),
            Keyword::new("nuclear", SearchType::Exact),
            Keyword::new("ranium", SearchType::Substring),
            Keyword::new("evil\\.[a-z]+", SearchType::Regex),
        ],
    ));
    KeywordSearchSettings {
        lists,
        ..KeywordSearchSettings::default()
    }
}

fn t(file: &str, kw: &str, set: &str, st: i64) -> (String, String, String, i64) {
    (file.into(), kw.into(), set.into(), st)
}

#[test]
fn keyword_search_end_to_end() {
    let fx = Fixture::new();
    // Mark one file known (as Hash Lookup would with NSRL).
    let known = fx.file("known.txt");
    fx.case
        .db()
        .set_file_analysis(
            known.id,
            &FileAnalysis {
                known: Some(Known::Known),
                ..FileAnalysis::default()
            },
        )
        .unwrap();
    // An artifact whose text mentions a keyword.
    let notes = fx.file("notes.txt");
    let bookmark = NewArtifact {
        type_id: artifact_type_id("TSK_WEB_BOOKMARK").unwrap(),
        ds_id: fx.ds_id,
        file_id: Some(notes.id),
        source_artifact_id: None,
        score: None,
        conclusion: None,
        configuration: None,
        justification: None,
        created_at: 0,
        attributes: vec![(
            attribute_type_id("TSK_TITLE").unwrap(),
            Value::Text("Molotov cocktails at home".into()),
            "test".into(),
        )],
    };
    let bookmark_id = fx.case.db().add_artifact(&bookmark).unwrap();

    let inbox = fx.run(&settings());
    let hits = fx.hits();
    let expected = [
        t("notes.txt", "bomb", "notable", 0),
        t("notes.txt", "jean", "notable", 0),
        t("notes.txt", "john.doe@example.com", EMAIL_ADDRESSES, 2),
        t("notes.txt", "(555) 123-4567", PHONE_NUMBERS, 2),
        t("page.html", "molotov", "notable", 0),
        t("page.html", "evil.example", "notable", 2),
        t("plans.docx", "uranium", "notable", 1),
        t("secret bomb plans.dat", "bomb", "notable", 0),
    ];
    for e in &expected {
        assert!(hits.contains(e), "missing {e:?} in {hits:#?}");
    }
    // Known files, archives and empty files are not searched.
    assert!(
        !hits
            .iter()
            .any(|h| h.0 == "known.txt" || h.0 == "archive.zip" || h.0 == "empty.txt"),
        "{hits:#?}"
    );
    // The deleted text is found in unallocated space (string extraction).
    let unalloc_hit = fx
        .case
        .db()
        .artifacts_of_type(
            artifact_type_id("TSK_KEYWORD_HIT").unwrap(),
            Some(fx.ds_id),
            0,
            10_000,
        )
        .unwrap()
        .into_iter()
        .any(|a| {
            let f = fx.case.db().file(a.file_id.unwrap()).unwrap().unwrap();
            f.origin == Origin::UnallocBlocks
                && fx
                    .case
                    .db()
                    .attributes(a.id)
                    .unwrap()
                    .iter()
                    .any(|x| x.value == Value::Text("nuclear".into()))
        });
    assert!(unalloc_hit, "{hits:#?}");
    // The artifact's text was searched: a hit with the associated artifact.
    let kwh = artifact_type_id("TSK_KEYWORD_HIT").unwrap();
    let assoc =
        fx.case
            .db()
            .artifacts_of_type(kwh, Some(fx.ds_id), 0, 10_000)
            .unwrap()
            .into_iter()
            .any(|a| {
                fx.case.db().attributes(a.id).unwrap().iter().any(|x| {
                    x.name == "TSK_ASSOCIATED_ARTIFACT" && x.value == Value::Int(bookmark_id)
                })
            });
    assert!(assoc);
    // Scores.
    assert_eq!(
        fx.case.db().file_score(notes.id).unwrap(),
        Significance::LikelyNotable
    );

    // Credit card: an account instance, no keyword hit.
    let cards = fx.file("cards.bin");
    assert!(!hits.iter().any(|h| h.0 == "cards.bin"));
    let accounts: Vec<_> = fx
        .case
        .db()
        .artifacts_for_file(cards.id)
        .unwrap()
        .into_iter()
        .filter(|a| a.type_id == artifact_type_id("TSK_ACCOUNT").unwrap())
        .collect();
    assert_eq!(accounts.len(), 1);
    let attrs = fx.case.db().attributes(accounts[0].id).unwrap();
    let has = |n: &str, v: &str| {
        attrs
            .iter()
            .any(|x| x.name == n && x.value == Value::Text(v.into()))
    };
    assert!(has("TSK_ACCOUNT_TYPE", "CREDIT_CARD"));
    assert!(has("TSK_ID", "4539747947839518654"));
    assert!(has("TSK_CARD_NUMBER", "4539747947839518654"));
    assert!(has("TSK_SET_NAME", CREDIT_CARD_NUMBERS));
    assert!(
        fx.case
            .db()
            .account("CREDIT_CARD", "4539747947839518654")
            .unwrap()
            .is_some()
    );

    // Metadata of the document.
    let docx = fx.file("plans.docx");
    let meta: Vec<_> = fx
        .case
        .db()
        .artifacts_for_file(docx.id)
        .unwrap()
        .into_iter()
        .filter(|a| a.type_id == artifact_type_id("TSK_METADATA").unwrap())
        .collect();
    assert_eq!(meta.len(), 1);
    let attrs = fx.case.db().attributes(meta[0].id).unwrap();
    let get = |n: &str| attrs.iter().find(|x| x.name == n).map(|x| x.value.clone());
    assert_eq!(get("TSK_OWNER"), Some(Value::Text("Alice Author".into())));
    assert_eq!(get("TSK_USER_ID"), Some(Value::Text("Bob Editor".into())));
    assert_eq!(get("TSK_DESCRIPTION"), Some(Value::Text("Plans".into())));
    assert_eq!(
        get("TSK_DATETIME_CREATED"),
        Some(Value::DateTime(1_577_934_245))
    );

    // The keyword index answers ad-hoc queries; the name index is current.
    let ix = CaseIndex::open(fx.case.dir()).unwrap();
    ix.content().reload().unwrap();
    let page = ix
        .content()
        .search(
            &TextQuery {
                term: "bomb".into(),
                search_type: SearchType::Exact,
                ds_ids: vec![fx.ds_id],
            },
            None,
            100,
            &|| false,
        )
        .unwrap();
    let objs: BTreeSet<ObjRef> = page.hits.iter().map(|h| h.obj).collect();
    assert!(objs.contains(&ObjRef::File(notes.id)));
    // Known files are indexed by name only.
    assert!(!objs.contains(&ObjRef::File(known.id)));
    assert_eq!(
        ix.content().num_chunks(ObjRef::File(known.id)).unwrap(),
        Some(0)
    );
    assert!(
        ix.content()
            .num_chunks(ObjRef::File(notes.id))
            .unwrap()
            .unwrap()
            >= 1
    );
    assert!(
        ix.content()
            .num_chunks(ObjRef::Artifact(bookmark_id))
            .unwrap()
            .is_some()
    );
    let names = ix
        .names()
        .search(
            &NameQuery {
                text: "plans".into(),
                mode: NameMatch::Substring,
                ..NameQuery::default()
            },
            None,
            10,
        )
        .unwrap();
    assert!(names.ids.contains(&docx.id));

    // Summary message.
    let msgs = inbox.since(0);
    let summary = msgs
        .iter()
        .find(|m| m.subject == "Keyword Indexing Results")
        .expect("summary");
    assert_eq!(summary.kind, MessageKind::Info);
    assert!(
        summary.details.contains("Files with known types"),
        "{}",
        summary.details
    );

    // Re-running adds nothing.
    let before = (
        fx.count("TSK_KEYWORD_HIT"),
        fx.count("TSK_ACCOUNT"),
        fx.count("TSK_METADATA"),
    );
    fx.run(&settings());
    let after = (
        fx.count("TSK_KEYWORD_HIT"),
        fx.count("TSK_ACCOUNT"),
        fx.count("TSK_METADATA"),
    );
    assert_eq!(before, after);
}

#[test]
fn hits_only_indexing() {
    let fx = Fixture::new();
    let s = KeywordSearchSettings {
        index_text: false,
        index_artifacts: false,
        ..settings()
    };
    let inbox = fx.run(&s);
    assert!(
        inbox
            .since(0)
            .iter()
            .any(|m| m.subject == "Text indexing is disabled.")
    );
    let ix = CaseIndex::open(fx.case.dir()).unwrap();
    ix.content().reload().unwrap();
    // A file with hits is indexed, one without hits is not.
    assert!(
        ix.content()
            .num_chunks(ObjRef::File(fx.file("notes.txt").id))
            .unwrap()
            .is_some()
    );
    assert!(
        ix.content()
            .num_chunks(ObjRef::File(fx.file("plain.txt").id))
            .unwrap()
            .is_none()
    );
    let hits = fx.hits();
    assert!(hits.contains(&t("notes.txt", "bomb", "notable", 0)));
}

#[test]
fn disabled_by_default() {
    let fx = Fixture::new();
    let modules: Vec<Arc<dyn ModuleFactory>> = vec![Arc::new(KeywordSearchFactory)];
    // With default settings the only module is off: nothing to run.
    let r = run_job(
        &fx.case,
        fx.ds_id,
        &JobSettings::default(),
        &modules,
        &Arc::new(IngestProgress::default()),
        &Arc::new(Inbox::default()),
    );
    assert!(matches!(r, Err(IngestError::NoModules)), "{r:?}");
    assert_eq!(fx.count("TSK_KEYWORD_HIT"), 0);
    assert!(
        pm_modules::core_modules()
            .iter()
            .any(|m| m.info().id == "keyword_search")
    );
}
