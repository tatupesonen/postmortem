//! The command palette's entries and actions (see `crate::palette`).

use std::sync::Arc;

use crate::model::NodeId;
use crate::palette::{Action, Entry, Tool};
use crate::views::View;

use super::PostmortemApp;

impl PostmortemApp {
    /// Draws the palette when open and runs what is chosen.
    pub(super) fn command_palette(&mut self, ctx: &egui::Context) {
        if !self.palette.is_open() {
            return;
        }
        let case = self.analysis.lock().case().cloned();
        let entries = self.palette_entries(ctx, case.as_ref());
        if let Some(action) = self.palette.ui(ctx, case.as_ref(), &entries) {
            self.run_action(ctx, action);
        }
    }

    /// Tools, then the tree's nodes.
    fn palette_entries(
        &mut self,
        ctx: &egui::Context,
        case: Option<&Arc<pm_case::Case>>,
    ) -> Vec<Entry> {
        let mut out = crate::palette::tool_entries();
        let Some(case) = case else {
            return out;
        };
        let live = self.analysis.lock().any_active();
        let tree = self.case_sources.get(ctx, Some(case), live);
        for ds in tree.iter() {
            out.push(Entry::new(
                ds.ds.name.clone(),
                "Data Source",
                Action::Navigate(NodeId::DataSource(ds.ds.id)),
            ));
        }
        for (i, (name, _)) in crate::views::EXT_GROUPS.iter().enumerate() {
            out.push(Entry::new(
                *name,
                "Views › File Types",
                Action::Navigate(NodeId::View(View::Ext(i))),
            ));
        }
        for (i, (name, _)) in crate::views::DOC_GROUPS.iter().enumerate() {
            out.push(Entry::new(
                format!("Documents › {name}"),
                "Views › File Types",
                Action::Navigate(NodeId::View(View::Doc(i))),
            ));
        }
        for fs_only in [true, false] {
            let v = View::Deleted(fs_only);
            out.push(Entry::new(
                v.title(),
                "Views",
                Action::Navigate(NodeId::View(v)),
            ));
        }
        for i in 0..crate::views::SIZE_BUCKETS.len() {
            let v = View::Size(i);
            out.push(Entry::new(
                v.title(),
                "Views",
                Action::Navigate(NodeId::View(v)),
            ));
        }
        for t in self.results.counts.get(ctx, Some(case), live).iter() {
            let section = match t.category {
                pm_casedb::catalog::Category::DataArtifact => "Data Artifacts",
                pm_casedb::catalog::Category::AnalysisResult => "Analysis Results",
            };
            out.push(Entry::new(
                format!("{} ({})", t.display, t.count),
                section,
                Action::Navigate(NodeId::Results { type_id: t.type_id }),
            ));
        }
        out.push(Entry::new(
            "OS Accounts",
            "Analysis Results",
            Action::Navigate(NodeId::OsAccounts),
        ));
        for (label, bad) in [("Bad Items", true), ("Suspicious Items", false)] {
            out.push(Entry::new(
                label,
                "Score",
                Action::Navigate(NodeId::Score { bad }),
            ));
        }
        if let Some(summary) = self.nodes.summary.peek() {
            for t in &summary.tags {
                out.push(Entry::new(
                    format!("{} › File Tags", t.label),
                    "Tags",
                    Action::Navigate(NodeId::TagFiles(t.id)),
                ));
                out.push(Entry::new(
                    format!("{} › Result Tags", t.label),
                    "Tags",
                    Action::Navigate(NodeId::TagResults(t.id)),
                ));
            }
        }
        out
    }

    /// Runs a palette choice.
    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Tool(t) => self.open_tool(ctx, t),
            Action::Navigate(node) => self.navigate(node),
            Action::RevealFile(id) => self.reveal_file(id),
            Action::RevealArtifact(id) => self.reveal_artifact(id),
            Action::Pivot(v) => self.pivot = Some(crate::pivot::PivotWindow::new(&v)),
        }
    }

    fn open_tool(&mut self, ctx: &egui::Context, tool: Tool) {
        let needs_case = !matches!(
            tool,
            Tool::NewCase | Tool::OpenCase | Tool::Options | Tool::Plugins
        );
        if needs_case && !self.has_case() {
            self.errors.push("Create or open a case first.".into());
            return;
        }
        match tool {
            Tool::NewCase => self.new_case_wizard(),
            Tool::OpenCase => self.pick_case(ctx),
            Tool::AddDataSource => self.pick_image(ctx),
            Tool::Timeline => {
                self.open_timeline(ctx);
            }
            Tool::Gallery => self.open_gallery(ctx),
            Tool::Discovery => self.open_discovery(ctx),
            Tool::Communications => self.open_communications(ctx),
            Tool::Geolocation => self.open_geolocation(ctx),
            Tool::GenerateReport => self.open_report_wizard(ctx),
            Tool::KeywordSearch => {
                self.keyword_panel
                    .get_or_insert_with(crate::keyword_search_ui::KeywordSearchPanel::default);
                self.keyword_indexed.refresh();
            }
            Tool::FileSearch => {
                self.file_search
                    .get_or_insert_with(crate::file_search_ui::FileSearchWindow::default);
                self.views.mime_types.refresh();
            }
            Tool::IngestProgress => self.progress_open = true,
            Tool::Plugins => self.plugins_open = true,
            Tool::Options => self.open_options(),
            Tool::Notes => {
                self.notes
                    .get_or_insert_with(crate::notes_ui::NotesWindow::default);
            }
        }
    }

    /// Opens a file's folder and focuses the file (`file:123`).
    pub(super) fn reveal_file(&mut self, file_id: i64) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        match case.db().file(file_id) {
            Ok(Some(f)) => {
                let node = match f.parent_id {
                    Some(dir) => NodeId::Dir {
                        ds: f.ds_id,
                        file: dir,
                    },
                    None => NodeId::TopFiles(f.ds_id),
                };
                self.navigate(node);
                self.table.reveal(file_id);
            }
            Ok(None) => self
                .errors
                .push(format!("There is no file {file_id} in this case.")),
            Err(e) => self.errors.push(e.to_string()),
        }
    }

    /// Opens a result's type and focuses the result (`artifact:45`).
    pub(super) fn reveal_artifact(&mut self, artifact_id: i64) {
        let Some(case) = self.analysis.lock().case().cloned() else {
            return;
        };
        match case.db().artifact(artifact_id) {
            Ok(Some(a)) => {
                self.navigate(NodeId::Results { type_id: a.type_id });
                self.result_table.reveal(artifact_id);
            }
            Ok(None) => self
                .errors
                .push(format!("There is no result {artifact_id} in this case.")),
            Err(e) => self.errors.push(e.to_string()),
        }
    }
}
