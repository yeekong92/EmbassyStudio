//! Chip database access.
//!
//! Nothing about chips is hardcoded. STM32 chip lists, pinouts, peripherals and RCC
//! register layouts come from the generated JSON published by the stm32-data project
//! (`embassy-rs/stm32-data-generated`), or from a local checkout of it. ESP32 chips come
//! from esp-hal's `esp-metadata` device files (see [`crate::esp`]) and are converted to
//! the same [`Chip`] model. Downloaded files are cached on disk so the app works offline
//! after the first sync.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const UPSTREAM_REPO: &str = "embassy-rs/stm32-data-generated";
pub const UPSTREAM_BRANCH: &str = "main";
const USER_AGENT: &str = concat!("embassy-studio/", env!("CARGO_PKG_VERSION"));

/// Where chip data is read from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum DataSource {
    /// Download from GitHub on demand, caching locally.
    GitHub,
    /// A local clone of `stm32-data-generated` (or a `stm32-data` checkout with a `build/data` folder).
    Local(PathBuf),
}

impl Default for DataSource {
    fn default() -> Self {
        Self::GitHub
    }
}

impl DataSource {
    pub fn describe(&self) -> String {
        match self {
            Self::GitHub => format!("github.com/{UPSTREAM_REPO}"),
            Self::Local(p) => p.display().to_string(),
        }
    }

    /// Resolve the `data` directory (the one holding `chips/` and `registers/`) of a local checkout.
    fn local_data_dir(root: &Path) -> Result<PathBuf> {
        for candidate in [root.join("data"), root.join("build").join("data"), root.to_path_buf()] {
            if candidate.join("chips").is_dir() {
                return Ok(candidate);
            }
        }
        bail!(
            "{} does not look like an stm32-data checkout (no data/chips or build/data/chips folder)",
            root.display()
        )
    }
}

pub fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("EmbassyStudio")
}

fn cache_dir() -> PathBuf {
    app_dir().join("stm32-data")
}

pub(crate) fn http_get(url: &str) -> Result<String> {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder().tls_config(tls).build().into()
    });
    let mut res = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let body = res
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    Ok(body)
}

fn raw_url(path: &str) -> String {
    format!("https://raw.githubusercontent.com/{UPSTREAM_REPO}/{UPSTREAM_BRANCH}/data/{path}")
}

/// Read a data file (`chips/X.json`, `registers/Y.json`) through the cache.
fn read_data_file(source: &DataSource, rel: &str, refresh: bool) -> Result<String> {
    match source {
        DataSource::Local(root) => {
            let path = DataSource::local_data_dir(root)?.join(rel);
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
        }
        DataSource::GitHub => {
            let path = cache_dir().join(rel);
            if !refresh {
                if let Ok(s) = std::fs::read_to_string(&path) {
                    return Ok(s);
                }
            }
            let body = http_get(&raw_url(rel))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&path, &body).ok();
            Ok(body)
        }
    }
}

// ---------------------------------------------------------------------------
// Chip index
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChipIndex {
    pub source: String,
    pub fetched_unix: u64,
    pub chips: Vec<String>,
    /// Where the ESP32 part of `chips` came from; `None` for an index written before
    /// ESP32 support (or when the esp-hal metadata could not be reached).
    #[serde(default)]
    pub esp_source: Option<String>,
}

impl ChipIndex {
    fn cache_path() -> PathBuf {
        cache_dir().join("index.json")
    }

    pub fn load_cached() -> Option<Self> {
        let s = std::fs::read_to_string(Self::cache_path()).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Build the chip index from the data source. For GitHub this walks the git tree
    /// (`data/chips`) with the GitHub API, which lists every chip in three requests.
    pub fn fetch(source: &DataSource) -> Result<Self> {
        let mut chips = match source {
            DataSource::Local(root) => {
                let dir = DataSource::local_data_dir(root)?.join("chips");
                let mut out = Vec::new();
                for entry in std::fs::read_dir(&dir)? {
                    let name = entry?.file_name().to_string_lossy().to_string();
                    if let Some(chip) = name.strip_suffix(".json") {
                        out.push(chip.to_string());
                    }
                }
                out
            }
            DataSource::GitHub => {
                let tree = |sha: &str| -> Result<Vec<Value>> {
                    let url = format!("https://api.github.com/repos/{UPSTREAM_REPO}/git/trees/{sha}");
                    let v: Value = serde_json::from_str(&http_get(&url)?)?;
                    v["tree"]
                        .as_array()
                        .cloned()
                        .ok_or_else(|| anyhow!("unexpected GitHub API response from {url}"))
                };
                let find = |entries: &[Value], name: &str| -> Result<String> {
                    entries
                        .iter()
                        .find(|e| e["path"] == name)
                        .and_then(|e| e["sha"].as_str())
                        .map(str::to_string)
                        .ok_or_else(|| anyhow!("'{name}' not found in {UPSTREAM_REPO}"))
                };
                let root = tree(UPSTREAM_BRANCH)?;
                let data = tree(&find(&root, "data")?)?;
                let chips = tree(&find(&data, "chips")?)?;
                chips
                    .iter()
                    .filter_map(|e| e["path"].as_str()?.strip_suffix(".json").map(str::to_string))
                    .collect()
            }
        };
        // ESP32 chips are listed from esp-hal's metadata. Without network access the
        // STM32 list still loads; the ESP32 part falls back to the previous index.
        let esp_source = match crate::esp::list_chips() {
            Ok(esp) => {
                chips.extend(esp);
                Some(crate::esp::describe_source())
            }
            Err(_) => {
                let old = Self::load_cached().filter(|i| i.esp_source.is_some());
                let cached: Vec<String> = old
                    .as_ref()
                    .map(|i| i.chips.iter().filter(|c| Platform::of(c) == Platform::Esp).cloned().collect())
                    .unwrap_or_default();
                chips.extend(cached);
                old.and_then(|i| i.esp_source)
            }
        };
        chips.sort();
        chips.dedup();
        let index = Self {
            source: source.describe(),
            fetched_unix: crate::project::now_unix(),
            chips,
            esp_source,
        };
        if *source == DataSource::GitHub || index.esp_source.is_some() {
            std::fs::create_dir_all(cache_dir()).ok();
            std::fs::write(Self::cache_path(), serde_json::to_string(&index)?).ok();
        }
        Ok(index)
    }
}

/// Chip vendor / code generation target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Platform {
    /// STM32 (stm32-data, embassy-stm32).
    #[default]
    Stm32,
    /// Espressif ESP32 family (esp-metadata, esp-hal).
    Esp,
}

impl Platform {
    /// Platform of a chip name as it appears in the chip index.
    pub fn of(chip: &str) -> Self {
        if chip.to_ascii_uppercase().starts_with("ESP") { Self::Esp } else { Self::Stm32 }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Stm32 => "STM32",
            Self::Esp => "ESP32",
        }
    }
}

/// The family prefix of a chip name, e.g. `STM32F411CE` -> `STM32F4`, `ESP32C3` -> `ESP32-C3`.
pub fn family_of(chip: &str) -> std::borrow::Cow<'_, str> {
    if Platform::of(chip) == Platform::Esp {
        return crate::esp::display_name(chip).into();
    }
    let end = chip
        .char_indices()
        .skip(5)
        .find(|(_, c)| c.is_ascii_digit())
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(chip.len());
    chip[..end.min(chip.len())].into()
}

// ---------------------------------------------------------------------------
// Chip data (subset of the stm32-data schema we use)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
pub struct Chip {
    pub name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub line: String,
    #[serde(default)]
    pub packages: Vec<Package>,
    #[serde(default)]
    pub(crate) memory: Value,
    #[serde(default)]
    pub docs: Vec<Doc>,
    #[serde(default)]
    pub cores: Vec<Core>,
    /// Not part of the stm32-data schema: set by the loader.
    #[serde(skip)]
    pub platform: Platform,
    /// ESP32-only details (target, CPU clocks, pin restrictions).
    #[serde(skip)]
    pub esp: Option<crate::esp::EspChip>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub package: String,
    #[serde(default)]
    pub pins: Vec<PackagePin>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackagePin {
    pub position: String,
    #[serde(default)]
    pub signals: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct MemoryRegion {
    pub name: String,
    pub kind: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Doc {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Core {
    pub name: String,
    #[serde(default)]
    pub peripherals: Vec<Peripheral>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Peripheral {
    pub name: String,
    #[serde(default)]
    pub address: u64,
    pub registers: Option<RegRef>,
    pub rcc: Option<PeriphRcc>,
    #[serde(default)]
    pub pins: Vec<PeriphPin>,
    #[serde(default)]
    pub interrupts: Vec<PeriphInterrupt>,
    #[serde(default)]
    pub dma_channels: Vec<Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RegRef {
    pub kind: String,
    pub version: String,
    pub block: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PeriphRcc {
    pub bus_clock: Option<String>,
    pub kernel_clock: Option<KernelClock>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum KernelClock {
    Clock(String),
    Mux(FieldRef),
}

#[derive(Clone, Debug, Deserialize)]
pub struct FieldRef {
    pub register: String,
    pub field: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PeriphPin {
    pub pin: String,
    pub signal: String,
    pub af: Option<u8>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PeriphInterrupt {
    pub signal: String,
    pub interrupt: String,
}

impl Chip {
    pub fn load(source: &DataSource, name: &str, refresh: bool) -> Result<Self> {
        if Platform::of(name) == Platform::Esp {
            return crate::esp::load_chip(name, refresh);
        }
        let text = read_data_file(source, &format!("chips/{name}.json"), refresh)?;
        serde_json::from_str(&text).with_context(|| format!("parsing chip {name}"))
    }

    /// Memory regions, flattened across banks.
    pub fn memory(&self) -> Vec<MemoryRegion> {
        fn walk(v: &Value, out: &mut Vec<MemoryRegion>) {
            match v {
                Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
                Value::Object(_) => {
                    if let Ok(r) = serde_json::from_value(v.clone()) {
                        out.push(r);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.memory, &mut out);
        out
    }

    pub fn flash_size(&self) -> u64 {
        self.memory()
            .iter()
            .filter(|m| m.kind == "flash" && m.name.starts_with("BANK"))
            .map(|m| m.size)
            .sum()
    }

    pub fn ram_size(&self) -> u64 {
        self.memory().iter().filter(|m| m.kind == "ram").map(|m| m.size).sum()
    }

    pub fn core(&self) -> Option<&Core> {
        self.cores.first()
    }

    pub fn core_name(&self) -> &str {
        self.core().map(|c| c.name.as_str()).unwrap_or("?")
    }

    pub fn peripherals(&self) -> &[Peripheral] {
        self.core().map(|c| c.peripherals.as_slice()).unwrap_or(&[])
    }

    pub fn peripheral(&self, name: &str) -> Option<&Peripheral> {
        self.peripherals().iter().find(|p| p.name == name)
    }

    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// RCC register block reference (kind = "rcc").
    pub fn rcc_registers(&self) -> Option<&RegRef> {
        self.peripherals()
            .iter()
            .find(|p| p.registers.as_ref().is_some_and(|r| r.kind == "rcc"))
            .and_then(|p| p.registers.as_ref())
    }

    /// For every GPIO pin name, the peripheral signals that can be routed to it.
    pub fn pin_functions(&self) -> BTreeMap<String, Vec<PinFunction>> {
        let mut map: BTreeMap<String, Vec<PinFunction>> = BTreeMap::new();
        for p in self.peripherals() {
            for pin in &p.pins {
                map.entry(pin.pin.clone()).or_default().push(PinFunction {
                    peripheral: p.name.clone(),
                    signal: pin.signal.clone(),
                    af: pin.af,
                });
            }
        }
        for v in map.values_mut() {
            v.sort_by(|a, b| a.peripheral.cmp(&b.peripheral).then(a.signal.cmp(&b.signal)));
            v.dedup_by(|a, b| a.peripheral == b.peripheral && a.signal == b.signal);
        }
        map
    }
}

#[derive(Clone, Debug)]
pub struct PinFunction {
    pub peripheral: String,
    pub signal: String,
    pub af: Option<u8>,
}

impl PinFunction {
    pub fn label(&self) -> String {
        format!("{}_{}", self.peripheral, self.signal)
    }
}

/// Normalise a package signal name to a GPIO pin name, e.g. `PC14-OSC32_IN` -> `PC14`.
/// ESP32 pins are named `GPIO<n>` (as in esp-hal's `Peripherals`).
pub fn gpio_name(signal: &str) -> Option<String> {
    let s: String = signal
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if let Some(n) = s.strip_prefix("GPIO") {
        return (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then_some(s);
    }
    let mut chars = s.chars();
    let is_gpio = chars.next() == Some('P')
        && chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && s.len() > 2
        && s[2..].chars().all(|c| c.is_ascii_digit());
    is_gpio.then_some(s)
}

// ---------------------------------------------------------------------------
// Register files (registers/<kind>_<version>.json)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct RegisterFile {
    /// Registers of the main block, in address order.
    pub registers: Vec<BlockItem>,
    pub fieldsets: BTreeMap<String, Fieldset>,
    pub enums: BTreeMap<String, EnumDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BlockItem {
    pub name: String,
    #[serde(default)]
    pub fieldset: Option<String>,
    /// Register arrays (e.g. H7 `PLLDIVR` x3): `{"len": 3, "stride": 8}`.
    #[serde(default)]
    pub array: Option<Value>,
}

impl BlockItem {
    /// Number of registers in an array item (1 for plain registers).
    pub fn array_len(&self) -> usize {
        self.array
            .as_ref()
            .and_then(|a| a.get("len"))
            .and_then(Value::as_u64)
            .map(|n| n.max(1) as usize)
            .unwrap_or(1)
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Fieldset {
    #[serde(default)]
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(rename = "enum", default)]
    pub enum_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct EnumDef {
    #[serde(default)]
    pub variants: Vec<EnumVariant>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EnumVariant {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub value: u64,
}

impl RegisterFile {
    pub fn load(source: &DataSource, kind: &str, version: &str, block: &str, refresh: bool) -> Result<Self> {
        let rel = format!("registers/{kind}_{version}.json");
        let text = read_data_file(source, &rel, refresh)?;
        let raw: BTreeMap<String, Value> =
            serde_json::from_str(&text).with_context(|| format!("parsing {rel}"))?;
        let mut file = RegisterFile::default();
        for (key, value) in raw {
            if let Some(name) = key.strip_prefix("fieldset/") {
                file.fieldsets.insert(name.to_string(), serde_json::from_value(value)?);
            } else if let Some(name) = key.strip_prefix("enum/") {
                file.enums.insert(name.to_string(), serde_json::from_value(value)?);
            } else if key == format!("block/{block}") {
                file.registers = serde_json::from_value(value["items"].clone()).unwrap_or_default();
            }
        }
        Ok(file)
    }

    /// Iterate `(register, field)` in register order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &Field)> {
        self.registers.iter().flat_map(move |r| {
            let fs = r.fieldset.as_deref().and_then(|f| self.fieldsets.get(f));
            fs.into_iter()
                .flat_map(|fs| fs.fields.iter())
                .map(move |f| (r.name.as_str(), f))
        })
    }
}
