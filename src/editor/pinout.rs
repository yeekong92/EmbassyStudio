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
            let color = self.pin_color(p, &conflicts);
            let stroke = if selected {
                Stroke::new(2.0, Color32::WHITE)
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
            }
            if resp.secondary_clicked() {
                self.selected_pin = Some(p.position.clone());
                ui.ctx().memory_mut(|m| m.data.insert_temp(egui::Id::unique("pin_ctx"), p.gpio.clone()));
            }
            let mut tip = format!("Pin {}  ·  {}", p.position, p.signals.join(" / "));
            if let Some(cfg) = p.gpio.as_ref().and_then(|g| self.project.pins.get(g)) {
                tip.push_str(&format!("\n{}", cfg.mode.label()));
                if !cfg.label.is_empty() {
                    tip.push_str(&format!("  “{}”", cfg.label));
                }
            }
            if !resp.context_menu_opened() {
                resp.clone().on_hover_text_at_pointer(tip);
            }
        } else if resp.clicked() {
            self.selected_pin = None;
        }

        let ctx_pin: Option<String> = ui.ctx().memory(|m| m.data.get_temp::<Option<String>>(egui::Id::unique("pin_ctx"))).flatten();
        if let Some(gpio) = ctx_pin {
            resp.context_menu(|ui| {
                ui.set_min_width(200.0);
                ui.label(RichText::new(&gpio).strong().color(theme::ACCENT));
                ui.separator();
                if let Some(mode) = self.mode_menu(ui, &gpio) {
                    self.assign_pin(&gpio, mode);
                    ui.close();
                }
            });
        }

        // Help overlay
        painter.text(
            rect.left_bottom() + vec2(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            "Click: select  ·  Right-click: assign  ·  Wheel: zoom  ·  Drag: pan  ·  Double-click: reset view",
            FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }

    /// Menu with all modes for a GPIO; returns Some(choice) when clicked (None = reset).
    fn mode_menu(&self, ui: &mut egui::Ui, gpio: &str) -> Option<Option<PinMode>> {
        let mut out = None;
        if ui.button("Reset (unused)").clicked() {
            out = Some(None);
        }
        for (mode, text) in [
            (PinMode::GpioOutput, "GPIO_Output"),
            (PinMode::GpioInput, "GPIO_Input"),
            (PinMode::ExtiInput, "GPIO_EXTI (interrupt)"),
            (PinMode::Analog, "GPIO_Analog"),
        ] {
            if ui.button(text).clicked() {
                out = Some(Some(mode));
            }
        }
        let funcs = self.pin_functions.get(gpio).cloned().unwrap_or_default();
        if !funcs.is_empty() {
            ui.separator();
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for f in funcs {
                    let af = f.af.map(|a| format!("  AF{a}")).unwrap_or_default();
                    if ui.button(format!("{}{af}", f.label())).clicked() {
                        out = Some(Some(PinMode::Peripheral { peripheral: f.peripheral, signal: f.signal, af: f.af }));
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

    fn gpio_settings(&mut self, ui: &mut egui::Ui, gpio: &str) {
        let current = self.project.pins.get(gpio).cloned();
        let label = current.as_ref().map(|c| c.mode.label()).unwrap_or_else(|| "Reset (unused)".into());
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
                    choice = self.mode_menu(ui, gpio);
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

fn draw_grid(painter: &egui::Painter, rect: Rect) {
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
