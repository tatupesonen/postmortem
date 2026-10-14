//! Case creation and the welcome screen (Autopsy's New Case wizard and
//! Welcome window).

use std::path::{Path, PathBuf};

use pm_case::{CaseDetails, Examiner};

/// Characters Autopsy rejects in case names (`Case.isValidName`).
const ILLEGAL: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

/// Checks a case name; the name also becomes the case folder's name.
pub(crate) fn validate_case_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a case name.".into());
    }
    if let Some(c) = name.chars().find(|c| ILLEGAL.contains(c) || c.is_control()) {
        return Err(format!(
            "The case name cannot contain {c:?} (not allowed: \\ / : * ? \" < > |)."
        ));
    }
    if name == "." || name == ".." {
        return Err("Choose another case name.".into());
    }
    Ok(())
}

/// What the wizard asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WizardAction {
    None,
    Cancel,
    Create { dir: PathBuf, details: CaseDetails },
}

/// The two-step New Case wizard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewCaseWizard {
    step: u8,
    pub(crate) name: String,
    pub(crate) base: String,
    pub(crate) number: String,
    pub(crate) examiner: String,
    pub(crate) phone: String,
    pub(crate) email: String,
    pub(crate) notes: String,
    pub(crate) error: Option<String>,
}

impl NewCaseWizard {
    pub(crate) fn new(base: &Path) -> Self {
        Self {
            step: 1,
            name: String::new(),
            base: base.display().to_string(),
            number: String::new(),
            examiner: String::new(),
            phone: String::new(),
            email: String::new(),
            notes: String::new(),
            error: None,
        }
    }

    /// The folder the case will be created in.
    pub(crate) fn case_dir(&self) -> PathBuf {
        PathBuf::from(self.base.trim()).join(self.name.trim())
    }

    /// Validates step 1 (name and location).
    pub(crate) fn check_location(&self) -> Result<(), String> {
        validate_case_name(&self.name)?;
        if self.base.trim().is_empty() {
            return Err("Choose a base directory.".into());
        }
        let dir = self.case_dir();
        if dir.exists() && std::fs::read_dir(&dir).map_or(true, |mut d| d.next().is_some()) {
            return Err(format!(
                "{} already exists. Choose another case name or base directory.",
                dir.display()
            ));
        }
        Ok(())
    }

    fn details(&self) -> CaseDetails {
        CaseDetails {
            name: self.name.trim().to_owned(),
            number: self.number.trim().to_owned(),
            examiner: Examiner {
                name: self.examiner.trim().to_owned(),
                phone: self.phone.trim().to_owned(),
                email: self.email.trim().to_owned(),
            },
            notes: self.notes.clone(),
        }
    }

    /// Draws the wizard as a modal over the main window.
    pub(crate) fn ui(&mut self, ctx: &egui::Context) -> WizardAction {
        let mut action = WizardAction::None;
        let modal = egui::Modal::new(egui::Id::new("new-case")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.heading("New Case");
            ui.label(match self.step {
                1 => "Step 1 of 2 — Case Information",
                _ => "Step 2 of 2 — Optional Information",
            });
            ui.separator();
            if self.step == 1 {
                self.step_one(ui);
            } else {
                self.step_two(ui);
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    action = WizardAction::Cancel;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.step == 1 {
                        if ui.button("Next ›").clicked() {
                            match self.check_location() {
                                Ok(()) => {
                                    self.error = None;
                                    self.step = 2;
                                }
                                Err(e) => self.error = Some(e),
                            }
                        }
                    } else {
                        if ui.button("Finish").clicked() {
                            match self.check_location() {
                                Ok(()) => {
                                    action = WizardAction::Create {
                                        dir: self.case_dir(),
                                        details: self.details(),
                                    };
                                }
                                Err(e) => {
                                    self.error = Some(e);
                                    self.step = 1;
                                }
                            }
                        }
                        if ui.button("‹ Back").clicked() {
                            self.step = 1;
                        }
                    }
                });
            });
        });
        if modal.should_close() && action == WizardAction::None {
            action = WizardAction::Cancel;
        }
        action
    }

    fn step_one(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("case-info")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Case Name:");
                let mut edited = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.name)
                            .hint_text("e.g. Operation Teapot")
                            .desired_width(380.0),
                    )
                    .changed();
                ui.end_row();
                ui.label("Base Directory:");
                ui.horizontal(|ui| {
                    edited |= ui
                        .add(egui::TextEdit::singleline(&mut self.base).desired_width(300.0))
                        .changed();
                    if ui.button("Browse…").clicked()
                        && let Some(p) = rfd::FileDialog::new()
                            .set_title("Base directory for the case")
                            .set_directory(self.base.trim())
                            .pick_folder()
                    {
                        self.base = p.display().to_string();
                        edited = true;
                    }
                });
                ui.end_row();
                if edited {
                    self.error = None;
                }
                ui.label("Case Type:");
                ui.horizontal(|ui| {
                    let _ = ui.radio(true, "Single-user");
                    ui.add_enabled(false, egui::RadioButton::new(false, "Multi-user"))
                        .on_disabled_hover_text(
                            "Configure multi-user settings first (Tools › Options).",
                        );
                });
                ui.end_row();
                ui.label("Case data will be stored in:");
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(self.case_dir().display().to_string()).monospace(),
                    )
                    .wrap(),
                );
                ui.end_row();
            });
    }

    fn step_two(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("case-optional")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.strong("Case");
                ui.end_row();
                ui.label("Number:");
                ui.add(egui::TextEdit::singleline(&mut self.number).desired_width(380.0));
                ui.end_row();
                ui.strong("Examiner");
                ui.end_row();
                ui.label("Name:");
                ui.add(egui::TextEdit::singleline(&mut self.examiner).desired_width(380.0));
                ui.end_row();
                ui.label("Phone:");
                ui.add(egui::TextEdit::singleline(&mut self.phone).desired_width(380.0));
                ui.end_row();
                ui.label("Email:");
                ui.add(egui::TextEdit::singleline(&mut self.email).desired_width(380.0));
                ui.end_row();
                ui.label("Notes:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.notes)
                        .desired_rows(4)
                        .desired_width(380.0),
                );
                ui.end_row();
            });
    }
}

/// Is `path` a case (a `case.pmcase` file or a folder containing one)?
pub(crate) fn is_case_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "pmcase") || path.join(pm_case::CASE_FILE).is_file()
}

/// What the welcome screen asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WelcomeAction {
    None,
    NewCase,
    OpenCase,
    OpenRecent(PathBuf),
}

/// Autopsy's Welcome window, shown when no case is open.
pub(crate) fn welcome(
    ui: &mut egui::Ui,
    wordmark: Option<&egui::TextureHandle>,
    recent: &[PathBuf],
) -> WelcomeAction {
    let mut action = WelcomeAction::None;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() / 2.0 - 220.0).max(0.0));
        if let Some(tex) = wordmark {
            ui.add(egui::Image::new(tex).max_width(420.0));
        }
        ui.add_space(24.0);
        let big = |text: &str| {
            egui::Button::new(egui::RichText::new(text).size(16.0))
                .min_size(egui::vec2(220.0, 36.0))
        };
        if ui.add(big("🗁  New Case")).clicked() {
            action = WelcomeAction::NewCase;
        }
        ui.add_space(6.0);
        if ui.add(big("📂  Open Case")).clicked() {
            action = WelcomeAction::OpenCase;
        }
        if !recent.is_empty() {
            ui.add_space(18.0);
            ui.weak("Recent cases");
            for p in recent.iter().take(8) {
                let name = p.file_name().map_or_else(
                    || p.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                if ui
                    .link(name)
                    .on_hover_text(p.display().to_string())
                    .clicked()
                {
                    action = WelcomeAction::OpenRecent(p.clone());
                }
            }
        }
        ui.add_space(18.0);
        ui.weak(
            "A case holds your data sources and analysis results. Create or open one to begin.",
        );
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_names() {
        assert!(validate_case_name("Operation Teapot 2026").is_ok());
        assert!(validate_case_name("  ").is_err());
        for bad in ["a/b", "a\\b", "c:d", "x*", "q?", "\"", "<x>", "a|b", ".."] {
            assert!(validate_case_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn location_checks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut w = NewCaseWizard::new(tmp.path());
        assert!(w.check_location().is_err());
        w.name = " Teapot ".into();
        assert_eq!(w.case_dir(), tmp.path().join("Teapot"));
        assert!(w.check_location().is_ok());
        std::fs::create_dir(tmp.path().join("Teapot")).unwrap();
        assert!(w.check_location().is_ok(), "empty folder is fine");
        std::fs::write(tmp.path().join("Teapot/x"), b"x").unwrap();
        assert!(w.check_location().is_err());
        assert_eq!(w.details().name, "Teapot");
    }

    #[test]
    fn case_paths() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_case_path(tmp.path()));
        std::fs::write(tmp.path().join("case.pmcase"), "").unwrap();
        assert!(is_case_path(tmp.path()));
        assert!(is_case_path(Path::new("/x/case.pmcase")));
        assert!(!is_case_path(Path::new("/x/disk.E01")));
    }
}
