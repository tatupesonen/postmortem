//! Autopsy's Data Artifacts / Analysis Results and Annotations content
//! viewers for the selected file: the results posted on it (with their
//! attributes, score, conclusion and justification), and its tags, score
//! and the examiner's notes about it. Read from the case database.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::catalog::Category;
use pm_casedb::{ArtifactRecord, ObjKind, Value};

use crate::format::DisplayPrefs;
use crate::task::Keyed;

/// One result on the file.
#[derive(Debug, Clone)]
pub(crate) struct ResultOnFile {
    pub(crate) artifact: ArtifactRecord,
    pub(crate) type_name: String,
    pub(crate) analysis: bool,
    pub(crate) attrs: Vec<(String, Value)>,
}

/// Everything shown for a file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Related {
    pub(crate) results: Vec<ResultOnFile>,
    pub(crate) tags: Vec<pm_casedb::FileTag>,
    pub(crate) score: Option<pm_casedb::Score>,
    pub(crate) notes: Vec<pm_casedb::notes::Note>,
}

pub(crate) fn load(case: &Case, file_id: i64) -> Result<Related, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let names = db.artifact_type_names().map_err(err)?;
    let mut results = Vec::new();
    for a in db.artifacts_for_file(file_id).map_err(err)? {
        let (name, analysis) = names
            .iter()
            .find(|n| n.0 == a.type_id)
            .map_or((format!("type {}", a.type_id), false), |n| {
                (n.2.clone(), n.3 == Category::AnalysisResult as i64)
            });
        let attrs = db
            .attributes(a.id)
            .map_err(err)?
            .into_iter()
            .map(|at| (at.display_name, at.value))
            .collect();
        results.push(ResultOnFile {
            artifact: a,
            type_name: name,
            analysis,
            attrs,
        });
    }
    Ok(Related {
        results,
        tags: db.file_tags(file_id).map_err(err)?,
        score: Some(db.score(ObjKind::File, file_id).map_err(err)?),
        notes: db.notes_about(ObjKind::File, file_id).map_err(err)?,
    })
}

/// The Results and Annotations tabs' state.
#[derive(Debug, Default)]
pub(crate) struct RelatedView {
    data: Keyed<i64, Related>,
}

/// What a click in the view asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Open {
    Artifact(i64),
}

impl RelatedView {
    fn get(&mut self, ctx: &egui::Context, case: &Arc<Case>, file_id: i64) -> Option<Arc<Related>> {
        let case = Arc::clone(case);
        self.data.get(ctx, &file_id, move || load(&case, file_id))
    }

    /// Results posted on the file: data artifacts, then analysis results.
    pub(crate) fn results_ui(
        &mut self,
        ui: &mut egui::Ui,
        case: &Arc<Case>,
        file_id: i64,
        prefs: &DisplayPrefs,
    ) -> Option<Open> {
        let Some(d) = self.get(ui.ctx(), case, file_id) else {
            ui.spinner();
            return None;
        };
        if d.results.is_empty() {
            ui.weak("No data artifacts or analysis results for this file.");
            return None;
        }
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for analysis in [false, true] {
                    let of: Vec<&ResultOnFile> = d
                        .results
                        .iter()
                        .filter(|r| r.analysis == analysis)
                        .collect();
                    if of.is_empty() {
                        continue;
                    }
                    ui.strong(if analysis {
                        "Analysis Results"
                    } else {
                        "Data Artifacts"
                    });
                    for r in of {
                        egui::CollapsingHeader::new(&r.type_name)
                            .id_salt(("related-result", r.artifact.id))
                            .default_open(true)
                            .show(ui, |ui| {
                                egui::Grid::new(("related-grid", r.artifact.id))
                                    .num_columns(2)
                                    .striped(true)
                                    .show(ui, |ui| {
                                        if analysis {
                                            ui.weak("Score");
                                            ui.label(
                                                r.artifact
                                                    .significance
                                                    .map_or("—", pm_casedb::Significance::name),
                                            );
                                            ui.end_row();
                                            for (k, v) in [
                                                ("Conclusion", &r.artifact.conclusion),
                                                ("Configuration", &r.artifact.configuration),
                                                ("Justification", &r.artifact.justification),
                                            ] {
                                                if let Some(v) =
                                                    v.as_deref().filter(|v| !v.is_empty())
                                                {
                                                    ui.weak(k);
                                                    ui.label(v);
                                                    ui.end_row();
                                                }
                                            }
                                        }
                                        for (name, v) in &r.attrs {
                                            ui.weak(name);
                                            ui.label(match v {
                                                Value::DateTime(t) => prefs
                                                    .time(jiff::Timestamp::from_second(*t).ok()),
                                                other => other.display(),
                                            });
                                            ui.end_row();
                                        }
                                    });
                                if ui.small_button("Go to result").clicked() {
                                    open = Some(Open::Artifact(r.artifact.id));
                                }
                            });
                    }
                    ui.add_space(6.0);
                }
            });
        open
    }

    /// Tags, score and notes about the file.
    pub(crate) fn annotations_ui(
        &mut self,
        ui: &mut egui::Ui,
        case: &Arc<Case>,
        file_id: i64,
        prefs: &DisplayPrefs,
    ) {
        let Some(d) = self.get(ui.ctx(), case, file_id) else {
            ui.spinner();
            return;
        };
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if let Some(s) = d.score {
                    ui.horizontal(|ui| {
                        ui.strong("Score:");
                        ui.label(s.significance.name());
                    });
                }
                ui.add_space(4.0);
                ui.strong(format!("Tags ({})", d.tags.len()));
                if d.tags.is_empty() {
                    ui.weak("No tags.");
                }
                egui::Grid::new("annotation-tags")
                    .num_columns(3)
                    .striped(true)
                    .show(ui, |ui| {
                        for t in &d.tags {
                            ui.label(&t.tag_name);
                            ui.label(&t.comment);
                            ui.weak(&t.examiner);
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                ui.strong(format!("Notes ({})", d.notes.len()));
                if d.notes.is_empty() {
                    ui.weak("No notes. Add Note… in the row's menu writes one.");
                }
                for n in &d.notes {
                    ui.group(|ui| {
                        ui.set_width(ui.available_width());
                        ui.weak(format!(
                            "{}{} · {}",
                            if n.finding { "Finding · " } else { "" },
                            n.examiner,
                            prefs.time(jiff::Timestamp::from_second(n.updated_at).ok())
                        ));
                        ui.label(&n.text);
                    });
                }
            });
    }
}
