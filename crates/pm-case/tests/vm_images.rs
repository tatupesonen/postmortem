//! End to end: the demo disk wrapped in virtual machine containers (VHD,
//! VHDX, VMDK built in Rust; VMDK streamOptimized and QCOW2 via `qemu-img`
//! when installed) is added as a data source and lists exactly the files of
//! the raw demo disk; a reopened case still reads content through the
//! container.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test code"
)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use pm_case::{Case, CaseDetails, Examiner, LoadOptions, LoadProgress, add_image};
use pm_casedb::{FileKindCode, FileRecord, Origin};
use pm_core::ReadAt;
use pm_testkit::vdisk::{VhdxOptions, vhd_dynamic, vhdx, vmdk_sparse};

fn details() -> CaseDetails {
    CaseDetails {
        name: "VM images".into(),
        number: "1".into(),
        examiner: Examiner::default(),
        notes: String::new(),
    }
}

fn all_files(case: &Case, ds_id: i64) -> Vec<FileRecord> {
    let q = pm_casedb::FileQuery {
        ds_id: Some(ds_id),
        ..Default::default()
    };
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = case.db().query_files(&q, after, 1000).unwrap();
        let Some(last) = page.last() else { break };
        after = last.id;
        out.extend(page);
    }
    out
}

/// `(path, size, kind)` of every file system file, ignoring ids.
fn listing(case: &Case, ds_id: i64) -> BTreeSet<(String, u64, String)> {
    all_files(case, ds_id)
        .into_iter()
        .filter(|f| f.origin == Origin::FileSystem)
        .map(|f| {
            (
                format!("{}{}", f.parent_path, f.name),
                f.size,
                format!("{:?}", f.kind),
            )
        })
        .collect()
}

fn add(case: &Case, path: &Path) -> i64 {
    add_image(
        case,
        path,
        &LoadOptions::default(),
        &LoadProgress::default(),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    .ds_id
}

#[test]
fn vm_containers_of_the_demo_disk_list_the_same_files() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = pm_testkit::demo::demo_disk();
    let ev = tmp.path().join("evidence");
    std::fs::create_dir(&ev).unwrap();
    let raw_path = ev.join("demo.dd");
    std::fs::write(&raw_path, &raw).unwrap();
    let images = vec![
        ("demo.vhd", vhd_dynamic(&raw, 2 << 20), "VHD"),
        ("demo.vhdx", vhdx(&raw, &VhdxOptions::default()), "VHDX"),
        ("demo.vmdk", vmdk_sparse(&raw, "demo.vmdk", None), "VMDK"),
    ];
    let qemu = Command::new("qemu-img")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let mut converted = Vec::new();
    if qemu {
        for (name, fmt, opts, label) in [
            (
                "demo-stream.vmdk",
                "vmdk",
                "subformat=streamOptimized",
                "VMDK",
            ),
            (
                "demo-split.vmdk",
                "vmdk",
                "subformat=twoGbMaxExtentSparse",
                "VMDK",
            ),
            ("demo.qcow2", "qcow2", "compat=1.1", "QCOW"),
        ] {
            let out = ev.join(name);
            let st = Command::new("qemu-img")
                .args(["convert", "-f", "raw", "-O", fmt, "-o", opts])
                .arg(&raw_path)
                .arg(&out)
                .status()
                .unwrap();
            assert!(st.success());
            converted.push((name, label));
        }
    } else {
        eprintln!("skipping qemu-img containers: qemu-img is not installed");
    }
    for (name, data, _) in &images {
        std::fs::write(ev.join(name), data).unwrap();
    }

    let case = Case::create(tmp.path().join("case"), details()).unwrap();
    let want = listing(&case, add(&case, &raw_path));
    assert!(want.len() > 10, "demo disk lists files");
    let mut added = Vec::new();
    for (name, label) in images
        .iter()
        .map(|(n, _, l)| (*n, *l))
        .chain(converted.iter().copied())
    {
        let ds = add(&case, &ev.join(name));
        assert_eq!(listing(&case, ds), want, "{name}");
        let rec = case
            .db()
            .data_sources()
            .unwrap()
            .into_iter()
            .find(|d| d.id == ds)
            .unwrap();
        assert!(
            rec.image_format
                .as_deref()
                .is_some_and(|f| f.starts_with(label)),
            "{name}: {:?}",
            rec.image_format
        );
        assert_eq!(rec.size, raw.len() as u64, "{name}");
        added.push((name, ds));
    }

    // Reopen: content is read back through the containers.
    let dir = case.dir().to_path_buf();
    drop(case);
    let case = Case::open(&dir).unwrap();
    for (name, ds) in added {
        let files = all_files(&case, ds);
        let f = files
            .iter()
            .filter(|f| {
                f.kind == FileKindCode::Regular && f.size > 0 && f.origin == Origin::FileSystem
            })
            .max_by_key(|f| f.size)
            .unwrap();
        let data = case.content(f).unwrap();
        let bytes = data
            .read_vec_at(0, usize::try_from(f.size).unwrap())
            .unwrap();
        assert_eq!(bytes.len() as u64, f.size, "{name}: {}", f.name);
    }
}
