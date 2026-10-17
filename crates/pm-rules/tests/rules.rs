#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

//! The rule language: every condition, inclusive/exclusive logic, the
//! set flags, XML import/export and the standard sets. Includes the rule
//! semantics of Autopsy's `IngestFileFiltersTest`.

use pm_rules::standard::{STANDARD_SET_FILES, merge, standard_sets, with_standard_sets};
use pm_rules::{
    Comparator, FileFacts, FileMeta, FileOrigin, FilesSet, MetaType, NameCondition, Rule,
    RulesError, SizeCondition, SizeUnit, TextMatch, java_equals_ignore_case, xml,
};
use proptest::prelude::*;

const NOW: i64 = 1_800_000_000;

fn file<'a>(parent: &'a str, name: &'a str, ext: &'a str) -> FileFacts<'a> {
    FileFacts {
        name,
        extension: ext,
        parent_path: parent,
        mime_type: None,
        size: 100,
        meta: FileMeta::Regular,
        origin: FileOrigin::Normal,
        known: false,
        crtime: 0,
        mtime: 0,
    }
}

fn rule(name: &str) -> Rule {
    Rule::all(name, MetaType::Files)
}

fn set(rules: Vec<Rule>) -> FilesSet {
    FilesSet {
        name: "S".into(),
        description: String::new(),
        ignore_known_files: false,
        ignore_unallocated_space: false,
        standard_set: false,
        version: 0,
        rules,
    }
}

fn member(s: &FilesSet, f: &FileFacts<'_>) -> Option<String> {
    s.compile().unwrap().member_rule(f, NOW).map(str::to_owned)
}

/// `IngestFileFiltersTest`: parent path (substring, case-insensitive,
/// `parent + "/"`), extension, full name, rule AND within a rule, OR
/// across rules, unallocated space.
#[test]
fn ingest_file_filters_semantics() {
    let dir1 = Rule {
        path: Some(TextMatch::Partial("dir1".into())),
        ..rule("testFileType")
    };
    let jpg = Rule {
        name_condition: Some(NameCondition::Extension(TextMatch::AnyOf(vec![
            "jpg".into(),
        ]))),
        ..rule("FindJpgExtention")
    };
    let jpg_in_dir1 = Rule {
        path: Some(TextMatch::Partial("dir1".into())),
        ..jpg.clone()
    };
    let in1 = file("/dir1/", "file.jpg", "jpg");
    let in1_doc = file("/dir1/", "file.docx", "docx");
    let in2 = file("/dir2/", "file.jpg", "jpg");
    let deep = file("/a/DIR1/b/", "x.txt", "txt");

    let s = set(vec![dir1.clone()]);
    assert_eq!(member(&s, &in1).as_deref(), Some("testFileType"));
    assert_eq!(member(&s, &in1_doc).as_deref(), Some("testFileType"));
    assert_eq!(member(&s, &deep).as_deref(), Some("testFileType"));
    assert_eq!(member(&s, &in2), None);

    // One rule: extension AND path.
    let s = set(vec![jpg_in_dir1]);
    assert!(member(&s, &in1).is_some());
    assert!(member(&s, &in1_doc).is_none());
    assert!(member(&s, &in2).is_none());

    // Two rules: OR.
    let s = set(vec![jpg.clone(), dir1]);
    assert!(member(&s, &in1).is_some());
    assert!(member(&s, &in1_doc).is_some());
    assert!(member(&s, &in2).is_some());
    assert!(member(&s, &file("/dir2/", "a.doc", "doc")).is_none());

    // Full name, case-insensitive.
    let s = set(vec![Rule {
        name_condition: Some(NameCondition::FullName(TextMatch::Exact(
            "file.docx".into(),
        ))),
        ..rule("FullName")
    }]);
    assert!(member(&s, &file("/x/", "FILE.DOCX", "docx")).is_some());
    assert!(member(&s, &file("/x/", "file.docx.bak", "bak")).is_none());

    // Unallocated space and slack are rejected when ignored.
    let mut s = set(vec![Rule::all("all", MetaType::All)]);
    let unalloc = FileFacts {
        origin: FileOrigin::Unallocated,
        ..file("/$Unalloc/", "Unalloc_1_0_100", "")
    };
    let slack = FileFacts {
        origin: FileOrigin::Slack,
        ..file("/", "a.txt-slack", "")
    };
    assert!(member(&s, &unalloc).is_some());
    s.ignore_unallocated_space = true;
    assert!(member(&s, &unalloc).is_none());
    assert!(member(&s, &slack).is_none());
    assert!(member(&s, &in1).is_some());
}

#[test]
fn meta_types() {
    let dir = FileFacts {
        meta: FileMeta::Directory,
        ..file("/", "d", "")
    };
    let vdir = FileFacts {
        meta: FileMeta::VirtualDirectory,
        ..file("/", "$OrphanFiles", "")
    };
    let link = FileFacts {
        meta: FileMeta::Other,
        ..file("/", "l", "")
    };
    let reg = file("/", "f", "");
    let named = |m| Rule {
        path: Some(TextMatch::Partial("/".into())),
        ..Rule::all("r", m)
    };
    let cases = [
        (MetaType::Files, [true, false, false, false]),
        (MetaType::Directories, [false, true, true, false]),
        (MetaType::FilesAndDirectories, [true, true, true, false]),
        (MetaType::All, [true, true, true, true]),
    ];
    for (m, want) in cases {
        let s = set(vec![named(m)]);
        let got = [&reg, &dir, &vdir, &link].map(|f| member(&s, f).is_some());
        assert_eq!(got, want, "{m:?}");
    }
}

#[test]
fn size_mime_date_regex_and_known() {
    let size = |c, v, u| Rule {
        size: Some(SizeCondition {
            comparator: c,
            unit: u,
            value: v,
        }),
        ..rule("size")
    };
    let mut f = file("/", "a", "");
    f.size = 2048;
    for (c, v, u, want) in [
        (Comparator::GreaterThan, 2, SizeUnit::Kilobytes, false),
        (Comparator::GreaterThanEqual, 2, SizeUnit::Kilobytes, true),
        (Comparator::Equal, 2048, SizeUnit::Bytes, true),
        (Comparator::LessThan, 1, SizeUnit::Megabytes, true),
        (Comparator::LessThanEqual, 2047, SizeUnit::Bytes, false),
    ] {
        assert_eq!(
            member(&set(vec![size(c, v, u)]), &f).is_some(),
            want,
            "{c:?} {v}"
        );
    }
    // MIME type: exact and case-sensitive.
    let mime = set(vec![Rule {
        mime_type: Some("image/jpeg".into()),
        ..rule("m")
    }]);
    f.mime_type = Some("image/jpeg");
    assert!(member(&mime, &f).is_some());
    f.mime_type = Some("IMAGE/JPEG");
    assert!(member(&mime, &f).is_none());
    f.mime_type = None;
    assert!(member(&mime, &f).is_none());
    // Created or modified within N days.
    let recent = set(vec![Rule {
        days_included: Some(7),
        ..rule("recent")
    }]);
    f.mtime = NOW - 6 * 86_400;
    assert!(member(&recent, &f).is_some());
    f.mtime = NOW - 8 * 86_400;
    f.crtime = NOW - 1;
    assert!(member(&recent, &f).is_some());
    f.crtime = 0;
    assert!(member(&recent, &f).is_none());
    // Regexes are found anywhere and are case-sensitive.
    let re = set(vec![Rule {
        name_condition: Some(NameCondition::FullName(TextMatch::Regex("crypt".into()))),
        ..rule("re")
    }]);
    assert!(member(&re, &file("/", "ecryptfs", "")).is_some());
    assert!(member(&re, &file("/", "ECRYPTFS", "")).is_none());
    // Known files are skipped when the set says so.
    let mut s = set(vec![rule("all")]);
    s.rules[0].path = Some(TextMatch::Partial("/".into()));
    let mut k = file("/", "kernel32.dll", "dll");
    k.known = true;
    assert!(member(&s, &k).is_some());
    s.ignore_known_files = true;
    assert!(member(&s, &k).is_none());
}

#[test]
fn exclusive_rules() {
    let txt = Rule {
        name_condition: Some(NameCondition::Extension(TextMatch::AnyOf(vec![
            "txt".into(),
        ]))),
        ..rule("txt")
    };
    let tmp_dir = Rule {
        path: Some(TextMatch::Partial("/tmp/".into())),
        exclusive: true,
        ..rule("not tmp")
    };
    let s = set(vec![txt, tmp_dir.clone()]);
    assert_eq!(
        member(&s, &file("/home/", "a.txt", "txt")).as_deref(),
        Some("txt")
    );
    assert_eq!(member(&s, &file("/tmp/", "a.txt", "txt")), None);
    // Only exclusive rules: everything else is "Not Excluded".
    let s = set(vec![tmp_dir]);
    assert_eq!(
        member(&s, &file("/home/", "a.bin", "bin")).as_deref(),
        Some("Not Excluded")
    );
    assert_eq!(member(&s, &file("/tmp/", "a.bin", "bin")), None);
    // No rules: never a member.
    assert_eq!(member(&set(vec![]), &file("/", "a", "")), None);
}

#[test]
fn text_matching_details() {
    assert!(java_equals_ignore_case("ÄBC", "äbc"));
    assert!(!java_equals_ignore_case("abc", "abcd"));
    // Java's CASE_INSENSITIVE substring matching is ASCII-only.
    let s = set(vec![Rule {
        path: Some(TextMatch::Partial("Ä".into())),
        ..rule("p")
    }]);
    assert!(member(&s, &file("/Ä/", "a", "")).is_some());
    assert!(member(&s, &file("/ä/", "a", "")).is_none());
    // The path gets "/" appended: "/dir1//" for files in /dir1/.
    let s = set(vec![Rule {
        path: Some(TextMatch::Regex("dir1//$".into())),
        ..rule("p")
    }]);
    assert!(member(&s, &file("/dir1/", "a", "")).is_some());
    // Invalid regexes fail compilation.
    let bad = set(vec![Rule {
        path: Some(TextMatch::Regex("(".into())),
        ..rule("bad")
    }]);
    assert!(matches!(bad.compile(), Err(RulesError::Regex(_, _))));
    assert_eq!(
        FilesSet {
            name: String::new(),
            ..set(vec![])
        }
        .compile()
        .unwrap_err(),
        RulesError::EmptySetName
    );
}

#[test]
fn xml_import_export() {
    let doc = r#"<?xml version="1.0" encoding="UTF-8"?>
<INTERESTING_FILE_SETS>
 <INTERESTING_FILE_SET name="Docs" description="d" ignoreKnown="TRUE" ingoreUnallocated="true" versionNumber="3">
  <EXTENSION name="office" typeFilter="file">.doc,docx</EXTENSION>
  <NAME name="readme" regex="false">README</NAME>
  <NAME name="re" regex="true" typeFilter="all" pathRegex="^/home/">.*\.key</NAME>
  <EXTENSION name="big" typeFilter="file" comparatorSymbol="&gt;=" sizeValue="10" sizeUnits="Megabytes"/>
  <EXTENSION name="new" daysIncluded="30" mimeType="image/png" pathFilter="pics" isExclusive="true"/>
  <EXTENSION name="dirs" typeFilter="dir" pathFilter="/Windows/"/>
 </INTERESTING_FILE_SET>
 <INTERESTING_FILE_SET name="Docs" description="duplicate, ignored"><NAME name="x">x</NAME></INTERESTING_FILE_SET>
 <INTERESTING_FILE_SET description="no name, ignored"><NAME name="x">x</NAME></INTERESTING_FILE_SET>
</INTERESTING_FILE_SETS>"#;
    let sets = xml::read(doc).unwrap();
    assert_eq!(sets.len(), 1);
    let s = &sets[0];
    assert!(s.ignore_known_files && s.ignore_unallocated_space && !s.standard_set);
    assert_eq!(s.version, 3);
    assert_eq!(s.rules.len(), 6);
    assert_eq!(
        s.rules[0].name_condition,
        Some(NameCondition::Extension(TextMatch::AnyOf(vec![
            "doc".into(),
            "docx".into()
        ])))
    );
    assert_eq!(s.rules[2].meta_type, MetaType::All);
    assert_eq!(s.rules[2].path, Some(TextMatch::Regex("^/home/".into())));
    assert_eq!(
        s.rules[3].size,
        Some(SizeCondition {
            comparator: Comparator::GreaterThanEqual,
            unit: SizeUnit::Megabytes,
            value: 10
        })
    );
    assert!(s.rules[4].exclusive);
    assert_eq!(s.rules[4].days_included, Some(30));
    assert_eq!(s.rules[5].meta_type, MetaType::Directories);
    assert_eq!(s.rules[2].display(), "re (.*\\.key)");
    assert_eq!(s.rules[3].display(), "big (≥ 10 Megabytes)");

    // Export, re-import: the same sets (rules sorted by name).
    let back = xml::read(&xml::write(&sets)).unwrap();
    let mut sorted = sets[0].clone();
    sorted.rules.sort_by_key(|r| r.name.to_lowercase());
    assert_eq!(back, vec![sorted]);

    // Invalid rules fail the import with Autopsy's messages.
    for (bad, msg) in [
        (
            r#"<NAME name="r">a:b</NAME>"#,
            "File name has illegal character of ':' in rule 'r'",
        ),
        (
            r#"<NAME name="r" regex="true">(</NAME>"#,
            "Error compiling 'regex' regex in rule 'r'",
        ),
        (
            r#"<NAME name="r" typeFilter="file"/>"#,
            "Invalid rule in files set, missing necessary conditions for 'r'",
        ),
        (
            r#"<NAME name="r" typeFilter="x">a</NAME>"#,
            "Files set is malformed for metatype condition, 'x', in rule 'r'",
        ),
        (
            r#"<NAME name="r" sizeValue="1">a</NAME>"#,
            "Files set is malformed missing at least one 'fileSize' attribute for rule 'r'",
        ),
        (
            r#"<NAME name="r" comparatorSymbol="!" sizeValue="1" sizeUnits="Bytes">a</NAME>"#,
            "Invalid comparator or size unit in files set for rule 'r'",
        ),
        (
            r#"<NAME name="r" comparatorSymbol="=" sizeValue="x" sizeUnits="Bytes">a</NAME>"#,
            "Non integer size in files set for rule 'r'",
        ),
        (
            r#"<NAME name="r" pathFilter="a" pathRegex="b">a</NAME>"#,
            "Error creating path condition for rule 'r'",
        ),
        (
            r#"<OTHER name="r" regex="true">a</OTHER>"#,
            "Name condition has invalid tag name of 'OTHER' for rule 'r'",
        ),
        (
            r#"<NAME name="r">*.exe</NAME>"#,
            "Error compiling 'regex' regex in rule 'r'",
        ),
    ] {
        let doc = format!(
            "<INTERESTING_FILE_SETS><INTERESTING_FILE_SET name=\"s\">{bad}</INTERESTING_FILE_SET></INTERESTING_FILE_SETS>"
        );
        assert_eq!(xml::read(&doc).unwrap_err().to_string(), msg, "{bad}");
    }
    assert!(matches!(xml::read("<a"), Err(RulesError::Xml(_))));
}

#[test]
fn standard_sets_load_and_merge() {
    let std = standard_sets();
    let names: Vec<&str> = std.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(STANDARD_SET_FILES.len(), 5);
    for n in [
        "Cloud Storage",
        "Cryptocurrency Wallets",
        "Encryption Programs",
        "Privacy Programs",
        "Remote Monitoring Management Programs",
    ] {
        assert!(names.contains(&n), "{n}: {names:?}");
    }
    assert!(std.iter().all(|s| s.standard_set && !s.rules.is_empty()));
    for s in &std {
        s.compile().unwrap();
    }
    // Encryption Programs flags KeePass by full name.
    let enc = std
        .iter()
        .find(|s| s.name == "Encryption Programs")
        .unwrap();
    let c = enc.compile().unwrap();
    let f = file("/Program Files/KeePass/", "KeePass.exe", "exe");
    assert_eq!(c.member_rule(&f, NOW), Some("KeePass"));

    // A user's set with a standard name is kept as "(Custom)".
    let mut mine = set(vec![rule("x")]);
    mine.name = "Encryption Programs".into();
    let all = with_standard_sets(vec![mine]);
    assert!(
        all.iter()
            .any(|s| s.name == "Encryption Programs (Custom)" && !s.standard_set)
    );
    assert!(
        all.iter()
            .any(|s| s.name == "Encryption Programs" && s.standard_set)
    );
    // A newer version replaces an older one; an older one does not.
    let mut dest = vec![FilesSet {
        version: 2,
        ..set(vec![])
    }];
    merge(
        vec![FilesSet {
            version: 1,
            ..set(vec![rule("old")])
        }],
        &mut dest,
        false,
    );
    assert!(dest[0].rules.is_empty());
    merge(
        vec![FilesSet {
            version: 3,
            ..set(vec![rule("new")])
        }],
        &mut dest,
        false,
    );
    assert_eq!(dest[0].rules[0].name, "new");
}

proptest! {
    /// Arbitrary XML-ish input never panics the reader.
    #[test]
    fn xml_reader_never_panics(s in "(<[A-Z_]{0,22}( [a-zA-Z]{1,12}=\"[a-z0-9*.,:<>=]{0,6}\"){0,3}/?>|[a-z.*,]{0,5}){0,20}") {
        let _ = xml::read(&s);
    }
}
