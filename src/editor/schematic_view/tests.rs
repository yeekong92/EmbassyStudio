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
/// It changes the project: PA5 becomes a GPIO output and its node is moved.
#[test]
#[ignore = "needs SCH_UI_PROJECT (a project with an imported schematic) and SCH_OUT"]
fn schematic_tab_walkthrough() {
    let dir = PathBuf::from(std::env::var("SCH_UI_PROJECT").expect("set SCH_UI_PROJECT"));
    let out = PathBuf::from(std::env::var("SCH_OUT").expect("set SCH_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let project = Project::load(&dir).unwrap();
    assert!(!project.schematic.sheets.is_empty(), "import a schematic into the project first");
    let state = State { data: None, editor: Editor::new(dir.clone(), project, false), notes: Vec::new() };

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
            s.editor.ui(ui, data, &mut actions);
            for a in actions {
                if let Action::Notify(text, _) = a {
                    s.notes.push(text);
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

    // View mode: a click opens the pin's function list; nodes cannot be dragged.
    click(&mut h, panned);
    settle(&mut h, 3, 20);
    shot(&mut h, "3_functions.png");
    h.get_by_label("GPIO_Output").click();
    settle(&mut h, 4, 20);
    assert_eq!(h.state().editor.project.pins.get("PA5").map(|c| c.mode.clone()), Some(PinMode::GpioOutput));
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, stored);
    h.remove_cursor();
    shot(&mut h, "4_assigned.png");

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
    shot(&mut h, "5_dragging.png");
    h.event(button(grab + delta, false));
    h.step();
    let moved = h.state().editor.sch.draft.as_ref().unwrap().nodes.iter().find(|n| n.pin == "PA5").unwrap().clone();
    assert!((moved.x - old.x - delta.x / (size.x * scale)).abs() < 2e-4, "{old:?} -> {moved:?}");
    assert!((moved.y - old.y - delta.y / (size.y * scale)).abs() < 2e-4, "{old:?} -> {moved:?}");
    // Not in the project until saved.
    assert_eq!(h.state().editor.project.schematic.sheets[0].nodes, stored);

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
    shot(&mut h, "6_removed.png");

    // The removed pin is offered for placing: pick it, then click on the schematic.
    h.get_by_label("PA6").click();
    settle(&mut h, 3, 20);
    assert_eq!(h.state().editor.sch.placing.as_deref(), Some("PA6"));
    let target = pa6 + vec2(-120.0, 10.0);
    click(&mut h, target);
    settle(&mut h, 2, 20);
    assert!(h.state().editor.sch.placing.is_none());
    assert!(node_pos(&h, "PA6").distance(target) < 0.5);
    h.remove_cursor();
    shot(&mut h, "7_placed.png");

    let save = h.get_all_by_label("Save").max_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).unwrap();
    save.click();
    settle(&mut h, 4, 20);
    assert!(h.state().editor.sch.mode == Mode::View);
    let on_disk = Project::load(&dir).unwrap();
    let saved = on_disk.schematic.sheets[0].nodes.iter().find(|n| n.pin == "PA5").unwrap();
    assert!((saved.x - moved.x).abs() < 1e-5 && (saved.y - moved.y).abs() < 1e-5, "{saved:?} vs {moved:?}");
    assert!((0.0..=1.0).contains(&saved.x) && (0.0..=1.0).contains(&saved.y));
    println!("notifications: {:?}", h.state().notes);

    h.get_by_label("Fit").click();
    settle(&mut h, 20, 40);
    h.remove_cursor();
    shot(&mut h, "8_saved.png");
}
