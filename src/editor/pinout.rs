//! Interactive package pinout (QFP/QFN, SO/TSSOP and BGA/WLCSP layouts).

use std::f32::consts::FRAC_PI_2;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::Editor;
use crate::db::{Chip, Package, gpio_name};
use crate::project::{PinMode, Pull, Speed};
use crate::theme;

#[derive(Default)]
pub struct PinView {
    zoom: f32,
    pan: Vec2,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PinPopupKind {
    /// First step: the pin's functions (GPIO modes and alternate functions).
    Functions,
    /// Second step, shown by itself once a function is chosen: the user label.
    Label,
}

/// The popup opened by clicking a pin: choose its function, then name it.
pub struct PinPopup {
    gpio: String,
    kind: PinPopupKind,
    /// True on the frame the popup was opened.
    opening: bool,
    /// Label being edited.
    label: String,
}

impl PinPopup {
    /// The function list of a pin; choosing one leads on to the label prompt.
    pub(super) fn functions(gpio: String) -> Self {
        Self { gpio, kind: PinPopupKind::Functions, opening: true, label: String::new() }
    }

    /// The label prompt alone, for a pin that already has a function.
    pub(super) fn label(gpio: String, label: String) -> Self {
        Self { gpio, kind: PinPopupKind::Label, opening: true, label }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Bottom,
    Right,
    Top,
    Grid,
}

#[derive(Clone, Copy, PartialEq)]
enum PinKind {
    Gpio,
    Power,
    Special,
}

struct PinGeom {
    position: String,
    signals: Vec<String>,
    gpio: Option<String>,
    kind: PinKind,
    /// Model-space rectangle (for grid pins, the ball's bounding box).
    rect: Rect,
    side: Side,
}

struct Layout {
    pins: Vec<PinGeom>,
    body: Rect,
    /// Extent including room for labels.
    bounds: Rect,
    grid: bool,
}

fn classify(signals: &[String]) -> (Option<String>, PinKind) {
    if let Some(g) = signals.iter().find_map(|s| gpio_name(s)) {
        return (Some(g), PinKind::Gpio);
    }
    let first = signals.first().map(String::as_str).unwrap_or("");
    let power = ["VDD", "VSS", "VBAT", "VREF", "VCAP", "VLCD", "VCORE", "VDDA", "VSSA", "VDDIO", "VDDUSB", "VDDSMPS", "VSSSMPS", "VLXSMPS", "VFBSMPS"];
    if power.iter().any(|p| first.starts_with(p)) {
        (None, PinKind::Power)
    } else {
        (None, PinKind::Special)
    }
}

const PIN_LEN: f32 = 2.4;
const PIN_W: f32 = 0.62;
const LABEL_ROOM: f32 = 9.0;

fn build_layout(pkg: &Package) -> Layout {
    let grid = pkg.pins.iter().any(|p| p.position.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    if grid {
        return grid_layout(pkg);
    }
    // Numeric positions; package name usually carries the lead count (LQFP64 -> 64).
    let mut pins: Vec<(u32, &crate::db::PackagePin)> = pkg
        .pins
        .iter()
        .filter_map(|p| p.position.parse::<u32>().ok().map(|n| (n, p)))
        .collect();
    pins.sort_by_key(|(n, _)| *n);
    let declared: u32 = pkg.package.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
    let max_pos = pins.last().map(|(n, _)| *n).unwrap_or(0);
    let n = if declared >= 4 && declared <= max_pos { declared } else { max_pos };
    let up = pkg.package.to_ascii_uppercase();
    let dual = up.contains("SO") || up.contains("DIP");

    let mut out = Vec::new();
    let (body, per_side) = if dual {
        let s = n.div_ceil(2);
        (Rect::from_min_size(Pos2::ZERO, vec2(8.0, s as f32 + 1.5)), s)
    } else {
        let s = n.div_ceil(4);
        let size = s as f32 + 2.0;
        (Rect::from_min_size(Pos2::ZERO, vec2(size, size)), s)
    };
    let margin_y = (body.height() - per_side as f32) / 2.0;
    let margin_x = (body.width() - per_side as f32) / 2.0;
    let mut exposed = Vec::new();
    for (num, p) in pins {
        let (gpio, kind) = classify(&p.signals);
        if num > n {
            exposed.push((p, gpio, kind));
            continue;
        }
        let i = (num - 1) % per_side;
        let side_idx = (num - 1) / per_side;
        let side = if dual {
            [Side::Left, Side::Right][side_idx.min(1) as usize]
        } else {
            [Side::Left, Side::Bottom, Side::Right, Side::Top][side_idx.min(3) as usize]
        };
        let off = i as f32 + 0.5;
        let rect = match side {
            Side::Left => Rect::from_center_size(pos2(-PIN_LEN / 2.0, margin_y + off), vec2(PIN_LEN, PIN_W)),
            Side::Bottom => Rect::from_center_size(pos2(margin_x + off, body.bottom() + PIN_LEN / 2.0), vec2(PIN_W, PIN_LEN)),
            Side::Right => Rect::from_center_size(
                pos2(body.right() + PIN_LEN / 2.0, body.bottom() - margin_y - off),
                vec2(PIN_LEN, PIN_W),
            ),
            Side::Top | Side::Grid => Rect::from_center_size(pos2(body.right() - margin_x - off, -PIN_LEN / 2.0), vec2(PIN_W, PIN_LEN)),
        };
        out.push(PinGeom {
            position: p.position.clone(),
            signals: p.signals.clone(),
            gpio,
            kind,
            rect,
            side,
        });
    }
    // Exposed pads / extra positions are drawn inside the body.
    for (k, (p, gpio, kind)) in exposed.into_iter().enumerate() {
        let c = body.center() + vec2(0.0, 2.2 + k as f32 * 1.2);
        out.push(PinGeom {
            position: p.position.clone(),
            signals: p.signals.clone(),
            gpio,
            kind,
            rect: Rect::from_center_size(c, vec2(3.0, 0.9)),
            side: Side::Grid,
        });
    }
    let bounds = body.expand(PIN_LEN + LABEL_ROOM);
    Layout { pins: out, body, bounds, grid: false }
}

fn grid_layout(pkg: &Package) -> Layout {
    let split = |pos: &str| -> Option<(String, u32)> {
        let letters: String = pos.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        let num = pos[letters.len()..].parse().ok()?;
        Some((letters, num))
    };
    let mut rows: Vec<String> = pkg.pins.iter().filter_map(|p| split(&p.position).map(|s| s.0)).collect();
    rows.sort_by(|a, b| a.len().cmp(&b.len()).then(a.cmp(b)));
    rows.dedup();
    let cols = pkg.pins.iter().filter_map(|p| split(&p.position).map(|s| s.1)).max().unwrap_or(1);
    let pitch = 1.6;
    let body = Rect::from_min_size(Pos2::ZERO, vec2(cols as f32 * pitch + 1.0, rows.len() as f32 * pitch + 1.0));
    let pins = pkg
        .pins
        .iter()
        .filter_map(|p| {
            let (r, c) = split(&p.position)?;
            let ri = rows.iter().position(|x| *x == r)?;
            let center = pos2(0.5 + (c as f32 - 0.5) * pitch, 0.5 + (ri as f32 + 0.5) * pitch);
            let (gpio, kind) = classify(&p.signals);
            Some(PinGeom {
                position: p.position.clone(),
                signals: p.signals.clone(),
                gpio,
                kind,
                rect: Rect::from_center_size(center, vec2(1.2, 1.2)),
                side: Side::Grid,
            })
        })
        .collect();
    let bounds = Rect::from_min_max(body.min - vec2(2.0, 5.0), body.max + vec2(2.0, 2.0));
    Layout { pins, body, bounds, grid: true }
}

fn pin_name(p: &PinGeom) -> String {
    p.gpio.clone().unwrap_or_else(|| p.signals.first().cloned().unwrap_or_default())
}

impl Editor {
    fn pin_color(&self, p: &PinGeom, conflicts: &std::collections::BTreeSet<String>) -> Color32 {
        match (p.kind, &p.gpio) {
            (PinKind::Power, _) => theme::PIN_POWER,
            (PinKind::Special, _) => theme::PIN_SPECIAL,
            (PinKind::Gpio, Some(g)) => match self.project.pins.get(g) {
                _ if conflicts.contains(g) => theme::PIN_CONFLICT,
                Some(c) if matches!(c.mode, PinMode::Peripheral { .. }) => theme::PIN_PERIPH,
                Some(_) => theme::PIN_GPIO,
                None => theme::PIN_FREE,
            },
            _ => theme::PIN_FREE,
        }
    }

    pub(super) fn pinout(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        let Some(pkg) = chip.package(&self.project.package).or(chip.packages.first()) else {
            ui.label("This chip has no package data.");
            return;
        };
        let layout = build_layout(pkg);
        let conflicts = self.conflicts();

        // Search bar: pins, signals, peripherals and user labels.
        let mut search_enter = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.pin_search)
                    .hint_text("🔍 Find pin or peripheral (PA5, I2C1, LED)  Ctrl+F")
                    .desired_width(400.0),
            );
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
                r.request_focus();
            }
            search_enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.pin_search.is_empty() && ui.small_button("✕").on_hover_text("Clear search").clicked() {
                self.pin_search.clear();
            }
        });
        let query: Vec<String> = self.pin_search.split_whitespace().map(|t| t.to_ascii_uppercase()).collect();
        let hits: Option<Vec<usize>> = (!query.is_empty()).then(|| {
            layout
                .pins
                .iter()
                .enumerate()
                .filter(|(_, p)| query.iter().all(|t| self.pin_matches(p, t)))
                .map(|(i, _)| i)
                .collect()
        });
        if let Some(h) = &hits {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let n = h.len();
                let color = if n == 0 { theme::WARN } else { theme::TEXT_DIM };
                ui.label(RichText::new(format!("{n} matching pin{}", if n == 1 { "" } else { "s" })).size(11.5).color(color));
                // A peripheral name opens its configuration directly.
                let name = self.pin_search.trim().to_ascii_uppercase();
                if chip.peripheral(&name).is_some() && ui.small_button(format!("Configure {name}")).clicked() {
                    self.selected_periph = Some(name);
                }
            });
            if search_enter {
                if let Some(&i) = h.first() {
                    self.selected_pin = Some(layout.pins[i].position.clone());
                }
            }
        }
        let hit_set: std::collections::BTreeSet<usize> = hits.iter().flatten().copied().collect();

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
        draw_grid(&painter, rect);

        // View transform: fit to rect, then user zoom/pan.
        if self.pin_view.zoom == 0.0 || resp.double_clicked() {
            self.pin_view = PinView { zoom: 1.0, pan: Vec2::ZERO };
        }
        let fit = (rect.width() / layout.bounds.width()).min(rect.height() / layout.bounds.height()) * 0.96;
        if resp.hovered() {
            let (scroll, zoom_delta, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
            let factor = zoom_delta * (scroll / 400.0).exp();
            if factor != 1.0 {
                if let Some(hp) = hover {
                    let old = fit * self.pin_view.zoom;
                    let new_zoom = (self.pin_view.zoom * factor).clamp(0.4, 12.0);
                    let new = fit * new_zoom;
                    // Keep the point under the cursor fixed.
                    let center = rect.center() + self.pin_view.pan;
                    let model = (hp - center) / old;
                    self.pin_view.pan = hp - rect.center() - model * new;
                    self.pin_view.zoom = new_zoom;
                }
            }
        }
        if resp.dragged() {
            self.pin_view.pan += resp.drag_delta();
        }
        let scale = fit * self.pin_view.zoom;
        let origin = rect.center() + self.pin_view.pan - layout.bounds.center().to_vec2() * scale;
        let to_screen = |p: Pos2| origin + p.to_vec2() * scale;
        let rect_screen = |r: Rect| Rect::from_min_max(to_screen(r.min), to_screen(r.max));

        // Body
        let body = rect_screen(layout.body);
        painter.rect(body, 0.6 * scale, Color32::from_rgb(0x20, 0x23, 0x2a), Stroke::new(1.5, theme::BORDER), StrokeKind::Inside);
        if !layout.grid {
            painter.circle_filled(body.min + vec2(1.2, 1.2) * scale, 0.35 * scale, theme::TEXT_DIM);
        }
        let title_size = (body.width() / 14.0).clamp(10.0, 34.0);
        // Ball grids fill the body, so their title goes above it.
        let title_at = if layout.grid { body.center_top() - vec2(0.0, title_size * 1.6) } else { body.center() };
        painter.text(title_at - vec2(0.0, title_size * 0.8), Align2::CENTER_CENTER, &chip.name, FontId::proportional(title_size), theme::TEXT);
        painter.text(
            title_at + vec2(0.0, title_size * 0.4),
            Align2::CENTER_CENTER,
            format!("{}  ·  {}", pkg.package, pkg.name),
            FontId::monospace(title_size * 0.45),
            theme::TEXT_DIM,
        );

        // Candidate pins for the selected peripheral.
        let highlight: std::collections::BTreeSet<String> = self
            .selected_periph
            .as_ref()
            .and_then(|p| chip.peripheral(p))
            .map(|p| p.pins.iter().map(|x| x.pin.clone()).collect())
            .unwrap_or_default();

        let hover_pos = resp.hover_pos();
        let mut hovered: Option<usize> = None;
        let font = (scale * 0.5).clamp(5.0, 16.0);
        for (i, p) in layout.pins.iter().enumerate() {
            let r = rect_screen(p.rect);
            let is_hover = hover_pos.is_some_and(|h| r.expand(1.0).contains(h));
            if is_hover {
                hovered = Some(i);
            }
            let selected = self.selected_pin.as_deref() == Some(p.position.as_str());
            let is_hit = hit_set.contains(&i);
            let mut color = self.pin_color(p, &conflicts);
            if hits.is_some() && !is_hit {
                color = color.gamma_multiply(0.3);
            }
            let stroke = if selected {
                Stroke::new(2.0, Color32::WHITE)
            } else if is_hit {
                Stroke::new(2.5, theme::WARN)
            } else if is_hover {
                Stroke::new(1.5, theme::ACCENT)
            } else if p.gpio.as_ref().is_some_and(|g| highlight.contains(g)) {
                Stroke::new(1.5, theme::ACCENT)
            } else {
                Stroke::NONE
            };
            if layout.grid {
                painter.circle(r.center(), r.width() / 2.0, color, stroke);
                if font > 7.0 {
                    let name = pin_name(p);
                    let fs = (r.width() / name.len().max(3) as f32 * 1.5).min(font);
                    painter.text(r.center(), Align2::CENTER_CENTER, name, FontId::proportional(fs), Color32::from_rgb(0x15, 0x17, 0x1c));
                }
            } else {
                painter.rect(r, 1.5, color, stroke, StrokeKind::Outside);
                // Name inside the pin
                if font > 6.0 {
                    let angle = match p.side {
                        Side::Top => -FRAC_PI_2,
                        Side::Bottom => FRAC_PI_2,
                        _ => 0.0,
                    };
                    let name = pin_name(p);
                    let fs = font.min(r.width().max(r.height()) / (name.len().max(4) as f32 * 0.62));
                    rotated_text(&painter, r.center(), &name, fs, Color32::from_rgb(0x15, 0x17, 0x1c), angle, true);
                }
                // Assignment label outside the pin
                if let Some(cfg) = p.gpio.as_ref().and_then(|g| self.project.pins.get(g)) {
                    let text = if cfg.label.is_empty() { cfg.mode.label() } else { format!("{} [{}]", cfg.label, cfg.mode.label()) };
                    let gap = 0.4 * scale;
                    let (anchor_pos, angle) = match p.side {
                        Side::Left => (pos2(r.left() - gap, r.center().y), 0.0),
                        Side::Right => (pos2(r.right() + gap, r.center().y), 0.0),
                        Side::Top => (pos2(r.center().x, r.top() - gap), -FRAC_PI_2),
                        Side::Bottom => (pos2(r.center().x, r.bottom() + gap), FRAC_PI_2),
                        Side::Grid => (r.center(), 0.0),
                    };
                    let right_aligned = p.side == Side::Left;
                    outside_label(&painter, anchor_pos, &text, font.max(9.0), color, angle, right_aligned);
                }
            }
        }

        // Interaction
        if let Some(i) = hovered {
            let p = &layout.pins[i];
            if resp.clicked() {
                self.selected_pin = Some(p.position.clone());
                self.pin_popup = p.gpio.clone().map(PinPopup::functions);
            }
            if self.pin_popup.is_none() {
                let mut tip = format!("Pin {}  ·  {}", p.position, p.signals.join(" / "));
                if let Some(cfg) = p.gpio.as_ref().and_then(|g| self.project.pins.get(g)) {
                    tip.push_str(&format!("\n{}", cfg.mode.label()));
                    if !cfg.label.is_empty() {
                        tip.push_str(&format!("  “{}”", cfg.label));
                    }
                }
                resp.clone().on_hover_text_at_pointer(tip);
            }
        } else if resp.clicked() {
            self.selected_pin = None;
            self.pin_popup = None;
        }
        self.show_pin_popup(ui);

        // Help overlay
        painter.text(
            rect.left_bottom() + vec2(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            "Click a pin: choose its function, then name it  ·  Wheel: zoom  ·  Drag: pan  ·  Double-click: reset view",
            FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }

    /// Does a pin match one search token? Looks at position, package signals,
    /// routable peripheral functions, the assigned mode and the user label.
    fn pin_matches(&self, p: &PinGeom, token: &str) -> bool {
        if p.position.to_ascii_uppercase().contains(token) || p.signals.iter().any(|s| s.to_ascii_uppercase().contains(token)) {
            return true;
        }
        let Some(g) = &p.gpio else { return false };
        if let Some(cfg) = self.project.pins.get(g) {
            if cfg.label.to_ascii_uppercase().contains(token) || cfg.mode.label().contains(token) {
                return true;
            }
        }
        self.pin_functions.get(g).is_some_and(|fs| fs.iter().any(|f| f.label().contains(token)))
    }

    /// The open pin popup. Choosing a function moves straight on to the label
    /// prompt, so one click on a pin sets it up completely.
    pub(super) fn show_pin_popup(&mut self, ui: &mut egui::Ui) {
        let Some(mut pp) = self.pin_popup.take() else { return };
        let id = egui::Id::unique(("pin_popup", &pp.gpio, pp.kind));
        let opening = std::mem::take(&mut pp.opening);
        let gpio = pp.gpio.clone();
        let assigned = self.project.pins.get(&gpio).map(|c| c.mode.label());
        let mut choice: Option<Option<PinMode>> = None;
        let mut new_label: Option<String> = None;
        let (mut to_label, mut close) = (false, false);

        let shown = egui::Popup::new(id, ui.ctx().clone(), egui::PopupAnchor::PointerFixed, ui.layer_id())
            .kind(egui::PopupKind::Menu)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .style(egui::containers::menu::menu_style)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .open_memory(opening.then_some(egui::SetOpenCommand::Bool(true)))
            .show(|ui| match pp.kind {
                PinPopupKind::Functions => {
                    ui.set_min_width(230.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&gpio).strong().color(theme::ACCENT));
                        if assigned.is_some() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                to_label = ui.small_button("Label…").on_hover_text("Keep the function and edit the user label").clicked();
                            });
                        }
                    });
                    ui.separator();
                    choice = self.pin_function_list(ui, &gpio);
                }
                PinPopupKind::Label => {
                    ui.set_min_width(260.0);
                    ui.label(RichText::new(format!("{gpio}  ·  {}", assigned.as_deref().unwrap_or("Reset_State"))).strong().color(theme::ACCENT));
                    ui.separator();
                    ui.label(RichText::new("User label").color(theme::TEXT_DIM));
                    let r = ui.add(egui::TextEdit::singleline(&mut pp.label).hint_text("e.g. LED_GREEN").desired_width(240.0));
                    if opening {
                        r.request_focus();
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() || enter {
                            new_label = Some(pp.label.trim().to_string());
                        }
                        close = ui.button("Skip").on_hover_text("Leave the label as it is").clicked();
                    });
                    ui.label(RichText::new("Names the pin in the generated board.rs. Leave it empty for no label.").size(11.0).color(theme::TEXT_DIM));
                }
            });

        close |= shown.is_none() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        match choice {
            Some(Some(mode)) => {
                self.assign_pin(&gpio, Some(mode));
                to_label = true;
            }
            Some(None) => {
                self.assign_pin(&gpio, None);
                close = true;
            }
            None => {}
        }
        if let Some(l) = new_label {
            if let Some(cfg) = self.project.pins.get_mut(&gpio) {
                if cfg.label != l {
                    cfg.label = l;
                    self.mark_dirty();
                }
            }
            close = true;
        }
        if to_label {
            // Next step: the label prompt, opened where the pointer is now.
            egui::Popup::close_id(ui.ctx(), id);
            pp.kind = PinPopupKind::Label;
            pp.opening = true;
            pp.label = self.project.pins.get(&gpio).map(|c| c.label.clone()).unwrap_or_default();
            self.pin_popup = Some(pp);
        } else if close {
            egui::Popup::close_id(ui.ctx(), id);
        } else {
            self.pin_popup = Some(pp);
        }
    }

    /// All functions of a GPIO, CubeMX style; returns Some(choice) when one is clicked
    /// (`Some(None)` = Reset_State). The current function is highlighted.
    fn pin_function_list(&self, ui: &mut egui::Ui, gpio: &str) -> Option<Option<PinMode>> {
        let current = self.project.pins.get(gpio).map(|c| &c.mode);
        let mut out = None;
        if ui.selectable_label(current.is_none(), "Reset_State").clicked() {
            out = Some(None);
        }
        let line: String = gpio.chars().skip(2).collect();
        for (mode, text) in [
            (PinMode::GpioInput, "GPIO_Input".to_string()),
            (PinMode::GpioOutput, "GPIO_Output".to_string()),
            (PinMode::Analog, "GPIO_Analog".to_string()),
            (PinMode::ExtiInput, format!("GPIO_EXTI{line}")),
        ] {
            if ui.selectable_label(current == Some(&mode), text).clicked() {
                out = Some(Some(mode));
            }
        }
        let funcs = self.pin_functions.get(gpio).cloned().unwrap_or_default();
        if !funcs.is_empty() {
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for f in funcs {
                    let mode = PinMode::Peripheral { peripheral: f.peripheral.clone(), signal: f.signal.clone(), af: f.af };
                    let selected = matches!(current, Some(PinMode::Peripheral { peripheral, signal, .. }) if *peripheral == f.peripheral && *signal == f.signal);
                    let mut text = egui::text::LayoutJob::default();
                    text.append(&f.label(), 0.0, egui::TextFormat { color: theme::TEXT, ..Default::default() });
                    if let Some(a) = f.af {
                        text.append(&format!("   AF{a}"), 0.0, egui::TextFormat { color: theme::TEXT_DIM, font_id: FontId::monospace(11.0), ..Default::default() });
                    }
                    if ui.selectable_label(selected, text).clicked() {
                        out = Some(Some(mode));
                    }
                }
            });
        }
        out
    }

    pub(super) fn pin_inspector(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        ui.add_space(6.0);
        theme::heading(ui, "Pin");
        ui.add_space(4.0);
        let pkg = chip.package(&self.project.package).or(chip.packages.first());
        let pin = self
            .selected_pin
            .as_ref()
            .and_then(|pos| pkg?.pins.iter().find(|p| &p.position == pos))
            .cloned();
        match pin {
            None => {
                ui.label(RichText::new("Select a pin on the package to configure it.").color(theme::TEXT_DIM));
            }
            Some(p) => {
                let (gpio, _) = classify(&p.signals);
                egui::Grid::new("pin_info").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("Position").color(theme::TEXT_DIM));
                    ui.label(RichText::new(&p.position).monospace());
                    ui.end_row();
                    ui.label(RichText::new("Signals").color(theme::TEXT_DIM));
                    ui.label(RichText::new(p.signals.join(", ")).monospace());
                    ui.end_row();
                });
                if let Some(g) = gpio {
                    ui.add_space(8.0);
                    self.gpio_settings(ui, &g);
                } else {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Power / system pin — not configurable.").color(theme::TEXT_DIM));
                }
            }
        }

        ui.add_space(16.0);
        ui.separator();
        theme::section(ui, "Legend");
        for (c, t) in [
            (theme::PIN_FREE, "Free I/O"),
            (theme::PIN_GPIO, "GPIO"),
            (theme::PIN_PERIPH, "Peripheral function"),
            (theme::PIN_CONFLICT, "Conflict"),
            (theme::PIN_POWER, "Power"),
            (theme::PIN_SPECIAL, "Reset / boot / system"),
        ] {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                ui.painter().rect_filled(r, 3.0, c);
                ui.label(RichText::new(t).size(12.0));
            });
        }
    }

    pub(super) fn gpio_settings(&mut self, ui: &mut egui::Ui, gpio: &str) {
        let current = self.project.pins.get(gpio).cloned();
        let label = current.as_ref().map(|c| c.mode.label()).unwrap_or_else(|| "Reset_State".into());
        ui.horizontal(|ui| {
            ui.label(RichText::new(gpio).strong().size(16.0).color(theme::ACCENT));
        });
        ui.add_space(4.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mode").color(theme::TEXT_DIM));
            egui::ComboBox::from_id_salt(("pin_mode", gpio))
                .selected_text(label)
                .width(200.0)
                .height(420.0)
                .show_ui(ui, |ui| {
                    choice = self.pin_function_list(ui, gpio);
                });
        });
        if let Some(mode) = choice {
            self.assign_pin(gpio, mode);
        }
        let Some(mut cfg) = self.project.pins.get(gpio).cloned() else { return };

        ui.add_space(4.0);
        egui::Grid::new(("pin_cfg", gpio)).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label(RichText::new("Label").color(theme::TEXT_DIM));
            ui.add(egui::TextEdit::singleline(&mut cfg.label).hint_text("e.g. LED").desired_width(160.0));
            ui.end_row();
            match &cfg.mode {
                PinMode::GpioOutput => {
                    ui.label(RichText::new("Initial level").color(theme::TEXT_DIM));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut cfg.initial_high, false, "Low");
                        ui.radio_value(&mut cfg.initial_high, true, "High");
                    });
                    ui.end_row();
                    ui.label(RichText::new("Speed").color(theme::TEXT_DIM));
                    egui::ComboBox::from_id_salt(("speed", gpio))
                        .selected_text(format!("{:?}", cfg.speed))
                        .show_ui(ui, |ui| {
                            for s in [Speed::Low, Speed::Medium, Speed::High, Speed::VeryHigh] {
                                ui.selectable_value(&mut cfg.speed, s, format!("{s:?}"));
                            }
                        });
                    ui.end_row();
                }
                PinMode::GpioInput | PinMode::ExtiInput => {
                    ui.label(RichText::new("Pull").color(theme::TEXT_DIM));
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut cfg.pull, Pull::None, "None");
                        ui.radio_value(&mut cfg.pull, Pull::Up, "Up");
                        ui.radio_value(&mut cfg.pull, Pull::Down, "Down");
                    });
                    ui.end_row();
                }
                PinMode::Peripheral { peripheral, af, .. } => {
                    ui.label(RichText::new("Peripheral").color(theme::TEXT_DIM));
                    if ui.link(peripheral).clicked() {
                        self.selected_periph = Some(peripheral.clone());
                    }
                    ui.end_row();
                    if let Some(af) = af {
                        ui.label(RichText::new("Alternate fn").color(theme::TEXT_DIM));
                        ui.label(RichText::new(format!("AF{af}")).monospace());
                        ui.end_row();
                    }
                }
                PinMode::Analog => {}
            }
        });
        if self.project.pins.get(gpio) != Some(&cfg) {
            self.project.pins.insert(gpio.to_string(), cfg);
            self.mark_dirty();
        }
    }
}

pub(super) fn draw_grid(painter: &egui::Painter, rect: Rect) {
    let step = 24.0;
    let color = Color32::from_rgb(0x1c, 0x1f, 0x25);
    let mut x = rect.left();
    while x < rect.right() {
        let mut y = rect.top();
        while y < rect.bottom() {
            painter.circle_filled(pos2(x, y), 1.0, color);
            y += step;
        }
        x += step;
    }
}

/// Text centred on `center`, optionally rotated by ±90°.
fn rotated_text(painter: &egui::Painter, center: Pos2, text: &str, size: f32, color: Color32, angle: f32, _centered: bool) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(size), color);
    let sz = galley.size();
    let pos = if angle == 0.0 {
        center - sz / 2.0
    } else if angle < 0.0 {
        // Reading bottom-to-top.
        pos2(center.x - sz.y / 2.0, center.y + sz.x / 2.0)
    } else {
        pos2(center.x + sz.y / 2.0, center.y - sz.x / 2.0)
    };
    painter.add(egui::epaint::TextShape::new(pos, galley, color).with_angle(angle));
}

/// Label starting at `anchor` and running away from the chip.
fn outside_label(painter: &egui::Painter, anchor: Pos2, text: &str, size: f32, color: Color32, angle: f32, right_aligned: bool) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(size), color);
    let sz = galley.size();
    let pos = if angle == 0.0 {
        if right_aligned {
            pos2(anchor.x - sz.x, anchor.y - sz.y / 2.0)
        } else {
            pos2(anchor.x, anchor.y - sz.y / 2.0)
        }
    } else if angle < 0.0 {
        // Top side: text runs upward from the anchor.
        pos2(anchor.x - sz.y / 2.0, anchor.y)
    } else {
        // Bottom side: text runs downward.
        pos2(anchor.x + sz.y / 2.0, anchor.y)
    };
    painter.add(egui::epaint::TextShape::new(pos, galley, color).with_angle(angle));
}
