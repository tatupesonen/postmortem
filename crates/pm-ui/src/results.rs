//! Analysis results and data artifacts from the case database (Autopsy's
//! "Data Artifacts" / "Analysis Results" tree and result viewer).

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_extras::{Column, TableBuilder};
use pm_case::Case;
use pm_casedb::catalog::Category;
use pm_casedb::{ArtifactRecord, FileRecord, Significance, Value};

use crate::format::{DisplayPrefs, grouped};
use crate::model::RowTag;
use crate::sortable::{Order, Sort, cmp_text};
use crate::tags_ui::{TagCatalog, TagRequest};
use crate::task::Task;

/// Rows shown per result type (the table is not paged yet).
pub(crate) const MAX_ROWS: usize = 20_000;
/// How often counts refresh while analysis is running.
const REFRESH: Duration = Duration::from_secs(2);

/// A result type with its number of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeCount {
    pub(crate) type_id: i64,
    /// Type name (`KAAMOS_WEB_HISTORY`), for the tree icon.
    pub(crate) name: String,
    pub(crate) display: String,
    pub(crate) category: Category,
    pub(crate) count: u64,
}

/// Loads the result types that have results, in display-name order.
pub(crate) fn load_counts(case: &Case) -> Result<Vec<TypeCount>, String> {
    let db = case.db();
    let counts = db.artifact_counts(None).map_err(|e| e.to_string())?;
    let names = db.artifact_type_names().map_err(|e| e.to_string())?;
    let mut out: Vec<TypeCount> = counts
        .into_iter()
        .filter_map(|(type_id, count)| {
            let (_, name, display, cat) = names.iter().find(|n| n.0 == type_id)?;
            Some(TypeCount {
                type_id,
                name: name.clone(),
                display: display.clone(),
                category: if *cat == Category::AnalysisResult as i64 {
                    Category::AnalysisResult
                } else {
                    Category::DataArtifact
                },
                count,
            })
        })
        .collect();
    out.sort_by_key(|t| t.display.to_lowercase());
    Ok(out)
}

/// Result-type counts, refreshed periodically in the background.
#[derive(Debug, Default)]
pub(crate) struct Counts {
    task: Option<Task<Result<Vec<TypeCount>, String>>>,
    current: Arc<Vec<TypeCount>>,
    refreshed: Option<Instant>,
}

impl Counts {
    /// Latest counts; schedules a refresh when stale (`live`: analysis is
    /// running, so refresh every couple of seconds).
    pub(crate) fn get(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<Case>>,
        live: bool,
    ) -> Arc<Vec<TypeCount>> {
        if let Some(t) = &mut self.task
            && let Some(r) = t.poll()
        {
            if let Ok(v) = r {
                self.current = Arc::new(v.clone());
            }
            self.task = None;
            self.refreshed = Some(Instant::now());
        }
        let stale = self
            .refreshed
            .is_none_or(|t| live && t.elapsed() >= REFRESH);
        if self.task.is_none()
            && stale
            && let Some(case) = case
        {
            let case = Arc::clone(case);
            self.task = Some(Task::spawn(ctx, move || load_counts(&case)));
        }
        Arc::clone(&self.current)
    }

    /// Forces a reload (case changed or analysis finished).
    pub(crate) fn invalidate(&mut self) {
        self.refreshed = None;
        self.task = None;
        self.current = Arc::new(Vec::new());
    }
}

/// One row of a result table.
#[derive(Debug, Clone)]
pub(crate) struct ResultRow {
    pub(crate) artifact: ArtifactRecord,
    pub(crate) file: Option<FileRecord>,
    /// Attribute display name → formatted value.
    pub(crate) attrs: Vec<(String, AttrValue)>,
    /// Tags on the result (Remove Result Tag menu).
    pub(crate) tags: Vec<RowTag>,
}

/// An attribute value kept typed so times follow the display preference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AttrValue {
    Text(String),
    Int(i64),
    Double(f64),
    Time(i64),
}

impl AttrValue {
    fn show(&self, prefs: &DisplayPrefs) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Double(d) => d.to_string(),
            Self::Time(t) => prefs.time(jiff::Timestamp::from_second(*t).ok()),
        }
    }

    /// Sort order: numbers and times by value, text case-insensitively;
    /// different kinds (rare within one column) group by kind.
    fn sort_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Text(a), Self::Text(b)) => cmp_text(a, b),
            (Self::Int(a), Self::Int(b)) | (Self::Time(a), Self::Time(b)) => a.cmp(b),
            (Self::Double(a), Self::Double(b)) => a.total_cmp(b),
            #[expect(clippy::cast_precision_loss, reason = "ordering only")]
            (Self::Int(a), Self::Double(b)) => (*a as f64).total_cmp(b),
            #[expect(clippy::cast_precision_loss, reason = "ordering only")]
            (Self::Double(a), Self::Int(b)) => a.total_cmp(&(*b as f64)),
            _ => self.rank().cmp(&other.rank()),
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Int(_) | Self::Double(_) => 0,
            Self::Time(_) => 1,
            Self::Text(_) => 2,
        }
    }
}

/// Which results a result view lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ResultKey {
    /// Every result of a type.
    Type(i64),
    /// A Keyword Hits list, keyword or match.
    Keyword(pm_casedb::keyword_hits::KeywordHitFilter),
}

/// The results of one type.
#[derive(Debug, Clone, Default)]
pub(crate) struct ResultRows {
    /// Attribute columns, in first-seen order.
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<ResultRow>,
    pub(crate) truncated: bool,
}

/// Loads up to [`MAX_ROWS`] results of `type_id` with their attributes
/// and source files.
pub(crate) fn load_rows(case: &Case, key: &ResultKey) -> Result<ResultRows, String> {
    let db = case.db();
    let arts = match key {
        ResultKey::Type(type_id) => db.artifacts_of_type(*type_id, None, 0, MAX_ROWS + 1),
        ResultKey::Keyword(f) => db.keyword_hits_in(f, 0, MAX_ROWS + 1),
    }
    .map_err(|e| e.to_string())?;
    let truncated = arts.len() > MAX_ROWS;
    let mut out = ResultRows {
        truncated,
        ..ResultRows::default()
    };
    let ids: Vec<i64> = arts.iter().take(MAX_ROWS).map(|a| a.id).collect();
    let tags = db.artifact_tags_of(&ids).map_err(|e| e.to_string())?;
    for artifact in arts.into_iter().take(MAX_ROWS) {
        let attrs: Vec<(String, AttrValue)> = db
            .attributes(artifact.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|a| {
                let v = match a.value {
                    Value::DateTime(t) => AttrValue::Time(t),
                    Value::Int(i) => AttrValue::Int(i),
                    Value::Double(d) => AttrValue::Double(d),
                    other => AttrValue::Text(other.display()),
                };
                (a.display_name, v)
            })
            .collect();
        for (name, _) in &attrs {
            if !out.columns.contains(name) {
                out.columns.push(name.clone());
            }
        }
        let file = match artifact.file_id {
            Some(id) => db.file(id).map_err(|e| e.to_string())?,
            None => None,
        };
        let tags = tags
            .iter()
            .filter(|t| t.artifact_id == artifact.id)
            .map(|t| RowTag {
                id: t.id,
                tag_name_id: t.tag_name_id,
                name: t.tag_name.clone(),
                comment: t.comment.clone(),
            })
            .collect();
        out.rows.push(ResultRow {
            artifact,
            file,
            attrs,
            tags,
        });
    }
    Ok(out)
}

/// The query that finds a keyword hit result again (for the Extracted
/// Text viewer); `None` for other results and hits in an artifact's text.
pub(crate) fn keyword_query(r: &ResultRow) -> Option<pm_search::TextQuery> {
    if Some(r.artifact.type_id) != pm_casedb::catalog::artifact_type_id("KAAMOS_KEYWORD_HIT") {
        return None;
    }
    let attr = |name: &str| r.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v);
    if attr("Associated Artifact").is_some() {
        return None;
    }
    let text = |name: &str| match attr(name) {
        Some(AttrValue::Text(t)) => Some(t.as_str()),
        _ => None,
    };
    let search_type = match attr("Keyword Search Type") {
        Some(AttrValue::Int(i)) => Some(*i),
        _ => None,
    };
    Some(pm_search::TextQuery::for_keyword_hit(
        text("Keyword")?,
        search_type,
        text("Keyword Regular Expression"),
    ))
}

fn score_text(s: Option<Significance>) -> &'static str {
    s.map_or("", Significance::name)
}

/// A result-table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultCol {
    SourceFile,
    Score,
    Configuration,
    Justification,
    /// Index into [`ResultRows::columns`].
    Attr(usize),
    Path,
}

impl ResultCol {
    fn title(self, data: &ResultRows) -> &str {
        match self {
            Self::SourceFile => "Source File",
            Self::Score => "Score",
            Self::Configuration => "Configuration",
            Self::Justification => "Justification",
            Self::Attr(i) => data.columns.get(i).map_or("", String::as_str),
            Self::Path => "Path",
        }
    }

    /// Ascending order of two rows by this column.
    fn cmp<'a>(self, data: &ResultRows, a: &'a ResultRow, b: &'a ResultRow) -> Ordering {
        fn name(r: &ResultRow) -> &str {
            r.file.as_ref().map_or("", |f| f.name.as_str())
        }
        match self {
            Self::SourceFile => cmp_text(name(a), name(b)),
            Self::Score => a.artifact.significance.cmp(&b.artifact.significance),
            Self::Configuration => cmp_opt_text(
                a.artifact.configuration.as_deref(),
                b.artifact.configuration.as_deref(),
            ),
            Self::Justification => cmp_opt_text(
                a.artifact.justification.as_deref(),
                b.artifact.justification.as_deref(),
            ),
            Self::Attr(col) => {
                let Some(name) = data.columns.get(col) else {
                    return Ordering::Equal;
                };
                let first =
                    |r: &'a ResultRow| r.attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v);
                match (first(a), first(b)) {
                    (Some(x), Some(y)) => x.sort_cmp(y),
                    (x, y) => x.is_some().cmp(&y.is_some()),
                }
            }
            Self::Path => cmp_text(
                &a.file.as_ref().map_or_else(String::new, FileRecord::path),
                &b.file.as_ref().map_or_else(String::new, FileRecord::path),
            ),
        }
    }
}

/// Missing values first, then case-insensitive text.
fn cmp_opt_text(a: Option<&str>, b: Option<&str>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => cmp_text(x, y),
        (x, y) => x.is_some().cmp(&y.is_some()),
    }
}

/// The columns shown for `data`.
fn columns(data: &ResultRows) -> Vec<ResultCol> {
    let mut cols = vec![ResultCol::SourceFile];
    if data.rows.iter().any(|r| r.artifact.significance.is_some()) {
        cols.push(ResultCol::Score);
    }
    if data.rows.iter().any(|r| r.artifact.configuration.is_some()) {
        cols.push(ResultCol::Configuration);
    }
    if data.rows.iter().any(|r| r.artifact.justification.is_some()) {
        cols.push(ResultCol::Justification);
    }
    cols.extend((0..data.columns.len()).map(ResultCol::Attr));
    cols.push(ResultCol::Path);
    cols
}

/// Which item a tag menu choice is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagFor {
    Result,
    SourceFile,
}

/// What happened in the result table this frame (row indices into
/// `data.rows`).
#[derive(Debug, Default)]
pub(crate) struct ResultEvents {
    pub(crate) clicked: Option<usize>,
    /// A tag menu choice for the selected rows.
    pub(crate) tag: Option<(Vec<usize>, TagFor, TagRequest)>,
    /// The selected rows this frame (for Ctrl+B).
    pub(crate) marked: Vec<usize>,
    /// Pivot on a value of the row.
    pub(crate) pivot: Option<String>,
    /// Add Note… about the row.
    pub(crate) note: Option<usize>,
    /// View in Timeline around the row's time: (time, seconds either side).
    pub(crate) timeline: Option<(i64, i64)>,
}

/// A sortable, horizontally scrolling table of results.
#[derive(Debug, Default)]
pub(crate) struct ResultTable {
    sort: Option<Sort>,
    order: Order,
    /// Selected rows by artifact id: kept when the rows reload (after
    /// tagging); another listing gets a new table.
    sel: crate::selection::Selection<i64>,
    /// A result to focus and scroll to once its row appears.
    reveal: Option<i64>,
}

impl ResultTable {
    /// Focuses result `artifact_id` (and scrolls to it) when it is shown.
    pub(crate) fn reveal(&mut self, artifact_id: i64) {
        self.reveal = Some(artifact_id);
    }

    /// Draws the table; `selected` and the returned clicked row are
    /// indices into `data.rows`.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        data: &ResultRows,
        prefs: &DisplayPrefs,
        selected: Option<usize>,
        tags: Option<&TagCatalog>,
    ) -> ResultEvents {
        ui.style_mut().interaction.selectable_labels = false;
        ui.horizontal(|ui| {
            ui.strong(title);
            ui.weak(format!(
                "{} results{}",
                grouped(data.rows.len() as u64),
                if data.truncated {
                    format!(" (first {} shown)", grouped(MAX_ROWS as u64))
                } else {
                    String::new()
                }
            ));
        });
        let cols = columns(data);
        let order = self.order.get(
            std::ptr::from_ref(data) as usize,
            data.rows.len(),
            self.sort,
            |a, b, c| match (data.rows.get(a), data.rows.get(b), cols.get(c)) {
                (Some(a), Some(b), Some(col)) => col.cmp(data, a, b),
                _ => Ordering::Equal,
            },
        );
        // Artifact ids in display order.
        let keys: Vec<i64> = order
            .iter()
            .filter_map(|&i| data.rows.get(i).map(|r| r.artifact.id))
            .collect();
        if !ui.ctx().egui_wants_keyboard_input()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
        {
            self.sel.select_all(&keys);
        }
        let mut ev = ResultEvents::default();
        let mut scroll_to = None;
        if let Some(id) = self.reveal
            && let Some(p) = keys.iter().position(|k| *k == id)
        {
            self.reveal = None;
            self.sel.select_one(id);
            ev.clicked = order.get(p).copied();
            scroll_to = Some(p);
        }
        let mut sort = self.sort;
        crate::sortable::hscroll(ui, "results-h", |ui| {
            let mut table = TableBuilder::new(ui)
                .id_salt("results")
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for c in &cols {
                table = table.column(match c {
                    ResultCol::SourceFile => Column::initial(200.0).at_least(80.0).clip(true),
                    ResultCol::Score => Column::initial(110.0).clip(true),
                    ResultCol::Configuration => Column::initial(140.0).clip(true),
                    ResultCol::Justification => Column::initial(200.0).clip(true),
                    ResultCol::Attr(_) => Column::initial(180.0).at_least(60.0).clip(true),
                    ResultCol::Path => Column::remainder().at_least(240.0).clip(true),
                });
            }
            if let Some(p) = scroll_to {
                table = table.scroll_to_row(p, None);
            }
            table
                .header(22.0, |mut h| {
                    for (i, c) in cols.iter().enumerate() {
                        h.col(|ui| {
                            crate::sortable::header_mut(ui, c.title(data), &mut sort, i);
                        });
                    }
                })
                .body(|body| {
                    body.rows(20.0, order.len(), |mut row| {
                        let Some(i) = order.get(row.index()).copied() else {
                            return;
                        };
                        let Some(r) = data.rows.get(i) else { return };
                        row.set_selected(self.sel.contains(r.artifact.id) || selected == Some(i));
                        for c in &cols {
                            row.col(|ui| cell(ui, data, r, *c, prefs));
                        }
                        let resp = row.response();
                        if resp.clicked() {
                            let m = resp.ctx.input(|i| i.modifiers);
                            self.sel.click(&keys, row.index(), m);
                            ev.clicked = Some(i);
                        }
                        if let Some(cat) = tags {
                            resp.context_menu(|ui| {
                                // Right-clicking outside the selection
                                // selects that row; inside it, the menu
                                // acts on them all.
                                if !self.sel.contains(r.artifact.id) {
                                    self.sel.select_one(r.artifact.id);
                                }
                                self.sel.focus = Some(r.artifact.id);
                                ev.clicked = Some(i);
                                let marked: Vec<usize> = self
                                    .sel
                                    .positions(&keys)
                                    .into_iter()
                                    .filter_map(|p| order.get(p).copied())
                                    .collect();
                                let n = marked.len();
                                let label = |what: &str| {
                                    if n > 1 {
                                        format!("{what} ({n} results)")
                                    } else {
                                        what.to_owned()
                                    }
                                };
                                if let Some(t) =
                                    crate::tags_ui::add_menu(ui, &label("Add Result Tag"), cat)
                                {
                                    ev.tag = Some((marked.clone(), TagFor::Result, t));
                                }
                                if let Some(t) = crate::tags_ui::remove_menu(
                                    ui,
                                    &label("Remove Result Tag"),
                                    &r.tags,
                                ) {
                                    ev.tag = Some((marked.clone(), TagFor::Result, t));
                                }
                                if r.file.is_some() {
                                    ui.separator();
                                    if let Some(t) =
                                        crate::tags_ui::add_menu(ui, &label("Add File Tag"), cat)
                                    {
                                        ev.tag = Some((marked.clone(), TagFor::SourceFile, t));
                                    }
                                }
                                ui.separator();
                                if ui.button("Add Note…").clicked() {
                                    ev.note = Some(i);
                                    ui.close();
                                }
                                // The result's first time attribute.
                                let when = r.attrs.iter().find_map(|(_, v)| match v {
                                    AttrValue::Time(t) => Some(*t),
                                    _ => None,
                                });
                                ui.add_enabled_ui(when.is_some(), |ui| {
                                    ui.menu_button("View in Timeline", |ui| {
                                        for (label, pad) in crate::table::TIMELINE_CONTEXT {
                                            if ui.button(label).clicked() {
                                                ev.timeline = when.map(|t| (t, pad));
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                                ui.menu_button("Pivot on", |ui| {
                                    for (name, v) in &r.attrs {
                                        if let AttrValue::Text(t) = v
                                            && !t.is_empty()
                                        {
                                            let short: String = t.chars().take(60).collect();
                                            if ui.button(format!("{name}: {short}")).clicked() {
                                                ev.pivot = Some(t.clone());
                                                ui.close();
                                            }
                                        }
                                    }
                                    if let Some(f) = &r.file
                                        && ui.button(format!("Source file: {}", f.name)).clicked()
                                    {
                                        ev.pivot = Some(f.name.clone());
                                        ui.close();
                                    }
                                });
                            });
                        }
                    });
                });
        });
        self.sort = sort;
        ev.marked = self
            .sel
            .positions(&keys)
            .into_iter()
            .filter_map(|p| order.get(p).copied())
            .collect();
        ev
    }
}

fn cell(ui: &mut egui::Ui, data: &ResultRows, r: &ResultRow, c: ResultCol, prefs: &DisplayPrefs) {
    match c {
        ResultCol::SourceFile => {
            ui.label(r.file.as_ref().map_or("", |f| f.name.as_str()));
        }
        ResultCol::Score => {
            let s = r.artifact.significance;
            let text = egui::RichText::new(score_text(s));
            ui.label(match s {
                Some(Significance::Notable) => text.color(ui.visuals().error_fg_color),
                Some(Significance::LikelyNotable) => text.color(ui.visuals().warn_fg_color),
                _ => text,
            });
        }
        ResultCol::Configuration => {
            ui.label(r.artifact.configuration.as_deref().unwrap_or(""));
        }
        ResultCol::Justification => {
            ui.label(r.artifact.justification.as_deref().unwrap_or(""));
        }
        ResultCol::Attr(i) => {
            let Some(name) = data.columns.get(i) else {
                return;
            };
            let v: Vec<String> = r
                .attrs
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, v)| v.show(prefs))
                .collect();
            ui.label(v.join("; "));
        }
        ResultCol::Path => {
            ui.monospace(r.file.as_ref().map_or_else(String::new, FileRecord::path));
        }
    }
}

/// Data Artifacts / Analysis Results data shown by the tree and the result
/// tables, refreshed from the case change bus.
#[derive(Debug, Default)]
pub(crate) struct ResultData {
    pub(crate) counts: Counts,
    pub(crate) rows: std::collections::HashMap<ResultKey, Task<Result<Arc<ResultRows>, String>>>,
    /// Keyword Hits groups, and the hit count they were loaded for.
    pub(crate) keyword_groups: crate::task::Cached<crate::keyword_search_ui::HitTree>,
    pub(crate) keyword_groups_for: u64,
}

impl ResultData {
    /// Reacts to committed case changes.
    pub(crate) fn on_change(&mut self, watch: crate::changes::Watch, due: crate::changes::Due) {
        use crate::changes::Watch;
        match watch {
            Watch::Results => {
                self.counts.invalidate();
                self.keyword_groups.refresh();
                if due.rows {
                    self.rows.clear();
                }
            }
            // Tags and scores show in the rows.
            Watch::Annotations if due.rows => self.rows.clear(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_times_follow_prefs() {
        let prefs = DisplayPrefs::default();
        assert_eq!(AttrValue::Text("x".into()).show(&prefs), "x");
        assert!(AttrValue::Time(0).show(&prefs).starts_with("1970-01-01"));
    }
}
