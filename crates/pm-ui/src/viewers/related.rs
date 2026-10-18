//! Autopsy's Data Artifacts / Analysis Results, Annotations and Context
//! content viewers for the selected file: the results posted on it (with
//! their attributes, score, conclusion and justification); its tags, score
//! and the examiner's notes about it; and where it came from and how it
//! was used, from the results that point at it (`KAAMOS_ASSOCIATED_OBJECT`).
//! Read from the case database.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::catalog::Category;
use pm_casedb::{ArtifactRecord, AttributeRecord, ObjKind, Value};

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

/// Longest attribute text shown in a context line (Autopsy abbreviates
/// at 200 characters).
const CONTEXT_VALUE_MAX: usize = 200;

/// One line of the Context tab: a result that says where the file came
/// from (Source) or how it was used (Usage).
#[derive(Debug, Clone)]
pub(crate) struct ContextEntry {
    pub(crate) usage: bool,
    /// "Downloaded from", "Attached to", "Recent Documents", "Program Execution".
    pub(crate) heading: &'static str,
    /// "Email", "Message" or nothing, before the fields.
    pub(crate) prefix: Option<&'static str>,
    /// Labelled values, e.g. `("URL", Text(..))`, `("On", DateTime(..))`.
    pub(crate) fields: Vec<(&'static str, Value)>,
    /// Shown when no field has a value ("Opened at unknown time").
    pub(crate) unknown: Option<&'static str>,
    /// When it happened, for ordering.
    pub(crate) at: Option<i64>,
    pub(crate) artifact: i64,
}

/// Everything shown for a file.
#[derive(Debug, Clone, Default)]
pub(crate) struct Related {
    pub(crate) file_id: i64,
    pub(crate) results: Vec<ResultOnFile>,
    pub(crate) tags: Vec<pm_casedb::FileTag>,
    pub(crate) score: Option<pm_casedb::Score>,
    pub(crate) notes: Vec<pm_casedb::notes::Note>,
    /// Sources first-to-last by time (unknown times first, as Autopsy),
    /// then usage the same way.
    pub(crate) context: Vec<ContextEntry>,
}

/// The Context line for an artifact that points at the file, per Autopsy's
/// `ContextViewer`: messages and e-mails the file was attached to, web
/// downloads and cache entries it came from, recent-document and
/// program-run entries that used it. Other types give no line.
fn context_entry(type_name: &str, artifact: i64, attrs: &[AttributeRecord]) -> Option<ContextEntry> {
    let get = |name: &str| {
        attrs
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.clone())
            .filter(|v| match v {
                Value::Text(t) => !t.is_empty(),
                Value::DateTime(t) => *t > 0,
                _ => true,
            })
    };
    let time = |name: &str| match get(name) {
        Some(Value::DateTime(t)) => Some(t),
        _ => None,
    };
    let fields = |pairs: &[(&'static str, &str)]| -> Vec<(&'static str, Value)> {
        pairs
            .iter()
            .filter_map(|(label, name)| get(name).map(|v| (*label, v)))
            .collect()
    };
    let (usage, heading, prefix, fields, unknown, at) = match type_name {
        "KAAMOS_EMAIL_MSG" => (
            false,
            "Attached to",
            Some("Email"),
            fields(&[
                ("From", "KAAMOS_EMAIL_FROM"),
                ("To", "KAAMOS_EMAIL_TO"),
                ("On", "KAAMOS_DATETIME_SENT"),
            ]),
            None,
            time("KAAMOS_DATETIME_SENT"),
        ),
        "KAAMOS_MESSAGE" => (
            false,
            "Attached to",
            Some("Message"),
            fields(&[
                ("From", "KAAMOS_PHONE_NUMBER_FROM"),
                ("To", "KAAMOS_PHONE_NUMBER_TO"),
                ("On", "KAAMOS_DATETIME"),
            ]),
            None,
            time("KAAMOS_DATETIME"),
        ),
        "KAAMOS_WEB_DOWNLOAD" | "KAAMOS_WEB_CACHE" => (
            false,
            "Downloaded from",
            None,
            fields(&[("URL", "KAAMOS_URL"), ("On", "KAAMOS_DATETIME_CREATED")]),
            None,
            time("KAAMOS_DATETIME_CREATED"),
        ),
        "KAAMOS_RECENT_OBJECT" => (
            true,
            "Recent Documents",
            None,
            fields(&[("Opened at", "KAAMOS_DATETIME")]),
            Some("Opened at unknown time"),
            time("KAAMOS_DATETIME"),
        ),
        "KAAMOS_PROG_RUN" => (
            true,
            "Program Execution",
            None,
            fields(&[("Program Run On", "KAAMOS_DATETIME")]),
            Some("Program Run at unknown time"),
            time("KAAMOS_DATETIME"),
        ),
        _ => return None,
    };
    Some(ContextEntry {
        usage,
        heading,
        prefix,
        fields,
        unknown,
        at,
        artifact,
    })
}

/// The URL the file was downloaded from (Autopsy's File Metadata
/// "Downloaded From" row): the first web download or cache entry that
/// points at it.
pub(crate) fn download_source(case: &Case, file_id: i64) -> Option<String> {
    let db = case.db();
    let names = db.artifact_type_names().ok()?;
    let artifacts = db.artifacts_for_file(file_id).ok()?;
    load_context(case, &names, &artifacts)
        .ok()?
        .into_iter()
        .filter(|e| e.heading == "Downloaded from")
        .find_map(|e| {
            e.fields.into_iter().find_map(|(label, v)| match (label, v) {
                ("URL", Value::Text(url)) => Some(url),
                _ => None,
            })
        })
}

/// The context lines for a file, from its associated-object results.
fn load_context(
    case: &Case,
    names: &[(i64, String, String, i64)],
    artifacts: &[ArtifactRecord],
) -> Result<Vec<ContextEntry>, String> {
    let err = |e: pm_casedb::DbError| e.to_string();
    let db = case.db();
    let type_name = |id: i64| names.iter().find(|n| n.0 == id).map(|n| n.1.as_str());
    let mut out = Vec::new();
    for a in artifacts {
        if type_name(a.type_id) != Some("KAAMOS_ASSOCIATED_OBJECT") {
            continue;
        }
        let target = db.attributes(a.id).map_err(err)?.into_iter().find_map(|at| {
            match (at.name.as_str(), at.value) {
                ("KAAMOS_ASSOCIATED_ARTIFACT", Value::Int(id)) => Some(id),
                _ => None,
            }
        });
        let Some(target) = target else { continue };
        let Some(t) = db.artifact(target).map_err(err)? else {
            continue;
        };
        let Some(name) = type_name(t.type_id) else {
            continue;
        };
        let attrs = db.attributes(t.id).map_err(err)?;
        out.extend(context_entry(name, t.id, &attrs));
    }
    // Unknown times sort first, as in Autopsy (`None < Some`).
    out.sort_by_key(|e| (e.usage, e.at, e.artifact));
    Ok(out)
}

pub(crate) fn load(case: &Case, file_id: i64) -> Result<Related, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let names = db.artifact_type_names().map_err(err)?;
    let artifacts = db.artifacts_for_file(file_id).map_err(err)?;
    let context = load_context(case, &names, &artifacts)?;
    let mut results = Vec::new();
    for a in artifacts {
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
        file_id,
        results,
        tags: db.file_tags(file_id).map_err(err)?,
        score: Some(db.score(ObjKind::File, file_id).map_err(err)?),
        notes: db.notes_about(ObjKind::File, file_id).map_err(err)?,
        context,
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
    /// The file's data once loaded; never another file's (the previous
    /// value is kept while reloading only when it is the same file).
    fn get(&mut self, ctx: &egui::Context, case: &Arc<Case>, file_id: i64) -> Option<Arc<Related>> {
        let case = Arc::clone(case);
        self.data
            .get(ctx, &file_id, move || load(&case, file_id))
            .filter(|d| d.file_id == file_id)
    }

    /// Starts loading `file_id` (the Context tab is enabled only when it
    /// has something to show); whether it has context lines, once known.
    pub(crate) fn has_context(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        file_id: i64,
    ) -> Option<bool> {
        self.get(ctx, case, file_id).map(|d| !d.context.is_empty())
    }

    /// Reloads (results, tags or notes changed).
    pub(crate) fn invalidate(&mut self) {
        self.data.invalidate();
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

    /// Where the file came from and how it was used.
    pub(crate) fn context_ui(
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
        let mut open = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for (usage, title) in [(false, "Source"), (true, "Usage")] {
                    ui.strong(title);
                    let mut any = false;
                    for e in d.context.iter().filter(|e| e.usage == usage) {
                        any = true;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(format!("{}:", e.heading)).strong());
                            ui.label(context_text(e, prefs));
                            if ui.small_button("Go to Result").clicked() {
                                open = Some(Open::Artifact(e.artifact));
                            }
                        });
                    }
                    if !any {
                        ui.weak("Unknown");
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

/// A context line's text, e.g. "URL https://… On 2021-05-31 23:58:00 UTC".
fn context_text(e: &ContextEntry, prefs: &DisplayPrefs) -> String {
    let mut parts: Vec<String> = e.prefix.iter().map(|p| (*p).to_owned()).collect();
    for (label, v) in &e.fields {
        let mut text = match v {
            Value::DateTime(t) => prefs.time(jiff::Timestamp::from_second(*t).ok()),
            other => other.display(),
        };
        if text.chars().count() > CONTEXT_VALUE_MAX {
            text = text.chars().take(CONTEXT_VALUE_MAX - 1).collect::<String>() + "…";
        }
        parts.push(format!("{label} {text}"));
    }
    if e.fields.is_empty()
        && let Some(u) = e.unknown
    {
        parts.push(u.to_owned());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr(name: &str, value: Value) -> AttributeRecord {
        AttributeRecord {
            type_id: 0,
            name: name.to_owned(),
            display_name: name.to_owned(),
            value,
            source: String::new(),
        }
    }

    #[test]
    fn context_lines_follow_autopsy() {
        let download = context_entry(
            "KAAMOS_WEB_DOWNLOAD",
            7,
            &[
                attr("KAAMOS_URL", Value::Text("https://example.com/a.exe".into())),
                attr("KAAMOS_DATETIME_CREATED", Value::DateTime(0)),
            ],
        )
        .unwrap();
        assert!(!download.usage);
        assert_eq!(download.heading, "Downloaded from");
        assert_eq!(download.at, None, "a zero time is unknown");
        let prefs = DisplayPrefs::default();
        assert_eq!(context_text(&download, &prefs), "URL https://example.com/a.exe");

        let run = context_entry("KAAMOS_PROG_RUN", 8, &[]).unwrap();
        assert!(run.usage);
        assert_eq!(context_text(&run, &prefs), "Program Run at unknown time");

        let mail = context_entry(
            "KAAMOS_EMAIL_MSG",
            9,
            &[attr("KAAMOS_EMAIL_FROM", Value::Text("boss@example.com".into()))],
        )
        .unwrap();
        assert_eq!(context_text(&mail, &prefs), "Email From boss@example.com");
        assert!(context_entry("KAAMOS_WEB_HISTORY", 10, &[]).is_none());
    }
}
