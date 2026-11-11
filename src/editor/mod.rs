//! Main editor: pinout, clock tree, peripherals and generated code preview.

mod clock_view;
mod peripherals;
mod pinout;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align, Layout, RichText};

use crate::app::{Action, Data, Load};
use crate::clock::{ClockResults, ClockTree, format_hz};
use crate::codegen::{self, GenFile, RccFlavor};
use crate::db::{Chip, PinFunction};
use crate::project::{PinConfig, PinMode, Project};
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Pinout,
    Clock,
    Code,
}

pub struct Editor {
    pub dir: PathBuf,
    pub project: Project,
    chip: Option<Arc<Chip>>,
    tree: Option<ClockTree>,
    clocks: ClockResults,
    /// GPIO name -> routable peripheral functions.
    pin_functions: BTreeMap<String, Vec<PinFunction>>,
    load_error: Option<String>,
    tab: Tab,
    selected_pin: Option<String>,
    selected_periph: Option<String>,
    periph_search: String,
    pin_view: pinout::PinView,
    dirty: bool,
    generate_when_ready: bool,
    preview: Vec<GenFile>,
    preview_file: usize,
    preview_dirty: bool,
}

impl Editor {
    pub fn new(dir: PathBuf, project: Project, generate: bool) -> Self {
        Self {
            dir,
            project,
            chip: None,
            tree: None,
            clocks: ClockResults::default(),
            pin_functions: BTreeMap::new(),
            load_error: None,
            tab: Tab::Pinout,
            selected_pin: None,
            selected_periph: None,
            periph_search: String::new(),
            pin_view: pinout::PinView::default(),
            dirty: false,
            generate_when_ready: generate,
            preview: Vec::new(),
            preview_file: 0,
            preview_dirty: true,
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.preview_dirty = true;
        if let Some(tree) = &self.tree {
            self.clocks = self.project.clock.compute(tree);
        }
    }

    pub fn save_quiet(&mut self) {
        if self.dirty && self.project.save(&self.dir).is_ok() {
            self.dirty = false;
        }
    }

    fn save(&mut self, actions: &mut Vec<Action>) {
        match self.project.save(&self.dir) {
            Ok(()) => {
                self.dirty = false;
                actions.push(Action::Notify("Project saved".into(), false));
            }
            Err(e) => actions.push(Action::Notify(format!("Save failed: {e:#}"), true)),
        }
    }

    fn generate(&mut self, actions: &mut Vec<Action>) {
        let Some(chip) = &self.chip else { return };
        if let Err(e) = self.project.save(&self.dir) {
            actions.push(Action::Notify(format!("Save failed: {e:#}"), true));
            return;
        }
        self.dirty = false;
        let files = codegen::generate(&self.project, chip, self.tree.as_ref());
        match codegen::write(&self.dir, &files) {
            Ok(written) if written.is_empty() => {
                actions.push(Action::Notify("Code is up to date".into(), false));
            }
            Ok(written) => actions.push(Action::Notify(
                format!("Generated {} file(s): {}", written.len(), written.join(", ")),
                false,
            )),
            Err(e) => actions.push(Action::Notify(format!("Code generation failed: {e:#}"), true)),
        }
    }

    /// Load chip and RCC data as they become available.
    fn poll_data(&mut self, data: &mut Data) {
        if self.chip.is_none() && self.load_error.is_none() {
            match data.chip(&self.project.chip) {
                Load::Ready(chip) => {
                    self.pin_functions = chip.pin_functions();
                    if chip.package(&self.project.package).is_none() {
                        if let Some(p) = chip.packages.first() {
                            self.project.package = p.name.clone();
                        }
                    }
                    self.chip = Some(chip);
                }
                Load::Failed(e) => self.load_error = Some(e),
                Load::Loading => {}
            }
        }
        if let (Some(chip), None) = (&self.chip, &self.tree) {
            match chip.rcc_registers() {
                Some(r) => match data.registers(r) {
                    Load::Ready(rcc) => {
                        let tree = ClockTree::from_rcc(&rcc, &r.version);
                        self.project.clock.normalize(&tree);
                        self.clocks = self.project.clock.compute(&tree);
                        self.tree = Some(tree);
                    }
                    Load::Failed(e) => {
                        self.load_error = Some(format!("RCC registers: {e}"));
                        self.tree = Some(ClockTree::default());
                    }
                    Load::Loading => {}
                },
                None => self.tree = Some(ClockTree::default()),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, data: &mut Data, actions: &mut Vec<Action>) {
        self.poll_data(data);
        if self.generate_when_ready && self.chip.is_some() && self.tree.is_some() {
            self.generate_when_ready = false;
            self.generate(actions);
        }

        // Shortcuts
        let (save, gen_code) = ui.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::S),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::G),
            )
        });
        if save {
            self.save(actions);
        }
        if gen_code {
            self.generate(actions);
        }

        self.top_bar(ui, actions);
        self.status_bar(ui);

        let Some(chip) = self.chip.clone() else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| match &self.load_error {
                    Some(e) => ui.label(RichText::new(format!("Could not load {}: {e}", self.project.chip)).color(theme::ERROR)),
                    None => ui.label(format!("Loading {} from stm32-data…", self.project.chip)),
                });
            });
            return;
        };

        egui::Panel::left("peripherals")
            .resizable(true)
            .default_size(300.0)
            .min_size(240.0)
            .show(ui, |ui| self.peripheral_panel(ui, &chip));

        if self.tab == Tab::Pinout {
            egui::Panel::right("pin_inspector")
                .resizable(true)
                .default_size(300.0)
                .min_size(240.0)
                .show(ui, |ui| self.pin_inspector(ui, &chip));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG))
            .show(ui, |ui| match self.tab {
                Tab::Pinout => self.pinout(ui, &chip),
                Tab::Clock => self.clock_view(ui),
                Tab::Code => self.code_view(ui, &chip),
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        egui::Panel::top("editor_top")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                    crate::hub::logo(ui.painter(), rect);
                    ui.menu_button("File", |ui| {
                        if ui.button("Save            Ctrl+S").clicked() {
                            self.save(actions);
                            ui.close();
                        }
                        if ui.button("Generate Code   Ctrl+G").clicked() {
                            self.generate(actions);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Show in Explorer").clicked() {
                            crate::hub::open_in_explorer(&self.dir);
                            ui.close();
                        }
                        if ui.button("Open in VS Code").clicked() {
                            let r = std::process::Command::new("cmd")
                                .args(["/C", "code", "."])
                                .current_dir(&self.dir)
                                .spawn();
                            if r.is_err() {
                                actions.push(Action::Notify("Could not start VS Code (`code` not on PATH)".into(), true));
                            }
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Close Project").clicked() {
                            self.save_quiet();
                            actions.push(Action::CloseProject);
                            ui.close();
                        }
                    });
                    ui.menu_button("Options", |ui| {
                        let mut changed = ui.checkbox(&mut self.project.codegen.defmt, "Use defmt logging").changed();
                        changed |= ui.checkbox(&mut self.project.codegen.vscode, "Generate .vscode config").changed();
                        if changed {
                            self.mark_dirty();
                        }
                    });
                    ui.separator();
                    ui.label(RichText::new(&self.project.name).strong().size(15.0).color(theme::TEXT));
                    ui.label(
                        RichText::new(format!("{} · {}", self.project.chip, self.project.package))
                            .monospace()
                            .color(theme::ACCENT),
                    );
                    if self.dirty {
                        ui.label(RichText::new("●").color(theme::WARN)).on_hover_text("Unsaved changes");
                    }
                    ui.add_space(16.0);
                    for (tab, label) in [(Tab::Pinout, "Pinout"), (Tab::Clock, "Clock Configuration"), (Tab::Code, "Code Preview")] {
                        if ui.selectable_label(self.tab == tab, RichText::new(label).size(14.0)).clicked() {
                            self.tab = tab;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let ready = self.chip.is_some() && self.tree.is_some();
                        if ui.add_enabled(ready, theme::primary_button("⚙  Generate Code")).clicked() {
                            self.generate(actions);
                        }
                        if ui.button("Save").clicked() {
                            self.save(actions);
                        }
                        if ui.button("⌂ Hub").on_hover_text("Save and return to the Project Hub").clicked() {
                            self.save_quiet();
                            actions.push(Action::CloseProject);
                        }
                    });
                });
            });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let small = |t: String| RichText::new(t).size(11.5).color(theme::TEXT_DIM);
                    ui.label(small(self.dir.display().to_string()));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.tree.is_some() {
                            ui.label(small(format!(
                                "SYSCLK {}  ·  HCLK {}",
                                format_hz(self.clocks.sysclk),
                                format_hz(self.clocks.hclk)
                            )));
                        }
                        let used = self.project.pins.len();
                        ui.label(small(format!("{used} pins configured")));
                        let enabled = self.project.peripherals.values().filter(|p| p.enabled).count();
                        ui.label(small(format!("{enabled} peripherals enabled")));
                        if let Some(chip) = &self.chip {
                            ui.label(small(format!("core {}", chip.core_name())));
                        }
                    });
                });
            });
    }

    fn code_view(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        if self.preview_dirty {
            self.preview = codegen::generate(&self.project, chip, self.tree.as_ref());
            self.preview_dirty = false;
        }
        egui::Panel::left("code_files")
            .resizable(false)
            .exact_size(210.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::same(10)))
            .show(ui, |ui| {
                theme::section(ui, "Generated files");
                ui.add_space(4.0);
                for (i, f) in self.preview.iter().enumerate() {
                    let tag = match f.policy {
                        codegen::Policy::Overwrite => "",
                        codegen::Policy::CreateOnly => "  (once)",
                        codegen::Policy::KeepUserSection => "  (merged)",
                    };
                    let text = RichText::new(format!("{}{tag}", f.path)).monospace().size(12.0);
                    if ui.selectable_label(self.preview_file == i, text).clicked() {
                        self.preview_file = i;
                    }
                }
                ui.add_space(10.0);
                if let Some(tree) = &self.tree {
                    let (txt, color) = match codegen::rcc_flavor(tree) {
                        RccFlavor::SummaryOnly => (
                            format!("RCC '{}': clock tree written as comments; set config.rcc by hand.", tree.rcc_version),
                            theme::WARN,
                        ),
                        _ => (format!("RCC '{}': clock code generated.", tree.rcc_version), theme::TEXT_DIM),
                    };
                    ui.label(RichText::new(txt).size(11.0).color(color));
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new("(once) files are created on first generation and then belong to you.")
                        .size(11.0)
                        .color(theme::TEXT_DIM),
                );
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(12)))
            .show(ui, |ui| {
                if let Some(f) = self.preview.get(self.preview_file) {
                    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                        let mut text = f.contents.as_str();
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .code_editor()
                                .desired_width(f32::INFINITY)
                                .frame(egui::Frame::NONE),
                        );
                    });
                }
            });
    }

    // ---- shared pin helpers ----

    /// Assign a mode to a pin; peripheral assignments also enable the peripheral and
    /// release any other pin carrying the same signal.
    fn assign_pin(&mut self, pin: &str, mode: Option<PinMode>) {
        match mode {
            None => {
                self.project.pins.remove(pin);
            }
            Some(mode) => {
                if let PinMode::Peripheral { peripheral, signal, .. } = &mode {
                    self.project.pins.retain(|p, c| {
                        p == pin || !matches!(&c.mode, PinMode::Peripheral { peripheral: pp, signal: ss, .. } if pp == peripheral && ss == signal)
                    });
                    self.project.peripherals.entry(peripheral.clone()).or_default().enabled = true;
                }
                let label = self.project.pins.get(pin).map(|c| c.label.clone()).unwrap_or_default();
                let mut cfg = PinConfig::new(mode);
                cfg.label = label;
                self.project.pins.insert(pin.to_string(), cfg);
            }
        }
        self.mark_dirty();
    }

    /// Peripheral signals assigned to more than one pin.
    fn conflicts(&self) -> std::collections::BTreeSet<String> {
        let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (pin, c) in &self.project.pins {
            if let PinMode::Peripheral { .. } = c.mode {
                seen.entry(c.mode.label()).or_default().push(pin.clone());
            }
        }
        seen.into_values().filter(|v| v.len() > 1).flatten().collect()
    }
}
