//! Content viewers (the lower pane, and pop-out windows).
//!
//! As in Autopsy, several viewers are offered as tabs and the most useful
//! one for the selected file is chosen automatically. The user's explicit
//! choice is kept while they browse files of the same kind.

mod hex;
mod image_view;
mod metadata;
mod strings;
mod text;

use pm_core::SharedSource;

use crate::format::DisplayPrefs;
use crate::model::Content;

pub(crate) use hex::HexView;
pub(crate) use image_view::ImageView;
pub(crate) use strings::StringsView;
pub(crate) use text::TextView;

/// Viewer tabs, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Tab {
    Hex,
    Strings,
    Text,
    Application,
    Metadata,
}

impl Tab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Hex,
        Self::Strings,
        Self::Text,
        Self::Application,
        Self::Metadata,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Hex => "Hex",
            Self::Strings => "Strings",
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

/// State of one viewer panel (main window or a pop-out).
#[derive(Debug)]
pub(crate) struct ViewerPanel {
    content: Option<Content>,
    source: Option<Result<SharedSource, String>>,
    kind: Sniffed,
    tab: Tab,
    /// Tab explicitly chosen by the user (overrides auto-selection).
    pinned: Option<Tab>,
    hex: HexView,
    strings: StringsView,
    text: TextView,
    image: ImageView,
}

impl Default for ViewerPanel {
    fn default() -> Self {
        Self {
            content: None,
            source: None,
            kind: Sniffed::Empty,
            tab: Tab::Hex,
            pinned: None,
            hex: HexView::default(),
            strings: StringsView::default(),
            text: TextView::default(),
            image: ImageView::default(),
        }
    }
}

impl ViewerPanel {
    pub(crate) fn content(&self) -> Option<&Content> {
        self.content.as_ref()
    }

    /// Shows `content` (no-op if already shown).
    pub(crate) fn set_content(&mut self, content: Option<Content>) {
        if self.content == content {
            return;
        }
        self.hex = HexView::default();
        self.strings = StringsView::default();
        self.text = TextView::default();
        self.image = ImageView::default();
        self.source = content
            .as_ref()
            .filter(|c| !c.row.is_dir())
            .map(Content::source);
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
        self.tab = self
            .pinned
            .filter(|t| self.supports(*t))
            .unwrap_or_else(|| self.preferred());
    }

    fn supports(&self, tab: Tab) -> bool {
        match tab {
            Tab::Metadata => self.content.is_some(),
            Tab::Application => self.kind == Sniffed::Picture,
            Tab::Hex | Tab::Strings | Tab::Text => {
                matches!(self.source, Some(Ok(_))) && self.kind != Sniffed::Empty
            }
        }
    }

    /// Autopsy-style preferred viewer: application viewer for pictures,
    /// text for text, otherwise hex; metadata for directories.
    fn preferred(&self) -> Tab {
        match self.kind {
            Sniffed::Picture => Tab::Application,
            Sniffed::Text => Tab::Text,
            Sniffed::Binary => Tab::Hex,
            Sniffed::Empty => Tab::Metadata,
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, prefs: &DisplayPrefs) {
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
            (Tab::Metadata, _) => metadata::ui(ui, &content, prefs),
            (Tab::Hex, Some(src)) => self.hex.ui(ui, &src),
            (Tab::Strings, Some(src)) => self.strings.ui(ui, &src),
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
