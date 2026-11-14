//! Clock configuration, laid out like STM32CubeMX:
//! input sources → PLLs → system clock mux → AHB/APB buses → outputs, then clock
//! outputs (MCO, RTC, LSCO) and every peripheral kernel-clock mux below.
//!
//! Everything is driven by the RCC description discovered in `crate::clock`, so the
//! same view serves every STM32 family.

use std::collections::BTreeMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, pos2, vec2};

use super::Editor;
use crate::clock::{ClockConfig, ClockTree, Role, Selector, factor, format_hz, oscillator_base};
use crate::db::KernelClock;
use crate::theme;

// Column positions (canvas units).
const OSC_X: f32 = 0.0;
const OSC_W: f32 = 190.0;
const PLL_X: f32 = 218.0;
const PLL_W: f32 = 600.0;
const SYS_X: f32 = 848.0;
const AHB_X: f32 = 1020.0;
const OUT_X: f32 = 1182.0;
const OUT_W: f32 = 226.0;
const GRID_X: f32 = 848.0;
const GRID_W: f32 = 180.0;
const GRID_COLS: usize = 3;
const CANVAS_W: f32 = 1412.0;

const DIM_WIRE: Color32 = Color32::from_rgb(0x3a, 0x40, 0x4b);
const OFF_FILL: Color32 = Color32::from_rgb(0x1e, 0x21, 0x27);
const OFF_TEXT: Color32 = Color32::from_rgb(0x5d, 0x64, 0x70);

/// Friendly display for a variant: `Div4` -> `/4`, `Mul168` -> `×168`, `Range4M` -> `4 MHz`.
fn pretty(variant: &str) -> String {
    match factor(variant) {
        Some(f) if f >= 1.0 && variant.to_ascii_lowercase().starts_with("mul") => format!("×{}", trim(f)),
        Some(f) if f > 0.0 => format!("/{}", trim(1.0 / f)),
        _ => variant.strip_prefix("Range").map(|r| r.replace('K', " kHz").replace('M', " MHz")).unwrap_or_else(|| variant.to_string()),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn wire(out: &mut Vec<egui::Shape>, from: Pos2, to: Pos2, active: bool) {
    let dx = ((to.x - from.x) * 0.5).max(30.0);
    let color = if active { theme::ACCENT } else { DIM_WIRE };
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
        if active { theme::CARD } else { OFF_FILL },
        Stroke::new(1.0, if active { theme::ACCENT_DIM } else { theme::BORDER }),
        StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + vec2(10.0, 7.0),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(11.5),
        if active { theme::TEXT_DIM } else { OFF_TEXT },
    );
}

/// Frequency read-out at the bottom-left of a node.
fn freq(painter: &egui::Painter, rect: Rect, label: &str, hz: f64, active: bool) {
    let text = if label.is_empty() { format_hz(hz) } else { format!("{label} {}", format_hz(hz)) };
    painter.text(
        rect.left_bottom() + vec2(10.0, -7.0),
        Align2::LEFT_BOTTOM,
        text,
        FontId::monospace(11.5),
        if active && hz > 0.0 { theme::ACCENT } else { theme::TEXT_DIM },
    );
}

fn section_title(painter: &egui::Painter, pos: Pos2, text: &str) {
    painter.text(pos, Align2::LEFT_BOTTOM, text, FontId::proportional(11.0), theme::TEXT_DIM);
}

/// Input of a node: middle of its left edge, at the height of the title row.
fn input(rect: Rect) -> Pos2 {
    pos2(rect.left(), rect.top() + 34.0)
}

fn output(rect: Rect) -> Pos2 {
    pos2(rect.right(), rect.top() + 34.0)
}

/// What to change in the project after drawing a frame.
enum Edit {
    Role(Role, String),
    Apb(String, String),
    Value(Selector, String),
    PllEnabled(bool),
    PllOutput(Role, bool),
    AuxEnabled(String, bool),
    OscEnabled(String, bool),
    OscFreq(String, f64),
    HseBypass(bool),
}

/// Everything needed to draw one PLL (main or extra) the same way.
struct PllDraw<'a> {
    title: String,
    name: String,
    enabled: bool,
    src: Option<(&'a Selector, String)>,
    m: Option<(&'a Selector, String)>,
    n: (&'a Selector, String),
    /// (letter, selector, current value, output active, frequency, main-PLL role)
    outs: Vec<(char, &'a Selector, String, bool, Option<f64>, Option<Role>)>,
    pll_in: f64,
    vco: f64,
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
        })
        .response
        .on_hover_text(format!("{}  ({})", sel.key(), sel.enum_name));
        (value != current).then_some(value)
    }

    fn apply_clock_edit(&mut self, tree: &ClockTree, edit: Edit) {
        let clk = &mut self.project.clock;
        match edit {
            Edit::Role(role, v) => {
                if role == Role::Sys && v.starts_with("PLL") {
                    clk.pll_enabled = true;
                }
                clk.roles.insert(role, v);
            }
            Edit::Apb(field, v) => {
                clk.apb.insert(field, v);
            }
            Edit::Value(sel, v) => {
                // Picking a PLL output anywhere switches that PLL on, as CubeMX does.
                let main = tree.main_pll.trim_end_matches('1');
                if v.starts_with(&format!("{main}_")) || v.starts_with(&format!("{}_", tree.main_pll)) || v == "PLL" {
                    clk.pll_enabled = true;
                }
                for pll in &tree.aux_plls {
                    if v.starts_with(&format!("{}_", pll.name)) {
                        clk.aux_pll_enabled.insert(pll.name.clone(), true);
                    }
                }
                clk.set_value(&sel, v);
            }
            Edit::PllEnabled(on) => clk.pll_enabled = on,
            Edit::PllOutput(role, on) => {
                clk.pll_outputs.insert(role, on);
            }
            Edit::AuxEnabled(name, on) => {
                clk.aux_pll_enabled.insert(name, on);
            }
            Edit::OscEnabled(name, on) => {
                clk.enabled.insert(name, on);
            }
            Edit::OscFreq(name, hz) => {
                clk.oscillators.insert(name, hz);
            }
            Edit::HseBypass(on) => clk.hse_bypass = on,
        }
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
        let mut edits: Vec<Edit> = Vec::new();
        self.clock_toolbar(ui, &tree);
        egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
            self.clock_canvas(ui, &tree, &mut edits);
        });
        if !edits.is_empty() {
            for e in edits {
                self.apply_clock_edit(&tree, e);
            }
            self.mark_dirty();
        }
    }

    fn clock_toolbar(&mut self, ui: &mut egui::Ui, tree: &ClockTree) {
        let res = self.clocks.clone();
        let mut reset = false;
        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let chip = |ui: &mut egui::Ui, name: &str, hz: f64| {
                    ui.label(RichText::new(name).size(11.5).color(theme::TEXT_DIM));
                    ui.label(RichText::new(format_hz(hz)).monospace().strong().color(theme::ACCENT));
                    ui.add_space(10.0);
                };
                chip(ui, "SYSCLK", res.sysclk);
                chip(ui, "HCLK", res.hclk);
                for (bus, _, f) in &res.apb {
                    chip(ui, bus, *f);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    reset = ui.button("Reset clocks").on_hover_text("Back to the reset configuration of the RCC").clicked();
                    let (txt, color) = match crate::codegen::rcc_flavor(tree) {
                        crate::codegen::RccFlavor::SummaryOnly => (format!("RCC '{}': written to board.rs as comments", tree.rcc_version), theme::WARN),
                        _ => (format!("RCC '{}': clock code generated", tree.rcc_version), theme::TEXT_DIM),
                    };
                    ui.label(RichText::new(txt).size(11.0).color(color));
                });
            });
        });
        ui.separator();
        if reset {
            self.project.clock = ClockConfig::default();
            self.project.clock.normalize(tree);
            self.mark_dirty();
        }
    }

    fn clock_canvas(&self, ui: &mut egui::Ui, tree: &ClockTree, edits: &mut Vec<Edit>) {
        let res = &self.clocks;
        let clk = &self.project.clock;

        // Peripheral users of every mux, for titles and highlighting.
        let mut users: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        if let Some(chip) = &self.chip {
            for p in chip.peripherals() {
                if let Some(KernelClock::Mux(m)) = p.rcc.as_ref().and_then(|r| r.kernel_clock.as_ref()) {
                    users
                        .entry(format!("{}.{}", m.register, m.field))
                        .or_default()
                        .push((p.name.clone(), self.project.is_peripheral_enabled(&p.name)));
                }
            }
        }
        let kernel: Vec<&Selector> = tree.muxes.iter().filter(|(k, _)| !ClockTree::is_system_mux(k)).map(|(_, s)| s).collect();
        let outputs = tree.mcos.len() + tree.misc.len()
            + tree.muxes.keys().filter(|k| k.ends_with(".RTCSEL") || k.ends_with(".LSCOSEL")).count();

        // ---- size the canvas ----
        let osc_step = 108.0;
        let osc_bottom = tree.oscillators.len() as f32 * osc_step;
        let pll_h = |outs: usize| 44.0 + (outs.max(1) as f32 * 82.0).max(110.0);
        let main_outs = [&tree.pll_p, &tree.pll_q, &tree.pll_r].iter().filter(|s| s.is_some()).count();
        let mut pll_bottom = if tree.has_pll() { pll_h(main_outs) } else { 60.0 };
        for p in &tree.aux_plls {
            pll_bottom += 30.0 + pll_h([&p.p, &p.q, &p.r].iter().filter(|s| s.is_some()).count());
        }
        let apb_top = 160.0;
        let right_bottom = apb_top + tree.apb.len() as f32 * 112.0;
        let rows = |n: usize| n.div_ceil(GRID_COLS) as f32 * 96.0;
        let grid_top = right_bottom.max(230.0) + 50.0;
        let kernel_top = grid_top + rows(outputs) + 50.0;
        let grid_bottom = kernel_top + rows(kernel.len());
        let height = osc_bottom.max(pll_bottom).max(grid_bottom) + 90.0;

        let (canvas, _) = ui.allocate_exact_size(vec2(CANVAS_W + 28.0, height), egui::Sense::hover());
        let painter = ui.painter_at(canvas);
        // Wires go underneath the nodes: reserve a slot now, fill it at the end.
        let wire_slot = painter.add(egui::Shape::Noop);
        let mut wires: Vec<egui::Shape> = Vec::new();
        let o = canvas.min.to_vec2() + vec2(16.0, 44.0);
        let at = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y) + o, vec2(w, h));
        let put = |x: f32, y: f32| pos2(x, y) + o;

        for (x, t) in [(OSC_X, "INPUT SOURCES"), (PLL_X, "PLLs"), (SYS_X, "SYSTEM CLOCK MUX"), (AHB_X, "AHB"), (OUT_X, "BUS CLOCKS")] {
            section_title(&painter, put(x, -14.0), t);
        }

        // ---- input sources ----
        let mut osc_out: BTreeMap<String, Pos2> = BTreeMap::new();
        for (i, name) in tree.oscillators.iter().enumerate() {
            let r = at(OSC_X, i as f32 * osc_step, OSC_W, osc_step - 12.0);
            let enabled = clk.is_enabled(name);
            let external = name.starts_with("HSE") || name.starts_with("LSE");
            let kind = if external { "external" } else { "RC" };
            node(&painter, r, &format!("{name}  ·  {kind}"), enabled);
            osc_out.insert(name.clone(), output(r));
            let mut en = enabled;
            if ui
                .put(Rect::from_min_size(r.right_top() + vec2(-28.0, 4.0), vec2(22.0, 20.0)), egui::Checkbox::without_text(&mut en))
                .on_hover_text("Enable this source")
                .changed()
            {
                edits.push(Edit::OscEnabled(name.clone(), en));
            }
            let row1 = Rect::from_min_size(r.left_top() + vec2(10.0, 28.0), vec2(120.0, 24.0));
            let f_now = clk.osc_frequency(tree, name);
            if let Some(sel) = tree.osc_range.get(name) {
                if let Some(v) = Self::selector_combo(ui, row1, sel, clk.value(sel)) {
                    edits.push(Edit::Value(sel.clone(), v));
                }
            } else {
                let mut mhz = clk.oscillators.get(name).copied().unwrap_or(0.0) / 1e6;
                let drag = egui::DragValue::new(&mut mhz).range(0.0..=200.0).speed(0.01).suffix(" MHz").max_decimals(6);
                let resp = ui.put(row1, drag).on_hover_text(if external {
                    "Frequency of the crystal / clock on your board"
                } else {
                    "Nominal frequency of the internal RC oscillator"
                });
                if resp.changed() {
                    edits.push(Edit::OscFreq(name.clone(), mhz * 1e6));
                }
            }
            let row2 = Rect::from_min_size(r.left_top() + vec2(10.0, 56.0), vec2(110.0, 22.0));
            if name.starts_with("HSE") {
                let mut byp = clk.hse_bypass;
                if ui.put(row2, egui::Checkbox::new(&mut byp, "Bypass")).on_hover_text("External clock signal instead of a crystal").changed() {
                    edits.push(Edit::HseBypass(byp));
                }
            } else if name.starts_with("LSE") {
                if let Some(sel) = &tree.lse_drive {
                    painter.text(row2.left_center(), Align2::LEFT_CENTER, "Drive", FontId::proportional(11.0), theme::TEXT_DIM);
                    if let Some(v) = Self::selector_combo(ui, row2.translate(vec2(40.0, 0.0)), sel, clk.value(sel)) {
                        edits.push(Edit::Value(sel.clone(), v));
                    }
                }
            }
            if let Some(sel) = tree.osc_div.get(name) {
                if let Some(v) = Self::selector_combo(ui, row2.with_max_x(row2.left() + 70.0), sel, clk.value(sel)) {
                    edits.push(Edit::Value(sel.clone(), v));
                }
            }
            // Resulting frequency in the title row, left of the enable box.
            painter.text(
                r.right_top() + vec2(-34.0, 7.0),
                Align2::RIGHT_TOP,
                format_hz(f_now),
                FontId::monospace(11.5),
                if enabled { theme::ACCENT } else { theme::TEXT_DIM },
            );
        }

        // ---- PLLs ----
        let mut y = 0.0;
        let mut main_outputs: Vec<(char, Pos2)> = Vec::new();
        let mut main_src_out: Option<Pos2> = None;
        if tree.has_pll() {
            let n = tree.pll_n.as_ref().unwrap();
            let outs = [(Role::PllP, 'P', res.pll_p), (Role::PllQ, 'Q', res.pll_q), (Role::PllR, 'R', res.pll_r)]
                .into_iter()
                .filter_map(|(role, letter, f)| {
                    let sel = tree.selector(role)?;
                    let active = clk.pll_enabled && clk.pll_output_enabled(role);
                    Some((letter, sel, clk.role(role).unwrap_or("").to_string(), active, f, Some(role)))
                })
                .collect();
            let draw = PllDraw {
                title: format!("Main PLL  ({})", tree.main_pll),
                name: tree.main_pll.clone(),
                enabled: clk.pll_enabled,
                src: tree.pll_src.as_ref().map(|s| (s, clk.role(Role::PllSrc).unwrap_or("").to_string())),
                m: tree.pll_m.as_ref().map(|s| (s, clk.role(Role::PllM).unwrap_or("").to_string())),
                n: (n, clk.role(Role::PllN).unwrap_or("").to_string()),
                outs,
                pll_in: res.pll_in,
                vco: res.vco,
            };
            let (h, outs, src_out) = Self::draw_pll(ui, &painter, &mut wires, &at, y, &draw, &osc_out, None, edits, true);
            main_outputs = outs;
            main_src_out = src_out;
            y += h + 30.0;
        } else {
            painter.text(put(PLL_X + 10.0, 10.0), Align2::LEFT_TOP, "No PLL in this RCC", FontId::proportional(12.0), theme::TEXT_DIM);
            y += 60.0;
        }
        for (pll, r) in tree.aux_plls.iter().zip(&res.aux) {
            let outs = ['P', 'Q', 'R']
                .into_iter()
                .filter_map(|l| {
                    let sel = pll.output(l)?;
                    Some((l, sel, clk.value(sel).to_string(), r.enabled, r.output(l), None))
                })
                .collect();
            let draw = PllDraw {
                title: pll.name.clone(),
                name: pll.name.clone(),
                enabled: r.enabled,
                src: pll.src.as_ref().map(|s| (s, clk.value(s).to_string())),
                m: pll.m.as_ref().map(|s| (s, clk.value(s).to_string())),
                n: (&pll.n, clk.value(&pll.n).to_string()),
                outs,
                pll_in: r.pll_in,
                vco: r.vco,
            };
            let (h, _, _) = Self::draw_pll(ui, &painter, &mut wires, &at, y, &draw, &osc_out, main_src_out, edits, false);
            y += h + 30.0;
        }

        // ---- SYSCLK mux ----
        let sys_sel = tree.sys.as_ref().unwrap();
        let sys_rect = at(SYS_X, 40.0, 150.0, 92.0);
        node(&painter, sys_rect, "SYSCLK source", true);
        let sys_cur = clk.role(Role::Sys).unwrap_or("").to_string();
        for v in &sys_sel.variants {
            let from = if v.name.starts_with("PLL") {
                let letter = v.name.rsplit('_').next().and_then(|s| s.chars().next()).unwrap_or('P');
                main_outputs.iter().find(|(l, _)| *l == letter).or(main_outputs.first()).map(|(_, p)| *p)
            } else {
                oscillator_base(&v.name).and_then(|b| osc_out.get(&b).copied())
            };
            if let Some(from) = from {
                wire(&mut wires, from, input(sys_rect), v.name == sys_cur);
            }
        }
        if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(sys_rect.left_top() + vec2(8.0, 28.0), vec2(134.0, 24.0)), sys_sel, &sys_cur) {
            edits.push(Edit::Role(Role::Sys, v));
        }
        freq(&painter, sys_rect, "SYSCLK", res.sysclk, true);

        // ---- AHB ----
        let ahb_rect = at(AHB_X, 40.0, 140.0, 92.0);
        node(&painter, ahb_rect, "AHB prescaler", true);
        wire(&mut wires, output(sys_rect), input(ahb_rect), true);
        if let Some(sel) = &tree.ahb {
            let cur = clk.role(Role::Ahb).unwrap_or("");
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(ahb_rect.left_top() + vec2(8.0, 28.0), vec2(124.0, 24.0)), sel, cur) {
                edits.push(Edit::Role(Role::Ahb, v));
            }
        }
        freq(&painter, ahb_rect, "HCLK", res.hclk, true);

        // ---- bus clocks ----
        let hclk_rect = at(OUT_X, 0.0, OUT_W, 66.0);
        node(&painter, hclk_rect, "HCLK → AHB bus, core, memory, DMA", true);
        painter.text(hclk_rect.left_top() + vec2(10.0, 30.0), Align2::LEFT_TOP, format_hz(res.hclk), FontId::monospace(16.0), theme::ACCENT);
        wire(&mut wires, output(ahb_rect), input(hclk_rect), true);

        let tick_rect = at(OUT_X, 78.0, OUT_W, 66.0);
        node(&painter, tick_rect, "Cortex System timer (SysTick)", true);
        painter.text(
            tick_rect.left_top() + vec2(10.0, 32.0),
            Align2::LEFT_TOP,
            format!("/1 {}   ·   /8 {}", format_hz(res.hclk), format_hz(res.hclk / 8.0)),
            FontId::monospace(11.5),
            theme::ACCENT,
        );
        wire(&mut wires, output(ahb_rect), input(tick_rect), true);

        for (i, sel) in tree.apb.iter().enumerate() {
            let rect = at(OUT_X, apb_top + i as f32 * 112.0, OUT_W, 100.0);
            let (bus, f) = res.apb.iter().find(|(_, fld, _)| *fld == sel.field).map(|(b, _, f)| (b.clone(), *f)).unwrap_or_default();
            let tim = res.apb_timers.iter().find(|(b, _)| *b == bus).map(|(_, f)| *f).unwrap_or(f);
            node(&painter, rect, &format!("{bus}  ·  {} prescaler", sel.field), true);
            wire(&mut wires, output(ahb_rect), input(rect), true);
            let cur = clk.apb.get(&sel.field).cloned().unwrap_or_default();
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(rect.left_top() + vec2(8.0, 28.0), vec2(90.0, 24.0)), sel, &cur) {
                edits.push(Edit::Apb(sel.field.clone(), v));
            }
            let mono = FontId::monospace(11.5);
            painter.text(rect.left_top() + vec2(110.0, 30.0), Align2::LEFT_TOP, "peripherals", FontId::proportional(11.0), theme::TEXT_DIM);
            painter.text(rect.right_top() + vec2(-10.0, 30.0), Align2::RIGHT_TOP, format_hz(f), mono.clone(), theme::ACCENT);
            painter.text(rect.left_top() + vec2(110.0, 56.0), Align2::LEFT_TOP, "timers", FontId::proportional(11.0), theme::TEXT_DIM);
            painter.text(rect.right_top() + vec2(-10.0, 56.0), Align2::RIGHT_TOP, format_hz(tim), mono, theme::ACCENT);
            painter.text(
                rect.left_bottom() + vec2(10.0, -7.0),
                Align2::LEFT_BOTTOM,
                if tim != f { "timers ×2 (prescaler ≠ 1)" } else { "timers = PCLK" },
                FontId::proportional(10.5),
                theme::TEXT_DIM,
            );
        }

        // ---- clock outputs, RTC and other choices ----
        section_title(&painter, put(GRID_X, grid_top - 10.0), "CLOCK OUTPUTS · RTC · OTHER");
        let mut cell = 0usize;
        let mut next_cell = |top: f32| {
            let r = at(GRID_X + (cell % GRID_COLS) as f32 * (GRID_W + 12.0), top + (cell / GRID_COLS) as f32 * 96.0, GRID_W, 84.0);
            cell += 1;
            r
        };
        for ((sel, pre), f) in tree.mcos.iter().zip(&res.mco) {
            let r = next_cell(grid_top);
            let cur = clk.value(sel);
            let on = cur != "DISABLE" && !cur.starts_with("DISABLE") && *f > 0.0;
            let pin = sel.field.trim_end_matches("SEL");
            node(&painter, r, &format!("{pin} output (pin)"), on);
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(if pre.is_some() { 100.0 } else { GRID_W - 16.0 }, 24.0)), sel, cur) {
                edits.push(Edit::Value(sel.clone(), v));
            }
            if let Some(pre) = pre {
                if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(114.0, 28.0), vec2(58.0, 24.0)), pre, clk.value(pre)) {
                    edits.push(Edit::Value(pre.clone(), v));
                }
            }
            freq(&painter, r, "", *f, on);
        }
        for (key, sel) in tree.muxes.iter().filter(|(k, _)| k.ends_with(".RTCSEL") || k.ends_with(".LSCOSEL")) {
            let r = next_cell(grid_top);
            let f = res.kernel.get(key).copied().unwrap_or(0.0);
            let rtc = key.ends_with(".RTCSEL");
            // LSCO has a separate enable bit we do not model, so it only shows its source.
            let title = if rtc { "RTC / LCD clock" } else { "LSCO output source" };
            node(&painter, r, title, rtc && f > 0.0);
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(GRID_W - 16.0, 24.0)), sel, clk.value(sel)) {
                edits.push(Edit::Value(sel.clone(), v));
            }
            freq(&painter, r, "", f, rtc && f > 0.0);
        }
        for sel in &tree.misc {
            let r = next_cell(grid_top);
            let title = match sel.field.as_str() {
                "STOPWUCK" => "Wake-up clock from Stop",
                "STOPKERWUCK" => "Kernel wake-up clock from Stop",
                "TIMPRE" => "Timer clock prescaler",
                "LSIPREDIV" => "LSI prescaler",
                other => other,
            };
            node(&painter, r, title, true);
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(GRID_W - 16.0, 24.0)), sel, clk.value(sel)) {
                edits.push(Edit::Value(sel.clone(), v));
            }
            painter.text(r.left_bottom() + vec2(10.0, -7.0), Align2::LEFT_BOTTOM, sel.key(), FontId::monospace(10.5), theme::TEXT_DIM);
        }

        // ---- peripheral kernel clocks ----
        section_title(&painter, put(GRID_X, kernel_top - 10.0), "PERIPHERAL CLOCKS (kernel clock muxes)");
        let mut cell = 0usize;
        for sel in &kernel {
            let r = at(GRID_X + (cell % GRID_COLS) as f32 * (GRID_W + 12.0), kernel_top + (cell / GRID_COLS) as f32 * 96.0, GRID_W, 84.0);
            cell += 1;
            let key = sel.key();
            let list = users.get(&key).cloned().unwrap_or_default();
            let any_enabled = list.iter().any(|(_, e)| *e);
            let title = if list.is_empty() {
                sel.field.trim_end_matches("SEL").to_string()
            } else {
                let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
                let joined = names.join(", ");
                if joined.len() > 14 && names.len() > 1 { format!("{} +{}", names[0], names.len() - 1) } else { joined }
            };
            let f = res.kernel.get(&key).copied().unwrap_or(0.0);
            node(&painter, r, &title, any_enabled);
            painter.text(r.right_top() + vec2(-10.0, 8.0), Align2::RIGHT_TOP, &sel.field, FontId::monospace(10.0), OFF_TEXT);
            let cur = clk.value(sel);
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(GRID_W - 16.0, 24.0)), sel, cur) {
                edits.push(Edit::Value((*sel).clone(), v));
            }
            let known = res.resolve(cur).is_some();
            if known {
                freq(&painter, r, "", f, any_enabled);
            } else {
                painter.text(r.left_bottom() + vec2(10.0, -7.0), Align2::LEFT_BOTTOM, "external clock", FontId::proportional(11.0), theme::TEXT_DIM);
            }
            if !list.is_empty() {
                let tip = list
                    .iter()
                    .map(|(n, e)| format!("{n}{}", if *e { "  (enabled)" } else { "" }))
                    .collect::<Vec<_>>()
                    .join("\n");
                ui.interact(Rect::from_min_size(r.left_top(), vec2(r.width(), 24.0)), ui.scope_id().with(("kclk_tip", &key)), egui::Sense::hover())
                    .on_hover_text(tip);
            }
        }

        painter.set(wire_slot, egui::Shape::Vec(wires));

        painter.text(
            pos2(canvas.left() + 20.0, canvas.bottom() - 14.0),
            Align2::LEFT_BOTTOM,
            format!(
                "Clock tree discovered from stm32-data registers/rcc_{}.json · frequencies of internal RC oscillators are nominal · check the datasheet for maximum clock limits",
                tree.rcc_version
            ),
            FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }

    /// Draw one PLL group starting at `y`. Returns (height, output anchors, source-mux output).
    #[allow(clippy::too_many_arguments)]
    fn draw_pll(
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        wires: &mut Vec<egui::Shape>,
        at: &dyn Fn(f32, f32, f32, f32) -> Rect,
        y: f32,
        pll: &PllDraw,
        osc_out: &BTreeMap<String, Pos2>,
        shared_src: Option<Pos2>,
        edits: &mut Vec<Edit>,
        main: bool,
    ) -> (f32, Vec<(char, Pos2)>, Option<Pos2>) {
        let on = pll.enabled;
        let h = 44.0 + (pll.outs.len().max(1) as f32 * 82.0).max(110.0);
        let group = at(PLL_X, y, PLL_W, h);
        painter.rect(
            group,
            10.0,
            Color32::from_rgba_unmultiplied(0x24, 0x28, 0x30, if on { 60 } else { 20 }),
            Stroke::new(1.0, if on { theme::ACCENT_DIM } else { theme::BORDER }),
            StrokeKind::Inside,
        );
        painter.text(group.left_top() + vec2(12.0, 10.0), Align2::LEFT_TOP, &pll.title, FontId::proportional(13.0), if on { theme::TEXT } else { theme::TEXT_DIM });
        let mut en = on;
        if ui
            .put(Rect::from_min_size(group.right_top() + vec2(-110.0, 6.0), vec2(100.0, 22.0)), egui::Checkbox::new(&mut en, "Enable"))
            .changed()
        {
            edits.push(if main { Edit::PllEnabled(en) } else { Edit::AuxEnabled(pll.name.clone(), en) });
        }
        let top = y + 40.0;
        // The main PLL keeps its settings as roles (used by code generation); others by register field.
        let set = |sel: &Selector, v: String, role: Option<Role>| match role.filter(|_| main) {
            Some(role) => Edit::Role(role, v),
            None => Edit::Value(sel.clone(), v),
        };

        // Source mux
        let mut prev: Option<Pos2> = shared_src;
        let mut src_out = None;
        if let Some((sel, cur)) = &pll.src {
            let r = at(PLL_X + 12.0, top, 128.0, 74.0);
            node(painter, r, "Source mux", on);
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(112.0, 24.0)), sel, cur) {
                edits.push(set(sel, v, Some(Role::PllSrc)));
            }
            for v in &sel.variants {
                if let Some(from) = oscillator_base(&v.name).and_then(|b| osc_out.get(&b)) {
                    wire(wires, *from, input(r), on && v.name == *cur);
                }
            }
            prev = Some(output(r));
            src_out = prev;
        }
        // /M
        if let Some((sel, cur)) = &pll.m {
            let r = at(PLL_X + 152.0, top, 100.0, 74.0);
            node(painter, r, "/M", on);
            if let Some(p) = prev {
                wire(wires, p, input(r), on);
            }
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 28.0), vec2(84.0, 24.0)), sel, cur) {
                edits.push(set(sel, v, Some(Role::PllM)));
            }
            freq(painter, r, "in", pll.pll_in, on);
            prev = Some(output(r));
        }
        // ×N
        let (nsel, ncur) = &pll.n;
        let nr = at(PLL_X + 266.0, top, 120.0, 92.0);
        node(painter, nr, "×N", on);
        if let Some(p) = prev {
            wire(wires, p, input(nr), on);
        }
        if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(nr.left_top() + vec2(8.0, 28.0), vec2(104.0, 24.0)), nsel, ncur) {
            edits.push(set(nsel, v, Some(Role::PllN)));
        }
        freq(painter, nr, "VCO", pll.vco, on);
        let n_out = output(nr);

        // Outputs
        let mut anchors = Vec::new();
        for (i, (letter, sel, cur, active, f, role)) in pll.outs.iter().enumerate() {
            let r = at(PLL_X + 400.0, top + i as f32 * 82.0, 188.0, 70.0);
            let name = format!("{}_{letter}", pll.name);
            node(painter, r, &format!("/{letter}  →  {name}"), *active);
            wire(wires, n_out, input(r), *active);
            let combo_w = if main { 112.0 } else { 172.0 };
            if let Some(v) = Self::selector_combo(ui, Rect::from_min_size(r.left_top() + vec2(8.0, 26.0), vec2(combo_w, 24.0)), sel, cur) {
                edits.push(set(sel, v, *role));
            }
            if let (true, Some(role)) = (main, role) {
                let mut out_on = *active;
                if ui
                    .put(Rect::from_min_size(r.left_top() + vec2(128.0, 27.0), vec2(55.0, 22.0)), egui::Checkbox::new(&mut out_on, "on"))
                    .on_hover_text("Enable this output (always on when it drives SYSCLK or a peripheral clock)")
                    .changed()
                {
                    edits.push(Edit::PllOutput(*role, out_on));
                }
            }
            freq(painter, r, "", f.unwrap_or(0.0), *active);
            anchors.push((*letter, output(r)));
        }
        if anchors.is_empty() {
            // F0/F1 style: the multiplier output is the PLL clock.
            anchors.push(('P', n_out));
        }
        (h, anchors, src_out)
    }
}
