//! Hosts and persons (Autopsy `datamodel/hosts`, `datamodel/persons`):
//! the host node menu, Case › Manage Hosts, New Person and the merge
//! confirmation. Writes run as background tasks.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::Host;

use crate::case_tree::CaseTree;
use crate::task::Task;

/// A host or person change requested from the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostAction {
    AssociatePerson {
        host: i64,
        person: Option<i64>,
    },
    /// Open the New Person dialog for a host.
    NewPersonFor(i64),
    /// Ask before merging `source` into `dest`.
    ConfirmMerge {
        source: Host,
        dest: Host,
    },
}

/// Autopsy `HostNameValidator`.
pub(crate) fn validate_host_name(
    name: &str,
    original: Option<&str>,
    hosts: &[Host],
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Please provide some text for the host name.".into());
    }
    if original.is_some_and(|o| o == name) {
        return Err("Please provide a new name for this host.".into());
    }
    if hosts
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case(name) && Some(h.name.as_str()) != original)
    {
        return Err(
            "Another host already has the same name.  Please choose a different name.".into(),
        );
    }
    Ok(())
}

/// The host node's context menu.
pub(crate) fn host_menu(ui: &mut egui::Ui, host: &Host, tree: &CaseTree) -> Option<HostAction> {
    let mut out = None;
    if let Some(pid) = host.person_id {
        let name = tree
            .persons
            .iter()
            .find(|p| p.id == pid)
            .map_or("Unknown Person", |p| p.name.as_str());
        if ui.button(format!("Remove from Person ({name})")).clicked() {
            out = Some(HostAction::AssociatePerson {
                host: host.id,
                person: None,
            });
            ui.close();
        }
    } else {
        ui.menu_button("Associate with Person", |ui| {
            for p in &tree.persons {
                if ui.button(&p.name).clicked() {
                    out = Some(HostAction::AssociatePerson {
                        host: host.id,
                        person: Some(p.id),
                    });
                    ui.close();
                }
            }
            if !tree.persons.is_empty() {
                ui.separator();
            }
            if ui.button("New Person...").clicked() {
                out = Some(HostAction::NewPersonFor(host.id));
                ui.close();
            }
        });
    }
    let others: Vec<&Host> = tree.hosts.iter().filter(|h| h.id != host.id).collect();
    ui.add_enabled_ui(!others.is_empty(), |ui| {
        ui.menu_button("Merge Into Other Host", |ui| {
            for h in others {
                if ui.button(&h.name).clicked() {
                    out = Some(HostAction::ConfirmMerge {
                        source: host.clone(),
                        dest: h.clone(),
                    });
                    ui.close();
                }
            }
        });
    });
    out
}

/// An add/edit-host form inside Manage Hosts.
#[derive(Debug, Clone, Default)]
struct HostForm {
    /// `None`: a new host.
    id: Option<i64>,
    original: Option<String>,
    name: String,
    error: Option<String>,
}

/// The Manage Hosts dialog's selection.
#[derive(Debug, Clone, Copy, Default)]
struct ManageState {
    selected: Option<i64>,
}

/// Dialog and task state.
#[derive(Debug, Default)]
pub(crate) struct HostsUi {
    /// Case › Manage Hosts, when open.
    manage: Option<ManageState>,
    form: Option<HostForm>,
    new_person: Option<(i64, String, Option<String>)>,
    confirm_merge: Option<(Host, Host)>,
    tasks: Vec<Task<Result<(), String>>>,
}

impl HostsUi {
    pub(crate) fn open_manage(&mut self) {
        self.manage = Some(ManageState::default());
    }

    /// Handles a menu action.
    pub(crate) fn act(&mut self, ctx: &egui::Context, case: &Arc<Case>, a: HostAction) {
        match a {
            HostAction::AssociatePerson { host, person } => {
                self.spawn(ctx, case, move |c| {
                    c.db()
                        .set_host_person(host, person)
                        .map_err(|e| e.to_string())
                });
            }
            HostAction::NewPersonFor(host) => self.new_person = Some((host, String::new(), None)),
            HostAction::ConfirmMerge { source, dest } => self.confirm_merge = Some((source, dest)),
        }
    }

    fn spawn(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        f: impl FnOnce(&Case) -> Result<(), String> + Send + 'static,
    ) {
        let case = Arc::clone(case);
        self.tasks.push(Task::spawn(ctx, move || f(&case)));
    }

    /// Draws the dialogs; returns errors to show.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<Case>>,
        tree: &CaseTree,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        self.tasks.retain_mut(|t| match t.poll() {
            Some(r) => {
                if let Err(e) = r {
                    errors.push(e.clone());
                }
                false
            }
            None => true,
        });
        let Some(case) = case else {
            *self = Self::default();
            return errors;
        };
        self.manage_dialog(ctx, case, tree);
        self.new_person_dialog(ctx, case, tree);
        self.merge_dialog(ctx, case);
        errors
    }

    fn manage_dialog(&mut self, ctx: &egui::Context, case: &Arc<Case>, tree: &CaseTree) {
        let Some(ManageState { mut selected }) = self.manage else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new("Manage Hosts")
            .open(&mut open)
            .collapsible(false)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label("Hosts represent individual devices that may have multiple data sources.");
                ui.separator();
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(240.0);
                        ui.strong("Hosts");
                        egui::ScrollArea::vertical()
                            .id_salt("hosts-list")
                            .max_height(260.0)
                            .show(ui, |ui| {
                                for h in &tree.hosts {
                                    if ui
                                        .selectable_label(selected == Some(h.id), &h.name)
                                        .clicked()
                                    {
                                        selected = Some(h.id);
                                    }
                                }
                            });
                    });
                    ui.vertical(|ui| {
                        ui.strong("Host Details");
                        if let Some(h) =
                            selected.and_then(|id| tree.hosts.iter().find(|h| h.id == id))
                        {
                            ui.label(format!("Host Name: {}", h.name));
                            for t in tree
                                .iter()
                                .filter(|t| t.host.as_ref().map(|x| x.id) == Some(h.id))
                            {
                                ui.label(format!("  {}", t.ds.name));
                            }
                        }
                    });
                });
                let has_sources = selected.is_some_and(|id| {
                    tree.iter()
                        .any(|t| t.host.as_ref().map(|x| x.id) == Some(id))
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("New").clicked() {
                        self.form = Some(HostForm::default());
                    }
                    if ui
                        .add_enabled(selected.is_some(), egui::Button::new("Edit"))
                        .clicked()
                        && let Some(h) =
                            selected.and_then(|id| tree.hosts.iter().find(|h| h.id == id))
                    {
                        self.form = Some(HostForm {
                            id: Some(h.id),
                            original: Some(h.name.clone()),
                            name: h.name.clone(),
                            error: None,
                        });
                    }
                    // Only hosts without data sources can be deleted.
                    if ui
                        .add_enabled(
                            selected.is_some() && !has_sources,
                            egui::Button::new("Delete"),
                        )
                        .clicked()
                        && let Some(id) = selected
                    {
                        let name = tree
                            .hosts
                            .iter()
                            .find(|h| h.id == id)
                            .map(|h| h.name.clone())
                            .unwrap_or_default();
                        self.spawn(ctx, case, move |c| {
                            c.db().delete_host(id).map_err(|e| {
                                format!("Unable to delete host {name} at this time. ({e})")
                            })
                        });
                        selected = None;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        self.manage = (open && !close).then_some(ManageState { selected });
        self.host_form(ctx, case, tree);
    }

    fn host_form(&mut self, ctx: &egui::Context, case: &Arc<Case>, tree: &CaseTree) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let mut done = false;
        let mut submit = None;
        egui::Window::new(if form.id.is_some() {
            "Edit Host"
        } else {
            "Add Host"
        })
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(&mut form.name);
            });
            if let Some(e) = &form.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    match validate_host_name(&form.name, form.original.as_deref(), &tree.hosts) {
                        Ok(()) => submit = Some((form.id, form.name.trim().to_owned())),
                        Err(e) => form.error = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
        if let Some((id, name)) = submit {
            done = true;
            self.spawn(ctx, case, move |c| match id {
                Some(id) => c.db().rename_host(id, &name).map_err(|e| {
                    format!("Unable to update host {name} with id: {id} at this time. ({e})")
                }),
                None => c
                    .db()
                    .host_by_name_or_new(&name)
                    .map(|_| ())
                    .map_err(|e| format!("Unable to add new host {name} at this time. ({e})")),
            });
        }
        if done {
            self.form = None;
        }
    }

    fn new_person_dialog(&mut self, ctx: &egui::Context, case: &Arc<Case>, tree: &CaseTree) {
        let Some((host, name, error)) = self.new_person.as_mut() else {
            return;
        };
        let (host, mut done, mut submit) = (*host, false, None);
        egui::Window::new("Add Person").collapsible(false).resizable(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(name);
            });
            if let Some(e) = error.as_ref() {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() {
                    let n = name.trim();
                    if n.is_empty() {
                        *error = Some("Please provide some text for the person name.".into());
                    } else if tree.persons.iter().any(|p| p.name.eq_ignore_ascii_case(n)) {
                        *error = Some("Another person already has the same name.  Please choose a different name.".into());
                    } else {
                        submit = Some(n.to_owned());
                    }
                }
                if ui.button("Cancel").clicked() {
                    done = true;
                }
            });
        });
        if let Some(n) = submit {
            done = true;
            let host_name = tree
                .hosts
                .iter()
                .find(|h| h.id == host)
                .map(|h| h.name.clone())
                .unwrap_or_default();
            self.spawn(ctx, case, move |c| {
                let db = c.db();
                let p = db.new_person(&n).map_err(|e| {
                    format!("There was an error while associating host {host_name} with new person {n}. ({e})")
                })?;
                db.set_host_person(host, Some(p.id)).map_err(|e| e.to_string())
            });
        }
        if done {
            self.new_person = None;
        }
    }

    fn merge_dialog(&mut self, ctx: &egui::Context, case: &Arc<Case>) {
        let Some((source, dest)) = self.confirm_merge.clone() else {
            return;
        };
        let mut done = false;
        egui::Modal::new(egui::Id::new("merge-hosts")).show(ctx, |ui| {
            ui.heading("Confirmation");
            ui.label(format!(
                "Are you sure you want to merge {} into {}?\\nThis may include merging OS Accounts and cannot be undone.",
                source.name, dest.name
            ));
            ui.horizontal(|ui| {
                if ui.button("Yes").clicked() {
                    let (s, d) = (source.id, dest.id);
                    self.spawn(ctx, case, move |c| {
                        c.db().merge_hosts(s, d).map_err(|e| {
                            format!("An error occurred while merging hosts.\\nTry again in a few minutes or check the log for details. ({e})")
                        })
                    });
                    done = true;
                }
                if ui.button("No").clicked() {
                    done = true;
                }
            });
        });
        if done {
            self.confirm_merge = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: i64, name: &str) -> Host {
        Host {
            id,
            name: name.into(),
            person_id: None,
        }
    }

    #[test]
    fn host_names_are_validated_like_autopsy() {
        let hosts = [host(1, "laptop"), host(2, "desktop")];
        assert_eq!(
            validate_host_name(" ", None, &hosts).unwrap_err(),
            "Please provide some text for the host name."
        );
        assert!(
            validate_host_name("LAPTOP", None, &hosts)
                .unwrap_err()
                .starts_with("Another host")
        );
        assert_eq!(
            validate_host_name("laptop", Some("laptop"), &hosts).unwrap_err(),
            "Please provide a new name for this host."
        );
        assert!(validate_host_name("phone", None, &hosts).is_ok());
        assert!(validate_host_name("laptop2", Some("laptop"), &hosts).is_ok());
    }
}
