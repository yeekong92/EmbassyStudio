//! Headless walkthrough of the Schematic tab (egui_kittest, rendered with wgpu).

use std::path::PathBuf;
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;

use super::*;
use crate::app::Data;
use crate::project::Project;

struct State {
    data: Option<Data>,
    editor: Editor,
    notes: Vec<String>,
    /// "Save Project As" result: folder and project name.
    created: Option<(PathBuf, String)>,
}

fn settle(h: &mut Harness<'_, State>, frames: usize, pause_ms: u64) {
    for _ in 0..frames {
        h.step();
        std::thread::sleep(Duration::from_millis(pause_ms));
    }
}

fn button(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

fn right_click(h: &mut Harness<'_, State>, pos: Pos2) {
    let b = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
    h.hover_at(pos);
    h.event(b(true));
    h.event(b(false));
    h.step();
    h.step();
    h.step();
}

fn click(h: &mut Harness<'_, State>, pos: Pos2) {
    h.hover_at(pos);
    h.event(button(pos, true));
    h.event(button(pos, false));
    h.step();
    h.step();
}

/// Where a node is on screen, from the transform the canvas last used.
fn node_pos(h: &Harness<'_, State>, pin: &str) -> Pos2 {
    let e = &h.state().editor;
    let (origin, scale, size) = e.sch.view.expect("canvas not painted");
    let nodes = match &e.sch.draft {
        Some(d) => &d.nodes,
        None => &e.project.schematic.sheets[e.sch.sheet].nodes,
    };
    let n = nodes.iter().find(|n| n.pin == pin).unwrap_or_else(|| panic!("no node for {pin}"));
    origin + vec2(n.x * size.x, n.y * size.y) * scale
}

/// Walks through the Schematic tab on a real project and saves a screenshot of
/// every step. The project must already have a schematic imported:
///
/// ```text
/// embassy-studio --import-schematic <project> board.kicad_sch
/// SCH_UI_PROJECT=<project> SCH_OUT=shots cargo test --release schematic_tab_walkthrough -- --ignored --nocapture
/// ```
///
/// It changes the project: PA5 becomes a GPIO output and its node is moved, and
/// the sheet is closed at the end. A `<project>_copy` folder beside the project
/// is created and removed again.
#[test]
#[ignore = "needs SCH_UI_PROJECT (a project with an imported schematic) and SCH_OUT"]
fn schematic_tab_walkthrough() {
    let dir = PathBuf::from(std::env::var("SCH_UI_PROJECT").expect("set SCH_UI_PROJECT"));
    let out = PathBuf::from(std::env::var("SCH_OUT").expect("set SCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let project = Project::load(&dir).unwrap();
    assert!(!project.schematic.sheets.is_empty(), "import a schematic into the project first");
    let copy_name = format!("{}_copy", project.name);
    let copy_dir = dir.parent().unwrap().join(&copy_name);
    std::fs::remove_dir_all(&copy_dir).ok();
    let state = State { data: None, editor: Editor::new(dir.clone(), project, false), notes: Vec::new(), created: None };

    // The app gets symbol glyphs from system fonts through eframe; the harness has only egui's own.
    let builder = Harness::builder().with_size(vec2(1440.0, 900.0)).allow_missing_glyphs().with_accessibility_check(false);
    let mut h = builder.wgpu().build_ui_state(
        |ui, s: &mut State| {
            let ctx = ui.ctx().clone();
            let data = s.data.get_or_insert_with(|| {
                theme::install(&ctx);
                Data::new(ctx.clone(), crate::project::Settings::load().data_source)
            });
            data.poll();
            let mut actions = Vec::new();
            s.editor.ui(ui, data, &[], &mut actions);
            for a in actions {
                match a {
                    Action::Notify(text, _) => s.notes.push(text),
                    Action::CreateProject { dir, project } => s.created = Some((dir, project.name)),
                    _ => {}
                }
            }
        },
        state,
    );
    let shot = |h: &mut Harness<'_, State>, name: &str| {
        h.step();
        h.render().unwrap().save(out.join(name)).unwrap();
        println!("saved {name}");
    };

    // Chip data arrives from a background thread.
    for _ in 0..200 {
        settle(&mut h, 1, 50);
        if h.state().editor.chip.is_some() {
            break;
        }
    }
    assert!(h.state().editor.chip.is_some(), "chip data did not load: {:?}", h.state().editor.load_error);

    h.get_by_label("Schematic").click();
    settle(&mut h, 4, 20);
    for _ in 0..200 {
        settle(&mut h, 1, 50);
        if h.state().editor.sch.images.values().any(|i| i.ready()) {
            break;
        }
    }
    shot(&mut h, "1_view.png");

    // Zoom in on PA5: the node must stay under the cursor, glued to the drawing.
    let before = node_pos(&h, "PA5");
    h.hover_at(before);
    h.step();
    for _ in 0..3 {
        h.event(egui::Event::Zoom(2.0));
        h.step();
    }
    let after = node_pos(&h, "PA5");
    assert!(before.distance(after) < 0.5, "zoom moved the point under the cursor: {before:?} -> {after:?}");
    assert!((h.state().editor.sch.zoom - 8.0).abs() < 1e-3);
    // Let the sharp render for this view arrive.
    settle(&mut h, 30, 40);
    shot(&mut h, "2_zoomed.png");

    // Pan: background and nodes move together.
    let start = pos2(700.0, 600.0);
    let by = vec2(-60.0, 40.0);
    h.hover_at(start);
    h.event(button(start, true));
    h.hover_at(start + by);
    h.step();
    h.event(button(start + by, false));
    h.step();
    let panned = node_pos(&h, "PA5");
    assert!((panned - after - by).length() < 0.5, "pan: {after:?} -> {panned:?}");
    assert!(h.state().editor.sch.mode == Mode::View);
    let stored = h.state().editor.project.schematic.sheets[0].nodes.clone();

    // View mode: a click opens the pin's function list, and choosing a function
    // goes straight on to the label prompt. Nodes cannot be dragged.
    click(&mut h, panned);
    settle(&mut h, 3, 20);
    shot(&mut h, "3_functions.png");
    h.get_by_label("GPIO_Output").click();
    settle(&mut h, 4, 20);
    assert_eq!(h.state().editor.project.pins.get("PA5").map(|c| c.mode.clone()), Some(PinMode::GpioOutput));
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, stored);
    shot(&mut h, "4_label_prompt.png");
    // The label box has the keyboard focus.
    h.event(egui::Event::Text("LED_GREEN".into()));
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h, 4, 20);
    assert_eq!(h.state().editor.project.pins.get("PA5").map(|c| c.label.as_str()), Some("LED_GREEN"));
    assert!(h.state().editor.pin_popup.is_none());

    // The pin list: picking a row selects the pin and brings its node into view.
    // A pin whose row is on screen but whose node is below the visible part of the sheet.
    let row = ["PB6", "PB5", "PB4"]
        .into_iter()
        .find(|p| h.query_by_label(&format!("Pin {p}")).is_some_and(|n| n.rect().bottom() < 840.0))
        .expect("no pin row is visible in the list");
    assert!(node_pos(&h, row).y > 860.0, "the test needs a node outside the view");
    println!("picking {row} in the pin list; its node is at {:?}", node_pos(&h, row));
    h.get_by_label(&format!("Pin {row}")).click();
    settle(&mut h, 3, 20);
    assert_eq!(h.state().editor.sch.selected.as_deref(), Some(row));
    let shown = node_pos(&h, row);
    assert!((300.0..1050.0).contains(&shown.x) && (90.0..860.0).contains(&shown.y), "{row} is not in view: {shown:?}");
    h.remove_cursor();
    shot(&mut h, "5_list_pick.png");
    settle(&mut h, 40, 40);

    // Right-click: the canvas menu.
    right_click(&mut h, pos2(520.0, 700.0));
    shot(&mut h, "6_view_menu.png");
    h.get_by_label("Fit to window").click();
    settle(&mut h, 20, 40);
    assert!((h.state().editor.sch.zoom - 1.0).abs() < 1e-3);
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, stored);

    // Edit mode: drag the node, then Save writes relative coordinates to the project file.
    h.get_by_label_contains("Edit positions").click();
    settle(&mut h, 4, 20);
    assert!(h.state().editor.sch.mode == Mode::Edit);
    let grab = node_pos(&h, "PA5");
    let (_, scale, size) = h.state().editor.sch.view.unwrap();
    let old = stored.iter().find(|n| n.pin == "PA5").unwrap().clone();
    let delta = vec2(45.0, -30.0);
    h.hover_at(grab);
    h.event(button(grab, true));
    for i in 1..=5 {
        h.hover_at(grab + delta * (i as f32 / 5.0));
    }
    h.step();
    shot(&mut h, "7_dragging.png");
    h.event(button(grab + delta, false));
    h.step();
    let moved = h.state().editor.sch.draft.as_ref().unwrap().nodes.iter().find(|n| n.pin == "PA5").unwrap().clone();
    assert!((moved.x - old.x - delta.x / (size.x * scale)).abs() < 2e-4, "{old:?} -> {moved:?}");
    assert!((moved.y - old.y - delta.y / (size.y * scale)).abs() < 2e-4, "{old:?} -> {moved:?}");
    // Not in the project until saved.
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, stored);

    // Ctrl+Z takes the drag back, Ctrl+Y does it again.
    let at = |h: &Harness<'_, State>, pin: &str| {
        let n = h.state().editor.sch.draft.as_ref().unwrap().nodes.iter().find(|n| n.pin == pin).cloned().unwrap();
        (n.x, n.y)
    };
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert_eq!(at(&h, "PA5"), (old.x, old.y), "undo");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Y);
    h.step();
    assert_eq!(at(&h, "PA5"), (moved.x, moved.y), "redo");

    // R turns the label of the node under the pointer, Del removes the node.
    let draft = |h: &Harness<'_, State>, pin: &str| h.state().editor.sch.draft.as_ref().unwrap().nodes.iter().find(|n| n.pin == pin).cloned();
    let turned = draft(&h, "PA7").unwrap().label.rotated();
    h.hover_at(node_pos(&h, "PA7"));
    h.step();
    h.key_press(egui::Key::R);
    h.step();
    assert!(draft(&h, "PA7").unwrap().label == turned);
    let pa6 = node_pos(&h, "PA6");
    h.hover_at(pa6);
    h.step();
    h.key_press(egui::Key::Delete);
    h.step();
    assert!(draft(&h, "PA6").is_none());
    // ... and Ctrl+Z brings it back, once.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert!(draft(&h, "PA6").is_some());
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.step();
    assert!(draft(&h, "PA6").is_none());

    // The removed pin is offered for placing: pick it, then click on the schematic.
    h.get_by_label("PA6").click();
    settle(&mut h, 3, 20);
    assert_eq!(h.state().editor.sch.placing.as_deref(), Some("PA6"));
    let target = pa6 + vec2(-120.0, 10.0);
    click(&mut h, target);
    settle(&mut h, 2, 20);
    assert!(h.state().editor.sch.placing.is_none());
    assert!(node_pos(&h, "PA6").distance(target) < 0.5);

    // Snap grid from the right-click menu: a dragged node lands on a grid point.
    h.hover_at(node_pos(&h, "PA5"));
    settle(&mut h, 2, 20);
    for _ in 0..3 {
        h.event(egui::Event::Zoom(2.0));
        h.step();
    }
    let menu_at = node_pos(&h, "PA5") + vec2(90.0, 60.0);
    right_click(&mut h, menu_at);
    shot(&mut h, "8_edit_menu.png");
    h.get_by_label_contains("Snap grid").click();
    settle(&mut h, 4, 30);
    shot(&mut h, "9_grid_menu.png");
    h.get_by_label_contains("Half pin pitch").click();
    settle(&mut h, 4, 30);
    let grid = h.state().editor.sch.draft.as_ref().unwrap().grid;
    assert!(grid > 0.0, "no grid was set");
    let (_, scale, size) = h.state().editor.sch.view.unwrap();
    let from = node_pos(&h, "PA7");
    let to = from + vec2(37.0, 23.0);
    h.hover_at(from);
    h.event(button(from, true));
    h.hover_at(from + vec2(20.0, 10.0));
    h.hover_at(to);
    h.step();
    h.event(button(to, false));
    h.step();
    let (x, y) = at(&h, "PA7");
    let off = |v: f32, extent: f32| ((v * extent / grid) - (v * extent / grid).round()).abs();
    assert!(off(x, size.x) < 2e-3 && off(y, size.y) < 2e-3, "PA7 is off the {grid} grid: {x} {y}");
    assert!(node_pos(&h, "PA7").distance(to) <= grid * scale, "PA7 did not follow the pointer");
    h.remove_cursor();
    settle(&mut h, 30, 40);
    shot(&mut h, "10_grid.png");

    let save = h.get_all_by_label("Save").max_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).unwrap();
    save.click();
    settle(&mut h, 4, 20);
    assert!(h.state().editor.sch.mode == Mode::View);
    let on_disk = Project::load(&dir).unwrap();
    let saved = on_disk.schematic.sheets[0].nodes.iter().find(|n| n.pin == "PA5").unwrap();
    assert!((saved.x - moved.x).abs() < 1e-5 && (saved.y - moved.y).abs() < 1e-5, "{saved:?} vs {moved:?}");
    assert!((0.0..=1.0).contains(&saved.x) && (0.0..=1.0).contains(&saved.y));
    assert!((on_disk.schematic.sheets[0].grid - grid).abs() < 1e-6, "the grid is stored with the sheet");

    h.get_by_label("Fit").click();
    settle(&mut h, 20, 40);
    h.remove_cursor();
    shot(&mut h, "11_saved.png");

    // File menu and Save As: the copy gets the configuration and the drawings.
    h.get_by_label("File").click();
    settle(&mut h, 4, 30);
    shot(&mut h, "12_file_menu.png");
    h.get_by_label_contains("Save As").click();
    settle(&mut h, 4, 30);
    shot(&mut h, "13_save_as.png");
    h.get_by_label("Save As").click();
    settle(&mut h, 4, 30);
    let (made, name) = h.state().created.clone().expect("Save As did not hand over a project");
    assert_eq!((made.as_path(), name.as_str()), (copy_dir.as_path(), copy_name.as_str()));
    assert!(copy_dir.join(&on_disk.schematic.sheets[0].file).is_file(), "the drawing was not copied");
    std::fs::remove_dir_all(&copy_dir).ok();

    // Refresh reads the source file again and keeps the calibrated nodes.
    let kept = h.state().editor.project.schematic.sheets[0].nodes.clone();
    h.get_by_label("Refresh").click();
    for _ in 0..400 {
        settle(&mut h, 1, 50);
        if h.state().editor.sch.import.is_none() {
            break;
        }
    }
    assert!(h.state().notes.iter().any(|n| n.starts_with("Refreshed")), "{:?}", h.state().notes);
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, kept);
    for _ in 0..200 {
        settle(&mut h, 1, 50);
        if h.state().editor.sch.images.values().any(|i| i.ready()) {
            break;
        }
    }

    // The other tabs, for the look of the tab strip.
    h.get_by_label("Pinout").click();
    settle(&mut h, 6, 30);
    h.remove_cursor();
    shot(&mut h, "14_pinout_tab.png");
    h.get_by_label("Schematic").click();
    settle(&mut h, 6, 30);

    // Close the schematic: asked first, then the sheet is gone and another can be imported.
    h.get_by_label("Close").click();
    settle(&mut h, 4, 30);
    shot(&mut h, "15_close_prompt.png");
    h.get_by_label("Close schematic").click();
    settle(&mut h, 4, 30);
    assert!(h.state().editor.project.schematic.sheets.is_empty());
    assert_eq!(h.state().editor.project.pins.get("PA5").map(|c| c.label.as_str()), Some("LED_GREEN"), "closing keeps the pin setup");
    h.remove_cursor();
    shot(&mut h, "16_closed.png");
    println!("notifications: {:?}", h.state().notes);
}
