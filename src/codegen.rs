//! Code generation: turns a project into a ready-to-build Embassy firmware crate.
//!
//! Generated layout:
//! - `Cargo.toml`, `build.rs`, `.cargo/config.toml`, `rust-toolchain.toml` — build setup
//!   (Cargo.toml keeps everything below the user-section marker on regeneration)
//! - `src/board.rs` — clocks, pins and peripheral drivers; always regenerated
//! - `src/main.rs`, `README.md` — created once, then owned by the user
//! - `.vscode/` — rust-analyzer, probe-rs debugging and build tasks

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::clock::{ClockTree, Role, format_hz};
use crate::db::{Chip, KernelClock};
use crate::project::{PinMode, Project, Pull, Speed, crate_name, ident};

pub const USER_SECTION_MARKER: &str = "# ---- user section: everything below this line is kept on regeneration ----";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Always rewritten.
    Overwrite,
    /// Written only if it does not exist yet.
    CreateOnly,
    /// Rewritten, but the text after [`USER_SECTION_MARKER`] is preserved.
    KeepUserSection,
}

#[derive(Clone, Debug)]
pub struct GenFile {
    pub path: String,
    pub contents: String,
    pub policy: Policy,
}

/// Rust target triple for a Cortex-M core name from stm32-data (`cm4`, `cm0p`, ...).
pub fn target_triple(core: &str) -> &'static str {
    match core {
        "cm0" | "cm0p" => "thumbv6m-none-eabi",
        "cm3" => "thumbv7m-none-eabi",
        "cm23" => "thumbv8m.base-none-eabi",
        "cm33" | "cm35p" | "cm55" => "thumbv8m.main-none-eabihf",
        _ => "thumbv7em-none-eabihf",
    }
}

/// `embassy-stm32` feature selecting the chip (multi-core parts need the core suffix).
pub fn embassy_feature(chip: &Chip) -> String {
    let base = chip.name.to_ascii_lowercase();
    if chip.cores.len() > 1 {
        format!("{base}-{}", chip.core_name())
    } else {
        base
    }
}

/// How well RCC code generation is supported for this RCC register version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RccFlavor {
    /// F2/F4/F7: `pll_src` + `pll: Pll { prediv, mul, divp, divq, divr }`.
    F247,
    /// G4: `pll: Pll { source, prediv, mul, divp, divq, divr }`.
    G4,
    /// Not mapped yet: the clock tree is emitted as documentation, defaults are used.
    SummaryOnly,
}

pub fn rcc_flavor(tree: &ClockTree) -> RccFlavor {
    let v = tree.rcc_version.as_str();
    if v.starts_with("f2") || v.starts_with("f4") || v.starts_with("f7") {
        RccFlavor::F247
    } else if v.starts_with("g4") {
        RccFlavor::G4
    } else {
        RccFlavor::SummaryOnly
    }
}

/// `PLLSRC` -> `Pllsrc`, `I2SSRC_CFGR` -> `I2ssrcCfgr` (stm32-metapac type naming).
fn pac_type_name(enum_name: &str) -> String {
    enum_name
        .split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let lower = s.to_ascii_lowercase();
            let mut c = lower.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Enum variant as named in stm32-metapac (`Div4` -> `DIV4`).
fn pac_variant(v: &str) -> String {
    v.to_ascii_uppercase()
}

pub fn generate(project: &Project, chip: &Chip, tree: Option<&ClockTree>) -> Vec<GenFile> {
    let triple = target_triple(chip.core_name());
    let pkg = crate_name(&project.name);
    let mut files = vec![
        GenFile {
            path: "Cargo.toml".into(),
            contents: cargo_toml(project, chip, &pkg),
            policy: Policy::KeepUserSection,
        },
        GenFile {
            path: "build.rs".into(),
            contents: build_rs(project.codegen.defmt),
            policy: Policy::Overwrite,
        },
        GenFile {
            path: ".cargo/config.toml".into(),
            contents: cargo_config(project, triple),
            policy: Policy::Overwrite,
        },
        GenFile {
            path: "rust-toolchain.toml".into(),
            contents: format!(
                "[toolchain]\nchannel = \"stable\"\ncomponents = [\"rustfmt\", \"llvm-tools\"]\ntargets = [\"{triple}\"]\n"
            ),
            policy: Policy::Overwrite,
        },
        GenFile {
            path: "src/board.rs".into(),
            contents: board_rs(project, chip, tree),
            policy: Policy::Overwrite,
        },
        GenFile {
            path: "src/main.rs".into(),
            contents: main_rs(project),
            policy: Policy::CreateOnly,
        },
        GenFile {
            path: ".gitignore".into(),
            contents: "/target\n".into(),
            policy: Policy::CreateOnly,
        },
        GenFile {
            path: "README.md".into(),
            contents: readme(project, chip, triple),
            policy: Policy::CreateOnly,
        },
    ];
    if project.codegen.vscode {
        files.extend(vscode_files(project, chip, triple, &pkg));
    }
    files
}

/// Write generated files into the project directory. Returns the paths written.
pub fn write(dir: &Path, files: &[GenFile]) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for f in files {
        let path = dir.join(&f.path);
        let existing = std::fs::read_to_string(&path).ok();
        let contents = match (f.policy, &existing) {
            (Policy::CreateOnly, Some(_)) => continue,
            (Policy::KeepUserSection, Some(old)) => match old.split_once(USER_SECTION_MARKER) {
                Some((_, tail)) => {
                    let head = f.contents.split_once(USER_SECTION_MARKER).map_or(f.contents.as_str(), |(h, _)| h);
                    format!("{head}{USER_SECTION_MARKER}{tail}")
                }
                None => f.contents.clone(),
            },
            _ => f.contents.clone(),
        };
        if existing.as_deref() == Some(contents.as_str()) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        written.push(f.path.clone());
    }
    Ok(written)
}

fn cargo_toml(project: &Project, chip: &Chip, pkg: &str) -> String {
    let defmt = project.codegen.defmt;
    let f = |s: &str| if defmt { format!(", {s}") } else { String::new() };
    let mut out = format!(
        r#"# Generated by Embassy Studio for {chip} ({package}).
# Re-generating rewrites this file except for the user section at the bottom.

[package]
name = "{pkg}"
version = "0.1.0"
edition = "2024"

[dependencies]
embassy-stm32 = {{ version = "0.6", features = ["{feature}", "time-driver-any", "memory-x", "exti", "unstable-pac"{defmt_stm32}] }}
embassy-executor = {{ version = "0.10", features = ["platform-cortex-m", "executor-thread"{defmt_f}] }}
embassy-time = {{ version = "0.5", features = ["tick-hz-32_768"{defmt_f}] }}
cortex-m = {{ version = "0.7", features = ["inline-asm", "critical-section-single-core"] }}
cortex-m-rt = "0.7"
"#,
        chip = chip.name,
        package = project.package,
        feature = embassy_feature(chip),
        defmt_stm32 = f("\"defmt\""),
        defmt_f = f("\"defmt\""),
    );
    if defmt {
        out.push_str("defmt = \"1\"\ndefmt-rtt = \"1\"\npanic-probe = { version = \"1\", features = [\"print-defmt\"] }\n");
    } else {
        out.push_str("panic-halt = \"1\"\n");
    }
    out.push_str(
        r#"
[profile.dev]
opt-level = "s"

[profile.release]
debug = 2
lto = true
opt-level = "s"
codegen-units = 1

"#,
    );
    out.push_str(USER_SECTION_MARKER);
    out.push('\n');
    out
}

fn build_rs(defmt: bool) -> String {
    let defmt = if defmt { "    println!(\"cargo:rustc-link-arg-bins=-Tdefmt.x\");\n" } else { "" };
    format!(
        "// Generated by Embassy Studio.\nfn main() {{\n    println!(\"cargo:rustc-link-arg-bins=--nmagic\");\n    println!(\"cargo:rustc-link-arg-bins=-Tlink.x\");\n{defmt}}}\n"
    )
}

fn cargo_config(project: &Project, triple: &str) -> String {
    let log = if project.codegen.defmt { "\n[env]\nDEFMT_LOG = \"debug\"\n" } else { "" };
    format!(
        r#"# Generated by Embassy Studio.
[target.'cfg(all(target_arch = "arm", target_os = "none"))']
runner = "probe-rs run --chip {chip}"

[build]
target = "{triple}"
{log}"#,
        chip = project.package,
    )
}

fn main_rs(project: &Project) -> String {
    let (uses, log_start, log_tick) = if project.codegen.defmt {
        (
            "use defmt::info;\nuse {defmt_rtt as _, panic_probe as _};\n",
            "    info!(\"{name} started\");\n",
            "        info!(\"tick\");\n",
        )
    } else {
        ("use panic_halt as _;\n", "", "")
    };
    let log_start = log_start.replace("{name}", &project.name.replace('"', "'"));
    let blink = project
        .pins
        .iter()
        .find(|(_, p)| p.mode == PinMode::GpioOutput)
        .map(|(pin, p)| field_name(pin, &p.label));
    let body = match blink {
        Some(field) => format!(
            "    loop {{\n        board.{field}.toggle();\n{log_tick}        Timer::after_millis(500).await;\n    }}\n"
        ),
        None => format!("    loop {{\n{log_tick}        Timer::after_millis(1000).await;\n    }}\n"),
    };
    format!(
        r#"#![no_std]
#![no_main]

// This file was created by Embassy Studio and is yours to edit;
// it is not overwritten when code is regenerated. Hardware setup lives in board.rs.

mod board;

use embassy_executor::Spawner;
use embassy_time::Timer;
{uses}
#[embassy_executor::main]
async fn main(_spawner: Spawner) {{
    #[allow(unused_mut, unused_variables)]
    let mut board = board::init();
{log_start}
{body}}}
"#
    )
}

/// Rust field name for a pin: its user label, or the pin name.
fn field_name(pin: &str, label: &str) -> String {
    if label.trim().is_empty() {
        ident(pin)
    } else {
        ident(label)
    }
}

struct BoardField {
    name: String,
    ty: String,
    init: String,
}

fn board_rs(project: &Project, chip: &Chip, tree: Option<&ClockTree>) -> String {
    let mut fields: Vec<BoardField> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut irq_bindings: Vec<(String, String)> = Vec::new();
    let mut used_names = std::collections::BTreeSet::new();
    let mut unique = |base: String| -> String {
        let mut name = base.clone();
        let mut i = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{base}_{i}");
            i += 1;
        }
        name
    };

    // Lower GPIO speed variants are not available everywhere (gpio v1 / F0).
    let gpio_v1 = chip
        .peripherals()
        .iter()
        .any(|p| p.registers.as_ref().is_some_and(|r| r.kind == "gpio" && r.version == "v1"));
    let speed = |s: Speed| match s {
        Speed::Low => "Speed::Low",
        Speed::Medium => "Speed::Medium",
        Speed::High if !gpio_v1 && !chip.family.contains("F0") => "Speed::High",
        Speed::High | Speed::VeryHigh => "Speed::VeryHigh",
    };
    let pull = |p: Pull| match p {
        Pull::None => "Pull::None",
        Pull::Up => "Pull::Up",
        Pull::Down => "Pull::Down",
    };

    // GPIO pins.
    let exti = chip.peripheral("EXTI");
    let mut exti_lines = std::collections::BTreeMap::new();
    for (pin, cfg) in &project.pins {
        let name = field_name(pin, &cfg.label);
        match &cfg.mode {
            PinMode::GpioOutput => fields.push(BoardField {
                name: unique(name),
                ty: "Output<'static>".into(),
                init: format!(
                    "Output::new(p.{pin}, Level::{}, {})",
                    if cfg.initial_high { "High" } else { "Low" },
                    speed(cfg.speed)
                ),
            }),
            PinMode::GpioInput => fields.push(BoardField {
                name: unique(name),
                ty: "Input<'static>".into(),
                init: format!("Input::new(p.{pin}, {})", pull(cfg.pull)),
            }),
            PinMode::ExtiInput => {
                let line: String = pin.chars().skip(2).collect();
                let channel = format!("EXTI{line}");
                if let Some(other) = exti_lines.insert(line.clone(), pin.clone()) {
                    notes.push(format!(
                        "{pin}: EXTI line {line} is already used by {other}; only one pin per line can be an interrupt. Skipped."
                    ));
                    continue;
                }
                let irq = exti
                    .and_then(|e| e.interrupts.iter().find(|i| i.signal == channel))
                    .map(|i| i.interrupt.clone());
                let Some(irq) = irq else {
                    notes.push(format!("{pin}: no interrupt found for {channel} in chip data. Skipped."));
                    continue;
                };
                if !irq_bindings.iter().any(|(i, _)| *i == irq) {
                    irq_bindings.push((irq.clone(), format!("exti::InterruptHandler<interrupt::typelevel::{irq}>")));
                }
                fields.push(BoardField {
                    name: unique(name),
                    ty: "ExtiInput<'static, Async>".into(),
                    init: format!("ExtiInput::new(p.{pin}, p.{channel}, {}, Irqs)", pull(cfg.pull)),
                });
            }
            PinMode::Analog => notes.push(format!(
                "{pin} ({}) is reserved as analog input: pass `p.{pin}` to the ADC driver.",
                if cfg.label.is_empty() { "no label" } else { &cfg.label }
            )),
            PinMode::Peripheral { .. } => {}
        }
    }

    // Peripheral drivers.
    let mut needs = std::collections::BTreeSet::new();
    for (name, pcfg) in &project.peripherals {
        if !pcfg.enabled {
            continue;
        }
        let Some(periph) = chip.peripheral(name) else { continue };
        let kind = periph.registers.as_ref().map(|r| r.kind.as_str()).unwrap_or("");
        let pins = project.peripheral_pins(name);
        let pin = |sig: &str| pins.iter().find(|(_, s)| s == sig).map(|(p, _)| format!("p.{p}"));
        let field = unique(ident(name));
        match kind {
            "usart" | "lpuart" => {
                let baud = project.setting(name, "baudrate").unwrap_or("115200");
                let cfg = format!("{{ let mut c = usart::Config::default(); c.baudrate = {baud}; c }}");
                let (ty, init) = match (pin("RX"), pin("TX")) {
                    (Some(rx), Some(tx)) => (
                        "Uart<'static, Blocking>",
                        format!("Uart::new_blocking(p.{name}, {rx}, {tx}, {cfg}).unwrap()"),
                    ),
                    (None, Some(tx)) => (
                        "UartTx<'static, Blocking>",
                        format!("UartTx::new_blocking(p.{name}, {tx}, {cfg}).unwrap()"),
                    ),
                    (Some(rx), None) => (
                        "UartRx<'static, Blocking>",
                        format!("UartRx::new_blocking(p.{name}, {rx}, {cfg}).unwrap()"),
                    ),
                    (None, None) => {
                        notes.push(format!("{name}: enabled but no TX/RX pin assigned; driver not created."));
                        continue;
                    }
                };
                needs.insert("usart");
                fields.push(BoardField { name: field, ty: ty.into(), init });
            }
            "spi" => {
                let freq = project.setting(name, "frequency").unwrap_or("1000000");
                let cfg = format!("{{ let mut c = spi::Config::default(); c.frequency = Hertz({freq}); c }}");
                let init = match (pin("SCK"), pin("MOSI"), pin("MISO")) {
                    (Some(sck), Some(mosi), Some(miso)) => {
                        format!("Spi::new_blocking(p.{name}, {sck}, {mosi}, {miso}, {cfg})")
                    }
                    (Some(sck), Some(mosi), None) => format!("Spi::new_blocking_txonly(p.{name}, {sck}, {mosi}, {cfg})"),
                    (Some(sck), None, Some(miso)) => format!("Spi::new_blocking_rxonly(p.{name}, {sck}, {miso}, {cfg})"),
                    _ => {
                        notes.push(format!("{name}: assign SCK plus MOSI and/or MISO to create the SPI driver."));
                        continue;
                    }
                };
                needs.insert("spi");
                fields.push(BoardField {
                    name: field,
                    ty: "Spi<'static, Blocking, spi::mode::Master>".into(),
                    init,
                });
            }
            "i2c" => {
                let freq = project.setting(name, "frequency").unwrap_or("100000");
                let (Some(scl), Some(sda)) = (pin("SCL"), pin("SDA")) else {
                    notes.push(format!("{name}: assign SCL and SDA to create the I2C driver."));
                    continue;
                };
                needs.insert("i2c");
                fields.push(BoardField {
                    name: field,
                    ty: "I2c<'static, Blocking, i2c::Master>".into(),
                    init: format!(
                        "I2c::new_blocking(p.{name}, {scl}, {sda}, {{ let mut c = i2c::Config::default(); c.frequency = Hertz({freq}); c }})"
                    ),
                });
            }
            _ => {
                let pin_list: Vec<String> = pins.iter().map(|(p, s)| format!("{s}={p}")).collect();
                notes.push(format!(
                    "{name} ({kind}) enabled{}: create its driver from `p.{name}` in board::init.",
                    if pin_list.is_empty() { String::new() } else { format!(", pins {}", pin_list.join(", ")) }
                ));
            }
        }
    }

    // ---- assemble ----
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Board support generated by Embassy Studio for {} ({}).\n//!\n//! DO NOT EDIT: this file is regenerated from embassy-studio.json.\n//! Put application code in main.rs.\n",
        chip.name, project.package
    );
    let has = |ty: &str| fields.iter().any(|f| f.ty.starts_with(ty));
    let mut uses = vec!["Config".to_string()];
    let mut gpio = Vec::new();
    if has("Output") || has("Input") || has("ExtiInput") {
        gpio.push("Level");
    }
    if has("Output") {
        gpio.push("Output");
        gpio.push("Speed");
    }
    if has("Input") {
        gpio.push("Input");
    }
    if has("Input") || has("ExtiInput") {
        gpio.push("Pull");
    }
    gpio.sort();
    gpio.dedup();
    if !gpio.is_empty() {
        uses.push(format!("gpio::{{{}}}", gpio.join(", ")));
    }
    if has("ExtiInput") {
        uses.push("bind_interrupts".into());
        uses.push("exti::{self, ExtiInput}".into());
        uses.push("interrupt".into());
        uses.push("mode::Async".into());
    }
    if !needs.is_empty() {
        uses.push("mode::Blocking".into());
    }
    if needs.contains("usart") {
        let mut items = vec!["self"];
        for t in ["Uart", "UartTx", "UartRx"] {
            if fields.iter().any(|f| f.ty.starts_with(&format!("{t}<"))) {
                items.push(t);
            }
        }
        uses.push(format!("usart::{{{}}}", items.join(", ")));
    }
    if needs.contains("spi") {
        uses.push("spi::{self, Spi}".into());
    }
    if needs.contains("i2c") {
        uses.push("i2c::{self, I2c}".into());
    }
    let rcc = rcc_config(project, chip, tree);
    if needs.contains("spi") || needs.contains("i2c") || rcc.uses_hertz {
        uses.push("time::Hertz".into());
    }
    let _ = writeln!(out, "#[allow(unused_imports)]\nuse embassy_stm32::{{{}}};\n", uses.join(", "));

    if !irq_bindings.is_empty() {
        out.push_str("bind_interrupts!(pub struct Irqs {\n");
        for (irq, handler) in &irq_bindings {
            let _ = writeln!(out, "    {irq} => {handler};");
        }
        out.push_str("});\n\n");
    }

    out.push_str("/// Clock configuration.\n");
    out.push_str(&rcc.text);
    out.push('\n');

    out.push_str("/// Configured pins and peripherals.\n#[allow(dead_code)]\npub struct Board {\n");
    for f in &fields {
        let _ = writeln!(out, "    pub {}: {},", f.name, f.ty);
    }
    out.push_str("}\n\n");

    out.push_str("/// Initialise the chip: clocks first, then pins and peripheral drivers.\npub fn init() -> Board {\n");
    out.push_str("    let p = embassy_stm32::init(config());\n");
    if fields.is_empty() {
        out.push_str("    let _ = p;\n");
    }
    for note in &notes {
        let _ = writeln!(out, "    // NOTE: {note}");
    }
    out.push_str("    Board {\n");
    for f in &fields {
        let _ = writeln!(out, "        {}: {},", f.name, f.init);
    }
    out.push_str("    }\n}\n");
    out
}

struct RccCode {
    text: String,
    uses_hertz: bool,
}

fn rcc_config(project: &Project, chip: &Chip, tree: Option<&ClockTree>) -> RccCode {
    let mut out = String::from("pub fn config() -> Config {\n    #[allow(unused_mut)]\n    let mut config = Config::default();\n");
    let Some(tree) = tree else {
        out.push_str("    config\n}\n");
        return RccCode { text: out, uses_hertz: false };
    };
    let clk = &project.clock;
    let results = clk.compute(tree);

    // Human-readable summary of the configured tree.
    out.push_str("    // Clock tree (from Embassy Studio):\n");
    for osc in &tree.oscillators {
        if clk.is_enabled(osc) {
            let _ = writeln!(out, "    //   {osc}: {}", format_hz(clk.oscillators.get(osc).copied().unwrap_or(0.0)));
        }
    }
    if clk.pll_enabled && tree.has_pll() {
        let _ = writeln!(
            out,
            "    //   PLL: src={} {} {} -> VCO {}",
            clk.role(Role::PllSrc).unwrap_or("-"),
            clk.role(Role::PllM).unwrap_or("-"),
            clk.role(Role::PllN).unwrap_or("-"),
            format_hz(results.vco)
        );
    }
    let _ = writeln!(
        out,
        "    //   SYSCLK={} ({}), HCLK={}",
        format_hz(results.sysclk),
        clk.role(Role::Sys).unwrap_or("-"),
        format_hz(results.hclk)
    );
    for (bus, _, f) in &results.apb {
        let _ = writeln!(out, "    //   {bus}={}", format_hz(*f));
    }
    for (pll, r) in tree.aux_plls.iter().zip(&results.aux).filter(|(_, r)| r.enabled) {
        let hz = |f: Option<f64>| f.map(format_hz).unwrap_or_else(|| "-".into());
        let _ = writeln!(
            out,
            "    //   {}: VCO {} -> P={} Q={} R={}",
            pll.name,
            format_hz(r.vco),
            hz(r.p),
            hz(r.q),
            hz(r.r)
        );
    }

    let flavor = rcc_flavor(tree);
    let mut code = String::new();
    let mut uses_hertz = false;
    let ty = |role: Role, alias: &str| -> Option<String> {
        let v = clk.role(role)?;
        tree.selector(role)?;
        Some(format!("{alias}::{}", pac_variant(v)))
    };
    let sys = clk.role(Role::Sys).unwrap_or("");
    let uses_hse = clk.is_enabled("HSE");
    let pll_needed = clk.pll_enabled && tree.has_pll();
    let not_default = uses_hse || pll_needed || sys != tree.sys.as_ref().map(|s| s.default_variant()).unwrap_or("")
        || clk.role(Role::Ahb) != tree.ahb.as_ref().map(|s| s.default_variant())
        || tree.apb.iter().any(|s| clk.apb.get(&s.field).map(String::as_str) != Some(s.default_variant()));

    match flavor {
        RccFlavor::F247 | RccFlavor::G4 if not_default => {
            code.push_str("    {\n        use embassy_stm32::rcc::*;\n");
            if uses_hse {
                uses_hertz = true;
                let hz = clk.oscillators.get("HSE").copied().unwrap_or(0.0) as u64;
                let mode = if clk.hse_bypass { "Bypass" } else { "Oscillator" };
                let _ = writeln!(
                    code,
                    "        config.rcc.hse = Some(Hse {{ freq: Hertz({hz}), mode: HseMode::{mode} }});"
                );
            }
            if pll_needed {
                let opt = |role: Role, alias: &str| match ty(role, alias) {
                    Some(v) if clk.pll_output_enabled(role) => format!("Some({v})"),
                    _ => "None".into(),
                };
                let prediv = ty(Role::PllM, "PllPreDiv").unwrap_or_default();
                let mul = ty(Role::PllN, "PllMul").unwrap_or_default();
                let src = ty(Role::PllSrc, "PllSource").unwrap_or_default();
                let divp = opt(Role::PllP, "PllPDiv");
                let divq = opt(Role::PllQ, "PllQDiv");
                let divr = opt(Role::PllR, "PllRDiv");
                if flavor == RccFlavor::F247 {
                    let _ = writeln!(code, "        config.rcc.pll_src = {src};");
                    let _ = writeln!(
                        code,
                        "        config.rcc.pll = Some(Pll {{ prediv: {prediv}, mul: {mul}, divp: {divp}, divq: {divq}, divr: {divr} }});"
                    );
                } else {
                    let _ = writeln!(
                        code,
                        "        config.rcc.pll = Some(Pll {{ source: {src}, prediv: {prediv}, mul: {mul}, divp: {divp}, divq: {divq}, divr: {divr} }});"
                    );
                }
            }
            if let Some(v) = ty(Role::Sys, "Sysclk") {
                let _ = writeln!(code, "        config.rcc.sys = {v};");
            }
            if let Some(v) = ty(Role::Ahb, "AHBPrescaler") {
                let _ = writeln!(code, "        config.rcc.ahb_pre = {v};");
            }
            for sel in &tree.apb {
                let Some(v) = clk.apb.get(&sel.field) else { continue };
                let n: String = sel.field.chars().filter(|c| c.is_ascii_digit()).collect();
                let n = if n.is_empty() { "1".to_string() } else { n };
                let _ = writeln!(code, "        config.rcc.apb{n}_pre = APBPrescaler::{};", pac_variant(v));
            }
            code.push_str("    }\n");
        }
        RccFlavor::SummaryOnly if not_default => {
            let _ = writeln!(
                code,
                "    // NOTE: automatic RCC code for the '{}' RCC version is not implemented yet.\n    // Set `config.rcc` by hand to match the tree above (see embassy-stm32 rcc docs).",
                tree.rcc_version
            );
        }
        _ => {}
    }

    // Kernel clock muxes chosen by the user.
    let mut mux_lines = String::new();
    for (key, variant) in &clk.muxes {
        let Some(sel) = tree.muxes.get(key) else { continue };
        let periph_enabled = chip.peripherals().iter().any(|p| {
            project.is_peripheral_enabled(&p.name)
                && matches!(
                    p.rcc.as_ref().and_then(|r| r.kernel_clock.as_ref()),
                    Some(KernelClock::Mux(m)) if m.register == sel.register && m.field == sel.field
                )
        });
        if !periph_enabled || variant == sel.default_variant() {
            continue;
        }
        let _ = writeln!(
            mux_lines,
            "    config.rcc.mux.{} = embassy_stm32::rcc::mux::{}::{};",
            sel.field.to_ascii_lowercase(),
            pac_type_name(&sel.enum_name),
            pac_variant(variant)
        );
    }
    if flavor != RccFlavor::SummaryOnly {
        code.push_str(&mux_lines);
    } else if !mux_lines.is_empty() {
        for l in mux_lines.lines() {
            let _ = writeln!(code, "    // {}", l.trim());
        }
    }

    out.push_str(&code);
    out.push_str("    config\n}\n");
    RccCode { text: out, uses_hertz }
}

fn readme(project: &Project, chip: &Chip, triple: &str) -> String {
    format!(
        r#"# {name}

Embassy firmware for **{chip}** ({package}), generated by Embassy Studio.

## Layout

| Path | Owner |
|------|-------|
| `embassy-studio.json` | Embassy Studio project (pins, clocks, peripherals) |
| `src/board.rs` | Generated — regenerated on every *Generate Code* |
| `src/main.rs` | Yours — created once, never overwritten |
| `Cargo.toml` | Generated, except the user section at the bottom |

## Build & flash

```sh
rustup target add {triple}
cargo install probe-rs-tools
cargo build --release
cargo run --release     # flashes with probe-rs and streams defmt logs
```

## IDEs

- **VS Code**: open this folder; install *rust-analyzer* and *probe-rs debugger*.
  Press F5 to build, flash and debug.
- **STM32CubeIDE / other GDB-based IDEs**: build with `cargo build`, then debug the ELF at
  `target/{triple}/debug/{pkg}` with an ST-LINK GDB server or `probe-rs gdb`.
"#,
        name = project.name,
        chip = chip.name,
        package = project.package,
        pkg = crate_name(&project.name),
    )
}

fn vscode_files(project: &Project, _chip: &Chip, triple: &str, pkg: &str) -> Vec<GenFile> {
    let settings = format!(
        r#"{{
  "rust-analyzer.cargo.target": "{triple}",
  "rust-analyzer.cargo.allTargets": false,
  "rust-analyzer.check.allTargets": false,
  "editor.formatOnSave": true
}}
"#
    );
    let launch = format!(
        r#"{{
  "version": "0.2.0",
  "configurations": [
    {{
      "type": "probe-rs-debug",
      "request": "launch",
      "name": "Flash & debug ({chip})",
      "cwd": "${{workspaceFolder}}",
      "preLaunchTask": "cargo build",
      "chip": "{chip}",
      "flashingConfig": {{ "flashingEnabled": true, "haltAfterReset": false }},
      "coreConfigs": [
        {{
          "coreIndex": 0,
          "programBinary": "target/{triple}/debug/{pkg}",
          "rttEnabled": true
        }}
      ]
    }}
  ]
}}
"#,
        chip = project.package
    );
    let tasks = r#"{
  "version": "2.0.0",
  "tasks": [
    {
      "label": "cargo build",
      "type": "cargo",
      "command": "build",
      "problemMatcher": ["$rustc"],
      "group": { "kind": "build", "isDefault": true }
    },
    {
      "label": "cargo run (flash)",
      "type": "cargo",
      "command": "run",
      "args": ["--release"],
      "problemMatcher": ["$rustc"]
    }
  ]
}
"#;
    let extensions = r#"{
  "recommendations": ["rust-lang.rust-analyzer", "probe-rs.probe-rs-debugger", "tamasfe.even-better-toml"]
}
"#;
    vec![
        GenFile { path: ".vscode/settings.json".into(), contents: settings, policy: Policy::Overwrite },
        GenFile { path: ".vscode/launch.json".into(), contents: launch, policy: Policy::Overwrite },
        GenFile { path: ".vscode/tasks.json".into(), contents: tasks.into(), policy: Policy::Overwrite },
        GenFile { path: ".vscode/extensions.json".into(), contents: extensions.into(), policy: Policy::CreateOnly },
    ]
}
