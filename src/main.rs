#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod clock;
mod codegen;
mod db;
mod editor;
mod hub;
mod project;
mod schematic;
mod theme;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--generate") {
        // Release builds use the GUI subsystem; reuse the calling terminal for output.
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
        let Some(dir) = args.get(i + 1) else {
            eprintln!("usage: embassy-studio --generate <project-dir>");
            std::process::exit(2);
        };
        match generate_headless(std::path::Path::new(dir)) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }

    if let Some(i) = args.iter().position(|a| a == "--import-schematic") {
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
        let (Some(dir), Some(file)) = (args.get(i + 1), args.get(i + 2)) else {
            eprintln!("usage: embassy-studio --import-schematic <project-dir> <schematic.svg|pdf|kicad_sch|SchDoc>");
            std::process::exit(2);
        };
        match import_schematic_headless(std::path::Path::new(dir), std::path::Path::new(file)) {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("error: {e:#}");
                std::process::exit(1);
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Embassy Studio")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    // `embassy-studio <project-dir>` opens that project directly (e.g. from Explorer).
    let open = args
        .iter()
        .skip(1)
        .map(std::path::PathBuf::from)
        .find(|p| p.join(project::PROJECT_FILE).is_file());
    eframe::run_native(
        "Embassy Studio",
        options,
        Box::new(|cc| {
            theme::install(&cc.egui_ctx);
            Ok(Box::new(app::App::new(cc.egui_ctx.clone(), open)))
        }),
    )
}

/// Import a schematic into a project and auto-locate the MCU pins, without the GUI.
fn import_schematic_headless(dir: &std::path::Path, file: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let settings = project::Settings::load();
    let mut project = project::Project::load(dir)?;
    let chip = db::Chip::load(&settings.data_source, &project.chip, false)?;
    let pkg = chip.package(&project.package).or(chip.packages.first()).context("the chip has no package data")?;
    let pins = schematic::PinNames::from_package(pkg);
    let drawings = schematic::import(file, &pins)?;
    // Absolute, so the sheet can be refreshed from the GUI later.
    let source = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let done = schematic::add_to_project(&mut project.schematic, dir, &source, drawings, &pins)?;
    for e in &done.errors {
        eprintln!("could not write {e}");
    }
    project.save(dir)?;
    for f in &done.files {
        let nodes = project.schematic.sheets.iter().find(|s| &s.file == f).map(|s| s.nodes.len()).unwrap_or(0);
        println!("{f}: {nodes} nodes");
    }
    println!("{} ({}): located {} of {} I/O pins", project.chip, pkg.name, done.gpios.len(), pins.gpios.len());
    Ok(())
}

/// Regenerate a project's code without the GUI (for scripts and CI).
fn generate_headless(dir: &std::path::Path) -> anyhow::Result<()> {
    let settings = project::Settings::load();
    let mut project = project::Project::load(dir)?;
    let chip = db::Chip::load(&settings.data_source, &project.chip, false)?;
    let tree = match chip.rcc_registers() {
        Some(r) => {
            let rcc = db::RegisterFile::load(&settings.data_source, &r.kind, &r.version, &r.block, false)?;
            let tree = clock::ClockTree::from_rcc(&rcc, &r.version);
            project.clock.normalize(&tree);
            Some(tree)
        }
        None => None,
    };
    if let Some(t) = &tree {
        let r = project.clock.compute(t);
        println!(
            "{}: SYSCLK {}, HCLK {} (rcc {})",
            project.chip,
            clock::format_hz(r.sysclk),
            clock::format_hz(r.hclk),
            t.rcc_version
        );
    }
    let files = codegen::generate(&project, &chip, tree.as_ref());
    let written = codegen::write(dir, &files)?;
    println!("wrote {} file(s): {}", written.len(), written.join(", "));
    Ok(())
}
