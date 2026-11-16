//! Dark developer theme.

use egui::{Color32, CornerRadius, Stroke, Visuals};

/// The window's title strip, behind the view tabs.
pub const TITLE: Color32 = Color32::from_rgb(0x0e, 0x10, 0x14);
pub const BG: Color32 = Color32::from_rgb(0x16, 0x18, 0x1d);
pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x26);
pub const CARD: Color32 = Color32::from_rgb(0x24, 0x28, 0x30);
pub const CARD_HOVER: Color32 = Color32::from_rgb(0x2c, 0x31, 0x3b);
pub const BORDER: Color32 = Color32::from_rgb(0x34, 0x39, 0x44);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdc, 0xe4);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8a, 0x92, 0xa1);
pub const ACCENT: Color32 = Color32::from_rgb(0x4f, 0xc3, 0xa1);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x2a, 0x6e, 0x5c);
pub const WARN: Color32 = Color32::from_rgb(0xe8, 0xb3, 0x4b);
pub const ERROR: Color32 = Color32::from_rgb(0xe5, 0x5c, 0x5c);

// Pin colours
pub const PIN_POWER: Color32 = Color32::from_rgb(0x4a, 0x4f, 0x5a);
pub const PIN_SPECIAL: Color32 = Color32::from_rgb(0x8c, 0x74, 0x4a);
pub const PIN_FREE: Color32 = Color32::from_rgb(0x9a, 0xa1, 0xad);
pub const PIN_GPIO: Color32 = Color32::from_rgb(0xe8, 0xc5, 0x4b);
pub const PIN_PERIPH: Color32 = Color32::from_rgb(0x4f, 0xc3, 0xa1);
pub const PIN_CONFLICT: Color32 = Color32::from_rgb(0xe5, 0x5c, 0x5c);

pub fn install(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(0x12, 0x14, 0x18);
    v.faint_bg_color = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.override_text_color = None;

    let r = CornerRadius::same(5);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.weak_bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.bg_fill = CARD_HOVER;
    v.widgets.inactive.weak_bg_fill = CARD_HOVER;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.bg_fill = CARD_HOVER;
    v.widgets.hovered.weak_bg_fill = CARD_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_DIM);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.corner_radius = r;
    v.widgets.open.weak_bg_fill = CARD_HOVER;
    v.widgets.open.corner_radius = r;

    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
    });
}

/// A primary (accent) button.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_string()).color(Color32::from_rgb(0x0e, 0x1a, 0x16)).strong())
        .fill(ACCENT)
        .corner_radius(CornerRadius::same(5))
}

pub fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(18.0).strong().color(TEXT));
}

pub fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text.to_uppercase()).size(11.0).strong().color(TEXT_DIM));
}

pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
}
