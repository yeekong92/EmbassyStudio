//! Project Hub: the launch screen with recent projects and the new-project wizard.

use std::path::PathBuf;

use egui::{Align, Layout, RichText, Sense};

use crate::app::{Action, Data, Load};
use crate::db::{DataSource, Platform, family_of};
use crate::project::{Project, Settings};
use crate::theme;

#[derive(Default)]
pub struct Hub {
    name: String,
    location: Option<PathBuf>,
    search: String,
    selector_open: bool,
    chip: Option<String>,
    package: Option<String>,
    error: Option<String>,
    /// Put the keyboard focus in the project name box.
    focus_name: bool,
    /// MCU selector: only chips of this platform (`None` = all).
    platform: Option<Platform>,
}

impl Hub {
    /// The Hub, ready for typing a new project's name.
    pub fn new_project() -> Self {
        Self { focus_name: true, ..Self::default() }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, data: &mut Data, settings: &mut Settings, actions: &mut Vec<Action>) {
        if self.location.is_none() {
            self.location = settings
                .last_location
                .clone()
                .or_else(dirs::document_dir)
                .or_else(dirs::home_dir);
        }

        egui::Panel::top("hub_header")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(24, 14)))
            .show(ui, |ui| self.header(ui, data));

        egui::Panel::left("hub_recent")
            .resizable(false)
            .exact_size(440.0)
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 24, right: 12, top: 8, bottom: 16 }))
            .show(ui, |ui| self.recent(ui, settings, actions));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 12, right: 24, top: 8, bottom: 16 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.open_card(ui, actions);
                    ui.add_space(14.0);
                    self.new_project_card(ui, data, actions);
                });
            });
    }

    fn header(&mut self, ui: &mut egui::Ui, data: &mut Data) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), Sense::hover());
            logo(ui.painter(), rect);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Embassy Studio").size(22.0).strong().color(theme::TEXT));
                ui.label(
                    RichText::new("Open-source STM32 & ESP32 configurator and Embassy code generator")
                        .size(12.0)
                        .color(theme::TEXT_DIM),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("Data source ▾", |ui| {
                    if ui
                        .radio(data.source == DataSource::GitHub, "GitHub (stm32-data-generated)")
                        .clicked()
                    {
                        data.set_source(DataSource::GitHub);
                        ui.close();
                    }
                    if ui.button("Local stm32-data folder…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Select stm32-data checkout").pick_folder() {
                            data.set_source(DataSource::Local(dir));
                        }
                        ui.close();
                    }
                });
                let sync = ui.add_enabled(!data.index_loading, egui::Button::new("⟳ Sync database"));
                if sync.on_hover_text("Re-download the chip list from the data source").clicked() {
                    data.sync_index();
                }
                ui.add_space(8.0);
                let status = if data.index_loading {
                    RichText::new("Syncing chip databases…").color(theme::WARN)
                } else if let Some(e) = &data.index_error {
                    RichText::new(format!("Database error: {e}")).color(theme::ERROR)
                } else if let Some(i) = &data.index {
                    let esp = i.chips.iter().filter(|c| Platform::of(c) == Platform::Esp).count();
                    let text = match &i.esp_source {
                        Some(e) => format!("{} STM32 · {} · {esp} ESP32 · {e}", i.chips.len() - esp, i.source),
                        None => format!("{} chips · {}", i.chips.len(), i.source),
                    };
                    RichText::new(text).color(theme::TEXT_DIM)
                } else {
                    RichText::new("No database").color(theme::WARN)
                };
                if data.index_loading {
                    ui.spinner();
                }
                ui.label(status.size(12.0));
            });
        });
    }

    fn recent(&mut self, ui: &mut egui::Ui, settings: &mut Settings, actions: &mut Vec<Action>) {
        theme::section(ui, "Recent projects");
        ui.add_space(6.0);
        if settings.recent.is_empty() {
            theme::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("No recent projects yet.").color(theme::TEXT_DIM));
                ui.label(RichText::new("Create one on the right, or open an existing folder.").color(theme::TEXT_DIM));
            });
            return;
        }
        let mut remove = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, r) in settings.recent.iter().enumerate() {
                let exists = r.path.join(crate::project::PROJECT_FILE).exists();
                let id = ui.make_persistent_id(("recent", i));
                let hovered = ui.ctx().memory(|m| m.data.get_temp::<bool>(id)).unwrap_or(false);
                let resp = egui::Frame::new()
                    .fill(if hovered { theme::CARD_HOVER } else { theme::CARD })
                    .stroke(egui::Stroke::new(1.0, if hovered { theme::ACCENT_DIM } else { theme::BORDER }))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let color = if exists { theme::TEXT } else { theme::TEXT_DIM };
                            ui.label(RichText::new(&r.name).size(15.0).strong().color(color));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(&r.chip).monospace().color(theme::ACCENT));
                            });
                        });
                        ui.label(RichText::new(r.path.display().to_string()).size(11.5).color(theme::TEXT_DIM));
                        if !exists {
                            ui.label(RichText::new("Folder missing").size(11.0).color(theme::WARN));
                        } else {
                            ui.label(RichText::new(format_age(r.last_opened)).size(11.0).color(theme::TEXT_DIM));
                        }
                    })
                    .response
                    .interact(Sense::click());
                ui.ctx().memory_mut(|m| m.data.insert_temp(id, resp.hovered()));
                if resp.clicked() && exists {
                    actions.push(Action::OpenProject(r.path.clone()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        actions.push(Action::OpenProject(r.path.clone()));
                        ui.close();
                    }
                    if ui.button("Show in Explorer").clicked() {
                        open_in_explorer(&r.path);
                        ui.close();
                    }
                    if ui.button("Remove from list").clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
                ui.add_space(6.0);
            }
        });
        if let Some(i) = remove {
            settings.recent.remove(i);
            settings.save();
        }
    }

    fn open_card(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        theme::section(ui, "Open project");
        ui.add_space(6.0);
        theme::card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Open an existing project folder").strong().color(theme::TEXT));
                    ui.label(
                        RichText::new(format!("Any folder containing {}", crate::project::PROJECT_FILE))
                            .size(12.0)
                            .color(theme::TEXT_DIM),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(egui::Button::new("📂  Open Project…")).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Open Embassy Studio project").pick_folder() {
                            actions.push(Action::OpenProject(dir));
                        }
                    }
                });
            });
        });
    }

    fn new_project_card(&mut self, ui: &mut egui::Ui, data: &mut Data, actions: &mut Vec<Action>) {
        theme::section(ui, "New project");
        ui.add_space(6.0);
        theme::card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new("new_project_form")
                .num_columns(2)
                .spacing([16.0, 12.0])
                .min_col_width(130.0)
                .show(ui, |ui| {
                    ui.label(RichText::new("Project name").color(theme::TEXT_DIM));
                    let name = ui.add(
                        egui::TextEdit::singleline(&mut self.name)
                            .hint_text("my-firmware")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut self.focus_name) {
                        name.request_focus();
                    }
                    ui.end_row();

                    ui.label(RichText::new("Location").color(theme::TEXT_DIM));
                    ui.horizontal(|ui| {
                        if ui.button("Browse…").clicked() {
                            let mut dlg = rfd::FileDialog::new().set_title("Project location");
                            if let Some(l) = &self.location {
                                dlg = dlg.set_directory(l);
                            }
                            if let Some(dir) = dlg.pick_folder() {
                                self.location = Some(dir);
                            }
                        }
                        let loc = self.location.as_ref().map(|l| l.display().to_string()).unwrap_or_default();
                        ui.label(RichText::new(loc).monospace().size(12.0).color(theme::TEXT));
                    });
                    ui.end_row();

                    ui.label(RichText::new("MCU").color(theme::TEXT_DIM));
                    self.mcu_selector(ui, data);
                    ui.end_row();
                });

            if let Some(chip_name) = self.chip.clone() {
                ui.add_space(10.0);
                self.chip_details(ui, data, &chip_name);
            }

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let target = self.target_dir();
                if let Some(t) = &target {
                    ui.label(RichText::new(format!("→ {}", t.display())).size(12.0).color(theme::TEXT_DIM));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ready = target.is_some() && self.chip.is_some() && self.package.is_some();
                    if ui.add_enabled(ready, theme::primary_button("Create Project")).clicked() {
                        self.create(actions);
                    }
                    if let Some(e) = &self.error {
                        ui.label(RichText::new(e).color(theme::ERROR));
                    }
                });
            });
        });
    }

    fn mcu_selector(&mut self, ui: &mut egui::Ui, data: &mut Data) {
        ui.vertical(|ui| {
            let label = self.chip.clone().unwrap_or_else(|| "Select a microcontroller…".into());
            let button = egui::Button::new(
                RichText::new(format!("{label}   {}", if self.selector_open { "▴" } else { "▾" }))
                    .monospace()
                    .color(if self.chip.is_some() { theme::ACCENT } else { theme::TEXT }),
            )
            .min_size(egui::vec2(320.0, 28.0));
            let just_opened = ui.add(button).clicked() && {
                self.selector_open = !self.selector_open;
                self.selector_open
            };
            if !self.selector_open {
                return;
            }
            let Some(index) = &data.index else {
                if data.index_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading chip list…");
                    });
                } else {
                    ui.label(RichText::new("Chip database unavailable. Use “Sync database” above.").color(theme::WARN));
                }
                return;
            };
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width().min(560.0));
                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("🔍 Search, e.g. F411, G431CB, ESP32C3, S3")
                            .desired_width(f32::INFINITY),
                    );
                    if just_opened {
                        search.request_focus();
                    }
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.platform, None, "All");
                        ui.selectable_value(&mut self.platform, Some(Platform::Stm32), Platform::Stm32.title());
                        ui.selectable_value(&mut self.platform, Some(Platform::Esp), Platform::Esp.title());
                    });
                    let tokens: Vec<String> = self.search.split_whitespace().map(|t| t.to_ascii_uppercase()).collect();
                    let matches: Vec<&String> = index
                        .chips
                        .iter()
                        .filter(|c| self.platform.is_none_or(|p| Platform::of(c) == p))
                        .filter(|c| tokens.iter().all(|t| c.contains(t.as_str())))
                        .collect();
                    ui.label(RichText::new(format!("{} of {} chips", matches.len(), index.chips.len())).size(11.0).color(theme::TEXT_DIM));
                    let row_h = 22.0;
                    egui::ScrollArea::vertical()
                        .max_height(260.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_h, matches.len(), |ui, range| {
                            for chip in &matches[range] {
                                let selected = self.chip.as_deref() == Some(chip.as_str());
                                ui.horizontal(|ui| {
                                    let r = ui.add_sized(
                                        [200.0, row_h - 2.0],
                                        egui::Button::selectable(selected, RichText::new(chip.as_str()).monospace()),
                                    );
                                    ui.label(RichText::new(family_of(chip)).size(11.0).color(theme::TEXT_DIM));
                                    if r.clicked() {
                                        self.chip = Some(chip.to_string());
                                        self.package = None;
                                        self.selector_open = false;
                                        self.error = None;
                                    }
                                });
                            }
                        });
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) && matches.len() == 1 {
                        self.chip = Some(matches[0].clone());
                        self.package = None;
                        self.selector_open = false;
                    }
                });
        });
    }

    fn chip_details(&mut self, ui: &mut egui::Ui, data: &mut Data, name: &str) {
        match data.chip(name) {
            Load::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!(
                        "Loading {name} from {}…",
                        if Platform::of(name) == Platform::Esp { "esp-hal metadata" } else { "stm32-data" }
                    ));
                });
            }
            Load::Failed(e) => {
                ui.label(RichText::new(format!("Failed to load {name}: {e}")).color(theme::ERROR));
            }
            Load::Ready(chip) => {
                if self.package.is_none() {
                    self.package = chip.packages.first().map(|p| p.name.clone());
                }
                egui::Frame::new()
                    .fill(theme::PANEL)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::Grid::new("chip_info").num_columns(4).spacing([18.0, 6.0]).show(ui, |ui| {
                            let kv = |ui: &mut egui::Ui, k: &str, v: String| {
                                ui.label(RichText::new(k).size(12.0).color(theme::TEXT_DIM));
                                ui.label(RichText::new(v).monospace().color(theme::TEXT));
                            };
                            kv(ui, "Family", chip.family.clone());
                            kv(ui, "Line", chip.line.clone());
                            ui.end_row();
                            if let Some(esp) = &chip.esp {
                                kv(ui, "Target", esp.target.clone());
                                kv(ui, "Peripherals", chip.peripherals().len().to_string());
                                ui.end_row();
                                let cpu = esp.max_cpu_clock_mhz().map(|f| format!("{f} MHz")).unwrap_or_else(|| "-".into());
                                kv(ui, "CPU", cpu);
                                kv(ui, "SRAM", format_size(chip.ram_size()));
                                ui.end_row();
                                kv(ui, "GPIOs", chip.packages.first().map(|p| p.pins.len()).unwrap_or(0).to_string());
                                kv(ui, "Flash", "external (module)".into());
                                ui.end_row();
                            } else {
                                kv(ui, "Core", chip.cores.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(" + "));
                                kv(ui, "Peripherals", chip.peripherals().len().to_string());
                                ui.end_row();
                                kv(ui, "Flash", format_size(chip.flash_size()));
                                kv(ui, "RAM", format_size(chip.ram_size()));
                                ui.end_row();
                            }
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Package").size(12.0).color(theme::TEXT_DIM));
                            let current = self.package.clone().unwrap_or_default();
                            let text = chip
                                .package(&current)
                                .map(|p| format!("{}  ({}, {} pins)", p.name, p.package, p.pins.len()))
                                .unwrap_or(current);
                            egui::ComboBox::from_id_salt("package")
                                .selected_text(text)
                                .width(320.0)
                                .show_ui(ui, |ui| {
                                    for p in &chip.packages {
                                        ui.selectable_value(
                                            &mut self.package,
                                            Some(p.name.clone()),
                                            format!("{}  ({}, {} pins)", p.name, p.package, p.pins.len()),
                                        );
                                    }
                                });
                        });
                        if let Some(doc) = chip.docs.iter().find(|d| d.url.contains("reference_manual")).or(chip.docs.first()) {
                            ui.hyperlink_to(RichText::new(format!("📄 {}", doc.title)).size(12.0), &doc.url);
                        }
                    });
            }
        }
    }

    fn target_dir(&self) -> Option<PathBuf> {
        let name = self.name.trim();
        if name.is_empty() || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return None;
        }
        Some(self.location.as_ref()?.join(name))
    }

    fn create(&mut self, actions: &mut Vec<Action>) {
        let (Some(dir), Some(chip), Some(package)) = (self.target_dir(), &self.chip, &self.package) else {
            return;
        };
        if dir.join(crate::project::PROJECT_FILE).exists() {
            self.error = Some("A project already exists there.".into());
            return;
        }
        if dir.is_dir() && std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
            self.error = Some("Folder exists and is not empty.".into());
            return;
        }
        self.error = None;
        let project = Project::new(self.name.trim(), chip, package);
        actions.push(Action::CreateProject { dir, project });
    }
}

/// Draw the app mark: a chip outline with an accent core.
pub fn logo(painter: &egui::Painter, rect: egui::Rect) {
    let c = rect.center();
    let s = rect.width() * 0.5;
    let body = egui::Rect::from_center_size(c, egui::vec2(s * 1.3, s * 1.3));
    for i in 0..3 {
        let t = (i as f32 + 0.5) / 3.0;
        let x = egui::lerp(body.left()..=body.right(), t);
        let y = egui::lerp(body.top()..=body.bottom(), t);
        let st = egui::Stroke::new(2.0, theme::TEXT_DIM);
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, body.top())], st);
        painter.line_segment([egui::pos2(x, body.bottom()), egui::pos2(x, rect.bottom())], st);
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(body.left(), y)], st);
        painter.line_segment([egui::pos2(body.right(), y), egui::pos2(rect.right(), y)], st);
    }
    painter.rect_filled(body, 4.0, theme::CARD);
    painter.rect_stroke(body, 4.0, egui::Stroke::new(1.5, theme::ACCENT), egui::StrokeKind::Inside);
    painter.rect_filled(egui::Rect::from_center_size(c, egui::vec2(s * 0.55, s * 0.55)), 2.0, theme::ACCENT);
}

pub fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 && bytes % (1024 * 1024) == 0 {
        format!("{} MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

fn format_age(unix: u64) -> String {
    let secs = crate::project::now_unix().saturating_sub(unix);
    match secs {
        0..=59 => "Opened just now".into(),
        60..=3599 => format!("Opened {} min ago", secs / 60),
        3600..=86_399 => format!("Opened {} h ago", secs / 3600),
        _ => format!("Opened {} days ago", secs / 86_400),
    }
}

pub fn open_in_explorer(path: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
