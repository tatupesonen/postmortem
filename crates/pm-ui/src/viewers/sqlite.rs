//! Autopsy's SQLite viewer (`SQLiteViewer`): the Application tab for
//! SQLite databases. The database is copied to a private temp folder with
//! its `-wal` and `-journal` companions (so uncommitted transactions show,
//! which Autopsy misses) and opened through the hardened evidence opener
//! (`pm_browser::sqlite::open`: query only, no triggers or views from the
//! file). Tables are listed in schema order and read 100 rows a page;
//! a column's header menu shows its numbers as dates ("Display as" in
//! Autopsy, which only knows milliseconds since 1970); Export to CSV writes
//! the whole table.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use egui_extras::{Column, TableBuilder};
use pm_core::io::{ReadAt, SharedSource};
use rusqlite::Connection;
use rusqlite::types::ValueRef;

use crate::format::DisplayPrefs;
use crate::model::Content;
use crate::task::{Keyed, Task};

/// Rows per page, as in Autopsy.
pub(crate) const ROWS_PER_PAGE: u64 = 100;

/// Longest text shown in a cell; the rest is cut (CSV export has it all).
const CELL_MAX: usize = 1000;

/// Whether `head` starts like an SQLite database.
pub(crate) fn is_sqlite(head: &[u8]) -> bool {
    head.starts_with(b"SQLite format 3\0")
}

/// An evidence database opened from a private temp copy.
pub(crate) struct Db {
    /// What it was opened for (the shown file).
    key: String,
    conn: Mutex<Connection>,
    pub(crate) tables: Vec<String>,
    /// Removed after the connection is closed (fields drop in order).
    _dir: tempfile::TempDir,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("key", &self.key)
            .field("tables", &self.tables)
            .finish_non_exhaustive()
    }
}

/// Copies `main` and its companions (`("-wal", source)`, …) to a temp
/// folder and opens the copy.
pub(crate) fn open(
    key: String,
    main: &dyn ReadAt,
    companions: &[(&'static str, SharedSource)],
) -> Result<Db, String> {
    let dir = tempfile::Builder::new()
        .prefix("pm-sqlite-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let path = dir.path().join("db.sqlite");
    crate::model::copy_out(main, &path)?;
    for (suffix, src) in companions {
        crate::model::copy_out(src.as_ref(), &dir.path().join(format!("db.sqlite{suffix}")))?;
    }
    let conn = pm_browser::sqlite::open(&path)
        .map_err(|e| format!("The database tables in the file could not be read: {e}"))?;
    let tables = {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .map_err(|e| e.to_string())?;
        st.query_map([], |r| r.get::<_, String>(0))
            .and_then(Iterator::collect::<rusqlite::Result<Vec<_>>>)
            .map_err(|e| e.to_string())?
    };
    Ok(Db {
        key,
        conn: Mutex::new(conn),
        tables,
        _dir: dir,
    })
}

/// The `-wal` and `-journal` files next to a case file.
fn companions(content: &Content) -> Vec<(&'static str, SharedSource)> {
    let Some((case, f)) = content.case_file() else {
        return Vec::new();
    };
    let Some(parent) = f.parent_id else {
        return Vec::new();
    };
    ["-wal", "-journal"]
        .into_iter()
        .filter_map(|suffix| {
            let rec = case
                .db()
                .child_named(parent, &format!("{}{suffix}", f.name))
                .ok()??;
            Some((suffix, case.content(&rec).ok()?))
        })
        .collect()
}

/// A cell as read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    fn from_ref(v: ValueRef<'_>) -> Self {
        match v {
            ValueRef::Null => Self::Null,
            ValueRef::Integer(i) => Self::Int(i),
            ValueRef::Real(f) => Self::Real(f),
            ValueRef::Text(t) => Self::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Self::Blob(b.to_vec()),
        }
    }

    /// The CSV text: NULL empty, BLOBs in hex.
    fn csv(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Int(i) => i.to_string(),
            Self::Real(f) => f.to_string(),
            Self::Text(t) => t.clone(),
            Self::Blob(b) => pm_core::decode::to_hex(b),
        }
    }

    /// The number the cell holds, for showing it as a date.
    fn number(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            #[expect(clippy::cast_possible_truncation, reason = "dates are whole")]
            Self::Real(f) if f.is_finite() => Some(*f as i64),
            Self::Text(t) => t.trim().parse().ok(),
            _ => None,
        }
    }
}

/// How a column's numbers are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DateAs {
    #[default]
    Raw,
    /// Seconds since 1970.
    UnixSeconds,
    /// Milliseconds since 1970 (Autopsy's "Date").
    UnixMillis,
    /// Microseconds since 1601 (Chrome/WebKit).
    WebKit,
    /// Seconds since 2001 (Apple Cocoa / Mac absolute time).
    MacAbsolute,
}

impl DateAs {
    const ALL: [Self; 5] = [
        Self::Raw,
        Self::UnixSeconds,
        Self::UnixMillis,
        Self::WebKit,
        Self::MacAbsolute,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Raw => "Raw Data",
            Self::UnixSeconds => "Date (Unix seconds)",
            Self::UnixMillis => "Date (Unix milliseconds)",
            Self::WebKit => "Date (WebKit microseconds since 1601)",
            Self::MacAbsolute => "Date (Mac absolute seconds since 2001)",
        }
    }

    /// The time `n` stands for; `None` for zero or out of range.
    pub(crate) fn timestamp(self, n: i64) -> Option<jiff::Timestamp> {
        /// Seconds from 1601-01-01 to 1970-01-01.
        const WEBKIT_EPOCH: i64 = 11_644_473_600;
        /// Seconds from 1970-01-01 to 2001-01-01.
        const MAC_EPOCH: i64 = 978_307_200;
        if n == 0 {
            return None;
        }
        match self {
            Self::Raw => None,
            Self::UnixSeconds => jiff::Timestamp::from_second(n).ok(),
            Self::UnixMillis => jiff::Timestamp::from_millisecond(n).ok(),
            Self::WebKit => {
                jiff::Timestamp::from_microsecond(n.checked_sub(WEBKIT_EPOCH * 1_000_000)?).ok()
            }
            Self::MacAbsolute => jiff::Timestamp::from_second(n.checked_add(MAC_EPOCH)?).ok(),
        }
    }
}

/// One page of a table.
#[derive(Debug)]
pub(crate) struct Page {
    key: (String, String, u64),
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Vec<Cell>>,
    pub(crate) total: u64,
}

/// `name` as an SQL identifier.
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Reads page `page` (from 0) of `table`.
pub(crate) fn read_page(db: &Db, table: &str, page: u64) -> Result<Page, String> {
    let conn = db.conn.lock().map_err(|_| "database lock poisoned".to_owned())?;
    let err = |e: rusqlite::Error| format!("Error getting rows for table: {table} ({e})");
    let total: i64 = conn
        .query_row(&format!("SELECT count(*) FROM {}", quote(table)), [], |r| r.get(0))
        .map_err(err)?;
    let mut st = conn
        .prepare(&format!(
            "SELECT * FROM {} LIMIT {ROWS_PER_PAGE} OFFSET {}",
            quote(table),
            page.saturating_mul(ROWS_PER_PAGE)
        ))
        .map_err(err)?;
    let columns: Vec<String> = st.column_names().into_iter().map(str::to_owned).collect();
    let n = columns.len();
    let mut rows = Vec::new();
    let mut q = st.query([]).map_err(err)?;
    while let Some(r) = q.next().map_err(err)? {
        rows.push(
            (0..n)
                .map(|i| r.get_ref(i).map_or(Cell::Null, Cell::from_ref))
                .collect(),
        );
    }
    Ok(Page {
        key: (db.key.clone(), table.to_owned(), page),
        columns,
        rows,
        total: u64::try_from(total).unwrap_or(0),
    })
}

/// Writes all of `table` to `dest` as CSV (a header row of column names,
/// every value quoted); returns the number of rows.
pub(crate) fn export_csv(db: &Db, table: &str, dest: &Path) -> Result<u64, String> {
    use std::io::Write as _;
    let conn = db.conn.lock().map_err(|_| "database lock poisoned".to_owned())?;
    let err = |e: rusqlite::Error| format!("Failed to export table content to csv file: {e}");
    let mut st = conn
        .prepare(&format!("SELECT * FROM {}", quote(table)))
        .map_err(err)?;
    let columns: Vec<String> = st.column_names().into_iter().map(str::to_owned).collect();
    let file = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .map(|c| crate::table::csv_field(c))
            .collect::<Vec<_>>()
            .join(",")
    };
    writeln!(out, "{}", line(columns.clone())).map_err(io)?;
    let mut q = st.query([]).map_err(err)?;
    let mut n = 0;
    while let Some(r) = q.next().map_err(err)? {
        let cells = (0..columns.len())
            .map(|i| r.get_ref(i).map_or(Cell::Null, Cell::from_ref).csv())
            .collect();
        writeln!(out, "{}", line(cells)).map_err(io)?;
        n += 1;
    }
    out.flush().map_err(io)?;
    Ok(n)
}

/// The viewer's state.
#[derive(Debug, Default)]
pub(crate) struct SqliteView {
    db: Keyed<String, Db>,
    table: Option<String>,
    page: u64,
    pages: Keyed<(String, String, u64), Page>,
    /// Per (table, column) date display.
    date_as: HashMap<(String, String), DateAs>,
    export: Option<Task<Result<(u64, PathBuf), String>>>,
    status: Option<String>,
}

impl SqliteView {
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        content: &Content,
        src: &SharedSource,
        prefs: &DisplayPrefs,
    ) {
        let key = format!("{:?}:{}", content.row.file_id(), content.row.path);
        let (db_key, shown, source) = (key.clone(), content.clone(), Arc::clone(src));
        let db = self
            .db
            .get(ui.ctx(), &key, move || {
                open(db_key, source.as_ref(), &companions(&shown))
            })
            .filter(|d| d.key == key);
        let Some(db) = db else {
            match self.db.error() {
                Some(e) if !self.db.loading() => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Opening database…");
                    });
                }
            }
            return;
        };
        if db.tables.is_empty() {
            ui.weak("No tables found");
            return;
        }
        if !self.table.as_ref().is_some_and(|t| db.tables.contains(t)) {
            self.table = db.tables.first().cloned();
            self.page = 0;
        }
        let table = self.table.clone().unwrap_or_default();
        let page_key = (key, table.clone(), self.page);
        let (reader, name, number) = (Arc::clone(&db), table.clone(), self.page);
        let page = self
            .pages
            .get(ui.ctx(), &page_key, move || read_page(&reader, &name, number))
            .filter(|pg| pg.key.0 == page_key.0 && pg.key.1 == page_key.1);
        self.poll_export();
        let total = page.as_ref().map_or(0, |p| p.total);
        let pages = total.div_ceil(ROWS_PER_PAGE).max(1);
        ui.horizontal(|ui| {
            ui.label("Table");
            egui::ComboBox::from_id_salt("sqlite-table")
                .selected_text(&table)
                .show_ui(ui, |ui| {
                    for t in &db.tables {
                        if ui.selectable_label(*t == table, t).clicked() {
                            self.table = Some(t.clone());
                            self.page = 0;
                        }
                    }
                });
            ui.label(format!("{total} entries"));
            ui.separator();
            ui.label("Page");
            if ui
                .add_enabled(self.page > 0, egui::Button::new("⏴"))
                .on_hover_text("Previous page")
                .clicked()
            {
                self.page -= 1;
            }
            ui.label(format!("{} of {pages}", self.page + 1));
            if ui
                .add_enabled(self.page + 1 < pages, egui::Button::new("⏵"))
                .on_hover_text("Next page")
                .clicked()
            {
                self.page += 1;
            }
            ui.separator();
            let busy = self.export.is_some();
            if ui
                .add_enabled(total > 0 && !busy, egui::Button::new("Export to CSV…"))
                .clicked()
                && let Some(dest) = rfd::FileDialog::new()
                    .set_title("Export to csv file")
                    .set_file_name(format!("{table}.csv"))
                    .add_filter("CSV", &["csv"])
                    .save_file()
            {
                let (d, t) = (Arc::clone(&db), table.clone());
                self.export = Some(Task::spawn(ui.ctx(), move || {
                    export_csv(&d, &t, &dest).map(|n| (n, dest))
                }));
            }
            if busy {
                ui.spinner();
            }
            if let Some(s) = &self.status {
                ui.weak(s);
            }
        });
        if let Some(e) = self.pages.error() {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        let Some(page) = page else {
            ui.spinner();
            return;
        };
        self.table_ui(ui, &table, &page, prefs);
    }

    fn poll_export(&mut self) {
        if let Some(t) = &mut self.export
            && let Some(r) = t.poll()
        {
            self.status = Some(match r {
                Ok((n, p)) => format!("Exported {n} rows to {}", p.display()),
                Err(e) => e.clone(),
            });
            self.export = None;
        }
    }

    fn table_ui(&mut self, ui: &mut egui::Ui, table: &str, page: &Page, prefs: &DisplayPrefs) {
        let text_h = egui::TextStyle::Body.resolve(ui.style()).size + 6.0;
        let modes: Vec<DateAs> = page
            .columns
            .iter()
            .map(|c| {
                self.date_as
                    .get(&(table.to_owned(), c.clone()))
                    .copied()
                    .unwrap_or_default()
            })
            .collect();
        crate::sortable::hscroll(ui, "sqlite-h", |ui| {
            ui.push_id(("sqlite-table", table), |ui| {
                let mut builder = TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                for i in 0..page.columns.len() {
                    builder = if i + 1 == page.columns.len() {
                        builder.column(Column::remainder().at_least(80.0))
                    } else {
                        builder.column(Column::initial(140.0).at_least(40.0).clip(true))
                    };
                }
                builder
                    .header(text_h + 2.0, |mut h| {
                        for (col, mode) in page.columns.iter().zip(&modes) {
                            h.col(|ui| {
                                let r = ui
                                    .add(egui::Label::new(egui::RichText::new(col).strong()).sense(egui::Sense::click()))
                                    .on_hover_text("Right-click: Display as");
                                r.context_menu(|ui| {
                                    ui.label("Display as");
                                    for m in DateAs::ALL {
                                        if ui.radio(*mode == m, m.label()).clicked() {
                                            self.date_as.insert((table.to_owned(), col.clone()), m);
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(text_h, page.rows.len(), |mut row| {
                            let Some(cells) = page.rows.get(row.index()) else {
                                return;
                            };
                            for (cell, mode) in cells.iter().zip(&modes) {
                                row.col(|ui| cell_ui(ui, cell, *mode, prefs));
                            }
                        });
                    });
            });
        });
    }
}

fn cell_ui(ui: &mut egui::Ui, cell: &Cell, mode: DateAs, prefs: &DisplayPrefs) {
    if mode != DateAs::Raw
        && let Some(t) = cell.number().and_then(|n| mode.timestamp(n))
    {
        ui.label(egui::RichText::new(prefs.time(Some(t))).italics());
        return;
    }
    match cell {
        Cell::Null => {
            ui.weak("NULL");
        }
        Cell::Int(i) => {
            ui.label(i.to_string());
        }
        Cell::Real(f) => {
            ui.label(f.to_string());
        }
        Cell::Text(t) => {
            if t.chars().count() > CELL_MAX {
                let cut: String = t.chars().take(CELL_MAX).collect();
                ui.label(cut + "…").on_hover_text("Cut; Export to CSV has the whole value");
            } else {
                ui.label(t);
            }
        }
        Cell::Blob(b) => {
            ui.weak(format!("BLOB ({} bytes)", b.len()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.db");
        let c = Connection::open(&p).unwrap();
        c.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE \"we\"\"ird\" (id INTEGER, name TEXT, data BLOB, t INTEGER);",
        )
        .unwrap();
        for i in 0..250 {
            c.execute(
                "INSERT INTO \"we\"\"ird\" VALUES (?1, ?2, x'00ff', 1600000000)",
                rusqlite::params![i, format!("row \"{i}\", ok")],
            )
            .unwrap();
        }
        // Everything is still in the WAL: the main file alone has no rows.
        let main = std::fs::read(&p).unwrap();
        let wal = std::fs::read(dir.path().join("s.db-wal")).unwrap();
        drop(c);
        (dir, main, wal)
    }

    #[test]
    fn reads_pages_with_the_wal_and_exports_csv() {
        let (dir, main, wal) = sample();
        assert!(is_sqlite(&main));
        let src = |b: Vec<u8>| -> SharedSource { Arc::new(pm_core::io::MemSource::new(b)) };
        let db = open("k".into(), src(main.clone()).as_ref(), &[("-wal", src(wal))]).unwrap();
        assert_eq!(db.tables, ["we\"ird"]);
        let p = read_page(&db, "we\"ird", 2).unwrap();
        assert_eq!(p.total, 250);
        assert_eq!(p.columns, ["id", "name", "data", "t"]);
        assert_eq!(p.rows.len(), 50, "the last page");
        assert_eq!(p.rows[0][0], Cell::Int(200));
        assert_eq!(p.rows[0][2], Cell::Blob(vec![0, 0xff]));

        let out = dir.path().join("out.csv");
        assert_eq!(export_csv(&db, "we\"ird", &out).unwrap(), 250);
        let csv = std::fs::read_to_string(&out).unwrap();
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("\"id\",\"name\",\"data\",\"t\""));
        assert_eq!(
            lines.next(),
            Some("\"0\",\"row \"\"0\"\", ok\",\"00ff\",\"1600000000\"")
        );

        // Without its WAL the copy has the table but no rows yet.
        let bare = open("b".into(), src(main).as_ref(), &[]).unwrap();
        assert!(read_page(&bare, "we\"ird", 0).map_or(true, |p| p.total == 0));
    }

    #[test]
    fn dates() {
        let s = |t: Option<jiff::Timestamp>| t.map(|t| t.to_string());
        let want = Some("2020-09-13T12:26:40Z".to_owned());
        assert_eq!(s(DateAs::UnixSeconds.timestamp(1_600_000_000)), want);
        assert_eq!(s(DateAs::UnixMillis.timestamp(1_600_000_000_000)), want);
        assert_eq!(
            s(DateAs::WebKit.timestamp((1_600_000_000 + 11_644_473_600) * 1_000_000)),
            want
        );
        assert_eq!(s(DateAs::MacAbsolute.timestamp(1_600_000_000 - 978_307_200)), want);
        assert_eq!(DateAs::UnixSeconds.timestamp(0), None);
        assert_eq!(DateAs::Raw.timestamp(5), None);
        assert_eq!(Cell::Text(" 42 ".into()).number(), Some(42));
    }
}
