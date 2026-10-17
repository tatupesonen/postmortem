//! "Autopsy Logical Imager Results" data sources (Autopsy
//! `LogicalImagerDSProcessor` / `AddLogicalImageTask`): the output folder
//! of the logical imager, which holds `SearchResults.txt` and either sparse
//! VHD images of the searched drives or, without them, the extracted files
//! under `root/`.
//!
//! As in Autopsy the folder is first copied into the case
//! (`ModuleOutput/LogicalImager/<folder>`); `SearchResults.txt` and every
//! `*_users.txt` become case reports. VHDs are added as disk images and
//! each search hit is looked up by its metadata address and name;
//! otherwise the extracted files are added as one logical file set under
//! `root/<image>/<parent path>` with the times the imager recorded. Every
//! hit gets an Interesting Item result in the "Logical Imager results" set,
//! its category the matching rule.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pm_casedb::catalog::{artifact_type_id, attribute_type_id};
use pm_casedb::{
    DataSourceKind, FileKindCode, NewArtifact, NewDataSource, NewFile, Origin, Priority,
    Significance, Value, default_host_name,
};

use crate::load::{LoadOptions, LoadProgress, LoadSummary, add_image};
use crate::{Case, CaseError, Result, io_err};

const SEARCH_RESULTS: &str = "SearchResults.txt";
const USERS_SUFFIX: &str = "_users.txt";
const MODULE: &str = "Logical Imager";
const SET_NAME: &str = "Logical Imager results";
const ROOT: &str = "root";
/// Fields of a `SearchResults.txt` line.
const FIELDS: usize = 14;

/// One line of `SearchResults.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) vhd: String,
    pub(crate) meta_addr: u64,
    pub(crate) rule_set: String,
    pub(crate) rule: String,
    pub(crate) name: String,
    pub(crate) parent_path: String,
    pub(crate) extracted: String,
    /// crtime, mtime, atime, ctime (Unix seconds; 0 unknown).
    pub(crate) times: [i64; 4],
}

/// Parses `SearchResults.txt` (tab-separated, a header line first).
pub(crate) fn parse_results(text: &str) -> Result<Vec<Hit>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate().skip(1) {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let [
            vhd,
            _fs_offset,
            meta,
            _status,
            rule_set,
            rule,
            _desc,
            name,
            parent,
            extracted,
            cr,
            m,
            a,
            c,
        ] = f.as_slice()
        else {
            return Err(CaseError::Invalid(format!(
                "{SEARCH_RESULTS} does not contain enough fields at line {}, got {}, expecting {FIELDS}",
                i + 1,
                f.len()
            )));
        };
        let num = |s: &str| s.trim().parse::<i64>().unwrap_or(0);
        out.push(Hit {
            vhd: (*vhd).to_owned(),
            meta_addr: meta.trim().parse().unwrap_or(0),
            rule_set: (*rule_set).to_owned(),
            rule: (*rule).to_owned(),
            name: (*name).to_owned(),
            parent_path: (*parent).to_owned(),
            extracted: (*extracted).to_owned(),
            times: [num(cr), num(m), num(a), num(c)],
        });
    }
    Ok(out)
}

/// Whether `folder` looks like logical imager output.
pub fn is_logical_imager_folder(folder: &Path) -> bool {
    folder.join(SEARCH_RESULTS).is_file()
}

/// Adds a logical imager output folder; returns every data source added
/// (one per VHD, or one logical file set).
pub fn add_logical_imager(
    case: &Case,
    src: &Path,
    load: &LoadOptions,
    progress: &LoadProgress,
) -> Result<Vec<LoadSummary>> {
    if !is_logical_imager_folder(src) {
        return Err(CaseError::Invalid(format!(
            "Cannot find {SEARCH_RESULTS} in {}",
            src.display()
        )));
    }
    let src_name = src.file_name().map_or_else(
        || "LogicalImager".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let base = case.module_output_dir().join("LogicalImager");
    let mut dest = base.join(&src_name);
    let mut n = 1;
    while dest.exists() {
        dest = base.join(format!("{src_name}_{n}"));
        n += 1;
    }
    progress.set_stage("Copying logical imager results");
    copy_dir(src, &dest, progress)?;
    if progress.is_cancelled() {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(CaseError::Cancelled);
    }
    let db = case.db();
    let now = jiff::Timestamp::now().as_second();
    let mut reports = vec![dest.join(SEARCH_RESULTS)];
    let mut vhds = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dest)
        .map_err(io_err(&dest))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.ends_with(USERS_SUFFIX) {
            reports.push(p);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vhd")) {
            vhds.push(p);
        }
    }
    for r in &reports {
        let name = r
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        db.add_report(
            &r.display().to_string(),
            "LogicalImager",
            &format!("{name} {src_name}"),
            now,
        )?;
    }
    let text =
        std::fs::read(dest.join(SEARCH_RESULTS)).map_err(io_err(dest.join(SEARCH_RESULTS)))?;
    let hits = parse_results(&String::from_utf8_lossy(&text))?;

    let mut added = Vec::new();
    let mut found: Vec<(i64, i64, &Hit)> = Vec::new();
    if vhds.is_empty() {
        let root = dest.join(ROOT);
        if !root.is_dir() {
            return Err(CaseError::Invalid(format!(
                "Directory {} does not contain any images",
                dest.display()
            )));
        }
        progress.set_stage("Adding extracted files");
        let (summary, ids) = add_extracted(case, &dest, &src_name, load, &hits, progress)?;
        found.extend(
            ids.into_iter()
                .zip(&hits)
                .map(|(id, h)| (summary.ds_id, id, h)),
        );
        added.push(summary);
    } else {
        let mut by_name: HashMap<String, i64> = HashMap::new();
        for vhd in &vhds {
            let summary = add_image(
                case,
                vhd,
                &LoadOptions {
                    name: None,
                    ..load.clone()
                },
                progress,
            )?;
            let name = vhd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            by_name.insert(name, summary.ds_id);
            added.push(summary);
        }
        progress.set_stage("Searching for interesting files");
        for h in &hits {
            let Some(&ds) = by_name.get(&h.vhd) else {
                if let Some(s) = added.first_mut() {
                    s.warnings
                        .push(format!("Cannot find a data source for {}", h.vhd));
                }
                continue;
            };
            for id in db.files_by_inode(ds, h.meta_addr, &h.name)? {
                found.push((ds, id, h));
            }
        }
    }
    progress.set_stage("Adding search results as interesting files");
    let (set_name, category) = (
        attribute_type_id("KAAMOS_SET_NAME").unwrap_or(0),
        attribute_type_id("KAAMOS_CATEGORY").unwrap_or(0),
    );
    db.bulk(|b| {
        for (ds_id, file_id, h) in &found {
            b.add_artifact(&NewArtifact {
                type_id: artifact_type_id("KAAMOS_INTERESTING_ITEM").unwrap_or(0),
                ds_id: *ds_id,
                file_id: Some(*file_id),
                source_artifact_id: None,
                score: Some((Significance::LikelyNotable, Priority::Normal)),
                conclusion: None,
                configuration: Some(SET_NAME.to_owned()),
                justification: None,
                created_at: now,
                attributes: vec![
                    (
                        set_name,
                        Value::Text(SET_NAME.to_owned()),
                        MODULE.to_owned(),
                    ),
                    (category, Value::Text(h.rule.clone()), MODULE.to_owned()),
                ],
            })?;
        }
        Ok(())
    })?;
    progress.set_stage("Done");
    Ok(added)
}

/// Adds the files extracted under `root/` as one logical file set; returns
/// it and each hit's file id, in order.
fn add_extracted(
    case: &Case,
    dest: &Path,
    name: &str,
    load: &LoadOptions,
    hits: &[Hit],
    progress: &LoadProgress,
) -> Result<(LoadSummary, Vec<i64>)> {
    let db = case.db();
    let mut total = 0u64;
    let result = db.bulk(|b| {
        let ds_id = b.add_data_source(&NewDataSource {
            name: name.to_owned(),
            kind: DataSourceKind::LogicalFiles,
            paths: vec![dest.display().to_string()],
            size: 0,
            sector_size: 0,
            timezone: load.timezone.clone(),
            image_format: None,
            md5: None,
            sha1: None,
            sha256: None,
            acquisition: Vec::new(),
            added_at: jiff::Timestamp::now().as_second(),
        })?;
        let host = load
            .host
            .clone()
            .unwrap_or_else(|| default_host_name(Some(name), ds_id));
        b.assign_host(ds_id, &host)?;
        // Virtual folders, by path ("root/<vhd>/<parent>").
        let mut dirs: HashMap<String, i64> = HashMap::new();
        let mut ids = Vec::with_capacity(hits.len());
        for h in hits {
            let parent = format!("{ROOT}/{}/{}", h.vhd, h.parent_path);
            let mut parent_id = None;
            let mut path = String::from("/");
            for seg in parent.split(['/', '\\']).filter(|s| !s.is_empty()) {
                let key = format!("{path}{seg}");
                let id = if let Some(&id) = dirs.get(&key) {
                    id
                } else {
                    let dir = NewFile::new(
                        ds_id,
                        parent_id,
                        &path,
                        seg,
                        Origin::Local,
                        FileKindCode::Directory,
                    );
                    let id = b.add_file(&dir)?;
                    dirs.insert(key.clone(), id);
                    id
                };
                parent_id = Some(id);
                path = format!("{key}/");
            }
            let local = dest.join(&h.extracted);
            let size = std::fs::metadata(&local).map_or(0, |m| m.len());
            total = total.saturating_add(size);
            let mut file = NewFile::new(
                ds_id,
                parent_id,
                &path,
                &h.name,
                Origin::Local,
                FileKindCode::Regular,
            );
            file.size = size;
            let known = |secs: i64| (secs > 0).then_some((secs, 0));
            let [crtime, mtime, atime, ctime] = h.times;
            file.times = [known(mtime), known(atime), known(ctime), known(crtime)];
            file.local_path = Some(local.display().to_string());
            ids.push(b.add_file(&file)?);
            progress
                .files
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok((ds_id, dirs.len(), ids))
    })?;
    let (ds_id, dir_count, ids) = result;
    db.set_data_source_size(ds_id, total)?;
    Ok((
        LoadSummary {
            ds_id,
            fs_ids: Vec::new(),
            files: (ids.len() + dir_count) as u64,
            warnings: Vec::new(),
        },
        ids,
    ))
}

fn copy_dir(src: &Path, dest: &Path, progress: &LoadProgress) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(io_err(dest))?;
    for e in std::fs::read_dir(src).map_err(io_err(src))? {
        if progress.is_cancelled() {
            return Ok(());
        }
        let e = e.map_err(io_err(src))?;
        let (from, to) = (e.path(), dest.join(e.file_name()));
        let ft = e.file_type().map_err(io_err(&from))?;
        if ft.is_dir() {
            copy_dir(&from, &to, progress)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to).map_err(io_err(&from))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_lines_parse() {
        let text = "VHD\tOffset\tMeta\tStatus\tSet\tRule\tDesc\tName\tParent\tExtracted\tcr\tm\ta\tc\n\
                    sparse_image.vhd\t32256\t1234\t0\tSet A\tDocs\td\tplan.docx\t/Users/bob\troot/plan.docx\t10\t20\t30\t40\n";
        let hits = parse_results(text).unwrap();
        assert_eq!(hits.len(), 1);
        let h = hits.first().unwrap();
        assert_eq!(h.meta_addr, 1234);
        assert_eq!(h.rule, "Docs");
        assert_eq!(h.times, [10, 20, 30, 40]);
        assert!(parse_results("h\nshort\tline\n").is_err());
    }
}
