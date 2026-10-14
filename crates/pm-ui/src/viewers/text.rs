//! Plain-text viewer with BOM-based encoding detection.

use pm_core::SharedSource;

use crate::task::Task;

const MAX_TEXT: u64 = 8 * 1024 * 1024;

/// Decodes text: UTF-8/UTF-16 BOMs honoured, otherwise lossy UTF-8.
pub(super) fn decode(bytes: &[u8]) -> (String, &'static str) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(rest).into_owned(), "UTF-8 (BOM)");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_le_bytes(c))
            .collect();
        return (String::from_utf16_lossy(&units), "UTF-16LE");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c))
            .collect();
        return (String::from_utf16_lossy(&units), "UTF-16BE");
    }
    (String::from_utf8_lossy(bytes).into_owned(), "UTF-8")
}

/// (text, encoding name, truncated)
type Decoded = (String, &'static str, bool);

#[derive(Debug, Default)]
pub(crate) struct TextView {
    task: Option<Task<Result<Decoded, String>>>,
    wrap: bool,
}

impl TextView {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, src: &SharedSource) {
        let task = self.task.get_or_insert_with(|| {
            let src = src.clone();
            Task::spawn(ui.ctx(), move || {
                let data = src
                    .read_vec_at(0, src.size().min(MAX_TEXT) as usize)
                    .map_err(|e| e.to_string())?;
                let (text, enc) = decode(&data);
                Ok((text, enc, src.size() > MAX_TEXT))
            })
        });
        let Some(result) = task.poll() else {
            ui.spinner();
            return;
        };
        match result {
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e.as_str());
            }
            Ok((text, enc, truncated)) => {
                ui.horizontal(|ui| {
                    ui.label(format!("Encoding: {enc}"));
                    if *truncated {
                        ui.weak(format!(
                            "— first {} shown",
                            pm_core::units::format_size(MAX_TEXT)
                        ));
                    }
                    ui.checkbox(&mut self.wrap, "Wrap lines");
                });
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    let mut view: &str = text;
                    let mut edit = egui::TextEdit::multiline(&mut view)
                        .code_editor()
                        .desired_width(f32::INFINITY);
                    if !self.wrap {
                        edit = edit.desired_width(f32::INFINITY).clip_text(false);
                    }
                    ui.add(edit);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding() {
        assert_eq!(decode(b"plain"), ("plain".into(), "UTF-8"));
        assert_eq!(decode(b"\xEF\xBB\xBFbom"), ("bom".into(), "UTF-8 (BOM)"));
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("hi".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(decode(&le), ("hi".into(), "UTF-16LE"));
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain("hi".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode(&be), ("hi".into(), "UTF-16BE"));
    }
}
