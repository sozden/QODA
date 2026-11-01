//! The look shared by the QODA windows (receiver and transmitter): colours, theme and widgets.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Vec2, vec2};

/// Colour palette.
pub mod palette {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(11, 15, 22);
    pub const DEEP: Color32 = Color32::from_rgb(7, 10, 15);
    pub const CARD: Color32 = Color32::from_rgb(19, 25, 35);
    pub const BORDER: Color32 = Color32::from_rgb(34, 43, 57);
    pub const TEXT: Color32 = Color32::from_rgb(214, 222, 235);
    pub const MUTED: Color32 = Color32::from_rgb(128, 141, 160);
    pub const ACCENT: Color32 = Color32::from_rgb(56, 200, 240);
    pub const GOOD: Color32 = Color32::from_rgb(74, 222, 128);
    pub const WARN: Color32 = Color32::from_rgb(251, 191, 36);
    pub const BAD: Color32 = Color32::from_rgb(248, 113, 113);
    pub const BAND: Color32 = Color32::from_rgb(251, 191, 36);
}
use palette::*;

/// Dark theme with navy tones.
pub fn apply_theme(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        let mut v = egui::Visuals::dark();
        v.panel_fill = BG;
        v.window_fill = CARD;
        v.extreme_bg_color = DEEP;
        v.faint_bg_color = CARD;
        v.window_corner_radius = CornerRadius::same(10);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let radius = CornerRadius::same(6);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.noninteractive.corner_radius = radius;
        for (w, fill) in [
            (&mut v.widgets.inactive, Color32::from_rgb(28, 36, 49)),
            (&mut v.widgets.hovered, Color32::from_rgb(38, 50, 67)),
            (&mut v.widgets.active, Color32::from_rgb(48, 64, 86)),
            (&mut v.widgets.open, Color32::from_rgb(38, 50, 67)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.corner_radius = radius;
        }
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals = v;
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.interact_size.y = 24.0;
    });
}

/// A titled, bordered card.
pub fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(11.0).strong().color(ACCENT));
            ui.add_space(6.0);
            add(ui);
        });
    ui.add_space(10.0);
}

/// Frequency display tunable digit by digit (MHz, 3 decimals). The mouse
/// wheel over a digit changes it by that digit's place; clicking the upper half
/// of a digit increases it, clicking the lower half decreases it.
pub fn freq_tuner(ui: &mut egui::Ui, mhz: &mut f64, size: f32) -> bool {
    let text = group_mhz(*mhz);
    let int_digits = text.split('.').next().unwrap_or("").chars().filter(char::is_ascii_digit).count();
    let font = FontId::monospace(size);
    let mut delta = 0.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (mut int_index, mut frac_index, mut after_dot) = (0, 0, false);
        for c in text.chars() {
            let step = if !c.is_ascii_digit() {
                after_dot |= c == '.';
                None
            } else if after_dot {
                frac_index += 1;
                Some(10f64.powi(-frac_index))
            } else {
                int_index += 1;
                Some(10f64.powi((int_digits - int_index) as i32))
            };
            let plain = ui.painter().layout_no_wrap(c.to_string(), font.clone(), TEXT);
            let sense = if step.is_some() { Sense::click() } else { Sense::hover() };
            let (rect, response) = ui.allocate_exact_size(plain.size(), sense);
            let mut galley = plain;
            if let (Some(step), true) = (step, response.hovered()) {
                ui.painter().rect_filled(rect.expand(1.0), CornerRadius::same(3), ACCENT.gamma_multiply(0.22));
                galley = ui.painter().layout_no_wrap(c.to_string(), font.clone(), ACCENT);
                let (steps, _) = wheel_steps(ui);
                delta += steps as f64 * step;
                if let Some(pos) = response.clicked().then(|| response.interact_pointer_pos()).flatten() {
                    delta += if pos.y < rect.center().y { step } else { -step };
                }
            }
            ui.painter().galley(rect.min, galley, TEXT);
        }
        ui.label(RichText::new(" MHz").size(size * 0.55).color(MUTED));
    });
    if delta == 0.0 {
        return false;
    }
    *mhz = ((*mhz + delta) * 1000.0).round() / 1000.0;
    true
}

/// Mouse wheel steps in this frame (up is positive) and whether Shift is held.
/// The scroll used here is consumed so it does not reach surrounding scroll areas.
pub fn wheel_steps(ui: &egui::Ui) -> (i32, bool) {
    let (steps, shift) = ui.input(|i| {
        let steps: i32 = i
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => {
                    let d = delta.x + delta.y;
                    (d != 0.0).then_some(d.signum() as i32)
                }
                _ => None,
            })
            .sum();
        (steps, i.modifiers.shift)
    });
    if steps != 0 {
        ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
    }
    (steps, shift)
}

/// Small coloured status dot.
pub fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

/// Small labelled measurement tile.
pub fn metric(ui: &mut egui::Ui, label: &str, value: &str, color: Color32) {
    egui::Frame::new()
        .fill(DEEP)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(108.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(label).size(10.0).color(MUTED));
                ui.label(RichText::new(value).size(14.0).monospace().color(color));
            });
        });
}

/// 10491.527 → "10 491.527" (frequency made readable with a thousands separator).
pub fn group_mhz(mhz: f64) -> String {
    let text = format!("{mhz:.3}");
    let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(c);
    }
    format!("{grouped}.{frac}")
}
