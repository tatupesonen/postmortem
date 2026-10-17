//! Ad-hoc keyword search (Autopsy's toolbar "Keyword Search" panel,
//! `DropdownSingleTermSearchPanel`) and its result views ("Keyword search
//! N - term"), with "Save search results".
//!
//! Searches run on a background task against the case's content index
//! (`pm_search::CaseIndex`); results list one row per file with Autopsy's
//! "Keyword Preview".

use std::collections::BTreeSet;
use std::sync::Arc;

use pm_case::Case;
use pm_search::{ObjRef, SearchType, TextQuery};

use crate::model::FileRow;

/// Rows per search (like File Search: narrow the term for more).
pub(crate) const MAX_ROWS: usize = crate::file_search_ui::MAX_ROWS;

/// Module name recorded on saved keyword hits.
const MODULE: &str = "Keyword Search";

/// Autopsy's warning for `^`/`$` in a regular expression.
const BOUNDARY_WARNING: &str = "Boundary characters ^ and $ do not match word boundaries. Consider\nreplacing with an explicit list of boundary characters, such as [ \\.,]";

/// A search requested from the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeywordRequest {
    pub(crate) query: TextQuery,
    pub(crate) save: bool,
}

/// What the panel asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PanelAction {
    None,
    Search(KeywordRequest),
    Close,
}

/// The toolbar's Keyword Search panel.
#[derive(Debug)]
pub(crate) struct KeywordSearchPanel {
    term: String,
    search_type: SearchType,
    restrict: bool,
    ds_ids: BTreeSet<i64>,
    save: bool,
    error: Option<String>,
    warning: Option<String>,
    /// Waiting for "proceed anyway?" while ingest runs.
    confirm: Option<KeywordRequest>,
}

impl Default for KeywordSearchPanel {
    fn default() -> Self {
        Self {
            term: String::new(),
            search_type: SearchType::Exact,
            restrict: false,
            ds_ids: BTreeSet::new(),
            save: false,
            error: None,
            warning: None,
            confirm: None,
        }
    }
}

/// Autopsy's checks before a search; the warning does not block it.
pub(crate) fn validate(term: &str, st: SearchType) -> Result<Option<String>, String> {
    let term = term.trim();
    if term.is_empty() {
        return Err("Keyword list is empty, please add at least one keyword to the list".into());
    }
    pm_search::matcher::Matcher::single(term, st).map_err(|e| {
        format!(
            "Invalid query statement. If this is a regular expression, only Lucene regular expression patterns are supported. POSIX character classes (such as \\n or \\w) are not.\n{}",
            e.error
        )
    })?;
    let warn = st == SearchType::Regex
        && (term.starts_with('^') || (term.ends_with('$') && !term.ends_with("\\$")));
    Ok(warn.then(|| BOUNDARY_WARNING.to_owned()))
}

impl KeywordSearchPanel {
    /// Draws the panel. `sources` are `(id, name)`; `indexed` is the
    /// "Files Indexed" count once loaded; `ingesting` asks before searching
    /// a partial index.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        sources: &[(i64, String)],
        indexed: Option<u64>,
        ingesting: bool,
    ) -> PanelAction {
        let mut action = PanelAction::None;
        let mut open = true;
        egui::Window::new("Keyword Search")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 64.0))
            .show(ctx, |ui| {
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.term)
                        .desired_width(320.0)
                        .hint_text("Search term"),
                );
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    for st in [SearchType::Exact, SearchType::Substring, SearchType::Regex] {
                        ui.radio_value(&mut self.search_type, st, radio_label(st));
                    }
                });
                ui.checkbox(
                    &mut self.restrict,
                    "Restrict search to the selected data sources:",
                );
                ui.add_enabled_ui(self.restrict, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(90.0)
                        .show(ui, |ui| {
                            for (id, name) in sources {
                                let mut on = self.ds_ids.contains(id);
                                if ui.checkbox(&mut on, name).changed() {
                                    if on {
                                        self.ds_ids.insert(*id);
                                    } else {
                                        self.ds_ids.remove(id);
                                    }
                                }
                            }
                        });
                });
                ui.checkbox(&mut self.save, "Save search results")
                    .on_hover_text("Saving will add Keyword Hit Results to the current case");
                if let Some(w) = &self.warning {
                    ui.colored_label(ui.visuals().warn_fg_color, w);
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    if ui.button("Search").clicked() || enter {
                        match self.request() {
                            Ok(r) if ingesting => self.confirm = Some(r),
                            Ok(r) => action = PanelAction::Search(r),
                            Err(e) => self.error = Some(e),
                        }
                    }
                    let label = match indexed {
                        Some(n) if ingesting => format!(
                            "Files Indexed: {} (ingest is ongoing)",
                            crate::format::grouped(n)
                        ),
                        Some(n) => format!("Files Indexed: {}", crate::format::grouped(n)),
                        None => "Files Indexed:".to_owned(),
                    };
                    ui.weak(label);
                });
            });
        if let Some(r) = self.confirm.clone() {
            egui::Modal::new(egui::Id::new("kws-ingest-running")).show(ctx, |ui| {
                ui.heading("Keyword Search Ingest in Progress");
                ui.label(
                    "Keyword Search Ingest is currently running.\nNot all files have been indexed and this search might yield incomplete results.\nDo you want to proceed with this search anyway?",
                );
                ui.horizontal(|ui| {
                    if ui.button("Yes").clicked() {
                        action = PanelAction::Search(r);
                        self.confirm = None;
                    }
                    if ui.button("No").clicked() {
                        self.confirm = None;
                    }
                });
            });
        }
        if !open {
            action = PanelAction::Close;
        }
        action
    }

    fn request(&mut self) -> Result<KeywordRequest, String> {
        self.error = None;
        self.warning = validate(&self.term, self.search_type)?;
        Ok(KeywordRequest {
            query: TextQuery {
                term: self.term.trim().to_owned(),
                search_type: self.search_type,
                ds_ids: if self.restrict {
                    self.ds_ids.iter().copied().collect()
                } else {
                    Vec::new()
                },
            },
            save: self.save,
        })
    }
}

fn radio_label(st: SearchType) -> &'static str {
    match st {
        SearchType::Exact => "Exact Match",
        SearchType::Substring => "Substring Match",
        SearchType::Regex => "Regular Expression",
    }
}

/// Files in the content index ("Files Indexed: N").
pub(crate) fn indexed_count(case: &Case) -> Result<u64, String> {
    let ix = case.index().map_err(|e| e.to_string())?;
    ix.content().reload().map_err(|e| e.to_string())?;
    Ok(ix.content().stats().map_err(|e| e.to_string())?.objects)
}

/// Rows of a keyword search, with their previews.
#[derive(Debug, Default)]
pub(crate) struct KeywordRows {
    pub(crate) rows: Vec<FileRow>,
    pub(crate) previews: Vec<String>,
    /// More than [`MAX_ROWS`] objects matched.
    pub(crate) more: bool,
    /// Keyword hit results added by "Save search results".
    pub(crate) saved: usize,
}

/// Runs a search (in a task): one row per matching file (a hit in an
/// artifact's text lists the artifact's file), and saves the hits when
/// asked.
pub(crate) fn run(
    case: &Case,
    req: &KeywordRequest,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<KeywordRows, String> {
    let ix = case.index().map_err(|e| e.to_string())?;
    let content = ix.content();
    content.reload().map_err(|e| e.to_string())?;
    if content.stats().map_err(|e| e.to_string())?.objects == 0 {
        return Err(
            "No files are in index yet.\nIf keyword search indexing was enabled, wait for ingest to complete".into(),
        );
    }
    let db = case.db();
    let mut out = KeywordRows::default();
    let mut records = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut after = None;
    let now = jiff::Timestamp::now().as_second();
    loop {
        let page = content
            .search(&req.query, after, 200, cancelled)
            .map_err(|e| match e {
                pm_search::SearchError::InvalidQuery(m) => format!("Invalid query statement. {m}"),
                e => format!("Could not perform the query {}: {e}", req.query.term),
            })?;
        for hit in &page.hits {
            let (file_id, artifact) = match hit.obj {
                ObjRef::File(id) => (Some(id), None),
                ObjRef::Artifact(id) => {
                    let a = db.artifact(id).map_err(|e| e.to_string())?;
                    (a.as_ref().and_then(|a| a.file_id), a)
                }
            };
            let Some(file_id) = file_id else { continue };
            let Some(rec) = db.file(file_id).map_err(|e| e.to_string())? else {
                continue;
            };
            if req.save {
                out.saved += save_hits(case, &req.query, hit, &rec, artifact.as_ref(), now)?;
            }
            if seen.insert(file_id) {
                if records.len() == MAX_ROWS {
                    out.more = true;
                    continue;
                }
                records.push(rec);
                out.previews.push(hit.preview.clone());
            }
        }
        match page.next {
            Some(n) if !cancelled() && (req.save || !out.more) => after = Some(n),
            _ => break,
        }
    }
    out.rows = records.iter().map(crate::views::row_of).collect();
    crate::tags_ui::annotate(case, &mut out.rows)?;
    Ok(out)
}

/// Adds a keyword hit result per hit term not saved before.
fn save_hits(
    case: &Case,
    q: &TextQuery,
    hit: &pm_search::ObjectHit,
    file: &pm_casedb::FileRecord,
    artifact: Option<&pm_casedb::ArtifactRecord>,
    now: i64,
) -> Result<usize, String> {
    let db = case.db();
    let existing = db.keyword_hit_keys(file.id).map_err(|e| e.to_string())?;
    let mut added = 0;
    for term in &hit.hits {
        let (art, key) =
            pm_search::results::keyword_hit_artifact(&pm_search::results::KeywordHitSpec {
                ds_id: file.ds_id,
                file_id: Some(file.id),
                associated_artifact: artifact.map(|a| a.id),
                list_name: "",
                term: &q.term,
                search_type: q.search_type,
                selector: None,
                hit: term,
                snippet: &hit.preview,
                module: MODULE,
                created_at: now,
            });
        if existing.contains(&key) {
            continue;
        }
        db.add_artifact(&art).map_err(|e| e.to_string())?;
        added += 1;
    }
    Ok(added)
}

/// A finished or running keyword search shown in the result pane.
#[derive(Debug)]
pub(crate) struct KeywordResults {
    pub(crate) number: usize,
    pub(crate) request: Arc<KeywordRequest>,
    pub(crate) rows: crate::task::Cached<KeywordRows>,
    /// The tree was refreshed for saved hits.
    pub(crate) reported: bool,
    /// This tab's sort and selection.
    pub(crate) table: crate::table::TableState,
}

impl KeywordResults {
    /// Autopsy's result window title, "Keyword search {n} - {term}".
    pub(crate) fn title(&self) -> String {
        format!(
            "Keyword search {} - {}",
            self.number, self.request.query.term
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_follows_autopsy() {
        assert!(validate("  ", SearchType::Exact).is_err());
        assert!(validate("(", SearchType::Regex).is_err());
        assert_eq!(
            validate("C++", SearchType::Exact),
            Ok(None),
            "literals never regexes"
        );
        assert!(validate("^abc", SearchType::Regex).unwrap().is_some());
        assert!(validate("abc$", SearchType::Regex).unwrap().is_some());
        assert_eq!(validate("abc\\$", SearchType::Regex), Ok(None));
    }
}
