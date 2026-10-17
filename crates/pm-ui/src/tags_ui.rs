//! Tagging (Autopsy's Add/Remove File and Result Tag menus, the "Tag and
//! Comment" and "New Tag" dialogs, Ctrl+B Bookmark).
//!
//! Menus are built from a [`TagCatalog`] loaded in the background, and the
//! tags already on a row come with the row, so opening a menu never touches
//! the database. Applying a tag runs as a background task.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::{TagName, TagSet};

use crate::model::{Annotations, FileRow, RowTag};
use crate::task::Task;

/// Autopsy's standard tag names (`TagsManager.getStandardTagNames`).
pub(crate) const STANDARD_TAGS: [&str; 3] = ["Bookmark", "Follow Up", "Notable Item"];
pub(crate) const BOOKMARK: &str = "Bookmark";
/// Suffix of notable tag names in menus and the tree.
pub(crate) const NOTABLE_LABEL: &str = " (Notable)";

/// The case's tag names and tag sets.
#[derive(Debug, Clone, Default)]
pub(crate) struct TagCatalog {
    pub(crate) names: Vec<TagName>,
    pub(crate) sets: Vec<TagSet>,
}

impl TagCatalog {
    pub(crate) fn load(case: &Case) -> Result<Self, String> {
        let db = case.db();
        Ok(Self {
            names: db.tag_names().map_err(|e| e.to_string())?,
            sets: db.tag_sets().map_err(|e| e.to_string())?,
        })
    }

    pub(crate) fn by_name(&self, name: &str) -> Option<&TagName> {
        self.names.iter().find(|t| t.name == name)
    }

    pub(crate) fn by_id(&self, id: i64) -> Option<&TagName> {
        self.names.iter().find(|t| t.id == id)
    }

    /// Tree/menu label: `<set>: <name>` for set members, plus
    /// `" (Notable)"` for notable names (Autopsy `TagUtils`).
    pub(crate) fn decorated(&self, t: &TagName) -> String {
        let mut s = match t
            .tag_set_id
            .and_then(|id| self.sets.iter().find(|x| x.id == id))
        {
            Some(set) => format!("{}: {}", set.name, t.name),
            None => t.name.clone(),
        };
        if t.notable {
            s.push_str(NOTABLE_LABEL);
        }
        s
    }
}

/// The catalogue, reloaded in the background after changes.
#[derive(Debug, Default)]
pub(crate) struct Catalog {
    task: Option<Task<Result<TagCatalog, String>>>,
    current: Arc<TagCatalog>,
    loaded: bool,
}

impl Catalog {
    pub(crate) fn get(&mut self, ctx: &egui::Context, case: Option<&Arc<Case>>) -> Arc<TagCatalog> {
        if let Some(t) = &mut self.task
            && let Some(r) = t.poll()
        {
            if let Ok(c) = r {
                self.current = Arc::new(c.clone());
            }
            self.task = None;
            self.loaded = true;
        }
        if !self.loaded
            && self.task.is_none()
            && let Some(case) = case
        {
            let case = Arc::clone(case);
            self.task = Some(Task::spawn(ctx, move || TagCatalog::load(&case)));
        }
        Arc::clone(&self.current)
    }

    pub(crate) fn invalidate(&mut self) {
        self.loaded = false;
        self.task = None;
    }
}

/// What the user picked in a tag menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagRequest {
    /// Apply this tag name without a comment.
    Apply(i64),
    /// Open the "Tag and Comment" dialog.
    TagAndComment,
    /// Open the "New Tag" dialog.
    NewTag,
    /// Remove this tag (by tag id).
    Remove(i64),
}

/// Autopsy's Add Tag submenu, in its order: custom names (sorted), then
/// the standard names, then one submenu per tag set, then "Tag and
/// Comment..." and "New Tag...".
pub(crate) fn add_menu(ui: &mut egui::Ui, label: &str, cat: &TagCatalog) -> Option<TagRequest> {
    let mut out = None;
    ui.menu_button(label, |ui| {
        let item = |ui: &mut egui::Ui, t: &TagName, out: &mut Option<TagRequest>| {
            let text = if t.notable {
                format!("{}{NOTABLE_LABEL}", t.name)
            } else {
                t.name.clone()
            };
            let mut b = egui::Button::new(text);
            if t.name == BOOKMARK {
                b = b.shortcut_text(ui.ctx().format_shortcut(&bookmark_shortcut()));
            }
            if ui.add(b).clicked() {
                *out = Some(TagRequest::Apply(t.id));
                ui.close();
            }
        };
        let mut custom: Vec<&TagName> = cat
            .names
            .iter()
            .filter(|t| t.tag_set_id.is_none() && !STANDARD_TAGS.contains(&t.name.as_str()))
            .collect();
        custom.sort_by(|a, b| a.name.cmp(&b.name));
        for t in &custom {
            item(ui, t, &mut out);
        }
        if !custom.is_empty() {
            ui.separator();
        }
        for name in STANDARD_TAGS {
            if let Some(t) = cat.by_name(name).filter(|t| t.tag_set_id.is_none()) {
                item(ui, t, &mut out);
            }
        }
        for set in &cat.sets {
            ui.menu_button(&set.name, |ui| {
                for t in &set.names {
                    item(ui, t, &mut out);
                }
            });
        }
        ui.separator();
        if ui.button("Tag and Comment...").clicked() {
            out = Some(TagRequest::TagAndComment);
            ui.close();
        }
        if ui.button("New Tag...").clicked() {
            out = Some(TagRequest::NewTag);
            ui.close();
        }
    });
    out
}

/// Autopsy's Remove Tag submenu (disabled when the item has no tags).
pub(crate) fn remove_menu(ui: &mut egui::Ui, label: &str, tags: &[RowTag]) -> Option<TagRequest> {
    let mut out = None;
    ui.add_enabled_ui(!tags.is_empty(), |ui| {
        ui.menu_button(label, |ui| {
            for t in tags {
                if ui.button(&t.name).clicked() {
                    out = Some(TagRequest::Remove(t.id));
                    ui.close();
                }
            }
        });
    });
    out
}

/// Fills the S and C columns of case rows (call in the listing task).
pub(crate) fn annotate(case: &Case, rows: &mut [FileRow]) -> Result<(), String> {
    let db = case.db();
    let ids: Vec<i64> = rows.iter().filter_map(FileRow::file_id).collect();
    let scores = db
        .scores(pm_casedb::ObjKind::File, &ids)
        .map_err(|e| e.to_string())?;
    let tags = db.file_tags_of(&ids).map_err(|e| e.to_string())?;
    for r in rows.iter_mut() {
        let Some(id) = r.file_id() else { continue };
        let score = scores
            .iter()
            .find(|(f, _)| *f == id)
            .map_or(pm_casedb::Significance::Unknown, |(_, s)| s.significance);
        let tags = tags
            .iter()
            .filter(|t| t.file_id == id)
            .map(|t| RowTag {
                id: t.id,
                tag_name_id: t.tag_name_id,
                name: t.tag_name.clone(),
                comment: t.comment.clone(),
            })
            .collect();
        r.annotations = Some(Arc::new(Annotations { score, tags }));
    }
    Ok(())
}

/// Ctrl+B (⌘B on macOS): bookmark the selection.
pub(crate) fn bookmark_shortcut() -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::B)
}

/// Autopsy `TagsManager.containsIllegalCharacters` + the dialog's checks.
pub(crate) fn validate_tag_name(name: &str, cat: &TagCatalog) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Must supply a tag name to continue.".into());
    }
    if name.contains(['\\', ':', '*', '?', '"', '<', '>', '|', ',', ';']) {
        return Err(
            "The tag name contains illegal characters.\nCannot contain any of the following symbols: \\ : * ? \" < > | , ;"
                .into(),
        );
    }
    if cat.names.iter().any(|t| t.name.eq_ignore_ascii_case(name)) {
        return Err(format!("A {name} tag name has already been defined."));
    }
    Ok(())
}

/// What a tag dialog asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialogResult {
    /// Apply an existing tag with a comment.
    Apply { tag_name_id: i64, comment: String },
    /// Create a tag name, then apply it.
    Create {
        name: String,
        description: String,
        notable: bool,
        comment: String,
    },
}

/// The state of a dialog after a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Open,
    Ok(DialogResult),
    Cancel,
}

/// The "Tag and Comment" dialog (`GetTagNameAndCommentDialog`).
#[derive(Debug, Default)]
pub(crate) struct TagCommentDialog {
    selected: Option<i64>,
    comment: String,
    /// "New Tag" pressed inside the dialog.
    new_tag: Option<NewTagDialog>,
}

/// The "New Tag" dialog (`GetTagNameDialog`).
#[derive(Debug, Default)]
pub(crate) struct NewTagDialog {
    name: String,
    description: String,
    notable: bool,
    error: Option<String>,
}

impl NewTagDialog {
    /// Draws the dialog for one frame.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, cat: &TagCatalog) -> Outcome {
        let mut out = Outcome::Open;
        egui::Window::new("Create Tag")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Pre-existing Tag Names:");
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for t in &cat.names {
                            ui.weak(cat.decorated(t));
                        }
                    });
                ui.separator();
                ui.strong("New Tag");
                egui::Grid::new("new-tag").num_columns(2).show(ui, |ui| {
                    ui.label("Tag Name:");
                    ui.text_edit_singleline(&mut self.name);
                    ui.end_row();
                    ui.label("Description:");
                    ui.text_edit_singleline(&mut self.description);
                    ui.end_row();
                });
                ui.checkbox(&mut self.notable, "Tag indicates item is notable.");
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        match validate_tag_name(&self.name, cat) {
                            Ok(()) => {
                                out = Outcome::Ok(DialogResult::Create {
                                    name: self.name.trim().to_owned(),
                                    description: self.description.clone(),
                                    notable: self.notable,
                                    comment: String::new(),
                                });
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        out = Outcome::Cancel;
                    }
                });
            });
        out
    }
}

impl TagCommentDialog {
    /// Draws the dialog for one frame.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, cat: &TagCatalog) -> Outcome {
        if let Some(n) = &mut self.new_tag {
            match n.ui(ctx, cat) {
                Outcome::Ok(DialogResult::Create {
                    name,
                    description,
                    notable,
                    ..
                }) => {
                    // Created from inside this dialog: apply with the comment.
                    self.new_tag = None;
                    return Outcome::Ok(DialogResult::Create {
                        name,
                        description,
                        notable,
                        comment: self.comment.clone(),
                    });
                }
                Outcome::Ok(_) | Outcome::Cancel => self.new_tag = None,
                Outcome::Open => {}
            }
            return Outcome::Open;
        }
        let mut out = Outcome::Open;
        egui::Window::new("Tag and Comment")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("tag-comment")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Tag:");
                        let current = self
                            .selected
                            .and_then(|id| cat.by_id(id))
                            .map_or_else(|| "Select Tag".to_owned(), |t| cat.decorated(t));
                        egui::ComboBox::from_id_salt("tag-combo")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                if cat.names.is_empty() {
                                    ui.weak("No Tags");
                                }
                                for t in &cat.names {
                                    ui.selectable_value(
                                        &mut self.selected,
                                        Some(t.id),
                                        cat.decorated(t),
                                    );
                                }
                            })
                            .response
                            .on_hover_text("Select tag to use");
                        ui.end_row();
                        ui.label("Comment: ");
                        ui.text_edit_singleline(&mut self.comment)
                            .on_hover_text("Enter an optional tag comment or leave blank");
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button("New Tag").clicked() {
                        self.new_tag = Some(NewTagDialog::default());
                    }
                    if ui
                        .add_enabled(self.selected.is_some(), egui::Button::new("OK"))
                        .clicked()
                        && let Some(id) = self.selected
                    {
                        out = Outcome::Ok(DialogResult::Apply {
                            tag_name_id: id,
                            comment: self.comment.clone(),
                        });
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        out = Outcome::Cancel;
                    }
                });
            });
        out
    }
}

/// What a tag request applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TagTarget {
    Files(Vec<i64>),
    Artifacts(Vec<i64>),
}

#[derive(Debug)]
enum Dialog {
    Comment(TagCommentDialog),
    New(NewTagDialog),
}

/// Tag requests, dialogs and the background writes behind them.
#[derive(Debug, Default)]
pub(crate) struct Tagging {
    pub(crate) catalog: Catalog,
    dialog: Option<(TagTarget, Dialog)>,
    tasks: Vec<Task<Result<(), String>>>,
    changed: bool,
}

impl Tagging {
    /// Handles a menu choice (or Ctrl+B) for `target`.
    pub(crate) fn request(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        target: TagTarget,
        req: TagRequest,
    ) {
        match req {
            TagRequest::TagAndComment => {
                self.dialog = Some((target, Dialog::Comment(TagCommentDialog::default())));
            }
            TagRequest::NewTag => {
                self.dialog = Some((target, Dialog::New(NewTagDialog::default())));
            }
            TagRequest::Apply(tag_name_id) => {
                self.spawn(
                    ctx,
                    case,
                    target,
                    Write::Apply {
                        tag_name_id,
                        comment: String::new(),
                    },
                );
            }
            TagRequest::Remove(tag_id) => self.spawn(ctx, case, target, Write::Remove(tag_id)),
        }
    }

    /// Bookmarks `target` (Ctrl+B).
    pub(crate) fn bookmark(&mut self, ctx: &egui::Context, case: &Arc<Case>, target: TagTarget) {
        let cat = self.catalog.get(ctx, Some(case));
        if let Some(t) = cat.by_name(BOOKMARK) {
            self.request(ctx, case, target, TagRequest::Apply(t.id));
        }
    }

    fn spawn(&mut self, ctx: &egui::Context, case: &Arc<Case>, target: TagTarget, w: Write) {
        let case = Arc::clone(case);
        self.tasks
            .push(Task::spawn(ctx, move || w.run(&case, &target)));
    }

    /// Draws open dialogs and collects finished writes; returns errors to
    /// show.
    pub(crate) fn ui(&mut self, ctx: &egui::Context, case: Option<&Arc<Case>>) -> Vec<String> {
        let mut errors = Vec::new();
        self.tasks.retain_mut(|t| match t.poll() {
            Some(r) => {
                if let Err(e) = r {
                    errors.push(e.clone());
                }
                self.changed = true;
                false
            }
            None => true,
        });
        if self.changed {
            self.catalog.invalidate();
        }
        let Some(case) = case else {
            self.dialog = None;
            return errors;
        };
        let cat = self.catalog.get(ctx, Some(case));
        let result = match &mut self.dialog {
            Some((_, Dialog::Comment(d))) => d.ui(ctx, &cat),
            Some((_, Dialog::New(d))) => d.ui(ctx, &cat),
            None => Outcome::Open,
        };
        if result == Outcome::Cancel {
            self.dialog = None;
        }
        if let Outcome::Ok(r) = result
            && let Some((target, _)) = self.dialog.take()
        {
            let w = match r {
                DialogResult::Apply {
                    tag_name_id,
                    comment,
                } => Write::Apply {
                    tag_name_id,
                    comment,
                },
                DialogResult::Create {
                    name,
                    description,
                    notable,
                    comment,
                } => Write::Create {
                    name,
                    description,
                    notable,
                    comment,
                },
            };
            self.spawn(ctx, case, target, w);
        }
        errors
    }

    /// Whether a tag change finished since the last call (listings, the
    /// Tags tree and scores need reloading).
    pub(crate) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

/// A tag write, run off the UI thread.
#[derive(Debug)]
enum Write {
    Apply {
        tag_name_id: i64,
        comment: String,
    },
    Create {
        name: String,
        description: String,
        notable: bool,
        comment: String,
    },
    Remove(i64),
}

impl Write {
    fn run(self, case: &Case, target: &TagTarget) -> Result<(), String> {
        let db = case.db();
        let err = |e: pm_casedb::DbError| e.to_string();
        let now = jiff::Timestamp::now().as_second();
        let who = examiner();
        let (tag_name_id, comment) = match self {
            Self::Remove(tag_id) => {
                return match target {
                    TagTarget::Files(_) => db.untag_file(tag_id).map_err(err),
                    TagTarget::Artifacts(_) => db.untag_artifact(tag_id).map_err(err),
                };
            }
            Self::Apply {
                tag_name_id,
                comment,
            } => (tag_name_id, comment),
            Self::Create {
                name,
                description,
                notable,
                comment,
            } => (
                db.add_tag_name(&name, &description, notable)
                    .map_err(|e| format!("Unable to add the {name} tag name to the case. ({e})"))?,
                comment,
            ),
        };
        match target {
            TagTarget::Files(ids) => {
                for id in ids {
                    db.tag_file(*id, tag_name_id, &comment, &who, now)
                        .map_err(|e| format!("Unable to tag file {id}. ({e})"))?;
                }
            }
            TagTarget::Artifacts(ids) => {
                for id in ids {
                    db.tag_artifact(*id, tag_name_id, &comment, &who, now)
                        .map_err(|e| format!("Unable to tag result {id}. ({e})"))?;
                }
            }
        }
        Ok(())
    }
}

/// The examiner name recorded on tags (Autopsy uses the OS login).
pub(crate) fn examiner() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "examiner".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> TagCatalog {
        TagCatalog {
            names: vec![TagName {
                id: 1,
                name: "Bookmark".into(),
                description: String::new(),
                color: "NONE".into(),
                notable: false,
                tag_set_id: None,
                rank: None,
            }],
            sets: Vec::new(),
        }
    }

    #[test]
    fn tag_names_are_validated_like_autopsy() {
        assert!(validate_tag_name("Evidence", &cat()).is_ok());
        assert_eq!(
            validate_tag_name("  ", &cat()).unwrap_err(),
            "Must supply a tag name to continue."
        );
        assert!(
            validate_tag_name("a;b", &cat())
                .unwrap_err()
                .contains("illegal characters")
        );
        assert!(
            validate_tag_name("bookmark", &cat())
                .unwrap_err()
                .contains("already been defined")
        );
    }

    #[test]
    fn decorated_names() {
        let mut c = cat();
        c.names[0].notable = true;
        assert_eq!(c.decorated(&c.names[0]), "Bookmark (Notable)");
    }
}
