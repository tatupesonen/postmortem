//! The examiner's notebook (Tools › Notes): notes, optionally about a file
//! or result ("Add Note…" in the row menus), marked as findings for the
//! report.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::ObjKind;
use pm_casedb::notes::Note;

use crate::pivot::Open;
use crate::task::{Cached, Task};

/// A note with a label for what it is about.
#[derive(Debug, Clone)]
pub(crate) struct Listed {
    pub(crate) note: Note,
    pub(crate) about: Option<String>,
}

/// `file:N /path/name` or `artifact:N Type`.
pub(crate) fn about_label(case: &Case, kind: ObjKind, id: i64) -> String {
    let db = case.db();
    match kind {
        ObjKind::File => db.file(id).ok().flatten().map_or_else(
            || format!("file:{id}"),
            |f| format!("file:{id}  {}", f.path()),
        ),
        ObjKind::Artifact => db
            .artifact(id)
            .ok()
            .flatten()
            .and_then(|a| {
                pm_casedb::catalog::ARTIFACT_TYPES
                    .iter()
                    .find(|t| t.0 == a.type_id)
                    .map(|t| t.2.to_owned())
            })
            .map_or_else(
                || format!("artifact:{id}"),
                |t| format!("artifact:{id}  {t}"),
            ),
        ObjKind::OsAccount => format!("account:{id}"),
    }
}

fn load(case: &Case) -> Result<Vec<Listed>, String> {
    let notes = case.db().notes(false).map_err(|e| e.to_string())?;
    Ok(notes
        .into_iter()
        .map(|n| Listed {
            about: n.about.map(|(k, id)| about_label(case, k, id)),
            note: n,
        })
        .collect())
}

/// The Notes window.
#[derive(Debug, Default)]
pub(crate) struct NotesWindow {
    notes: Cached<Vec<Listed>>,
    draft: String,
    /// What the next note is about, with its label.
    draft_about: Option<(ObjKind, i64, String)>,
    focus_draft: bool,
    /// The note being edited: id, text.
    editing: Option<(i64, String)>,
    writes: Vec<Task<Result<(), String>>>,
    error: Option<String>,
}

impl NotesWindow {
    /// Starts a note about a file or result.
    pub(crate) fn note_about(&mut self, kind: ObjKind, id: i64, label: String) {
        self.draft_about = Some((kind, id, label));
        self.focus_draft = true;
    }

    /// Reloads (the case's notes changed).
    pub(crate) fn refresh(&mut self) {
        self.notes.refresh();
    }

    fn write(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        f: impl FnOnce(&Case) -> Result<(), String> + Send + 'static,
    ) {
        let case = Arc::clone(case);
        self.writes.push(Task::spawn(ctx, move || f(&case)));
    }

    /// Draws the window; returns what to open and `false` once closed.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: &Arc<Case>) -> (Option<Open>, bool) {
        self.writes.retain_mut(|t| match t.poll() {
            Some(Err(e)) => {
                self.error = Some(e.clone());
                false
            }
            Some(Ok(())) => false,
            None => true,
        });
        let mut open = true;
        let mut chosen = None;
        let loader = Arc::clone(case);
        let notes = self.notes.get(ctx, move || load(&loader));
        egui::Window::new("Notes")
            .id(egui::Id::new("notes-window"))
            .open(&mut open)
            .default_size([560.0, 600.0])
            .show(ctx, |ui| {
                if let Some(e) = self.error.clone().or_else(|| self.notes.error().map(str::to_owned)) {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                // New note.
                if let Some(label) = self.draft_about.as_ref().map(|d| d.2.clone()) {
                    ui.horizontal(|ui| {
                        ui.label("About:");
                        ui.monospace(&label);
                        if ui.small_button("×").on_hover_text("A general note").clicked() {
                            self.draft_about = None;
                        }
                    });
                }
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut self.draft)
                        .hint_text("Write a note… (Ctrl+Enter adds it)")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.focus_draft) {
                    edit.request_focus();
                }
                let submit = edit.has_focus()
                    && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if (ui
                        .add_enabled(!self.draft.trim().is_empty(), egui::Button::new("Add Note"))
                        .clicked()
                        || submit)
                        && !self.draft.trim().is_empty()
                    {
                        let text = std::mem::take(&mut self.draft).trim().to_owned();
                        let about = self.draft_about.take().map(|(k, id, _)| (k, id));
                        self.write(ctx, case, move |c| {
                            c.db()
                                .add_note(&text, about, &crate::tags_ui::examiner(), jiff::Timestamp::now().as_second())
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        });
                    }
                });
                ui.separator();
                let Some(notes) = notes else {
                    ui.spinner();
                    return;
                };
                if notes.is_empty() {
                    ui.weak("No notes yet. “Add Note…” in a row's menu writes one about that file or result.");
                }
                let mut actions: Vec<NoteAction> = Vec::new();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for l in notes.iter() {
                        let n = &l.note;
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let mut finding = n.finding;
                                if ui
                                    .checkbox(&mut finding, "Finding")
                                    .on_hover_text("Findings are listed in the HTML report")
                                    .changed()
                                {
                                    actions.push(NoteAction::Save(n.id, n.text.clone(), finding));
                                }
                                ui.weak(format!(
                                    "{} · {}",
                                    n.examiner,
                                    jiff::Timestamp::from_second(n.updated_at)
                                        .map(|t| t.strftime("%Y-%m-%d %H:%M UTC").to_string())
                                        .unwrap_or_default()
                                ));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("🗑").on_hover_text("Delete note").clicked() {
                                        actions.push(NoteAction::Delete(n.id));
                                    }
                                    if ui.small_button("✏").on_hover_text("Edit note").clicked() {
                                        self.editing = Some((n.id, n.text.clone()));
                                    }
                                });
                            });
                            if let (Some(label), Some((kind, id))) = (&l.about, n.about)
                                && ui.link(label).clicked()
                            {
                                chosen = match kind {
                                    ObjKind::File => Some(Open::File(id)),
                                    ObjKind::Artifact => Some(Open::Artifact(id)),
                                    ObjKind::OsAccount => None,
                                };
                            }
                            match &mut self.editing {
                                Some((eid, text)) if *eid == n.id => {
                                    ui.add(
                                        egui::TextEdit::multiline(text)
                                            .desired_rows(3)
                                            .desired_width(f32::INFINITY),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("Save").clicked() {
                                            actions.push(NoteAction::Save(n.id, text.clone(), n.finding));
                                            actions.push(NoteAction::StopEditing);
                                        }
                                        if ui.button("Cancel").clicked() {
                                            actions.push(NoteAction::StopEditing);
                                        }
                                    });
                                }
                                _ => {
                                    ui.label(&n.text);
                                }
                            }
                        });
                    }
                });
                for a in actions {
                    match a {
                        NoteAction::Save(id, text, finding) => self.write(ctx, case, move |c| {
                            c.db()
                                .update_note(id, &text, finding, jiff::Timestamp::now().as_second())
                                .map_err(|e| e.to_string())
                        }),
                        NoteAction::Delete(id) => self.write(ctx, case, move |c| {
                            c.db().delete_note(id).map_err(|e| e.to_string())
                        }),
                        NoteAction::StopEditing => self.editing = None,
                    }
                }
            });
        (chosen, open)
    }
}

enum NoteAction {
    Save(i64, String, bool),
    Delete(i64),
    StopEditing,
}
