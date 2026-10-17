//! Derived files added during a job (Autopsy `addFilesToJob`): insertion,
//! high-priority re-injection, recursion, cancellation and termination.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "test code"
)]

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use pm_case::derived::{EncodedWriter, Encoding, OutputDir, XOR1_HEADER};
use pm_case::{Case, CaseDetails, LoadOptions, LoadProgress, add_image};
use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{FileKindCode, FileQuery, FileRecord, NewArtifact, NewFile, Origin, Value};
use pm_ingest::*;

fn demo_image() -> &'static PathBuf {
    static IMG: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &IMG.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("demo.dd");
        std::fs::write(&p, pm_testkit::demo::demo_disk()).unwrap();
        (dir, p)
    })
    .1
}

fn case() -> (tempfile::TempDir, Arc<Case>, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let case = Case::create(tmp.path().join("case"), CaseDetails::default()).unwrap();
    let s = add_image(
        &case,
        demo_image(),
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap();
    (tmp, Arc::new(case), s.ds_id)
}

fn all_files(case: &Case, ds_id: i64) -> Vec<FileRecord> {
    case.db()
        .query_files(
            &FileQuery {
                ds_id: Some(ds_id),
                ..FileQuery::default()
            },
            0,
            1_000_000,
        )
        .unwrap()
}

/// Two regular file-system files with content, used as "archives".
fn roots(case: &Case, ds_id: i64) -> Vec<FileRecord> {
    all_files(case, ds_id)
        .into_iter()
        .filter(|f| {
            f.origin == Origin::FileSystem
                && f.kind == FileKindCode::Regular
                && f.size > 0
                && FileFilter::default().accepts(f)
        })
        .take(2)
        .collect()
}

/// Writes `data` as an XOR1-encoded derived file and returns its row.
fn derived(ctx: &JobContext, parent: &FileRecord, source: i64, name: &str, data: &[u8]) -> NewFile {
    let out = OutputDir::new(ctx.case.dir(), "Test", source);
    let id = ctx.reserve_file_ids(1).start;
    let rel = out.relative(&id.to_string());
    let mut w = EncodedWriter::create(&out.absolute(&rel), Encoding::Xor1).unwrap();
    w.write_all(data).unwrap();
    let size = w.finish().unwrap();
    let mut f = NewFile::new(
        ctx.ds_id,
        Some(parent.id),
        format!("{}/", parent.path()),
        name,
        Origin::Derived,
        FileKindCode::Regular,
    );
    f.id = Some(id);
    f.size = size;
    f.derived_from = Some(source);
    f.local_path = Some(rel);
    f
}

/// Expands root files into `<root>/x.d/{a.bin, child.nest1}` and every
/// `child.nestK` (K < depth) into `{child.nest(K+1), leafK.bin}`.
#[derive(Default)]
struct Expander {
    roots: HashSet<i64>,
    depth: u32,
    /// Cancel the job after the first expansion.
    cancel: bool,
    /// Only expand roots without children (Autopsy re-extraction check).
    skip_existing: bool,
    /// Insert with `insert_derived_files` and schedule only regular files.
    defer: bool,
    added: Mutex<Vec<FileRecord>>,
}

struct ExpanderJob(Arc<Expander>);
struct ExpanderFile(Arc<Expander>);

struct ExpF(Arc<Expander>);

impl ModuleFactory for ExpF {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "expander",
            name: "Expander",
            description: "",
            version: "1",
            enabled_by_default: true,
        }
    }
    fn start(
        &self,
        _: &JobContext,
        _: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        Ok(Arc::new(ExpanderJob(Arc::clone(&self.0))))
    }
}

impl ModuleJob for ExpanderJob {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(ExpanderFile(Arc::clone(&self.0))))
    }
}

impl FileModule for ExpanderFile {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        let e = &self.0;
        let rec = &file.rec;
        let mut new = Vec::new();
        if e.roots.contains(&rec.id) {
            if e.skip_existing && ctx.case.db().has_children(rec.id)? {
                return Ok(());
            }
            let dir_id = ctx.reserve_file_ids(1).start;
            let mut dir = NewFile::new(
                ctx.ds_id,
                Some(rec.id),
                format!("{}/", rec.path()),
                "x.d",
                Origin::Derived,
                FileKindCode::Directory,
            );
            dir.id = Some(dir_id);
            dir.derived_from = Some(rec.id);
            let dir_rec = dir.to_record(dir_id);
            new.push(dir);
            new.push(derived(ctx, &dir_rec, rec.id, "a.bin", b"AAAA"));
            new.push(derived(ctx, &dir_rec, rec.id, "child.nest1", b"nest 1"));
        } else if let Some(k) = rec.name.strip_prefix("child.nest") {
            let k: u32 = k.parse().unwrap();
            if k < e.depth {
                let next = format!("child.nest{}", k + 1);
                new.push(derived(ctx, rec, rec.id, &next, next.as_bytes()));
                new.push(derived(ctx, rec, rec.id, &format!("leaf{k}.bin"), b"leaf"));
            }
        }
        if !new.is_empty() {
            if e.cancel {
                ctx.cancel_job();
            }
            let recs = if e.defer {
                let recs = ctx.insert_derived_files(new)?;
                ctx.add_files_to_job(
                    recs.iter()
                        .filter(|r| r.kind == FileKindCode::Regular)
                        .cloned()
                        .collect(),
                );
                recs
            } else {
                ctx.add_derived_files(new)?
            };
            e.added.lock().extend(recs);
        }
        Ok(())
    }
}

/// Records every processed file, checks derived content and tags derived
/// files with an artifact.
#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<(i64, Origin)>>,
    content: Mutex<HashMap<i64, Vec<u8>>>,
    /// Data-source module behaviour: `(stage, delay)` adding one derived
    /// file under `ds_root` after `delay`.
    ds: Option<(u32, Duration)>,
    ds_root: Mutex<Option<FileRecord>>,
    ds_added: Mutex<Vec<FileRecord>>,
}

struct RecorderJob(Arc<Recorder>);
struct RecorderFile(Arc<Recorder>);
struct RecorderDs(Arc<Recorder>);

struct RecF(Arc<Recorder>);

impl ModuleFactory for RecF {
    fn info(&self) -> ModuleInfo {
        ModuleInfo {
            id: "recorder",
            name: "Recorder",
            description: "",
            version: "1",
            enabled_by_default: true,
        }
    }
    fn start(
        &self,
        _: &JobContext,
        _: &serde_json::Value,
    ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
        Ok(Arc::new(RecorderJob(Arc::clone(&self.0))))
    }
}

impl ModuleJob for RecorderJob {
    fn file_module(&self) -> Option<Box<dyn FileModule>> {
        Some(Box::new(RecorderFile(Arc::clone(&self.0))))
    }
    fn data_source_module(&self) -> Option<(u32, Box<dyn DataSourceModule>)> {
        self.0.ds.map(|(stage, _)| {
            (
                stage,
                Box::new(RecorderDs(Arc::clone(&self.0))) as Box<dyn DataSourceModule>,
            )
        })
    }
}

impl FileModule for RecorderFile {
    fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
        self.0.seen.lock().push((file.rec.id, file.rec.origin));
        if file.rec.origin == Origin::Derived && file.is_file() {
            let src = file.content()?;
            let data = src.read_vec_at(0, usize::try_from(src.size()).unwrap())?;
            self.0.content.lock().insert(file.rec.id, data);
            let set = attribute_type_id("TSK_SET_NAME").unwrap();
            ctx.post_artifact(
                NewArtifact {
                    type_id: artifact_type_id("TSK_INTERESTING_FILE_HIT").unwrap(),
                    ds_id: ctx.ds_id,
                    file_id: Some(file.rec.id),
                    source_artifact_id: None,
                    score: None,
                    conclusion: None,
                    configuration: None,
                    justification: None,
                    created_at: ctx.now(),
                    attributes: vec![(set, Value::Text("Derived".into()), "rec".into())],
                },
                &[set],
            );
        }
        Ok(())
    }
}

impl DataSourceModule for RecorderDs {
    fn process(&mut self, ctx: &JobContext, _: &DsProgress<'_>) -> Result<(), ModuleError> {
        let (_, delay) = self.0.ds.unwrap();
        std::thread::sleep(delay);
        let root = self.0.ds_root.lock().clone().unwrap();
        let f = derived(ctx, &root, root.id, "from_ds.bin", b"ds");
        let recs = ctx.add_derived_files(vec![f])?;
        self.0.ds_added.lock().extend(recs);
        Ok(())
    }
}

fn run(
    case: &Arc<Case>,
    ds_id: i64,
    settings: &JobSettings,
    factories: Vec<Arc<dyn ModuleFactory>>,
) -> (JobSummary, Arc<IngestProgress>) {
    let progress = Arc::new(IngestProgress::default());
    let inbox = Arc::new(Inbox::default());
    let s = run_job(case, ds_id, settings, &factories, &progress, &inbox).unwrap();
    (s, progress)
}

fn settings() -> JobSettings {
    JobSettings {
        threads: 4,
        ..JobSettings::default()
    }
}

#[test]
fn derived_files_are_reinjected_recursively_exactly_once() {
    let (_t, case, ds_id) = case();
    let roots = roots(&case, ds_id);
    assert_eq!(roots.len(), 2);
    let before = all_files(&case, ds_id).len();
    let expander = Arc::new(Expander {
        roots: roots.iter().map(|r| r.id).collect(),
        depth: 4,
        ..Expander::default()
    });
    let recorder = Arc::new(Recorder::default());
    let (s, progress) = run(
        &case,
        ds_id,
        &settings(),
        vec![
            Arc::new(ExpF(Arc::clone(&expander))),
            Arc::new(RecF(Arc::clone(&recorder))),
        ],
    );
    assert!(!s.cancelled);
    assert!(s.errors.is_empty(), "{:?}", s.errors);

    // Per root: dir, a.bin, child.nest1, then (nest2, leaf1), (nest3,
    // leaf2), (nest4, leaf3).
    let added = expander.added.lock().clone();
    assert_eq!(added.len(), 2 * 9);
    let after = all_files(&case, ds_id);
    assert_eq!(after.len(), before + added.len());

    // Every derived file reached every module exactly once (the producer
    // did not stream them a second time).
    let seen = recorder.seen.lock().clone();
    let mut ids: Vec<i64> = seen.iter().map(|x| x.0).collect();
    ids.sort_unstable();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "a file was processed twice");
    for a in &added {
        assert!(ids.contains(&a.id), "{} not processed", a.path());
        // The in-memory record handed to modules matches the database.
        let db = case.db().file(a.id).unwrap().unwrap();
        assert_eq!(db.path(), a.path());
        assert_eq!(db.parent_id, a.parent_id);
        let parent = case.db().file(a.parent_id.unwrap()).unwrap().unwrap();
        assert_eq!(a.parent_path, format!("{}/", parent.path()));
    }
    let deepest = added
        .iter()
        .find(|f| f.name == "child.nest4")
        .expect("depth reached");
    assert!(
        deepest
            .path()
            .ends_with("/x.d/child.nest1/child.nest2/child.nest3/child.nest4")
    );

    // Content decodes from XOR1; the raw file carries TSK's header.
    let content = recorder.content.lock().clone();
    let a_bin = added.iter().find(|f| f.name == "a.bin").unwrap();
    assert_eq!(content[&a_bin.id], b"AAAA");
    assert_eq!(content[&deepest.id], b"child.nest4");
    let raw = std::fs::read(case.dir().join(a_bin.local_path.as_deref().unwrap())).unwrap();
    assert!(raw.starts_with(XOR1_HEADER));
    assert_eq!(raw.len(), 36);

    // Artifacts on derived files were saved (the rows existed in time).
    let derived_regular = added
        .iter()
        .filter(|f| f.kind == FileKindCode::Regular)
        .count();
    assert_eq!(s.artifacts, derived_regular as u64);
    for a in added.iter().filter(|f| f.kind == FileKindCode::Regular) {
        assert_eq!(case.db().artifacts_for_file(a.id).unwrap().len(), 1);
    }
    let snap = progress.snapshot();
    assert_eq!(snap.files_done, snap.files_total);
    assert_eq!(s.files, (before + added.len()) as u64);
}

/// `insert_derived_files` writes rows without scheduling them; the module
/// schedules what it wants with `add_files_to_job`.
#[test]
fn inserted_files_run_only_when_scheduled() {
    let (_t, case, ds_id) = case();
    let roots = roots(&case, ds_id);
    let expander = Arc::new(Expander {
        roots: roots.iter().map(|r| r.id).collect(),
        depth: 2,
        defer: true,
        ..Expander::default()
    });
    let recorder = Arc::new(Recorder::default());
    let (s, progress) = run(
        &case,
        ds_id,
        &settings(),
        vec![
            Arc::new(ExpF(Arc::clone(&expander))),
            Arc::new(RecF(Arc::clone(&recorder))),
        ],
    );
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    let added = expander.added.lock().clone();
    // Per root: dir, a.bin, child.nest1, then (nest2, leaf1).
    assert_eq!(added.len(), 2 * 5);
    let seen: Vec<i64> = recorder.seen.lock().iter().map(|x| x.0).collect();
    for a in &added {
        assert!(case.db().file(a.id).unwrap().is_some());
        let n = seen.iter().filter(|i| **i == a.id).count();
        let want = usize::from(a.kind == FileKindCode::Regular);
        assert_eq!(n, want, "{}", a.path());
    }
    let snap = progress.snapshot();
    assert_eq!(snap.files_done, snap.files_total);
}

#[test]
fn cancellation_stops_reinjection() {
    let (_t, case, ds_id) = case();
    let roots = roots(&case, ds_id);
    let expander = Arc::new(Expander {
        roots: roots.iter().map(|r| r.id).collect(),
        depth: 50,
        cancel: true,
        ..Expander::default()
    });
    let recorder = Arc::new(Recorder::default());
    let t = Instant::now();
    let (s, progress) = run(
        &case,
        ds_id,
        &settings(),
        vec![
            Arc::new(ExpF(Arc::clone(&expander))),
            Arc::new(RecF(Arc::clone(&recorder))),
        ],
    );
    assert!(s.cancelled);
    assert!(t.elapsed() < Duration::from_secs(20));
    assert_eq!(progress.snapshot().phase, Phase::Cancelled);
    // The files were inserted, but none was scheduled after cancelling.
    let added = expander.added.lock().clone();
    assert!(!added.is_empty());
    let seen: HashSet<i64> = recorder.seen.lock().iter().map(|x| x.0).collect();
    assert!(added.iter().all(|a| !seen.contains(&a.id)));
    assert!(
        added
            .iter()
            .all(|a| case.db().file(a.id).unwrap().is_some())
    );
}

/// The job waits for a stage-1 data-source module that adds files after
/// the file producer is done, and runs files a later stage adds.
#[test]
fn data_source_modules_can_add_files() {
    for (stage, delay) in [(1, Duration::from_millis(400)), (2, Duration::ZERO)] {
        let (_t, case, ds_id) = case();
        let root = roots(&case, ds_id).remove(0);
        let recorder = Arc::new(Recorder {
            ds: Some((stage, delay)),
            ..Recorder::default()
        });
        *recorder.ds_root.lock() = Some(root);
        let (s, _) = run(
            &case,
            ds_id,
            &settings(),
            vec![Arc::new(RecF(Arc::clone(&recorder)))],
        );
        assert!(s.errors.is_empty(), "{:?}", s.errors);
        let added = recorder.ds_added.lock().clone();
        assert_eq!(added.len(), 1);
        let seen: Vec<i64> = recorder.seen.lock().iter().map(|x| x.0).collect();
        assert_eq!(
            seen.iter().filter(|i| **i == added[0].id).count(),
            1,
            "stage {stage}"
        );
        assert_eq!(recorder.content.lock()[&added[0].id], b"ds");
    }
}

/// On re-ingest, derived files from the first run are streamed like any
/// other file (once), and the module does not add them again.
#[test]
fn reingest_processes_existing_derived_files_once() {
    let (_t, case, ds_id) = case();
    let roots: HashSet<i64> = roots(&case, ds_id).iter().map(|r| r.id).collect();
    let make = || {
        Arc::new(Expander {
            roots: roots.clone(),
            depth: 2,
            skip_existing: true,
            ..Expander::default()
        })
    };
    let first = make();
    run(
        &case,
        ds_id,
        &settings(),
        vec![Arc::new(ExpF(Arc::clone(&first)))],
    );
    let added = first.added.lock().clone();
    assert_eq!(added.len(), 2 * 5);

    let second = make();
    let recorder = Arc::new(Recorder::default());
    let (s, _) = run(
        &case,
        ds_id,
        &settings(),
        vec![
            Arc::new(ExpF(Arc::clone(&second))),
            Arc::new(RecF(Arc::clone(&recorder))),
        ],
    );
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    // Nested members of existing derived files are re-added by design of
    // the test module (it only checks roots), so count only root members.
    let seen: Vec<i64> = recorder.seen.lock().iter().map(|x| x.0).collect();
    for a in &added {
        assert_eq!(
            seen.iter().filter(|i| **i == a.id).count(),
            1,
            "{}",
            a.path()
        );
    }
}

#[test]
fn filtered_files_are_not_scheduled() {
    struct AddUnalloc(AtomicBool, Mutex<Option<i64>>);
    struct J(Arc<AddUnalloc>);
    struct F(Arc<AddUnalloc>);
    struct AddF(Arc<AddUnalloc>);

    impl ModuleFactory for AddF {
        fn info(&self) -> ModuleInfo {
            ModuleInfo {
                id: "add",
                name: "Add",
                description: "",
                version: "1",
                enabled_by_default: true,
            }
        }
        fn start(
            &self,
            _: &JobContext,
            _: &serde_json::Value,
        ) -> Result<Arc<dyn ModuleJob>, ModuleError> {
            Ok(Arc::new(J(Arc::clone(&self.0))))
        }
    }
    impl ModuleJob for J {
        fn file_module(&self) -> Option<Box<dyn FileModule>> {
            Some(Box::new(F(Arc::clone(&self.0))))
        }
    }
    impl FileModule for F {
        fn process(&mut self, ctx: &JobContext, file: &mut IngestFile) -> Result<(), ModuleError> {
            if file.rec.name == "carved.bin" {
                // Never scheduled: the filter skips unallocated space.
                self.0.0.store(true, Ordering::SeqCst);
            }
            if file.rec.origin == Origin::FileSystem && file.is_file() && self.0.1.lock().is_none()
            {
                let mut f = NewFile::new(
                    ctx.ds_id,
                    Some(file.rec.id),
                    format!("{}/", file.rec.path()),
                    "carved.bin",
                    Origin::UnallocBlocks,
                    FileKindCode::Regular,
                );
                f.layout = Some("[[0,512]]".into());
                f.size = 512;
                let r = ctx.add_derived_files(vec![f])?;
                *self.0.1.lock() = Some(r[0].id);
            }
            Ok(())
        }
    }
    let (_t, case, ds_id) = case();
    let m = Arc::new(AddUnalloc(AtomicBool::new(false), Mutex::new(None)));
    let settings = JobSettings {
        filter: FileFilter {
            ignore_unallocated: true,
            ignore_known: false,
        },
        ..settings()
    };
    let (s, _) = run(
        &case,
        ds_id,
        &settings,
        vec![Arc::new(AddF(Arc::clone(&m)))],
    );
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(m.1.lock().is_some());
    assert!(!m.0.load(Ordering::SeqCst));
}
