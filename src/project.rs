//! Project model, the on-disk project file, and the recent-projects list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::clock::ClockConfig;
use crate::schematic::SchematicConfig;

pub const PROJECT_FILE: &str = "embassy-studio.json";
const FORMAT_VERSION: u32 = 1;

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub format_version: u32,
    pub name: String,
    /// Chip name as in stm32-data, e.g. `STM32F411CE`.
    pub chip: String,
    /// Package (ordering code) name, e.g. `STM32F411CEUx`.
    pub package: String,
    #[serde(default)]
    pub clock: ClockConfig,
    /// Pin configuration keyed by GPIO name (`PA5`).
    #[serde(default)]
    pub pins: BTreeMap<String, PinConfig>,
    /// Peripheral configuration keyed by instance name (`USART1`).
    #[serde(default)]
    pub peripherals: BTreeMap<String, PeripheralConfig>,
    #[serde(default)]
    pub codegen: CodegenOptions,
    /// Imported schematic sheets and the pin nodes placed on them.
    #[serde(default, skip_serializing_if = "SchematicConfig::is_empty")]
    pub schematic: SchematicConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode")]
pub enum PinMode {
    GpioOutput,
    GpioInput,
    ExtiInput,
    Analog,
    Peripheral { peripheral: String, signal: String, af: Option<u8> },
}

impl PinMode {
    pub fn label(&self) -> String {
        match self {
            Self::GpioOutput => "GPIO_Output".into(),
            Self::GpioInput => "GPIO_Input".into(),
            Self::ExtiInput => "GPIO_EXTI".into(),
            Self::Analog => "GPIO_Analog".into(),
            Self::Peripheral { peripheral, signal, .. } => format!("{peripheral}_{signal}"),
        }
    }

    pub fn peripheral(&self) -> Option<&str> {
        match self {
            Self::Peripheral { peripheral, .. } => Some(peripheral),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Pull {
    #[default]
    None,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Speed {
    #[default]
    Low,
    Medium,
    High,
    VeryHigh,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinConfig {
    #[serde(flatten)]
    pub mode: PinMode,
    /// User label, used as the field name in generated code.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub pull: Pull,
    #[serde(default)]
    pub speed: Speed,
    #[serde(default)]
    pub initial_high: bool,
}

impl PinConfig {
    pub fn new(mode: PinMode) -> Self {
        Self {
            mode,
            label: String::new(),
            pull: Pull::None,
            speed: Speed::Low,
            initial_high: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeripheralConfig {
    pub enabled: bool,
    /// Free-form settings (baudrate, frequency...).
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodegenOptions {
    #[serde(default = "yes")]
    pub defmt: bool,
    #[serde(default = "yes")]
    pub vscode: bool,
}

fn yes() -> bool {
    true
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self { defmt: true, vscode: true }
    }
}

impl Project {
    pub fn new(name: &str, chip: &str, package: &str) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: name.to_string(),
            chip: chip.to_string(),
            package: package.to_string(),
            clock: ClockConfig::default(),
            pins: BTreeMap::new(),
            peripherals: BTreeMap::new(),
            codegen: CodegenOptions::default(),
            schematic: SchematicConfig::default(),
        }
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(PROJECT_FILE);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("{} has no {PROJECT_FILE}; is it an Embassy Studio project?", dir.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(dir.join(PROJECT_FILE), text)?;
        Ok(())
    }

    pub fn is_peripheral_enabled(&self, name: &str) -> bool {
        self.peripherals.get(name).is_some_and(|p| p.enabled)
    }

    pub fn set_peripheral_enabled(&mut self, name: &str, enabled: bool) {
        self.peripherals.entry(name.to_string()).or_default().enabled = enabled;
        if !enabled {
            self.pins.retain(|_, p| p.mode.peripheral() != Some(name));
        }
    }

    /// Pins currently routed to a peripheral, as (pin, signal).
    pub fn peripheral_pins(&self, peripheral: &str) -> Vec<(String, String)> {
        self.pins
            .iter()
            .filter_map(|(pin, cfg)| match &cfg.mode {
                PinMode::Peripheral { peripheral: p, signal, .. } if p == peripheral => {
                    Some((pin.clone(), signal.clone()))
                }
                _ => None,
            })
            .collect()
    }

    pub fn setting(&self, peripheral: &str, key: &str) -> Option<&str> {
        self.peripherals
            .get(peripheral)
            .and_then(|p| p.settings.get(key))
            .map(String::as_str)
    }
}

// ---------------------------------------------------------------------------
// Recent projects
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentProject {
    pub name: String,
    pub path: PathBuf,
    pub chip: String,
    pub last_opened: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub recent: Vec<RecentProject>,
    #[serde(default)]
    pub data_source: crate::db::DataSource,
    #[serde(default)]
    pub last_location: Option<PathBuf>,
}

impl Settings {
    fn path() -> PathBuf {
        crate::db::app_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            std::fs::write(path, text).ok();
        }
    }

    pub fn touch_recent(&mut self, project: &Project, dir: &Path) {
        self.recent.retain(|r| r.path != dir);
        self.recent.insert(
            0,
            RecentProject {
                name: project.name.clone(),
                path: dir.to_path_buf(),
                chip: project.chip.clone(),
                last_opened: now_unix(),
            },
        );
        self.recent.truncate(20);
        self.save();
    }
}

/// Turn a free-form project name into a valid Cargo package name.
pub fn crate_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    match out.chars().next() {
        None => "firmware".into(),
        Some(c) if c.is_ascii_digit() => format!("fw-{out}"),
        _ => out,
    }
}

/// Turn a label into a valid Rust identifier (snake case).
pub fn ident(label: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in label.trim().chars() {
        if c.is_ascii_alphanumeric() {
            // camelCase -> camel_case; acronyms like LED or USART1 stay together.
            if c.is_ascii_uppercase() && prev_lower {
                out.push('_');
            }
            prev_lower = c.is_ascii_lowercase();
            out.push(c.to_ascii_lowercase());
        } else {
            prev_lower = false;
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
        }
    }
    let out = out.trim_end_matches('_').to_string();
    match out.chars().next() {
        None => "pin".into(),
        Some(c) if c.is_ascii_digit() => format!("_{out}"),
        _ if matches!(out.as_str(), "type" | "match" | "loop" | "move" | "ref" | "self" | "mod" | "fn" | "use" | "in" | "as" | "impl") => {
            format!("{out}_")
        }
        _ => out,
    }
}
