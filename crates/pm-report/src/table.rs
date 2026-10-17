//! Autopsy's `TableReportGenerator`: the tables of a report (one per
//! artifact type, keyword hits per list and keyword, hash set hits per set,
//! tagged files, tagged results), handed to a [`TableReportModule`] (HTML,
//! Excel) in Autopsy's order and with its column layouts.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pm_casedb::{ArtifactRecord, AttributeRecord, CaseDb, FileRecord, Value, catalog};

use crate::columns::{SOURCE_FILE, SOURCE_FILE_LAST, TAGS, TYPE_COLUMNS};
use crate::{Progress, ReportError, ReportSettings};

/// A report table column (Autopsy's `Column` implementations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Col {
    /// An attribute's value (`header`, attribute type name).
    Attr(&'static str, &'static str),
    /// The source file's unique path.
    SourceFile(&'static str),
    /// Filled by a type-specific rule (`ArtifactData.getOrderedRowDataAsStrings`).
    Special(&'static str),
    /// The review status.
    Status,
}

/// An output format fed by the generator (Autopsy `TableReportModule`).
/// Calls arrive in order: `start_report`, then per data type
/// `start_data_type` … `end_data_type` with tables (optionally grouped in
/// sets), then `end_report`.
pub trait TableReportModule {
    fn start_report(&mut self) -> Result<(), ReportError>;
    fn end_report(&mut self) -> Result<(), ReportError>;
    fn start_data_type(&mut self, title: &str, description: &str) -> Result<(), ReportError>;
    fn end_data_type(&mut self) -> Result<(), ReportError>;
    /// An index of the sets that follow (keyword lists, hash sets).
    fn add_set_index(&mut self, sets: &[String]) -> Result<(), ReportError>;
    fn start_set(&mut self, name: &str) -> Result<(), ReportError>;
    fn end_set(&mut self) -> Result<(), ReportError>;
    /// A heading for the table that follows within a set (a keyword).
    fn add_set_element(&mut self, name: &str) -> Result<(), ReportError>;
    fn start_table(&mut self, headers: &[String]) -> Result<(), ReportError>;
    fn add_row(&mut self, cells: &[String]) -> Result<(), ReportError>;
    fn end_table(&mut self) -> Result<(), ReportError>;
}

/// An artifact with its attributes and tag names.
type Row = (ArtifactRecord, Vec<AttributeRecord>, BTreeSet<String>);

/// Rows read per query (bounded memory: one page at a time is formatted;
/// rows of one table are kept to sort them, as Autopsy does).
const PAGE: usize = 5_000;

/// What the generator found that the report could not include.
#[derive(Debug, Default)]
pub struct TableSummary {
    pub tables: usize,
    pub rows: usize,
    pub errors: Vec<String>,
}

pub(crate) struct Generator<'a> {
    db: &'a CaseDb,
    settings: &'a ReportSettings,
    progress: &'a dyn Progress,
    files: HashMap<i64, Option<FileRecord>>,
    ds_names: HashMap<i64, String>,
    summary: TableSummary,
}

impl<'a> Generator<'a> {
    pub(crate) fn new(
        db: &'a CaseDb,
        settings: &'a ReportSettings,
        progress: &'a dyn Progress,
    ) -> Result<Self, ReportError> {
        let ds_names = db
            .data_sources()?
            .into_iter()
            .map(|d| (d.id, d.name))
            .collect();
        Ok(Self {
            db,
            settings,
            progress,
            files: HashMap::new(),
            ds_names,
            summary: TableSummary::default(),
        })
    }

    /// Writes every table to `m`.
    pub(crate) fn run(
        mut self,
        m: &mut dyn TableReportModule,
    ) -> Result<TableSummary, ReportError> {
        m.start_report()?;
        self.artifact_tables(m)?;
        if self.settings.tagged_files {
            self.tagged_files(m)?;
        }
        if self.settings.tagged_results {
            self.tagged_results(m)?;
        }
        m.end_report()?;
        Ok(self.summary)
    }

    fn cancelled(&self) -> Result<(), ReportError> {
        if self.progress.is_cancelled() {
            Err(ReportError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn file(&mut self, id: Option<i64>) -> Option<FileRecord> {
        let id = id?;
        if let Some(f) = self.files.get(&id) {
            return f.clone();
        }
        let f = self.db.file(id).ok().flatten();
        // A bounded cache: tables touch files in id order, mostly once.
        if self.files.len() > 100_000 {
            self.files.clear();
        }
        self.files.insert(id, f.clone());
        f
    }

    /// Autopsy's unique path: `/<data source>/<path>`.
    fn unique_path(&self, f: &FileRecord) -> String {
        let ds = self.ds_names.get(&f.ds_id).map_or("", String::as_str);
        format!("/{ds}{}", f.path())
    }

    fn in_scope(&self, ds_id: i64) -> bool {
        self.settings.data_sources.is_empty() || self.settings.data_sources.contains(&ds_id)
    }

    fn comment(&self) -> String {
        if self.settings.tag_names.is_empty() {
            String::new()
        } else {
            format!(
                "Contains results that were tagged with one of the following: {}",
                self.settings.tag_names.join(", ")
            )
        }
    }

    /// Artifacts of a type in scope, with their attributes and tags,
    /// filtered by the tag names when the settings name some.
    fn artifacts(&mut self, type_id: i64) -> Result<Vec<Row>, ReportError> {
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            self.cancelled()?;
            let page = self.db.artifacts_of_type(type_id, None, after, PAGE)?;
            let Some(last) = page.last() else { break };
            after = last.id;
            let ids: Vec<i64> = page.iter().map(|a| a.id).collect();
            let mut tags: HashMap<i64, BTreeSet<String>> = HashMap::new();
            for t in self.db.artifact_tags_of(&ids)? {
                tags.entry(t.artifact_id).or_default().insert(t.tag_name);
            }
            for a in page {
                if !self.in_scope(a.ds_id) {
                    continue;
                }
                let t = tags.remove(&a.id).unwrap_or_default();
                if !self.settings.tag_names.is_empty()
                    && !t.iter().any(|n| self.settings.tag_names.contains(n))
                {
                    continue;
                }
                let attrs = self.db.attributes(a.id)?;
                out.push((a, attrs, t));
            }
        }
        Ok(out)
    }

    fn artifact_tables(&mut self, m: &mut dyn TableReportModule) -> Result<(), ReportError> {
        let comment = self.comment();
        let mut types = self.db.artifact_type_names()?;
        types.retain(|(_, name, _, _)| !catalog::HIDDEN_ARTIFACT_TYPES.contains(&name.as_str()));
        if let Some(only) = &self.settings.artifact_types {
            types.retain(|(_, name, _, _)| only.contains(name));
        }
        // Autopsy orders the types by display name.
        types.sort_by(|a, b| a.2.cmp(&b.2));
        for (type_id, name, display, _) in types {
            self.cancelled()?;
            self.progress
                .status(&format!("Now processing {display}..."));
            match name.as_str() {
                "TSK_KEYWORD_HIT" => self.keyword_hits(m, type_id, &comment)?,
                "TSK_HASHSET_HIT" => self.hashset_hits(m, type_id, &comment)?,
                _ => {
                    let arts = self.artifacts(type_id)?;
                    if !arts.is_empty() {
                        self.type_table(m, &name, &display, &comment, &arts)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `writeTableForDataType`.
    fn type_table(
        &mut self,
        m: &mut dyn TableReportModule,
        name: &str,
        display: &str,
        comment: &str,
        arts: &[Row],
    ) -> Result<(), ReportError> {
        // Every attribute type present, by display name.
        let mut present: BTreeMap<String, String> = BTreeMap::new();
        for (_, attrs, _) in arts {
            for a in attrs {
                present
                    .entry(a.display_name.clone())
                    .or_insert_with(|| a.name.clone());
            }
        }
        let columns = columns_for(name, &present);
        let headers: Vec<String> = columns.iter().map(Column::header).collect();
        let mut rows: Vec<Vec<String>> = Vec::with_capacity(arts.len());
        for (a, attrs, tags) in arts {
            let file = self.file(a.file_id);
            rows.push(self.row(name, &columns, a, attrs, tags, file.as_ref()));
        }
        // Autopsy sorts rows by their cells.
        rows.sort();
        m.start_data_type(display, comment)?;
        m.start_table(&headers)?;
        for r in &rows {
            m.add_row(r)?;
        }
        m.end_table()?;
        m.end_data_type()?;
        self.summary.tables += 1;
        self.summary.rows += rows.len();
        Ok(())
    }

    fn row(
        &self,
        type_name: &str,
        columns: &[Column],
        a: &ArtifactRecord,
        attrs: &[AttributeRecord],
        tags: &BTreeSet<String>,
        file: Option<&FileRecord>,
    ) -> Vec<String> {
        let tag_list = tags.iter().cloned().collect::<Vec<_>>().join(", ");
        let path = file.map(|f| self.unique_path(f)).unwrap_or_default();
        if type_name == "TSK_EXT_MISMATCH_DETECTED" {
            return match file {
                Some(f) => vec![
                    f.name.clone(),
                    f.extension.clone(),
                    f.mime.clone().unwrap_or_default(),
                    path,
                    tag_list,
                ],
                None => vec![
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    tag_list,
                ],
            };
        }
        columns
            .iter()
            .map(|c| match c {
                Column::Attr(_, attr) => {
                    // Interesting items show the file's unique path as Path.
                    if attr == "TSK_PATH"
                        && matches!(
                            type_name,
                            "TSK_INTERESTING_FILE_HIT" | "TSK_INTERESTING_ITEM"
                        )
                    {
                        return path.clone();
                    }
                    attrs
                        .iter()
                        .filter(|x| &x.name == attr)
                        .map(|x| self.value(&x.value))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                Column::SourceFile(_) => path.clone(),
                Column::Special(h) if *h == "Size" => {
                    file.map_or_else(String::new, |f| f.size.to_string())
                }
                Column::Special(_) => String::new(),
                Column::Status => review_status(a.review_status).to_owned(),
                Column::Tags => tag_list.clone(),
            })
            .collect()
    }

    fn value(&self, v: &Value) -> String {
        match v {
            Value::DateTime(t) => self.settings.format_time(*t),
            other => other.display(),
        }
    }

    /// `writeKeywordHits`: per list (ad-hoc searches as "User Searches"),
    /// per keyword, a table of Preview / Source File / Tags.
    fn keyword_hits(
        &mut self,
        m: &mut dyn TableReportModule,
        type_id: i64,
        comment: &str,
    ) -> Result<(), ReportError> {
        let arts = self.artifacts(type_id)?;
        if arts.is_empty() {
            return Ok(());
        }
        let text = |attrs: &[AttributeRecord], n: &str| {
            attrs
                .iter()
                .find(|a| a.name == n)
                .map(|a| a.value.display())
                .unwrap_or_default()
        };
        // (list, keyword, parent path, name, preview) as Autopsy orders them.
        let mut hits: Vec<(String, String, String, String, String, String, String)> = Vec::new();
        for (a, attrs, tags) in &arts {
            let list = match text(attrs, "TSK_SET_NAME") {
                l if l.is_empty() => "User Searches".to_owned(),
                l => l,
            };
            let file = self.file(a.file_id);
            let (parent, name, path) = file.as_ref().map_or_else(Default::default, |f| {
                (f.parent_path.clone(), f.name.clone(), self.unique_path(f))
            });
            hits.push((
                list,
                text(attrs, "TSK_KEYWORD"),
                parent,
                name,
                text(attrs, "TSK_KEYWORD_PREVIEW"),
                path,
                tags.iter().cloned().collect::<Vec<_>>().join(", "),
            ));
        }
        hits.sort();
        let lists: Vec<String> = hits
            .iter()
            .map(|h| h.0.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        m.start_data_type("Keyword Hits", comment)?;
        m.add_set_index(&lists)?;
        let headers = [
            "Preview".to_owned(),
            SOURCE_FILE.to_owned(),
            TAGS.to_owned(),
        ];
        let mut current: Option<(String, String)> = None;
        for (list, keyword, _, _, preview, path, tags) in &hits {
            if current.as_ref().is_none_or(|c| &c.0 != list) {
                if current.is_some() {
                    m.end_table()?;
                    m.end_set()?;
                }
                m.start_set(list)?;
                current = None;
            }
            if current.as_ref().is_none_or(|c| &c.1 != keyword) {
                if current.is_some() {
                    m.end_table()?;
                }
                m.add_set_element(keyword)?;
                m.start_table(&headers)?;
                self.summary.tables += 1;
                current = Some((list.clone(), keyword.clone()));
            }
            m.add_row(&[preview.clone(), path.clone(), tags.clone()])?;
            self.summary.rows += 1;
        }
        if current.is_some() {
            m.end_table()?;
            m.end_set()?;
        }
        m.end_data_type()
    }

    /// `writeHashsetHits`: per hash set, a table of File / Size / Tags.
    fn hashset_hits(
        &mut self,
        m: &mut dyn TableReportModule,
        type_id: i64,
        comment: &str,
    ) -> Result<(), ReportError> {
        let arts = self.artifacts(type_id)?;
        if arts.is_empty() {
            return Ok(());
        }
        let mut sets: BTreeMap<String, Vec<Vec<String>>> = BTreeMap::new();
        for (a, attrs, tags) in &arts {
            let set = attrs
                .iter()
                .find(|x| x.name == "TSK_SET_NAME")
                .map(|x| x.value.display())
                .unwrap_or_default();
            let file = self.file(a.file_id);
            sets.entry(set).or_default().push(vec![
                file.as_ref()
                    .map(|f| self.unique_path(f))
                    .unwrap_or_default(),
                file.as_ref()
                    .map_or_else(String::new, |f| f.size.to_string()),
                tags.iter().cloned().collect::<Vec<_>>().join(", "),
            ]);
        }
        let names: Vec<String> = sets.keys().cloned().collect();
        m.start_data_type("Hashset Hits", comment)?;
        m.add_set_index(&names)?;
        let headers = ["File".to_owned(), "Size".to_owned(), TAGS.to_owned()];
        for (set, mut rows) in sets {
            rows.sort();
            m.start_set(&set)?;
            m.start_table(&headers)?;
            for r in &rows {
                m.add_row(r)?;
            }
            m.end_table()?;
            m.end_set()?;
            self.summary.tables += 1;
            self.summary.rows += rows.len();
        }
        m.end_data_type()
    }

    fn selected_tags(&self) -> Result<Vec<pm_casedb::TagName>, ReportError> {
        let mut names = self.db.tag_names()?;
        if !self.settings.tag_names.is_empty() {
            names.retain(|t| self.settings.tag_names.contains(&t.name));
        }
        Ok(names)
    }

    /// `makeContentTagsTables`.
    fn tagged_files(&mut self, m: &mut dyn TableReportModule) -> Result<(), ReportError> {
        self.progress.status("Now processing Tagged Files...");
        let mut rows = Vec::new();
        for t in self.selected_tags()? {
            self.cancelled()?;
            for (tag, f) in self.db.files_with_tag(t.id)? {
                if !self.in_scope(f.ds_id) {
                    continue;
                }
                let time = |i: usize| {
                    f.times
                        .get(i)
                        .copied()
                        .flatten()
                        .map(|(s, _)| self.settings.format_time(s))
                        .unwrap_or_default()
                };
                rows.push(vec![
                    tag.tag_name.clone(),
                    f.name.clone(),
                    tag.comment.clone(),
                    tag.examiner.clone(),
                    time(0),
                    time(2),
                    time(1),
                    time(3),
                    f.size.to_string(),
                    f.md5
                        .as_deref()
                        .map(pm_core::decode::to_hex)
                        .unwrap_or_default(),
                ]);
            }
        }
        if rows.is_empty() {
            return Ok(());
        }
        let headers: Vec<String> = [
            "Tag",
            "File",
            "Comment",
            "User Name",
            "Time Modified",
            "Time Changed",
            "Time Accessed",
            "Time Created",
            "Size (Bytes)",
            "Hash",
        ]
        .map(str::to_owned)
        .to_vec();
        self.simple_table(m, "Tagged Files", &self.comment_files(), &headers, rows)
    }

    fn comment_files(&self) -> String {
        if self.settings.tag_names.is_empty() {
            String::new()
        } else {
            format!(
                "Contains files that were tagged with one of the following: {}",
                self.settings.tag_names.join(", ")
            )
        }
    }

    /// `makeBlackboardArtifactTagsTables`.
    fn tagged_results(&mut self, m: &mut dyn TableReportModule) -> Result<(), ReportError> {
        self.progress.status("Now processing Tagged Results...");
        let names: HashMap<i64, String> = self
            .db
            .artifact_type_names()?
            .into_iter()
            .map(|(id, _, display, _)| (id, display))
            .collect();
        let mut rows = Vec::new();
        for t in self.selected_tags()? {
            let mut after = 0;
            loop {
                self.cancelled()?;
                let page = self.db.artifact_tags_named(t.id, None, after, PAGE)?;
                let Some(last) = page.last() else { break };
                after = last.id;
                for tag in page {
                    let Some(a) = self.db.artifact(tag.artifact_id)? else {
                        continue;
                    };
                    if !self.in_scope(a.ds_id) {
                        continue;
                    }
                    let file = self.file(a.file_id);
                    rows.push(vec![
                        names.get(&a.type_id).cloned().unwrap_or_default(),
                        tag.tag_name.clone(),
                        tag.comment.clone(),
                        file.as_ref()
                            .map(|f| self.unique_path(f))
                            .unwrap_or_default(),
                        tag.examiner.clone(),
                    ]);
                }
            }
        }
        if rows.is_empty() {
            return Ok(());
        }
        let headers: Vec<String> = ["Result Type", "Tag", "Comment", "Source File", "User Name"]
            .map(str::to_owned)
            .to_vec();
        let comment = self.comment();
        self.simple_table(m, "Tagged Results", &comment, &headers, rows)
    }

    fn simple_table(
        &mut self,
        m: &mut dyn TableReportModule,
        title: &str,
        comment: &str,
        headers: &[String],
        mut rows: Vec<Vec<String>>,
    ) -> Result<(), ReportError> {
        rows.sort();
        m.start_data_type(title, comment)?;
        m.start_table(headers)?;
        for r in &rows {
            m.add_row(r)?;
        }
        m.end_table()?;
        m.end_data_type()?;
        self.summary.tables += 1;
        self.summary.rows += rows.len();
        Ok(())
    }
}

/// A column of an artifact table.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Column {
    Attr(String, String),
    SourceFile(String),
    Special(&'static str),
    Status,
    Tags,
}

impl Column {
    fn header(&self) -> String {
        match self {
            Self::Attr(h, _) | Self::SourceFile(h) => h.clone(),
            Self::Special(h) => (*h).to_owned(),
            Self::Status => "Status".to_owned(),
            Self::Tags => TAGS.to_owned(),
        }
    }
}

/// `getArtifactTableColumns`: the type's own leading columns, then every
/// other attribute present (by display name), then Source File (for some
/// types) and Tags. Types without a layout list every attribute.
fn columns_for(type_name: &str, present: &BTreeMap<String, String>) -> Vec<Column> {
    let Some((_, lead, removed)) = TYPE_COLUMNS.iter().find(|(t, _, _)| t.contains(&type_name))
    else {
        let mut cols: Vec<Column> = present
            .iter()
            .map(|(display, name)| Column::Attr(display.clone(), name.clone()))
            .collect();
        cols.push(Column::SourceFile(SOURCE_FILE.to_owned()));
        cols.push(Column::Tags);
        return cols;
    };
    let mut cols: Vec<Column> = lead
        .iter()
        .map(|c| match *c {
            Col::Attr(h, a) => Column::Attr(h.to_owned(), a.to_owned()),
            Col::SourceFile(h) => Column::SourceFile(h.to_owned()),
            Col::Special(h) => Column::Special(h),
            Col::Status => Column::Status,
        })
        .collect();
    if type_name == "TSK_EXT_MISMATCH_DETECTED" {
        cols.push(Column::Tags);
        return cols;
    }
    let used: BTreeSet<&str> = lead
        .iter()
        .filter_map(|c| match c {
            Col::Attr(_, a) => Some(*a),
            _ => None,
        })
        .chain(removed.iter().copied())
        .collect();
    for (display, name) in present {
        if !used.contains(name.as_str()) {
            cols.push(Column::Attr(display.clone(), name.clone()));
        }
    }
    if SOURCE_FILE_LAST.contains(&type_name) {
        cols.push(Column::SourceFile(SOURCE_FILE.to_owned()));
    }
    cols.push(Column::Tags);
    cols
}

fn review_status(code: i64) -> &'static str {
    match code {
        1 => "Approved",
        2 => "Rejected",
        _ => "Undecided",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(d, n)| ((*d).to_owned(), (*n).to_owned()))
            .collect()
    }

    #[test]
    fn web_history_columns_follow_autopsy() {
        let cols = columns_for(
            "TSK_WEB_HISTORY",
            &present(&[
                ("URL", "TSK_URL"),
                ("Domain", "TSK_DOMAIN"),
                ("Title", "TSK_TITLE"),
            ]),
        );
        let headers: Vec<String> = cols.iter().map(Column::header).collect();
        assert_eq!(
            headers,
            [
                "URL",
                "Date Accessed",
                "Referrer",
                "Title",
                "Program",
                "URL Domain",
                "Domain",
                "Source File",
                "Tags"
            ]
        );
    }

    #[test]
    fn unknown_types_list_every_attribute() {
        let cols = columns_for(
            "PM_SHIMCACHE",
            &present(&[("Path", "TSK_PATH"), ("Date/Time", "TSK_DATETIME")]),
        );
        let headers: Vec<String> = cols.iter().map(Column::header).collect();
        assert_eq!(headers, ["Date/Time", "Path", "Source File", "Tags"]);
    }
}
