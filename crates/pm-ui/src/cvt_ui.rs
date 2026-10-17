//! Tools › Communications (Autopsy's Communications Visualization Tool):
//! accounts with relationships, filtered by data source, account type,
//! relationship kind and date range; for the selected account its
//! messages, call logs and contacts, the details of the chosen item, and a
//! graph of the accounts it communicated with.
//!
//! Everything loads in background tasks; tables are virtualised.

use std::collections::BTreeSet;
use std::sync::Arc;

use pm_case::Case;
use pm_casedb::communications::{Account, CommsFilter, RelationshipKind};
use pm_casedb::{ArtifactRecord, AttributeRecord, Value};

use crate::format::DisplayPrefs;
use crate::grid::{Grid, GridData};
use crate::task::Cached;

/// The case's filter choices.
#[derive(Debug, Clone, Default)]
pub(crate) struct Choices {
    pub(crate) data_sources: Vec<(i64, String)>,
    /// `(type id, display name)`.
    pub(crate) account_types: Vec<(i64, String)>,
}

impl Choices {
    pub(crate) fn load(case: &Case) -> Result<Self, String> {
        let db = case.db();
        let err = |e: pm_casedb::DbError| e.to_string();
        Ok(Self {
            data_sources: db
                .data_sources()
                .map_err(err)?
                .into_iter()
                .map(|d| (d.id, d.name))
                .collect(),
            account_types: db
                .account_types_in_use()
                .map_err(err)?
                .into_iter()
                .filter(|t| t.name != "DEVICE")
                .map(|t| (t.id, t.display_name))
                .collect(),
        })
    }
}

/// The accounts table.
#[derive(Debug, Default)]
struct Accounts {
    rows: Vec<(Account, i64)>,
    grid: GridData,
}

fn load_accounts(case: &Case, filter: &CommsFilter, choices: &Choices) -> Result<Accounts, String> {
    let db = case.db();
    let list = db
        .account_device_instances(filter)
        .map_err(|e| e.to_string())?;
    let mut grid = GridData::new(vec!["Account", "Device", "Type", "Items"]);
    let mut rows = Vec::with_capacity(list.len());
    for a in list {
        let ds = choices
            .data_sources
            .iter()
            .find(|d| d.0 == a.ds_id)
            .map_or_else(String::new, |d| d.1.clone());
        let ty = choices
            .account_types
            .iter()
            .find(|t| t.0 == a.account.type_id)
            .map_or_else(|| a.account.type_name.clone(), |t| t.1.clone());
        let n = i64::try_from(a.relationships).unwrap_or(i64::MAX);
        grid.push(vec![
            (a.account.identifier.clone(), None),
            (ds, None),
            (ty, None),
            (crate::format::grouped(a.relationships), Some(n)),
        ]);
        rows.push((a.account, a.ds_id));
    }
    Ok(Accounts { rows, grid })
}

/// One communication of the selected account.
#[derive(Debug, Clone)]
struct Item {
    artifact: ArtifactRecord,
    attrs: Vec<AttributeRecord>,
}

/// What the selected account took part in.
#[derive(Debug, Default)]
struct Selection {
    messages: (Vec<Item>, GridData),
    calls: (Vec<Item>, GridData),
    contacts: (Vec<Item>, GridData),
    related: Vec<(Account, u64)>,
}

/// The most items listed for one account.
const MAX_ITEMS: usize = 20_000;

fn text(attrs: &[AttributeRecord], names: &[&str], prefs: &DisplayPrefs) -> String {
    for n in names {
        if let Some(a) = attrs.iter().find(|a| a.name == *n) {
            return match &a.value {
                Value::DateTime(t) => prefs.time(jiff::Timestamp::from_second(*t).ok()),
                v => v.display(),
            };
        }
    }
    String::new()
}

fn time_key(attrs: &[AttributeRecord], names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|n| match attrs.iter().find(|a| a.name == *n)?.value {
            Value::DateTime(t) => Some(t),
            _ => None,
        })
}

fn load_selection(
    case: &Case,
    account: i64,
    filter: &CommsFilter,
    prefs: &DisplayPrefs,
) -> Result<Selection, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut sel = Selection {
        messages: (
            Vec::new(),
            GridData::new(vec!["From", "To", "Date", "Subject"]),
        ),
        calls: (
            Vec::new(),
            GridData::new(vec!["From", "To", "Date", "Direction", "Duration"]),
        ),
        contacts: (Vec::new(), GridData::new(vec!["Name", "Phone", "E-mail"])),
        related: db.related_accounts(account, filter).map_err(err)?,
    };
    let names = db.artifact_type_names().map_err(err)?;
    for id in db
        .relationship_sources(account, None, filter)
        .map_err(err)?
        .into_iter()
        .take(MAX_ITEMS)
    {
        let Some(artifact) = db.artifact(id).map_err(err)? else {
            continue;
        };
        let attrs = db.attributes(id).map_err(err)?;
        let ty = names
            .iter()
            .find(|n| n.0 == artifact.type_id)
            .map_or("", |n| n.1.as_str());
        let t = |n: &[&str]| text(&attrs, n, prefs);
        match ty {
            "KAAMOS_EMAIL_MSG" | "KAAMOS_MESSAGE" => {
                let date = [
                    "KAAMOS_DATETIME_SENT",
                    "KAAMOS_DATETIME_RCVD",
                    "KAAMOS_DATETIME",
                ];
                sel.messages.1.push(vec![
                    (t(&["KAAMOS_EMAIL_FROM", "KAAMOS_PHONE_NUMBER_FROM"]), None),
                    (t(&["KAAMOS_EMAIL_TO", "KAAMOS_PHONE_NUMBER_TO"]), None),
                    (t(&date), time_key(&attrs, &date)),
                    (t(&["KAAMOS_SUBJECT", "KAAMOS_TEXT"]), None),
                ]);
                sel.messages.0.push(Item { artifact, attrs });
            }
            "KAAMOS_CALLLOG" => {
                let date = ["KAAMOS_DATETIME_START", "KAAMOS_DATETIME"];
                sel.calls.1.push(vec![
                    (t(&["KAAMOS_PHONE_NUMBER_FROM"]), None),
                    (t(&["KAAMOS_PHONE_NUMBER_TO"]), None),
                    (t(&date), time_key(&attrs, &date)),
                    (t(&["KAAMOS_DIRECTION"]), None),
                    (t(&["KAAMOS_DATETIME_END"]), None),
                ]);
                sel.calls.0.push(Item { artifact, attrs });
            }
            "KAAMOS_CONTACT" => {
                sel.contacts.1.push(vec![
                    (t(&["KAAMOS_NAME"]), None),
                    (
                        t(&[
                            "KAAMOS_PHONE_NUMBER",
                            "KAAMOS_PHONE_NUMBER_MOBILE",
                            "KAAMOS_PHONE_NUMBER_CELL",
                            "KAAMOS_PHONE_NUMBER_HOME",
                        ]),
                        None,
                    ),
                    (
                        t(&["KAAMOS_EMAIL", "KAAMOS_EMAIL_HOME", "KAAMOS_EMAIL_WORK"]),
                        None,
                    ),
                ]);
                sel.contacts.0.push(Item { artifact, attrs });
            }
            _ => {}
        }
    }
    Ok(sel)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Messages,
    Calls,
    Contacts,
    Visualize,
}

pub(crate) struct CvtWindow {
    choices: Choices,
    filter: CommsFilter,
    applied: Option<CommsFilter>,
    start: String,
    end: String,
    kinds: BTreeSet<i64>,
    accounts: Cached<Accounts>,
    accounts_grid: Grid,
    selected: Option<Account>,
    selection: Cached<Selection>,
    tab: Tab,
    item_grid: Grid,
    item: Option<usize>,
}

impl std::fmt::Debug for CvtWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CvtWindow")
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

/// `YYYY-MM-DD` as the Unix second it starts (`end`: the day's last second).
fn parse_day(s: &str, end: bool) -> Option<i64> {
    let d: jiff::civil::Date = s.trim().parse().ok()?;
    let t = d
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    Some(if end { t + 86_399 } else { t })
}

impl CvtWindow {
    pub(crate) fn new(choices: Choices) -> Self {
        Self {
            filter: CommsFilter {
                ds_ids: choices.data_sources.iter().map(|d| d.0).collect(),
                account_type_ids: choices.account_types.iter().map(|t| t.0).collect(),
                ..CommsFilter::default()
            },
            kinds: [
                RelationshipKind::Message,
                RelationshipKind::CallLog,
                RelationshipKind::Contact,
            ]
            .iter()
            .map(|k| *k as i64)
            .collect(),
            choices,
            applied: None,
            start: String::new(),
            end: String::new(),
            accounts: Cached::default(),
            accounts_grid: Grid::default(),
            selected: None,
            selection: Cached::default(),
            tab: Tab::Messages,
            item_grid: Grid::default(),
            item: None,
        }
    }

    fn select(&mut self, a: Account) {
        self.selected = Some(a);
        self.selection.refresh();
        self.item = None;
        self.item_grid.reset();
    }

    /// Draws the window; `false` once closed.
    pub(crate) fn ui(
        &mut self,
        ctx: &egui::Context,
        case: &Arc<Case>,
        prefs: &DisplayPrefs,
    ) -> bool {
        if self.applied.is_none() {
            self.applied = Some(self.filter.clone());
            self.accounts.refresh();
        }
        let applied = self.applied.clone().unwrap_or_default();
        let (loader, f, choices) = (Arc::clone(case), applied.clone(), self.choices.clone());
        let accounts = self
            .accounts
            .get(ctx, move || load_accounts(&loader, &f, &choices));
        let selection = self.selected.as_ref().and_then(|a| {
            let (loader, f, p, id) = (Arc::clone(case), applied.clone(), prefs.clone(), a.id);
            self.selection
                .get(ctx, move || load_selection(&loader, id, &f, &p))
        });
        let mut open = true;
        let mut pick: Option<Account> = None;
        egui::Window::new("Communications")
            .open(&mut open)
            .default_size([1200.0, 760.0])
            .show(ctx, |ui| {
                egui::Panel::left("cvt-filters")
                    .resizable(true)
                    .default_size(230.0)
                    .show(ui, |ui| self.filters(ui));
                egui::Panel::left("cvt-accounts")
                    .resizable(true)
                    .default_size(420.0)
                    .min_size(320.0)
                    .show(ui, |ui| {
                        ui.strong("Accounts");
                        let Some(a) = accounts.as_deref() else {
                            ui.spinner();
                            return;
                        };
                        if a.rows.is_empty() {
                            ui.weak("No accounts with communications match the filters.");
                            return;
                        }
                        let ev = self
                            .accounts_grid
                            .ui(ui, "cvt-accounts-grid", &a.grid, |_, _| {});
                        if let Some(acc) = ev.selected.and_then(|i| a.rows.get(i)) {
                            pick = Some(acc.0.clone());
                        }
                    });
                egui::CentralPanel::default().show(ui, |ui| {
                    let Some(acc) = self.selected.clone() else {
                        ui.weak("Select an account to see its messages, call logs and contacts.");
                        return;
                    };
                    ui.heading(&acc.identifier);
                    let Some(sel) = selection.as_deref() else {
                        ui.spinner();
                        return;
                    };
                    ui.horizontal(|ui| {
                        for (t, label) in [
                            (
                                Tab::Messages,
                                format!("Messages ({})", sel.messages.0.len()),
                            ),
                            (Tab::Calls, format!("Call Logs ({})", sel.calls.0.len())),
                            (
                                Tab::Contacts,
                                format!("Contacts ({})", sel.contacts.0.len()),
                            ),
                            (Tab::Visualize, "Visualize".to_owned()),
                        ] {
                            if ui.selectable_label(self.tab == t, label).clicked() {
                                self.tab = t;
                                self.item = None;
                                self.item_grid.reset();
                            }
                        }
                    });
                    ui.separator();
                    let (items, grid) = match self.tab {
                        Tab::Messages => (&sel.messages.0, &sel.messages.1),
                        Tab::Calls => (&sel.calls.0, &sel.calls.1),
                        Tab::Contacts => (&sel.contacts.0, &sel.contacts.1),
                        Tab::Visualize => {
                            if let Some(a) = graph(ui, &acc, &sel.related) {
                                pick = Some(a);
                            }
                            return;
                        }
                    };
                    egui::Panel::bottom("cvt-item")
                        .resizable(true)
                        .default_size(220.0)
                        .show(ui, |ui| match self.item.and_then(|i| items.get(i)) {
                            Some(item) => details(ui, item, prefs),
                            None => {
                                ui.weak("Select an item to see its details.");
                            }
                        });
                    egui::CentralPanel::default().show(ui, |ui| {
                        let ev = self.item_grid.ui(ui, "cvt-items", grid, |_, _| {});
                        if let Some(i) = ev.selected {
                            self.item = Some(i);
                        }
                    });
                });
            });
        if let Some(a) = pick
            && self.selected.as_ref().is_none_or(|s| s.id != a.id)
        {
            self.select(a);
        }
        open
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong("Devices");
            for (id, name) in &self.choices.data_sources {
                toggle(ui, &mut self.filter.ds_ids, *id, name);
            }
            ui.separator();
            ui.strong("Account Types");
            for (id, name) in &self.choices.account_types {
                toggle(ui, &mut self.filter.account_type_ids, *id, name);
            }
            ui.separator();
            ui.strong("Relationships");
            for k in [
                RelationshipKind::Message,
                RelationshipKind::CallLog,
                RelationshipKind::Contact,
            ] {
                let mut on = self.kinds.contains(&(k as i64));
                if ui.checkbox(&mut on, k.display_name()).changed() {
                    if on {
                        self.kinds.insert(k as i64);
                    } else {
                        self.kinds.remove(&(k as i64));
                    }
                }
            }
            ui.separator();
            ui.strong("Date Range");
            egui::Grid::new("cvt-dates").num_columns(2).show(ui, |ui| {
                ui.label("Start:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.start)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0),
                );
                ui.end_row();
                ui.label("End:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.end)
                        .hint_text("YYYY-MM-DD")
                        .desired_width(100.0),
                );
                ui.end_row();
            });
            ui.add_space(6.0);
            if ui.button("Apply").clicked() {
                self.filter.start = parse_day(&self.start, false);
                self.filter.end = parse_day(&self.end, true);
                self.filter.kinds = [
                    RelationshipKind::Message,
                    RelationshipKind::CallLog,
                    RelationshipKind::Contact,
                ]
                .into_iter()
                .filter(|k| self.kinds.contains(&(*k as i64)))
                .collect();
                self.applied = Some(self.filter.clone());
                self.accounts.refresh();
                self.accounts_grid.reset();
                self.selection.refresh();
            }
        });
    }
}

fn toggle(ui: &mut egui::Ui, list: &mut Vec<i64>, id: i64, label: &str) {
    let mut on = list.contains(&id);
    if ui.checkbox(&mut on, label).changed() {
        if on {
            list.push(id);
        } else {
            list.retain(|x| *x != id);
        }
    }
}

/// Every attribute of the chosen item (Autopsy's message/contact viewer).
fn details(ui: &mut egui::Ui, item: &Item, prefs: &DisplayPrefs) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new(("cvt-detail", item.artifact.id))
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    for a in &item.attrs {
                        ui.label(&a.display_name);
                        let v = match &a.value {
                            Value::DateTime(t) => prefs.time(jiff::Timestamp::from_second(*t).ok()),
                            v => v.display(),
                        };
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                });
        });
}

/// The account and its correspondents (up to 48) on a circle; edge width
/// grows with the number of shared items. Clicking a node selects it.
fn graph(ui: &mut egui::Ui, center: &Account, related: &[(Account, u64)]) -> Option<Account> {
    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let c = rect.center();
    let radius = rect.width().min(rect.height()) * 0.38;
    let shown: Vec<&(Account, u64)> = related.iter().take(48).collect();
    let max = shown.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f32;
    let text_colour = ui.visuals().text_color();
    let mut picked = None;
    for (i, (acc, n)) in shown.iter().enumerate() {
        let angle = i as f32 / shown.len().max(1) as f32 * std::f32::consts::TAU;
        let p = c + egui::vec2(angle.cos(), angle.sin()) * radius;
        let w = 1.0 + 5.0 * (*n as f32 / max);
        painter.line_segment(
            [c, p],
            egui::Stroke::new(w, egui::Color32::from_rgb(0x1e, 0x6f, 0xd9)),
        );
        painter.circle_filled(p, 7.0, egui::Color32::from_rgb(0x2e, 0xa0, 0x43));
        painter.text(
            p + egui::vec2(0.0, 10.0),
            egui::Align2::CENTER_TOP,
            format!("{} ({n})", acc.identifier),
            egui::FontId::proportional(11.0),
            text_colour,
        );
        if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
            && pos.distance(p) < 10.0
        {
            picked = Some(acc.clone());
        }
    }
    painter.circle_filled(c, 10.0, egui::Color32::from_rgb(0xd9, 0x2e, 0x2e));
    painter.text(
        c + egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_TOP,
        &center.identifier,
        egui::FontId::proportional(13.0),
        text_colour,
    );
    if related.len() > shown.len() {
        painter.text(
            rect.left_bottom() + egui::vec2(6.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{} more accounts not shown", related.len() - shown.len()),
            egui::FontId::proportional(11.0),
            text_colour,
        );
    }
    picked
}

#[cfg(test)]
mod tests {
    #[test]
    fn days_parse_to_utc_bounds() {
        assert_eq!(super::parse_day("2021-03-04", false), Some(1_614_816_000));
        assert_eq!(super::parse_day("2021-03-04", true), Some(1_614_902_399));
        assert_eq!(super::parse_day("nope", false), None);
    }
}
