//! Custom egui widgets reproducing Autopsy's Timeline visuals: the `JavaFX`
//! stacked bar chart of the Counts view and the range-slider histogram
//! below it.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use jiff::tz::TimeZone;
use pm_timeline::{Bin, TimeUnit};

/// Something the user did in the chart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ChartEvent {
    /// Click on a bar (or empty space: `None`).
    Select(Option<(i64, i64)>),
    /// Double-click on a bar or drag across bars, or a range picked in
    /// the overview: show this interval.
    ZoomTo((i64, i64)),
    /// Mouse wheel: scale the visible span by `factor` around `anchor`.
    Wheel { factor: f64, anchor: i64 },
}

/// Inputs of [`stacked_bars`].
pub(crate) struct Chart<'a> {
    pub(crate) bins: &'a [Bin],
    /// Series name and colour, parallel to `Bin::counts`.
    pub(crate) series: &'a [(String, [u8; 3])],
    pub(crate) log: bool,
    pub(crate) unit: TimeUnit,
    pub(crate) tz: &'a TimeZone,
    pub(crate) selection: Option<(i64, i64)>,
    pub(crate) range: (i64, i64),
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Darker edge of a bar segment (`JavaFX` draws bars with a border).
fn edge(c: Color32) -> Color32 {
    Color32::from_rgb(
        (f32::from(c.r()) * 0.7) as u8,
        (f32::from(c.g()) * 0.7) as u8,
        (f32::from(c.b()) * 0.7) as u8,
    )
}

/// Nice linear tick step for `max`.
fn linear_step(max: f64) -> f64 {
    let raw = (max / 5.0).max(1.0);
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let nice = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// Second label row for units whose labels repeat (e.g. months → year).
fn group_label(unit: TimeUnit, t: i64, tz: &TimeZone) -> Option<String> {
    let z = jiff::Timestamp::from_second(t).ok()?.to_zoned(tz.clone());
    match unit {
        TimeUnit::Months => Some(z.strftime("%Y").to_string()),
        TimeUnit::Days => Some(z.strftime("%B %Y").to_string()),
        TimeUnit::Hours | TimeUnit::Minutes | TimeUnit::Seconds => {
            Some(z.strftime("%Y-%m-%d").to_string())
        }
        TimeUnit::Years => None,
    }
}

/// Short per-bar label.
fn bar_label(unit: TimeUnit, t: i64, tz: &TimeZone) -> String {
    let Ok(ts) = jiff::Timestamp::from_second(t) else {
        return String::new();
    };
    let z = ts.to_zoned(tz.clone());
    match unit {
        TimeUnit::Years => z.strftime("%Y").to_string(),
        TimeUnit::Months => z.strftime("%b").to_string(),
        TimeUnit::Days => z.strftime("%d").to_string(),
        TimeUnit::Hours => z.strftime("%H:00").to_string(),
        TimeUnit::Minutes => z.strftime("%H:%M").to_string(),
        TimeUnit::Seconds => z.strftime("%H:%M:%S").to_string(),
    }
}

/// The Counts view: a stacked bar chart with axes, grid, labels, legend.
pub(crate) fn stacked_bars(ui: &mut egui::Ui, height: f32, chart: &Chart<'_>) -> Vec<ChartEvent> {
    let mut events = Vec::new();
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    let dark = vis.dark_mode;
    let bg = if dark {
        Color32::from_gray(30)
    } else {
        Color32::WHITE
    };
    let plot_bg = if dark {
        Color32::from_gray(38)
    } else {
        Color32::from_gray(244)
    };
    let grid = if dark {
        Color32::from_gray(60)
    } else {
        Color32::from_gray(214)
    };
    let axis = if dark {
        Color32::from_gray(140)
    } else {
        Color32::from_gray(110)
    };
    let text = vis.text_color();
    let weak = vis.weak_text_color();
    painter.rect_filled(rect, 0.0, bg);

    // Legend (bottom), axis labels, plot area.
    let used: Vec<usize> = (0..chart.series.len())
        .filter(|&s| {
            chart
                .bins
                .iter()
                .any(|b| b.counts.get(s).copied().unwrap_or(0) > 0)
        })
        .collect();
    let two_rows = group_label(chart.unit, chart.range.0, chart.tz).is_some();
    let legend_h = if used.is_empty() { 0.0 } else { 22.0 };
    let x_axis_h = if two_rows { 36.0 } else { 20.0 };
    let plot = Rect::from_min_max(
        rect.min + Vec2::new(56.0, 10.0),
        rect.max - Vec2::new(12.0, x_axis_h + legend_h + 6.0),
    );
    if chart.bins.is_empty() || plot.width() < 20.0 || plot.height() < 20.0 {
        return events;
    }
    painter.rect_filled(plot, 0.0, plot_bg);

    let scale = |n: f64| if chart.log { (n + 1.0).log10() } else { n };
    let max_total = chart.bins.iter().map(Bin::total).max().unwrap_or(0).max(1) as f64;
    let (top, ticks): (f64, Vec<f64>) = if chart.log {
        let top = max_total.log10().ceil().max(1.0);
        let ticks = (0..=top as i32).map(|e| 10f64.powi(e)).collect();
        (10f64.powf(top), ticks)
    } else {
        let step = linear_step(max_total);
        let top = (max_total / step).ceil() * step;
        let ticks = (0..=(top / step) as usize)
            .map(|i| i as f64 * step)
            .collect();
        (top, ticks)
    };
    let y_of = |n: f64| plot.bottom() - (scale(n) / scale(top)) as f32 * plot.height();
    // Horizontal grid + Y ticks.
    for t in &ticks {
        let y = y_of(*t);
        painter.line_segment(
            [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
            Stroke::new(1.0, grid),
        );
        painter.line_segment(
            [Pos2::new(plot.left() - 4.0, y), Pos2::new(plot.left(), y)],
            Stroke::new(1.0, axis),
        );
        painter.text(
            Pos2::new(plot.left() - 7.0, y),
            Align2::RIGHT_CENTER,
            crate::format::grouped(*t as u64),
            FontId::proportional(11.0),
            weak,
        );
    }
    painter.text(
        Pos2::new(rect.left() + 4.0, plot.center().y),
        Align2::LEFT_CENTER,
        if chart.log { "log" } else { "" },
        FontId::proportional(9.0),
        weak,
    );
    painter.line_segment(
        [plot.left_bottom(), plot.left_top()],
        Stroke::new(1.0, axis),
    );
    painter.line_segment(
        [plot.left_bottom(), plot.right_bottom()],
        Stroke::new(1.0, axis),
    );

    let n = chart.bins.len();
    let slot = plot.width() / n as f32;
    let bar_w = (slot * 0.8).max(1.0);
    let bin_at = |x: f32| -> Option<usize> {
        let i = ((x - plot.left()) / slot).floor();
        (i >= 0.0 && (i as usize) < n).then_some(i as usize)
    };
    let hovered = resp
        .hover_pos()
        .filter(|p| plot.contains(*p))
        .and_then(|p| bin_at(p.x));

    // Bars (stacked bottom-up in series order).
    for (i, b) in chart.bins.iter().enumerate() {
        let x0 = plot.left() + i as f32 * slot + (slot - bar_w) / 2.0;
        let mut cum = 0.0;
        let selected = chart.selection == Some((b.start, b.end));
        for (s, &cnt) in b.counts.iter().enumerate() {
            if cnt == 0 {
                continue;
            }
            let lo = y_of(cum);
            cum += f64::from(cnt);
            let hi = y_of(cum);
            let col = chart.series.get(s).map_or(Color32::GRAY, |(_, k)| rgb(*k));
            let col = if hovered == Some(i) {
                col.gamma_multiply(1.15)
            } else {
                col
            };
            let r = Rect::from_min_max(Pos2::new(x0, hi), Pos2::new(x0 + bar_w, lo));
            painter.rect_filled(r, 0.0, col);
            if bar_w > 4.0 {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, edge(col)), StrokeKind::Inside);
            }
        }
        if selected || hovered == Some(i) {
            let r = Rect::from_min_max(
                Pos2::new(x0 - 2.0, plot.top()),
                Pos2::new(x0 + bar_w + 2.0, plot.bottom()),
            );
            let stroke = if selected {
                Stroke::new(2.0, vis.selection.stroke.color)
            } else {
                Stroke::new(1.0, axis)
            };
            painter.rect_stroke(r, 2.0, stroke, StrokeKind::Outside);
        }
    }

    // X axis: per-bar labels (thinned to fit) and a group row.
    let label_w = match chart.unit {
        TimeUnit::Years | TimeUnit::Months | TimeUnit::Days => 34.0,
        _ => 56.0,
    };
    let every = ((label_w / slot).ceil() as usize).max(1);
    let mut last_group: Option<String> = None;
    for (i, b) in chart.bins.iter().enumerate() {
        let x = plot.left() + (i as f32 + 0.5) * slot;
        if i % every == 0 {
            painter.line_segment(
                [
                    Pos2::new(x, plot.bottom()),
                    Pos2::new(x, plot.bottom() + 4.0),
                ],
                Stroke::new(1.0, axis),
            );
            painter.text(
                Pos2::new(x, plot.bottom() + 5.0),
                Align2::CENTER_TOP,
                bar_label(chart.unit, b.start, chart.tz),
                FontId::proportional(11.0),
                if b.total() > 0 { text } else { weak },
            );
        }
        if let Some(g) = group_label(chart.unit, b.start, chart.tz)
            && last_group.as_ref() != Some(&g)
        {
            let gx = plot.left() + i as f32 * slot;
            painter.line_segment(
                [
                    Pos2::new(gx, plot.bottom()),
                    Pos2::new(gx, plot.bottom() + 32.0),
                ],
                Stroke::new(1.0, grid),
            );
            painter.text(
                Pos2::new(gx + 3.0, plot.bottom() + 20.0),
                Align2::LEFT_TOP,
                &g,
                FontId::proportional(11.0),
                text,
            );
            last_group = Some(g);
        }
    }

    // Legend.
    if !used.is_empty() {
        let mut lx = plot.left();
        let ly = rect.bottom() - legend_h / 2.0 - 2.0;
        for series in used {
            let Some((name, rgb_val)) = chart.series.get(series) else {
                continue;
            };
            let swatch = Rect::from_center_size(Pos2::new(lx + 6.0, ly), Vec2::splat(10.0));
            painter.rect_filled(swatch, 0.0, rgb(*rgb_val));
            painter.rect_stroke(
                swatch,
                0.0,
                Stroke::new(1.0, edge(rgb(*rgb_val))),
                StrokeKind::Inside,
            );
            let galley = painter.layout_no_wrap(name.clone(), FontId::proportional(11.0), text);
            let width = galley.size().x;
            painter.galley(
                Pos2::new(lx + 15.0, ly - galley.size().y / 2.0),
                galley,
                text,
            );
            lx += width + 30.0;
            if lx > rect.right() - 60.0 {
                break;
            }
        }
    }

    // Interaction.
    if let Some(i) = hovered
        && let Some(b) = chart.bins.get(i)
    {
        let resp2 = resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.strong(pm_timeline::TimeUnit::Seconds.label(b.start, chart.tz));
            ui.weak(format!(
                "to {}",
                TimeUnit::Seconds.label(b.end - 1, chart.tz)
            ));
            for (s, &cnt) in b.counts.iter().enumerate() {
                if cnt > 0 {
                    ui.horizontal(|ui| {
                        let col = chart.series.get(s).map_or(Color32::GRAY, |(_, k)| rgb(*k));
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                        ui.painter().rect_filled(r, 0.0, col);
                        ui.label(format!(
                            "{}: {}",
                            chart.series.get(s).map_or("", |(n, _)| n.as_str()),
                            crate::format::grouped(u64::from(cnt))
                        ));
                    });
                }
            }
            if b.total() == 0 {
                ui.weak("No events");
            }
        });
        if resp2.double_clicked() {
            events.push(ChartEvent::ZoomTo((b.start, b.end)));
        } else if resp2.clicked() {
            let sel = (chart.selection != Some((b.start, b.end))).then_some((b.start, b.end));
            events.push(ChartEvent::Select(sel));
        }
    } else if resp.clicked() {
        events.push(ChartEvent::Select(None));
    }
    // Drag across bars selects an interval to zoom into.
    let drag_id = resp.id.with("drag");
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        ui.data_mut(|d| d.insert_temp(drag_id, p.x));
    }
    if let (Some(x0), Some(p)) = (
        ui.data(|d| d.get_temp::<f32>(drag_id)),
        resp.interact_pointer_pos(),
    ) {
        if resp.dragged() {
            let band = Rect::from_min_max(
                Pos2::new(x0.min(p.x), plot.top()),
                Pos2::new(x0.max(p.x), plot.bottom()),
            );
            painter.rect_filled(band, 0.0, vis.selection.bg_fill.gamma_multiply(0.3));
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<f32>(drag_id));
            if let (Some(a), Some(b2)) = (
                bin_at(x0.min(p.x).max(plot.left())),
                bin_at(x0.max(p.x).min(plot.right() - 0.5)),
            ) && let (Some(first), Some(last)) = (chart.bins.get(a), chart.bins.get(b2))
            {
                events.push(ChartEvent::ZoomTo((first.start, last.end)));
            }
        }
    }
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let frac = f64::from(((p.x - plot.left()) / plot.width()).clamp(0.0, 1.0));
            let anchor = chart.range.0 + ((chart.range.1 - chart.range.0) as f64 * frac) as i64;
            events.push(ChartEvent::Wheel {
                factor: if scroll > 0.0 { 0.8 } else { 1.25 },
                anchor,
            });
        }
    }
    events
}

/// Autopsy's range slider: a histogram of the whole time span with a
/// window marking the visible interval; drag the window to pan, its edges
/// to resize. Returns the new interval when changed.
pub(crate) fn overview(
    ui: &mut egui::Ui,
    totals: &[u64],
    span: (i64, i64),
    range: (i64, i64),
    tz: &TimeZone,
) -> Option<(i64, i64)> {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 46.0),
        Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    let bg = if vis.dark_mode {
        Color32::from_gray(34)
    } else {
        Color32::from_gray(236)
    };
    painter.rect_filled(rect, 3.0, bg);
    let inner = rect.shrink2(Vec2::new(6.0, 4.0));
    let total_span = (span.1 - span.0).max(1) as f64;
    let x_of = |t: i64| inner.left() + ((t - span.0) as f64 / total_span) as f32 * inner.width();
    let t_of = |x: f32| {
        span.0 + (f64::from((x - inner.left()) / inner.width()).clamp(0.0, 1.0) * total_span) as i64
    };
    let max = totals.iter().copied().max().unwrap_or(0).max(1) as f32;
    let w = inner.width() / totals.len().max(1) as f32;
    let bar = vis.weak_text_color().gamma_multiply(0.7);
    for (i, &n) in totals.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let h = ((n as f32).ln_1p() / max.ln_1p()) * (inner.height() - 12.0);
        let x = inner.left() + i as f32 * w;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x, inner.bottom() - 12.0 - h),
                Pos2::new(x + w.max(1.0), inner.bottom() - 12.0),
            ),
            0.0,
            bar,
        );
    }
    let label = |t: i64| TimeUnit::Years.label(t, tz);
    painter.text(
        Pos2::new(inner.left(), inner.bottom()),
        Align2::LEFT_BOTTOM,
        label(span.0),
        FontId::proportional(10.0),
        vis.weak_text_color(),
    );
    painter.text(
        Pos2::new(inner.right(), inner.bottom()),
        Align2::RIGHT_BOTTOM,
        label(span.1),
        FontId::proportional(10.0),
        vis.weak_text_color(),
    );
    // Visible window.
    let (wx0, wx1) = (
        x_of(range.0.max(span.0)),
        x_of(range.1.min(span.1)).max(x_of(range.0.max(span.0)) + 3.0),
    );
    let win = Rect::from_min_max(
        Pos2::new(wx0, inner.top()),
        Pos2::new(wx1, inner.bottom() - 11.0),
    );
    painter.rect_filled(win, 2.0, vis.selection.bg_fill.gamma_multiply(0.35));
    painter.rect_stroke(
        win,
        2.0,
        Stroke::new(1.5, vis.selection.stroke.color),
        StrokeKind::Inside,
    );
    for x in [wx0, wx1] {
        painter.rect_filled(
            Rect::from_center_size(Pos2::new(x, win.center().y), Vec2::new(4.0, 14.0)),
            1.0,
            vis.selection.stroke.color,
        );
    }
    // Drag: which part was grabbed (0 = left edge, 1 = right edge, 2 = body).
    let id = resp.id.with("grab");
    if resp.drag_started()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let part: u8 = if (p.x - wx0).abs() < 6.0 {
            0
        } else if (p.x - wx1).abs() < 6.0 {
            1
        } else {
            2
        };
        ui.data_mut(|d| d.insert_temp(id, (part, p.x, range)));
    }
    let mut out = None;
    if let (Some((part, x0, r0)), Some(p)) = (
        ui.data(|d| d.get_temp::<(u8, f32, (i64, i64))>(id)),
        resp.interact_pointer_pos(),
    ) {
        if resp.dragged() {
            let dt = t_of(p.x) - t_of(x0);
            let new = match part {
                0 => ((r0.0 + dt).min(r0.1 - 1), r0.1),
                1 => (r0.0, (r0.1 + dt).max(r0.0 + 1)),
                _ => (r0.0 + dt, r0.1 + dt),
            };
            out = Some(new);
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<(u8, f32, (i64, i64))>(id));
        }
    } else if resp.clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        // Click centres the window on the clicked time.
        let half = (range.1 - range.0) / 2;
        let t = t_of(p.x);
        out = Some((t - half, t + half));
    }
    out
}
