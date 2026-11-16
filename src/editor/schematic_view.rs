//! Schematic tab: the imported schematic as a background with one interactive
//! node glued beside every MCU pin.
//!
//! * **Edit mode** (calibration): nodes can be dragged, added, turned and
//!   removed, optionally snapped to a grid, with undo and redo; *Save* writes
//!   their relative positions to the project file.
//! * **View mode** (configuration): nodes are locked; clicking one chooses the
//!   pin's function and then asks for its label, exactly like clicking the pin
//!   on the package drawing. The side panel lists every pin.
//!
//! Background and nodes share one transform (`origin + position × scale`), so
//! they pan and zoom as a single picture. Right-clicking the canvas opens a
//! menu with the commands of the current mode.

use std::collections::{BTreeSet, HashMap};
use std::f32::consts::FRAC_PI_2;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::Editor;
use super::pinout::{PinPopup, draw_grid};
use crate::app::Action;
use crate::db::{Chip, gpio_name};
use crate::project::PinMode;
use crate::schematic::render::SheetImage;
use crate::schematic::{self, Drawing, LabelDir, Node, PinNames};
use crate::theme;

const OUTLINE: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
const TAG_BG: Color32 = Color32::from_rgba_premultiplied(0x18, 0x1a, 0x20, 0xee);
/// Height of a row in the pin list.
const ROW: f32 = 21.0;
/// Undo steps kept per calibration.
const UNDO_STEPS: usize = 200;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    View,
    Edit,
}

/// What undo and redo restore.
struct Snapshot {
    nodes: Vec<Node>,
    node_size: f32,
}

/// Working copy of a sheet's nodes while calibrating.
struct Draft {
    nodes: Vec<Node>,
    node_size: f32,
    /// Snap grid in drawing units (0 = off).
    grid: f32,
    changed: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The node-size slider is being moved; the move already has its undo step.
    sizing: bool,
    /// Pin pitch of the symbol in drawing units, once the sheet size is known.
    pitch: Option<Option<f32>>,
}

impl Draft {
    fn snapshot(&self) -> Snapshot {
        Snapshot { nodes: self.nodes.clone(), node_size: self.node_size }
    }

    /// Remember the current state as an undo step, before changing it.
    fn checkpoint(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.changed = true;
    }

    /// Undo (or redo) one step. False when there is nothing to step to.
    fn step(&mut self, redo: bool) -> bool {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(s) = from.pop() else { return false };
        to.push(Snapshot { nodes: std::mem::replace(&mut self.nodes, s.nodes), node_size: std::mem::replace(&mut self.node_size, s.node_size) });
        self.changed = true;
        true
    }
}

struct ImportJob {
    rx: Receiver<Result<Vec<Drawing>, String>>,
    path: PathBuf,
    file: String,
    /// Reading a sheet again instead of adding a new one.
    refresh: bool,
}

/// A command from the canvas menu, a sheet's menu or the toolbar.
enum Cmd {
    Fit,
    Edit,
    Import,
    Refresh,
    Close,
    Function(String),
    Label(String),
    Reset(String),
    Turn(usize),
    Remove(usize),
    Undo,
    Redo,
    Grid(f32),
    SnapAll,
    Save,
    Cancel,
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
    /// Node being dragged (Edit mode), where it was grabbed relative to its
    /// centre, and whether it has moved yet.
    drag: Option<usize>,
    drag_offset: Vec2,
    drag_moved: bool,
    /// Node the open canvas menu was opened on.
    menu_node: Option<usize>,
    /// Pin waiting for a click that places its node (Edit mode).
    placing: Option<String>,
    /// Selected pin name.
    selected: Option<String>,
    /// Pin picked in the list: bring its node into view.
    focus: Option<String>,
    /// When a pin was picked in the list (its node pulses for a moment).
    flash_at: f64,
    /// Pin picked on the canvas: scroll the list to it.
    reveal: Option<String>,
    /// Pin list filter.
    filter: String,
    configured_only: bool,
    /// Sheet waiting for the "close this schematic?" answer.
    confirm_close: Option<usize>,
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
            drag_offset: Vec2::ZERO,
            drag_moved: false,
            menu_node: None,
            placing: None,
            selected: None,
            focus: None,
            flash_at: f64::NEG_INFINITY,
            reveal: None,
            filter: String::new(),
            configured_only: false,
            confirm_close: None,
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

/// One line of text cut off with "…" at `width`, centred on `pos.y`.
fn cell(painter: &egui::Painter, pos: Pos2, width: f32, text: &str, font: FontId, color: Color32) {
    if text.is_empty() || width < 8.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, ..Default::default() };
    let galley = painter.layout_job(job);
    painter.galley(pos2(pos.x, pos.y - galley.size().y / 2.0), galley, color);
}

/// A value in drawing units without trailing zeros: `2.54`, `10`.
fn units(v: f32) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Round to three significant figures.
fn tidy(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let mag = 10f32.powf(2.0 - v.log10().floor());
    (v * mag).round() / mag
}

/// The usual distance between neighbouring nodes, in drawing units. In practice
/// this is the pin pitch of the MCU symbol, which makes a natural snap grid.
fn pin_pitch(nodes: &[Node], size: Vec2) -> Option<f32> {
    let at: Vec<Pos2> = nodes.iter().map(|n| pos2(n.x * size.x, n.y * size.y)).collect();
    let mut nearest: Vec<f32> = at
        .iter()
        .enumerate()
        .filter_map(|(i, a)| at.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, b)| a.distance(*b)).filter(|d| *d > 1e-3).min_by(f32::total_cmp))
        .collect();
    if nearest.len() < 4 {
        return None;
    }
    nearest.sort_by(f32::total_cmp);
    Some(tidy(nearest[nearest.len() / 2]))
}

/// Grid pitches offered in the drop-downs, as (label, pitch).
fn grid_choices(pitch: Option<f32>) -> Vec<(String, f32)> {
    let mut out = vec![("Off".to_string(), 0.0)];
    if let Some(p) = pitch {
        for (name, div) in [("Pin pitch", 1.0), ("Half pin pitch", 2.0), ("Quarter pin pitch", 4.0)] {
            out.push((format!("{name}  ·  {}", units(tidy(p / div))), tidy(p / div)));
        }
    }
    out
}

/// Move a relative position onto the grid (pitch in drawing units, origin at
/// the sheet's top-left corner).
fn snap(x: f32, y: f32, size: Vec2, grid: f32) -> (f32, f32) {
    if grid <= 0.0 {
        return (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
    }
    let on = |v: f32, extent: f32| {
        let cells = (extent / grid).floor().max(0.0);
        ((v * extent / grid).round().clamp(0.0, cells) * grid / extent).clamp(0.0, 1.0)
    };
    (on(x, size.x), on(y, size.y))
}

/// Sort key that puts `PA2` before `PA10`.
fn pin_order(name: &str) -> (String, u32) {
    let digits = name.trim_start_matches(|c: char| !c.is_ascii_digit());
    (name[..name.len() - digits.len()].to_string(), digits.parse().unwrap_or(0))
}

struct PinRow {
    pin: String,
    gpio: bool,
    status: &'static str,
    color: Color32,
    function: String,
    label: String,
    /// Has a node on the sheet being shown.
    here: bool,
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
            sheet.grid = draft.grid;
            self.dirty = true;
        }
    }

    fn schematic_enter_edit(&mut self) {
        let Some(sheet) = self.project.schematic.sheets.get(self.sch.sheet) else { return };
        self.sch.draft = Some(Draft {
            nodes: sheet.nodes.clone(),
            node_size: sheet.node_size,
            grid: sheet.grid,
            changed: false,
            undo: Vec::new(),
            redo: Vec::new(),
            sizing: false,
            pitch: None,
        });
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

    /// Store the calibration in the project file and return to View mode.
    fn schematic_save(&mut self, actions: &mut Vec<Action>) {
        if let Some(d) = &mut self.sch.draft {
            d.changed = true;
        }
        self.schematic_commit();
        self.schematic_leave_edit();
        self.save(actions);
    }

    fn schematic_unsaved(&self) -> bool {
        self.sch.draft.as_ref().is_some_and(|d| d.changed)
    }

    fn schematic_step(&mut self, redo: bool) {
        if self.sch.draft.as_mut().is_some_and(|d| d.step(redo)) {
            self.sch.drag = None;
            self.sch.menu_node = None;
        }
    }

    fn schematic_run(&mut self, ctx: &egui::Context, pins: &PinNames, cmd: Cmd, actions: &mut Vec<Action>) {
        let sheet_size = self.sch.view.map(|v| v.2);
        match cmd {
            Cmd::Fit => self.sch.fit = true,
            Cmd::Edit => self.schematic_enter_edit(),
            Cmd::Import => self.schematic_pick_import(ctx, pins.clone()),
            Cmd::Refresh => self.schematic_refresh(ctx, pins.clone(), self.sch.sheet),
            Cmd::Close => self.sch.confirm_close = Some(self.sch.sheet),
            Cmd::Function(pin) => self.pin_popup = Some(PinPopup::functions(pin)),
            Cmd::Label(pin) => {
                let label = self.project.pins.get(&pin).map(|c| c.label.clone()).unwrap_or_default();
                self.pin_popup = Some(PinPopup::label(pin, label));
            }
            Cmd::Reset(pin) => self.assign_pin(&pin, None),
            Cmd::Turn(i) => {
                if let Some(d) = self.sch.draft.as_mut().filter(|d| i < d.nodes.len()) {
                    d.checkpoint();
                    d.nodes[i].label = d.nodes[i].label.rotated();
                }
            }
            Cmd::Remove(i) => {
                if let Some(d) = self.sch.draft.as_mut().filter(|d| i < d.nodes.len()) {
                    d.checkpoint();
                    d.nodes.remove(i);
                }
                self.sch.drag = None;
            }
            Cmd::Undo => self.schematic_step(false),
            Cmd::Redo => self.schematic_step(true),
            Cmd::Grid(g) => {
                if let Some(d) = &mut self.sch.draft {
                    d.grid = if g.is_finite() { g.max(0.0) } else { 0.0 };
                }
            }
            Cmd::SnapAll => {
                if let (Some(d), Some(size)) = (self.sch.draft.as_mut().filter(|d| d.grid > 0.0), sheet_size) {
                    d.checkpoint();
                    for n in &mut d.nodes {
                        (n.x, n.y) = snap(n.x, n.y, size, d.grid);
                    }
                }
            }
            Cmd::Save => self.schematic_save(actions),
            Cmd::Cancel => self.schematic_leave_edit(),
        }
        self.sch.menu_node = None;
        ctx.request_repaint();
    }

    // ---- import, refresh, close ----

    fn schematic_pick_import(&mut self, ctx: &egui::Context, pins: PinNames) {
        let picked = rfd::FileDialog::new()
            .set_title("Import schematic")
            .add_filter("Schematic (SVG, PDF, KiCad, Altium)", schematic::EXTENSIONS)
            .add_filter("All files", &["*"])
            .pick_file();
        if let Some(path) = picked {
            self.schematic_spawn_import(ctx, pins, path, false);
        }
    }

    /// Read a sheet's source file again: the drawing is replaced, the nodes on
    /// it stay where they are and pins that had no node get one.
    fn schematic_refresh(&mut self, ctx: &egui::Context, pins: PinNames, idx: usize) {
        let Some(sheet) = self.project.schematic.sheets.get(idx) else { return };
        let mut path = PathBuf::from(&sheet.origin);
        if sheet.origin.is_empty() || !path.is_file() {
            // Imported before the path was recorded, or the file has moved.
            let picked = rfd::FileDialog::new()
                .set_title(format!("Locate {} to refresh the sheet", sheet.source))
                .add_filter("Schematic (SVG, PDF, KiCad, Altium)", schematic::EXTENSIONS)
                .add_filter("All files", &["*"])
                .pick_file();
            match picked {
                Some(p) => path = p,
                None => return,
            }
        }
        self.schematic_spawn_import(ctx, pins, path, true);
    }

    fn schematic_spawn_import(&mut self, ctx: &egui::Context, pins: PinNames, path: PathBuf, refresh: bool) {
        let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let source = path.clone();
        std::thread::spawn(move || {
            // A malformed file must not take the application down with it.
            let res = std::panic::catch_unwind(|| schematic::import(&source, &pins))
                .map_err(|_| "the file could not be processed".to_string())
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
            let _ = tx.send(res);
            ctx.request_repaint();
        });
        self.sch.import = Some(ImportJob { rx, path, file, refresh });
    }

    fn schematic_poll_import(&mut self, pins: &PinNames, actions: &mut Vec<Action>) {
        let Some(job) = &self.sch.import else { return };
        let res = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the importer stopped unexpectedly".to_string()),
        };
        let Some(ImportJob { path, file, refresh, .. }) = self.sch.import.take() else { return };
        let verb = if refresh { "refresh" } else { "import" };
        let drawings = match res {
            Ok(d) => d,
            Err(e) => {
                actions.push(Action::Notify(format!("Could not {verb} {file}: {e}"), true));
                return;
            }
        };
        let count = |s: &Self| s.project.schematic.sheets.iter().map(|s| s.nodes.len()).sum::<usize>();
        let before = count(self);
        let showing = self.project.schematic.sheets.get(self.sch.sheet).map(|s| s.file.clone());
        let done = match schematic::add_to_project(&mut self.project.schematic, &self.dir, &path, drawings, pins) {
            Ok(d) => d,
            Err(e) => {
                actions.push(Action::Notify(format!("Could not {verb} {file}: {e:#}"), true));
                return;
            }
        };
        for e in &done.errors {
            actions.push(Action::Notify(format!("Could not write {e}"), true));
        }
        // A sheet read again gets a fresh background (its nodes are kept).
        for f in &done.files {
            self.sch.images.remove(f);
        }
        let Some(idx) = done.sheet else { return };
        self.mark_dirty();
        if refresh {
            // Stay on the sheet being shown if the file still has it.
            if !showing.is_some_and(|f| done.files.contains(&f)) {
                self.sch.sheet = idx;
                self.sch.fit = true;
            }
            if self.sch.mode == Mode::Edit {
                self.schematic_enter_edit();
            }
            let added = count(self).saturating_sub(before);
            let nodes = match added {
                0 => "the nodes are where they were".to_string(),
                1 => "1 new node placed".to_string(),
                n => format!("{n} new nodes placed"),
            };
            actions.push(Action::Notify(format!("Refreshed from {file}: {nodes}."), false));
            return;
        }
        let (imported, found) = (done.files.len(), done.gpios);
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
        self.sch.selected = None;
        self.mark_dirty();
    }

    /// "Close this schematic?": closing drops the node positions, so ask first.
    fn schematic_close_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.sch.confirm_close else { return };
        let Some(sheet) = self.project.schematic.sheets.get(idx) else {
            self.sch.confirm_close = None;
            return;
        };
        let (mut close, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("close_schematic")).show(ui.ctx(), |ui| {
            ui.set_width(400.0);
            theme::heading(ui, &format!("Close {}?", sheet.name));
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "The sheet and its {} node positions are removed from this project, so another schematic can be imported. Pin functions and labels are kept.",
                    sheet.nodes.len()
                ))
                .color(theme::TEXT_DIM),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                close = ui.add(theme::primary_button("Close schematic")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if close {
            self.sch.confirm_close = None;
            self.schematic_remove_sheet(idx);
        } else if cancel || modal.should_close() {
            self.sch.confirm_close = None;
        }
    }

    // ---- central view ----

    pub(super) fn schematic_view(&mut self, ui: &mut egui::Ui, chip: &Chip, actions: &mut Vec<Action>) {
        let pins = self.pin_names(chip);
        self.schematic_poll_import(&pins, actions);
        if self.sch.sheet >= self.project.schematic.sheets.len() {
            self.sch.sheet = 0;
        }
        // Undo / redo, unless a text box is using the keyboard. Redo first:
        // Ctrl+Z alone would match Ctrl+Shift+Z too.
        if self.sch.mode == Mode::Edit && !ui.ctx().egui_wants_keyboard_input() {
            let cmd = egui::Modifiers::COMMAND;
            let (redo, undo) = ui.input_mut(|i| {
                let redo = i.consume_key(cmd | egui::Modifiers::SHIFT, egui::Key::Z) || i.consume_key(cmd, egui::Key::Y);
                (redo, i.consume_key(cmd, egui::Key::Z))
            });
            if redo || undo {
                self.schematic_step(redo);
            }
        }
        self.schematic_close_dialog(ui);
        self.schematic_toolbar(ui, &pins, actions);
        if self.project.schematic.sheets.is_empty() {
            self.schematic_empty(ui, &pins);
        } else {
            self.schematic_canvas(ui, chip, &pins, actions);
        }
    }

    fn schematic_toolbar(&mut self, ui: &mut egui::Ui, pins: &PinNames, actions: &mut Vec<Action>) {
        let unsaved = self.schematic_unsaved();
        let busy = self.sch.import.is_some();
        let blocked = "Save or cancel the calibration first";
        let mut switch_to = None;
        let mut cmd = None;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if let Some(job) = &self.sch.import {
                ui.spinner();
                ui.label(RichText::new(format!("Reading {}…", job.file)).color(theme::TEXT_DIM));
            } else if ui
                .add_enabled(!unsaved, egui::Button::new("📂 Import schematic…"))
                .on_hover_text("SVG, PDF, KiCad .kicad_sch or Altium .SchDoc")
                .on_disabled_hover_text(blocked)
                .clicked()
            {
                cmd = Some(Cmd::Import);
            }
            if !self.project.schematic.sheets.is_empty() {
                ui.separator();
                for (i, s) in self.project.schematic.sheets.iter().enumerate() {
                    let current = self.sch.sheet == i;
                    let r = ui
                        .add_enabled(current || !unsaved, egui::Button::selectable(current, RichText::new(&s.name).size(13.0)))
                        .on_hover_text(format!("{} · {} nodes", s.source, s.nodes.len()))
                        .on_disabled_hover_text(blocked);
                    if r.clicked() && !current {
                        switch_to = Some(i);
                    }
                    if !current {
                        continue;
                    }
                    // Beside the sheet being shown: read it again, or close it.
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                                                let refresh = ui
                            .add_enabled(!unsaved && !busy, egui::Button::new("Refresh"))
                            .on_hover_text(format!("Refresh: read {} again. The nodes stay where they are.", s.source))
                            .on_disabled_hover_text(blocked);
                        if refresh.clicked() {
                            cmd = Some(Cmd::Refresh);
                        }
                        let close = ui
                            .add_enabled(!unsaved && !busy, egui::Button::new("Close"))
                            .on_hover_text("Close this schematic, so another one can be imported")
                            .on_disabled_hover_text(blocked);
                        if close.clicked() {
                            cmd = Some(Cmd::Close);
                        }
                    });
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if self.project.schematic.sheets.is_empty() {
                    return;
                }
                if ui.button("Fit").on_hover_text("Show the whole sheet").clicked() {
                    cmd = Some(Cmd::Fit);
                }
                ui.separator();
                match self.sch.mode {
                    Mode::View => {
                        if ui.button("✏ Edit positions").on_hover_text("Calibrate: drag the nodes onto their pins").clicked() {
                            cmd = Some(Cmd::Edit);
                        }
                        ui.label(RichText::new("VIEW").size(11.0).strong().color(theme::TEXT_DIM));
                    }
                    Mode::Edit => {
                        if ui.button("Cancel").on_hover_text("Discard the changes made in this calibration").clicked() {
                            cmd = Some(Cmd::Cancel);
                        }
                        if ui.add(theme::primary_button("Save")).on_hover_text("Store the node positions in the project file").clicked() {
                            cmd = Some(Cmd::Save);
                        }
                        ui.separator();
                        let (undo, redo) = self.sch.draft.as_ref().map(|d| (!d.undo.is_empty(), !d.redo.is_empty())).unwrap_or_default();
                        if ui.add_enabled(redo, egui::Button::new("Redo")).on_hover_text("Ctrl+Y").clicked() {
                            cmd = Some(Cmd::Redo);
                        }
                        if ui.add_enabled(undo, egui::Button::new("Undo")).on_hover_text("Ctrl+Z").clicked() {
                            cmd = Some(Cmd::Undo);
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
        if let Some(cmd) = cmd {
            self.schematic_run(ui.ctx(), pins, cmd, actions);
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
                self.schematic_pick_import(ui.ctx(), pins.clone());
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

    fn schematic_canvas(&mut self, ui: &mut egui::Ui, chip: &Chip, pins: &PinNames, actions: &mut Vec<Action>) {
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

        // ---- nodes ----
        let editing = self.sch.mode == Mode::Edit;
        let sheet = &self.project.schematic.sheets[self.sch.sheet];
        let sheet_name = sheet.name.clone();
        let (mut nodes, node_size, grid) = match &mut self.sch.draft {
            Some(d) if editing => {
                if d.pitch.is_none() {
                    d.pitch = Some(pin_pitch(&d.nodes, size));
                }
                (d.nodes.clone(), d.node_size, d.grid)
            }
            _ => (sheet.nodes.clone(), sheet.node_size, 0.0),
        };

        // ---- view transform: fit to the canvas, then user zoom and pan ----
        let now = ui.input(|i| i.time);
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
        // A pin picked in the list: bring its node into view if it is outside.
        if let Some(n) = self.sch.focus.take().and_then(|pin| nodes.iter().find(|n| n.pin == pin)) {
            let from_centre = vec2((n.x - 0.5) * size.x, (n.y - 0.5) * size.y) * scale;
            if !rect.shrink(48.0).contains(rect.center() + self.sch.pan + from_centre) {
                self.sch.pan = -from_centre;
                self.sch.moved_at = now;
            }
        }
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

        // The snap grid, once its lines are far enough apart to read.
        let step = grid * scale;
        if editing && step >= 5.0 {
            let area = Rect::from_min_size(origin, size * scale).intersect(rect);
            let line = Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.22));
            let first = |from: f32, start: f32| ((from - start) / step).ceil().max(0.0) as i32;
            let mut i = first(area.left(), origin.x);
            while origin.x + i as f32 * step <= area.right() && i as f32 * grid <= size.x {
                painter.vline(origin.x + i as f32 * step, area.y_range(), line);
                i += 1;
            }
            let mut i = first(area.top(), origin.y);
            while origin.y + i as f32 * step <= area.bottom() && i as f32 * grid <= size.y {
                painter.hline(area.x_range(), origin.y + i as f32 * step, line);
                i += 1;
            }
        }

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
        // `changed`: the nodes were edited here; `checkpoint`: as a new undo step.
        let (mut changed, mut checkpoint) = (false, false);
        if editing {
            // Alt lets a node go between the grid lines.
            let grid = if ui.input(|i| i.modifiers.alt) { 0.0 } else { grid };
            if let Some(pin) = self.sch.placing.clone() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) || resp.secondary_clicked() {
                    self.sch.placing = None;
                } else if let (true, Some(p)) = (resp.clicked(), resp.interact_pointer_pos()) {
                    let (x, y) = to_sheet(p);
                    if (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y) {
                        let (x, y) = snap(x, y, size, grid);
                        nodes.push(Node { pin: pin.clone(), x, y, label: LabelDir::Right });
                        self.sch.selected = Some(pin);
                        self.sch.placing = None;
                        (changed, checkpoint) = (true, true);
                    }
                }
                if resp.dragged() {
                    self.sch.pan += resp.drag_delta();
                    self.sch.moved_at = now;
                }
            } else {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    let press = ui.input(|i| i.pointer.press_origin());
                    self.sch.drag = press.and_then(|p| node_at(&nodes, p));
                    self.sch.drag_moved = false;
                    if let (Some(i), Some(p)) = (self.sch.drag, press) {
                        self.sch.drag_offset = to_screen(&nodes[i]) - p;
                        self.sch.selected = Some(nodes[i].pin.clone());
                    }
                }
                if resp.dragged() {
                    match (self.sch.drag.filter(|i| *i < nodes.len()), resp.interact_pointer_pos()) {
                        (Some(i), Some(p)) => {
                            let (x, y) = to_sheet(p + self.sch.drag_offset);
                            let (x, y) = snap(x, y, size, grid);
                            if (x, y) != (nodes[i].x, nodes[i].y) {
                                (nodes[i].x, nodes[i].y) = (x, y);
                                changed = true;
                                // The whole drag is one undo step.
                                checkpoint = !std::mem::replace(&mut self.sch.drag_moved, true);
                            }
                        }
                        (Some(_), None) => {}
                        (None, _) => {
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
                // Keyboard on the node under the pointer.
                if let Some(i) = hovered.filter(|_| self.sch.drag.is_none() && !resp.context_menu_opened()) {
                    let (turn, delete) = ui.input(|i| (i.key_pressed(egui::Key::R), i.key_pressed(egui::Key::Delete)));
                    if turn {
                        nodes[i].label = nodes[i].label.rotated();
                        (changed, checkpoint) = (true, true);
                    } else if delete {
                        nodes.remove(i);
                        (changed, checkpoint) = (true, true);
                    }
                }
            }
        } else {
            if resp.dragged() {
                self.sch.pan += resp.drag_delta();
                self.sch.moved_at = now;
            }
            match hovered {
                Some(i) => {
                    let pin = nodes[i].pin.clone();
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        self.pin_popup = pins.is_gpio(&pin).then(|| PinPopup::functions(pin.clone()));
                        self.schematic_select(chip, Some(pin.clone()));
                    }
                    if self.pin_popup.is_none() && !resp.context_menu_opened() {
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
                    if resp.clicked() {
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
                if checkpoint {
                    d.checkpoint();
                }
                d.nodes = nodes.clone();
                d.changed = true;
            }
            self.sch.menu_node = None;
        }

        // ---- right-click menu ----
        let mut cmd = None;
        if self.sch.placing.is_none() {
            if resp.secondary_clicked() {
                self.sch.menu_node = hovered;
                if let Some(i) = hovered {
                    let pin = nodes[i].pin.clone();
                    if editing {
                        self.sch.selected = Some(pin);
                    } else {
                        self.schematic_select(chip, Some(pin));
                    }
                }
            }
            let on_node = self.sch.menu_node.filter(|i| *i < nodes.len()).map(|i| (i, nodes[i].pin.clone()));
            if editing {
                let (can_undo, can_redo, pitch) = self.sch.draft.as_ref().map(|d| (!d.undo.is_empty(), !d.redo.is_empty(), d.pitch.flatten())).unwrap_or_default();
                resp.context_menu(|ui| {
                    ui.set_min_width(210.0);
                    if let Some((i, pin)) = &on_node {
                        ui.label(RichText::new(pin).strong().color(theme::ACCENT));
                        if ui.button("Turn label                 R").clicked() {
                            cmd = Some(Cmd::Turn(*i));
                        }
                        if ui.button("Remove node           Del").clicked() {
                            cmd = Some(Cmd::Remove(*i));
                        }
                        ui.separator();
                    }
                    if ui.add_enabled(can_undo, egui::Button::new("Undo                 Ctrl+Z")).clicked() {
                        cmd = Some(Cmd::Undo);
                    }
                    if ui.add_enabled(can_redo, egui::Button::new("Redo                 Ctrl+Y")).clicked() {
                        cmd = Some(Cmd::Redo);
                    }
                    ui.separator();
                    let title = if grid > 0.0 { format!("Snap grid  ·  {}", units(grid)) } else { "Snap grid  ·  off".to_string() };
                    // The value box needs the menu to stay open while it is used.
                    let keep_open = egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    egui::containers::menu::SubMenuButton::new(title).config(keep_open).ui(ui, |ui| {
                        ui.set_min_width(200.0);
                        if let Some(g) = grid_picker(ui, grid, pitch, size) {
                            cmd = Some(Cmd::Grid(g));
                        }
                    });
                    if ui.add_enabled(grid > 0.0, egui::Button::new("Snap all nodes to the grid")).clicked() {
                        cmd = Some(Cmd::SnapAll);
                    }
                    ui.separator();
                    if ui.button("Fit to window").clicked() {
                        cmd = Some(Cmd::Fit);
                    }
                    if ui.button("Save positions").clicked() {
                        cmd = Some(Cmd::Save);
                    }
                    if ui.button("Cancel editing").clicked() {
                        cmd = Some(Cmd::Cancel);
                    }
                });
            } else {
                let busy = self.sch.import.is_some();
                resp.context_menu(|ui| {
                    ui.set_min_width(210.0);
                    if let Some((_, pin)) = on_node.as_ref().filter(|(_, pin)| pins.is_gpio(pin)) {
                        let configured = self.project.pins.contains_key(pin);
                        ui.label(RichText::new(pin).strong().color(theme::ACCENT));
                        if ui.button("Function…").clicked() {
                            cmd = Some(Cmd::Function(pin.clone()));
                        }
                        if ui.add_enabled(configured, egui::Button::new("Label…")).clicked() {
                            cmd = Some(Cmd::Label(pin.clone()));
                        }
                        if ui.add_enabled(configured, egui::Button::new("Reset pin")).clicked() {
                            cmd = Some(Cmd::Reset(pin.clone()));
                        }
                        ui.separator();
                    }
                    if ui.button("Fit to window").clicked() {
                        cmd = Some(Cmd::Fit);
                    }
                    if ui.button("Edit positions").clicked() {
                        cmd = Some(Cmd::Edit);
                    }
                    ui.separator();
                    if ui.add_enabled(!busy, egui::Button::new(format!("Refresh {sheet_name}"))).clicked() {
                        cmd = Some(Cmd::Refresh);
                    }
                    if ui.add_enabled(!busy, egui::Button::new(format!("Close {sheet_name}…"))).clicked() {
                        cmd = Some(Cmd::Close);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Import schematic…")).clicked() {
                        cmd = Some(Cmd::Import);
                    }
                });
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
        // A pin picked in the list pulses twice so the eye finds it.
        let pulse = Some(((now - self.sch.flash_at) / 0.7) as f32).filter(|t| (0.0..2.0).contains(t)).map(f32::fract);
        if pulse.is_some() {
            ui.ctx().request_repaint();
        }
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
                painter.circle_filled(pos, r + 9.0, theme::ACCENT.gamma_multiply(0.16));
                painter.circle_stroke(pos, r + 4.0, Stroke::new(2.0, theme::ACCENT));
                if let Some(t) = pulse {
                    painter.circle_stroke(pos, r + 4.0 + t * 26.0, Stroke::new(2.5, theme::ACCENT.gamma_multiply(1.0 - t)));
                }
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
            (None, true) => "Drag a node onto its pin  ·  R: turn label  ·  Del: remove  ·  Ctrl+Z / Ctrl+Y: undo / redo  ·  Right-click: grid and more  ·  Wheel: zoom".to_string(),
            (None, false) => "Click a node: choose its function, then name it  ·  Right-click: menu  ·  Wheel: zoom  ·  Drag: pan  ·  Double-click: fit".to_string(),
        };
        let shadowed = |pos: Pos2, align: Align2, text: String, color: Color32| {
            let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), color);
            let r = align.anchor_size(pos, galley.size());
            painter.rect_filled(r.expand2(vec2(6.0, 3.0)), 4.0, theme::BG.gamma_multiply(0.85));
            painter.galley(r.min, galley, color);
        };
        shadowed(rect.left_bottom() + vec2(14.0, -12.0), Align2::LEFT_BOTTOM, help, theme::TEXT_DIM);
        let placed = nodes.iter().filter(|n| pins.is_gpio(&n.pin)).map(|n| n.pin.as_str()).collect::<BTreeSet<_>>().len();
        let snapping = if editing && grid > 0.0 { format!("  ·  grid {}", units(grid)) } else { String::new() };
        shadowed(
            rect.right_bottom() + vec2(-14.0, -12.0),
            Align2::RIGHT_BOTTOM,
            format!("{placed} of {} I/O pins on this sheet{snapping}  ·  {:.0}%", pins.gpios.len(), self.sch.zoom * 100.0),
            theme::TEXT_DIM,
        );

        if let Some(cmd) = cmd {
            self.schematic_run(ui.ctx(), pins, cmd, actions);
        }
    }

    /// Select a pin here and on the package drawing.
    fn schematic_select(&mut self, chip: &Chip, pin: Option<String>) {
        if let (Some(name), Some(pkg)) = (&pin, chip.package(&self.project.package).or(chip.packages.first())) {
            if let Some(p) = pkg.pins.iter().find(|p| p.signals.iter().any(|s| gpio_name(s).as_deref() == Some(name.as_str()))) {
                self.selected_pin = Some(p.position.clone());
            }
        }
        self.sch.reveal = pin.clone();
        self.sch.selected = pin;
    }

    /// A pin picked in the list: select it and point it out on the schematic,
    /// on whichever sheet shows it.
    fn schematic_show_pin(&mut self, ui: &egui::Ui, chip: &Chip, pin: &str) {
        self.schematic_select(chip, Some(pin.to_string()));
        self.sch.reveal = None;
        let sheets = &self.project.schematic.sheets;
        let has = |i: usize| sheets.get(i).is_some_and(|s| s.nodes.iter().any(|n| n.pin == pin));
        if !has(self.sch.sheet) {
            if let Some(i) = (0..sheets.len()).find(|i| has(*i)) {
                self.sch.sheet = i;
                self.sch.fit = true;
            }
        }
        self.sch.focus = Some(pin.to_string());
        self.sch.flash_at = ui.input(|i| i.time);
        self.pin_popup = None;
    }

    // ---- side panel ----

    pub(super) fn schematic_panel(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        let pins = self.pin_names(chip);
        if self.sch.mode == Mode::Edit && self.sch.draft.is_some() {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(6.0);
                self.schematic_edit_panel(ui, &pins);
            });
        } else {
            ui.add_space(6.0);
            self.schematic_view_panel(ui, chip, &pins);
        }
    }

    fn schematic_view_panel(&mut self, ui: &mut egui::Ui, chip: &Chip, pins: &PinNames) {
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
                ui.label(RichText::new("Click a node on the schematic, or a pin in the list below.").color(theme::TEXT_DIM));
            }
        }
        if self.project.schematic.sheets.is_empty() {
            return;
        }
        ui.add_space(10.0);
        ui.separator();
        self.schematic_pin_list(ui, chip, pins);
    }

    /// Every pin of the package with its status, function and label. Picking a
    /// row points the pin out on the schematic.
    fn schematic_pin_list(&mut self, ui: &mut egui::Ui, chip: &Chip, pins: &PinNames) {
        let conflicts = self.conflicts();
        let here: BTreeSet<&str> = self.project.schematic.sheets.get(self.sch.sheet).map(|s| s.nodes.iter().map(|n| n.pin.as_str()).collect()).unwrap_or_default();
        let anywhere: BTreeSet<&str> = self.project.schematic.sheets.iter().flat_map(|s| s.nodes.iter().map(|n| n.pin.as_str())).collect();
        let mut gpios: Vec<&String> = pins.gpios.iter().collect();
        gpios.sort_by_key(|g| pin_order(g));
        let mut rows: Vec<PinRow> = gpios
            .into_iter()
            .map(|g| {
                let cfg = self.project.pins.get(g);
                let (status, color) = match cfg {
                    _ if conflicts.contains(g) => ("Conflict", theme::PIN_CONFLICT),
                    Some(c) if matches!(c.mode, PinMode::Peripheral { .. }) => ("Peripheral", theme::PIN_PERIPH),
                    Some(_) => ("GPIO", theme::PIN_GPIO),
                    None => ("Free", theme::PIN_FREE),
                };
                PinRow {
                    pin: g.clone(),
                    gpio: true,
                    status,
                    color,
                    function: cfg.map(|c| c.mode.label()).unwrap_or_default(),
                    label: cfg.map(|c| c.label.clone()).unwrap_or_default(),
                    here: here.contains(g.as_str()),
                }
            })
            .collect();
        // Power and system pins, one row per name.
        rows.extend(pins.passive.values().map(|(name, _)| PinRow {
            pin: name.clone(),
            gpio: false,
            status: "Power",
            color: theme::TEXT_DIM,
            function: String::new(),
            label: String::new(),
            here: here.contains(name.as_str()),
        }));
        let total = rows.len();
        let configured = rows.iter().filter(|r| !r.function.is_empty()).count();
        let query: Vec<String> = self.sch.filter.split_whitespace().map(str::to_ascii_uppercase).collect();
        let only = self.sch.configured_only;
        rows.retain(|r| {
            (!only || !r.function.is_empty())
                && query.iter().all(|t| r.pin.to_ascii_uppercase().contains(t) || r.function.to_ascii_uppercase().contains(t) || r.label.to_ascii_uppercase().contains(t) || r.status.to_ascii_uppercase().contains(t))
        });

        ui.horizontal(|ui| {
            theme::section(ui, "Pins");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("{configured} of {total} configured")).size(11.0).color(theme::TEXT_DIM));
            });
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.sch.configured_only, "Configured").on_hover_text("List only the pins that have a function");
            ui.add(egui::TextEdit::singleline(&mut self.sch.filter).hint_text("Filter: pin, function, label").desired_width(f32::INFINITY));
        });

        // Columns, shared by the header and the rows.
        let columns = |rect: Rect| {
            let pin = rect.left() + 20.0;
            let status = pin + 46.0;
            let function = status + 66.0;
            let label = function + (rect.right() - 6.0 - function) * 0.56;
            [pin, status, function, label, rect.right() - 4.0]
        };
        let small = FontId::proportional(11.0);
        let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let x = columns(head.shrink2(vec2(0.0, 0.0)).with_max_x(head.right() - ui.spacing().scroll.bar_width - ui.spacing().scroll.bar_inner_margin));
        for (i, title) in ["PIN", "STATUS", "FUNCTION", "LABEL"].into_iter().enumerate() {
            ui.painter().text(pos2(x[i], head.center().y), Align2::LEFT_CENTER, title, FontId::proportional(10.0), theme::TEXT_DIM);
        }

        let footer = 40.0;
        let reveal = self.sch.reveal.take().and_then(|pin| rows.iter().position(|r| r.pin == pin));
        let mut picked = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut list = egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - footer).max(80.0));
            if let Some(i) = reveal {
                // Selected on the schematic: show its row, a few rows down from the top.
                list = list.vertical_scroll_offset((i as f32 - 3.0).max(0.0) * ROW);
            }
            list.show_rows(ui, ROW, rows.len(), |ui, range| {
                for r in &rows[range] {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                    let selected = self.sch.selected.as_deref() == Some(r.pin.as_str());
                    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, format!("Pin {}", r.pin)));
                    let painter = ui.painter_at(rect);
                    if selected {
                        painter.rect_filled(rect, 4.0, theme::ACCENT_DIM);
                    } else if resp.hovered() {
                        painter.rect_filled(rect, 4.0, theme::CARD_HOVER);
                    }
                    let x = columns(rect);
                    let y = rect.center().y;
                    // Filled: has a node on the sheet being shown.
                    let dot = pos2(rect.left() + 10.0, y);
                    if r.here {
                        painter.circle(dot, 4.5, r.color, Stroke::new(1.0, OUTLINE));
                    } else {
                        painter.circle_stroke(dot, 4.0, Stroke::new(1.2, r.color.gamma_multiply(0.7)));
                    }
                    let text = if r.gpio { theme::TEXT } else { theme::TEXT_DIM };
                    cell(&painter, pos2(x[0], y), x[1] - x[0] - 4.0, &r.pin, FontId::monospace(12.0), text);
                    cell(&painter, pos2(x[1], y), x[2] - x[1] - 4.0, r.status, small.clone(), r.color);
                    cell(&painter, pos2(x[2], y), x[3] - x[2] - 6.0, if r.function.is_empty() && r.gpio { "—" } else { &r.function }, FontId::proportional(12.0), text);
                    cell(&painter, pos2(x[3], y), x[4] - x[3], &r.label, FontId::proportional(12.0), theme::ACCENT);
                    if resp.clicked() {
                        picked = Some(r.pin.clone());
                    }
                    let place = match (r.here, anywhere.contains(r.pin.as_str())) {
                        (true, _) => "",
                        (false, true) => "\nIts node is on another sheet.",
                        (false, false) => "\nNo node on the schematic yet (Edit positions places one).",
                    };
                    let mut tip = format!("{}  ·  {}", r.pin, r.status);
                    if !r.function.is_empty() {
                        tip.push_str(&format!("  ·  {}", r.function));
                    }
                    if !r.label.is_empty() {
                        tip.push_str(&format!("  “{}”", r.label));
                    }
                    resp.on_hover_text(format!("{tip}{place}"));
                }
            });
        });
        if rows.is_empty() {
            ui.label(RichText::new("No pin matches the filter.").size(11.5).color(theme::TEXT_DIM));
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new("● has a node on this sheet   ○ has none here.  On the schematic, a ring marks the pins the selected peripheral can use.")
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
        if let Some(pin) = picked {
            self.schematic_show_pin(ui, chip, &pin);
        }
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
        let sheet_size = self.sch.view.map(|v| v.2);
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
        let slider = ui.add(egui::Slider::new(&mut permille, 1.0..=30.0).fixed_decimals(1).suffix(" ‰ of width"));
        if slider.changed() {
            // One undo step for a whole drag of the slider.
            if !std::mem::replace(&mut draft.sizing, true) {
                draft.checkpoint();
            }
            draft.node_size = permille / 1000.0;
            draft.changed = true;
        }
        if !slider.dragged() && !slider.has_focus() {
            draft.sizing = false;
        }

        ui.add_space(12.0);
        theme::section(ui, "Snap grid");
        let mut snap_all = false;
        if let Some(size) = sheet_size {
            let choices = grid_choices(draft.pitch.flatten());
            let current = choices.iter().find(|c| (c.1 - draft.grid).abs() < 1e-4).map(|c| c.0.clone()).unwrap_or_else(|| format!("Custom  ·  {}", units(draft.grid)));
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("snap_grid").selected_text(current).width(170.0).show_ui(ui, |ui| {
                    for (name, value) in &choices {
                        ui.selectable_value(&mut draft.grid, *value, name);
                    }
                });
                ui.add(egui::DragValue::new(&mut draft.grid).speed(size.x / 4000.0).range(0.0..=size.x / 4.0).max_decimals(3))
                    .on_hover_text("Grid pitch in drawing units. 0 switches the grid off.");
            });
            ui.label(
                RichText::new(format!("In the drawing's own units (the sheet is {} × {}). Hold Alt to place a node between the lines.", units(size.x), units(size.y)))
                    .size(11.0)
                    .color(theme::TEXT_DIM),
            );
            snap_all = ui.add_enabled(draft.grid > 0.0, egui::Button::new("Snap all nodes to the grid")).clicked();
        } else {
            ui.label(RichText::new("Available once the drawing is shown.").size(12.0).color(theme::TEXT_DIM));
        }
        if let (true, Some(size)) = (snap_all, sheet_size) {
            draft.checkpoint();
            for n in &mut draft.nodes {
                (n.x, n.y) = snap(n.x, n.y, size, draft.grid);
            }
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
        if !draft.nodes.is_empty() && ui.button("Remove all nodes").on_hover_text("Start over and place every pin by hand (Ctrl+Z brings them back)").clicked() {
            draft.checkpoint();
            draft.nodes.clear();
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

/// The grid choices as a list plus a box for any other pitch; returns the pitch picked.
fn grid_picker(ui: &mut egui::Ui, grid: f32, pitch: Option<f32>, size: Vec2) -> Option<f32> {
    let mut out = None;
    for (name, value) in grid_choices(pitch) {
        if ui.radio((value - grid).abs() < 1e-4, name).clicked() {
            out = Some(value);
            ui.close();
        }
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Pitch");
        let mut g = grid;
        if ui.add(egui::DragValue::new(&mut g).speed(size.x / 4000.0).range(0.0..=size.x / 4.0).max_decimals(3)).changed() {
            out = Some(g);
        }
        ui.label(RichText::new("drawing units").size(11.0).color(theme::TEXT_DIM));
    });
    out
}
