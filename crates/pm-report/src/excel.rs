//! Autopsy's Excel report (`ExcelReport`): a Summary sheet, then one sheet
//! per data type with a bold header row; sets (keyword lists, hash sets)
//! and set elements (keywords) become bold rows above their tables.

use std::path::{Path, PathBuf};

use pm_case::Case;
use rust_xlsxwriter::{Format, Workbook, Worksheet};

use crate::ReportError;
use crate::table::TableReportModule;

/// Excel's sheet name limit.
const MAX_SHEET_NAME: usize = 31;

pub struct ExcelReport {
    path: PathBuf,
    book: Workbook,
    sheet: Option<Worksheet>,
    row: u32,
    names: Vec<String>,
    case_lines: Vec<(String, String)>,
}

impl std::fmt::Debug for ExcelReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExcelReport")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[allow(clippy::needless_pass_by_value, reason = "used with map_err")]
fn xl(e: rust_xlsxwriter::XlsxError) -> ReportError {
    ReportError::Excel(e.to_string())
}

/// A unique, valid sheet name (`[]:*?/\` are not allowed).
fn sheet_name(title: &str, taken: &[String]) -> String {
    let clean: String = title
        .chars()
        .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c })
        .take(MAX_SHEET_NAME)
        .collect();
    let mut name = clean.clone();
    let mut n = 1;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
        n += 1;
        let suffix = format!(" ({n})");
        let keep = MAX_SHEET_NAME.saturating_sub(suffix.len());
        name = format!("{}{suffix}", clean.chars().take(keep).collect::<String>());
    }
    name
}

impl ExcelReport {
    pub fn new(dir: &Path, case: &Case) -> Self {
        let d = case.details();
        let sources = case.db().data_sources().map_or(0, |s| s.len());
        Self {
            path: dir.join("Excel.xlsx"),
            book: Workbook::new(),
            sheet: None,
            row: 0,
            names: Vec::new(),
            case_lines: vec![
                ("Case Name:".into(), d.name.clone()),
                ("Case Number:".into(), d.number.clone()),
                ("Examiner:".into(), d.examiner.name.clone()),
                ("Number of Data Sources:".into(), sources.to_string()),
            ],
        }
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn write_row(&mut self, cells: &[String], bold: bool) -> Result<(), ReportError> {
        let row = self.row;
        let Some(sheet) = self.sheet.as_mut() else {
            return Ok(());
        };
        let fmt = Format::new().set_bold();
        for (i, c) in cells.iter().enumerate() {
            let col = u16::try_from(i).unwrap_or(u16::MAX);
            if bold {
                sheet
                    .write_string_with_format(row, col, c, &fmt)
                    .map_err(xl)?;
            } else {
                sheet.write_string(row, col, c).map_err(xl)?;
            }
        }
        self.row += 1;
        Ok(())
    }
}

impl TableReportModule for ExcelReport {
    fn start_report(&mut self) -> Result<(), ReportError> {
        let mut summary = Worksheet::new();
        summary.set_name("Summary").map_err(xl)?;
        let bold = Format::new().set_bold();
        summary
            .write_string_with_format(0, 0, "Summary", &bold)
            .map_err(xl)?;
        for (i, (k, v)) in self.case_lines.iter().enumerate() {
            let r = u32::try_from(i + 2).unwrap_or(u32::MAX);
            summary.write_string(r, 0, k).map_err(xl)?;
            summary.write_string(r, 1, v).map_err(xl)?;
        }
        summary.set_column_width(0, 24).map_err(xl)?;
        self.book.push_worksheet(summary);
        self.names.push("Summary".into());
        Ok(())
    }

    fn end_report(&mut self) -> Result<(), ReportError> {
        self.book.save(&self.path).map_err(xl)
    }

    fn start_data_type(&mut self, title: &str, description: &str) -> Result<(), ReportError> {
        let name = sheet_name(title, &self.names);
        let mut sheet = Worksheet::new();
        sheet.set_name(&name).map_err(xl)?;
        self.names.push(name);
        self.sheet = Some(sheet);
        self.row = 0;
        self.write_row(&[title.to_owned()], true)?;
        if !description.is_empty() {
            self.write_row(&[description.to_owned()], false)?;
        }
        self.row += 1;
        Ok(())
    }

    fn end_data_type(&mut self) -> Result<(), ReportError> {
        if let Some(mut sheet) = self.sheet.take() {
            sheet.autofit();
            self.book.push_worksheet(sheet);
        }
        Ok(())
    }

    fn add_set_index(&mut self, _sets: &[String]) -> Result<(), ReportError> {
        Ok(())
    }

    fn start_set(&mut self, name: &str) -> Result<(), ReportError> {
        self.write_row(&[name.to_owned()], true)
    }

    fn end_set(&mut self) -> Result<(), ReportError> {
        self.row += 1;
        Ok(())
    }

    fn add_set_element(&mut self, name: &str) -> Result<(), ReportError> {
        self.write_row(&[name.to_owned()], true)
    }

    fn start_table(&mut self, headers: &[String]) -> Result<(), ReportError> {
        self.write_row(headers, true)
    }

    fn add_row(&mut self, cells: &[String]) -> Result<(), ReportError> {
        self.write_row(cells, false)
    }

    fn end_table(&mut self) -> Result<(), ReportError> {
        self.row += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_names_are_valid_and_unique() {
        let taken = vec!["Web History".to_owned()];
        assert_eq!(sheet_name("Web History", &taken), "Web History (2)");
        assert_eq!(sheet_name("a/b:c", &[]), "a_b_c");
        let long = "Extension Mismatch Detected And More Words";
        assert_eq!(sheet_name(long, &[]).chars().count(), MAX_SHEET_NAME);
    }
}
