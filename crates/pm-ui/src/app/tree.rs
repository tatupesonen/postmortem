//! The tree on the left (data sources, views, results, case nodes) and navigation.

use std::sync::Arc;

use crate::icons;
use crate::model::{FileRow, NodeId};
use crate::task::Task;
use crate::views::View;

use super::{PostmortemApp, ResultTab};

impl PostmortemApp {
    pub fn navigate(&mut self, node: NodeId) {
        self.searches.tab = ResultTab::Listing;
        if self.selected.as_ref() == Some(&node) {
            return;
        }
        if let Some(prev) = self.selected.take() {
            self.back.push(prev);
        }
        self.forward.clear();
        self.selected = Some(node);
        self.table.reset();
        self.nodes.grid.reset();
        self.nodes.account_details = None;
        self.result_selected = None;
        self.result_table = crate::results::ResultTable::default();
    }

    pub(super) fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            if let Some(cur) = self.selected.replace(prev) {
                self.forward.push(cur);
            }
            self.table.reset();
        }
    }

    pub(super) fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            if let Some(cur) = self.selected.replace(next) {
                self.back.push(cur);
            }
            self.table.reset();
        }
    }

    pub(super) fn tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            if self.has_case() {
                let on = self.selected == Some(NodeId::Overview);
                if ui
                    .selectable_label(on, egui::RichText::new("📋 Case Overview").heading())
                    .clicked()
                {
                    self.navigate(NodeId::Overview);
                }
                ui.add_space(4.0);
            }
            ui.heading(format!("{} Data Sources", icons::DATA_SOURCES));
            if !self.has_data_sources() {
                ui.weak(if self.has_case() {
                    "No data sources. Use Case › Add Data Source."
                } else {
                    "No case open. Use Case › New Case or Open Case."
                });
            }
            let (case, live) = {
                let a = self.analysis.lock();
                (a.case().cloned(), a.any_active())
            };
            let trees = self.case_sources.get(ui.ctx(), case.as_ref(), live);
            if let Some(e) = self.case_sources.error() {
                ui.colored_label(ui.visuals().error_fg_color, e.to_owned());
            }
            if trees.is_empty() && self.case_sources.loading() {
                ui.spinner();
            }
            // A case with data sources opens on its overview.
            if self.selected.is_none() && !trees.is_empty() {
                self.navigate(NodeId::Overview);
            }
            // Autopsy's default layout: Data Sources › host › data source.
            let mut host_action = None;
            for (host, sources) in trees.by_host() {
                let node = NodeId::Host(host.map(|h| h.id));
                let label = format!(
                    "{} {}",
                    icons::HOST,
                    host.map_or("Unknown Host", |h| h.name.as_str())
                );
                let resp = self.tree_node_with(ui, &node, &label, true, true, |app, ui| {
                    for t in sources {
                        let node = NodeId::DataSource(t.ds.id);
                        let resp = app.tree_node(
                            ui,
                            &node,
                            &format!("{} {}", icons::DATA_SOURCE, t.ds.name),
                            true,
                            |app, ui| app.data_source_children(ui, t),
                        );
                        resp.context_menu(|ui| app.data_source_menu(ui, &t.ds));
                    }
                });
                if let Some(h) = host {
                    resp.context_menu(|ui| {
                        if let Some(a) = crate::hosts_ui::host_menu(ui, h, &trees) {
                            host_action = Some(a);
                        }
                    });
                }
            }
            if let (Some(a), Some(case)) = (host_action, case.as_ref()) {
                self.hosts_ui.act(ui.ctx(), case, a);
            }
            self.views_tree(ui);
            self.results_tree(ui);
            self.case_nodes_tree(ui);
        });
    }

    /// Autopsy's Views: file types, deleted files and file sizes.
    pub(super) fn views_tree(&mut self, ui: &mut egui::Ui) {
        let (case, live) = {
            let a = self.analysis.lock();
            (a.case().cloned(), a.any_active())
        };
        if case.is_none() || !self.has_data_sources() {
            return;
        }
        let hide = self.persisted.hide_known_in_views;
        let c = self.views.counts.get(ui.ctx(), case.as_ref(), live, hide);
        let n = |v: Option<&u64>| crate::format::grouped(v.copied().unwrap_or(0));
        ui.add_space(8.0);
        ui.heading(format!("{} Views", icons::VIEWS));
        let leaf = |app: &mut Self, ui: &mut egui::Ui, view: View, label: String| {
            app.tree_node(ui, &NodeId::View(view), &label, false, |_, _| {});
        };
        egui::CollapsingHeader::new(format!("{} File Types", icons::FILE_TYPES))
            .id_salt("views-types")
            .default_open(true)
            .show(ui, |ui| {
                egui::CollapsingHeader::new(format!("{} By Extension", icons::BY_EXTENSION))
                    .id_salt("views-ext")
                    .default_open(true)
                    .show(ui, |ui| {
                        for (i, (name, _)) in crate::views::EXT_GROUPS.iter().enumerate() {
                            leaf(
                                self,
                                ui,
                                View::Ext(i),
                                format!("{} {name} ({})", icons::file_group(name), n(c.ext.get(i))),
                            );
                        }
                        egui::CollapsingHeader::new(format!(
                            "{} Documents ({})",
                            icons::file_group("Documents"),
                            crate::format::grouped(c.doc.iter().sum())
                        ))
                        .id_salt("views-docs")
                        .show(ui, |ui| {
                            for (i, (name, _)) in crate::views::DOC_GROUPS.iter().enumerate() {
                                leaf(
                                    self,
                                    ui,
                                    View::Doc(i),
                                    format!(
                                        "{} {name} ({})",
                                        icons::file_group(name),
                                        n(c.doc.get(i))
                                    ),
                                );
                            }
                        });
                    });
                egui::CollapsingHeader::new(format!("{} By MIME Type", icons::BY_MIME))
                    .id_salt("views-mime")
                    .show(ui, |ui| {
                        if c.mime.is_empty() {
                            ui.weak("Run File Type Identification to populate.");
                        }
                        let mut media = String::new();
                        for (mime, count) in &c.mime {
                            let (top, sub) = mime.split_once('/').unwrap_or((mime.as_str(), ""));
                            if top != media {
                                top.clone_into(&mut media);
                                ui.strong(format!("{} {media}", icons::media_type(&media)));
                            }
                            leaf(
                                self,
                                ui,
                                View::Mime(mime.clone()),
                                format!("{sub} ({})", crate::format::grouped(*count)),
                            );
                        }
                    });
            });
        egui::CollapsingHeader::new(format!(
            "{} Deleted Files ({})",
            icons::DELETED,
            n(c.deleted.get(1))
        ))
        .id_salt("views-deleted")
        .show(ui, |ui| {
            leaf(
                self,
                ui,
                View::Deleted(true),
                format!("{} File System ({})", icons::DELETED, n(c.deleted.first())),
            );
            leaf(
                self,
                ui,
                View::Deleted(false),
                format!("{} All ({})", icons::DELETED, n(c.deleted.get(1))),
            );
        });
        egui::CollapsingHeader::new(format!("{} File Size", icons::FILE_SIZE))
            .id_salt("views-size")
            .show(ui, |ui| {
                for (i, (name, ..)) in crate::views::SIZE_BUCKETS.iter().enumerate() {
                    leaf(
                        self,
                        ui,
                        View::Size(i),
                        format!("{} {name} ({})", icons::FILE_SIZE, n(c.size.get(i))),
                    );
                }
            });
    }

    /// "Data Artifacts" and "Analysis Results" with per-type counts.
    pub(super) fn results_tree(&mut self, ui: &mut egui::Ui) {
        let (case, live) = {
            let a = self.analysis.lock();
            (a.case().cloned(), a.any_active())
        };
        if case.is_none() {
            return;
        }
        let counts = self.results.counts.get(ui.ctx(), case.as_ref(), live);
        // Cached rows go stale while results are still being added.
        for t in counts.iter() {
            let stale = self
                .results
                .rows
                .get_mut(&crate::results::ResultKey::Type(t.type_id))
                .and_then(Task::poll)
                .and_then(|r| r.as_ref().ok())
                .is_some_and(|rows| {
                    rows.rows.len() < (t.count as usize).min(crate::results::MAX_ROWS)
                });
            if stale {
                self.results
                    .rows
                    .remove(&crate::results::ResultKey::Type(t.type_id));
            }
        }
        for (icon, heading, cat) in [
            (
                icons::DATA_ARTIFACTS,
                "Data Artifacts",
                pm_casedb::catalog::Category::DataArtifact,
            ),
            (
                icons::ANALYSIS_RESULTS,
                "Analysis Results",
                pm_casedb::catalog::Category::AnalysisResult,
            ),
        ] {
            ui.add_space(8.0);
            ui.heading(format!("{icon} {heading}"));
            let of_cat: Vec<_> = counts
                .iter()
                .filter(|t| t.category == cat)
                .cloned()
                .collect();
            let analysis = cat == pm_casedb::catalog::Category::AnalysisResult;
            // OS Accounts sits under Analysis Results, so that section is
            // never empty.
            if of_cat.is_empty() && !analysis {
                ui.weak(if live {
                    "Analysis is running…"
                } else {
                    "None"
                });
            }
            for t in of_cat {
                let node = NodeId::Results { type_id: t.type_id };
                let icon =
                    icons::artifact(&t.name, cat == pm_casedb::catalog::Category::AnalysisResult);
                let label = format!("{icon} {} ({})", t.display, crate::format::grouped(t.count));
                if t.name == "KAAMOS_KEYWORD_HIT" {
                    self.keyword_hits_node(ui, &node, &label, case.as_ref(), t.count);
                } else {
                    self.tree_node(ui, &node, &label, false, |_, _| {});
                }
            }
            if analysis {
                self.os_accounts_node(ui, case.as_ref());
            }
        }
    }

    /// Keyword Hits › list › keyword › match (Autopsy `KeywordHits`).
    pub(super) fn keyword_hits_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        case: Option<&Arc<pm_case::Case>>,
        total: u64,
    ) {
        use crate::keyword_search_ui::MAX_TREE_CHILDREN;
        use pm_casedb::keyword_hits::KeywordHitFilter;
        let Some(case) = case else {
            return;
        };
        if total != self.results.keyword_groups_for {
            self.results.keyword_groups_for = total;
            self.results.keyword_groups.refresh();
            self.results
                .rows
                .retain(|k, _| !matches!(k, crate::results::ResultKey::Keyword(_)));
        }
        let loader = Arc::clone(case);
        let tree = self
            .results
            .keyword_groups
            .get(ui.ctx(), move || {
                let groups = loader
                    .db()
                    .keyword_hit_groups()
                    .map_err(|e| e.to_string())?;
                Ok(crate::keyword_search_ui::HitTree::build(&groups))
            })
            .unwrap_or_default();
        let filter = |list: &str, term: Option<&str>, inst: Option<&str>| {
            NodeId::KeywordHits(KeywordHitFilter {
                list: list.to_owned(),
                term: term.map(str::to_owned),
                instance: inst.map(str::to_owned),
            })
        };
        let more = |ui: &mut egui::Ui, n: usize| {
            if n > MAX_TREE_CHILDREN {
                ui.weak(format!(
                    "… {} more (listed in the parent's results)",
                    crate::format::grouped((n - MAX_TREE_CHILDREN) as u64)
                ));
            }
        };
        self.tree_node(ui, node, label, true, |app, ui| {
            for l in &tree.lists {
                let node = filter(&l.name, None, None);
                app.tree_node(ui, &node, &l.label, !l.terms.is_empty(), |app, ui| {
                    for t in l.terms.iter().take(MAX_TREE_CHILDREN) {
                        let node = filter(&l.name, Some(&t.term), None);
                        let regex = !t.instances.is_empty();
                        app.tree_node(ui, &node, &t.label, regex, |app, ui| {
                            for (inst, label) in t.instances.iter().take(MAX_TREE_CHILDREN) {
                                let node = filter(&l.name, Some(&t.term), Some(inst));
                                app.tree_node(ui, &node, label, false, |_, _| {});
                            }
                            more(ui, t.instances.len());
                        });
                    }
                    more(ui, l.terms.len());
                });
            }
        });
    }

    /// The OS Accounts node (its count comes with the case summary).
    pub(super) fn os_accounts_node(
        &mut self,
        ui: &mut egui::Ui,
        case: Option<&Arc<pm_case::Case>>,
    ) {
        let Some(case) = case else {
            return;
        };
        let loader = Arc::clone(case);
        let count = self
            .nodes
            .summary
            .get(ui.ctx(), move || crate::case_nodes::load_summary(&loader))
            .map(|s| crate::format::grouped(s.os_accounts));
        let label = format!(
            "{} OS Accounts ({})",
            icons::OS_ACCOUNTS,
            count.as_deref().unwrap_or("…")
        );
        self.tree_node(ui, &NodeId::OsAccounts, &label, false, |_, _| {});
    }

    /// A host node: its data sources (Autopsy lists the node's children).
    pub(super) fn host_view(&mut self, ui: &mut egui::Ui, host: Option<i64>) {
        let case = self.analysis.lock().case().cloned();
        let tree = self.case_sources.get(ui.ctx(), case.as_ref(), false);
        let name = host
            .and_then(|id| tree.hosts.iter().find(|h| h.id == id))
            .map_or("Unknown Host", |h| h.name.as_str());
        ui.heading(format!("{} {name}", icons::HOST));
        let mut open = None;
        for t in tree
            .iter()
            .filter(|t| t.host.as_ref().map(|h| h.id) == host)
        {
            if ui
                .selectable_label(false, format!("{} {}", icons::DATA_SOURCE, t.ds.name))
                .double_clicked()
            {
                open = Some(t.ds.id);
            }
        }
        if let Some(ds) = open {
            self.navigate(NodeId::DataSource(ds));
        }
    }

    /// Autopsy's data source node actions.
    pub(super) fn data_source_menu(&mut self, ui: &mut egui::Ui, ds: &pm_casedb::DataSourceRecord) {
        let busy = self.analysis.lock().any_active();
        if ui
            .add_enabled(!busy, egui::Button::new("Run Ingest Modules"))
            .on_disabled_hover_text("Ingest is running")
            .clicked()
        {
            self.run_ingest = Some(crate::add_source::RunIngestDialog {
                ds_id: ds.id,
                ds_name: ds.name.clone(),
                prefs: self.persisted.ingest.clone(),
            });
            ui.close();
        }
        if ui.button("Open File Search by Attributes").clicked() {
            let w = self
                .file_search
                .get_or_insert_with(crate::file_search_ui::FileSearchWindow::default);
            w.preselect_data_source(ds.id);
            self.views.mime_types.refresh();
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(!busy, egui::Button::new("Remove Data Source"))
            .on_disabled_hover_text("Cannot remove a data source while ingest is running")
            .clicked()
        {
            self.confirm_remove = Some((ds.id, ds.name.clone()));
            ui.close();
        }
    }

    /// Tags and Score (Autopsy's tree order after the results).
    pub(super) fn case_nodes_tree(&mut self, ui: &mut egui::Ui) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        let loader = Arc::clone(&case);
        let Some(sum) = self
            .nodes
            .summary
            .get(ui.ctx(), move || crate::case_nodes::load_summary(&loader))
        else {
            return;
        };
        ui.add_space(8.0);
        ui.heading(format!("{} Tags", icons::TAGS));
        if sum.tags.is_empty() {
            ui.weak("None");
        }
        for t in &sum.tags {
            egui::CollapsingHeader::new(format!("{} ({})", t.label, t.files + t.results))
                .id_salt(("tag-name", t.id))
                .show(ui, |ui| {
                    self.tree_node(
                        ui,
                        &NodeId::TagFiles(t.id),
                        &format!("File Tags ({})", t.files),
                        false,
                        |_, _| {},
                    );
                    self.tree_node(
                        ui,
                        &NodeId::TagResults(t.id),
                        &format!("Result Tags ({})", t.results),
                        false,
                        |_, _| {},
                    );
                });
        }
        ui.add_space(8.0);
        ui.heading(format!("{} Score", icons::SCORE));
        self.tree_node(
            ui,
            &NodeId::Score { bad: true },
            &format!(
                "{} Bad Items ({})",
                icons::SCORE_NOTABLE,
                crate::format::grouped(sum.bad)
            ),
            false,
            |_, _| {},
        );
        self.tree_node(
            ui,
            &NodeId::Score { bad: false },
            &format!(
                "{} Suspicious Items ({})",
                icons::SCORE_SUSPICIOUS,
                crate::format::grouped(sum.suspicious)
            ),
            false,
            |_, _| {},
        );
        // Autopsy's Reports node: generated reports, opened on click.
        ui.add_space(8.0);
        ui.heading(format!("{} Reports", icons::REPORTS));
        if sum.reports.is_empty() {
            ui.weak("None");
        }
        let mut remove = None;
        for r in &sum.reports {
            let when = self
                .prefs
                .time(jiff::Timestamp::from_second(r.created_at).ok());
            let path = std::path::PathBuf::from(&r.path);
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let resp = ui
                    .selectable_label(false, format!("{} {when}", r.display_name))
                    .on_hover_text(&r.path);
                if resp.clicked() {
                    crate::platform::open(&path);
                }
                resp.context_menu(|ui| {
                    if ui.button("Open Report").clicked() {
                        crate::platform::open(&path);
                        ui.close();
                    }
                    if ui.button("Open Containing Folder").clicked() {
                        if let Some(dir) = path.parent() {
                            crate::platform::open(dir);
                        }
                        ui.close();
                    }
                    if ui.button("Delete Report").clicked() {
                        remove = Some(r.id);
                        ui.close();
                    }
                });
            });
        }
        if let Some(id) = remove
            && let Err(e) = case.db().delete_report(id)
        {
            self.errors.push(e.to_string());
        }
    }

    /// One collapsible tree node with a selectable label; children render lazily.
    pub(super) fn tree_node(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        has_children: bool,
        children: impl FnOnce(&mut Self, &mut egui::Ui),
    ) -> egui::Response {
        self.tree_node_with(ui, node, label, has_children, false, children)
    }

    /// [`Self::tree_node`] with a choice of initial expansion.
    pub(super) fn tree_node_with(
        &mut self,
        ui: &mut egui::Ui,
        node: &NodeId,
        label: &str,
        has_children: bool,
        default_open: bool,
        children: impl FnOnce(&mut Self, &mut egui::Ui),
    ) -> egui::Response {
        // Keyed by the node alone: an id derived from the parent widgets
        // changes whenever they do (e.g. while the tree reloads), and the
        // node then forgets that it was opened.
        let id = egui::Id::new(("tree-node", node));
        let selected = self.selected.as_ref() == Some(node);
        if has_children {
            let state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                default_open,
            );
            let (_, header, _) = state
                .show_header(ui, |ui| ui.selectable_label(selected, label))
                .body(|ui| children(self, ui));
            // Click selects and expands; double-click toggles.
            let (clicked, double) = (header.inner.clicked(), header.inner.double_clicked());
            if clicked || double {
                let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    id,
                    false,
                );
                st.set_open(if double { !st.is_open() } else { true });
                st.store(ui.ctx());
            }
            if clicked {
                self.navigate(node.clone());
            }
            header.inner
        } else {
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let r = ui.selectable_label(selected, label);
                if r.clicked() {
                    // A node can gain children after it was drawn as a
                    // leaf (a volume's file systems while the tree
                    // reloads): it then opens as if clicked open.
                    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(
                        ui.ctx(),
                        id,
                        false,
                    );
                    st.set_open(true);
                    st.store(ui.ctx());
                    self.navigate(node.clone());
                }
                r
            })
            .inner
        }
    }

    pub(super) fn data_source_children(&mut self, ui: &mut egui::Ui, t: &crate::case_tree::DsTree) {
        for v in &t.volumes {
            let label = format!(
                "{} {}",
                if v.unallocated() {
                    icons::UNALLOCATED
                } else {
                    icons::VOLUME
                },
                v.label()
            );
            let node = NodeId::Volume {
                ds: t.ds.id,
                volume: v.volume.id,
            };
            self.tree_node(ui, &node, &label, !v.roots.is_empty(), |app, ui| {
                for r in &v.roots {
                    app.fs_root_children(ui, r);
                }
            });
        }
        if t.volumes.is_empty() && t.loose.is_empty() && t.top_dirs.is_empty() && t.top_files == 0 {
            ui.weak("No file system found");
        }
        for r in &t.loose {
            self.fs_root_children(ui, r);
        }
        // Logical file sets and unallocated space images.
        for d in &t.top_dirs {
            let node = NodeId::Dir {
                ds: t.ds.id,
                file: d.id,
            };
            let label = format!("{} {}", icons::FOLDER, d.name);
            self.tree_node(ui, &node, &label, true, |app, ui| {
                app.dir_children(ui, &node);
            });
        }
        if t.top_files > 0 {
            let node = NodeId::TopFiles(t.ds.id);
            let label = format!("{} Files ({})", icons::FOLDER, t.top_files);
            self.tree_node(ui, &node, &label, false, |_, _| {});
        }
    }

    pub(super) fn fs_root_children(&mut self, ui: &mut egui::Ui, r: &crate::case_tree::FsRoot) {
        let root = NodeId::Dir {
            ds: r.fs.ds_id,
            file: r.root.id,
        };
        let label = format!("{} / ({})", icons::FOLDER, r.fs.kind);
        self.tree_node(ui, &root, &label, true, |app, ui| {
            app.dir_children(ui, &root);
        });
    }

    pub(super) fn dir_children(&mut self, ui: &mut egui::Ui, node: &NodeId) {
        let NodeId::Dir { ds, .. } = node else {
            return;
        };
        let ds = *ds;
        let ctx = ui.ctx().clone();
        match self.listing(&ctx, node).cloned() {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Loading…");
                });
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            Some(Ok(rows)) => {
                let mut dirs: Vec<&FileRow> = rows.iter().filter(|r| r.can_open()).collect();
                dirs.sort_by_key(|r| r.name.to_lowercase());
                for r in dirs {
                    let Some(file) = r.file_id() else { continue };
                    let child = NodeId::Dir { ds, file };
                    let icon = if r.is_dir() {
                        icons::FOLDER
                    } else {
                        icons::ARCHIVE
                    };
                    let label = if r.deleted() {
                        format!("{icon} {} ✖", r.name)
                    } else {
                        format!("{icon} {}", r.name)
                    };
                    self.tree_node(ui, &child, &label, true, |app, ui| {
                        app.dir_children(ui, &child);
                    });
                }
            }
        }
    }
}
