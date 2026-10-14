//! Virtualised hex viewer: only visible rows are read and laid out, so
//! multi-terabyte sources scroll as smoothly as small files.

use std::fmt::Write as _;

use pm_core::SharedSource;

const BYTES_PER_ROW: u64 = 16;

#[derive(Debug, Default)]
pub(crate) struct HexView {
    goto: String,
    scroll_to: Option<u64>,
    /// Last read window: (offset, bytes) — avoids re-reading every frame.
    cache: Option<(u64, Vec<u8>)>,
}

/// Formats one row: `offset  hex bytes  |ascii|`.
pub(crate) fn format_row(offset: u64, bytes: &[u8]) -> String {
    let mut s = format!("{offset:016X}  ");
    for i in 0..BYTES_PER_ROW as usize {
        match bytes.get(i) {
            Some(b) => {
                let _ = write!(s, "{b:02X} ");
            }
            None => s.push_str("   "),
        }
        if i == 7 {
            s.push(' ');
        }
    }
    s.push(' ');
    s.extend(bytes.iter().map(|&b| {
        if (0x20..0x7F).contains(&b) {
            char::from(b)
        } else {
            '.'
        }
    }));
    s
}

/// Parses a go-to offset: decimal, `0x` hex, or plain hex digits with a-f.
pub(crate) fn parse_offset(s: &str) -> Option<u64> {
    let s = s.trim().replace(['_', ' '], "");
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16).ok();
    }
    s.parse().ok().or_else(|| u64::from_str_radix(&s, 16).ok())
}

impl HexView {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, src: &SharedSource) {
        let size = src.size();
        ui.horizontal(|ui| {
            ui.label(format!("{} bytes", crate::format::grouped(size)));
            ui.separator();
            ui.label("Go to offset:");
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.goto)
                    .desired_width(140.0)
                    .hint_text("0x1000"),
            );
            if r.lost_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some(off) = parse_offset(&self.goto)
            {
                self.scroll_to = Some(off.min(size.saturating_sub(1)));
            }
        });
        let rows = size.div_ceil(BYTES_PER_ROW) as usize;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let mut area = egui::ScrollArea::both().auto_shrink(false);
        if let Some(off) = self.scroll_to.take() {
            let spacing = ui.spacing().item_spacing.y;
            area = area.vertical_scroll_offset((off / BYTES_PER_ROW) as f32 * (row_h + spacing));
        }
        area.show_rows(ui, row_h, rows, |ui, range| {
            let start = range.start as u64 * BYTES_PER_ROW;
            let len = (range.len() as u64 * BYTES_PER_ROW).min(size.saturating_sub(start)) as usize;
            let cached = self
                .cache
                .as_ref()
                .is_some_and(|(o, b)| *o == start && b.len() == len);
            if !cached {
                let mut buf = vec![0u8; len];
                let n = src.read_at(start, &mut buf).unwrap_or(0);
                buf.truncate(n);
                self.cache = Some((start, buf));
            }
            let bytes = self
                .cache
                .as_ref()
                .map(|(_, b)| b.as_slice())
                .unwrap_or_default();
            for (i, chunk) in bytes.chunks(BYTES_PER_ROW as usize).enumerate() {
                let line = format_row(start + i as u64 * BYTES_PER_ROW, chunk);
                ui.add(egui::Label::new(egui::RichText::new(line).monospace()).extend());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_format() {
        let row = format_row(0x10, b"ABCDEFGHIJKLMNO\x00");
        assert_eq!(
            row,
            "0000000000000010  41 42 43 44 45 46 47 48  49 4A 4B 4C 4D 4E 4F 00  ABCDEFGHIJKLMNO."
        );
        let short = format_row(0, b"hi");
        assert!(short.ends_with("  hi"));
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("0x1000"), Some(4096));
        assert_eq!(parse_offset("4096"), Some(4096));
        assert_eq!(parse_offset("ff"), Some(255));
        assert_eq!(parse_offset("1_000"), Some(1000));
        assert_eq!(parse_offset("zz"), None);
    }
}
