//! Headless walkthrough of an ESP32 project (egui_kittest, rendered with wgpu).

use std::path::PathBuf;
use std::time::Duration;

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;

use super::*;
use crate::app::Data;

struct State {
    data: Option<Data>,
    editor: Editor,
}

/// Opens an ESP32 project and saves a screenshot of the Pinout (with a pin and a
/// peripheral selected), Clock Configuration and Code Preview tabs. Nothing is saved
/// to the project.
///
/// ```text
/// ESP_UI_PROJECT=<project> ESP_OUT=shots cargo test esp_project_walkthrough -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs ESP_UI_PROJECT (an ESP32 project) and ESP_OUT, and network access or cached esp-hal metadata"]
fn esp_project_walkthrough() {
    let dir = PathBuf::from(std::env::var("ESP_UI_PROJECT").expect("set ESP_UI_PROJECT"));
    let out = PathBuf::from(std::env::var("ESP_OUT").expect("set ESP_OUT"));
    std::fs::create_dir_all(&out).unwrap();
    let project = Project::load(&dir).unwrap();
    let first_pin = project.pins.keys().next().cloned();
    let first_periph = project.peripherals.iter().find(|(_, p)| p.enabled).map(|(n, _)| n.clone());
    let state = State { data: None, editor: Editor::new(dir, project, false) };

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
        },
        state,
    );
    let shot = |h: &mut Harness<'_, State>, name: &str| {
        h.step();
        h.step();
        h.render().unwrap().save(out.join(name)).unwrap();
        println!("saved {name}");
    };

    for _ in 0..400 {
        h.step();
        std::thread::sleep(Duration::from_millis(50));
        if h.state().editor.chip.is_some() && h.state().editor.tree.is_some() {
            break;
        }
    }
    let chip = h.state().editor.chip.clone().unwrap_or_else(|| panic!("chip did not load: {:?}", h.state().editor.load_error));
    assert_eq!(chip.platform, Platform::Esp);

    // Pinout with the first configured pin and the first enabled peripheral selected.
    if let Some(pin) = &first_pin {
        let pos = chip.packages[0].pins.iter().find(|p| p.signals[0] == *pin).map(|p| p.position.clone());
        h.state_mut().editor.selected_pin = pos;
    }
    h.state_mut().editor.selected_periph = first_periph;
    shot(&mut h, "1_pinout.png");

    h.get_by_label("Clock Configuration").click();
    shot(&mut h, "2_clock.png");

    h.get_by_label("Code Preview").click();
    shot(&mut h, "3_code.png");
    let board = h.state().editor.preview.iter().find(|f| f.path == "src/board.rs").expect("board.rs").contents.clone();
    assert!(board.contains("esp_rtos::start"), "board.rs is not esp-hal code:\n{board}");
}
