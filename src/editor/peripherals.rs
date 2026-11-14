//! Peripheral configuration panel: enable/disable hardware blocks and route their signals.

use std::collections::BTreeMap;

use egui::{RichText, Sense};

use super::Editor;
use crate::clock::format_hz;
use crate::db::{Chip, KernelClock, Peripheral};
use crate::project::PinMode;
use crate::theme;

/// Register kinds that are internal plumbing (configured elsewhere or not at all), never listed.
const HIDDEN_KINDS: &[&str] = &["rcc", "gpio", "flash", "pwr", "dbgmcu", "syscfg", "exti", "afio", "uid", "vrefintcal", "tscal", "adccommon", "flash_eeprom", "nvic", "scb", "ramcfg", "sbs", "octospim"];

/// Peripheral categories, in display order (same grouping as STM32CubeMX).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(super) enum Category {
    SystemCore,
    Analog,
    Timers,
    Connectivity,
    Multimedia,
    Security,
    Computing,
    Other,
}

impl Category {
    const ALL: [Category; 8] = [
        Self::SystemCore,
        Self::Analog,
        Self::Timers,
        Self::Connectivity,
        Self::Multimedia,
        Self::Security,
        Self::Computing,
        Self::Other,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::SystemCore => "System Core",
            Self::Analog => "Analog",
            Self::Timers => "Timers",
            Self::Connectivity => "Connectivity",
            Self::Multimedia => "Multimedia",
            Self::Security => "Security",
            Self::Computing => "Computing",
            Self::Other => "Other",
        }
    }

    /// Category for a register kind (from stm32-data) or, for blocks without
    /// register data, the lowercase name without its instance number.
    fn of(kind: &str) -> Self {
        match kind {
            "dma" | "bdma" | "gpdma" | "lpdma" | "hpdma" | "mdma" | "dmamux" | "iwdg" | "wwdg" | "crs" | "icache" | "dcache"
            | "hsem" | "ipcc" => Self::SystemCore,
            "adc" | "dac" | "comp" | "opamp" | "vrefbuf" | "sdadc" | "dfsdm" | "mdf" | "adf" | "dts" => Self::Analog,
            "timer" | "tim" | "lptim" | "rtc" | "hrtim" => Self::Timers,
            "usart" | "lpuart" | "uart" | "spi" | "i2s" | "i2c" | "i3c" | "can" | "fdcan" | "usb" | "otg" | "usbram" | "eth"
            | "sdmmc" | "octospi" | "quadspi" | "xspi" | "hspi" | "fmc" | "fsmc" | "swpmi" | "ucpd" | "mdios" | "cec" => {
                Self::Connectivity
            }
            "sai" | "dcmi" | "dcmipp" | "dma2d" | "ltdc" | "dsihost" | "jpeg" | "spdifrx" | "tsc" | "gfxmmu" | "lcd" | "gpu2d"
            | "pssi" => Self::Multimedia,
            "rng" | "aes" | "cryp" | "hash" | "pka" | "saes" | "tamp" | "gtzc" | "otfdec" | "mpcbb" => Self::Security,
            "crc" | "cordic" | "fmac" => Self::Computing,
            _ => Self::Other,
        }
    }
}

/// Register kind, or for blocks without register data (e.g. FMC) the name minus its digits.
fn kind(p: &Peripheral) -> String {
    match &p.registers {
        Some(r) => r.kind.clone(),
        None => p.name.trim_end_matches(|c: char| c.is_ascii_digit()).to_ascii_lowercase(),
    }
}

fn is_user_peripheral(p: &Peripheral) -> bool {
    (p.registers.is_some() || !p.pins.is_empty()) && !HIDDEN_KINDS.contains(&kind(p).as_str())
}

/// Sort key so TIM2 comes before TIM10.
fn natural_key(name: &str) -> (String, u32, String) {
    let prefix: String = name.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let rest = &name[prefix.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (prefix, digits.parse().unwrap_or(0), rest[digits.len()..].to_string())
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

    /// Left column: peripherals grouped by category. Selecting one opens its
    /// configuration in the column to the right (see `peripheral_config_panel`).
    pub(super) fn peripheral_panel(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        ui.add_space(6.0);
        theme::heading(ui, "Peripherals");
        ui.add(
            egui::TextEdit::singleline(&mut self.periph_search)
                .hint_text("🔍 Filter (e.g. usart, spi1, timers)")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);

        let filter = self.periph_search.trim().to_ascii_uppercase();
        let mut groups: BTreeMap<Category, Vec<&Peripheral>> = BTreeMap::new();
        for p in chip.peripherals().iter().filter(|p| is_user_peripheral(p)) {
            let k = kind(p);
            let cat = Category::of(&k);
            if !filter.is_empty()
                && !p.name.contains(&filter)
                && !k.to_ascii_uppercase().contains(&filter)
                && !cat.title().to_ascii_uppercase().contains(&filter)
            {
                continue;
            }
            groups.entry(cat).or_default().push(p);
        }
        for list in groups.values_mut() {
            list.sort_by_key(|p| natural_key(&p.name));
        }
        let show_middleware = filter.is_empty() || "MIDDLEWARE DEFMT LOGGING EMBASSY".contains(&filter);

        egui::ScrollArea::vertical()
            .id_salt("periph_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for cat in Category::ALL {
                    let Some(list) = groups.get(&cat) else { continue };
                    let enabled = list.iter().filter(|p| self.project.is_peripheral_enabled(&p.name)).count();
                    let header = if enabled > 0 {
                        RichText::new(format!("{}   {enabled}/{}", cat.title(), list.len())).strong().color(theme::ACCENT)
                    } else {
                        RichText::new(format!("{}   {}", cat.title(), list.len())).strong()
                    };
                    let selected_here = self
                        .selected_periph
                        .as_ref()
                        .is_some_and(|s| list.iter().any(|p| &p.name == s));
                    egui::CollapsingHeader::new(header)
                        .id_salt(("category", cat))
                        .default_open(!filter.is_empty() || enabled > 0 || selected_here)
                        .open((!filter.is_empty()).then_some(true))
                        .show(ui, |ui| {
                            for p in list {
                                self.peripheral_row(ui, p);
                            }
                        });
                }
                if show_middleware {
                    egui::CollapsingHeader::new(RichText::new("Middleware").strong())
                        .id_salt(("category", "middleware"))
                        .default_open(!filter.is_empty())
                        .show(ui, |ui| self.middleware_rows(ui));
                }
            });
    }

    /// Software components the generated project is built on.
    fn middleware_rows(&mut self, ui: &mut egui::Ui) {
        let dim = |t: &str| RichText::new(t).size(11.0).color(theme::TEXT_DIM);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.project.codegen.defmt, "").changed() {
                self.mark_dirty();
            }
            ui.label(RichText::new("DEFMT").monospace());
            ui.label(dim("RTT logging + panic-probe"));
        });
        ui.horizontal(|ui| {
            ui.add_enabled(false, egui::Checkbox::without_text(&mut true));
            ui.label(RichText::new("EMBASSY").monospace());
            ui.label(dim("async executor + time driver (always on)"));
        });
    }

    /// Right column: "Mode and Configuration" of the selected peripheral.
    pub(super) fn peripheral_config_panel(&mut self, ui: &mut egui::Ui, p: &Peripheral) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            theme::heading(ui, &format!("{} Configuration", p.name));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Close").clicked() {
                    self.selected_periph = None;
                }
            });
        });
        ui.label(
            RichText::new(format!("{}  ·  {}", Category::of(&kind(p)).title(), p.registers.as_ref().map(|r| format!("{} {}", r.kind, r.version)).unwrap_or_default()))
                .size(11.0)
                .color(theme::TEXT_DIM),
        );
        ui.separator();
        egui::ScrollArea::vertical().id_salt(("periph_detail", &p.name)).auto_shrink([false, false]).show(ui, |ui| {
            self.peripheral_detail(ui, p);
        });
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
        theme::section(ui, "Mode");
        let mut enabled = self.project.is_peripheral_enabled(&p.name);
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            self.project.set_peripheral_enabled(&p.name, enabled);
            self.mark_dirty();
        }

        ui.add_space(6.0);
        theme::section(ui, "Parameter Settings");
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
        let settings: &[(&str, &str, &str)] = match kind(p).as_str() {
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
