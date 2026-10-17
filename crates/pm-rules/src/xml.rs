//! Autopsy's files-set XML (`InterestingItemsFilesSetSettings`): the
//! `INTERESTING_FILE_SETS` documents its "Import Set" / "Export Set"
//! buttons and standard set files use.

use std::fmt::Write as _;

use crate::{
    Comparator, FilesSet, ILLEGAL_FILE_NAME_CHARS, MetaType, NameCondition, Rule, RulesError,
    SizeCondition, SizeUnit, TextMatch,
};

/// Java's `Boolean.parseBoolean`.
fn java_bool(s: &str) -> bool {
    s.eq_ignore_ascii_case("true")
}

/// `readDefinitionsXML`: every `INTERESTING_FILE_SET` element. A set
/// without a name or with an already-seen name is skipped (Autopsy logs
/// it); an invalid rule fails the whole import, as in Autopsy.
pub fn read(xml: &str) -> Result<Vec<FilesSet>, RulesError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| RulesError::Xml(e.to_string()))?;
    let mut sets: Vec<FilesSet> = Vec::new();
    for set in doc
        .root_element()
        .descendants()
        .filter(|n| n.has_tag_name("INTERESTING_FILE_SET"))
    {
        let attr = |k: &str| set.attribute(k).unwrap_or_default();
        let name = attr("name");
        if name.is_empty() {
            tracing::warn!("files set without a name ignored");
            continue;
        }
        if sets.iter().any(|s| s.name == name) {
            tracing::warn!("duplicate files set {name} ignored");
            continue;
        }
        let rules = set
            .children()
            .filter(roxmltree::Node::is_element)
            .map(read_rule)
            .collect::<Result<Vec<_>, _>>()?;
        sets.push(FilesSet {
            name: name.to_owned(),
            description: attr("description").to_owned(),
            ignore_known_files: java_bool(attr("ignoreKnown")),
            // Autopsy's attribute name (sic).
            ignore_unallocated_space: java_bool(attr("ingoreUnallocated")),
            standard_set: java_bool(attr("standardSet").trim()),
            version: attr("versionNumber").trim().parse().unwrap_or(0),
            rules,
        });
    }
    Ok(sets)
}

/// `readRule`.
fn read_rule(elem: roxmltree::Node<'_, '_>) -> Result<Rule, RulesError> {
    let attr = |k: &str| elem.attribute(k).unwrap_or_default();
    let name = attr("name").to_owned();
    let tag = elem.tag_name().name();
    // Name condition from the element text.
    let content: String = elem
        .descendants()
        .filter(roxmltree::Node::is_text)
        .filter_map(|t| t.text())
        .collect();
    let mut name_condition = None;
    if !content.is_empty() {
        if java_bool(attr("regex")) || content.contains('*') {
            if crate::java_regex::compile(&content).is_none() {
                return Err(RulesError::Regex("regex".into(), name));
            }
            name_condition = Some(match tag {
                "NAME" => NameCondition::FullName(TextMatch::Regex(content)),
                "EXTENSION" => NameCondition::Extension(TextMatch::Regex(content)),
                other => {
                    return Err(RulesError::Xml(format!(
                        "Name condition has invalid tag name of '{other}' for rule '{name}'"
                    )));
                }
            });
        } else {
            if let Some(c) = content
                .chars()
                .find(|c| ILLEGAL_FILE_NAME_CHARS.contains(c))
            {
                return Err(RulesError::IllegalChar(c, name));
            }
            name_condition = match tag {
                "NAME" => Some(NameCondition::FullName(TextMatch::Exact(content))),
                "EXTENSION" => Some(NameCondition::Extension(TextMatch::AnyOf(
                    content
                        .split(',')
                        .map(|e| e.strip_prefix('.').unwrap_or(e).to_owned())
                        .collect(),
                ))),
                _ => None,
            };
        }
    }
    let meta_type = match attr("typeFilter") {
        "" | "file" => MetaType::Files,
        "dir" => MetaType::Directories,
        "all" | "files_and_dirs" => MetaType::All,
        other => {
            return Err(RulesError::Xml(format!(
                "Files set is malformed for metatype condition, '{other}', in rule '{name}'"
            )));
        }
    };
    let (path, path_regex) = (attr("pathFilter"), attr("pathRegex"));
    let path = match (path.is_empty(), path_regex.is_empty()) {
        (true, true) => None,
        (true, false) => {
            if crate::java_regex::compile(path_regex).is_none() {
                return Err(RulesError::Regex("pathRegex".into(), name));
            }
            Some(TextMatch::Regex(path_regex.to_owned()))
        }
        (false, true) => Some(TextMatch::Partial(path.to_owned())),
        (false, false) => {
            return Err(RulesError::Xml(format!(
                "Error creating path condition for rule '{name}'"
            )));
        }
    };
    let mime_type = Some(attr("mimeType"))
        .filter(|m| !m.is_empty())
        .map(str::to_owned);
    let (cmp, value, unit) = (
        attr("comparatorSymbol"),
        attr("sizeValue"),
        attr("sizeUnits"),
    );
    let size = if !cmp.is_empty() && !value.is_empty() && !unit.is_empty() {
        let value: i32 = value.parse().map_err(|_| {
            RulesError::Xml(format!("Non integer size in files set for rule '{name}'"))
        })?;
        let (Some(comparator), Some(unit)) =
            (Comparator::from_symbol(cmp), SizeUnit::from_name(unit))
        else {
            return Err(RulesError::Xml(format!(
                "Invalid comparator or size unit in files set for rule '{name}'"
            )));
        };
        Some(SizeCondition {
            comparator,
            unit,
            value,
        })
    } else if !cmp.is_empty() || !value.is_empty() || !unit.is_empty() {
        return Err(RulesError::Xml(format!(
            "Files set is malformed missing at least one 'fileSize' attribute for rule '{name}'"
        )));
    } else {
        None
    };
    let days = attr("daysIncluded");
    let days_included = if days.is_empty() {
        None
    } else {
        Some(
            days.parse()
                .map_err(|_| RulesError::Xml("Error determining 'daysIncluded' number".into()))?,
        )
    };
    let rule = Rule {
        name,
        meta_type,
        name_condition,
        path,
        mime_type,
        size,
        days_included,
        exclusive: java_bool(attr("isExclusive")),
    };
    if !rule.has_condition() {
        return Err(RulesError::MissingConditions(rule.name));
    }
    Ok(rule)
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\r' => o.push_str("&#13;"),
            '\n' => o.push_str("&#10;"),
            '\t' => o.push_str("&#9;"),
            c => o.push(c),
        }
    }
    o
}

fn sort_key(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// `exportXmlDefinitionsFile`: sets and rules sorted by name ignoring
/// case. Unlike Autopsy, the "ignore unallocated space" flag is written
/// too (Autopsy's export drops it).
pub fn write(sets: &[FilesSet]) -> String {
    let mut sets: Vec<&FilesSet> = sets.iter().collect();
    sets.sort_by_key(|s| sort_key(&s.name));
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n<INTERESTING_FILE_SETS>\n",
    );
    for s in sets {
        let _ = writeln!(
            out,
            " <INTERESTING_FILE_SET description=\"{}\" ignoreKnown=\"{}\" ingoreUnallocated=\"{}\" name=\"{}\" standardSet=\"{}\" versionNumber=\"{}\">",
            esc(&s.description),
            s.ignore_known_files,
            s.ignore_unallocated_space,
            esc(&s.name),
            s.standard_set,
            s.version
        );
        let mut rules: Vec<&Rule> = s.rules.iter().collect();
        rules.sort_by_key(|r| sort_key(&r.name));
        for r in rules {
            let tag = match r.name_condition {
                Some(NameCondition::FullName(_)) => "NAME",
                _ => "EXTENSION",
            };
            let mut attrs: Vec<(&str, String)> = vec![("name", r.name.clone())];
            let text = match &r.name_condition {
                Some(NameCondition::FullName(t) | NameCondition::Extension(t)) => {
                    attrs.push(("regex", t.is_regex().to_string()));
                    t.text()
                }
                None => String::new(),
            };
            let type_filter = match r.meta_type {
                MetaType::Files => "file",
                MetaType::Directories => "dir",
                MetaType::FilesAndDirectories | MetaType::All => "all",
            };
            attrs.push(("typeFilter", type_filter.into()));
            if let Some(p) = &r.path {
                let k = if p.is_regex() {
                    "pathRegex"
                } else {
                    "pathFilter"
                };
                attrs.push((k, p.text()));
            }
            if let Some(m) = &r.mime_type {
                attrs.push(("mimeType", m.clone()));
            }
            if let Some(sz) = &r.size {
                attrs.push(("comparatorSymbol", sz.comparator.symbol().into()));
                attrs.push(("sizeValue", sz.value.to_string()));
                attrs.push(("sizeUnits", sz.unit.name().into()));
            }
            if let Some(d) = r.days_included {
                attrs.push(("daysIncluded", d.to_string()));
            }
            attrs.push(("isExclusive", r.exclusive.to_string()));
            attrs.sort_by(|a, b| a.0.cmp(b.0));
            let _ = write!(out, "  <{tag}");
            for (k, v) in attrs {
                let _ = write!(out, " {k}=\"{}\"", esc(&v));
            }
            if text.is_empty() {
                out.push_str("/>\n");
            } else {
                let _ = writeln!(out, ">{}</{tag}>", esc(&text));
            }
        }
        out.push_str(" </INTERESTING_FILE_SET>\n");
    }
    out.push_str("</INTERESTING_FILE_SETS>\n");
    out
}
