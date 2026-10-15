//! Autopsy's Timeline **Details** view: events grouped by type and
//! description (at a chosen detail level), clustered in time, and drawn as
//! labelled stripes packed into rows along a time axis.

use std::collections::HashMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use jiff::tz::TimeZone;
use pm_timeline::{EventType, ListRow, TimeUnit, Timeline};

/// Autopsy's "Description Detail" zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum DescLevel {
    Short,
    #[default]
    Medium,
    Full,
}

impl DescLevel {
    pub(crate) const ALL: [Self; 3] = [Self::Short, Self::Medium, Self::Full];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Short => "Short",
            Self::Medium => "Medium",
            Self::Full => "Full",
        }
    }
}

/// A run of same-type, same-description events close in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cluster {
    pub(crate) kind: EventType,
    pub(crate) description: String,
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) count: usize,
    /// Subject of the first event (for the content viewer).
    pub(crate) subject: u32,
}

/// The description at a detail level: URLs reduce to domain / first path
/// segment, paths to the top folder / parent folder.
pub(crate) fn describe(full: &str, level: DescLevel) -> String {
    if level == DescLevel::Full {
        return full.to_owned();
    }
    if let Some((scheme, rest)) = full.split_once("://") {
        let mut parts = rest.split('/');
        let host = parts.next().unwrap_or_default();
        return match level {
            DescLevel::Short => host.to_owned(),
            _ => match parts.next().filter(|p| !p.is_empty()) {
                Some(first) => format!("{scheme}://{host}/{first}"),
                None => format!("{scheme}://{host}"),
            },
        };
    }
    if full.starts_with('/') {
        let comps: Vec<&str> = full.split('/').filter(|c| !c.is_empty()).collect();
        return match level {
            DescLevel::Short => format!("/{}", comps.first().copied().unwrap_or_default()),
            _ => format!(
                "/{}",
                comps
                    .get(..comps.len().saturating_sub(1))
                    .unwrap_or_default()
                    .join("/")
            ),
        };
    }
    full.to_owned()
}

/// Groups and clusters events; `gap` is the largest time distance (s)
/// between events of one cluster.
pub(crate) fn clusters(
    rows: &[ListRow],
    timeline: &Timeline,
    level: DescLevel,
    gap: i64,
) -> Vec<Cluster> {
    let mut groups: HashMap<(EventType, String), Vec<(i64, u32)>> = HashMap::new();
    for r in rows {
        let Some(s) = timeline.subject(r.subject) else {
            continue;
        };
        let desc = describe(&s.path, level);
        for k in &r.kinds {
            groups
                .entry((*k, desc.clone()))
                .or_default()
                .push((r.time, r.subject));
        }
    }
    let mut out = Vec::new();
    for ((kind, description), mut evs) in groups {
        evs.sort_unstable();
        let mut cur: Option<Cluster> = None;
        for (t, subject) in evs {
            match &mut cur {
                Some(c) if t - c.end <= gap => {
                    c.end = t;
                    c.count += 1;
                }
                _ => {
                    if let Some(c) = cur.take() {
                        out.push(c);
                    }
                    cur = Some(Cluster {
                        kind,
                        description: description.clone(),
                        start: t,
                        end: t,
                        count: 1,
                        subject,
                    });
                }
            }
        }
        out.extend(cur);
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.kind.cmp(&b.kind)));
    out
}

/// What the user did in the details view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailsEvent {
    Select(Option<usize>),
    ZoomTo((i64, i64)),
    Wheel { zoom_in: bool, anchor: i64 },
}

const ROW_H: f32 = 22.0;
const AXIS_H: f32 = 26.0;
const MAX_ROWS: usize = 400;

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Draws the stripes; returns user actions.
pub(crate) fn view(
    ui: &mut egui::Ui,
    height: f32,
    clusters: &[Cluster],
    range: (i64, i64),
    tz: &TimeZone,
    selected: Option<usize>,
) -> Vec<DetailsEvent> {
    let mut events = Vec::new();
    let width = ui.available_width();
    let span = (range.1 - range.0).max(1) as f64;
    let font = FontId::proportional(12.0);
    // Pack clusters into rows (greedy, first fit).
    let x_of = |t: i64| ((t - range.0) as f64 / span * f64::from(width - 16.0)) as f32 + 8.0;
    let mut row_ends: Vec<f32> = Vec::new();
    let mut placed: Vec<(usize, usize, f32, f32, f32)> = Vec::new(); // (cluster, row, x0, bar_x1, label_end)
    for (i, c) in clusters.iter().enumerate() {
        let x0 = x_of(c.start);
        let bar_x1 = x_of(c.end).max(x0 + 6.0);
        let label_w = (c.description.chars().count() + 8) as f32 * 6.6;
        let end = bar_x1.max(x0 + 14.0 + label_w);
        let row = match row_ends.iter().position(|&e| e + 6.0 < x0) {
            Some(r) => r,
            None if row_ends.len() < MAX_ROWS => {
                row_ends.push(f32::MIN);
                row_ends.len() - 1
            }
            None => continue,
        };
        if let Some(e) = row_ends.get_mut(row) {
            *e = end;
        }
        placed.push((i, row, x0, bar_x1, end));
    }
    let content_h = AXIS_H + row_ends.len() as f32 * ROW_H + 8.0;
    egui::ScrollArea::vertical()
        .id_salt("tl-details-stripes")
        .max_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(width, content_h.max(height)),
                Sense::click_and_drag(),
            );
            let painter = ui.painter_at(rect);
            let v = ui.visuals().clone();
            let bg = if v.dark_mode {
                Color32::from_gray(30)
            } else {
                Color32::WHITE
            };
            let grid = if v.dark_mode {
                Color32::from_gray(55)
            } else {
                Color32::from_gray(225)
            };
            painter.rect_filled(rect, 0.0, bg);
            // Time axis: ticks at the unit for the visible span; labels (and
            // their grid lines) are thinned so they never overlap.
            let unit = TimeUnit::for_range(range.0, range.1);
            let mut t = unit.floor(range.0, tz);
            let mut guard = 0;
            let mut next_free = f32::MIN;
            while t < range.1 && guard < 2_000 {
                let x = rect.left() + x_of(t);
                if x >= rect.left() && x >= next_free {
                    let galley = painter.layout_no_wrap(
                        unit.label(t, tz),
                        FontId::proportional(11.0),
                        v.weak_text_color(),
                    );
                    let label_w = galley.size().x;
                    painter.line_segment(
                        [
                            Pos2::new(x, rect.top() + AXIS_H - 4.0),
                            Pos2::new(x, rect.bottom()),
                        ],
                        Stroke::new(1.0, grid),
                    );
                    painter.galley(
                        Pos2::new(x + 3.0, rect.top() + 4.0),
                        galley,
                        v.weak_text_color(),
                    );
                    next_free = x + label_w + 16.0;
                }
                t = unit.next(t, tz);
                guard += 1;
            }
            painter.line_segment(
                [
                    Pos2::new(rect.left(), rect.top() + AXIS_H - 4.0),
                    Pos2::new(rect.right(), rect.top() + AXIS_H - 4.0),
                ],
                Stroke::new(1.0, v.weak_text_color()),
            );
            // Stripes.
            let pointer = resp.hover_pos();
            let mut hovered = None;
            for &(i, row, x0, bar_x1, end) in &placed {
                let Some(c) = clusters.get(i) else { continue };
                let y = rect.top() + AXIS_H + row as f32 * ROW_H;
                let col = rgb(c.kind.color());
                let full = Rect::from_min_max(
                    Pos2::new(rect.left() + x0, y + 2.0),
                    Pos2::new(rect.left() + end, y + ROW_H - 2.0),
                );
                let is_hover = pointer.is_some_and(|p| full.contains(p));
                if is_hover {
                    hovered = Some(i);
                }
                let fill = col.gamma_multiply(if is_hover || selected == Some(i) {
                    0.35
                } else {
                    0.18
                });
                painter.rect_filled(full, 3.0, fill);
                // Time extent of the cluster.
                let bar = Rect::from_min_max(
                    Pos2::new(rect.left() + x0, y + 5.0),
                    Pos2::new(rect.left() + bar_x1, y + ROW_H - 5.0),
                );
                painter.rect_filled(bar, 2.0, col);
                // Type marker + label.
                painter.circle_filled(Pos2::new(rect.left() + x0 + 5.0, y + ROW_H / 2.0), 4.0, col);
                let label = if c.count > 1 {
                    format!("{} ({})", c.description, c.count)
                } else {
                    c.description.clone()
                };
                painter.text(
                    Pos2::new(rect.left() + x0 + 12.0, y + ROW_H / 2.0),
                    Align2::LEFT_CENTER,
                    label,
                    font.clone(),
                    v.text_color(),
                );
                if selected == Some(i) {
                    painter.rect_stroke(
                        full,
                        3.0,
                        Stroke::new(2.0, v.selection.stroke.color),
                        StrokeKind::Outside,
                    );
                }
            }
            if let Some(i) = hovered
                && let Some(c) = clusters.get(i)
            {
                let r = resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.strong(c.kind.name());
                    ui.label(&c.description);
                    ui.weak(format!(
                        "{} event(s): {} – {}",
                        c.count,
                        TimeUnit::Seconds.label(c.start, tz),
                        TimeUnit::Seconds.label(c.end, tz)
                    ));
                });
                if r.double_clicked() {
                    let pad = ((c.end - c.start) / 10).max(30);
                    events.push(DetailsEvent::ZoomTo((c.start - pad, c.end + pad + 1)));
                } else if r.clicked() {
                    events.push(DetailsEvent::Select(Some(i)));
                }
            } else if resp.clicked() {
                events.push(DetailsEvent::Select(None));
            }
            if let Some(p) = pointer {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if ui.input(|i| i.modifiers.command) && scroll.abs() > 0.5 {
                    let frac = f64::from(((p.x - rect.left()) / width).clamp(0.0, 1.0));
                    events.push(DetailsEvent::Wheel {
                        zoom_in: scroll > 0.0,
                        anchor: range.0 + (span * frac) as i64,
                    });
                }
            }
        });
    if clusters.is_empty() {
        ui.weak("No events in this time range.");
    } else if placed.len() < clusters.len() {
        ui.weak(format!(
            "{} clusters not shown — zoom in to see them.",
            clusters.len() - placed.len()
        ));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_levels() {
        let u = "http://www.google.com/search?q=x";
        assert_eq!(describe(u, DescLevel::Short), "www.google.com");
        assert_eq!(
            describe(u, DescLevel::Medium),
            "http://www.google.com/search?q=x"
        );
        assert_eq!(
            describe("https://a.com/x/y", DescLevel::Medium),
            "https://a.com/x"
        );
        let p = "/Users/alice/Pictures/sunset.bmp";
        assert_eq!(describe(p, DescLevel::Short), "/Users");
        assert_eq!(describe(p, DescLevel::Medium), "/Users/alice/Pictures");
        assert_eq!(describe(p, DescLevel::Full), p);
        assert_eq!(
            describe("hacking tools — google.com", DescLevel::Short),
            "hacking tools — google.com"
        );
    }
}
