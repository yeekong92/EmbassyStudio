//! Schematic tab: the imported schematic as a background with one interactive
//! node glued beside every MCU pin.
//!
//! * **Edit mode** (calibration): nodes can be dragged, added, turned and
//!   removed; *Save* writes their relative positions to the project file.
//! * **View mode** (configuration): nodes are locked; clicking one assigns the
//!   pin's function, exactly like clicking the pin on the package drawing.
//!
//! Background and nodes share one transform (`origin + position × scale`), so
//! they pan and zoom as a single picture.

use std::collections::{BTreeSet, HashMap};
use std::f32::consts::FRAC_PI_2;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::Editor;
use super::pinout::{PinPopup, PinPopupKind, draw_grid};
use crate::app::Action;
use crate::db::{Chip, gpio_name};
use crate::project::PinMode;
use crate::schematic::render::SheetImage;
use crate::schematic::{self, Drawing, LabelDir, Node, PinNames};
use crate::theme;

const OUTLINE: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
const TAG_BG: Color32 = Color32::from_rgba_premultiplied(0x18, 0x1a, 0x20, 0xee);

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    View,
    Edit,
}

/// Working copy of a sheet's nodes while calibrating.
struct Draft {
    nodes: Vec<Node>,
    node_size: f32,
    changed: bool,
}

struct ImportJob {
    rx: Receiver<Result<Vec<Drawing>, String>>,
    file: String,
}

pub struct SchState {
    sheet: usize,
    mode: Mode,
    zoom: f32,
    pan: Vec2,
    /// Reset the view to show the whole sheet.
    fit: bool,
    /// When the view last changed (sharp re-renders wait for it to settle).
    moved_at: f64,
    /// Backgrounds by drawing file.
    images: HashMap<String, SheetImage>,
    draft: Option<Draft>,
    /// Node being dragged (Edit mode).
    drag: Option<usize>,
    /// Node the context menu belongs to (Edit mode).
    menu_node: Option<usize>,
    /// Pin waiting for a click that places its node (Edit mode).
    placing: Option<String>,
    /// Selected pin name.
    selected: Option<String>,
    import: Option<ImportJob>,
    /// Last painted transform: screen position of the sheet's corner, points per
    /// drawing unit and the sheet size in drawing units.
    view: Option<(Pos2, f32, Vec2)>,
}

impl Default for SchState {
    fn default() -> Self {
        Self {
            sheet: 0,
            mode: Mode::View,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            moved_at: 0.0,
            images: HashMap::new(),
            draft: None,
            drag: None,
            menu_node: None,
            placing: None,
            selected: None,
            import: None,
            view: None,
        }
    }
}

/// Paint a label running away from a node in direction `dir`.
fn tag(painter: &egui::Painter, node: Pos2, gap: f32, dir: LabelDir, text: &str, size: f32, fg: Color32) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(size), fg);
    let (w, h) = (galley.size().x, galley.size().y);
    let pad = vec2(4.0, 1.0);
    let along = w + pad.x * 2.0;
    let across = h + pad.y * 2.0;
    let (rect, text_pos, angle) = match dir {
        LabelDir::Right => {
            let r = Rect::from_min_size(pos2(node.x + gap, node.y - across / 2.0), vec2(along, across));
            (r, r.min + pad, 0.0)
        }
        LabelDir::Left => {
            let r = Rect::from_min_size(pos2(node.x - gap - along, node.y - across / 2.0), vec2(along, across));
            (r, r.min + pad, 0.0)
        }
        // Vertical labels read bottom-to-top, like rotated text on a schematic.
        LabelDir::Up => {
            let r = Rect::from_min_size(pos2(node.x - across / 2.0, node.y - gap - along), vec2(across, along));
            (r, pos2(r.left() + pad.y, r.bottom() - pad.x), -FRAC_PI_2)
        }
        LabelDir::Down => {
            let r = Rect::from_min_size(pos2(node.x - across / 2.0, node.y + gap), vec2(across, along));
            (r, pos2(r.left() + pad.y, r.bottom() - pad.x), -FRAC_PI_2)
        }
    };
    painter.rect(rect, 3.0, TAG_BG, Stroke::new(1.0, fg.gamma_multiply(0.7)), StrokeKind::Inside);
    painter.add(egui::epaint::TextShape::new(text_pos, galley, fg).with_angle(angle));
}

impl Editor {
    fn pin_names(&self, chip: &Chip) -> PinNames {
        chip.package(&self.project.package).or(chip.packages.first()).map(PinNames::from_package).unwrap_or_default()
    }

    /// Write the calibration in progress into the project (it is saved with it).
    pub(super) fn schematic_commit(&mut self) {
        let Some(draft) = &mut self.sch.draft else { return };
        if !std::mem::take(&mut draft.changed) {
            return;
        }
        if let Some(sheet) = self.project.schematic.sheets.get_mut(self.sch.sheet) {
            sheet.nodes = draft.nodes.iter().map(Node::rounded).collect();
            sheet.node_size = draft.node_size;
            self.dirty = true;
        }
    }

    fn schematic_enter_edit(&mut self) {
        let Some(sheet) = self.project.schematic.sheets.get(self.sch.sheet) else { return };
        self.sch.draft = Some(Draft { nodes: sheet.nodes.clone(), node_size: sheet.node_size, changed: false });
        self.sch.mode = Mode::Edit;
        self.sch.drag = None;
        self.sch.menu_node = None;
        self.sch.placing = None;
        self.pin_popup = None;
    }

    fn schematic_leave_edit(&mut self) {
        self.sch.draft = None;
        self.sch.mode = Mode::View;
        self.sch.drag = None;
        self.sch.menu_node = None;
        self.sch.placing = None;
    }

    fn schematic_unsaved(&self) -> bool {
        self.sch.draft.as_ref().is_some_and(|d| d.changed)
    }

    // ---- import ----

    fn schematic_start_import(&mut self, ctx: &egui::Context, pins: PinNames) {
        let picked = rfd::FileDialog::new()
            .set_title("Import schematic")
            .add_filter("Schematic (SVG, PDF, KiCad, Altium)", schematic::EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file();
        let Some(path) = picked else { return };
        let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // A malformed file must not take the application down with it.
            let res = std::panic::catch_unwind(|| schematic::import(&path, &pins))
                .map_err(|_| "the file could not be processed".to_string())
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tx.send(res);
            ctx.request_repaint();
        });
        self.sch.import = Some(ImportJob { rx, file });
    }

    fn schematic_poll_import(&mut self, pins: &PinNames, actions: &mut Vec<Action>) {
        let Some(job) = &self.sch.import else { return };
        let res = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the importer stopped unexpectedly".to_string()),
        };
        let file = job.file.clone();
        self.sch.import = None;
        let drawings = match res {
            Ok(d) => d,
            Err(e) => {
                actions.push(Action::Notify(format!("Could not import {file}: {e}"), true));
                return;
            }
        };
        let done = match schematic::add_to_project(&mut self.project.schematic, &self.dir, &file, drawings, pins) {
            Ok(d) => d,
            Err(e) => {
                actions.push(Action::Notify(format!("Could not import {file}: {e:#}"), true));
                return;
            }
        };
        for e in &done.errors {
            actions.push(Action::Notify(format!("Could not write {e}"), true));
        }
        // A sheet imported again gets a fresh background (its nodes are kept).
        for f in &done.files {
            self.sch.images.remove(f);
        }
        let Some(idx) = done.sheet else { return };
        let (imported, found) = (done.files.len(), done.gpios);
        self.mark_dirty();
        self.sch.sheet = idx;
        self.sch.fit = true;
        self.sch.selected = None;
        // Auto-location is a starting point: go straight to calibration.
        self.schematic_enter_edit();
        let sheets = if imported == 1 { String::new() } else { format!(" on {imported} sheets") };
        let (text, warn) = if found.is_empty() {
            (
                format!("Imported {file}, but none of the {} pin names were found in its text. Place the nodes by hand from the list on the right.", self.project.chip),
                true,
            )
        } else {
            (format!("Imported {file}: located {} of {} I/O pins{sheets}. Drag nodes to fine-tune, then Save.", found.len(), pins.gpios.len()), false)
        };
        actions.push(Action::Notify(text, warn));
    }

    fn schematic_remove_sheet(&mut self, idx: usize) {
        if idx >= self.project.schematic.sheets.len() {
            return;
        }
        let sheet = self.project.schematic.sheets.remove(idx);
        self.sch.images.remove(&sheet.file);
        // The drawing is a copy made by the import; remove it with the sheet.
        let ours = sheet.file.starts_with(&format!("{}/", schematic::DIR)) && !sheet.file.contains("..");
        if ours && !self.project.schematic.sheets.iter().any(|s| s.file == sheet.file) {
            std::fs::remove_file(self.dir.join(&sheet.file)).ok();
        }
        self.schematic_leave_edit();
        self.sch.sheet = self.sch.sheet.min(self.project.schematic.sheets.len().saturating_sub(1));
        self.sch.fit = true;
        self.mark_dirty();
    }

    // ---- central view ----

    pub(super) fn schematic_view(&mut self, ui: &mut egui::Ui, chip: &Chip, actions: &mut Vec<Action>) {
        let pins = self.pin_names(chip);
        self.schematic_poll_import(&pins, actions);
        if self.sch.sheet >= self.project.schematic.sheets.len() {
            self.sch.sheet = 0;
        }
        self.schematic_toolbar(ui, &pins, actions);
        if self.project.schematic.sheets.is_empty() {
            self.schematic_empty(ui, &pins);
        } else {
            self.schematic_canvas(ui, chip, &pins);
        }
    }

    fn schematic_toolbar(&mut self, ui: &mut egui::Ui, pins: &PinNames, actions: &mut Vec<Action>) {
        let unsaved = self.schematic_unsaved();
        let mut switch_to = None;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if let Some(job) = &self.sch.import {
                ui.spinner();
                ui.label(RichText::new(format!("Reading {}…", job.file)).color(theme::TEXT_DIM));
            } else if ui
                .add_enabled(!unsaved, egui::Button::new("📂 Import schematic…"))
                .on_hover_text("SVG, PDF, KiCad .kicad_sch or Altium .SchDoc")
                .on_disabled_hover_text("Save or cancel the calibration first")
                .clicked()
            {
                self.schematic_start_import(ui.ctx(), pins.clone());
            }
            if !self.project.schematic.sheets.is_empty() {
                ui.separator();
                for (i, s) in self.project.schematic.sheets.iter().enumerate() {
                    let current = self.sch.sheet == i;
                    let r = ui
                        .add_enabled(current || !unsaved, egui::Button::selectable(current, RichText::new(&s.name).size(13.0)))
                        .on_hover_text(format!("{} · {} nodes", s.source, s.nodes.len()))
                        .on_disabled_hover_text("Save or cancel the calibration first");
                    if r.clicked() && !current {
                        switch_to = Some(i);
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if self.project.schematic.sheets.is_empty() {
                    return;
                }
                if ui.button("Fit").on_hover_text("Show the whole sheet").clicked() {
                    self.sch.fit = true;
                }
                ui.separator();
                match self.sch.mode {
                    Mode::View => {
                        if ui.button("✏ Edit positions").on_hover_text("Calibrate: drag the nodes onto their pins").clicked() {
                            self.schematic_enter_edit();
                        }
                        ui.label(RichText::new("VIEW").size(11.0).strong().color(theme::TEXT_DIM));
                    }
                    Mode::Edit => {
                        if ui.button("Cancel").on_hover_text("Discard the changes made in this calibration").clicked() {
                            self.schematic_leave_edit();
                        }
                        if ui.add(theme::primary_button("Save")).on_hover_text("Store the node positions in the project file").clicked() {
                            if let Some(d) = &mut self.sch.draft {
                                d.changed = true;
                            }
                            self.schematic_commit();
                            self.schematic_leave_edit();
                            self.save(actions);
                        }
                        ui.label(RichText::new("EDIT · calibration").size(11.0).strong().color(theme::WARN));
                    }
                }
            });
        });
        ui.add_space(4.0);
        if let Some(i) = switch_to {
            self.sch.sheet = i;
            self.sch.fit = true;
            self.sch.selected = None;
            if self.sch.mode == Mode::Edit {
                self.schematic_enter_edit();
            }
        }
    }

    fn schematic_empty(&mut self, ui: &mut egui::Ui, pins: &PinNames) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.28).max(20.0));
            theme::heading(ui, "Configure pins on your schematic");
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "Import the sheet that shows the {}. Its pins are located from the text on the drawing\nand an interactive node is placed beside each one.",
                    self.project.chip
                ))
                .color(theme::TEXT_DIM),
            );
            ui.add_space(14.0);
            if self.sch.import.is_none() && ui.add(theme::primary_button("📂  Import schematic…")).clicked() {
                self.schematic_start_import(ui.ctx(), pins.clone());
            }
            ui.add_space(10.0);
            ui.label(RichText::new("SVG  ·  PDF  ·  KiCad .kicad_sch  ·  Altium .SchDoc").size(11.5).color(theme::TEXT_DIM));
        });
    }

    /// Fill colour of a node, as on the package drawing.
    fn node_color(&self, pin: &str, gpio: bool, conflicts: &BTreeSet<String>) -> Color32 {
        if !gpio {
            return theme::PIN_POWER;
        }
        match self.project.pins.get(pin) {
            _ if conflicts.contains(pin) => theme::PIN_CONFLICT,
            Some(c) if matches!(c.mode, PinMode::Peripheral { .. }) => theme::PIN_PERIPH,
            Some(_) => theme::PIN_GPIO,
            None => theme::PIN_FREE,
        }
    }

    fn schematic_canvas(&mut self, ui: &mut egui::Ui, chip: &Chip, pins: &PinNames) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, theme::BG);
        draw_grid(&painter, rect);

        let file = self.project.schematic.sheets[self.sch.sheet].file.clone();
        let mut image = match self.sch.images.remove(&file) {
            Some(i) => i,
            None => SheetImage::load(ui.ctx(), self.dir.join(&file)),
        };
        image.poll(ui.ctx());
        let Some(size) = image.size else {
            let (text, color) = match &image.error {
                Some(e) => (format!("Could not show the drawing.\n{e}\nImport the schematic again to restore it."), theme::ERROR),
                None => ("Rendering the schematic…".to_string(), theme::TEXT_DIM),
            };
            painter.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(14.0), color);
            self.sch.images.insert(file, image);
            return;
        };

        // ---- view transform: fit to the canvas, then user zoom and pan ----
        let now = ui.input(|i| i.time);
        let editing = self.sch.mode == Mode::Edit;
        if self.sch.fit {
            self.sch.fit = false;
            self.sch.zoom = 1.0;
            self.sch.pan = Vec2::ZERO;
            self.sch.moved_at = now;
        }
        let fit = (rect.width() / size.x).min(rect.height() / size.y) * 0.95;
        if resp.hovered() {
            let (scroll, zoom_delta, hover) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
            let factor = zoom_delta * (scroll / 400.0).exp();
            if let (true, Some(hp)) = (factor != 1.0, hover) {
                let old = fit * self.sch.zoom;
                let zoom = (self.sch.zoom * factor).clamp(0.3, 120.0);
                // Keep the point under the cursor fixed.
                let centre = rect.center() + self.sch.pan;
                let model = (hp - centre) / old;
                self.sch.pan = hp - rect.center() - model * (fit * zoom);
                self.sch.zoom = zoom;
                self.sch.moved_at = now;
            }
        }
        let scale = fit * self.sch.zoom;
        let origin = rect.center() + self.sch.pan - size * scale / 2.0;
        let to_screen = |n: &Node| origin + vec2(n.x * size.x, n.y * size.y) * scale;
        let to_sheet = |p: Pos2| ((p.x - origin.x) / (size.x * scale), (p.y - origin.y) / (size.y * scale));

        self.sch.view = Some((origin, scale, size));
        image.paint(ui, rect, origin, scale, self.sch.moved_at);
        let ready = image.ready();
        self.sch.images.insert(file, image);
        if !ready {
            painter.text(rect.center(), Align2::CENTER_CENTER, "Rendering the schematic…", FontId::proportional(14.0), Color32::from_gray(120));
        }

        // ---- nodes ----
        let sheet = &self.project.schematic.sheets[self.sch.sheet];
        let (mut nodes, node_size) = match &self.sch.draft {
            Some(d) if editing => (d.nodes.clone(), d.node_size),
            _ => (sheet.nodes.clone(), sheet.node_size),
        };
        let natural = node_size * size.x * scale;
        // Zoomed out, nodes shrink with the drawing instead of piling up.
        let radius = (natural / 2.0).clamp(1.6, 11.0);
        let font = (natural * 0.9).clamp(9.5, 13.0);
        let show_tags = natural >= 6.5;
        let pointer = resp.hover_pos();
        let node_at = |nodes: &[Node], p: Pos2| -> Option<usize> {
            let reach = (radius + 3.0).max(6.0);
            nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (i, to_screen(n).distance(p)))
                .filter(|(_, d)| *d <= reach)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        let hovered = pointer.filter(|_| self.sch.placing.is_none()).and_then(|p| node_at(&nodes, p));

        // ---- interaction ----
        let mut changed = false;
        if editing {
            if let Some(pin) = self.sch.placing.clone() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.secondary_clicked() {
                    self.sch.placing = None;
                } else if let (true, Some(p)) = (resp.clicked(), resp.interact_pointer_pos()) {
                    let (x, y) = to_sheet(p);
                    if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
                        nodes.push(Node { pin: pin.clone(), x, y, label: LabelDir::Right });
                        self.sch.selected = Some(pin);
                        self.sch.placing = None;
                        changed = true;
                    }
                }
                if resp.dragged() {
                    self.sch.pan += resp.drag_delta();
                    self.sch.moved_at = now;
                }
            } else {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    self.sch.drag = ui.input(|i| i.pointer.press_origin()).and_then(|p| node_at(&nodes, p));
                    if let Some(i) = self.sch.drag {
                        self.sch.selected = Some(nodes[i].pin.clone());
                    }
                }
                if resp.dragged() {
                    match self.sch.drag.filter(|i| *i < nodes.len()) {
                        Some(i) => {
                            let d = resp.drag_delta();
                            nodes[i].x = (nodes[i].x + d.x / (size.x * scale)).clamp(0.0, 1.0);
                            nodes[i].y = (nodes[i].y + d.y / (size.y * scale)).clamp(0.0, 1.0);
                            changed = true;
                        }
                        None => {
                            self.sch.pan += resp.drag_delta();
                            self.sch.moved_at = now;
                        }
                    }
                }
                if resp.drag_stopped() {
                    self.sch.drag = None;
                }
                if self.sch.drag.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                } else if hovered.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if resp.clicked() {
                    self.sch.selected = hovered.map(|i| nodes[i].pin.clone());
                }
                if resp.secondary_clicked() {
                    self.sch.menu_node = hovered;
                    if let Some(i) = hovered {
                        self.sch.selected = Some(nodes[i].pin.clone());
                    }
                }
                // Keyboard on the node under the pointer.
                if let Some(i) = hovered.filter(|_| self.sch.drag.is_none()) {
                    let (turn, delete) = ui.input(|i| (i.key_pressed(egui::Key::R), i.key_pressed(egui::Key::Delete)));
                    if turn {
                        nodes[i].label = nodes[i].label.rotated();
                        changed = true;
                    } else if delete {
                        nodes.remove(i);
                        self.sch.menu_node = None;
                        changed = true;
                    }
                }
                if let Some(i) = self.sch.menu_node.filter(|i| *i < nodes.len() && !changed) {
                    let mut close = false;
                    resp.context_menu(|ui| {
                        ui.label(RichText::new(&nodes[i].pin).strong().color(theme::ACCENT));
                        ui.separator();
                        if ui.button("Turn label          R").clicked() {
                            nodes[i].label = nodes[i].label.rotated();
                            changed = true;
                        }
                        if ui.button("Remove node    Del").clicked() {
                            nodes.remove(i);
                            changed = true;
                            close = true;
                            ui.close();
                        }
                    });
                    if close {
                        self.sch.menu_node = None;
                    }
                }
            }
        } else {
            if resp.dragged() {
                self.sch.pan += resp.drag_delta();
                self.sch.moved_at = now;
            }
            let popup_for = |s: &Self, pin: &str, kind| {
                pins.is_gpio(pin).then(|| {
                    let label = s.project.pins.get(pin).map(|c| c.label.clone()).unwrap_or_default();
                    PinPopup::new(pin.to_string(), kind, label)
                })
            };
            match hovered {
                Some(i) => {
                    let pin = nodes[i].pin.clone();
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    if resp.clicked() || resp.secondary_clicked() {
                        let kind = if resp.clicked() { PinPopupKind::Functions } else { PinPopupKind::Label };
                        self.pin_popup = popup_for(self, &pin, kind);
                        self.schematic_select(chip, Some(pin.clone()));
                    }
                    if self.pin_popup.is_none() {
                        let mut tip = pin.clone();
                        if pins.is_gpio(&pin) {
                            match self.project.pins.get(&pin) {
                                Some(cfg) => {
                                    tip.push_str(&format!("  ·  {}", cfg.mode.label()));
                                    if !cfg.label.is_empty() {
                                        tip.push_str(&format!("  “{}”", cfg.label));
                                    }
                                }
                                None => tip.push_str("  ·  not configured"),
                            }
                        } else {
                            tip.push_str("  ·  power / system pin");
                        }
                        resp.clone().on_hover_text_at_pointer(tip);
                    }
                }
                None => {
                    if resp.clicked() || resp.secondary_clicked() {
                        self.schematic_select(chip, None);
                        self.pin_popup = None;
                    }
                    if resp.double_clicked() {
                        self.sch.fit = true;
                    }
                }
            }
        }
        if changed {
            if let Some(d) = &mut self.sch.draft {
                d.nodes = nodes.clone();
                d.changed = true;
            }
        }

        // ---- painting ----
        let conflicts = self.conflicts();
        let candidates: BTreeSet<&str> = self
            .selected_periph
            .as_ref()
            .and_then(|p| chip.peripheral(p))
            .map(|p| p.pins.iter().map(|x| x.pin.as_str()).collect())
            .unwrap_or_default();
        let hovered = pointer.filter(|_| self.sch.placing.is_none()).and_then(|p| node_at(&nodes, p));
        for (i, n) in nodes.iter().enumerate() {
            let pos = to_screen(n);
            if !rect.expand(200.0).contains(pos) {
                continue;
            }
            let gpio = pins.is_gpio(&n.pin);
            let color = self.node_color(&n.pin, gpio, &conflicts);
            let is_hover = hovered == Some(i) || self.sch.drag == Some(i);
            let is_selected = self.sch.selected.as_deref() == Some(n.pin.as_str());
            let r = if !gpio { radius * 0.75 } else if is_hover { radius * 1.25 } else { radius };

            if is_selected {
                painter.circle_stroke(pos, r + 4.0, Stroke::new(2.0, theme::ACCENT));
            } else if candidates.contains(n.pin.as_str()) {
                painter.circle_stroke(pos, r + 3.5, Stroke::new(1.5, theme::ACCENT.gamma_multiply(0.8)));
            }
            let outline = if is_hover { Stroke::new(2.0, theme::ACCENT) } else { Stroke::new((r * 0.22).clamp(0.5, 2.0), OUTLINE) };
            if editing {
                // A handle: ring with a centre mark, so the pin under it stays visible.
                painter.circle(pos, r, color.gamma_multiply(0.55), outline);
                painter.circle_filled(pos, (r * 0.28).max(0.8), OUTLINE);
            } else {
                painter.circle(pos, r, color, outline);
            }

            let text = if editing {
                Some(n.pin.clone())
            } else {
                self.project.pins.get(&n.pin).filter(|_| gpio).map(|cfg| {
                    if cfg.label.is_empty() { cfg.mode.label() } else { format!("{}  [{}]", cfg.label, cfg.mode.label()) }
                })
            };
            if let Some(text) = text.filter(|_| show_tags || is_hover || is_selected) {
                tag(&painter, pos, r + 3.0, n.label, &text, font, if gpio { color } else { theme::TEXT_DIM });
            }
        }
        if !editing {
            self.show_pin_popup(ui);
        }

        // ---- overlays ----
        if editing {
            painter.rect_stroke(rect.shrink(1.0), 0.0, Stroke::new(2.0, theme::WARN.gamma_multiply(0.7)), StrokeKind::Inside);
        }
        let help = match (&self.sch.placing, editing) {
            (Some(pin), _) => format!("Click on the schematic to place {pin}  ·  Esc: cancel"),
            (None, true) => "Drag a node onto its pin  ·  R: turn label  ·  Del / right-click: remove  ·  Drag background: pan  ·  Wheel: zoom".to_string(),
            (None, false) => "Left-click a node: pin functions  ·  Right-click: user label  ·  Wheel: zoom  ·  Drag: pan  ·  Double-click: fit".to_string(),
        };
        let shadowed = |pos: Pos2, align: Align2, text: String, color: Color32| {
            let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), color);
            let r = align.anchor_size(pos, galley.size());
            painter.rect_filled(r.expand2(vec2(6.0, 3.0)), 4.0, theme::BG.gamma_multiply(0.85));
            painter.galley(r.min, galley, color);
        };
        shadowed(rect.left_bottom() + vec2(14.0, -12.0), Align2::LEFT_BOTTOM, help, theme::TEXT_DIM);
        let placed = nodes.iter().filter(|n| pins.is_gpio(&n.pin)).map(|n| n.pin.as_str()).collect::<BTreeSet<_>>().len();
        shadowed(
            rect.right_bottom() + vec2(-14.0, -12.0),
            Align2::RIGHT_BOTTOM,
            format!("{placed} of {} I/O pins on this sheet  ·  {:.0}%", pins.gpios.len(), self.sch.zoom * 100.0),
            theme::TEXT_DIM,
        );
    }

    /// Select a pin here and on the package drawing.
    fn schematic_select(&mut self, chip: &Chip, pin: Option<String>) {
        if let (Some(name), Some(pkg)) = (&pin, chip.package(&self.project.package).or(chip.packages.first())) {
            if let Some(p) = pkg.pins.iter().find(|p| p.signals.iter().any(|s| gpio_name(s).as_deref() == Some(name.as_str()))) {
                self.selected_pin = Some(p.position.clone());
            }
        }
        self.sch.selected = pin;
    }

    // ---- side panel ----

    pub(super) fn schematic_panel(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        let pins = self.pin_names(chip);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            if self.sch.mode == Mode::Edit && self.sch.draft.is_some() {
                self.schematic_edit_panel(ui, &pins);
            } else {
                self.schematic_view_panel(ui, &pins);
            }
        });
    }

    fn schematic_view_panel(&mut self, ui: &mut egui::Ui, pins: &PinNames) {
        theme::heading(ui, "Pin");
        ui.add_space(4.0);
        match self.sch.selected.clone() {
            Some(pin) if pins.is_gpio(&pin) => self.gpio_settings(ui, &pin),
            Some(pin) => {
                ui.label(RichText::new(&pin).strong().size(16.0).color(theme::TEXT));
                ui.label(RichText::new("Power / system pin — not configurable.").color(theme::TEXT_DIM));
            }
            None if self.project.schematic.sheets.is_empty() => {
                ui.label(RichText::new("Import a schematic to configure pins where they are drawn.").color(theme::TEXT_DIM));
            }
            None => {
                ui.label(RichText::new("Click a node on the schematic to configure its pin.").color(theme::TEXT_DIM));
            }
        }

        if !self.project.schematic.sheets.is_empty() {
            ui.add_space(16.0);
            ui.separator();
            theme::section(ui, "Sheets");
            let mut remove = None;
            for (i, s) in self.project.schematic.sheets.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.sch.sheet == i, &s.name).on_hover_text(&s.file).clicked() && self.sch.sheet != i {
                        self.sch.sheet = i;
                        self.sch.fit = true;
                    }
                    ui.label(RichText::new(format!("{} nodes", s.nodes.len())).size(11.0).color(theme::TEXT_DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Remove this sheet, its nodes and its drawing from the project").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                self.schematic_remove_sheet(i);
            }
            let placed: BTreeSet<&str> = self.project.schematic.sheets.iter().flat_map(|s| s.nodes.iter().map(|n| n.pin.as_str())).collect();
            let missing = pins.gpios.iter().filter(|g| !placed.contains(g.as_str())).count();
            if missing > 0 && !self.project.schematic.sheets.is_empty() {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("{missing} I/O pins have no node yet. Use “Edit positions” to place them."))
                        .size(11.5)
                        .color(theme::TEXT_DIM),
                );
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
            (theme::PIN_POWER, "Power / system"),
        ] {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                ui.painter().circle(r.center(), 6.0, c, Stroke::new(1.0, OUTLINE));
                ui.label(RichText::new(t).size(12.0));
            });
        }
        ui.add_space(4.0);
        ui.label(RichText::new("A ring marks pins the selected peripheral can use.").size(11.0).color(theme::TEXT_DIM));
    }

    fn schematic_edit_panel(&mut self, ui: &mut egui::Ui, pins: &PinNames) {
        theme::heading(ui, "Calibration");
        ui.add_space(4.0);
        ui.label(
            RichText::new("Auto-location reads the text of the drawing, so rotated or offset pin names can put a node slightly off. Drag each node beside its pin, then Save.")
                .size(12.0)
                .color(theme::TEXT_DIM),
        );
        ui.add_space(10.0);

        let sheet = self.sch.sheet;
        // Pins that already have a node, here or on another sheet.
        let elsewhere: BTreeSet<String> = self
            .project
            .schematic
            .sheets
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != sheet)
            .flat_map(|(_, s)| s.nodes.iter().map(|n| n.pin.clone()))
            .collect();
        let Some(draft) = &mut self.sch.draft else { return };

        theme::section(ui, "Node size");
        let mut permille = draft.node_size * 1000.0;
        if ui.add(egui::Slider::new(&mut permille, 1.0..=30.0).fixed_decimals(1).suffix(" ‰ of width")).changed() {
            draft.node_size = permille / 1000.0;
            draft.changed = true;
        }

        ui.add_space(12.0);
        let here: BTreeSet<&str> = draft.nodes.iter().map(|n| n.pin.as_str()).collect();
        let unplaced: Vec<&String> = pins.gpios.iter().filter(|g| !here.contains(g.as_str()) && !elsewhere.contains(*g)).collect();
        theme::section(ui, &format!("Unplaced I/O pins ({})", unplaced.len()));
        if unplaced.is_empty() {
            ui.label(RichText::new("Every I/O pin has a node.").size(12.0).color(theme::TEXT_DIM));
        } else {
            ui.label(RichText::new("Pick a pin, then click where it is on the schematic.").size(12.0).color(theme::TEXT_DIM));
            ui.add_space(2.0);
            let mut pick = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for g in &unplaced {
                    let active = self.sch.placing.as_deref() == Some(g.as_str());
                    if ui.selectable_label(active, RichText::new(g.as_str()).monospace().size(11.5)).clicked() {
                        pick = Some(if active { None } else { Some((*g).clone()) });
                    }
                }
            });
            if let Some(p) = pick {
                self.sch.placing = p;
            }
        }

        ui.add_space(12.0);
        theme::section(ui, &format!("Nodes on this sheet ({})", draft.nodes.len()));
        if !draft.nodes.is_empty() && ui.button("Remove all nodes").on_hover_text("Start over and place every pin by hand").clicked() {
            draft.nodes.clear();
            draft.changed = true;
            self.sch.menu_node = None;
            self.sch.drag = None;
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("Positions are stored relative to the sheet (0–1), so they stay glued to the drawing at any zoom.")
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
    }
}
