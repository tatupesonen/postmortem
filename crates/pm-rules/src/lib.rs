//! Autopsy's files-set rule language (`modules/interestingitems/FilesSet`):
//! named sets of inclusive and exclusive rules over a file's name or
//! extension, parent path, MIME type, size, recent dates and meta type.
//! Used by the Interesting Files Identifier and by ingest file filters.
//!
//! * [`FilesSet`] / [`Rule`]: the definitions (serde-friendly);
//! * [`CompiledSet`]: a set ready to match [`FileFacts`];
//! * [`xml`]: Autopsy's `INTERESTING_FILE_SETS` import and export;
//! * [`standard`]: Autopsy's standard sets (cloud storage, cryptocurrency
//!   wallets, encryption, privacy and remote-management programs).

pub mod java_regex;
pub mod standard;
pub mod xml;

use serde::{Deserialize, Serialize};

/// Rule name reported when a set has only exclusive rules.
pub const NOT_EXCLUDED: &str = "Not Excluded";
/// Characters a plain name condition may not contain.
pub const ILLEGAL_FILE_NAME_CHARS: [char; 8] = ['\\', '/', ':', '*', '?', '"', '<', '>'];
/// Characters a plain path condition may not contain.
pub const ILLEGAL_FILE_PATH_CHARS: [char; 7] = ['\\', ':', '*', '?', '"', '<', '>'];

/// Invalid set or rule definitions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    #[error("Interesting files set name cannot be null or empty")]
    EmptySetName,
    #[error("Invalid rule in files set, missing necessary conditions for '{0}'")]
    MissingConditions(String),
    #[error("Error compiling '{0}' regex in rule '{1}'")]
    Regex(String, String),
    #[error("File name has illegal character of '{0}' in rule '{1}'")]
    IllegalChar(char, String),
    #[error("{0}")]
    Xml(String),
}

/// What kind of object a rule applies to (`MetaTypeCondition.Type`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaType {
    /// Regular files (`isFile()`).
    #[default]
    Files,
    /// Directories and virtual directories.
    Directories,
    /// Regular files, directories and virtual directories.
    FilesAndDirectories,
    /// Anything.
    All,
}

/// How text is compared (`FilesSet.Rule.TextMatcher`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum TextMatch {
    /// The whole text, ignoring case.
    Exact(String),
    /// A substring, ignoring ASCII case (Java's `CASE_INSENSITIVE`).
    Partial(String),
    /// Any of the values, ignoring case.
    AnyOf(Vec<String>),
    /// A regular expression found anywhere in the text (case-sensitive).
    Regex(String),
}

impl TextMatch {
    /// `getTextToMatch`.
    pub fn text(&self) -> String {
        match self {
            Self::Exact(s) | Self::Partial(s) | Self::Regex(s) => s.clone(),
            Self::AnyOf(v) => v.join(","),
        }
    }

    pub fn is_regex(&self) -> bool {
        matches!(self, Self::Regex(_))
    }
}

/// A name condition: the full name or the extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameCondition {
    FullName(TextMatch),
    /// Extensions without the leading dot.
    Extension(TextMatch),
}

/// `FileSizeCondition.COMPARATOR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    LessThan,
    LessThanEqual,
    Equal,
    GreaterThan,
    GreaterThanEqual,
}

impl Comparator {
    /// `getSymbol`.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::LessThan => "<",
            Self::LessThanEqual => "≤",
            Self::Equal => "=",
            Self::GreaterThan => ">",
            Self::GreaterThanEqual => "≥",
        }
    }

    /// `fromSymbol` (also accepts the ASCII forms).
    pub fn from_symbol(s: &str) -> Option<Self> {
        Some(match s {
            "<=" | "≤" => Self::LessThanEqual,
            "<" => Self::LessThan,
            "==" | "=" => Self::Equal,
            ">" => Self::GreaterThan,
            ">=" | "≥" => Self::GreaterThanEqual,
            _ => return None,
        })
    }
}

/// `FileSizeCondition.SIZE_UNIT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeUnit {
    Bytes,
    Kilobytes,
    Megabytes,
    Gigabytes,
}

impl SizeUnit {
    pub fn bytes(self) -> u64 {
        match self {
            Self::Bytes => 1,
            Self::Kilobytes => 1024,
            Self::Megabytes => 1024 * 1024,
            Self::Gigabytes => 1024 * 1024 * 1024,
        }
    }

    /// The unit's name in Autopsy's XML.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bytes => "Bytes",
            Self::Kilobytes => "Kilobytes",
            Self::Megabytes => "Megabytes",
            Self::Gigabytes => "Gigabytes",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        [
            Self::Bytes,
            Self::Kilobytes,
            Self::Megabytes,
            Self::Gigabytes,
        ]
        .into_iter()
        .find(|u| u.name() == s)
    }
}

/// `FileSizeCondition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeCondition {
    pub comparator: Comparator,
    pub unit: SizeUnit,
    /// A Java `int`.
    pub value: i32,
}

impl SizeCondition {
    pub fn passes(&self, size: u64) -> bool {
        let limit = i128::from(self.unit.bytes()) * i128::from(self.value);
        let size = i128::from(size);
        match self.comparator {
            Comparator::GreaterThan => size > limit,
            Comparator::GreaterThanEqual => size >= limit,
            Comparator::LessThanEqual => size <= limit,
            Comparator::LessThan => size < limit,
            Comparator::Equal => size == limit,
        }
    }
}

/// One rule: every present condition must pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub meta_type: MetaType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_condition: Option<NameCondition>,
    /// Matched against the parent path plus `/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<TextMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<SizeCondition>,
    /// Created or modified within this many days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_included: Option<i32>,
    /// An exclusive rule removes matching files from the set.
    #[serde(default)]
    pub exclusive: bool,
}

impl Rule {
    /// A rule matching files by meta type only (for the standard ingest
    /// filters; not valid in an interesting files set).
    pub fn all(name: &str, meta_type: MetaType) -> Self {
        Self {
            name: name.to_owned(),
            meta_type,
            name_condition: None,
            path: None,
            mime_type: None,
            size: None,
            days_included: None,
            exclusive: false,
        }
    }

    /// Whether a condition beyond the meta type is present (the XML
    /// reader rejects rules without one).
    pub fn has_condition(&self) -> bool {
        self.name_condition.is_some()
            || self.path.is_some()
            || self.mime_type.is_some()
            || self.size.is_some()
            || self.days_included.is_some()
    }

    /// `Rule.toString()` as shown in Autopsy's rule list.
    pub fn display(&self) -> String {
        if let Some(n) = &self.name_condition {
            let (NameCondition::FullName(t) | NameCondition::Extension(t)) = n;
            format!("{} ({})", self.name, t.text())
        } else if let Some(p) = &self.path {
            format!("{} ({})", self.name, p.text())
        } else if let Some(m) = &self.mime_type {
            format!("{} ({m})", self.name)
        } else if let Some(s) = &self.size {
            format!(
                "{} ({} {} {})",
                self.name,
                s.comparator.symbol(),
                s.value,
                s.unit.name()
            )
        } else if let Some(d) = self.days_included {
            format!("{}(modified within {d} day(s))", self.name)
        } else {
            format!("{} ()", self.name)
        }
    }
}

/// A named set of rules (`FilesSet`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesSet {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub ignore_known_files: bool,
    #[serde(default)]
    pub ignore_unallocated_space: bool,
    /// One of Autopsy's standard sets.
    #[serde(default)]
    pub standard_set: bool,
    #[serde(default)]
    pub version: i32,
    pub rules: Vec<Rule>,
}

impl FilesSet {
    /// Compiles the regular expressions; fails on an empty name or an
    /// invalid regex.
    pub fn compile(&self) -> Result<CompiledSet, RulesError> {
        if self.name.is_empty() {
            return Err(RulesError::EmptySetName);
        }
        let rules = self
            .rules
            .iter()
            .map(CompiledRule::new)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CompiledSet {
            set: self.clone(),
            rules,
        })
    }
}

/// How a file is classified for meta-type conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMeta {
    /// Autopsy meta type REG (`isFile()`).
    Regular,
    /// Autopsy meta type DIR.
    Directory,
    /// Autopsy meta type `VIRT_DIR`.
    VirtualDirectory,
    /// Anything else (symlinks, devices, virtual files).
    Other,
}

/// Where a file's content comes from, for "ignore unallocated space".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOrigin {
    Normal,
    /// Unallocated or unused blocks.
    Unallocated,
    Slack,
}

/// The facts the rules look at.
#[derive(Debug, Clone)]
pub struct FileFacts<'a> {
    pub name: &'a str,
    /// Lower-case extension without the dot (`getNameExtension`).
    pub extension: &'a str,
    /// The parent path with a trailing `/` (`getParentPath`).
    pub parent_path: &'a str,
    pub mime_type: Option<&'a str>,
    pub size: u64,
    pub meta: FileMeta,
    pub origin: FileOrigin,
    /// Hash-set known (NSRL).
    pub known: bool,
    /// Creation and modification times (Unix seconds; 0 when unknown).
    pub crtime: i64,
    pub mtime: i64,
}

#[derive(Debug, Clone)]
enum Matcher {
    Exact(String),
    Partial(String),
    AnyOf(Vec<String>),
    Regex(regex::Regex),
}

impl Matcher {
    fn new(t: &TextMatch, rule: &str) -> Result<Self, RulesError> {
        Ok(match t {
            TextMatch::Exact(s) => Self::Exact(s.clone()),
            TextMatch::Partial(s) => Self::Partial(s.to_ascii_lowercase()),
            TextMatch::AnyOf(v) => Self::AnyOf(v.clone()),
            TextMatch::Regex(r) => Self::Regex(
                java_regex::compile(r)
                    .ok_or_else(|| RulesError::Regex(r.clone(), rule.to_owned()))?,
            ),
        })
    }

    fn matches(&self, subject: &str) -> bool {
        match self {
            Self::Exact(s) => java_equals_ignore_case(subject, s),
            Self::Partial(lower) => subject.to_ascii_lowercase().contains(lower.as_str()),
            Self::AnyOf(v) => v.iter().any(|s| java_equals_ignore_case(subject, s)),
            Self::Regex(r) => r.is_match(subject),
        }
    }
}

/// Java's `String.equalsIgnoreCase`: same length, and each pair of
/// characters equal after upper- or lower-casing.
pub fn java_equals_ignore_case(a: &str, b: &str) -> bool {
    fn upper(c: char) -> char {
        let mut u = c.to_uppercase();
        match (u.next(), u.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    }
    fn lower(c: char) -> char {
        let mut l = c.to_lowercase();
        match (l.next(), l.next()) {
            (Some(x), None) => x,
            _ => c,
        }
    }
    a.chars().count() == b.chars().count()
        && a.chars()
            .zip(b.chars())
            .all(|(x, y)| x == y || upper(x) == upper(y) || lower(upper(x)) == lower(upper(y)))
}

#[derive(Debug, Clone)]
struct CompiledRule {
    rule: Rule,
    name: Option<(bool, Matcher)>,
    path: Option<Matcher>,
}

impl CompiledRule {
    fn new(r: &Rule) -> Result<Self, RulesError> {
        let name = match &r.name_condition {
            Some(NameCondition::FullName(t)) => Some((true, Matcher::new(t, &r.name)?)),
            Some(NameCondition::Extension(t)) => Some((false, Matcher::new(t, &r.name)?)),
            None => None,
        };
        let path = r
            .path
            .as_ref()
            .map(|t| Matcher::new(t, &r.name))
            .transpose()?;
        Ok(Self {
            rule: r.clone(),
            name,
            path,
        })
    }

    /// `Rule.isSatisfied`: meta type, size, name, MIME type, path, date.
    fn satisfied(&self, f: &FileFacts<'_>, now: i64) -> bool {
        let r = &self.rule;
        let meta_ok = match r.meta_type {
            MetaType::Files => f.meta == FileMeta::Regular,
            MetaType::Directories => {
                matches!(f.meta, FileMeta::Directory | FileMeta::VirtualDirectory)
            }
            MetaType::FilesAndDirectories => matches!(
                f.meta,
                FileMeta::Regular | FileMeta::Directory | FileMeta::VirtualDirectory
            ),
            MetaType::All => true,
        };
        if !meta_ok {
            return false;
        }
        if r.size.is_some_and(|s| !s.passes(f.size)) {
            return false;
        }
        if let Some((full, m)) = &self.name {
            let subject = if *full { f.name } else { f.extension };
            if !m.matches(subject) {
                return false;
            }
        }
        if let Some(m) = &r.mime_type
            && f.mime_type != Some(m.as_str())
        {
            return false;
        }
        if let Some(p) = &self.path
            && !p.matches(&format!("{}/", f.parent_path))
        {
            return false;
        }
        if let Some(days) = r.days_included {
            let threshold = now - i64::from(days) * 86_400;
            if !(f.crtime > threshold || f.mtime > threshold) {
                return false;
            }
        }
        true
    }
}

/// A set ready for matching.
#[derive(Debug, Clone)]
pub struct CompiledSet {
    set: FilesSet,
    rules: Vec<CompiledRule>,
}

impl CompiledSet {
    pub fn set(&self) -> &FilesSet {
        &self.set
    }

    /// `FilesSet.fileIsMemberOf`: the name of the first inclusive rule the
    /// file satisfies (or "Not Excluded" when the set has only exclusive
    /// rules), unless an exclusive rule also matches. `now` is the current
    /// Unix time for date conditions.
    ///
    /// Autopsy iterates its rules in hash order, so which rule name is
    /// reported for a file matching several is arbitrary there; here it is
    /// the first in definition order.
    pub fn member_rule(&self, f: &FileFacts<'_>, now: i64) -> Option<&str> {
        let s = &self.set;
        if s.ignore_known_files && f.known {
            return None;
        }
        if s.ignore_unallocated_space && f.origin != FileOrigin::Normal {
            return None;
        }
        let inclusive = self.rules.iter().filter(|r| !r.rule.exclusive);
        let mut exclusive = self.rules.iter().filter(|r| r.rule.exclusive).peekable();
        let has_inclusive = self.rules.iter().any(|r| !r.rule.exclusive);
        let name = if has_inclusive {
            inclusive
                .into_iter()
                .find(|r| r.satisfied(f, now))
                .map(|r| r.rule.name.as_str())
        } else if exclusive.peek().is_none() {
            return None;
        } else {
            Some(NOT_EXCLUDED)
        };
        if exclusive.any(|r| r.satisfied(f, now)) {
            return None;
        }
        name
    }
}
