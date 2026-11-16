//! Clock configuration for ESP32 chips.
//!
//! esp-hal configures the whole clock tree from one `CpuClock` preset (PLL, CPU divider,
//! APB and peripheral clocks follow from it), so the tab offers the presets of the chip's
//! esp-hal release and shows the crystal frequencies from its metadata.

use egui::{RichText, Sense, Stroke, pos2, vec2};

use super::Editor;
use crate::codegen_esp;
use crate::db::Chip;
use crate::theme;

impl Editor {
    pub(super) fn esp_clock_view(&mut self, ui: &mut egui::Ui, chip: &Chip) {
        let Some(esp) = chip.esp.clone() else { return };
        let current = codegen_esp::cpu_clock_mhz(&self.project, &esp);
        let max = esp.max_cpu_clock_mhz();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 20)).show(ui, |ui| {
                ui.set_max_width(720.0);
                theme::heading(ui, "Clock Configuration");
                ui.label(
                    RichText::new(
                        "esp-hal sets up the PLL, CPU, APB and peripheral clocks from one CPU clock preset. \
                         Peripheral clock sources follow from it.",
                    )
                    .color(theme::TEXT_DIM),
                );
                ui.add_space(16.0);

                // Source -> CPU diagram.
                let xtal = match esp.xtal_mhz.as_slice() {
                    [] => "XTAL".to_string(),
                    [one] => format!("XTAL {one} MHz"),
                    many => format!("XTAL {} MHz", many.iter().map(u32::to_string).collect::<Vec<_>>().join(" / ")),
                };
                let cpu = current.map(|f| format!("CPU {f} MHz")).unwrap_or_else(|| "CPU (default)".into());
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(620.0), 70.0), Sense::hover());
                let p = ui.painter_at(rect);
                let boxes = [(xtal.as_str(), 0.0), ("PLL", 0.37), (cpu.as_str(), 0.74)];
                let w = rect.width() * 0.26;
                for (i, (text, x)) in boxes.iter().enumerate() {
                    let r = egui::Rect::from_min_size(pos2(rect.left() + rect.width() * x, rect.top() + 12.0), vec2(w, 44.0));
                    let accent = i == 2;
                    p.rect_filled(r, 6.0, if accent { theme::ACCENT_DIM } else { theme::CARD });
                    p.rect_stroke(r, 6.0, Stroke::new(1.0, if accent { theme::ACCENT } else { theme::BORDER }), egui::StrokeKind::Inside);
                    p.text(r.center(), egui::Align2::CENTER_CENTER, *text, egui::FontId::proportional(14.0), theme::TEXT);
                    if i < 2 {
                        let a = pos2(r.right() + 4.0, r.center().y);
                        let b = pos2(rect.left() + rect.width() * boxes[i + 1].1 - 4.0, r.center().y);
                        p.arrow(a, b - a, Stroke::new(1.5, theme::TEXT_DIM));
                    }
                }
                ui.add_space(16.0);

                theme::card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::section(ui, "CPU clock");
                    ui.add_space(4.0);
                    if esp.cpu_clocks_mhz.is_empty() {
                        ui.label(RichText::new("No CpuClock presets found for this chip; esp-hal's default is used.").color(theme::WARN));
                    }
                    let mut pick = None;
                    for f in &esp.cpu_clocks_mhz {
                        let mut tags = Vec::new();
                        if Some(*f) == max {
                            tags.push("maximum");
                        }
                        if Some(*f) == esp.default_cpu_clock_mhz {
                            tags.push("esp-hal default");
                        }
                        let tag = if tags.is_empty() { String::new() } else { format!("   ({})", tags.join(", ")) };
                        let text = RichText::new(format!("{f} MHz{tag}")).monospace();
                        if ui.radio(current == Some(*f), text).clicked() {
                            pick = Some(*f);
                        }
                    }
                    if let Some(f) = pick {
                        // The maximum is the default of new projects; store only other choices.
                        let value = (Some(f) != max).then_some(f);
                        if self.project.cpu_clock_mhz != value {
                            self.project.cpu_clock_mhz = value;
                            self.mark_dirty();
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Generated as `esp_hal::Config::default().with_cpu_clock(CpuClock::…)` in board::config().")
                            .size(11.5)
                            .color(theme::TEXT_DIM),
                    );
                });
                ui.add_space(12.0);

                theme::card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    theme::section(ui, "Chip");
                    egui::Grid::new("esp_clock_info").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                        let kv = |ui: &mut egui::Ui, k: &str, v: String| {
                            ui.label(RichText::new(k).color(theme::TEXT_DIM));
                            ui.label(RichText::new(v).monospace());
                            ui.end_row();
                        };
                        kv(ui, "Core", format!("{} × {}", esp.cores, if esp.is_xtensa() { "Xtensa" } else { "RISC-V" }));
                        kv(ui, "Target", esp.target.clone());
                        if esp.xtal_mhz.len() > 1 {
                            kv(ui, "Crystal", "either value; handled by esp-hal".into());
                        }
                        kv(ui, "Time driver", "esp-rtos on TIMG0".into());
                    });
                });
            });
        });
    }
}
