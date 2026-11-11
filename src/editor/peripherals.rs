//! Peripheral configuration panel: enable/disable hardware blocks and route their signals.

use std::collections::BTreeMap;

use egui::{RichText, Sense};

use super::Editor;
use crate::clock::format_hz;
use crate::db::{Chip, KernelClock, Peripheral};
use crate::project::PinMode;
use crate::theme;

/// Register kinds that are part of the core system rather than user-facing peripherals.
const SYSTEM_KINDS: &[&str] = &["rcc", "gpio", "flash", "pwr", "dbgmcu", "syscfg", "exti", "afio", "uid", "bdma", "dma", "dmamux", "gpdma", "lpdma", "mdma", "vrefintcal", "tamp", "fmac", "icache", "dcache", "ramcfg", "sbs", "pka", "ipcc", "hsem", "adccommon", "flash_eeprom", "nvic", "scb"];

fn kind(p: &Peripheral) -> &str {
    p.registers.as_ref().map(|r| r.kind.as_str()).unwrap_or("")
}

fn is_user_peripheral(p: &Peripheral) -> bool {
    p.registers.is_some() && !SYSTEM_KINDS.contains(&kind(p))
}

impl Editor {
    /// Clock feeding a peripheral, resolved through its bus or kernel-clock mux.
    fn peripheral_clock(&self, p: &Peripheral) -> Option<(String, f64)> {
        let rcc = p.rcc.as_ref()?;
        match &rcc.kernel_clock {
            Some(KernelClock::Mux(m)) => {
                let key = format!("{}.{}", m.register, m.field);
                let sel = self.tree.as_ref()?.muxes.get(&key)?;
                let v = self.project.clock.muxes.get(&key).cloned().unwrap_or_else(|| sel.default_variant().to_string());
                let f = self.clocks.bus(&v).or_else(|| self.clocks.bus(v.trim_end_matches("_P")))?;
                Some((v, f))
            }
            Some(KernelClock::Clock(c)) => Some((c.clone(), self.clocks.bus(c)?)),
            None => {
                let b = rcc.bus_clock.clone()?;
                let f = self.clocks.bus(&b)?;
                Some((b, f))
            }
        }
    }

    pub(super) fn peripheral_panel(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        ui.add_space(6.0);
        theme::heading(ui, "Peripherals");
        ui.add(
            egui::TextEdit::singleline(&mut self.periph_search)
                .hint_text("🔍 Filter (e.g. usart, spi1)")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        let mut groups: BTreeMap<String, Vec<&Peripheral>> = BTreeMap::new();
        let filter = self.periph_search.to_ascii_uppercase();
        for p in chip.peripherals().iter().filter(|p| is_user_peripheral(p)) {
            if !filter.is_empty() && !p.name.contains(&filter) && !kind(p).to_ascii_uppercase().contains(&filter) {
                continue;
            }
            groups.entry(kind(p).to_ascii_uppercase()).or_default().push(p);
        }

        let detail_height = if self.selected_periph.is_some() { ui.available_height() * 0.5 } else { 0.0 };
        egui::ScrollArea::vertical()
            .id_salt("periph_list")
            .max_height(ui.available_height() - detail_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (group, list) in &groups {
                    let enabled = list.iter().filter(|p| self.project.is_peripheral_enabled(&p.name)).count();
                    let header = if enabled > 0 {
                        RichText::new(format!("{group}   {enabled}/{}", list.len())).strong().color(theme::ACCENT)
                    } else {
                        RichText::new(format!("{group}   {}", list.len())).strong()
                    };
                    egui::CollapsingHeader::new(header)
                        .id_salt(group)
                        .default_open(!filter.is_empty() || enabled > 0)
                        .show(ui, |ui| {
                            for p in list {
                                self.peripheral_row(ui, p);
                            }
                        });
                }
            });

        if let Some(name) = self.selected_periph.clone() {
            if let Some(p) = chip.peripheral(&name) {
                ui.separator();
                egui::ScrollArea::vertical().id_salt("periph_detail").auto_shrink([false, false]).show(ui, |ui| {
                    self.peripheral_detail(ui, p);
                });
            }
        }
    }

    fn peripheral_row(&mut self, ui: &mut egui::Ui, p: &Peripheral) {
        let mut enabled = self.project.is_peripheral_enabled(&p.name);
        let selected = self.selected_periph.as_deref() == Some(p.name.as_str());
        ui.horizontal(|ui| {
            if ui.checkbox(&mut enabled, "").changed() {
                self.project.set_peripheral_enabled(&p.name, enabled);
                self.mark_dirty();
            }
            let text = RichText::new(&p.name).monospace().color(if enabled { theme::TEXT } else { theme::TEXT_DIM });
            if ui.selectable_label(selected, text).clicked() {
                self.selected_periph = if selected { None } else { Some(p.name.clone()) };
            }
            if enabled {
                let routed = self.project.peripheral_pins(&p.name).len();
                if routed > 0 {
                    ui.label(RichText::new(format!("{routed} pin{}", if routed == 1 { "" } else { "s" })).size(11.0).color(theme::TEXT_DIM));
                }
            }
        });
    }

    fn peripheral_detail(&mut self, ui: &mut egui::Ui, p: &Peripheral) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&p.name).strong().size(16.0).color(theme::ACCENT));
            if let Some(r) = &p.registers {
                ui.label(RichText::new(format!("{} {}", r.kind, r.version)).size(11.0).color(theme::TEXT_DIM));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").clicked() {
                    self.selected_periph = None;
                }
            });
        });
        let mut enabled = self.project.is_peripheral_enabled(&p.name);
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            self.project.set_peripheral_enabled(&p.name, enabled);
            self.mark_dirty();
        }

        egui::Grid::new(("periph_info", &p.name)).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            ui.label(RichText::new("Address").color(theme::TEXT_DIM));
            ui.label(RichText::new(format!("0x{:08X}", p.address)).monospace());
            ui.end_row();
            if let Some(b) = p.rcc.as_ref().and_then(|r| r.bus_clock.as_ref()) {
                ui.label(RichText::new("Bus").color(theme::TEXT_DIM));
                ui.label(RichText::new(b).monospace());
                ui.end_row();
            }
            if let Some((src, f)) = self.peripheral_clock(p) {
                ui.label(RichText::new("Clock").color(theme::TEXT_DIM));
                ui.label(RichText::new(format!("{} ({src})", format_hz(f))).monospace());
                ui.end_row();
            }
            if !p.interrupts.is_empty() {
                ui.label(RichText::new("IRQ").color(theme::TEXT_DIM));
                let mut irqs: Vec<&str> = p.interrupts.iter().map(|i| i.interrupt.as_str()).collect();
                irqs.dedup();
                ui.label(RichText::new(irqs.join(", ")).monospace().size(12.0));
                ui.end_row();
            }
            if !p.dma_channels.is_empty() {
                ui.label(RichText::new("DMA").color(theme::TEXT_DIM));
                ui.label(RichText::new(format!("{} request(s)", p.dma_channels.len())).size(12.0));
                ui.end_row();
            }
        });

        // Kernel clock mux
        if let Some(KernelClock::Mux(m)) = p.rcc.as_ref().and_then(|r| r.kernel_clock.as_ref()) {
            let key = format!("{}.{}", m.register, m.field);
            if let Some(sel) = self.tree.as_ref().and_then(|t| t.muxes.get(&key)).cloned() {
                let mut v = self.project.clock.muxes.get(&key).cloned().unwrap_or_else(|| sel.default_variant().to_string());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Kernel clock").color(theme::TEXT_DIM));
                    egui::ComboBox::from_id_salt(("kmux", &key)).selected_text(&v).show_ui(ui, |ui| {
                        for var in &sel.variants {
                            ui.selectable_value(&mut v, var.name.clone(), &var.name)
                                .on_hover_text(var.description.clone().unwrap_or_default());
                        }
                    });
                });
                let current = self.project.clock.muxes.get(&key).map(String::as_str).unwrap_or(sel.default_variant());
                if current != v {
                    self.project.clock.muxes.insert(key, v);
                    self.mark_dirty();
                }
            }
        }

        // Settings for drivers the code generator knows.
        let settings: &[(&str, &str, &str)] = match kind(p) {
            "usart" | "lpuart" => &[("baudrate", "Baud rate", "115200")],
            "spi" => &[("frequency", "SCK frequency (Hz)", "1000000")],
            "i2c" => &[("frequency", "Bus frequency (Hz)", "100000")],
            _ => &[],
        };
        for (key, label, default) in settings {
            let mut v = self.project.setting(&p.name, key).unwrap_or(default).to_string();
            ui.horizontal(|ui| {
                ui.label(RichText::new(*label).color(theme::TEXT_DIM));
                ui.add(egui::TextEdit::singleline(&mut v).desired_width(100.0));
            });
            let digits: String = v.chars().filter(|c| c.is_ascii_digit()).collect();
            let current = self.project.setting(&p.name, key).unwrap_or(default);
            if !digits.is_empty() && digits != current {
                self.project.peripherals.entry(p.name.clone()).or_default().settings.insert(key.to_string(), digits);
                self.mark_dirty();
            }
        }

        // Signal routing
        ui.add_space(6.0);
        theme::section(ui, "Signals");
        let mut signals: BTreeMap<&str, Vec<(&str, Option<u8>)>> = BTreeMap::new();
        for pin in &p.pins {
            signals.entry(pin.signal.as_str()).or_default().push((pin.pin.as_str(), pin.af));
        }
        let package_gpios: std::collections::BTreeSet<String> = self
            .chip
            .as_ref()
            .and_then(|c| c.package(&self.project.package))
            .map(|pkg| pkg.pins.iter().flat_map(|pp| pp.signals.iter().filter_map(|s| crate::db::gpio_name(s))).collect())
            .unwrap_or_default();
        if signals.is_empty() {
            ui.label(RichText::new("No external signals.").color(theme::TEXT_DIM));
        }
        let assigned: BTreeMap<String, String> = self.project.peripheral_pins(&p.name).into_iter().map(|(pin, sig)| (sig, pin)).collect();
        let mut change: Option<(String, Option<String>, Option<u8>)> = None;
        egui::Grid::new(("sig", &p.name)).num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
            for (sig, pins) in &signals {
                let available: Vec<_> = pins.iter().filter(|(pin, _)| package_gpios.contains(*pin)).collect();
                ui.label(RichText::new(*sig).monospace());
                let current = assigned.get(*sig).cloned();
                let text = current.clone().unwrap_or_else(|| "—".into());
                egui::ComboBox::from_id_salt(("sigpin", &p.name, *sig)).selected_text(text).width(110.0).show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_none(), "—").clicked() {
                        change = Some((sig.to_string(), None, None));
                    }
                    for (pin, af) in &available {
                        let busy = self.project.pins.get(*pin).filter(|c| c.mode.peripheral() != Some(p.name.as_str()));
                        let label = match busy {
                            Some(c) => format!("{pin}  (used: {})", c.mode.label()),
                            None => pin.to_string(),
                        };
                        if ui.selectable_label(current.as_deref() == Some(*pin), label).clicked() {
                            change = Some((sig.to_string(), Some(pin.to_string()), *af));
                        }
                    }
                    if available.is_empty() {
                        ui.label(RichText::new("Not bonded in this package").color(theme::TEXT_DIM));
                    }
                });
                ui.end_row();
            }
        });
        if let Some((sig, pin, af)) = change {
            if let Some(old) = assigned.get(&sig) {
                self.project.pins.remove(old);
            }
            if let Some(pin) = pin {
                self.assign_pin(&pin, Some(PinMode::Peripheral { peripheral: p.name.clone(), signal: sig, af }));
            }
            self.mark_dirty();
        }
        let _ = ui.allocate_response(egui::vec2(1.0, 4.0), Sense::hover());
    }
}
