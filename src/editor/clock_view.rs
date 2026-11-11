//! Clock configuration map: oscillators → PLL → SYSCLK mux → AHB → APB buses.

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, pos2, vec2};

use super::Editor;
use crate::clock::{Role, Selector, factor, format_hz, oscillator_base};
use crate::theme;

const NODE_H: f32 = 64.0;

/// Friendly display for a variant: `Div4` -> `/4`, `Mul168` -> `×168`.
fn pretty(variant: &str) -> String {
    match factor(variant) {
        Some(f) if f >= 1.0 && variant.to_ascii_lowercase().starts_with("mul") => format!("×{}", trim(f)),
        Some(f) if f > 0.0 => format!("/{}", trim(1.0 / f)),
        _ => variant.to_string(),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn wire(out: &mut Vec<egui::Shape>, from: Pos2, to: Pos2, active: bool) {
    let dx = ((to.x - from.x) * 0.5).max(30.0);
    let color = if active { theme::ACCENT } else { Color32::from_rgb(0x3a, 0x40, 0x4b) };
    let width = if active { 2.2 } else { 1.2 };
    let shape = egui::epaint::CubicBezierShape::from_points_stroke(
        [from, from + vec2(dx, 0.0), to - vec2(dx, 0.0), to],
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    );
    out.push(shape.into());
    out.push(egui::Shape::circle_filled(to, if active { 3.0 } else { 2.0 }, color));
}

fn node(painter: &egui::Painter, rect: Rect, title: &str, active: bool) {
    painter.rect(
        rect,
        6.0,
        if active { theme::CARD } else { Color32::from_rgb(0x1e, 0x21, 0x27) },
        Stroke::new(1.0, if active { theme::ACCENT_DIM } else { theme::BORDER }),
        StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + vec2(10.0, 7.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(11.5),
        if active { theme::TEXT_DIM } else { Color32::from_rgb(0x5d, 0x64, 0x70) },
    );
}

impl Editor {
    /// A combo box for a selector placed at `rect`; returns the new value if changed.
    fn selector_combo(ui: &mut egui::Ui, rect: Rect, sel: &Selector, current: &str) -> Option<String> {
        let mut value = current.to_string();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            egui::ComboBox::from_id_salt(("clk", sel.key()))
                .width(rect.width())
                .height(400.0)
                .selected_text(RichText::new(pretty(current)).monospace())
                .show_ui(ui, |ui| {
                    for v in &sel.variants {
                        ui.selectable_value(&mut value, v.name.clone(), RichText::new(pretty(&v.name)).monospace())
                            .on_hover_text(v.description.clone().unwrap_or_else(|| v.name.clone()));
                    }
                });
        });
        (value != current).then_some(value)
    }

    pub(super) fn clock_view(&mut self, ui: &mut egui::Ui) {
        let Some(tree) = self.tree.clone() else {
            ui.centered_and_justified(|ui| ui.label("Loading RCC description…"));
            return;
        };
        if tree.sys.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(format!("No system clock switch found in RCC '{}' data.", tree.rcc_version))
            });
            return;
        }
        let res = self.clocks.clone();
        let clk = self.project.clock.clone();
        let mut changed = false;

        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            let oscs: Vec<String> = tree.oscillators.clone();
            let osc_h = 92.0;
            let height = (oscs.len() as f32 * osc_h + 120.0).max(520.0);
            let avail = ui.available_width();
            let width = avail.max(1160.0);
            let (canvas, _) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
            let painter = ui.painter_at(canvas);
            // Wires go underneath the nodes: reserve a slot now, fill it at the end.
            let wire_slot = painter.add(egui::Shape::Noop);
            let mut wires: Vec<egui::Shape> = Vec::new();
            let o = canvas.min.to_vec2() + vec2(20.0, 50.0);
            // Columns right of the oscillators are compressed to fit narrower windows.
            let sx = ((avail - 40.0 - 220.0) / 1240.0).clamp(0.72, 1.0);
            let cx = |x: f32| if x < 220.0 { x } else { 220.0 + (x - 220.0) * sx };
            let at = |x: f32, y: f32, w: f32, h: f32| {
                let w = if x < 220.0 { w } else { w * sx };
                Rect::from_min_size(pos2(cx(x), y) + o, vec2(w, h))
            };

            // Column headers
            for (x, t) in [(0.0, "OSCILLATORS"), (250.0, "PLL"), (900.0, "SYSTEM CLOCK"), (1090.0, "AHB"), (1270.0, "APB BUSES")] {
                painter.text(pos2(cx(x), -32.0) + o, Align2::LEFT_TOP, t, FontId::proportional(11.0), theme::TEXT_DIM);
            }

            // ---- Oscillators ----
            let mut osc_out = std::collections::BTreeMap::new();
            for (i, name) in oscs.iter().enumerate() {
                let r = at(0.0, i as f32 * osc_h, 200.0, osc_h - 14.0);
                let enabled = clk.is_enabled(name);
                node(&painter, r, name, enabled);
                osc_out.insert(name.clone(), pos2(r.right(), r.center().y));
                let mut en = enabled;
                ui.put(Rect::from_min_size(r.right_top() + vec2(-28.0, 4.0), vec2(22.0, 20.0)), egui::Checkbox::without_text(&mut en))
                    .on_hover_text("Enable oscillator");
                if en != enabled {
                    self.project.clock.enabled.insert(name.clone(), en);
                    changed = true;
                }
                let mut mhz = clk.oscillators.get(name).copied().unwrap_or(0.0) / 1e6;
                let resp = ui.put(
                    Rect::from_min_size(r.left_top() + vec2(10.0, 28.0), vec2(110.0, 24.0)),
                    egui::DragValue::new(&mut mhz).range(0.0..=200.0).speed(0.01).suffix(" MHz").max_decimals(6),
                );
                if resp.changed() {
                    self.project.clock.oscillators.insert(name.clone(), mhz * 1e6);
                    changed = true;
                }
                if name.starts_with("HSE") {
                    let mut byp = clk.hse_bypass;
                    ui.put(Rect::from_min_size(r.left_top() + vec2(126.0, 30.0), vec2(70.0, 20.0)), egui::Checkbox::new(&mut byp, "Bypass"));
                    if byp != clk.hse_bypass {
                        self.project.clock.hse_bypass = byp;
                        changed = true;
                    }
                }
            }

            // ---- PLL ----
            let pll_on = clk.pll_enabled;
            let pll_group = at(240.0, -8.0, 620.0, 300.0);
            if tree.has_pll() {
                painter.rect_stroke(pll_group, 10.0, Stroke::new(1.0, if pll_on { theme::ACCENT_DIM } else { theme::BORDER }), StrokeKind::Inside);
                let mut on = pll_on;
                ui.put(Rect::from_min_size(pll_group.left_top() + vec2(40.0, -30.0), vec2(120.0, 22.0)), egui::Checkbox::new(&mut on, "Enable PLL"));
                if on != pll_on {
                    self.project.clock.pll_enabled = on;
                    changed = true;
                }
            } else {
                painter.text(pll_group.left_top() + vec2(10.0, 10.0), Align2::LEFT_TOP, "No PLL in this RCC", FontId::proportional(12.0), theme::TEXT_DIM);
            }
            let mut pll_outputs: Vec<(char, Pos2)> = Vec::new();
            if tree.has_pll() {
                let src_rect = at(255.0, 20.0, 150.0, NODE_H);
                let m_rect = at(425.0, 20.0, 120.0, NODE_H);
                let n_rect = at(565.0, 20.0, 130.0, NODE_H + 18.0);
                // Source mux
                if let Some(sel) = &tree.pll_src {
                    node(&painter, src_rect, "Source", pll_on);
                    let cur = clk.role(Role::PllSrc).unwrap_or("");
                    if let Some(v) = Self::selector_combo(ui, src_rect.shrink2(vec2(8.0, 0.0)).translate(vec2(0.0, 26.0)), sel, cur) {
                        self.project.clock.roles.insert(Role::PllSrc, v);
                        changed = true;
                    }
                    for v in &sel.variants {
                        if let Some(from) = oscillator_base(&v.name).and_then(|b| osc_out.get(&b)) {
                            wire(&mut wires, *from, pos2(src_rect.left(), src_rect.center().y), pll_on && v.name == cur);
                        }
                    }
                }
                let mut prev = pos2(src_rect.right(), src_rect.center().y);
                for (role, rect, title) in [(Role::PllM, m_rect, "Prediv /M"), (Role::PllN, n_rect, "Multiplier ×N")] {
                    if let Some(sel) = tree.selector(role) {
                        node(&painter, rect, title, pll_on);
                        wire(&mut wires, prev, pos2(rect.left(), rect.top() + NODE_H / 2.0), pll_on);
                        let cur = clk.role(role).unwrap_or("");
                        let combo = Rect::from_min_size(rect.left_top() + vec2(8.0, 26.0), vec2(rect.width() - 16.0, 24.0));
                        if let Some(v) = Self::selector_combo(ui, combo, sel, cur) {
                            self.project.clock.roles.insert(role, v);
                            changed = true;
                        }
                        prev = pos2(rect.right(), rect.top() + NODE_H / 2.0);
                    }
                }
                painter.text(n_rect.left_bottom() + vec2(10.0, -6.0), Align2::LEFT_BOTTOM, format!("VCO {}", format_hz(res.vco)), FontId::monospace(11.0), if pll_on { theme::ACCENT } else { theme::TEXT_DIM });
                if tree.pll_m.is_some() {
                    painter.text(m_rect.left_bottom() + vec2(10.0, 14.0), Align2::LEFT_TOP, format!("in {}", format_hz(res.pll_in)), FontId::monospace(11.0), theme::TEXT_DIM);
                }

                // Outputs P/Q/R
                let outs = [(Role::PllP, 'P', res.pll_p), (Role::PllQ, 'Q', res.pll_q), (Role::PllR, 'R', res.pll_r)];
                let mut y = 0.0;
                for (role, letter, freq) in outs {
                    let Some(sel) = tree.selector(role) else { continue };
                    let rect = at(720.0, y, 125.0, NODE_H + 10.0);
                    let out_on = pll_on && clk.pll_output_enabled(role);
                    node(&painter, rect, &format!("Output {letter}"), out_on);
                    wire(&mut wires, prev, pos2(rect.left(), rect.top() + NODE_H / 2.0), out_on);
                    let mut en = clk.pll_output_enabled(role);
                    let toggle = ui
                        .put(Rect::from_min_size(rect.right_top() + vec2(-28.0, 3.0), vec2(22.0, 20.0)), egui::Checkbox::without_text(&mut en))
                        .on_hover_text("Enable this PLL output (always on when it drives SYSCLK)");
                    if toggle.changed() {
                        self.project.clock.pll_outputs.insert(role, en);
                        changed = true;
                    }
                    let combo = Rect::from_min_size(rect.left_top() + vec2(8.0, 24.0), vec2(rect.width() - 16.0, 24.0));
                    let cur = clk.role(role).unwrap_or("");
                    if let Some(v) = Self::selector_combo(ui, combo, sel, cur) {
                        self.project.clock.roles.insert(role, v);
                        changed = true;
                    }
                    painter.text(rect.left_bottom() + vec2(10.0, -5.0), Align2::LEFT_BOTTOM, format_hz(freq.unwrap_or(0.0)), FontId::monospace(11.0), if pll_on { theme::ACCENT } else { theme::TEXT_DIM });
                    pll_outputs.push((letter, pos2(rect.right(), rect.top() + NODE_H / 2.0)));
                    y += NODE_H + 22.0;
                }
                if pll_outputs.is_empty() {
                    // F0/F1 style: the multiplier output is the PLL clock.
                    pll_outputs.push(('P', prev));
                }
            }

            // ---- SYSCLK mux ----
            let sys_sel = tree.sys.clone().unwrap();
            let sys_rect = at(900.0, 60.0, 160.0, NODE_H + 18.0);
            node(&painter, sys_rect, "SYSCLK source", true);
            let sys_cur = clk.role(Role::Sys).unwrap_or("").to_string();
            for v in &sys_sel.variants {
                let to = pos2(sys_rect.left(), sys_rect.top() + NODE_H / 2.0);
                let from = if v.name.starts_with("PLL") {
                    let letter = v.name.rsplit('_').next().and_then(|s| s.chars().next()).unwrap_or('P');
                    pll_outputs.iter().find(|(l, _)| *l == letter).or(pll_outputs.first()).map(|(_, p)| *p)
                } else {
                    oscillator_base(&v.name).and_then(|b| osc_out.get(&b).copied())
                };
                if let Some(from) = from {
                    wire(&mut wires, from, to, v.name == sys_cur);
                }
            }
            let combo = Rect::from_min_size(sys_rect.left_top() + vec2(8.0, 26.0), vec2(sys_rect.width() - 16.0, 24.0));
            if let Some(v) = Self::selector_combo(ui, combo, &sys_sel, &sys_cur) {
                if v.starts_with("PLL") && !self.project.clock.pll_enabled {
                    self.project.clock.pll_enabled = true;
                }
                self.project.clock.roles.insert(Role::Sys, v);
                changed = true;
            }
            painter.text(sys_rect.left_bottom() + vec2(10.0, -6.0), Align2::LEFT_BOTTOM, format!("SYSCLK {}", format_hz(res.sysclk)), FontId::monospace(12.0), theme::ACCENT);

            // ---- AHB ----
            let ahb_rect = at(1090.0, 60.0, 150.0, NODE_H + 18.0);
            node(&painter, ahb_rect, "AHB prescaler", true);
            wire(&mut wires, pos2(sys_rect.right(), sys_rect.top() + NODE_H / 2.0), pos2(ahb_rect.left(), ahb_rect.top() + NODE_H / 2.0), true);
            if let Some(sel) = &tree.ahb {
                let combo = Rect::from_min_size(ahb_rect.left_top() + vec2(8.0, 26.0), vec2(ahb_rect.width() - 16.0, 24.0));
                let cur = clk.role(Role::Ahb).unwrap_or("");
                if let Some(v) = Self::selector_combo(ui, combo, sel, cur) {
                    self.project.clock.roles.insert(Role::Ahb, v);
                    changed = true;
                }
            }
            painter.text(ahb_rect.left_bottom() + vec2(10.0, -6.0), Align2::LEFT_BOTTOM, format!("HCLK {}", format_hz(res.hclk)), FontId::monospace(12.0), theme::ACCENT);

            // ---- APB ----
            for (i, sel) in tree.apb.iter().enumerate() {
                let rect = at(1270.0, i as f32 * (NODE_H + 26.0), 180.0, NODE_H + 14.0);
                let (bus, freq) = res
                    .apb
                    .iter()
                    .find(|(_, f, _)| *f == sel.field)
                    .map(|(b, _, f)| (b.clone(), *f))
                    .unwrap_or_default();
                node(&painter, rect, &format!("{} → {bus}", sel.field), true);
                wire(&mut wires, pos2(ahb_rect.right(), ahb_rect.top() + NODE_H / 2.0), pos2(rect.left(), rect.top() + NODE_H / 2.0), true);
                let combo = Rect::from_min_size(rect.left_top() + vec2(8.0, 24.0), vec2(90.0, 24.0));
                let cur = clk.apb.get(&sel.field).cloned().unwrap_or_default();
                if let Some(v) = Self::selector_combo(ui, combo, sel, &cur) {
                    self.project.clock.apb.insert(sel.field.clone(), v);
                    changed = true;
                }
                painter.text(rect.left_bottom() + vec2(10.0, -5.0), Align2::LEFT_BOTTOM, format!("{bus} {}", format_hz(freq)), FontId::monospace(12.0), theme::ACCENT);
            }

            painter.set(wire_slot, egui::Shape::Vec(wires));

            // Footer: data provenance
            painter.text(
                pos2(canvas.left() + 20.0, canvas.bottom() - 14.0),
                Align2::LEFT_BOTTOM,
                format!(
                    "Clock tree discovered from stm32-data registers/rcc_{}.json · oscillator frequencies are editable nominal values — check your datasheet for limits",
                    tree.rcc_version
                ),
                FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
        });

        if changed {
            self.mark_dirty();
        }
    }
}
