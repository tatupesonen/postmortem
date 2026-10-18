//! Command palette (Ctrl+K): jump to any tool, tree node, file or result
//! by typing part of its name. `file:123`, `artifact:45` and `ds:1` (the
//! references the MCP server cites) open that item directly.

use std::sync::Arc;

use pm_case::Case;

use crate::model::NodeId;
use crate::task::Keyed;

/// Most entries listed at once.
const MAX_SHOWN: usize = 40;
/// File-name matches fetched from the name index.
const MAX_FILES: usize = 20;

/// A tool or window the palette can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    NewCase,
    OpenCase,
    AddDataSource,
    Timeline,
    Gallery,
    Discovery,
    Communications,
    Geolocation,
    GenerateReport,
    KeywordSearch,
    FileSearch,
    IngestProgress,
    Plugins,
    Options,
    Notes,
    IocSearch,
}

impl Tool {
    const ALL: [(Self, &'static str); 16] = [
        (Self::NewCase, "New Case…"),
        (Self::OpenCase, "Open Case…"),
        (Self::AddDataSource, "Add Data Source…"),
        (Self::Timeline, "Timeline"),
        (Self::Gallery, "Image Gallery"),
        (Self::Discovery, "Discovery"),
        (Self::Communications, "Communications"),
        (Self::Geolocation, "Geolocation"),
        (Self::GenerateReport, "Generate Report…"),
        (Self::KeywordSearch, "Keyword Search"),
        (Self::FileSearch, "File Search by Attributes"),
        (Self::IngestProgress, "Ingest Progress"),
        (Self::Plugins, "Plugins"),
        (Self::Options, "Options"),
        (Self::Notes, "Notes"),
        (Self::IocSearch, "IOC Search (hashes, IPs, domains, names)"),
    ];
}

/// What choosing an entry does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Tool(Tool),
    Navigate(NodeId),
    RevealFile(i64),
    RevealArtifact(i64),
    /// Everything about a value.
    Pivot(String),
}

/// One choice in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) label: String,
    /// Where it is (shown dimmed after the label).
    pub(crate) detail: String,
    pub(crate) action: Action,
}

impl Entry {
    pub(crate) fn new(label: impl Into<String>, detail: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            action,
        }
    }
}

/// The tools, in menu order.
pub(crate) fn tool_entries() -> Vec<Entry> {
    Tool::ALL
        .iter()
        .map(|(t, name)| Entry::new(*name, "Tool", Action::Tool(*t)))
        .collect()
}

/// `file:123`, `artifact:45`, `ds:1` (also with a space after the colon).
pub(crate) fn parse_reference(q: &str) -> Option<Action> {
    let (kind, id) = q.trim().split_once(':')?;
    let id: i64 = id.trim().parse().ok()?;
    match kind.trim().to_ascii_lowercase().as_str() {
        "file" => Some(Action::RevealFile(id)),
        "artifact" | "result" => Some(Action::RevealArtifact(id)),
        "ds" => Some(Action::Navigate(NodeId::DataSource(id))),
        _ => None,
    }
}

/// How well `query` matches `text`: its characters in order (case
/// insensitive), better when they are adjacent or start words; `None`
/// when they do not all occur.
pub(crate) fn score(query: &str, text: &str) -> Option<i64> {
    let q: Vec<char> = query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let Some(&first) = q.first() else {
        return Some(0);
    };
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Greedy from every place the first character occurs; the best wins.
    let best = t
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == first)
        .filter_map(|(start, _)| greedy(&q, &t, start))
        .max()?;
    // Shorter texts rank first among equals.
    Some(best * 100 - i64::try_from(t.len()).unwrap_or(i64::MAX).min(99))
}

/// The score of matching `q` in order from `t[start]`, if it matches.
fn greedy(q: &[char], t: &[char], start: usize) -> Option<i64> {
    let word_start = |i: usize| i == 0 || t.get(i - 1).is_some_and(|p| !p.is_alphanumeric());
    let mut score = 0i64;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, c) in t.iter().enumerate().skip(start) {
        let Some(want) = q.get(qi) else { break };
        if c == want {
            score += 1;
            if word_start(i) {
                score += 8;
            }
            if prev.is_some_and(|p| p + 1 == i) {
                score += 5;
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// The palette window.
#[derive(Debug, Default)]
pub(crate) struct Palette {
    open: bool,
    query: String,
    cursor: usize,
    /// File names matching the query (name index), by query.
    files: Keyed<String, Vec<Entry>>,
}

impl Palette {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.cursor = 0;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Draws the palette over `entries` (tools and tree nodes); returns
    /// the chosen action.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<Case>>,
        entries: &[Entry],
    ) -> Option<Action> {
        if !self.open {
            return None;
        }
        let mut shown: Vec<(i64, &Entry)> = entries
            .iter()
            .filter_map(|e| {
                let s = score(&self.query, &e.label).or_else(|| {
                    score(&self.query, &format!("{} {}", e.detail, e.label)).map(|s| s - 50)
                })?;
                Some((s, e))
            })
            .collect();
        shown.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        shown.truncate(MAX_SHOWN);
        let reference = parse_reference(&self.query)
            .map(|a| Entry::new(format!("Go to {}", self.query.trim()), "Reference", a));
        // File names from the name index once three characters are typed.
        let files = match case {
            Some(case) if self.query.trim().chars().count() >= 3 && reference.is_none() => {
                let case = Arc::clone(case);
                let q = self.query.trim().to_owned();
                self.files
                    .get(ctx, &q.clone(), move || find_files(&case, &q))
                    .map(|v| (*v).clone())
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let typed = self.query.trim();
        let pivot = (typed.chars().count() >= 2 && parse_reference(typed).is_none()).then(|| {
            Entry::new(
                format!("Pivot on “{typed}”"),
                "Everything about it",
                Action::Pivot(typed.to_owned()),
            )
        });
        let list: Vec<Entry> = reference
            .into_iter()
            .chain(shown.into_iter().map(|(_, e)| e.clone()))
            .chain(files)
            .chain(pivot)
            .collect();
        self.cursor = self.cursor.min(list.len().saturating_sub(1));
        let (down, up, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if down {
            self.cursor = (self.cursor + 1).min(list.len().saturating_sub(1));
        }
        if up {
            self.cursor = self.cursor.saturating_sub(1);
        }
        let mut chosen = None;
        if enter {
            chosen = list.get(self.cursor).map(|e| e.action.clone());
        }
        let modal = egui::Modal::new(egui::Id::new("command-palette")).show(ctx, |ui| {
            ui.set_width(560.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(
                        "Go to a tool, view, result type, tag, file, or file:123 / artifact:45",
                    )
                    .desired_width(f32::INFINITY),
            );
            edit.request_focus();
            if edit.changed() {
                self.cursor = 0;
            }
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    if list.is_empty() {
                        ui.weak("Nothing matches.");
                    }
                    for (i, e) in list.iter().enumerate() {
                        let on = i == self.cursor;
                        let r = ui
                            .horizontal(|ui| {
                                let r = ui.selectable_label(on, &e.label);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.weak(&e.detail);
                                    },
                                );
                                r
                            })
                            .inner;
                        if on {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(e.action.clone());
                        }
                    }
                });
        });
        if chosen.is_some() || esc || modal.should_close() {
            self.open = false;
        }
        chosen
    }
}

/// Files whose name contains `q`, from the case's file-name index.
fn find_files(case: &Case, q: &str) -> Result<Vec<Entry>, String> {
    let ix = case.index().map_err(|e| e.to_string())?;
    let query = pm_search::NameQuery {
        text: q.to_owned(),
        ..pm_search::NameQuery::default()
    };
    let page = ix
        .names()
        .search(&query, None, MAX_FILES)
        .map_err(|e| e.to_string())?;
    let files = case
        .db()
        .files_by_ids(&page.ids)
        .map_err(|e| e.to_string())?;
    Ok(files
        .into_iter()
        .map(|f| {
            Entry::new(
                f.name.clone(),
                format!("File {}", f.parent_path),
                Action::RevealFile(f.id),
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_scores_prefer_word_starts_and_runs() {
        assert!(score("tl", "Timeline").is_some());
        assert!(score("xyz", "Timeline").is_none());
        let gal = score("gal", "Image Gallery").unwrap();
        let spread = score("gal", "Geolocation Analysis").unwrap_or(i64::MIN);
        assert!(gal > spread, "contiguous word start wins");
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn references_parse() {
        assert_eq!(parse_reference("file:123"), Some(Action::RevealFile(123)));
        assert_eq!(
            parse_reference(" artifact: 45 "),
            Some(Action::RevealArtifact(45))
        );
        assert_eq!(
            parse_reference("ds:2"),
            Some(Action::Navigate(NodeId::DataSource(2)))
        );
        assert_eq!(parse_reference("file:abc"), None);
        assert_eq!(parse_reference("timeline"), None);
    }
}
