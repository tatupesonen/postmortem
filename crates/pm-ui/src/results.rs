//! Analysis results and data artifacts from the case database (Autopsy's
//! "Data Artifacts" / "Analysis Results" tree and result viewer).

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_extras::{Column, TableBuilder};
use pm_case::Case;
use pm_casedb::catalog::Category;
use pm_casedb::{ArtifactRecord, FileRecord, Significance, Value};

use crate::format::{DisplayPrefs, grouped};
use crate::task::Task;

/// Rows shown per result type (the table is not paged yet).
pub(crate) const MAX_ROWS: usize = 20_000;
/// How often counts refresh while analysis is running.
const REFRESH: Duration = Duration::from_secs(2);

/// A result type with its number of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypeCount {
    pub(crate) type_id: i64,
    /// Type name (`TSK_WEB_HISTORY`), for the tree icon.
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
}

/// An attribute value kept typed so times follow the display preference.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AttrValue {
    Text(String),
    Time(i64),
}

impl AttrValue {
    fn show(&self, prefs: &DisplayPrefs) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Time(t) => prefs.time(jiff::Timestamp::from_second(*t).ok()),
        }
    }
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
pub(crate) fn load_rows(case: &Case, type_id: i64) -> Result<ResultRows, String> {
    let db = case.db();
    let arts = db
        .artifacts_of_type(type_id, None, 0, MAX_ROWS + 1)
        .map_err(|e| e.to_string())?;
    let truncated = arts.len() > MAX_ROWS;
    let mut out = ResultRows {
        truncated,
        ..ResultRows::default()
    };
    for artifact in arts.into_iter().take(MAX_ROWS) {
        let attrs: Vec<(String, AttrValue)> = db
            .attributes(artifact.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|a| {
                let v = match a.value {
                    Value::DateTime(t) => AttrValue::Time(t),
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
        out.rows.push(ResultRow {
            artifact,
            file,
            attrs,
        });
    }
    Ok(out)
}

fn score_text(s: Option<Significance>) -> &'static str {
    s.map_or("", Significance::name)
}

/// Draws the result table; returns the row clicked this frame.
pub(crate) fn table_ui(
    ui: &mut egui::Ui,
    title: &str,
    data: &ResultRows,
    prefs: &DisplayPrefs,
    selected: Option<usize>,
) -> Option<usize> {
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
    let has_score = data.rows.iter().any(|r| r.artifact.significance.is_some());
    let has_conf = data.rows.iter().any(|r| r.artifact.configuration.is_some());
    let has_just = data.rows.iter().any(|r| r.artifact.justification.is_some());
    let mut clicked = None;
    let mut table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(200.0).at_least(80.0).clip(true));
    if has_score {
        table = table.column(Column::initial(110.0).clip(true));
    }
    if has_conf {
        table = table.column(Column::initial(140.0).clip(true));
    }
    if has_just {
        table = table.column(Column::initial(200.0).clip(true));
    }
    for _ in &data.columns {
        table = table.column(Column::initial(180.0).at_least(60.0).clip(true));
    }
    table = table.column(Column::remainder().at_least(120.0).clip(true));
    table
        .header(22.0, |mut h| {
            h.col(|ui| {
                ui.strong("Source File");
            });
            if has_score {
                h.col(|ui| {
                    ui.strong("Score");
                });
            }
            if has_conf {
                h.col(|ui| {
                    ui.strong("Configuration");
                });
            }
            if has_just {
                h.col(|ui| {
                    ui.strong("Justification");
                });
            }
            for c in &data.columns {
                h.col(|ui| {
                    ui.strong(c);
                });
            }
            h.col(|ui| {
                ui.strong("Path");
            });
        })
        .body(|body| {
            body.rows(20.0, data.rows.len(), |mut row| {
                let i = row.index();
                let Some(r) = data.rows.get(i) else { return };
                row.set_selected(selected == Some(i));
                let name = r.file.as_ref().map_or("", |f| f.name.as_str());
                row.col(|ui| {
                    ui.label(name);
                });
                if has_score {
                    row.col(|ui| {
                        let s = r.artifact.significance;
                        let text = egui::RichText::new(score_text(s));
                        ui.label(match s {
                            Some(Significance::Notable) => text.color(ui.visuals().error_fg_color),
                            Some(Significance::LikelyNotable) => {
                                text.color(ui.visuals().warn_fg_color)
                            }
                            _ => text,
                        });
                    });
                }
                if has_conf {
                    row.col(|ui| {
                        ui.label(r.artifact.configuration.as_deref().unwrap_or(""));
                    });
                }
                if has_just {
                    row.col(|ui| {
                        ui.label(r.artifact.justification.as_deref().unwrap_or(""));
                    });
                }
                for c in &data.columns {
                    row.col(|ui| {
                        let v: Vec<String> = r
                            .attrs
                            .iter()
                            .filter(|(n, _)| n == c)
                            .map(|(_, v)| v.show(prefs))
                            .collect();
                        ui.label(v.join("; "));
                    });
                }
                row.col(|ui| {
                    ui.monospace(r.file.as_ref().map_or_else(String::new, FileRecord::path));
                });
                if row.response().clicked() {
                    clicked = Some(i);
                }
            });
        });
    clicked
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
