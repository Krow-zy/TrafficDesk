//! Fixed dark palette and hand-painted widgets (monospace, tabular, no reflow).
//!
//! The window is small and always on top, so the look is deliberately quiet:
//! one accent colour per row for the process category, one dot for how the
//! destination got its name, numbers right-aligned so they never jitter.

use crate::model::{Category, NameSource, fmt_rate};
use eframe::egui::{self, Align, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};

pub const BG: Color32 = Color32::from_rgb(10, 13, 17);
pub const BAR_BG: Color32 = Color32::from_rgb(16, 20, 26);
pub const ROW_BG: Color32 = Color32::from_rgb(19, 24, 30);
pub const ROW_ALT: Color32 = Color32::from_rgb(22, 28, 35);
pub const ROW_SEL: Color32 = Color32::from_rgb(30, 48, 62);
pub const ROW_HOVER: Color32 = Color32::from_rgb(27, 35, 44);
pub const BORDER: Color32 = Color32::from_rgb(31, 39, 48);
pub const TEXT: Color32 = Color32::from_rgb(214, 223, 232);
pub const DIM: Color32 = Color32::from_rgb(112, 127, 142);
pub const HEAD: Color32 = Color32::from_rgb(138, 155, 172);
pub const UP: Color32 = Color32::from_rgb(61, 220, 132);
pub const DOWN: Color32 = Color32::from_rgb(77, 184, 255);
pub const CACHED: Color32 = Color32::from_rgb(61, 220, 132);
pub const REMEMBERED: Color32 = Color32::from_rgb(126, 168, 206);
pub const WARN: Color32 = Color32::from_rgb(255, 176, 76);
pub const ERR: Color32 = Color32::from_rgb(255, 107, 107);

pub const ROW_H: f32 = 20.0;
pub const GAP: f32 = 4.0;

pub fn font(size: f32) -> FontId {
    FontId::monospace(size)
}

pub fn cat_color(c: Category) -> Color32 {
    match c {
        Category::System => Color32::from_rgb(122, 138, 154),
        Category::Browser => Color32::from_rgb(224, 164, 88),
        Category::Other => Color32::from_rgb(111, 168, 220),
    }
}

/// Colour of the "how was this named" dot. Unnamed rows get a hollow ring.
pub fn name_color(source: NameSource) -> Option<Color32> {
    match source {
        NameSource::Cache => Some(CACHED),
        NameSource::Remembered => Some(REMEMBERED),
        NameSource::None => None,
    }
}

pub fn corner(r: u8) -> egui::CornerRadius {
    egui::CornerRadius::same(r)
}

/// Truncate `text` so it fits `max_w` in `f`. Assumes fixed-width digits, so a
/// single measure of the full string is enough for the common case.
pub fn fit(painter: &egui::Painter, text: &str, f: &FontId, max_w: f32) -> String {
    if painter
        .layout_no_wrap(text.to_owned(), f.clone(), TEXT)
        .size()
        .x
        <= max_w
    {
        return text.to_owned();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "..";
        if painter
            .layout_no_wrap(candidate.clone(), f.clone(), TEXT)
            .size()
            .x
            <= max_w
        {
            return candidate;
        }
    }
    "..".to_string()
}

fn text_at(
    painter: &egui::Painter,
    rect: Rect,
    text: &str,
    color: Color32,
    size: f32,
    align: Align,
) {
    let (anchor, x) = match align {
        Align::Min => (Align2::LEFT_CENTER, rect.left()),
        Align::Center => (Align2::CENTER_CENTER, rect.center().x),
        Align::Max => (Align2::RIGHT_CENTER, rect.right()),
    };
    painter.text(
        egui::pos2(x, rect.center().y),
        anchor,
        text,
        font(size),
        color,
    );
}

/// Column layout shared by rows and their header, so the two always line up.
fn column_rects(right: f32, top: f32, bottom: f32, cols: &[(String, f32)]) -> (Vec<Rect>, f32) {
    let mut rects = Vec::with_capacity(cols.len());
    let mut edge = right - 6.0;
    for (_, w) in cols.iter().rev() {
        rects.push(Rect::from_min_max(
            egui::pos2(edge - w, top),
            egui::pos2(edge, bottom),
        ));
        edge -= w + 4.0;
    }
    rects.reverse();
    (rects, edge)
}

/// Header line above a list: same columns, dim, no background.
pub fn col_header(ui: &mut egui::Ui, cols: &[(String, f32)], key: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 15.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let (rects, key_edge) = column_rects(rect.right(), rect.top(), rect.bottom(), cols);
    for ((label, _), cell) in cols.iter().zip(rects) {
        text_at(p, cell, label, HEAD, 10.0, Align::Max);
    }
    text_at(
        p,
        Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.top()),
            egui::pos2(key_edge - 4.0, rect.bottom()),
        ),
        key,
        HEAD,
        10.0,
        Align::Min,
    );
}

/// Paint a fixed-height, fixed-column text row. Columns are laid out from the
/// right edge, so live values never shift neighbouring cells.
#[allow(clippy::too_many_arguments)]
pub fn paint_row(
    ui: &mut egui::Ui,
    height: f32,
    index: usize,
    selected: bool,
    accent: Color32,
    key: &str,
    key_color: Color32,
    cols: &[(String, f32)],
    named: NameSource,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let bg = if selected {
        ROW_SEL
    } else if resp.hovered() {
        ROW_HOVER
    } else if index.is_multiple_of(2) {
        ROW_BG
    } else {
        ROW_ALT
    };
    let p = ui.painter();
    let r = resp.rect.shrink2(Vec2::new(0.0, 0.5));
    p.rect_filled(r, corner(3), bg);
    // Category stripe: the process family the row mostly belongs to.
    p.rect_filled(
        Rect::from_min_size(
            r.min + Vec2::new(0.0, 2.0),
            Vec2::new(2.0, r.height() - 4.0),
        ),
        corner(1),
        accent,
    );

    let size = if height <= 18.0 { 11.0 } else { 11.5 };
    let (rects, key_edge) = column_rects(r.right(), r.top(), r.bottom(), cols);
    for ((text, _), cell) in cols.iter().zip(rects) {
        text_at(p, cell, text, DIM, size, Align::Max);
    }

    // Name-source dot: filled when a name is known, a hollow ring for a bare IP.
    let dot = egui::pos2(r.left() + 8.0, r.center().y);
    if let Some(color) = name_color(named) {
        p.circle_filled(dot, 2.5, color);
    } else {
        p.circle_stroke(
            dot,
            2.5,
            Stroke::new(1.0_f32, Color32::from_rgb(74, 87, 100)),
        );
    }
    let name_rect = Rect::from_min_max(
        egui::pos2(dot.x + 6.0, r.top()),
        egui::pos2(key_edge - 4.0, r.bottom()),
    );
    let name = fit(p, key, &font(size), name_rect.width());
    text_at(p, name_rect, &name, key_color, size, Align::Min);
    resp
}

/// Small rounded badge, used for the name source and in the footer legend.
pub fn chip(ui: &mut egui::Ui, text: &str, color: Color32) {
    let f = font(10.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), f.clone(), color);
    let size = Vec2::new(galley.size().x + 12.0, 14.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    p.rect_filled(rect, corner(3), BAR_BG);
    p.rect_stroke(
        rect,
        corner(3),
        Stroke::new(1.0_f32, BORDER),
        StrokeKind::Inside,
    );
    p.text(rect.center(), Align2::CENTER_CENTER, text, f, color);
}

/// Section label with a hairline, keeps the settings tab readable.
pub fn section(ui: &mut egui::Ui, label: &str) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let text_w = p
        .layout_no_wrap(label.to_owned(), font(10.0), HEAD)
        .size()
        .x;
    p.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        font(10.0),
        HEAD,
    );
    p.line_segment(
        [
            egui::pos2(rect.left() + text_w + 6.0, rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        Stroke::new(1.0_f32, BORDER),
    );
}

/// One dot of the name-source legend.
pub fn legend(ui: &mut egui::Ui, snap_source: NameSource, label: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 12.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if let Some(c) = name_color(snap_source) {
            p.circle_filled(rect.center(), 2.5, c);
        } else {
            p.circle_stroke(
                rect.center(),
                2.5,
                Stroke::new(1.0_f32, Color32::from_rgb(74, 87, 100)),
            );
        }
    }
    ui.label(egui::RichText::new(label).color(DIM).monospace().size(10.0));
}

/// Dual-sided bar graph of the last 60 throughput samples.
pub fn sparkline(ui: &mut egui::Ui, samples: &[(f64, f64)], height: f32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    p.rect_filled(rect, corner(3), BAR_BG);
    p.rect_stroke(
        rect,
        corner(3),
        Stroke::new(1.0_f32, BORDER),
        StrokeKind::Inside,
    );
    let inner = rect.shrink2(Vec2::new(1.0, 1.0));
    let mid = inner.center().y;
    // Quarter lines, so a quiet graph still shows its scale.
    for f in [0.25f32, 0.75] {
        let y = inner.top() + inner.height() * f;
        p.line_segment(
            [egui::pos2(inner.left(), y), egui::pos2(inner.right(), y)],
            Stroke::new(1.0_f32, Color32::from_rgb(26, 33, 41)),
        );
    }
    p.line_segment(
        [
            egui::pos2(inner.left(), mid),
            egui::pos2(inner.right(), mid),
        ],
        Stroke::new(1.0_f32, Color32::from_rgb(44, 54, 64)),
    );
    if samples.is_empty() {
        p.text(
            egui::pos2(inner.left() + 5.0, mid),
            Align2::LEFT_CENTER,
            "collecting throughput..",
            font(10.0),
            DIM,
        );
        return;
    }
    let peak_up = samples.iter().map(|s| s.0).fold(1.0f64, f64::max);
    let peak_down = samples.iter().map(|s| s.1).fold(1.0f64, f64::max);
    let slot = inner.width() / crate::collect::SPARK_LEN as f32;
    let bar_w = (slot - 1.0).max(1.0);
    let half = mid - inner.top() - 2.0;
    let start = crate::collect::SPARK_LEN.saturating_sub(samples.len());
    for (i, (up, down)) in samples.iter().enumerate() {
        let x = inner.left() + (start + i) as f32 * slot;
        let up_h = ((up / peak_up) * half as f64) as f32;
        let down_h = ((down / peak_down) * half as f64) as f32;
        if up_h > 0.4 {
            p.rect_filled(
                Rect::from_min_max(egui::pos2(x, mid - up_h), egui::pos2(x + bar_w, mid - 0.5)),
                corner(0),
                UP,
            );
        }
        if down_h > 0.4 {
            p.rect_filled(
                Rect::from_min_max(
                    egui::pos2(x, mid + 0.5),
                    egui::pos2(x + bar_w, mid + down_h),
                ),
                corner(0),
                DOWN,
            );
        }
    }
    // Latest sample as text, so the graph never has to be read off the pixels.
    let (last_up, last_down) = samples.last().copied().unwrap_or((0.0, 0.0));
    let label = format!("DN {}  UP {}", fmt_rate(last_down), fmt_rate(last_up));
    p.text(
        egui::pos2(inner.right() - 4.0, inner.top() + 6.0),
        Align2::RIGHT_CENTER,
        label,
        font(9.5),
        Color32::from_rgb(150, 164, 178),
    );
}

/// Small key/value row used on the settings and detail tabs.
pub fn kv(ui: &mut egui::Ui, key: &str, value: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [104.0, 15.0],
            egui::Label::new(egui::RichText::new(key).color(DIM).monospace().size(11.0)),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(value)
                    .color(color)
                    .monospace()
                    .size(11.0),
            )
            .truncate(),
        );
    });
}

pub fn set_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = BG;
    style.visuals.window_fill = BG;
    style.visuals.extreme_bg_color = BAR_BG;
    style.visuals.faint_bg_color = ROW_BG;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.window_corner_radius = corner(4);
    style.visuals.selection.bg_fill = ROW_SEL;
    style.visuals.selection.stroke = Stroke::new(1.0_f32, TEXT);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.inactive.weak_bg_fill = BAR_BG;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    style.visuals.widgets.inactive.corner_radius = corner(3);
    style.visuals.widgets.hovered.weak_bg_fill = ROW_HOVER;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(58, 72, 86));
    style.visuals.widgets.hovered.corner_radius = corner(3);
    style.visuals.widgets.active.weak_bg_fill = ROW_SEL;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, DOWN);
    style.visuals.widgets.active.corner_radius = corner(3);
    style.spacing.item_spacing = Vec2::new(6.0, GAP);
    style.spacing.button_padding = Vec2::new(6.0, 2.0);
    style.spacing.interact_size = Vec2::new(18.0, 16.0);
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.floating = false;
    style.spacing.slider_width = 150.0;
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_rows_get_a_dot_only_when_a_name_is_known() {
        assert!(name_color(NameSource::None).is_none());
        assert_eq!(name_color(NameSource::Cache), Some(CACHED));
        assert_eq!(name_color(NameSource::Remembered), Some(REMEMBERED));
    }

    #[test]
    fn category_colours_are_distinct() {
        let (a, b, c) = (
            cat_color(Category::System),
            cat_color(Category::Browser),
            cat_color(Category::Other),
        );
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }
}
