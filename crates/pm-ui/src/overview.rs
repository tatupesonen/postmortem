//! Case Overview (the triage dashboard shown when a case opens): the
//! systems found, users, notable hits, the result types an examiner looks
//! at first, top web domains, USB devices, timeline density and notes —
//! each a link into the tree or a tool. Everything comes from aggregate
//! queries on the case database.

use std::sync::Arc;

use pm_case::Case;
use pm_casedb::Value;
use pm_casedb::catalog::artifact_type_id;

use crate::model::NodeId;
use crate::palette::{Action, Tool};

/// A system (operating system installation) found on a data source.
#[derive(Debug, Clone, Default)]
pub(crate) struct System {
    pub(crate) ds_id: i64,
    pub(crate) ds_name: String,
    pub(crate) os: String,
    pub(crate) computer: String,
    pub(crate) owner: String,
    pub(crate) time_zone: String,
    pub(crate) installed: Option<i64>,
}

/// What the dashboard shows.
#[derive(Debug, Clone, Default)]
pub(crate) struct Overview {
    pub(crate) data_sources: Vec<(i64, String, u64)>,
    pub(crate) systems: Vec<System>,
    pub(crate) users: u64,
    pub(crate) user_names: Vec<String>,
    pub(crate) bad: u64,
    pub(crate) suspicious: u64,
    /// Result types worth a first look: (type id, display name, count).
    pub(crate) highlights: Vec<(i64, String, u64)>,
    pub(crate) domains: Vec<(String, u64)>,
    pub(crate) usb: Vec<(String, u64)>,
    /// Events per year: (year start, count).
    pub(crate) timeline: Vec<(i64, u64)>,
    pub(crate) notes: u64,
    pub(crate) findings: u64,
}

/// The result types listed under "Look first", in this order.
const HIGHLIGHTS: [&str; 12] = [
    "KAAMOS_HASHSET_HIT",
    "KAAMOS_INTERESTING_FILE_HIT",
    "KAAMOS_INTERESTING_ITEM",
    "KAAMOS_ENCRYPTION_DETECTED",
    "KAAMOS_ENCRYPTION_SUSPECTED",
    "KAAMOS_KEYWORD_HIT",
    "KAAMOS_DEVICE_ATTACHED",
    "KAAMOS_INSTALLED_PROG",
    "KAAMOS_EMAIL_MSG",
    "KAAMOS_MESSAGE",
    "KAAMOS_WEB_HISTORY",
    "KAAMOS_WEB_DOWNLOAD",
];

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Text(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Loads the dashboard.
pub(crate) fn load(case: &Case) -> Result<Overview, String> {
    let db = case.db();
    let err = |e: pm_casedb::DbError| e.to_string();
    let mut o = Overview::default();
    let sources = db.data_sources().map_err(err)?;
    let os_type = artifact_type_id("KAAMOS_OS_INFO").unwrap_or(0);
    for ds in &sources {
        o.data_sources.push((ds.id, ds.name.clone(), ds.size));
        for a in db
            .artifacts_of_type(os_type, Some(ds.id), 0, 8)
            .map_err(err)?
        {
            let mut s = System {
                ds_id: ds.id,
                ds_name: ds.name.clone(),
                ..System::default()
            };
            for attr in db.attributes(a.id).map_err(err)? {
                match attr.name.as_str() {
                    "KAAMOS_PROG_NAME" => s.os = text(&attr.value).unwrap_or_default(),
                    "KAAMOS_NAME" => s.computer = text(&attr.value).unwrap_or_default(),
                    "KAAMOS_OWNER" => s.owner = text(&attr.value).unwrap_or_default(),
                    "KAAMOS_TIMEZONE" => s.time_zone = text(&attr.value).unwrap_or_default(),
                    "KAAMOS_DATETIME" => {
                        if let Value::DateTime(t) | Value::Int(t) = attr.value {
                            s.installed = Some(t);
                        }
                    }
                    _ => {}
                }
            }
            o.systems.push(s);
        }
    }
    o.users = db.os_account_count(None).map_err(err)?;
    o.user_names = db
        .os_accounts(None, 0, 200)
        .map_err(err)?
        .into_iter()
        .filter_map(|a| a.login_name.or(a.full_name))
        .take(12)
        .collect();
    let counts: std::collections::HashMap<i64, u64> =
        db.artifact_counts(None).map_err(err)?.into_iter().collect();
    for name in HIGHLIGHTS {
        if let Some(id) = artifact_type_id(name)
            && let Some(&n) = counts.get(&id)
            && n > 0
        {
            let display = pm_casedb::catalog::ARTIFACT_TYPES
                .iter()
                .find(|t| t.0 == id)
                .map_or(name, |t| t.2);
            o.highlights.push((id, display.to_owned(), n));
        }
    }
    let summary = crate::case_nodes::load_summary(case)?;
    o.bad = summary.bad;
    o.suspicious = summary.suspicious;
    o.domains = db
        .top_values("KAAMOS_WEB_HISTORY", "KAAMOS_DOMAIN", 10)
        .map_err(err)?;
    o.usb = db
        .top_values("KAAMOS_DEVICE_ATTACHED", "KAAMOS_DEVICE_MODEL", 8)
        .map_err(err)?;
    if o.usb.is_empty() {
        o.usb = db
            .top_values("KAAMOS_DEVICE_ATTACHED", "KAAMOS_DEVICE_ID", 8)
            .map_err(err)?;
    }
    let notes = db.notes(false).map_err(err)?;
    o.notes = notes.len() as u64;
    o.findings = notes.iter().filter(|n| n.finding).count() as u64;
    o.timeline = timeline_years(case).unwrap_or_default();
    Ok(o)
}

/// Events per year over the case's time span.
fn timeline_years(case: &Case) -> Result<Vec<(i64, u64)>, String> {
    let tl = pm_timeline::db::DbTimeline::open(Arc::clone(case.db())).map_err(|e| e.to_string())?;
    let Some((start, end)) = tl.span() else {
        return Ok(Vec::new());
    };
    let filter = pm_timeline::Filter::default();
    let tz = jiff::tz::TimeZone::UTC;
    let bins = tl
        .counts(&pm_timeline::CountsQuery {
            start,
            end: end + 1,
            unit: pm_timeline::TimeUnit::Years,
            tz: &tz,
            filter: &filter,
            zoom: pm_timeline::TypeZoom::Category,
            max_bins: 400,
        })
        .map_err(|e| e.to_string())?;
    Ok(bins
        .into_iter()
        .map(|b| (b.start, b.counts.iter().map(|c| u64::from(*c)).sum()))
        .collect())
}

fn date(t: i64) -> String {
    jiff::Timestamp::from_second(t)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Draws the dashboard; returns what a click asks for.
pub(crate) fn ui(ui: &mut egui::Ui, o: &Overview) -> Option<Action> {
    let mut action = None;
    let mut go = |a: Action| action = Some(a);
    ui.heading("Case Overview");
    ui.weak("What was found, at a glance. Every line opens its listing.");
    ui.add_space(6.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            // As many 320-point cards per row as fit.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "small positive"
            )]
            let cols = ((ui.available_width() + 8.0) / 344.0).floor().max(1.0) as usize;
            egui::Grid::new("overview-cards")
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    let mut shown = 0usize;
                    let mut card =
                        |ui: &mut egui::Ui, title: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
                            ui.group(|ui| {
                                ui.set_width(320.0);
                                ui.set_min_height(150.0);
                                ui.vertical(|ui| {
                                    ui.strong(title);
                                    ui.separator();
                                    body(ui);
                                });
                            });
                            shown += 1;
                            if shown.is_multiple_of(cols) {
                                ui.end_row();
                            }
                        };
                    card(ui, "Systems", &mut |ui| {
                        if o.systems.is_empty() {
                            ui.weak("No operating system found (yet).");
                        }
                        for s in &o.systems {
                            if ui
                                .link(if s.os.is_empty() { &s.ds_name } else { &s.os })
                                .clicked()
                            {
                                go(Action::Navigate(NodeId::DataSource(s.ds_id)));
                            }
                            let mut facts = Vec::new();
                            if !s.computer.is_empty() {
                                facts.push(format!("computer {}", s.computer));
                            }
                            if !s.owner.is_empty() {
                                facts.push(format!("owner {}", s.owner));
                            }
                            if !s.time_zone.is_empty() {
                                facts.push(format!("time zone {}", s.time_zone));
                            }
                            if let Some(t) = s.installed {
                                facts.push(format!("installed {}", date(t)));
                            }
                            ui.weak(format!("{} · {}", s.ds_name, facts.join(", ")));
                        }
                        for (id, name, size) in &o.data_sources {
                            if !o.systems.iter().any(|s| s.ds_id == *id)
                                && ui
                                    .link(format!("{name} ({})", crate::format::size_long(*size)))
                                    .clicked()
                            {
                                go(Action::Navigate(NodeId::DataSource(*id)));
                            }
                        }
                    });
                    card(ui, "Notable", &mut |ui| {
                        let plain = ui.visuals().text_color();
                        let red = if o.bad > 0 {
                            ui.visuals().error_fg_color
                        } else {
                            plain
                        };
                        let amber = if o.suspicious > 0 {
                            ui.visuals().warn_fg_color
                        } else {
                            plain
                        };
                        if ui
                            .link(egui::RichText::new(format!("{} bad items", o.bad)).color(red))
                            .clicked()
                        {
                            go(Action::Navigate(NodeId::Score { bad: true }));
                        }
                        if ui
                            .link(
                                egui::RichText::new(format!("{} suspicious items", o.suspicious))
                                    .color(amber),
                            )
                            .clicked()
                        {
                            go(Action::Navigate(NodeId::Score { bad: false }));
                        }
                        ui.add_space(4.0);
                        if ui
                            .link(format!("{} notes, {} findings", o.notes, o.findings))
                            .clicked()
                        {
                            go(Action::Tool(Tool::Notes));
                        }
                    });
                    card(ui, "Look first", &mut |ui| {
                        if o.highlights.is_empty() {
                            ui.weak("No results yet.");
                        }
                        for (id, name, n) in &o.highlights {
                            if ui
                                .link(format!("{name} ({})", crate::format::grouped(*n)))
                                .clicked()
                            {
                                go(Action::Navigate(NodeId::Results { type_id: *id }));
                            }
                        }
                    });
                    card(ui, &format!("Users ({})", o.users), &mut |ui| {
                        if o.user_names.is_empty() {
                            ui.weak("No OS accounts found.");
                        }
                        ui.horizontal_wrapped(|ui| {
                            for u in &o.user_names {
                                if ui.link(u).on_hover_text("Pivot on this user").clicked() {
                                    go(Action::Pivot(u.clone()));
                                }
                            }
                        });
                        if o.users > 0 && ui.link("All OS accounts").clicked() {
                            go(Action::Navigate(NodeId::OsAccounts));
                        }
                    });
                    card(ui, "Top web domains", &mut |ui| {
                        if o.domains.is_empty() {
                            ui.weak("No web history.");
                        }
                        for (d, n) in &o.domains {
                            if ui
                                .link(format!("{d} ({n})"))
                                .on_hover_text("Pivot on this domain")
                                .clicked()
                            {
                                go(Action::Pivot(d.clone()));
                            }
                        }
                    });
                    card(ui, "USB devices", &mut |ui| {
                        if o.usb.is_empty() {
                            ui.weak("No USB devices.");
                        }
                        for (d, n) in &o.usb {
                            if ui.link(format!("{d} ({n})")).clicked() {
                                go(Action::Pivot(d.clone()));
                            }
                        }
                    });
                    card(ui, "Timeline", &mut |ui| {
                        if o.timeline.is_empty() {
                            ui.weak("No events.");
                            return;
                        }
                        let max = o.timeline.iter().map(|b| b.1).max().unwrap_or(1).max(1);
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(300.0, 80.0), egui::Sense::click());
                        let w = rect.width() / o.timeline.len() as f32;
                        let color = ui.visuals().selection.bg_fill;
                        for (i, (_, n)) in o.timeline.iter().enumerate() {
                            #[expect(clippy::cast_precision_loss, reason = "bar heights")]
                            let h = rect.height() * (*n as f32 / max as f32);
                            let x = rect.left() + w * i as f32;
                            ui.painter().rect_filled(
                                egui::Rect::from_min_max(
                                    egui::pos2(x, rect.bottom() - h),
                                    egui::pos2(x + (w - 1.0).max(1.0), rect.bottom()),
                                ),
                                0.0,
                                color,
                            );
                        }
                        let first = o.timeline.first().map(|b| date(b.0)).unwrap_or_default();
                        let last = o.timeline.last().map(|b| date(b.0)).unwrap_or_default();
                        ui.weak(format!(
                            "{} – {}, events per year",
                            first.get(..4).unwrap_or(""),
                            last.get(..4).unwrap_or("")
                        ));
                        if resp.on_hover_text("Open the Timeline").clicked()
                            || ui.link("Open the Timeline").clicked()
                        {
                            go(Action::Tool(Tool::Timeline));
                        }
                    });
                });
        });
    action
}
