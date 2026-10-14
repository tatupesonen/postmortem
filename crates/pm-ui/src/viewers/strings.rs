//! Strings viewer: printable ASCII and UTF-16LE runs, extracted in the
//! background and shown in a virtualised list with offsets.

use pm_core::SharedSource;

use crate::task::Task;

/// Read at most this much of a file for strings (keeps memory bounded).
const MAX_SCAN: u64 = 64 * 1024 * 1024;
const MIN_LEN: usize = 4;

/// A string found at `offset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Found {
    pub offset: u64,
    pub utf16: bool,
    pub text: String,
}

fn printable(b: u8) -> bool {
    (0x20..0x7F).contains(&b) || b == b'\t'
}

/// Extracts ASCII and UTF-16LE strings of at least `min` characters.
pub(super) fn extract(data: &[u8], base: u64, min: usize) -> Vec<Found> {
    let mut out = Vec::new();
    // ASCII
    let mut start = None;
    for (i, &b) in data
        .iter()
        .enumerate()
        .chain(std::iter::once((data.len(), &0u8)))
    {
        match (printable(b), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                if i - s >= min {
                    let text =
                        String::from_utf8_lossy(data.get(s..i).unwrap_or_default()).into_owned();
                    out.push(Found {
                        offset: base + s as u64,
                        utf16: false,
                        text,
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
    // UTF-16LE: printable ASCII code units with a zero high byte, at both alignments.
    for align in 0..2 {
        let mut run: Vec<u16> = Vec::new();
        let mut run_start = 0usize;
        let units = data.get(align..).unwrap_or_default().as_chunks::<2>().0;
        for (k, &[lo, hi]) in units.iter().chain(std::iter::once(&[0u8, 1u8])).enumerate() {
            if hi == 0 && printable(lo) {
                if run.is_empty() {
                    run_start = align + k * 2;
                }
                run.push(u16::from(lo));
            } else {
                if run.len() >= min {
                    out.push(Found {
                        offset: base + run_start as u64,
                        utf16: true,
                        text: String::from_utf16_lossy(&run),
                    });
                }
                run.clear();
            }
        }
    }
    out.sort_by_key(|f| f.offset);
    out
}

/// (strings, input truncated)
type Extracted = (Vec<Found>, bool);

#[derive(Debug, Default)]
pub(crate) struct StringsView {
    task: Option<Task<Result<Extracted, String>>>,
    filter: String,
}

impl StringsView {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, src: &SharedSource) {
        let task = self.task.get_or_insert_with(|| {
            let src = src.clone();
            Task::spawn(ui.ctx(), move || {
                let len = src.size().min(MAX_SCAN);
                let data = src
                    .read_vec_at(0, len as usize)
                    .map_err(|e| e.to_string())?;
                Ok((extract(&data, 0, MIN_LEN), src.size() > MAX_SCAN))
            })
        });
        let Some(result) = task.poll() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Extracting strings…");
            });
            return;
        };
        let (found, truncated) = match result {
            Ok(r) => r,
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
                return;
            }
        };
        ui.horizontal(|ui| {
            ui.label(format!(
                "{} strings (min. {MIN_LEN} chars, ASCII + UTF-16LE)",
                crate::format::grouped(found.len() as u64)
            ));
            if *truncated {
                ui.weak(format!(
                    "— first {} scanned",
                    pm_core::units::format_size(MAX_SCAN)
                ));
            }
            ui.separator();
            ui.label("Filter:");
            ui.text_edit_singleline(&mut self.filter);
        });
        let needle = self.filter.to_lowercase();
        let shown: Vec<&Found> = if needle.is_empty() {
            found.iter().collect()
        } else {
            found
                .iter()
                .filter(|f| f.text.to_lowercase().contains(&needle))
                .collect()
        };
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        egui::ScrollArea::both().auto_shrink(false).show_rows(
            ui,
            row_h,
            shown.len(),
            |ui, range| {
                for f in shown.get(range).unwrap_or_default() {
                    let tag = if f.utf16 { "U" } else { "A" };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{:012X} {tag}  {}", f.offset, f.text))
                                .monospace(),
                        )
                        .extend(),
                    );
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_utf16() {
        let mut data = b"\x00\x01hello world\x00ab\xFF".to_vec();
        data.extend("WIDE".encode_utf16().flat_map(u16::to_le_bytes));
        data.extend([0xFF, 0xFF]);
        let f = extract(&data, 100, 4);
        let texts: Vec<_> = f
            .iter()
            .map(|f| (f.offset, f.utf16, f.text.as_str()))
            .collect();
        assert_eq!(
            texts,
            vec![(102, false, "hello world"), (117, true, "WIDE")]
        );
    }

    #[test]
    fn string_at_end_of_buffer() {
        assert_eq!(extract(b"tail", 0, 4).len(), 1);
        assert!(extract(b"abc", 0, 4).is_empty());
    }
}
