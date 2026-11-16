//! ESP32 chip data from esp-hal's `esp-metadata` device files.
//!
//! Like the STM32 side, nothing chip-specific is hardcoded here. Each chip is read from
//! `esp-metadata/devices/<chip>/{soc,gpio,clocks}.toml` of the esp-hal release the code
//! generator targets ([`ESP_HAL_TAG`]), plus the `CpuClock` enum of that release, and
//! converted to the common [`Chip`] model:
//!
//! - every GPIO becomes a pin of a synthetic package (ESP32 parts route signals through a
//!   GPIO matrix, so the metadata has no package pinouts);
//! - the driver instances (`[device.uart]`, `[device.spi_master]`, …) become peripherals whose
//!   signals can be routed to any GPIO that can drive them;
//! - target triple, CPU clock presets, crystal frequencies and pin restrictions (strapping,
//!   flash, input-only) are kept in [`EspChip`].
//!
//! Files are cached in `%LOCALAPPDATA%\EmbassyStudio\esp-metadata\<tag>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};
use toml::{Table, Value as TomlValue};

use crate::db::{Chip, Core, Doc, Package, PackagePin, PeriphInterrupt, PeriphPin, Peripheral, Platform, RegRef};

const REPO: &str = "esp-rs/esp-hal";
/// esp-hal release whose metadata is read; it matches the esp-hal version in generated `Cargo.toml`s.
pub const ESP_HAL_TAG: &str = "esp-hal-v1.2.2";

/// The synthetic package every ESP32 chip gets (all GPIOs, in number order).
pub const GPIO_PACKAGE: &str = "GPIO matrix";

/// ESP32-specific chip details.
#[derive(Clone, Debug, Default)]
pub struct EspChip {
    /// Cargo feature / chip name in esp-hal, e.g. `esp32c3`.
    pub feature: String,
    /// `riscv` or `xtensa`.
    pub arch: String,
    /// Rust target triple, e.g. `riscv32imc-unknown-none-elf`.
    pub target: String,
    pub cores: u32,
    /// `CpuClock` presets of esp-hal, in MHz, ascending.
    pub cpu_clocks_mhz: Vec<u32>,
    /// `CpuClock::default()`.
    pub default_cpu_clock_mhz: Option<u32>,
    /// Supported crystal frequencies, in MHz.
    pub xtal_mhz: Vec<u32>,
    /// GPIO name -> restrictions (`strapping`, `spi_flash`, `input_only`, …).
    pub pin_limits: BTreeMap<String, Vec<String>>,
    /// The chip has a USB Serial/JTAG controller (flashing and debugging over USB).
    pub usb_serial_jtag: bool,
}

impl EspChip {
    pub fn is_xtensa(&self) -> bool {
        self.arch == "xtensa"
    }

    pub fn max_cpu_clock_mhz(&self) -> Option<u32> {
        self.cpu_clocks_mhz.iter().copied().max()
    }

    /// Restrictions of a pin that make it a poor choice for general I/O.
    pub fn reserved(&self, gpio: &str) -> bool {
        self.pin_limits.get(gpio).is_some_and(|l| {
            l.iter().any(|x| x.contains("flash") || x.contains("psram") || x.starts_with("esp32_pico"))
        })
    }

    pub fn input_only(&self, gpio: &str) -> bool {
        self.pin_limits.get(gpio).is_some_and(|l| l.iter().any(|x| x == "input_only"))
    }
}

/// Human-readable explanation of a pin restriction from the metadata.
pub fn describe_limit(limit: &str) -> String {
    match limit {
        "strapping" => "Strapping pin: its level at reset selects the boot mode".into(),
        "spi_flash" => "Connected to the SPI flash on most modules".into(),
        "spi_psram" => "Connected to PSRAM on modules that have it".into(),
        "octal_flash" | "octal_psram" => "Used by octal flash / PSRAM on modules that have it".into(),
        "input_only" => "Input only: cannot drive outputs".into(),
        "jtag" => "JTAG pin".into(),
        "usb_jtag" => "USB Serial/JTAG pin (D+ / D−)".into(),
        l if l.starts_with("esp32_pico") => "Only bonded out on ESP32-PICO parts".into(),
        l => l.replace('_', " "),
    }
}

fn cache_dir() -> PathBuf {
    crate::db::app_dir().join("esp-metadata").join(ESP_HAL_TAG)
}

pub fn describe_source() -> String {
    format!("github.com/{REPO} ({ESP_HAL_TAG})")
}

/// Read a file of the esp-hal repository at [`ESP_HAL_TAG`], through the cache. A failed
/// download falls back to the cached copy, so a refresh works offline.
fn read_file(rel: &str, refresh: bool) -> Result<String> {
    let path = cache_dir().join(rel);
    let cached = std::fs::read_to_string(&path).ok();
    if !refresh {
        if let Some(s) = cached {
            return Ok(s);
        }
    }
    let url = format!("https://raw.githubusercontent.com/{REPO}/{ESP_HAL_TAG}/{rel}");
    match crate::db::http_get(&url) {
        Ok(body) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&path, &body).ok();
            Ok(body)
        }
        Err(e) => cached.ok_or(e),
    }
}

/// ESP32 chips with a device description in the pinned esp-hal release, e.g. `ESP32C3`.
pub fn list_chips() -> Result<Vec<String>> {
    let url = format!("https://api.github.com/repos/{REPO}/contents/esp-metadata/devices?ref={ESP_HAL_TAG}");
    let v: Value = serde_json::from_str(&crate::db::http_get(&url)?)?;
    let entries = v.as_array().ok_or_else(|| anyhow!("unexpected GitHub API response from {url}"))?;
    let chips: Vec<String> = entries
        .iter()
        .filter(|e| e["type"] == "dir")
        .filter_map(|e| e["name"].as_str())
        .filter(|n| n.starts_with("esp32"))
        .map(str::to_ascii_uppercase)
        .collect();
    if chips.is_empty() {
        anyhow::bail!("no device files in {REPO} at {ESP_HAL_TAG}");
    }
    Ok(chips)
}

/// `ESP32C3` -> `ESP32-C3`, `ESP32` -> `ESP32`.
pub fn display_name(chip: &str) -> String {
    let up = chip.to_ascii_uppercase();
    match up.strip_prefix("ESP32") {
        Some(rest) if !rest.is_empty() && !rest.starts_with('-') => format!("ESP32-{rest}"),
        _ => up,
    }
}

pub fn load_chip(name: &str, refresh: bool) -> Result<Chip> {
    let feature = name.to_ascii_lowercase();
    let dir = format!("esp-metadata/devices/{feature}");
    let soc = read_file(&format!("{dir}/soc.toml"), refresh).with_context(|| format!("{name}: soc.toml"))?;
    let gpio = read_file(&format!("{dir}/gpio.toml"), refresh).with_context(|| format!("{name}: gpio.toml"))?;
    let clocks = read_file(&format!("{dir}/clocks.toml"), refresh).ok();
    let cpu_clock = read_file(&format!("esp-hal/src/soc/{feature}/clocks.rs"), refresh).ok();
    build_chip(name, &soc, &gpio, clocks.as_deref(), cpu_clock.as_deref())
}

/// Convert the metadata of one chip to the common chip model.
pub(crate) fn build_chip(name: &str, soc: &str, gpio: &str, clocks: Option<&str>, clocks_rs: Option<&str>) -> Result<Chip> {
    let soc: Table = toml::from_str(soc).with_context(|| format!("parsing {name} soc.toml"))?;
    let gpio: Table = toml::from_str(gpio).with_context(|| format!("parsing {name} gpio.toml"))?;
    let dev = soc.get("device").and_then(TomlValue::as_table).ok_or_else(|| anyhow!("{name}: no [device] table"))?;
    let s = |t: &Table, k: &str| t.get(k).and_then(TomlValue::as_str).unwrap_or_default().to_string();

    let feature = name.to_ascii_lowercase();
    let arch = s(dev, "arch");
    let mut esp = EspChip {
        feature: feature.clone(),
        target: s(dev, "target"),
        cores: dev.get("cores").and_then(TomlValue::as_integer).unwrap_or(1) as u32,
        usb_serial_jtag: dev.contains_key("usb_serial_jtag"),
        arch: arch.clone(),
        ..Default::default()
    };

    // ---- GPIOs ----
    let names = |key: &str| -> BTreeSet<String> {
        gpio.get(key)
            .and_then(TomlValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.get("name").and_then(TomlValue::as_str).map(str::to_string))
            .collect()
    };
    let inputs = names("input_signals");
    let outputs = names("output_signals");

    struct Gpio {
        name: String,
        signals: Vec<String>,
        analog: Vec<String>,
    }
    let ordered = |t: Option<&TomlValue>| -> Vec<String> {
        let mut v: Vec<(i64, String)> = t
            .and_then(TomlValue::as_table)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.parse().unwrap_or(99), v.as_str()?.to_string())))
            .collect();
        v.sort();
        v.into_iter().map(|(_, s)| s).collect()
    };
    let mut gpios: Vec<Gpio> = Vec::new();
    for p in gpio.get("pins").and_then(TomlValue::as_array).into_iter().flatten() {
        let Some(n) = p.get("pin").and_then(TomlValue::as_integer) else { continue };
        let gname = format!("GPIO{n}");
        let limits: Vec<String> = p
            .get("limitations")
            .and_then(TomlValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect();
        if !limits.is_empty() {
            esp.pin_limits.insert(gname.clone(), limits);
        }
        let analog = ordered(p.get("analog"));
        let mut signals = vec![gname.clone()];
        signals.extend(ordered(p.get("functions")));
        signals.extend(analog.iter().cloned());
        gpios.push(Gpio { name: gname, signals, analog });
    }
    gpios.sort_by_key(|g| g.name[4..].parse::<u32>().unwrap_or(0));

    // ---- Peripherals ----
    // Interrupts per soc peripheral, e.g. `{ name = "UART0", interrupts = { peri = "UART0" } }`.
    let mut soc_periphs: BTreeMap<String, Vec<PeriphInterrupt>> = BTreeMap::new();
    for p in dev
        .get("soc")
        .and_then(|v| v.get("peripherals"))
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
    {
        let Some(pname) = p.get("name").and_then(TomlValue::as_str) else { continue };
        let irqs = p
            .get("interrupts")
            .and_then(TomlValue::as_table)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some(PeriphInterrupt { signal: k.clone(), interrupt: v.as_str()?.to_string() }))
            .collect();
        soc_periphs.insert(pname.to_string(), irqs);
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Dir {
        In,
        Out,
        Any,
    }
    let mut peripherals: Vec<Peripheral> = Vec::new();
    let mut add = |pname: String, kind: &str, signals: Vec<(String, String, Dir)>| {
        let mut pins = Vec::new();
        for (friendly, matrix, dir) in &signals {
            if matrix.is_empty() {
                continue;
            }
            // Signals the GPIO matrix does not know cannot be routed.
            let known = inputs.contains(matrix) || outputs.contains(matrix);
            if !known {
                continue;
            }
            for g in &gpios {
                if *dir != Dir::In && esp.input_only(&g.name) {
                    continue;
                }
                pins.push(PeriphPin { pin: g.name.clone(), signal: friendly.clone(), af: None });
            }
        }
        peripherals.push(Peripheral {
            interrupts: soc_periphs.get(&pname).cloned().unwrap_or_default(),
            name: pname,
            address: 0,
            registers: Some(RegRef { kind: kind.into(), version: "esp-hal".into(), block: kind.into() }),
            rcc: None,
            pins,
            dma_channels: Vec::new(),
        });
    };
    let instances = |section: &str| -> Vec<Table> {
        dev.get(section)
            .and_then(|v| v.get("instances"))
            .and_then(TomlValue::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_table().cloned())
            .collect()
    };
    let list = |t: &Table, k: &str| -> Vec<String> {
        match t.get(k) {
            Some(TomlValue::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            Some(TomlValue::String(s)) => vec![s.clone()],
            _ => Vec::new(),
        }
    };

    for t in instances("uart") {
        let sig = vec![
            ("TX".into(), s(&t, "tx"), Dir::Out),
            ("RX".into(), s(&t, "rx"), Dir::In),
            ("CTS".into(), s(&t, "cts"), Dir::In),
            ("RTS".into(), s(&t, "rts"), Dir::Out),
        ];
        add(s(&t, "name").to_ascii_uppercase(), "uart", sig);
    }
    for t in instances("spi_master") {
        let sio = list(&t, "sio");
        let mut sig = vec![
            ("SCK".into(), s(&t, "sclk"), Dir::Out),
            ("MOSI".into(), sio.first().cloned().unwrap_or_default(), Dir::Out),
            ("MISO".into(), sio.get(1).cloned().unwrap_or_default(), Dir::In),
        ];
        for (i, cs) in list(&t, "cs").into_iter().enumerate() {
            sig.push((format!("CS{i}"), cs, Dir::Out));
        }
        add(s(&t, "name").to_ascii_uppercase(), "spi", sig);
    }
    for t in instances("i2c_master") {
        let sig = vec![("SCL".into(), s(&t, "scl"), Dir::Out), ("SDA".into(), s(&t, "sda"), Dir::Out)];
        add(s(&t, "name").to_ascii_uppercase(), "i2c", sig);
    }
    for t in instances("i2s") {
        let mut sig = vec![
            ("MCLK".into(), s(&t, "mclk"), Dir::Out),
            ("BCLK".into(), s(&t, "bclk"), Dir::Out),
            ("WS".into(), s(&t, "ws"), Dir::Out),
            ("BCLK_RX".into(), s(&t, "bclk_rx"), Dir::Out),
            ("WS_RX".into(), s(&t, "ws_rx"), Dir::Out),
        ];
        let dout = list(&t, "dout");
        let one = dout.len() == 1;
        for (i, d) in dout.into_iter().enumerate() {
            sig.push((if one { "DOUT".into() } else { format!("DOUT{i}") }, d, Dir::Out));
        }
        let din = list(&t, "din");
        let one = din.len() == 1;
        for (i, d) in din.into_iter().enumerate() {
            sig.push((if one { "DIN".into() } else { format!("DIN{i}") }, d, Dir::In));
        }
        add(s(&t, "name").to_ascii_uppercase(), "i2s", sig);
    }
    // Blocks without signal names in their driver section: signals are found by prefix.
    let by_prefix = |prefix: &str| -> Vec<(String, String, Dir)> {
        let all: BTreeSet<&String> = inputs.iter().chain(outputs.iter()).collect();
        all.into_iter()
            .filter_map(|m| {
                let friendly = m.strip_prefix(prefix)?;
                let dir = match (inputs.contains(m), outputs.contains(m)) {
                    (true, false) => Dir::In,
                    (false, true) => Dir::Out,
                    _ => Dir::Any,
                };
                Some((friendly.to_string(), m.clone(), dir))
            })
            .collect()
    };
    let twai: Vec<&String> = soc_periphs.keys().filter(|p| p.starts_with("TWAI")).collect();
    for p in &twai {
        let n = p.trim_start_matches("TWAI");
        let mut sig = by_prefix(&format!("TWAI{n}_"));
        if sig.is_empty() && twai.len() == 1 {
            sig = by_prefix("TWAI_");
        }
        add(p.to_string(), "twai", sig);
    }
    if soc_periphs.contains_key("LEDC") {
        add("LEDC".into(), "ledc", by_prefix("LEDC_"));
    }
    if soc_periphs.contains_key("RMT") {
        add("RMT".into(), "rmt", by_prefix("RMT_"));
    }
    // ADC channels are fixed analog functions of particular pins.
    for t in instances("adc") {
        let pname = s(&t, "name").to_ascii_uppercase();
        let prefix = format!("{pname}_");
        let pins = gpios
            .iter()
            .flat_map(|g| {
                g.analog.iter().filter_map(|a| {
                    a.strip_prefix(&prefix).map(|ch| PeriphPin { pin: g.name.clone(), signal: ch.to_string(), af: None })
                })
            })
            .collect();
        peripherals.push(Peripheral {
            interrupts: Vec::new(),
            name: pname,
            address: 0,
            registers: Some(RegRef { kind: "adc".into(), version: "esp-hal".into(), block: "adc".into() }),
            rcc: None,
            pins,
            dma_channels: Vec::new(),
        });
    }

    // ---- Clocks ----
    if let Some(rs) = clocks_rs {
        let (clocks, default) = parse_cpu_clocks(rs);
        esp.cpu_clocks_mhz = clocks;
        esp.default_cpu_clock_mhz = default;
    }
    if let Some(c) = clocks.and_then(|c| toml::from_str::<Table>(c).ok()) {
        esp.xtal_mhz = parse_xtal(&c);
    }

    // ---- Memory and docs ----
    let mut memory = Vec::new();
    for r in dev
        .get("soc")
        .and_then(|v| v.get("memory_map"))
        .and_then(|v| v.get("ranges"))
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(n), Some(a), Some(b)) = (
            r.get("name").and_then(TomlValue::as_str),
            r.get("start").and_then(TomlValue::as_integer),
            r.get("end").and_then(TomlValue::as_integer),
        ) else {
            continue;
        };
        if n == "dram" {
            memory.push(json!({ "name": "DRAM", "kind": "ram", "size": (b - a).max(0) }));
        }
    }
    let mut docs = Vec::new();
    for (key, title) in [("trm", "Technical Reference Manual"), ("datasheet", "Datasheet")] {
        let url = s(dev, key);
        if !url.is_empty() {
            docs.push(Doc { title: format!("{} {title}", display_name(name)), url });
        }
    }

    let arch_title = if arch == "xtensa" { "Xtensa" } else { "RISC-V" };
    let package = Package {
        name: format!("{}-{}", name.to_ascii_uppercase(), "GPIO"),
        package: GPIO_PACKAGE.into(),
        pins: gpios
            .iter()
            .enumerate()
            .map(|(i, g)| PackagePin { position: (i + 1).to_string(), signals: g.signals.clone() })
            .collect(),
    };
    peripherals.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Chip {
        name: name.to_ascii_uppercase(),
        family: display_name(name),
        line: format!("{arch_title}, {} core{}", esp.cores, if esp.cores == 1 { "" } else { "s" }),
        packages: vec![package],
        memory: Value::Array(memory),
        docs,
        cores: vec![Core { name: arch, peripherals }],
        platform: Platform::Esp,
        esp: Some(esp),
    })
}

/// Variants of `pub enum CpuClock { _80MHz = 80, … }` in esp-hal's `soc/<chip>/clocks.rs`,
/// ascending, and the `#[default]` one.
fn parse_cpu_clocks(rs: &str) -> (Vec<u32>, Option<u32>) {
    let mut out = Vec::new();
    let mut default = None;
    let mut inside = false;
    let mut next_default = false;
    for line in rs.lines() {
        let l = line.trim();
        if !inside {
            inside = l.starts_with("pub enum CpuClock");
            continue;
        }
        if l.starts_with('}') {
            break;
        }
        if l.starts_with("#[default]") {
            next_default = true;
            continue;
        }
        if let Some(rest) = l.strip_prefix('_') {
            if let Some(mhz) = rest.split("MHz").next().and_then(|d| d.parse::<u32>().ok()) {
                out.push(mhz);
                if std::mem::take(&mut next_default) {
                    default = Some(mhz);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    (out, default)
}

/// Crystal frequencies of the `XTAL_CLK` source in `clocks.toml` (`values = "26, 40"`).
fn parse_xtal(clocks: &Table) -> Vec<u32> {
    let tree = clocks
        .get("system_clocks")
        .and_then(|v| v.get("clock_tree"))
        .and_then(TomlValue::as_array);
    let Some(xtal) = tree.into_iter().flatten().find(|c| c.get("name").and_then(TomlValue::as_str) == Some("XTAL_CLK")) else {
        return Vec::new();
    };
    if let Some(values) = xtal.get("values").and_then(TomlValue::as_str) {
        return values.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    }
    // A fixed crystal: `output = "40_000_000"`.
    xtal.get("output")
        .and_then(TomlValue::as_str)
        .and_then(|o| o.replace('_', "").trim().parse::<u64>().ok())
        .map(|hz| vec![(hz / 1_000_000) as u32])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOC: &str = r#"
[device]
name   = "esp32c3"
arch   = "riscv"
target = "riscv32imc-unknown-none-elf"
cores  = 1
trm    = "https://example.com/esp32-c3_technical_reference_manual_en.pdf"

[device.soc]
memory_map = { ranges = [
    { name = "dram", start = 0x3FC8_0000, end = 0x3FCE_0000 },
] }
peripherals = [
    { name = "UART0", interrupts = { peri = "UART0" }, stable = true },
    { name = "TWAI0" },
]

[device.uart]
instances = [
    { name = "uart0", sys_instance = "Uart0", tx = "U0TXD", rx = "U0RXD", cts = "U0CTS", rts = "U0RTS" },
]

[device.spi_master]
instances = [
    {
        name = "spi2",
        sclk = "FSPICLK",
        sio = ["FSPID", "FSPIQ"],
        cs = ["FSPICS0"],
    },
]

[device.adc]
instances = [{ name = "adc1" }]

[device.usb_serial_jtag]
"#;

    const GPIO: &str = r#"
pins = [
    { pin =  0, analog = { 1 = "ADC1_CH0" } },
    { pin = 10, functions = { 2 = "FSPICS0" }, limitations = ["strapping"] },
    { pin =  2, limitations = ["input_only"] },
]
input_signals = [
    { name = "U0RXD", id = 6 }, { name = "U0CTS", id = 7 }, { name = "FSPIQ", id = 64 },
    { name = "TWAI_RX", id = 74 },
]
output_signals = [
    { name = "U0TXD", id = 6 }, { name = "U0RTS", id = 7 }, { name = "FSPICLK", id = 63 },
    { name = "FSPID", id = 65 }, { name = "FSPICS0", id = 68 }, { name = "TWAI_TX", id = 74 },
]
"#;

    const CLOCKS_RS: &str = r#"
pub enum CpuClock {
    /// 80 MHz CPU clock
    #[default]
    _80MHz  = 80,

    /// 160 MHz CPU clock
    _160MHz = 160,
}
"#;

    const CLOCKS: &str = r#"
system_clocks = { clock_tree = [
    { name = "XTAL_CLK", type = "source", values = "40", output = "VALUE * 1_000_000" },
] }
"#;

    #[test]
    fn builds_chip_from_metadata() {
        let chip = build_chip("ESP32C3", SOC, GPIO, Some(CLOCKS), Some(CLOCKS_RS)).unwrap();
        assert_eq!(chip.platform, Platform::Esp);
        assert_eq!(chip.family, "ESP32-C3");
        let esp = chip.esp.as_ref().unwrap();
        assert_eq!(esp.target, "riscv32imc-unknown-none-elf");
        assert_eq!(esp.cpu_clocks_mhz, vec![80, 160]);
        assert_eq!(esp.default_cpu_clock_mhz, Some(80));
        assert_eq!(esp.xtal_mhz, vec![40]);
        assert!(esp.usb_serial_jtag);
        assert!(esp.input_only("GPIO2"));
        assert_eq!(chip.ram_size(), 0x6_0000);

        // Pins in number order, with their IO MUX and analog functions.
        let pkg = &chip.packages[0];
        let names: Vec<&str> = pkg.pins.iter().map(|p| p.signals[0].as_str()).collect();
        assert_eq!(names, ["GPIO0", "GPIO2", "GPIO10"]);
        assert_eq!(pkg.pins[2].signals, ["GPIO10", "FSPICS0"]);
        assert_eq!(crate::db::gpio_name("GPIO10").as_deref(), Some("GPIO10"));

        // The GPIO matrix routes any signal to any pin, except outputs to input-only pins.
        let uart = chip.peripheral("UART0").unwrap();
        assert_eq!(uart.interrupts[0].interrupt, "UART0");
        let tx: Vec<&str> = uart.pins.iter().filter(|p| p.signal == "TX").map(|p| p.pin.as_str()).collect();
        assert_eq!(tx, ["GPIO0", "GPIO10"]);
        assert_eq!(uart.pins.iter().filter(|p| p.signal == "RX").count(), 3);

        let spi = chip.peripheral("SPI2").unwrap();
        let sigs: BTreeSet<&str> = spi.pins.iter().map(|p| p.signal.as_str()).collect();
        assert_eq!(sigs, BTreeSet::from(["CS0", "MISO", "MOSI", "SCK"]));

        let twai = chip.peripheral("TWAI0").unwrap();
        let sigs: BTreeSet<&str> = twai.pins.iter().map(|p| p.signal.as_str()).collect();
        assert_eq!(sigs, BTreeSet::from(["RX", "TX"]));

        let adc = chip.peripheral("ADC1").unwrap();
        assert_eq!(adc.pins.len(), 1);
        assert_eq!((adc.pins[0].pin.as_str(), adc.pins[0].signal.as_str()), ("GPIO0", "CH0"));
    }

    #[test]
    fn display_names() {
        assert_eq!(display_name("ESP32"), "ESP32");
        assert_eq!(display_name("esp32c61"), "ESP32-C61");
        assert_eq!(display_name("ESP32S3"), "ESP32-S3");
    }

    /// The chip list of the pinned esp-hal release, and every chip in it loads.
    #[test]
    #[ignore = "needs network access to github.com"]
    fn online_chip_list_loads() {
        let chips = list_chips().unwrap();
        assert!(chips.iter().any(|c| c == "ESP32C3"), "{chips:?}");
        for c in &chips {
            let chip = load_chip(c, false).unwrap_or_else(|e| panic!("{c}: {e:#}"));
            let esp = chip.esp.as_ref().unwrap();
            assert!(!esp.target.is_empty() && !esp.cpu_clocks_mhz.is_empty(), "{c}: {esp:?}");
            println!(
                "{c}: {} GPIOs, {} peripherals, CPU {:?} MHz, XTAL {:?}",
                chip.packages[0].pins.len(),
                chip.peripherals().len(),
                esp.cpu_clocks_mhz,
                esp.xtal_mhz
            );
        }
    }

    /// Every chip in the local esp-metadata cache (skipped when absent).
    #[test]
    fn cached_chips_load() {
        let dir = cache_dir().join("esp-metadata").join("devices");
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_uppercase();
            let chip = load_chip(&name, false).unwrap_or_else(|err| panic!("{name}: {err:#}"));
            assert!(!chip.packages[0].pins.is_empty(), "{name}: no pins");
            assert!(chip.peripheral("UART0").is_some(), "{name}: no UART0");
        }
    }
}
