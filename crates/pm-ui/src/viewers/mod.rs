//! Content viewers (the lower pane, and pop-out windows).
//!
//! As in Autopsy, several viewers are offered as tabs and the most useful
//! one for the selected file is chosen automatically. The user's explicit
//! choice is kept while they browse files of the same kind.

mod extracted;
mod hex;
mod image_view;
mod metadata;
mod strings;
mod text;

use std::sync::Arc;

use pm_core::SharedSource;

use crate::format::DisplayPrefs;
use crate::model::Content;

pub(crate) use extracted::ExtractedView;
pub(crate) use hex::HexView;
pub(crate) use image_view::ImageView;
pub(crate) use strings::StringsView;
pub(crate) use text::TextView;

/// Viewer tabs, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Tab {
    Hex,
    Strings,
    Extracted,
    Text,
    Application,
    Metadata,
}

impl Tab {
    pub(crate) const ALL: [Self; 6] = [
        Self::Hex,
        Self::Strings,
        Self::Extracted,
        Self::Text,
        Self::Application,
        Self::Metadata,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::Strings => "Strings",
            Self::Extracted => "Extracted Text",
            Self::Text => "Text",
            Self::Application => "Application",
            Self::Metadata => "File Metadata",
        }
    }
}

/// What a file's first bytes suggest it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sniffed {
    Picture,
    Text,
    Binary,
    Empty,
}

/// Classifies content from its leading bytes.
pub(crate) fn sniff(head: &[u8]) -> Sniffed {
    if head.is_empty() {
        return Sniffed::Empty;
    }
    if image_view::is_picture(head) {
        return Sniffed::Picture;
    }
    let sample = head.get(..4096).unwrap_or(head);
    let printable = sample
        .iter()
        .filter(|&&b| {
            b == b'\n' || b == b'\r' || b == b'\t' || (0x20..0x7F).contains(&b) || b >= 0x80
        })
        .count();
    let has_nul = sample.contains(&0);
    if !has_nul && printable * 100 / sample.len() >= 95
        || sample.starts_with(&[0xFF, 0xFE])
        || sample.starts_with(&[0xEF, 0xBB, 0xBF])
    {
        Sniffed::Text
    } else {
        Sniffed::Binary
    }
}

/// The shown file as the case database has it now (hashes, known status
/// and MIME type are filled in by analysis after the file was listed).
#[derive(Debug, Clone)]
pub(crate) struct CaseFileInfo {
    pub(crate) case: Arc<pm_case::Case>,
    pub(crate) record: Arc<pm_casedb::FileRecord>,
    pub(crate) md5: Option<String>,
    pub(crate) sha1: Option<String>,
    pub(crate) sha256: Option<String>,
    pub(crate) known: String,
    /// Detected by File Type Identification.
    pub(crate) mime: Option<String>,
}

type LookupFn = dyn Fn(&Content) -> Option<CaseFileInfo> + Send + Sync;

/// Finds a shown file in the open case (`None`: not in the case), for
/// every viewer panel (main window, pop-outs, gallery, Discovery,
/// Timeline). It runs off the UI thread.
#[derive(Clone)]
pub(crate) struct CaseLookup(pub(crate) Arc<LookupFn>);

impl std::fmt::Debug for CaseLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaseLookup")
    }
}

#[derive(Debug)]
pub(crate) struct ViewerPanel {
    content: Option<Content>,
    source: Option<Result<SharedSource, String>>,
    /// File system of the shown file (for live metadata, streams, layout).
    fs: Option<Arc<dyn pm_fs::FileSystem>>,
    kind: Sniffed,
    tab: Tab,
    /// Tab explicitly chosen by the user (overrides auto-selection).
    pinned: Option<Tab>,
    hex: HexView,
    strings: StringsView,
    text: TextView,
    extracted: ExtractedView,
    /// The keyword search whose hits the Extracted Text viewer marks.
    query: Option<pm_search::TextQuery>,
    image: ImageView,
    /// Loaded in the background (the case database is never read on the
    /// UI thread); `None` until requested.
    info: Option<crate::task::Task<Option<CaseFileInfo>>>,
    /// The shown file's hashes must be (re)loaded.
    hashes_stale: bool,
}

impl Default for ViewerPanel {
    fn default() -> Self {
        Self {
            content: None,
            source: None,
            fs: None,
            kind: Sniffed::Empty,
            tab: Tab::Hex,
            pinned: None,
            hex: HexView::default(),
            strings: StringsView::default(),
            text: TextView::default(),
            extracted: ExtractedView::default(),
            query: None,
            image: ImageView::default(),
            info: None,
            hashes_stale: false,
        }
    }
}

impl ViewerPanel {
    pub(crate) fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }

    /// Shows `content` (no-op if already shown).
    /// Re-reads the shown file's hashes (e.g. after analysis finished).
    pub(crate) fn refresh_hashes(&mut self) {
        self.hashes_stale = true;
    }

    /// Starts looking the shown file up in the case when stale.
    fn load_info(&mut self, ctx: &egui::Context, prefs: &DisplayPrefs) {
        if let Some(t) = &mut self.info {
            t.poll();
        }
        if !std::mem::take(&mut self.hashes_stale) {
            return;
        }
        self.info = match (&prefs.case, &self.content) {
            (Some(r), Some(c)) => {
                let (r, c) = (r.clone(), c.clone());
                Some(crate::task::Task::spawn(ctx, move || (r.0)(&c)))
            }
            _ => None,
        };
    }

    pub(crate) fn set_content(&mut self, content: Option<Content>) {
        self.show(content, None);
    }

    /// Shows `content` with the hits of `query` marked in the Extracted
    /// Text viewer, which is then preferred (as in Autopsy, for keyword
    /// search results and keyword hits).
    pub(crate) fn show(&mut self, content: Option<Content>, query: Option<pm_search::TextQuery>) {
        if self.query != query {
            self.query = query;
            self.extracted = ExtractedView::default();
            if self.content == content {
                self.tab = self.preferred();
                return;
            }
        }
        if self.content == content {
            return;
        }
        self.extracted = ExtractedView::default();
        self.hex = HexView::default();
        self.strings = StringsView::default();
        self.text = TextView::default();
        self.image = ImageView::default();
        self.source = content
            .as_ref()
            .filter(|c| !c.row.is_dir())
            .map(Content::source);
        self.fs = content.as_ref().and_then(Content::filesystem);
        self.kind = match &self.source {
            Some(Ok(src)) => {
                let mut head = vec![0u8; 4096];
                let n = src.read_at(0, &mut head).unwrap_or(0);
                head.truncate(n);
                sniff(&head)
            }
            _ => Sniffed::Empty,
        };
        self.content = content;
        self.refresh_hashes();
        self.tab = self
            .pinned
            .filter(|t| self.supports(*t))
            .unwrap_or_else(|| self.preferred());
    }

    /// Whether the case lookup found the shown file.
    fn info_ready(&self) -> bool {
        self.info
            .as_ref()
            .is_some_and(|t| t.peek().is_some_and(Option::is_some))
    }

    fn supports(&self, tab: Tab) -> bool {
        match tab {
            Tab::Metadata => self.content.is_some(),
            Tab::Extracted => self
                .content
                .as_ref()
                .is_some_and(|c| !c.row.is_dir() && (c.case_file().is_some() || self.info_ready())),
            Tab::Application => self.kind == Sniffed::Picture,
            Tab::Hex | Tab::Strings | Tab::Text => {
                matches!(self.source, Some(Ok(_))) && self.kind != Sniffed::Empty
            }
        }
    }

    /// Autopsy-style preferred viewer: application viewer for pictures,
    /// text for text, otherwise hex; metadata for directories.
    fn preferred(&self) -> Tab {
        if self.query.is_some() && self.supports(Tab::Extracted) {
            return Tab::Extracted;
        }
        match self.kind {
            Sniffed::Picture => Tab::Application,
            Sniffed::Text => Tab::Text,
            Sniffed::Binary => Tab::Hex,
            Sniffed::Empty => Tab::Metadata,
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, prefs: &DisplayPrefs) {
        self.load_info(ui.ctx(), prefs);
        ui.horizontal(|ui| {
            for tab in Tab::ALL {
                let enabled = self.supports(tab);
                let r = ui.add_enabled(
                    enabled,
                    egui::Button::selectable(self.tab == tab, tab.label()),
                );
                if r.clicked() {
                    self.tab = tab;
                    self.pinned = Some(tab);
                }
            }
            if let Some(c) = &self.content {
                ui.separator();
                ui.label(egui::RichText::new(&c.row.path).monospace().weak());
            }
        });
        ui.separator();
        let Some(content) = self.content.clone() else {
            ui.centered_and_justified(|ui| ui.weak("Select a file to view its contents"));
            return;
        };
        if let Some(Err(e)) = &self.source {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Cannot read content: {e}"),
            );
            return;
        }
        let src = self.source.as_ref().and_then(|s| s.as_ref().ok()).cloned();
        match (self.tab, src) {
            (Tab::Metadata, _) => {
                let info = self
                    .info
                    .as_ref()
                    .and_then(|t| t.peek())
                    .and_then(Option::as_ref);
                metadata::ui(ui, &content, self.fs.as_ref(), prefs, info);
            }
            (Tab::Hex, Some(src)) => self.hex.ui(ui, &src),
            (Tab::Strings, Some(src)) => self.strings.ui(ui, &src),
            (Tab::Extracted, _) => {
                let found = self
                    .info
                    .as_ref()
                    .and_then(|t| t.peek())
                    .and_then(Option::as_ref)
                    .map(|i| (Arc::clone(&i.case), Arc::clone(&i.record)));
                let file = content
                    .case_file()
                    .map(|(c, f)| (Arc::clone(c), Arc::new(f.clone())))
                    .or(found);
                match file {
                    Some((case, file)) => self.extracted.ui(ui, &case, &file, self.query.as_ref()),
                    None => {
                        ui.weak("No text available for this file.");
                    }
                }
            }
            (Tab::Text, Some(src)) => self.text.ui(ui, &src),
            (Tab::Application, Some(src)) => self.image.ui(ui, &src, &content.row.path),
            _ => {
                ui.weak("No content");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b""), Sniffed::Empty);
        assert_eq!(sniff(b"hello world\n"), Sniffed::Text);
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
            Sniffed::Picture
        );
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]), Sniffed::Picture);
        assert_eq!(sniff(&[0, 1, 2, 3, 4, 5]), Sniffed::Binary);
        assert_eq!(sniff("ääkköset ovat UTF-8:aa".as_bytes()), Sniffed::Text);
    }
}
