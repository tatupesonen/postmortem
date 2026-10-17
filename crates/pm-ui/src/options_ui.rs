//! Tools › Options (Autopsy's options dialog): one page per
//! category, edited on a draft that OK or Apply commits and Cancel drops.
//!
//! Pages so far: View (`ViewPreferencesPanel`) and Tags
//! (`TagOptionsPanel` with its `TagNameDialog`).

use crate::tags_ui::{STANDARD_TAGS, TagDefinition, upsert_definition};

/// A category of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    View,
    Tags,
}

impl Page {
    const ALL: [Self; 2] = [Self::View, Self::Tags];

    fn title(self) -> &'static str {
        match self {
            Self::View => "View",
            Self::Tags => "Tags",
        }
    }
}

/// The settings the dialog edits (a copy of the persisted ones).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) local_time: bool,
    pub(crate) hide_known_in_views: bool,
    /// User tag definitions, sorted by name.
    pub(crate) custom_tags: Vec<TagDefinition>,
}

/// What the dialog asks the app to do after a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OptionsAction {
    None,
    /// Store these settings (Apply).
    Apply(Settings),
    /// Store these settings and close (OK).
    ApplyAndClose(Settings),
    Close,
}

/// The open Options dialog.
#[derive(Debug)]
pub(crate) struct OptionsWindow {
    page: Page,
    saved: Settings,
    draft: Settings,
    tags: TagsPage,
}

impl OptionsWindow {
    pub(crate) fn new(current: Settings) -> Self {
        Self {
            page: Page::View,
            saved: current.clone(),
            draft: current,
            tags: TagsPage::default(),
        }
    }

    /// Draws the dialog; `ingest_running` locks existing tags (Autopsy:
    /// "Cannot make changes to existing tags when ingest is running!").
    pub(crate) fn ui(&mut self, ctx: &egui::Context, ingest_running: bool) -> OptionsAction {
        let mut action = OptionsAction::None;
        let mut open = true;
        egui::Window::new("Options")
            .open(&mut open)
            .collapsible(false)
            .default_size([760.0, 500.0])
            .show(ctx, |ui| {
                egui::Panel::bottom("options-buttons").show(ui, |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let dirty = self.draft != self.saved;
                        if ui.add_enabled(dirty, egui::Button::new("Apply")).clicked() {
                            self.saved = self.draft.clone();
                            action = OptionsAction::Apply(self.draft.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            action = OptionsAction::Close;
                        }
                        if ui.button("OK").clicked() {
                            action = if dirty {
                                OptionsAction::ApplyAndClose(self.draft.clone())
                            } else {
                                OptionsAction::Close
                            };
                        }
                    });
                });
                egui::Panel::left("options-pages")
                    .resizable(false)
                    .default_size(120.0)
                    .show(ui, |ui| {
                        for p in Page::ALL {
                            ui.selectable_value(&mut self.page, p, p.title());
                        }
                    });
                egui::CentralPanel::default().show(ui, |ui| match self.page {
                    Page::View => view_page(ui, &mut self.draft),
                    Page::Tags => self
                        .tags
                        .ui(ui, &mut self.draft.custom_tags, ingest_running),
                });
            });
        self.tags.dialog_ui(ctx, &mut self.draft.custom_tags);
        if !open {
            action = OptionsAction::Close;
        }
        action
    }
}

fn view_page(ui: &mut egui::Ui, s: &mut Settings) {
    ui.strong("Global Settings");
    ui.label("Hide known files (i.e. those in the NIST NSRL) in the:");
    ui.indent("hide-known", |ui| {
        ui.checkbox(&mut s.hide_known_in_views, "Views area");
    });
    ui.add_space(8.0);
    ui.label("When displaying times:");
    ui.indent("times", |ui| {
        ui.radio_value(&mut s.local_time, true, "Use local time zone");
        ui.radio_value(&mut s.local_time, false, "Use UTC");
    });
}

/// Autopsy `TagNameDialog` validation.
pub(crate) fn validate_definition(
    name: &str,
    description: &str,
    existing: &[TagDefinition],
    editing: bool,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("The tag name cannot be empty".into());
    }
    if name.contains(['\\', ':', '*', '?', '"', '<', '>', '|', ',', ';']) {
        return Err(
            "Tag name may not contain any of the following symbols: \\ : * ? \" < > | , ;".into(),
        );
    }
    if description.contains([',', ';']) {
        return Err("Tag descriptions may not contain commas (,) or semicolons (;)".into());
    }
    // Autopsy only checks the user's own tags; a standard name is refused
    // too, since saving it would change the standard tag in every case.
    if !editing
        && (existing.iter().any(|d| d.name.eq_ignore_ascii_case(name))
            || STANDARD_TAGS.iter().any(|s| s.eq_ignore_ascii_case(name)))
    {
        return Err("Tag name must be unique. A tag with this name already exists.".into());
    }
    Ok(())
}

/// Options › Tags.
#[derive(Debug, Default)]
struct TagsPage {
    selected: Option<String>,
    dialog: Option<TagNameDialog>,
}

/// New Tag / Edit Tag.
#[derive(Debug, Default)]
struct TagNameDialog {
    editing: bool,
    name: String,
    description: String,
    notable: bool,
    error: Option<String>,
}

impl TagsPage {
    fn ui(&mut self, ui: &mut egui::Ui, defs: &mut Vec<TagDefinition>, ingest_running: bool) {
        ui.label(
            "Create and manage tags. Tags can be applied to files and results in the case. \
             Notable tags will cause items tagged with them to be flagged as notable when \
             using a central repository. Changing the status of a tag will only effect items \
             in the current case.",
        );
        ui.add_space(6.0);
        let selected = self
            .selected
            .as_ref()
            .and_then(|n| defs.iter().position(|d| &d.name == n));
        ui.columns(2, |cols| {
            let [left, right] = cols else {
                return;
            };
            left.label("Tag Names:");
            egui::Frame::group(left.style()).show(left, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("tag-defs")
                    .min_scrolled_height(260.0)
                    .max_height(260.0)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        for d in defs.iter() {
                            let on = self.selected.as_deref() == Some(d.name.as_str());
                            if ui.selectable_label(on, &d.name).clicked() {
                                self.selected = Some(d.name.clone());
                            }
                        }
                    });
            });
            left.horizontal(|ui| {
                if ui.button("New Tag").clicked() {
                    self.dialog = Some(TagNameDialog::default());
                }
                let can_edit = selected.is_some() && !ingest_running;
                if ui
                    .add_enabled(can_edit, egui::Button::new("Edit Tag"))
                    .clicked()
                    && let Some(d) = selected.and_then(|i| defs.get(i))
                {
                    self.dialog = Some(TagNameDialog {
                        editing: true,
                        name: d.name.clone(),
                        description: d.description.clone(),
                        notable: d.notable,
                        error: None,
                    });
                }
                if ui
                    .add_enabled(can_edit, egui::Button::new("Delete Tag"))
                    .clicked()
                    && let Some(i) = selected
                {
                    // Autopsy removes the definition only; the case keeps
                    // the tag name and its tags.
                    defs.remove(i);
                    self.selected = None;
                }
            });
            right.label("Tag Description:");
            let d = selected.and_then(|i| defs.get(i));
            egui::Frame::group(right.style()).show(right, |ui| {
                ui.set_min_size(egui::vec2(ui.available_width(), 80.0));
                ui.label(d.map_or("", |d| d.description.as_str()));
            });
            right.horizontal(|ui| {
                ui.label("Tag indicates item is notable: ");
                ui.label(d.map_or("", |d| if d.notable { "Yes" } else { "No" }));
            });
            if ingest_running {
                right.add_space(12.0);
                right.colored_label(
                    right.visuals().warn_fg_color,
                    "⚠ Cannot make changes to existing tags when ingest is running!",
                );
            }
        });
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, defs: &mut Vec<TagDefinition>) {
        let Some(d) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        egui::Window::new(if d.editing { "Edit Tag" } else { "New Tag" })
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Grid::new("tag-def").num_columns(2).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add_enabled(!d.editing, egui::TextEdit::singleline(&mut d.name));
                    ui.end_row();
                    ui.label("Description:");
                    ui.add(egui::TextEdit::multiline(&mut d.description).desired_rows(3));
                    ui.end_row();
                });
                ui.checkbox(&mut d.notable, "Tag indicates item is notable.");
                if let Some(e) = &d.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                ui.horizontal(|ui| {
                    let ok = ui
                        .add_enabled(!d.name.trim().is_empty(), egui::Button::new("OK"))
                        .clicked();
                    if ok {
                        match validate_definition(&d.name, &d.description, defs, d.editing) {
                            Ok(()) => {
                                let def = TagDefinition {
                                    name: d.name.trim().to_owned(),
                                    description: d.description.clone(),
                                    notable: d.notable,
                                };
                                self.selected = Some(def.name.clone());
                                upsert_definition(defs, def);
                                close = true;
                            }
                            Err(e) => d.error = Some(e),
                        }
                    }
                    if ui.button("Cancel").clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        close = true;
                    }
                });
            });
        if close {
            self.dialog = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str) -> TagDefinition {
        TagDefinition {
            name: name.into(),
            description: String::new(),
            notable: false,
        }
    }

    #[test]
    fn definitions_are_validated_like_tag_name_dialog() {
        let defs = [def("Mine")];
        assert!(validate_definition(" ", "", &defs, false).is_err());
        assert!(validate_definition("a:b", "", &defs, false).is_err());
        assert!(validate_definition("ok", "x,y", &defs, false).is_err());
        assert!(validate_definition("mine", "", &defs, false).is_err());
        assert!(validate_definition("bookmark", "", &defs, false).is_err());
        assert!(validate_definition("Mine", "new", &defs, true).is_ok());
        assert!(validate_definition("Other", "", &defs, false).is_ok());
    }

    #[test]
    fn definitions_stay_sorted_and_unique() {
        let mut defs = vec![def("b")];
        upsert_definition(&mut defs, def("c"));
        upsert_definition(&mut defs, def("a"));
        upsert_definition(&mut defs, def("b"));
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }
}
