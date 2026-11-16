//! Editor top bar: File and Options menus, the view tabs and "Save Project As".

use std::path::{Path, PathBuf};

use egui::{Align, Align2, CornerRadius, FontId, Layout, Rect, RichText, Sense, Stroke, pos2, vec2};

use super::{Editor, Tab};
use crate::app::Action;
use crate::project::{PROJECT_FILE, RecentProject};
use crate::theme;

const TAB_HEIGHT: f32 = 32.0;
const TABS: [(Tab, &str); 4] = [(Tab::Pinout, "Pinout"), (Tab::Schematic, "Schematic"), (Tab::Clock, "Clock Configuration"), (Tab::Code, "Code Preview")];
/// How many recent projects the File menu lists.
const RECENT_SHOWN: usize = 12;

/// The "Save Project As" dialog.
pub(super) struct SaveAs {
    name: String,
    location: Option<PathBuf>,
    error: Option<String>,
    opening: bool,
}

impl Editor {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui, recent: &[RecentProject], actions: &mut Vec<Action>) {
        egui::Panel::top("editor_top").frame(egui::Frame::new().fill(theme::TITLE)).show_separator_line(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(6.0);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), TAB_HEIGHT), Layout::left_to_right(Align::Center), |ui| {
                ui.add_space(12.0);
                let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                crate::hub::logo(ui.painter(), rect);
                self.file_menu(ui, recent, actions);
                ui.menu_button("Options", |ui| {
                    let mut changed = ui.checkbox(&mut self.project.codegen.defmt, "Use defmt logging").changed();
                    changed |= ui.checkbox(&mut self.project.codegen.vscode, "Generate .vscode config").changed();
                    if changed {
                        self.mark_dirty();
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new(&self.project.name).strong().size(15.0).color(theme::TEXT))
                    .on_hover_text(format!("{} · {}", self.project.chip, self.project.package));
                if self.dirty {
                    ui.label(RichText::new("●").color(theme::WARN)).on_hover_text("Unsaved changes");
                }
                ui.add_space(18.0);
                self.tab_strip(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(12.0);
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
            // The selected tab runs into this strip, which sits on the panels below.
            let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 5.0), Sense::hover());
            ui.painter().rect_filled(strip, 0.0, theme::PANEL);
        });
        self.save_as_dialog(ui, actions);
    }

    fn file_menu(&mut self, ui: &mut egui::Ui, recent: &[RecentProject], actions: &mut Vec<Action>) {
        ui.menu_button("File", |ui| {
            ui.set_min_width(230.0);
            if ui.button("New Project…").on_hover_text("Save this project and open the new-project form").clicked() {
                self.save_quiet();
                actions.push(Action::NewProject);
            }
            if ui.button("Open Project…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Open Embassy Studio project").pick_folder() {
                    self.save_quiet();
                    actions.push(Action::OpenProject(dir));
                }
            }
            ui.menu_button("Recent Projects", |ui| {
                ui.set_min_width(260.0);
                let others: Vec<&RecentProject> = recent.iter().filter(|r| r.path != self.dir).take(RECENT_SHOWN).collect();
                if others.is_empty() {
                    ui.label(RichText::new("No other projects yet").color(theme::TEXT_DIM));
                }
                for r in others {
                    let exists = r.path.join(PROJECT_FILE).exists();
                    let mut text = egui::text::LayoutJob::default();
                    text.append(&r.name, 0.0, egui::TextFormat { color: theme::TEXT, ..Default::default() });
                    text.append(&r.chip, 14.0, egui::TextFormat { color: theme::TEXT_DIM, font_id: FontId::monospace(11.0), ..Default::default() });
                    let item = ui
                        .add_enabled(exists, egui::Button::new(text))
                        .on_hover_text(r.path.display().to_string())
                        .on_disabled_hover_text(format!("{} no longer holds a project", r.path.display()));
                    if item.clicked() {
                        self.save_quiet();
                        actions.push(Action::OpenProject(r.path.clone()));
                    }
                }
            });
            ui.separator();
            if ui.button("Save                     Ctrl+S").clicked() {
                self.save(actions);
            }
            if ui.button("Save As…             Ctrl+Shift+S").clicked() {
                self.open_save_as();
            }
            if ui.button("Generate Code    Ctrl+G").clicked() {
                self.generate(actions);
            }
            ui.separator();
            if ui.button("Show in Explorer").clicked() {
                crate::hub::open_in_explorer(&self.dir);
            }
            if ui.button("Open in VS Code").clicked() {
                let r = std::process::Command::new("cmd").args(["/C", "code", "."]).current_dir(&self.dir).spawn();
                if r.is_err() {
                    actions.push(Action::Notify("Could not start VS Code (`code` not on PATH)".into(), true));
                }
            }
            ui.separator();
            if ui.button("Close Project").clicked() {
                self.save_quiet();
                actions.push(Action::CloseProject);
            }
        });
    }

    /// View tabs shaped like browser tabs: the selected one is joined to the content below.
    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let font = FontId::proportional(13.5);
        let mut cells = Vec::new();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (tab, label) in TABS {
                let text = ui.painter().layout_no_wrap(label.to_string(), font.clone(), theme::TEXT).size().x;
                let (rect, resp) = ui.allocate_exact_size(vec2((text + 40.0).max(104.0), TAB_HEIGHT), Sense::click());
                let selected = self.tab == tab;
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, label));
                if resp.clicked() {
                    self.tab = tab;
                }
                cells.push((tab, label, rect, resp.hovered()));
            }
        });
        let painter = ui.painter();
        let active = cells.iter().position(|c| c.0 == self.tab);
        for (i, (_, label, rect, hovered)) in cells.iter().enumerate() {
            if Some(i) == active {
                continue;
            }
            if *hovered {
                painter.rect_filled(rect.shrink2(vec2(4.0, 4.0)), 8.0, theme::CARD);
            }
            painter.text(rect.center(), Align2::CENTER_CENTER, label, font.clone(), if *hovered { theme::TEXT } else { theme::TEXT_DIM });
            // A divider to the next tab, unless one of the two already stands out.
            let next_plain = cells.get(i + 1).is_some_and(|n| !n.3) && Some(i + 1) != active;
            if !hovered && next_plain {
                painter.vline(rect.right(), rect.center().y - 8.0..=rect.center().y + 8.0, Stroke::new(1.0, theme::BORDER));
            }
        }
        let Some((_, label, rect, _)) = active.map(|i| cells[i]) else { return };
        // The foot of the tab flares out into the strip: a filled square with a
        // quarter circle of the title colour cut out of it.
        let flare = 8.0;
        let foot = rect.bottom() - flare;
        for (square, centre) in [
            (Rect::from_min_max(pos2(rect.left() - flare, foot), rect.left_bottom()), pos2(rect.left() - flare, foot)),
            (Rect::from_min_max(pos2(rect.right(), foot), pos2(rect.right() + flare, rect.bottom())), pos2(rect.right() + flare, foot)),
        ] {
            painter.rect_filled(square, 0.0, theme::PANEL);
            painter.with_clip_rect(square).circle_filled(centre, flare, theme::TITLE);
        }
        painter.rect_filled(rect, CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 }, theme::PANEL);
        painter.text(rect.center(), Align2::CENTER_CENTER, label, font, theme::TEXT);
    }

    // ---- Save As ----

    pub(super) fn open_save_as(&mut self) {
        self.save_as = Some(SaveAs {
            name: format!("{}_copy", self.project.name),
            location: self.dir.parent().map(Path::to_path_buf),
            error: None,
            opening: true,
        });
    }

    fn save_as_dialog(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(mut dlg) = self.save_as.take() else { return };
        let name = dlg.name.trim().to_string();
        let valid = !name.is_empty() && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']);
        let target = dlg.location.as_ref().filter(|_| valid).map(|l| l.join(&name));
        let (mut save, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("save_project_as")).show(ui.ctx(), |ui| {
            ui.set_width(480.0);
            theme::heading(ui, "Save Project As");
            ui.add_space(4.0);
            ui.label(
                RichText::new("Copies the pin, clock and peripheral configuration and the schematic sheets into a new project folder and continues there. Code is generated fresh in the new folder; source files you wrote by hand are not copied.")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            ui.add_space(10.0);
            egui::Grid::new("save_as_form").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                ui.label(RichText::new("Project name").color(theme::TEXT_DIM));
                let r = ui.add(egui::TextEdit::singleline(&mut dlg.name).desired_width(340.0));
                if std::mem::take(&mut dlg.opening) {
                    r.request_focus();
                }
                save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.end_row();
                ui.label(RichText::new("Location").color(theme::TEXT_DIM));
                ui.horizontal(|ui| {
                    if ui.button("Browse…").clicked() {
                        let mut pick = rfd::FileDialog::new().set_title("Location of the new project");
                        if let Some(l) = &dlg.location {
                            pick = pick.set_directory(l);
                        }
                        if let Some(dir) = pick.pick_folder() {
                            dlg.location = Some(dir);
                            dlg.error = None;
                        }
                    }
                    let shown = dlg.location.as_ref().map(|l| l.display().to_string()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(shown).monospace().size(12.0).color(theme::TEXT)).truncate());
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            if let Some(t) = &target {
                ui.add(egui::Label::new(RichText::new(format!("→ {}", t.display())).size(12.0).color(theme::TEXT_DIM)).truncate());
            }
            if let Some(e) = &dlg.error {
                ui.label(RichText::new(e).color(theme::ERROR));
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                save |= ui.add_enabled(target.is_some(), theme::primary_button("Save As")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        if cancel || modal.should_close() {
            return;
        }
        if let (true, Some(dir)) = (save, target) {
            match self.save_copy(&dir, &name) {
                Ok(project) => {
                    actions.push(Action::CreateProject { dir, project });
                    return;
                }
                Err(e) => dlg.error = Some(e),
            }
        }
        self.save_as = Some(dlg);
    }

    /// Prepare `dir` as a copy of this project and return the project to store there.
    /// This project's own files are left as they are on disk.
    fn save_copy(&mut self, dir: &Path, name: &str) -> Result<crate::project::Project, String> {
        if dir.join(PROJECT_FILE).exists() {
            return Err("A project already exists there.".into());
        }
        if dir.is_dir() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
            return Err("That folder exists and is not empty.".into());
        }
        self.schematic_commit();
        let mut project = self.project.clone();
        project.name = name.to_string();
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        for sheet in &project.schematic.sheets {
            let to = dir.join(&sheet.file);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
            }
            std::fs::copy(self.dir.join(&sheet.file), &to).map_err(|e| format!("Could not copy {}: {e}", sheet.file))?;
        }
        Ok(project)
    }
}
