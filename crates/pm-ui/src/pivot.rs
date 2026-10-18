//! Pivot: everything about one value (a user name, domain, e-mail or IP
//! address, USB serial, hash, file name). Lists the results with an
//! attribute equal to it, the files named like it, the communication
//! accounts with it as identifier, and the files and results whose text
//! contains it (keyword index). Every row opens its item.

use std::sync::Arc;

use pm_case::Case;

use crate::task::Keyed;

/// Rows per section.
const LIMIT: usize = 200;

/// What a row opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Open {
    File(i64),
    Artifact(i64),
}

/// One listed item.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) open: Option<Open>,
}

/// The sections of a pivot.
#[derive(Debug, Default)]
pub(crate) struct Found {
    pub(crate) results: Vec<Item>,
    pub(crate) files: Vec<Item>,
    pub(crate) accounts: Vec<Item>,
    pub(crate) text: Vec<Item>,
    /// Why a section could not be searched (no index yet).
    pub(crate) notes: Vec<String>,
}

/// Looks `value` up everywhere.
pub(crate) fn find(case: &Case, value: &str) -> Result<Found, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut out = Found::default();
    let file_name = |id: Option<i64>| -> String {
        id.and_then(|id| db.file(id).ok().flatten())
            .map(|f| f.path())
            .unwrap_or_default()
    };
    for m in db.artifacts_with_value(value, LIMIT).map_err(err)? {
        out.results.push(Item {
            title: format!("{} › {}", m.type_display, m.attribute),
            detail: Some(file_name(m.artifact.file_id))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| format!("artifact:{}", m.artifact.id)),
            open: Some(Open::Artifact(m.artifact.id)),
        });
    }
    for a in db.accounts_with_identifier(value).map_err(err)? {
        out.accounts.push(Item {
            title: a.identifier.clone(),
            detail: a.type_name.clone(),
            open: None,
        });
    }
    match case.index() {
        Ok(ix) => {
            let q = pm_search::NameQuery {
                text: value.to_owned(),
                ..pm_search::NameQuery::default()
            };
            let page = ix
                .names()
                .search(&q, None, LIMIT)
                .map_err(|e| e.to_string())?;
            for f in db.files_by_ids(&page.ids).map_err(err)? {
                out.files.push(Item {
                    title: f.name.clone(),
                    detail: f.parent_path.clone(),
                    open: Some(Open::File(f.id)),
                });
            }
            let tq = pm_search::TextQuery {
                term: value.to_owned(),
                search_type: pm_search::SearchType::Substring,
                ds_ids: Vec::new(),
            };
            match ix.content().search(&tq, None, LIMIT, &|| false) {
                Ok(page) => {
                    for h in page.hits {
                        out.text.push(match h.obj {
                            pm_search::ObjRef::File(id) => Item {
                                title: file_name(Some(id)),
                                detail: "File text".into(),
                                open: Some(Open::File(id)),
                            },
                            pm_search::ObjRef::Artifact(id) => Item {
                                title: format!("Result {id}"),
                                detail: "Result text".into(),
                                open: Some(Open::Artifact(id)),
                            },
                        });
                    }
                }
                Err(e) => out.notes.push(format!("Text search: {e}")),
            }
        }
        Err(e) => out
            .notes
            .push(format!("File names and text are not indexed yet ({e}).")),
    }
    Ok(out)
}

/// The Pivot window.
#[derive(Debug, Default)]
pub(crate) struct PivotWindow {
    pub(crate) value: String,
    input: String,
    found: Keyed<String, Found>,
}

impl PivotWindow {
    pub(crate) fn new(value: &str) -> Self {
        Self {
            value: value.trim().to_owned(),
            input: value.trim().to_owned(),
            found: Keyed::default(),
        }
    }

    /// Draws the window; returns what to open, and `false` once closed.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: &Arc<Case>) -> (Option<Open>, bool) {
        let mut open = true;
        let mut chosen = None;
        egui::Window::new(format!("Pivot: {}", self.value))
            .id(egui::Id::new("pivot-window"))
            .open(&mut open)
            .default_size([640.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let label = ui.label("Value:");
                    let edit = ui
                        .add(egui::TextEdit::singleline(&mut self.input).desired_width(360.0))
                        .labelled_by(label.id);
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.button("Pivot").clicked() || enter) && !self.input.trim().is_empty() {
                        self.input.trim().clone_into(&mut self.value);
                    }
                });
                ui.separator();
                let case2 = Arc::clone(case);
                let value = self.value.clone();
                let found = self
                    .found
                    .get(ctx, &self.value.clone(), move || find(&case2, &value));
                if let Some(e) = self.found.error() {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                let Some(found) = found else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Searching…");
                    });
                    return;
                };
                if self.found.loading() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Searching…");
                    });
                }
                for n in &found.notes {
                    ui.weak(n);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (title, items) in [
                        ("Results with this value", &found.results),
                        ("Files named like it", &found.files),
                        ("Communication accounts", &found.accounts),
                        ("Text containing it", &found.text),
                    ] {
                        egui::CollapsingHeader::new(format!("{title} ({})", items.len()))
                            .id_salt(title)
                            .default_open(!items.is_empty())
                            .show(ui, |ui| {
                                for it in items {
                                    ui.horizontal(|ui| {
                                        let r = match it.open {
                                            Some(_) => ui.link(&it.title),
                                            None => ui.label(&it.title),
                                        };
                                        if r.clicked() {
                                            chosen = it.open;
                                        }
                                        ui.weak(&it.detail);
                                    });
                                }
                            });
                    }
                });
            });
        (chosen, open)
    }
}
