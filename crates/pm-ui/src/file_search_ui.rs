//! The File Search by Attributes window (Autopsy `FileSearchDialog`).
//!
//! Deviations: sizes use SI units (kB = 1000 B, ADR-005) where Autopsy
//! uses binary ones; dates are ISO `YYYY-MM-DD`; the "to" date is
//! inclusive (Autopsy compares against the start of that day).

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::Known;
use pm_casedb::file_search::{DateFilter, DateKinds, FileSearch, SizeOp};

use crate::model::FileRow;

/// Rows a search result shows (Autopsy warns above 10 000).
pub(crate) const MAX_ROWS: usize = 20_000;

const UNITS: [(&str, u64); 5] = [
    ("Byte(s)", 1),
    ("kB", 1_000),
    ("MB", 1_000_000),
    ("GB", 1_000_000_000),
    ("TB", 1_000_000_000_000),
];

/// The form state.
#[derive(Debug)]
pub(crate) struct FileSearchWindow {
    name_on: bool,
    name: String,
    size_on: bool,
    size_op: SizeOp,
    size: String,
    unit: usize,
    date_on: bool,
    from: String,
    to: String,
    zone: String,
    kinds: DateKinds,
    known_on: bool,
    known: [bool; 3],
    md5_on: bool,
    md5: String,
    sha256_on: bool,
    sha256: String,
    mime_on: bool,
    mime: Vec<String>,
    ds_on: bool,
    ds: Vec<i64>,
    deleted: bool,
    error: Option<String>,
}

impl Default for FileSearchWindow {
    fn default() -> Self {
        Self {
            name_on: false,
            name: String::new(),
            size_on: false,
            size_op: SizeOp::Greater,
            size: String::new(),
            unit: 0,
            date_on: false,
            from: String::new(),
            to: String::new(),
            zone: "UTC".into(),
            kinds: DateKinds {
                modified: true,
                changed: true,
                accessed: true,
                created: true,
            },
            known_on: false,
            known: [true, true, true],
            md5_on: false,
            md5: String::new(),
            sha256_on: false,
            sha256: String::new(),
            mime_on: false,
            mime: Vec::new(),
            ds_on: false,
            ds: Vec::new(),
            deleted: false,
            error: None,
        }
    }
}

/// What the window asks the app to do.
#[derive(Debug)]
pub(crate) enum SearchAction {
    None,
    Close,
    Search(Box<FileSearch>),
}

fn parse_hex(s: &str, len: usize, what: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() != len * 2 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{what} must be {} hexadecimal digits.", len * 2));
    }
    (0..len)
        .map(|i| {
            s.get(i * 2..i * 2 + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .ok_or_else(|| format!("{what} is not valid hexadecimal."))
        })
        .collect()
}

/// Start of `date` (`YYYY-MM-DD`) in `zone`, as Unix seconds.
fn day_start(date: &str, zone: &str) -> Result<i64, String> {
    let d: jiff::civil::Date = date
        .trim()
        .parse()
        .map_err(|_| format!("{date:?} is not a date (YYYY-MM-DD)."))?;
    let tz = jiff::tz::TimeZone::get(zone).map_err(|e| e.to_string())?;
    d.to_zoned(tz)
        .map(|z| z.timestamp().as_second())
        .map_err(|e| e.to_string())
}

impl FileSearchWindow {
    /// Autopsy's "Open File Search by Attributes" on a data source: the
    /// data source filter starts enabled with that source selected.
    pub(crate) fn preselect_data_source(&mut self, id: i64) {
        self.ds_on = true;
        self.ds = vec![id];
    }

    /// The search the form describes, or Autopsy's validation message.
    pub(crate) fn search(&self) -> Result<FileSearch, String> {
        let mut s = FileSearch::default();
        if self.name_on {
            if self.name.trim().is_empty() {
                return Err("Must enter something for name search.".into());
            }
            s.name = Some(self.name.trim().to_owned());
        }
        if self.size_on {
            let n: u64 = self
                .size
                .trim()
                .replace([',', ' ', '\u{a0}'], "")
                .parse()
                .map_err(|_| "Size must be a non-negative integer.".to_owned())?;
            let unit = UNITS.get(self.unit).map_or(1, |u| u.1);
            s.size = Some((self.size_op, n.saturating_mul(unit)));
        }
        if self.date_on {
            let k = self.kinds;
            if !(k.modified || k.changed || k.accessed || k.created) {
                return Err("At least one date type checkbox must be selected.".into());
            }
            let from = (!self.from.trim().is_empty())
                .then(|| day_start(&self.from, &self.zone))
                .transpose()?;
            let to = (!self.to.trim().is_empty())
                .then(|| day_start(&self.to, &self.zone).map(|t| t + 86_400))
                .transpose()?;
            if let (Some(f), Some(t)) = (from, to)
                && t <= f
            {
                return Err("The end date should be after the start date.".into());
            }
            s.dates = Some(DateFilter { from, to, kinds: k });
        }
        if self.known_on {
            for (on, k) in self
                .known
                .iter()
                .zip([Known::Unknown, Known::Known, Known::Notable])
            {
                if *on {
                    s.known.push(k);
                }
            }
            if s.known.is_empty() {
                return Err("At least one known status must be selected.".into());
            }
        }
        if self.md5_on {
            s.md5 = Some(parse_hex(&self.md5, 16, "MD5")?);
        }
        if self.sha256_on {
            s.sha256 = Some(parse_hex(&self.sha256, 32, "SHA-256")?);
        }
        if self.mime_on {
            if self.mime.is_empty() {
                return Err("At least one MIME type must be selected.".into());
            }
            s.mime.clone_from(&self.mime);
        }
        if self.ds_on {
            if self.ds.is_empty() {
                return Err("At least one data source must be selected.".into());
            }
            s.ds_ids.clone_from(&self.ds);
        }
        s.deleted = self.deleted;
        if s.is_empty() {
            return Err("At least one filter must be selected.".into());
        }
        Ok(s)
    }

    /// Draws the window; `sources` are `(id, name, time zone)`.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        sources: &[(i64, String, String)],
        mime_types: &[String],
    ) -> SearchAction {
        let mut action = SearchAction::None;
        let mut open = true;
        egui::Window::new("File Search by Attributes")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label("Search for files that match the following criteria:");
                ui.separator();
                ui.checkbox(&mut self.name_on, "Name:");
                ui.add_enabled(self.name_on, egui::TextEdit::singleline(&mut self.name));
                ui.separator();
                ui.checkbox(&mut self.size_on, "Size:");
                ui.add_enabled_ui(self.size_on, |ui| {
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("size-op")
                            .selected_text(match self.size_op {
                                SizeOp::Equal => "equal to",
                                SizeOp::Greater => "greater than",
                                SizeOp::Less => "less than",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.size_op, SizeOp::Equal, "equal to");
                                ui.selectable_value(
                                    &mut self.size_op,
                                    SizeOp::Greater,
                                    "greater than",
                                );
                                ui.selectable_value(&mut self.size_op, SizeOp::Less, "less than");
                            });
                        ui.add(egui::TextEdit::singleline(&mut self.size).desired_width(90.0));
                        egui::ComboBox::from_id_salt("size-unit")
                            .selected_text(UNITS.get(self.unit).map_or("", |u| u.0))
                            .show_ui(ui, |ui| {
                                for (i, (n, _)) in UNITS.iter().enumerate() {
                                    ui.selectable_value(&mut self.unit, i, *n);
                                }
                            });
                    });
                });
                ui.separator();
                ui.checkbox(&mut self.date_on, "Date:");
                ui.add_enabled_ui(self.date_on, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("from");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.from)
                                .hint_text("YYYY-MM-DD")
                                .desired_width(100.0),
                        );
                        ui.label("to");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.to)
                                .hint_text("YYYY-MM-DD")
                                .desired_width(100.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Timezone:");
                        egui::ComboBox::from_id_salt("search-zone")
                            .selected_text(self.zone.clone())
                            .show_ui(ui, |ui| {
                                let mut zones: Vec<String> =
                                    sources.iter().map(|s| s.2.clone()).collect();
                                zones.push("UTC".into());
                                if let Some(local) = jiff::tz::TimeZone::system().iana_name() {
                                    zones.push(local.to_owned());
                                }
                                zones.dedup();
                                for z in zones {
                                    ui.selectable_value(&mut self.zone, z.clone(), z);
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.kinds.modified, "Modified");
                        ui.checkbox(&mut self.kinds.changed, "Changed");
                        ui.checkbox(&mut self.kinds.accessed, "Accessed");
                        ui.checkbox(&mut self.kinds.created, "Created");
                    });
                });
                ui.separator();
                ui.checkbox(&mut self.known_on, "Known Status:");
                ui.add_enabled_ui(self.known_on, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.known[0], "Unknown");
                        ui.checkbox(&mut self.known[1], "Known (NSRL or other)");
                        ui.checkbox(&mut self.known[2], "Notable");
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.md5_on, "MD5:");
                    ui.add_enabled(
                        self.md5_on,
                        egui::TextEdit::singleline(&mut self.md5).desired_width(300.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.sha256_on, "SHA-256:");
                    ui.add_enabled(
                        self.sha256_on,
                        egui::TextEdit::singleline(&mut self.sha256).desired_width(300.0),
                    );
                });
                ui.separator();
                ui.checkbox(&mut self.mime_on, "MIME Type:");
                ui.add_enabled_ui(self.mime_on, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("mime-list")
                        .max_height(90.0)
                        .show(ui, |ui| {
                            if mime_types.is_empty() {
                                ui.weak(
                                    "No MIME types detected yet (run File Type Identification).",
                                );
                            }
                            for m in mime_types {
                                let mut on = self.mime.contains(m);
                                if ui.checkbox(&mut on, m).changed() {
                                    if on {
                                        self.mime.push(m.clone());
                                    } else {
                                        self.mime.retain(|x| x != m);
                                    }
                                }
                            }
                        });
                });
                ui.separator();
                ui.checkbox(&mut self.ds_on, "Data Source:");
                ui.add_enabled_ui(self.ds_on, |ui| {
                    for (id, name, _) in sources {
                        let mut on = self.ds.contains(id);
                        if ui.checkbox(&mut on, name).changed() {
                            if on {
                                self.ds.push(*id);
                            } else {
                                self.ds.retain(|x| x != id);
                            }
                        }
                    }
                    ui.weak("Multiple data sources can be selected");
                });
                ui.separator();
                ui.checkbox(&mut self.deleted, "Deleted");
                if let Some(e) = &self.error {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        format!("Validation Error: {e}"),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Search").clicked() {
                        match self.search() {
                            Ok(s) => {
                                self.error = None;
                                action = SearchAction::Search(Box::new(s));
                            }
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if ui.button("Close").clicked() {
                        action = SearchAction::Close;
                    }
                });
            });
        if !open {
            action = SearchAction::Close;
        }
        action
    }
}

/// Runs a search (in a task): up to [`MAX_ROWS`] annotated rows, and
/// whether more matched.
pub(crate) fn run(case: &Case, s: &FileSearch) -> Result<(Vec<FileRow>, bool), String> {
    let db = case.db();
    let mut records = Vec::new();
    let mut after = 0;
    while records.len() <= MAX_ROWS {
        let page = db
            .search_files(s, after, 5_000)
            .map_err(|e| e.to_string())?;
        let Some(last) = page.last() else { break };
        after = last.id;
        records.extend(page);
    }
    let more = records.len() > MAX_ROWS;
    records.truncate(MAX_ROWS);
    let mut rows: Vec<FileRow> = records.iter().map(crate::views::row_of).collect();
    crate::tags_ui::annotate(case, &mut rows)?;
    Ok((rows, more))
}

/// A finished or running search shown in the result pane.
#[derive(Debug)]
pub(crate) struct SearchResults {
    pub(crate) number: usize,
    pub(crate) rows: crate::task::Cached<(Vec<FileRow>, bool)>,
    pub(crate) search: Arc<FileSearch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_follows_autopsy() {
        let mut w = FileSearchWindow::default();
        assert_eq!(
            w.search().unwrap_err(),
            "At least one filter must be selected."
        );
        w.size_on = true;
        w.size = "1,5".into();
        w.unit = 2;
        assert_eq!(
            w.search().unwrap().size,
            Some((SizeOp::Greater, 15_000_000)),
            "MB is 10^6 bytes"
        );
        w.size = "-3".into();
        assert!(w.search().is_err());
        w.size_on = false;
        w.date_on = true;
        w.kinds = DateKinds::default();
        assert_eq!(
            w.search().unwrap_err(),
            "At least one date type checkbox must be selected."
        );
        w.kinds.modified = true;
        w.from = "2021-05-31".into();
        w.to = "2021-05-30".into();
        assert_eq!(
            w.search().unwrap_err(),
            "The end date should be after the start date."
        );
        w.to = "2021-05-31".into();
        let d = w.search().unwrap().dates.unwrap();
        assert_eq!(
            d.to.unwrap() - d.from.unwrap(),
            86_400,
            "the end day is included"
        );
        w.date_on = false;
        w.md5_on = true;
        w.md5 = "xyz".into();
        assert!(w.search().unwrap_err().contains("hexadecimal"));
        w.md5 = "d41d8cd98f00b204e9800998ecf8427e".into();
        assert_eq!(w.search().unwrap().md5.unwrap().len(), 16);
    }
}
