//! Application shell: screen switching, background data loading, notifications.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use crate::db::{Chip, ChipIndex, DataSource, RegRef, RegisterFile};
use crate::editor::Editor;
use crate::hub::Hub;
use crate::project::{Project, Settings};
use crate::theme;

#[derive(Clone)]
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

enum Msg {
    Index(Result<ChipIndex, String>),
    Chip(String, Result<Arc<Chip>, String>),
    Registers(String, Result<Arc<RegisterFile>, String>),
}

/// Shared access to the stm32-data database, loading in background threads.
pub struct Data {
    pub source: DataSource,
    pub index: Option<ChipIndex>,
    pub index_loading: bool,
    pub index_error: Option<String>,
    chips: HashMap<String, Load<Arc<Chip>>>,
    registers: HashMap<String, Load<Arc<RegisterFile>>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
}

impl Data {
    pub(crate) fn new(ctx: egui::Context, source: DataSource) -> Self {
        let (tx, rx) = channel();
        let mut data = Self {
            index: if source == DataSource::GitHub { ChipIndex::load_cached() } else { None },
            source,
            index_loading: false,
            index_error: None,
            chips: HashMap::new(),
            registers: HashMap::new(),
            tx,
            rx,
            ctx,
        };
        if data.index.is_none() {
            data.sync_index();
        }
        data
    }

    fn spawn(&self, f: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
    }

    /// Re-read the chip list from the data source (and drop cached chip data).
    pub fn sync_index(&mut self) {
        if self.index_loading {
            return;
        }
        self.index_loading = true;
        self.index_error = None;
        self.chips.clear();
        self.registers.clear();
        let source = self.source.clone();
        self.spawn(move || Msg::Index(ChipIndex::fetch(&source).map_err(|e| format!("{e:#}"))));
    }

    pub fn set_source(&mut self, source: DataSource) {
        self.source = source;
        self.index = None;
        self.index_loading = false;
        self.sync_index();
    }

    pub fn chip(&mut self, name: &str) -> Load<Arc<Chip>> {
        if let Some(l) = self.chips.get(name) {
            return l.clone();
        }
        self.chips.insert(name.to_string(), Load::Loading);
        let source = self.source.clone();
        let name = name.to_string();
        self.spawn(move || {
            // A refresh of the index means the user asked for fresh data.
            let res = Chip::load(&source, &name, false).map(Arc::new).map_err(|e| format!("{e:#}"));
            Msg::Chip(name, res)
        });
        Load::Loading
    }

    pub fn registers(&mut self, r: &RegRef) -> Load<Arc<RegisterFile>> {
        let key = format!("{}_{}/{}", r.kind, r.version, r.block);
        if let Some(l) = self.registers.get(&key) {
            return l.clone();
        }
        self.registers.insert(key.clone(), Load::Loading);
        let source = self.source.clone();
        let r = r.clone();
        self.spawn(move || {
            let res = RegisterFile::load(&source, &r.kind, &r.version, &r.block, false)
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            Msg::Registers(key, res)
        });
        Load::Loading
    }

    pub(crate) fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Index(Ok(index)) => {
                    self.index = Some(index);
                    self.index_loading = false;
                }
                Msg::Index(Err(e)) => {
                    self.index_error = Some(e);
                    self.index_loading = false;
                }
                Msg::Chip(name, res) => {
                    self.chips.insert(name, res.map_or_else(Load::Failed, Load::Ready));
                }
                Msg::Registers(key, res) => {
                    self.registers.insert(key, res.map_or_else(Load::Failed, Load::Ready));
                }
            }
        }
    }
}

/// Requests from screens to the application shell.
pub enum Action {
    OpenProject(PathBuf),
    CreateProject { dir: PathBuf, project: Project },
    CloseProject,
    Notify(String, bool),
}

enum Screen {
    Hub(Hub),
    Editor(Box<Editor>),
}

struct Toast {
    text: String,
    error: bool,
    until: f64,
}

pub struct App {
    data: Data,
    settings: Settings,
    screen: Screen,
    toasts: Vec<Toast>,
}

impl App {
    pub fn new(ctx: egui::Context, open: Option<PathBuf>) -> Self {
        let settings = Settings::load();
        let mut app = Self {
            data: Data::new(ctx.clone(), settings.data_source.clone()),
            settings,
            screen: Screen::Hub(Hub::default()),
            toasts: Vec::new(),
        };
        if let Some(dir) = open {
            app.open(&ctx, dir, false);
        }
        app
    }

    fn notify(&mut self, ctx: &egui::Context, text: impl Into<String>, error: bool) {
        let now = ctx.input(|i| i.time);
        self.toasts.push(Toast {
            text: text.into(),
            error,
            until: now + if error { 8.0 } else { 4.0 },
        });
    }

    fn open(&mut self, ctx: &egui::Context, dir: PathBuf, generate: bool) {
        match Project::load(&dir) {
            Ok(project) => {
                self.settings.touch_recent(&project, &dir);
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                    "{} — {} — Embassy Studio",
                    project.name, project.chip
                )));
                self.screen = Screen::Editor(Box::new(Editor::new(dir, project, generate)));
            }
            Err(e) => self.notify(ctx, format!("Could not open project: {e:#}"), true),
        }
    }

    fn handle(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::OpenProject(dir) => self.open(ctx, dir, false),
            Action::CreateProject { dir, project } => match project.save(&dir) {
                Ok(()) => {
                    self.settings.last_location = dir.parent().map(|p| p.to_path_buf());
                    self.open(ctx, dir, true);
                }
                Err(e) => self.notify(ctx, format!("Could not create project: {e:#}"), true),
            },
            Action::CloseProject => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title("Embassy Studio".into()));
                self.screen = Screen::Hub(Hub::default());
            }
            Action::Notify(text, error) => self.notify(ctx, text, error),
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -36.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    egui::Frame::new()
                        .fill(theme::CARD)
                        .stroke(egui::Stroke::new(1.0, if t.error { theme::ERROR } else { theme::ACCENT }))
                        .corner_radius(egui::CornerRadius::same(6))
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(egui::RichText::new(&t.text).color(theme::TEXT));
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.data.poll();
        let mut actions = Vec::new();
        match &mut self.screen {
            Screen::Hub(hub) => {
                let before = self.data.source.clone();
                hub.ui(ui, &mut self.data, &mut self.settings, &mut actions);
                if self.data.source != before {
                    self.settings.data_source = self.data.source.clone();
                    self.settings.save();
                }
            }
            Screen::Editor(editor) => editor.ui(ui, &mut self.data, &mut actions),
        }
        for a in actions {
            self.handle(&ctx, a);
        }
        self.show_toasts(&ctx);
    }

    fn on_exit(&mut self) {
        if let Screen::Editor(editor) = &mut self.screen {
            editor.save_quiet();
        }
        self.settings.save();
    }
}
