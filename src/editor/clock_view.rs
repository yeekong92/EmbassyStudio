//! Clock configuration drawn like the STM32CubeMX clock tree.
//!
//! Left to right: clock sources (low-speed group with the RTC mux and IWDG on
//! top, high-speed sources below), the PLL source mux and one frame per PLL,
//! the System Clock Mux with SYSCLK → AHB → HCLK, and the bus outputs on the
//! right ("To Power", "HCLK to AHB bus…", Cortex timer, FCLK, APBx peripheral
//! and timer clocks). Peripheral clock muxes, MCO and LSCO sit underneath.
//!
//! Muxes are trapezoids with one radio input per choice, dividers are boxes
//! with a drop-down, frequencies are read-out boxes, wires are orthogonal and
//! highlighted along the active path. Everything is generated from the RCC
//! description in `crate::clock`, so the same layout serves every STM32 family.
//!
//! The whole tree is painted in canvas units and scaled, so it can be zoomed
//! and panned; drop-downs and frequency editors open as normal-size popups.

use std::collections::BTreeMap;

use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::Editor;
use crate::clock::{ClockConfig, ClockTree, Role, Selector, factor, format_hz, oscillator_base};
use crate::db::KernelClock;
use crate::theme;

// ---- layout (canvas units) ----
const SRC_W: f32 = 96.0;
const GUTTER_X: f32 = 110.0;
const GUTTER_STEP: f32 = 8.0;
const MUX_W: f32 = 26.0;
const MUX_STEP: f32 = 16.0;
const MUX_B_X: f32 = 252.0;
const RTC_OUT_X: f32 = 300.0;
const PLL_X: f32 = 312.0;
const PLL_W: f32 = 296.0;
const SYS_GX: f32 = 630.0;
const SYS_MUX_X: f32 = 712.0;
const SYSCLK_X: f32 = 758.0;
const AHB_X: f32 = 856.0;
const HCLK_X: f32 = 924.0;
const TRUNK_X: f32 = 1008.0;
const PRE_X: f32 = 1016.0;
const BRANCH_X: f32 = 1062.0;
const OUT_X: f32 = 1100.0;
const ROW: f32 = 28.0;
const BOX_H: f32 = 20.0;
const CANVAS_W: f32 = 1400.0;
const GRID_COLS: usize = 4;

const WIRE_OFF: Color32 = Color32::from_rgb(0x46, 0x4c, 0x58);
const OFF_FILL: Color32 = Color32::from_rgb(0x1e, 0x21, 0x27);
const OFF_TEXT: Color32 = Color32::from_rgb(0x6a, 0x71, 0x7e);
const LABEL: Color32 = Color32::from_rgb(0xa4, 0xab, 0xb8);

/// Friendly display for a variant: `Div4` -> `/4`, `Mul168` -> `X 168`, `Range4M` -> `4 MHz`.
fn pretty(variant: &str) -> String {
    match factor(variant) {
        Some(f) if f >= 1.0 && variant.to_ascii_lowercase().starts_with("mul") => format!("X {}", trim(f)),
        Some(f) if f > 0.0 => format!("/{}", trim(1.0 / f)),
        // Oscillator ranges: `Range4M` (L4), `RANGE_4MHZ` (U5) -> `4 MHz`.
        _ if variant.to_ascii_uppercase().starts_with("RANGE") => {
            crate::clock::range_frequency(variant).map(format_hz).unwrap_or_else(|| variant.to_string())
        }
        _ => variant.to_string(),
    }
}

/// Name of a mux input as CubeMX prints it.
fn input_label(variant: &str) -> String {
    match variant {
        "SYS" => "SYSCLK".into(),
        v if v.starts_with("DISABLE") => "None".into(),
        v => v.to_string(),
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Pan / zoom of the clock canvas.
pub struct Camera {
    zoom: f32,
    pan: Vec2,
    /// Fit the width on the next frame (first show, or the Fit button).
    fit: bool,
    /// Height of the drawn tree, for pan limits.
    content_h: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { zoom: 1.0, pan: Vec2::ZERO, fit: true, content_h: 1200.0 }
    }
}

/// What a drop-down writes back to.
#[derive(Clone)]
enum Target {
    Role(Role),
    Apb(String),
    Value,
}

#[derive(Clone)]
enum PopupKind {
    Choose { sel: Selector, target: Target, current: String },
    Freq(String),
    Systick,
}

/// An open drop-down or frequency editor.
pub struct ClockPopup {
    kind: PopupKind,
    anchor: Pos2,
    opening: bool,
}

/// A change to the project, applied after the frame is drawn.
#[derive(Clone)]
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
    Systick(bool),
}

#[derive(Clone)]
enum Action {
    Edit(Edit),
    Popup(PopupKind),
}

struct Hit {
    rect: Rect,
    action: Option<Action>,
    tip: String,
}

/// One input of a mux.
struct MuxIn {
    label: String,
    selected: bool,
    action: Action,
    tip: String,
}

/// Painter in canvas units.
struct Cv {
    painter: egui::Painter,
    origin: Pos2,
    z: f32,
    hover: Option<Pos2>,
    wires: Vec<Shape>,
    hits: Vec<Hit>,
}

impl Cv {
    fn p(&self, m: Pos2) -> Pos2 {
        self.origin + m.to_vec2() * self.z
    }

    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(pos2(x, y)), vec2(w, h) * self.z)
    }

    fn font(&self, size: f32) -> FontId {
        FontId::proportional(size * self.z)
    }

    fn mono(&self, size: f32) -> FontId {
        FontId::monospace(size * self.z)
    }

    fn text(&self, at: Pos2, align: Align2, text: impl ToString, size: f32, color: Color32) {
        self.painter.text(self.p(at), align, text.to_string(), self.font(size), color);
    }

    fn hovered(&self, rect: Rect) -> bool {
        self.hover.is_some_and(|h| rect.contains(h))
    }

    /// Orthogonal wire through model points.
    fn wire(&mut self, pts: &[Pos2], on: bool) {
        let pts: Vec<Pos2> = pts.iter().map(|p| self.p(*p)).collect();
        let stroke = if on { Stroke::new(1.8 * self.z.max(0.6), theme::ACCENT) } else { Stroke::new(1.0 * self.z.max(0.6), WIRE_OFF) };
        self.wires.push(Shape::line(pts, stroke));
    }

    fn dot(&mut self, at: Pos2, on: bool) {
        let c = self.p(at);
        self.wires.push(Shape::circle_filled(c, 2.6 * self.z, if on { theme::ACCENT } else { WIRE_OFF }));
    }

    fn hit(&mut self, rect: Rect, action: Option<Action>, tip: impl ToString) {
        self.hits.push(Hit { rect, action, tip: tip.to_string() });
    }

    /// Frequency read-out box. Returns its right-middle point.
    fn vbox(&mut self, x: f32, y: f32, w: f32, hz: f64, on: bool, action: Option<Action>, tip: &str) -> Pos2 {
        let r = self.r(x, y, w, BOX_H);
        let editable = action.is_some();
        let hot = editable && self.hovered(r);
        self.painter.rect(
            r,
            3.0 * self.z,
            if on { theme::CARD } else { OFF_FILL },
            Stroke::new(1.0, if hot { theme::ACCENT } else if editable { LABEL } else if on { theme::ACCENT_DIM } else { theme::BORDER }),
            StrokeKind::Inside,
        );
        let text = if hz > 0.0 { format_hz(hz) } else { "0".into() };
        self.painter.text(r.center(), Align2::CENTER_CENTER, text, self.mono(10.5), if on && hz > 0.0 { theme::ACCENT } else { OFF_TEXT });
        self.hit(r, action, tip);
        pos2(x + w, y + BOX_H / 2.0)
    }

    /// Drop-down box (divider, multiplier, prescaler, range). Returns its right-middle point.
    fn sbox(&mut self, x: f32, y: f32, w: f32, text: &str, on: bool, action: Option<Action>, tip: &str) -> Pos2 {
        let r = self.r(x, y, w, BOX_H);
        let clickable = action.is_some();
        let hot = clickable && self.hovered(r);
        self.painter.rect(
            r,
            2.0 * self.z,
            if hot { theme::CARD_HOVER } else if on { Color32::from_rgb(0x2a, 0x2f, 0x39) } else { OFF_FILL },
            Stroke::new(1.0, if hot { theme::ACCENT } else { theme::BORDER }),
            StrokeKind::Inside,
        );
        let color = if on { theme::TEXT } else { OFF_TEXT };
        if clickable {
            self.painter.text(r.left_center() + vec2(5.0 * self.z, 0.0), Align2::LEFT_CENTER, text, self.mono(10.5), color);
            self.painter.text(r.right_center() - vec2(4.0 * self.z, 0.0), Align2::RIGHT_CENTER, "▾", self.font(8.0), OFF_TEXT);
        } else {
            self.painter.text(r.center(), Align2::CENTER_CENTER, text, self.mono(10.5), color);
        }
        self.hit(r, action, tip);
        pos2(x + w, y + BOX_H / 2.0)
    }

    fn check(&mut self, x: f32, y: f32, on: bool, action: Action, tip: &str) {
        let r = self.r(x, y, 10.0, 10.0);
        let hot = self.hovered(r.expand(2.0));
        self.painter.rect(
            r,
            2.0 * self.z,
            if on { theme::ACCENT_DIM } else { OFF_FILL },
            Stroke::new(1.0, if hot { theme::ACCENT } else { LABEL }),
            StrokeKind::Inside,
        );
        if on {
            // Tick drawn as lines: not every font has a check-mark glyph.
            let pts = [r.left_top() + vec2(0.22, 0.52) * r.width(), r.left_top() + vec2(0.42, 0.74) * r.width(), r.left_top() + vec2(0.80, 0.26) * r.width()];
            self.painter.add(Shape::line(pts.to_vec(), Stroke::new(1.6 * self.z.max(0.7), theme::TEXT)));
        }
        self.hit(r.expand(3.0 * self.z), Some(action), tip);
    }

    /// Trapezoid mux with radio inputs on the left. Returns the input attach
    /// points (left edge), the output point and the bottom y.
    fn mux(&mut self, x: f32, top: f32, inputs: Vec<MuxIn>, on: bool) -> (Vec<Pos2>, Pos2, f32) {
        let n = inputs.len().max(1) as f32;
        let h = n * MUX_STEP + 8.0;
        let inset = (h * 0.16).min(10.0);
        let pts = [pos2(x, top), pos2(x + MUX_W, top + inset), pos2(x + MUX_W, top + h - inset), pos2(x, top + h)];
        let screen: Vec<Pos2> = pts.iter().map(|p| self.p(*p)).collect();
        self.painter.add(Shape::convex_polygon(
            screen,
            if on { Color32::from_rgb(0x23, 0x30, 0x2e) } else { OFF_FILL },
            Stroke::new(1.0, if on { theme::ACCENT_DIM } else { theme::BORDER }),
        ));
        let mut attach = Vec::new();
        for (i, input) in inputs.into_iter().enumerate() {
            let y = top + 4.0 + MUX_STEP / 2.0 + i as f32 * MUX_STEP;
            let c = self.p(pos2(x + 8.0, y));
            let rad = 4.2 * self.z;
            let hot = self.hover.is_some_and(|h| h.distance(c) < rad * 2.2);
            self.painter.circle(c, rad, OFF_FILL, Stroke::new(1.0, if hot { theme::ACCENT } else { LABEL }));
            if input.selected {
                self.painter.circle_filled(c, rad * 0.55, theme::ACCENT);
            }
            self.text(
                pos2(x - 3.0, y - 1.5),
                Align2::RIGHT_BOTTOM,
                &input.label,
                9.0,
                if input.selected && on { theme::ACCENT } else { LABEL },
            );
            self.hit(Rect::from_center_size(c, Vec2::splat(rad * 4.0)), Some(input.action), input.tip);
            attach.push(pos2(x, y));
        }
        (attach, pos2(x + MUX_W, top + h / 2.0), top + h)
    }

    fn frame(&self, x: f32, y: f32, w: f32, h: f32, on: bool) {
        self.painter.rect(
            self.r(x, y, w, h),
            6.0 * self.z,
            Color32::from_rgba_unmultiplied(0x24, 0x28, 0x30, if on { 70 } else { 30 }),
            Stroke::new(1.0, if on { theme::ACCENT_DIM } else { theme::BORDER }),
            StrokeKind::Inside,
        );
    }

    /// Small caption above a box.
    fn caption(&self, x: f32, y: f32, text: &str) {
        self.text(pos2(x, y - 2.0), Align2::LEFT_BOTTOM, text, 9.0, LABEL);
    }
}

/// Everything needed to draw one PLL frame.
struct PllDraw<'a> {
    title: String,
    name: String,
    main: bool,
    enabled: bool,
    m: Option<(&'a Selector, String)>,
    /// /M shared with the main PLL (shown dimmed, not editable).
    m_shared: Option<String>,
    n: (&'a Selector, String),
    /// (letter, selector, value, active, frequency, main role)
    outs: Vec<(char, &'a Selector, String, bool, Option<f64>, Option<Role>)>,
    pll_in: f64,
    vco: f64,
}

impl Editor {
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
        self.clock_toolbar(ui, &tree);

        let mut cam = std::mem::take(&mut self.clock_cam);
        let mut edits = Vec::new();
        let mut open = None;
        self.clock_canvas(ui, &tree, &mut cam, &mut edits, &mut open);
        self.clock_cam = cam;
        if let Some(p) = open {
            self.clock_popup = Some(p);
        }
        self.show_clock_popup(ui, &mut edits);

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
        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 6)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let chip = |ui: &mut egui::Ui, name: &str, hz: f64| {
                    ui.label(RichText::new(name).size(11.5).color(theme::TEXT_DIM));
                    ui.label(RichText::new(format_hz(hz)).monospace().strong().color(theme::ACCENT));
                    ui.add_space(8.0);
                };
                chip(ui, "SYSCLK", res.sysclk);
                chip(ui, "HCLK", res.hclk);
                for (bus, _, f) in &res.apb {
                    chip(ui, bus, *f);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    reset = ui.button("Reset clocks").on_hover_text("Back to the reset configuration of the RCC").clicked();
                    ui.separator();
                    if ui.button("+").on_hover_text("Zoom in (Ctrl+wheel)").clicked() {
                        self.clock_cam.zoom = (self.clock_cam.zoom * 1.2).min(2.5);
                    }
                    if ui.button("Fit").on_hover_text("Fit the tree to the window width").clicked() {
                        self.clock_cam.fit = true;
                    }
                    if ui.button("−").on_hover_text("Zoom out (Ctrl+wheel)").clicked() {
                        self.clock_cam.zoom = (self.clock_cam.zoom / 1.2).max(0.35);
                    }
                    ui.label(RichText::new(format!("{:.0}%", self.clock_cam.zoom * 100.0)).size(11.0).color(theme::TEXT_DIM));
                    ui.separator();
                    let (txt, color) = match crate::codegen::rcc_flavor(tree) {
                        crate::codegen::RccFlavor::SummaryOnly => (format!("RCC '{}': board.rs gets these as comments", tree.rcc_version), theme::WARN),
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

    fn apply_clock_edit(&mut self, tree: &ClockTree, edit: Edit) {
        let clk = &mut self.project.clock;
        match edit {
            Edit::Role(role, v) => {
                if role == Role::Sys && v.starts_with("PLL") {
                    clk.pll_enabled = true;
                }
                if let Some(base) = oscillator_base(&v).filter(|b| tree.oscillators.contains(b)) {
                    clk.enabled.insert(base, true);
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
                // A source picked in a mux is switched on too.
                if let Some(base) = oscillator_base(&v).filter(|b| tree.oscillators.contains(b)) {
                    clk.enabled.insert(base, true);
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
            Edit::Systick(div8) => clk.systick_div8 = div8,
        }
    }

    fn show_clock_popup(&mut self, ui: &mut egui::Ui, edits: &mut Vec<Edit>) {
        let Some(mut pp) = self.clock_popup.take() else { return };
        let id = egui::Id::unique("clock_popup");
        let opening = std::mem::take(&mut pp.opening);
        let mut keep = true;
        let clk = &self.project.clock;
        let shown = egui::Popup::new(id, ui.ctx().clone(), egui::PopupAnchor::Position(pp.anchor), ui.layer_id())
            .kind(egui::PopupKind::Menu)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .style(egui::containers::menu::menu_style)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .open_memory(opening.then_some(egui::SetOpenCommand::Bool(true)))
            .show(|ui| match &mut pp.kind {
                PopupKind::Choose { sel, target, current } => {
                    ui.set_min_width(140.0);
                    ui.label(RichText::new(sel.key()).size(11.0).color(theme::TEXT_DIM));
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for v in &sel.variants {
                            let r = ui
                                .selectable_label(*current == v.name, RichText::new(pretty(&v.name)).monospace())
                                .on_hover_text(v.description.clone().unwrap_or_else(|| v.name.clone()));
                            if r.clicked() {
                                edits.push(match target {
                                    Target::Role(role) => Edit::Role(*role, v.name.clone()),
                                    Target::Apb(field) => Edit::Apb(field.clone(), v.name.clone()),
                                    Target::Value => Edit::Value(sel.clone(), v.name.clone()),
                                });
                                keep = false;
                            }
                        }
                    });
                }
                PopupKind::Freq(osc) => {
                    ui.label(RichText::new(format!("{osc} frequency")).strong());
                    let mut mhz = clk.oscillators.get(osc.as_str()).copied().unwrap_or(0.0) / 1e6;
                    let r = ui.add(egui::DragValue::new(&mut mhz).range(0.0..=200.0).speed(0.01).suffix(" MHz").max_decimals(6));
                    if opening {
                        r.request_focus();
                    }
                    if r.changed() {
                        edits.push(Edit::OscFreq(osc.clone(), mhz * 1e6));
                    }
                    let external = osc.starts_with("HSE") || osc.starts_with("LSE");
                    ui.label(
                        RichText::new(if external { "Crystal / clock on your board" } else { "Nominal value of the internal RC" })
                            .size(11.0)
                            .color(theme::TEXT_DIM),
                    );
                    if ui.button("Done").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        keep = false;
                    }
                }
                PopupKind::Systick => {
                    ui.label(RichText::new("Cortex System timer").size(11.0).color(theme::TEXT_DIM));
                    ui.separator();
                    for (div8, text) in [(false, "/1"), (true, "/8")] {
                        if ui.selectable_label(clk.systick_div8 == div8, RichText::new(text).monospace()).clicked() {
                            edits.push(Edit::Systick(div8));
                            keep = false;
                        }
                    }
                }
            });
        if shown.is_none() {
            keep = false;
        }
        if keep {
            self.clock_popup = Some(pp);
        } else {
            egui::Popup::close_id(ui.ctx(), id);
        }
    }

    fn clock_canvas(&self, ui: &mut egui::Ui, tree: &ClockTree, cam: &mut Camera, edits: &mut Vec<Edit>, open: &mut Option<ClockPopup>) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);

        // ---- camera ----
        if cam.fit {
            cam.zoom = ((rect.width() - 40.0) / CANVAS_W).clamp(0.35, 1.6);
            cam.pan = Vec2::ZERO;
            cam.fit = false;
        }
        if resp.hovered() {
            let (scroll, zoom_delta, hover) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            if zoom_delta != 1.0 {
                if let Some(h) = hover {
                    let old_origin = rect.min + vec2(20.0, 20.0) + cam.pan;
                    let model = (h - old_origin) / cam.zoom;
                    cam.zoom = (cam.zoom * zoom_delta).clamp(0.35, 2.5);
                    cam.pan = h - rect.min - vec2(20.0, 20.0) - model * cam.zoom;
                }
            } else {
                cam.pan += scroll;
            }
        }
        if resp.dragged() {
            cam.pan += resp.drag_delta();
        }
        let cw = CANVAS_W * cam.zoom;
        let ch = cam.content_h * cam.zoom;
        cam.pan.x = cam.pan.x.clamp((rect.width() - cw - 60.0).min(0.0), 40.0);
        cam.pan.y = cam.pan.y.clamp((rect.height() - ch - 80.0).min(0.0), 40.0);

        let wire_slot = painter.add(Shape::Noop);
        let mut cv = Cv {
            painter: painter.clone(),
            origin: rect.min + vec2(20.0, 20.0) + cam.pan,
            z: cam.zoom,
            hover: resp.hover_pos(),
            wires: Vec::new(),
            hits: Vec::new(),
        };
        let bottom = self.draw_tree(&mut cv, tree);
        cam.content_h = bottom + 40.0;
        painter.set(wire_slot, Shape::Vec(std::mem::take(&mut cv.wires)));

        // ---- interaction ----
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let target = pointer.and_then(|p| cv.hits.iter().rposition(|h| h.rect.contains(p)));
        if let Some(i) = target {
            let hit = &cv.hits[i];
            if !hit.tip.is_empty() && !resp.dragged() {
                resp.clone().on_hover_text_at_pointer(hit.tip.clone());
            }
            if hit.action.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                match hit.action.clone() {
                    Some(Action::Edit(e)) => edits.push(e),
                    Some(Action::Popup(kind)) => {
                        *open = Some(ClockPopup { kind, anchor: hit.rect.left_bottom() + vec2(0.0, 2.0), opening: true });
                    }
                    None => {}
                }
            }
        }
        if resp.double_clicked() && target.is_none() {
            cam.fit = true;
        }

        let strip = Rect::from_min_max(rect.left_bottom() - vec2(0.0, 24.0), rect.right_bottom());
        painter.rect_filled(strip, 0.0, theme::BG);
        painter.text(
            rect.left_bottom() + vec2(10.0, -6.0),
            Align2::LEFT_BOTTOM,
            "Click a box or a mux input to change it  ·  Wheel: scroll  ·  Ctrl+wheel: zoom  ·  Drag: pan  ·  Double-click: fit",
            FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
    }

    /// Paint the whole tree; returns the bottom y in canvas units.
    fn draw_tree(&self, cv: &mut Cv, tree: &ClockTree) -> f32 {
        let clk = &self.project.clock;
        let res = &self.clocks;
        let on = |name: &str| clk.is_enabled(name);
        let choose = |sel: &Selector, target: Target, current: &str| {
            Action::Popup(PopupKind::Choose { sel: sel.clone(), target, current: current.to_string() })
        };

        // Sources: low-speed first (top band), then high-speed.
        let low: Vec<&String> = tree.oscillators.iter().filter(|o| o.starts_with("LS")).collect();
        let high: Vec<&String> = tree.oscillators.iter().filter(|o| !o.starts_with("LS")).collect();
        let order: Vec<&String> = low.iter().chain(high.iter()).copied().collect();
        let gutter = |name: &str| GUTTER_X + order.iter().position(|o| *o == name).unwrap_or(0) as f32 * GUTTER_STEP;

        // Source output points and the wire attach points hanging off each source's gutter.
        let mut src_out: BTreeMap<String, Pos2> = BTreeMap::new();
        let mut taps: BTreeMap<String, Vec<(Pos2, bool)>> = BTreeMap::new();

        let draw_source = |cv: &mut Cv, name: &str, y: f32, src_out: &mut BTreeMap<String, Pos2>| -> f32 {
            let enabled = on(name);
            let external = name.starts_with("HSE") || name.starts_with("LSE");
            cv.check(0.0, y + 1.0, enabled, Action::Edit(Edit::OscEnabled(name.to_string(), !enabled)), "Enable this source");
            let title = if external { name.to_string() } else { format!("{name} RC") };
            cv.text(pos2(14.0, y), Align2::LEFT_TOP, title, 10.5, if enabled { theme::TEXT } else { OFF_TEXT });
            let f = clk.osc_frequency(tree, name);
            let out = if let Some(sel) = tree.osc_range.get(name) {
                let cur = clk.value(sel);
                cv.sbox(0.0, y + 16.0, SRC_W, &pretty(cur), enabled, Some(choose(sel, Target::Value, cur)), &format!("{name} range"))
            } else {
                let tip = if external { "Input frequency — click to edit" } else { "Nominal frequency — click to edit" };
                cv.vbox(0.0, y + 16.0, SRC_W, f, enabled, Some(Action::Popup(PopupKind::Freq(name.to_string()))), tip)
            };
            src_out.insert(name.to_string(), out);
            let mut h = 40.0;
            // Per-source extras on a second line.
            if name.starts_with("HSE") {
                cv.check(0.0, y + 42.0, clk.hse_bypass, Action::Edit(Edit::HseBypass(!clk.hse_bypass)), "External clock instead of a crystal");
                cv.text(pos2(14.0, y + 41.0), Align2::LEFT_TOP, "Bypass", 9.0, LABEL);
                h += 16.0;
            }
            if name.starts_with("LSE") {
                if let Some(sel) = &tree.lse_drive {
                    let cur = clk.value(sel);
                    cv.text(pos2(0.0, y + 50.0), Align2::LEFT_CENTER, "Drive", 9.0, LABEL);
                    cv.sbox(34.0, y + 40.0, SRC_W - 34.0, cur, enabled, Some(choose(sel, Target::Value, cur)), "LSE drive strength");
                    h += 24.0;
                }
            }
            if let Some(sel) = tree.osc_div.get(name) {
                let cur = clk.value(sel);
                cv.text(pos2(0.0, y + 50.0), Align2::LEFT_CENTER, "Divider", 9.0, LABEL);
                cv.sbox(44.0, y + 40.0, SRC_W - 44.0, &pretty(cur), enabled, Some(choose(sel, Target::Value, cur)), &sel.key());
                h += 24.0;
            }
            h
        };

        // ---- low-speed band: LSE/LSI, RTC mux, IWDG ----
        cv.text(pos2(0.0, -16.0), Align2::LEFT_TOP, "LOW-SPEED SOURCES", 9.0, theme::TEXT_DIM);
        let mut y = 0.0;
        for name in &low {
            y += draw_source(cv, name, y, &mut src_out) + 10.0;
        }
        let mut band_bottom = y;
        let mut rtc_bottom = 0.0;
        if let Some((key, sel)) = tree.muxes.iter().find(|(k, _)| k.ends_with(".RTCSEL")) {
            let cur = clk.value(sel).to_string();
            let inputs: Vec<MuxIn> = sel
                .variants
                .iter()
                .map(|v| MuxIn {
                    label: input_label(&v.name),
                    selected: v.name == cur,
                    action: Action::Edit(Edit::Value(sel.clone(), v.name.clone())),
                    tip: format!("{} = {}", key, v.name),
                })
                .collect();
            cv.caption(MUX_B_X - 30.0, 2.0, "RTC Clock Mux");
            let (attach, out, mb) = cv.mux(MUX_B_X, 4.0, inputs, true);
            for (v, at) in sel.variants.iter().zip(&attach) {
                if let Some(base) = oscillator_base(&v.name).filter(|b| src_out.contains_key(b)) {
                    let active = v.name == cur && on(&base);
                    taps.entry(base).or_default().push((*at, active));
                }
            }
            let f = res.kernel.get(key).copied().unwrap_or(0.0);
            let bx = RTC_OUT_X + 16.0;
            cv.wire(&[out, pos2(bx, out.y)], f > 0.0);
            let end = cv.vbox(bx, out.y - BOX_H / 2.0, 72.0, f, f > 0.0, None, "RTC clock");
            cv.text(end + vec2(6.0, 0.0), Align2::LEFT_CENTER, "To RTC", 10.0, LABEL);
            rtc_bottom = mb;
            band_bottom = band_bottom.max(mb);
        }
        let has_iwdg = self.chip.as_ref().is_some_and(|c| c.peripherals().iter().any(|p| p.name.starts_with("IWDG")));
        if let (true, Some(lsi)) = (has_iwdg, low.iter().find(|o| o.starts_with("LSI"))) {
            let yy = rtc_bottom + 12.0;
            let bx = RTC_OUT_X + 16.0;
            let f = res.resolve(lsi).unwrap_or(0.0);
            taps.entry(lsi.to_string()).or_default().push((pos2(bx, yy + BOX_H / 2.0), f > 0.0));
            let end = cv.vbox(bx, yy, 72.0, f, f > 0.0, None, "Independent watchdog clock (LSI)");
            cv.text(end + vec2(6.0, 0.0), Align2::LEFT_CENTER, "To IWDG", 10.0, LABEL);
            band_bottom = band_bottom.max(yy + BOX_H);
        }

        // ---- lanes from sources to the System Clock Mux ----
        let sys_sel = tree.sys.as_ref().unwrap();
        let sys_inputs: Vec<&crate::db::EnumVariant> = sys_sel.variants.iter().filter(|v| !v.name.starts_with("DISABLE")).collect();
        let lane_srcs: Vec<String> = sys_inputs
            .iter()
            .filter_map(|v| oscillator_base(&v.name).filter(|b| tree.oscillators.contains(b)))
            .collect();
        let lane_top = band_bottom + 22.0;
        let lane_y = |i: usize| lane_top + i as f32 * 7.0;
        let hs_top = lane_top + lane_srcs.len() as f32 * 7.0 + 34.0;

        // ---- high-speed sources ----
        cv.text(pos2(0.0, hs_top - 16.0), Align2::LEFT_TOP, "HIGH-SPEED SOURCES", 9.0, theme::TEXT_DIM);
        let mut y = hs_top;
        for name in &high {
            y += draw_source(cv, name, y, &mut src_out) + 12.0;
        }
        let sources_bottom = y;

        // ---- PLLs ----
        let mut pll_bottom = hs_top;
        let mut main_outs: Vec<(char, Pos2, bool)> = Vec::new();
        let mut main_src_out: Option<Pos2> = None;
        let mut fy = hs_top;
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
                title: format!("{}  (main PLL)", tree.main_pll),
                name: tree.main_pll.clone(),
                main: true,
                enabled: clk.pll_enabled,
                m: tree.pll_m.as_ref().map(|s| (s, clk.role(Role::PllM).unwrap_or("").to_string())),
                m_shared: None,
                n: (n, clk.role(Role::PllN).unwrap_or("").to_string()),
                outs,
                pll_in: res.pll_in,
                vco: res.vco,
            };
            let (h, m_in, outs) = self.draw_pll(cv, fy, &draw);
            main_outs = outs;
            // PLL source mux, centred on the /M input.
            if let Some(sel) = &tree.pll_src {
                let cur = clk.role(Role::PllSrc).unwrap_or("").to_string();
                let vars: Vec<&crate::db::EnumVariant> = sel.variants.iter().filter(|v| !v.name.starts_with("DISABLE")).collect();
                let inputs: Vec<MuxIn> = vars
                    .iter()
                    .map(|v| MuxIn {
                        label: input_label(&v.name),
                        selected: v.name == cur,
                        action: Action::Edit(Edit::Role(Role::PllSrc, v.name.clone())),
                        tip: format!("{} = {}", sel.key(), v.name),
                    })
                    .collect();
                let top = m_in.y - (vars.len() as f32 * MUX_STEP + 8.0) / 2.0;
                cv.caption(MUX_B_X - 34.0, top - 2.0, "PLL Source Mux");
                let (attach, out, mb) = cv.mux(MUX_B_X, top, inputs, clk.pll_enabled);
                for (v, at) in vars.iter().zip(&attach) {
                    if let Some(base) = oscillator_base(&v.name).filter(|b| src_out.contains_key(b)) {
                        let active = clk.pll_enabled && v.name == cur && on(&base);
                        taps.entry(base).or_default().push((*at, active));
                    }
                }
                cv.wire(&[out, pos2(m_in.x, out.y)], clk.pll_enabled);
                main_src_out = Some(out);
                pll_bottom = pll_bottom.max(mb);
            }
            fy += h + 16.0;
        } else {
            cv.text(pos2(PLL_X, hs_top), Align2::LEFT_TOP, "No PLL in this RCC", 10.0, theme::TEXT_DIM);
            fy += 40.0;
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
                main: false,
                enabled: r.enabled,
                m: pll.m.as_ref().map(|s| (s, clk.value(s).to_string())),
                m_shared: pll.m.is_none().then(|| clk.role(Role::PllM).map(pretty).unwrap_or_default()).filter(|_| tree.pll_m.is_some()),
                n: (&pll.n, clk.value(&pll.n).to_string()),
                outs,
                pll_in: r.pll_in,
                vco: r.vco,
            };
            let (h, m_in, _) = self.draw_pll(cv, fy, &draw);
            if let Some(sel) = &pll.src {
                // Own source mux (U5, F446 PLLI2S...).
                let cur = clk.value(sel).to_string();
                let vars: Vec<&crate::db::EnumVariant> = sel.variants.iter().filter(|v| !v.name.starts_with("DISABLE")).collect();
                let inputs: Vec<MuxIn> = vars
                    .iter()
                    .map(|v| MuxIn {
                        label: input_label(&v.name),
                        selected: v.name == cur,
                        action: Action::Edit(Edit::Value((*sel).clone(), v.name.clone())),
                        tip: format!("{} = {}", sel.key(), v.name),
                    })
                    .collect();
                let top = m_in.y - (vars.len() as f32 * MUX_STEP + 8.0) / 2.0;
                let (attach, out, _) = cv.mux(MUX_B_X, top, inputs, r.enabled);
                for (v, at) in vars.iter().zip(&attach) {
                    if let Some(base) = oscillator_base(&v.name).filter(|b| src_out.contains_key(b)) {
                        taps.entry(base.clone()).or_default().push((*at, r.enabled && v.name == cur && on(&base)));
                    }
                }
                cv.wire(&[out, pos2(m_in.x, out.y)], r.enabled);
            } else if let Some(out) = main_src_out {
                // Shares the main PLL source: a bus down the left of the frames.
                let bus_x = PLL_X - 10.0;
                cv.wire(&[pos2(bus_x, out.y), pos2(bus_x, m_in.y), m_in], r.enabled);
                cv.dot(pos2(bus_x, out.y), r.enabled);
            }
            fy += h + 16.0;
        }
        pll_bottom = pll_bottom.max(fy);

        // ---- System Clock Mux → SYSCLK → AHB → HCLK ----
        let rows_top = hs_top - 4.0;
        let chain_y = rows_top + ROW + BOX_H / 2.0;
        let sys_cur = clk.role(Role::Sys).unwrap_or("").to_string();
        let inputs: Vec<MuxIn> = sys_inputs
            .iter()
            .map(|v| MuxIn {
                label: input_label(&v.name),
                selected: v.name == sys_cur,
                action: Action::Edit(Edit::Role(Role::Sys, v.name.clone())),
                tip: format!("{} = {}", sys_sel.key(), v.name),
            })
            .collect();
        let sys_h = sys_inputs.len() as f32 * MUX_STEP + 8.0;
        let sys_top = (chain_y - sys_h / 2.0).max(hs_top - 6.0);
        cv.caption(SYS_MUX_X - 40.0, sys_top - 2.0, "System Clock Mux");
        let (attach, sys_out, sys_bottom) = cv.mux(SYS_MUX_X, sys_top, inputs, true);
        for (k, (v, at)) in sys_inputs.iter().zip(&attach).enumerate() {
            let gx = SYS_GX + k as f32 * 7.0;
            let selected = v.name == sys_cur;
            if v.name.starts_with("PLL") {
                let letter = v.name.rsplit('_').next().and_then(|s| s.chars().next()).unwrap_or('P');
                if let Some((_, from, active)) = main_outs.iter().find(|(l, _, _)| *l == letter).or(main_outs.first()) {
                    cv.wire(&[*from, pos2(gx, from.y), pos2(gx, at.y), *at], selected && *active);
                }
            } else if let Some(base) = oscillator_base(&v.name).filter(|b| src_out.contains_key(b)) {
                let lane = lane_srcs.iter().position(|s| *s == base).unwrap_or(0);
                let ly = lane_y(lane);
                let a = selected && on(&base);
                cv.wire(&[pos2(gutter(&base), ly), pos2(gx, ly), pos2(gx, at.y), *at], a);
                taps.entry(base.clone()).or_default().push((pos2(gutter(&base), ly), a));
            }
        }
        let chain_y = sys_out.y;
        cv.caption(SYSCLK_X + 10.0, chain_y - BOX_H / 2.0, "SYSCLK");
        cv.wire(&[sys_out, pos2(SYSCLK_X + 10.0, chain_y)], true);
        let p = cv.vbox(SYSCLK_X + 10.0, chain_y - BOX_H / 2.0, 72.0, res.sysclk, true, None, "System clock");
        cv.wire(&[p, pos2(AHB_X, chain_y)], true);
        cv.caption(AHB_X, chain_y - BOX_H / 2.0, "AHB Prescaler");
        let ahb_cur = clk.role(Role::Ahb).unwrap_or("").to_string();
        let p = match &tree.ahb {
            Some(sel) => cv.sbox(AHB_X, chain_y - BOX_H / 2.0, 54.0, &pretty(&ahb_cur), true, Some(choose(sel, Target::Role(Role::Ahb), &ahb_cur)), &sel.key()),
            None => pos2(AHB_X + 54.0, chain_y),
        };
        cv.wire(&[p, pos2(HCLK_X, chain_y)], true);
        cv.caption(HCLK_X, chain_y - BOX_H / 2.0, "HCLK");
        let hclk_out = cv.vbox(HCLK_X, chain_y - BOX_H / 2.0, 72.0, res.hclk, true, None, "AHB clock");

        // ---- bus outputs (right column) ----
        cv.text(pos2(OUT_X, rows_top - 20.0), Align2::LEFT_TOP, "OUTPUT CLOCKS", 9.0, theme::TEXT_DIM);
        let mut row_y = rows_top;
        let mut trunk_end = chain_y;
        let out_row = |cv: &mut Cv, y: f32, hz: f64, label: &str, tip: &str| {
            let end = cv.vbox(OUT_X, y, 72.0, hz, hz > 0.0, None, tip);
            cv.text(end + vec2(8.0, 0.0), Align2::LEFT_CENTER, label, 10.0, LABEL);
        };
        // Branches off the HCLK trunk that runs down at TRUNK_X.
        let branch = |cv: &mut Cv, y: f32, to_x: f32, trunk_end: &mut f32| {
            let cy = y + BOX_H / 2.0;
            cv.wire(&[pos2(TRUNK_X, cy), pos2(to_x, cy)], true);
            if (cy - chain_y).abs() > 0.5 {
                cv.dot(pos2(TRUNK_X, cy), true);
            }
            *trunk_end = trunk_end.max(cy);
        };
        for label in ["To Power", "HCLK to AHB bus, core, memory and DMA"] {
            branch(cv, row_y, OUT_X, &mut trunk_end);
            out_row(cv, row_y, res.hclk, label, "HCLK");
            row_y += ROW;
        }
        // Cortex System timer with its /1 or /8.
        branch(cv, row_y, PRE_X, &mut trunk_end);
        let st = if clk.systick_div8 { "/8" } else { "/1" };
        let p = cv.sbox(PRE_X, row_y, 40.0, st, true, Some(Action::Popup(PopupKind::Systick)), "SysTick clock source (Cortex CLKSOURCE)");
        cv.wire(&[p, pos2(OUT_X, p.y)], true);
        out_row(cv, row_y, res.hclk / if clk.systick_div8 { 8.0 } else { 1.0 }, "To Cortex System timer", "SysTick");
        row_y += ROW;
        branch(cv, row_y, OUT_X, &mut trunk_end);
        out_row(cv, row_y, res.hclk, "FCLK Cortex clock", "Free-running clock of the core");
        row_y += ROW + 14.0;
        for sel in &tree.apb {
            let (bus, f) = res.apb.iter().find(|(_, fld, _)| *fld == sel.field).map(|(b, _, f)| (b.clone(), *f)).unwrap_or_default();
            let tim = res.apb_timers.iter().find(|(b, _)| *b == bus).map(|(_, f)| *f).unwrap_or(f);
            let n: String = bus.chars().filter(|c| c.is_ascii_digit()).collect();
            let apb = format!("APB{n}");
            cv.caption(PRE_X, row_y, &format!("{apb} Prescaler"));
            branch(cv, row_y, PRE_X, &mut trunk_end);
            let cur = clk.apb.get(&sel.field).cloned().unwrap_or_default();
            let p = cv.sbox(PRE_X, row_y, 40.0, &pretty(&cur), true, Some(choose(sel, Target::Apb(sel.field.clone()), &cur)), &sel.key());
            cv.wire(&[p, pos2(OUT_X, p.y)], true);
            out_row(cv, row_y, f, &format!("{apb} peripheral clocks ({bus})"), &bus);
            // Timer clocks: X 1 or X 2 after the prescaler.
            let ty = row_y + ROW;
            cv.wire(&[pos2(BRANCH_X - 6.0, p.y), pos2(BRANCH_X - 6.0, ty + BOX_H / 2.0), pos2(BRANCH_X, ty + BOX_H / 2.0)], true);
            cv.dot(pos2(BRANCH_X - 6.0, p.y), true);
            let mul = if tim > f { "X 2" } else { "X 1" };
            let q = cv.sbox(BRANCH_X, ty, 30.0, mul, true, None, "Timers run at 2× PCLK when the APB prescaler divides");
            cv.wire(&[q, pos2(OUT_X, q.y)], true);
            out_row(cv, ty, tim, &format!("{apb} Timer clocks"), &format!("{bus}_TIM"));
            row_y += 2.0 * ROW + 14.0;
        }
        cv.wire(&[hclk_out, pos2(TRUNK_X, chain_y)], true);
        cv.wire(&[pos2(TRUNK_X, rows_top + BOX_H / 2.0), pos2(TRUNK_X, trunk_end)], true);
        let core_bottom = row_y.max(sys_bottom);

        // ---- source gutters (drawn once all taps are known) ----
        for name in &order {
            let Some(out) = src_out.get(name.as_str()) else { continue };
            let list = taps.get(name.as_str()).cloned().unwrap_or_default();
            if list.is_empty() {
                continue;
            }
            let gx = gutter(name);
            let any = list.iter().any(|(_, a)| *a);
            cv.wire(&[*out, pos2(gx, out.y)], any);
            let (lo, hi) = list.iter().map(|(p, _)| p.y).chain(std::iter::once(out.y)).fold((f32::MAX, f32::MIN), |(a, b), y| (a.min(y), b.max(y)));
            cv.wire(&[pos2(gx, lo), pos2(gx, hi)], false);
            for (p, active) in &list {
                if *active {
                    cv.wire(&[pos2(gx, out.y), pos2(gx, p.y)], true);
                }
                if (p.x - gx).abs() > 0.5 {
                    cv.wire(&[pos2(gx, p.y), *p], *active);
                }
                cv.dot(pos2(gx, p.y), *active);
            }
        }

        // ---- peripheral clock muxes, MCO, LSCO, other choices ----
        let grid_top = sources_bottom.max(pll_bottom).max(core_bottom) + 50.0;
        self.draw_mux_grid(cv, tree, grid_top)
    }

    /// One PLL frame at `y`. Returns (height, input point of its first box, output anchors).
    fn draw_pll(&self, cv: &mut Cv, y: f32, d: &PllDraw) -> (f32, Pos2, Vec<(char, Pos2, bool)>) {
        let on = d.enabled;
        let h = (30.0 + d.outs.len().max(1) as f32 * 34.0 + 6.0).max(84.0);
        cv.frame(PLL_X, y, PLL_W, h, on);
        cv.text(pos2(PLL_X + 22.0, y + 5.0), Align2::LEFT_TOP, &d.title, 10.5, if on { theme::TEXT } else { OFF_TEXT });
        let toggle = if d.main { Edit::PllEnabled(!on) } else { Edit::AuxEnabled(d.name.clone(), !on) };
        cv.check(PLL_X + 8.0, y + 6.0, on, Action::Edit(toggle), "Enable this PLL");
        let row = y + 38.0;

        // /M
        let mut x = PLL_X + 12.0;
        let m_in = pos2(x, row + BOX_H / 2.0);
        let mut prev = None;
        if let Some((sel, cur)) = &d.m {
            cv.caption(x, row, "/M");
            let target = if d.main { Target::Role(Role::PllM) } else { Target::Value };
            let action = Action::Popup(PopupKind::Choose { sel: (*sel).clone(), target, current: cur.clone() });
            prev = Some(cv.sbox(x, row, 48.0, &pretty(cur), on, Some(action), &sel.key()));
            x += 66.0;
        } else if let Some(shared) = &d.m_shared {
            cv.caption(x, row, "/M (shared)");
            prev = Some(cv.sbox(x, row, 48.0, shared, false, None, "Uses the main PLL's /M"));
            x += 66.0;
        }
        // *N
        let (nsel, ncur) = &d.n;
        if let Some(p) = prev {
            cv.wire(&[p, pos2(x, p.y)], on);
        }
        cv.caption(x, row, "*N");
        let target = if d.main { Target::Role(Role::PllN) } else { Target::Value };
        let action = Action::Popup(PopupKind::Choose { sel: (*nsel).clone(), target, current: ncur.clone() });
        let n_out = cv.sbox(x, row, 56.0, &pretty(ncur), on, Some(action), &nsel.key());
        cv.text(pos2(x, row + BOX_H + 3.0), Align2::LEFT_TOP, format!("in {}", format_hz(d.pll_in)), 8.5, LABEL);
        cv.text(pos2(x, row + BOX_H + 14.0), Align2::LEFT_TOP, format!("VCO {}", format_hz(d.vco)), 8.5, if on { theme::ACCENT } else { LABEL });

        // Outputs: /P /Q /R
        let trunk = PLL_X + 150.0;
        let mut anchors = Vec::new();
        let mut last_y = n_out.y;
        for (i, (letter, sel, cur, active, f, role)) in d.outs.iter().enumerate() {
            let by = y + 36.0 + i as f32 * 34.0;
            let cy = by + BOX_H / 2.0;
            cv.wire(&[pos2(trunk, cy), pos2(trunk + 8.0, cy)], *active);
            cv.dot(pos2(trunk, cy), *active);
            last_y = last_y.max(cy);
            cv.caption(trunk + 8.0, by, &format!("/{letter}"));
            let target = match role {
                Some(r) => Target::Role(*r),
                None => Target::Value,
            };
            let action = Action::Popup(PopupKind::Choose { sel: (*sel).clone(), target, current: cur.clone() });
            let p = cv.sbox(trunk + 8.0, by, 42.0, &pretty(cur), *active, Some(action), &sel.key());
            cv.wire(&[p, pos2(PLL_X + 210.0, p.y)], *active);
            let name = format!("{}_{letter}", d.name);
            cv.caption(PLL_X + 210.0, by, &name);
            let end = cv.vbox(PLL_X + 210.0, by, 72.0, f.unwrap_or(0.0), *active, None, &name);
            if let (true, Some(role)) = (d.main, role) {
                cv.check(PLL_X + PLL_W - 12.0, by - 11.0, *active, Action::Edit(Edit::PllOutput(*role, !*active)), "Enable this output (always on when it drives SYSCLK or a peripheral)");
            }
            let out = pos2(PLL_X + PLL_W, end.y);
            cv.wire(&[end, out], *active);
            anchors.push((*letter, out, *active));
        }
        cv.wire(&[n_out, pos2(trunk, n_out.y), pos2(trunk, last_y.max(n_out.y))], on);
        if anchors.is_empty() {
            // F0/F1 style: the multiplier output is the PLL clock.
            let out = pos2(PLL_X + PLL_W, n_out.y);
            cv.wire(&[n_out, out], on);
            anchors.push(('P', out, on));
        }
        (h, m_in, anchors)
    }

    /// Peripheral kernel muxes, clock outputs and other RCC choices, in a masonry grid.
    fn draw_mux_grid(&self, cv: &mut Cv, tree: &ClockTree, top: f32) -> f32 {
        let clk = &self.project.clock;
        let res = &self.clocks;
        cv.text(pos2(0.0, top - 22.0), Align2::LEFT_TOP, "PERIPHERAL CLOCK MUXES  ·  CLOCK OUTPUTS", 9.0, theme::TEXT_DIM);

        // Peripheral users of every mux.
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

        enum Block<'a> {
            Mux { sel: &'a Selector, to: String, title: String, used: bool, pre: Option<&'a Selector>, hz: f64 },
            Misc(&'a Selector),
        }
        let mut blocks: Vec<Block> = Vec::new();
        for ((sel, pre), f) in tree.mcos.iter().zip(&res.mco) {
            let pin = sel.field.trim_end_matches("SEL").to_string();
            blocks.push(Block::Mux { sel, to: pin.clone(), title: format!("{pin} source Mux"), used: *f > 0.0, pre: pre.as_ref(), hz: *f });
        }
        for (key, sel) in tree.muxes.iter().filter(|(k, _)| k.ends_with(".LSCOSEL")) {
            blocks.push(Block::Mux { sel, to: "To LSCO".into(), title: "LSCO Mux".into(), used: false, pre: None, hz: res.kernel.get(key).copied().unwrap_or(0.0) });
        }
        let mut kernel: Vec<(String, Block)> = tree
            .muxes
            .iter()
            .filter(|(k, _)| !ClockTree::is_system_mux(k))
            .map(|(key, sel)| {
                let list = users.get(key).cloned().unwrap_or_default();
                let names: Vec<&str> = list.iter().map(|(n, _)| n.as_str()).collect();
                let to = if names.is_empty() {
                    format!("To {}", sel.field.trim_end_matches("SEL"))
                } else if names.join(", ").len() > 20 && names.len() > 1 {
                    format!("To {} +{}", names[0], names.len() - 1)
                } else {
                    format!("To {}", names.join(", "))
                };
                let block = Block::Mux {
                    sel,
                    title: format!("{} Clock Mux", sel.field.trim_end_matches("SEL")),
                    to: to.clone(),
                    used: list.iter().any(|(_, e)| *e),
                    pre: None,
                    hz: res.kernel.get(key).copied().unwrap_or(0.0),
                };
                (to, block)
            })
            .collect();
        kernel.sort_by(|a, b| a.0.cmp(&b.0));
        blocks.extend(kernel.into_iter().map(|(_, b)| b));
        blocks.extend(tree.misc.iter().map(Block::Misc));

        let col_w = CANVAS_W / GRID_COLS as f32;
        let mut heights = [top; GRID_COLS];
        for block in blocks {
            let col = (0..GRID_COLS).min_by(|a, b| heights[*a].total_cmp(&heights[*b])).unwrap_or(0);
            let bx = col as f32 * col_w;
            let by = heights[col];
            let h = match block {
                Block::Mux { sel, to, title, used, pre, hz } => {
                    let cur = clk.value(sel).to_string();
                    let inputs: Vec<MuxIn> = sel
                        .variants
                        .iter()
                        .map(|v| MuxIn {
                            label: input_label(&v.name),
                            selected: v.name == cur,
                            action: Action::Edit(Edit::Value(sel.clone(), v.name.clone())),
                            tip: match res.resolve(&v.name) {
                                Some(f) => format!("{} = {}  ({})", sel.key(), v.name, format_hz(f)),
                                None => format!("{} = {}  (external clock)", sel.key(), v.name),
                            },
                        })
                        .collect();
                    let mx = bx + 96.0;
                    cv.text(pos2(bx, by), Align2::LEFT_TOP, &title, 9.5, if used { theme::TEXT } else { LABEL });
                    let active = hz > 0.0;
                    let (attach, out, mb) = cv.mux(mx, by + 16.0, inputs, used || active);
                    for (v, at) in sel.variants.iter().zip(&attach) {
                        let selected = v.name == cur;
                        cv.wire(&[pos2(bx + 30.0, at.y), *at], selected && active);
                    }
                    let mut p = out;
                    if let Some(pre) = pre {
                        let pcur = clk.value(pre).to_string();
                        let action = Action::Popup(PopupKind::Choose { sel: pre.clone(), target: Target::Value, current: pcur.clone() });
                        cv.wire(&[p, pos2(mx + 40.0, p.y)], active);
                        p = cv.sbox(mx + 40.0, p.y - BOX_H / 2.0, 40.0, &pretty(&pcur), active, Some(action), &pre.key());
                    }
                    let vx = mx + if pre.is_some() { 90.0 } else { 44.0 };
                    cv.wire(&[p, pos2(vx, p.y)], active);
                    let known = res.resolve(&cur).is_some();
                    let end = cv.vbox(vx, p.y - BOX_H / 2.0, 72.0, hz, active, None, &sel.key());
                    cv.text(
                        end + vec2(6.0, 0.0),
                        Align2::LEFT_CENTER,
                        if known { to.as_str() } else { "external clock" },
                        9.5,
                        if used { theme::TEXT } else { LABEL },
                    );
                    mb - by + 18.0
                }
                Block::Misc(sel) => {
                    let title = match sel.field.as_str() {
                        "STOPWUCK" => "Wake-up clock from Stop",
                        "STOPKERWUCK" => "Kernel wake-up from Stop",
                        "TIMPRE" => "Timer clock prescaler",
                        "LSIPREDIV" => "LSI prescaler",
                        other => other,
                    };
                    cv.text(pos2(bx, by), Align2::LEFT_TOP, title, 9.5, LABEL);
                    let cur = clk.value(sel).to_string();
                    let action = Action::Popup(PopupKind::Choose { sel: sel.clone(), target: Target::Value, current: cur.clone() });
                    cv.sbox(bx, by + 16.0, 110.0, &pretty(&cur), true, Some(action), &sel.key());
                    50.0
                }
            };
            heights[col] = by + h;
        }
        heights.iter().copied().fold(top, f32::max)
    }
}
