//! Clock tree model derived from the RCC register description in stm32-data.
//!
//! Rather than encoding each family's clock tree by hand, the tree is discovered
//! from the RCC fieldsets and enums: the system clock switch (`SW`), PLL source
//! (`PLLSRC`), PLL dividers/multipliers (`PLLM`/`PLLN`/`PLLP`/...), and bus
//! prescalers (`HPRE`, `PPRE*`). Enum variant names like `Div4`, `Mul168` or
//! `PLL1_P` give both the choices and their numeric meaning.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::db::{EnumVariant, RegisterFile};

/// One configurable selector in the clock tree: a register field backed by an enum.
#[derive(Clone, Debug)]
pub struct Selector {
    pub register: String,
    pub field: String,
    pub enum_name: String,
    pub variants: Vec<EnumVariant>,
}

impl Selector {
    fn from_field(rcc: &RegisterFile, register: &str, field: &crate::db::Field) -> Option<Self> {
        let enum_name = field.enum_name.clone()?;
        let variants = rcc.enums.get(&enum_name)?.variants.clone();
        if variants.is_empty() {
            return None;
        }
        Some(Self {
            register: register.to_string(),
            field: field.name.clone(),
            enum_name,
            variants,
        })
    }

    pub fn key(&self) -> String {
        format!("{}.{}", self.register, self.field)
    }

    pub fn has(&self, variant: &str) -> bool {
        self.variants.iter().any(|v| v.name == variant)
    }

    /// Default choice: the reset value (0) if present, else the first variant.
    pub fn default_variant(&self) -> &str {
        self.variants
            .iter()
            .find(|v| v.value == 0)
            .or(self.variants.first())
            .map(|v| v.name.as_str())
            .unwrap_or("")
    }
}

/// Clock tree topology discovered for one RCC version.
#[derive(Clone, Debug, Default)]
pub struct ClockTree {
    pub rcc_version: String,
    pub sys: Option<Selector>,
    pub pll_src: Option<Selector>,
    pub pll_m: Option<Selector>,
    pub pll_n: Option<Selector>,
    pub pll_p: Option<Selector>,
    pub pll_q: Option<Selector>,
    pub pll_r: Option<Selector>,
    pub ahb: Option<Selector>,
    pub apb: Vec<Selector>,
    /// Kernel clock muxes (e.g. `CCIPR.USART1SEL`), by `REGISTER.FIELD`.
    pub muxes: BTreeMap<String, Selector>,
    /// Oscillators referenced by the clock selectors (HSE, HSI, MSI, LSE, ...).
    pub oscillators: Vec<String>,
    /// Name of the main PLL as used in mux variants (`PLL1` or `PLL`).
    pub main_pll: String,
    /// Additional PLLs (PLLSAI1/2, PLLI2S, PLL2/3, ...).
    pub aux_plls: Vec<PllUnit>,
    /// Oscillators whose frequency is a selectable range (MSI: `MSIRANGE`).
    pub osc_range: BTreeMap<String, Selector>,
    /// Oscillators with an output divider (H7 HSI: `HSIDIV`).
    pub osc_div: BTreeMap<String, Selector>,
    /// LSE drive strength.
    pub lse_drive: Option<Selector>,
    /// Microcontroller clock outputs: (source mux, prescaler).
    pub mcos: Vec<(Selector, Option<Selector>)>,
    /// Other RCC choices shown in the tree (wake-up clock, timer prescaler, ...).
    pub misc: Vec<Selector>,
}

/// One PLL discovered from the RCC registers.
#[derive(Clone, Debug)]
pub struct PllUnit {
    /// Name used in mux variants, e.g. `PLLSAI1`, `PLLI2S`, `PLL2`.
    pub name: String,
    /// Own input mux; None = shares the main PLL source.
    pub src: Option<Selector>,
    /// Own input divider; None = shares the main PLL /M.
    pub m: Option<Selector>,
    pub n: Selector,
    pub p: Option<Selector>,
    pub q: Option<Selector>,
    pub r: Option<Selector>,
}

impl PllUnit {
    pub fn output(&self, letter: char) -> Option<&Selector> {
        match letter {
            'P' => self.p.as_ref(),
            'Q' => self.q.as_ref(),
            'R' => self.r.as_ref(),
            _ => None,
        }
    }
}

/// `PLLSAI1CFGR` -> `PLLSAI1`, `PLL2DIVR` -> `PLL2`, `PLLCFGR` -> `PLL`.
fn pll_group(register: &str) -> Option<String> {
    if !register.starts_with("PLL") {
        return None;
    }
    ["CKSELR", "CFGR", "DIVR", "FRACR"]
        .iter()
        .find_map(|s| register.strip_suffix(s))
        .map(str::to_string)
}

/// Role of a field inside a PLL register: M, N, P, Q, R or S(ource).
fn pll_role(field: &str) -> Option<char> {
    Some(match field {
        "PLLM" | "DIVM" => 'M',
        "PLLN" | "DIVN" | "PLLMUL" => 'N',
        "PLLP" | "DIVP" => 'P',
        "PLLQ" | "DIVQ" => 'Q',
        "PLLR" | "DIVR" => 'R',
        "PLLSRC" => 'S',
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    Sys,
    PllSrc,
    PllM,
    PllN,
    PllP,
    PllQ,
    PllR,
    Ahb,
}

impl ClockTree {
    pub fn from_rcc(rcc: &RegisterFile, version: &str) -> Self {
        let find = |names: &[&str]| -> Option<Selector> {
            names.iter().find_map(|n| {
                rcc.fields()
                    .find(|(_, f)| f.name == *n && f.enum_name.is_some())
                    .and_then(|(r, f)| Selector::from_field(rcc, r, f))
            })
        };
        let mut tree = ClockTree {
            rcc_version: version.to_string(),
            sys: find(&["SW"]),
            pll_src: find(&["PLLSRC", "PLL1SRC"]),
            pll_m: find(&["PLLM", "PLL1M", "DIVM", "DIVM1", "PREDIV", "PREDIV1"]),
            pll_n: find(&["PLLN", "PLL1N", "DIVN", "DIVN1", "PLLMUL"]),
            pll_p: find(&["PLLP", "PLL1P", "DIVP", "DIVP1"]),
            pll_q: find(&["PLLQ", "PLL1Q", "DIVQ", "DIVQ1"]),
            pll_r: find(&["PLLR", "PLL1R", "DIVR", "DIVR1"]),
            ahb: find(&["HPRE"]),
            ..Default::default()
        };

        let mut seen = std::collections::BTreeSet::new();
        for (reg, f) in rcc.fields() {
            let is_apb = f.name.contains("PPRE") && !f.name.starts_with("MCO");
            if is_apb && seen.insert(f.name.clone()) {
                if let Some(s) = Selector::from_field(rcc, reg, f) {
                    tree.apb.push(s);
                }
            }
            if f.name.ends_with("SEL") || f.name.ends_with("SRC") {
                if let Some(s) = Selector::from_field(rcc, reg, f) {
                    tree.muxes.insert(s.key(), s);
                }
            }
        }

        let mut osc = std::collections::BTreeSet::new();
        let mut add = |s: Option<&Selector>| {
            for v in s.iter().flat_map(|s| s.variants.iter()) {
                if let Some(base) = oscillator_base(&v.name) {
                    osc.insert(base);
                }
            }
        };
        add(tree.sys.as_ref());
        add(tree.pll_src.as_ref());
        add(find(&["RTCSEL"]).as_ref());
        // Kernel muxes and clock outputs can reach oscillators nothing else uses (HSI48, LSI...).
        for s in tree.muxes.values() {
            add(Some(s));
        }
        // Put HSE first, then the rest in a stable order.
        let mut list: Vec<String> = osc.into_iter().collect();
        list.sort_by_key(|o| (!o.starts_with("HSE"), !o.starts_with("HS"), o.clone()));
        tree.oscillators = list;

        tree.discover_plls(rcc);
        for o in &tree.oscillators {
            if let Some(s) = find(&[&format!("{o}RANGE")]) {
                tree.osc_range.insert(o.clone(), s);
            }
            if let Some(s) = find(&[&format!("{o}DIV")]) {
                tree.osc_div.insert(o.clone(), s);
            }
        }
        tree.lse_drive = find(&["LSEDRV"]);
        for (sel_name, pre_name) in [("MCOSEL", "MCOPRE"), ("MCO1SEL", "MCO1PRE"), ("MCO2SEL", "MCO2PRE")] {
            if let Some(sel) = find(&[sel_name]) {
                tree.mcos.push((sel, find(&[pre_name])));
            }
        }
        for name in ["STOPWUCK", "STOPKERWUCK", "TIMPRE", "LSIPREDIV"] {
            if let Some(s) = find(&[name]) {
                tree.misc.push(s);
            }
        }
        tree
    }

    /// Group PLL registers by PLL and split them into the main PLL and the others.
    fn discover_plls(&mut self, rcc: &RegisterFile) {
        // group -> role -> selector, plus whether the group came from a register array.
        let mut groups: BTreeMap<String, (BTreeMap<char, Selector>, bool)> = BTreeMap::new();
        for item in &rcc.registers {
            let Some(group) = pll_group(&item.name) else { continue };
            let Some(fs) = item.fieldset.as_deref().and_then(|f| rcc.fieldsets.get(f)) else { continue };
            let len = item.array_len();
            for f in &fs.fields {
                let Some(role) = pll_role(&f.name) else { continue };
                let Some(sel) = Selector::from_field(rcc, &item.name, f) else { continue };
                for idx in 0..len {
                    let mut s = sel.clone();
                    let name = if len > 1 {
                        s.register = format!("{}{}", item.name, idx + 1);
                        format!("{group}{}", idx + 1)
                    } else {
                        group.clone()
                    };
                    let entry = groups.entry(name).or_insert_with(|| (BTreeMap::new(), len > 1));
                    entry.0.entry(role).or_insert(s);
                }
            }
        }
        let main_reg = self.pll_n.as_ref().map(|s| s.register.clone()).unwrap_or_default();
        let shared_m = groups.get("PLL").and_then(|(g, _)| g.get(&'M').cloned());
        for (name, (mut roles, from_array)) in groups {
            let Some(n) = roles.remove(&'N') else { continue };
            if n.register == main_reg || n.register == format!("{main_reg}1") {
                // Variants call the main PLL `PLL1` even when its registers say `PLL`.
                self.main_pll = if name.ends_with(|c: char| c.is_ascii_digit()) { name } else { format!("{name}1") };
                continue;
            }
            // H7-style arrays share one DIVM field per PLL, numbered by PLL.
            let m = roles.remove(&'M').or_else(|| {
                let digit = name.chars().last().filter(|c| c.is_ascii_digit())?;
                let mut m = shared_m.clone().filter(|_| from_array)?;
                m.field = format!("{}{digit}", m.field);
                Some(m)
            });
            self.aux_plls.push(PllUnit {
                name,
                src: roles.remove(&'S'),
                m,
                n,
                p: roles.remove(&'P'),
                q: roles.remove(&'Q'),
                r: roles.remove(&'R'),
            });
        }
        if self.main_pll.is_empty() {
            self.main_pll = "PLL1".into();
        }
    }

    /// Kernel-clock muxes that are not part of the core tree (PLL source, MCO, RTC...).
    pub fn is_system_mux(key: &str) -> bool {
        let field = key.rsplit('.').next().unwrap_or(key);
        field.starts_with("PLL")
            || field.starts_with("MCO")
            || field.starts_with("MSI")
            || matches!(field, "RTCSEL" | "LSCOSEL" | "SW")
    }

    pub fn selector(&self, role: Role) -> Option<&Selector> {
        match role {
            Role::Sys => self.sys.as_ref(),
            Role::PllSrc => self.pll_src.as_ref(),
            Role::PllM => self.pll_m.as_ref(),
            Role::PllN => self.pll_n.as_ref(),
            Role::PllP => self.pll_p.as_ref(),
            Role::PllQ => self.pll_q.as_ref(),
            Role::PllR => self.pll_r.as_ref(),
            Role::Ahb => self.ahb.as_ref(),
        }
    }

    pub fn has_pll(&self) -> bool {
        self.pll_n.is_some()
    }
}

/// If a variant names an oscillator (e.g. `HSE`, `HSI_DIV2`, `LSE`), return its base name.
pub fn oscillator_base(variant: &str) -> Option<String> {
    let base = variant.split('_').next().unwrap_or(variant);
    let base = base.trim_end_matches("SYS");
    let known_prefix = ["HSE", "HSI", "LSE", "LSI", "MSI", "CSI"];
    known_prefix
        .iter()
        .any(|p| base.starts_with(p))
        .then(|| base.to_string())
}

/// Numeric meaning of a divider/multiplier variant name (`Div4`, `DIV1_5`, `Mul168`).
pub fn factor(variant: &str) -> Option<f64> {
    let lower = variant.to_ascii_lowercase();
    let (num, mul) = if let Some(n) = lower.strip_prefix("div") {
        (n, false)
    } else if let Some(n) = lower.strip_prefix("mul") {
        (n, true)
    } else if let Some(n) = lower.strip_prefix("prediv") {
        (n, false)
    } else {
        return None;
    };
    let num = num.trim_start_matches('_').replace('_', ".");
    let v: f64 = num.parse().ok()?;
    if v == 0.0 {
        return None;
    }
    Some(if mul { v } else { 1.0 / v })
}

/// User clock configuration, stored in the project file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClockConfig {
    /// Oscillator frequencies in Hz (user editable, e.g. the crystal on the board).
    #[serde(default)]
    pub oscillators: BTreeMap<String, f64>,
    /// Enabled external oscillators (HSE, LSE).
    #[serde(default)]
    pub enabled: BTreeMap<String, bool>,
    #[serde(default)]
    pub hse_bypass: bool,
    #[serde(default)]
    pub pll_enabled: bool,
    /// Selected variants per role.
    #[serde(default)]
    pub roles: BTreeMap<Role, String>,
    /// Selected APB prescalers by field name.
    #[serde(default)]
    pub apb: BTreeMap<String, String>,
    /// Selected kernel clock mux variants by `REGISTER.FIELD`.
    #[serde(default)]
    pub muxes: BTreeMap<String, String>,
    /// PLL P/Q/R outputs the user switched on (the one feeding SYSCLK is always on).
    #[serde(default)]
    pub pll_outputs: BTreeMap<Role, bool>,
    /// Other selector values by `REGISTER.FIELD`: extra PLLs, MSI range, MCO prescaler, ...
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
    /// Extra PLLs switched on, by name (`PLLSAI1`).
    #[serde(default)]
    pub aux_pll_enabled: BTreeMap<String, bool>,
    /// Cortex SysTick fed by HCLK/8 instead of HCLK.
    #[serde(default)]
    pub systick_div8: bool,
}

/// Fields stored in `ClockConfig::muxes` (the rest go to `extra`), mirroring `ClockTree::from_rcc`.
fn is_mux_field(field: &str) -> bool {
    field.ends_with("SEL") || field.ends_with("SRC")
}

/// Frequency named by an oscillator range variant: `Range4M` -> 4 MHz, `Range100K` -> 100 kHz.
pub fn range_frequency(variant: &str) -> Option<f64> {
    let upper = variant.to_ascii_uppercase();
    let start = upper.find(|c: char| c.is_ascii_digit())?;
    let rest = &upper[start..];
    let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '_' || c == '.')).unwrap_or(rest.len());
    let value: f64 = rest[..num_len].trim_end_matches('_').replace('_', ".").parse().ok()?;
    let unit = match rest[num_len..].chars().next() {
        Some('M') => 1e6,
        Some('K') => 1e3,
        _ => 1.0,
    };
    Some(value * unit)
}

/// Nominal oscillator frequency used to seed a new configuration.
/// Users edit these to match their board / the datasheet.
/// `rcc_version` is the stm32-data RCC version (`f1`, `l4plus`, `h7rm0433`, ...):
/// the internal oscillators differ between families.
pub fn nominal_frequency(osc: &str, rcc_version: &str) -> f64 {
    let v = rcc_version.to_ascii_lowercase();
    let old_f = ["f0", "f1", "f3"].iter().any(|f| v.starts_with(f));
    match osc {
        "HSE" => 8_000_000.0,
        "HSI48" => 48_000_000.0,
        // HSI is 8 MHz on F0/F1/F3, 64 MHz before HSIDIV on H7/H5, 48 MHz on C0.
        "HSI" if old_f => 8_000_000.0,
        "HSI" if v.starts_with("h7") || v.starts_with("h5") => 64_000_000.0,
        "HSI" if v.starts_with("c0") => 48_000_000.0,
        "HSI" => 16_000_000.0,
        "MSI" | "MSIS" | "MSIK" | "CSI" => 4_000_000.0,
        "LSE" => 32_768.0,
        "LSI" if old_f => 40_000.0,
        "LSI" => 32_000.0,
        _ => 0.0,
    }
}

impl ClockConfig {
    /// Fill in anything missing for the given tree (new project, or after a data update).
    pub fn normalize(&mut self, tree: &ClockTree) {
        for osc in &tree.oscillators {
            self.oscillators
                .entry(osc.clone())
                .or_insert_with(|| nominal_frequency(osc, &tree.rcc_version));
            let external = osc.starts_with("HSE") || osc.starts_with("LSE");
            self.enabled.entry(osc.clone()).or_insert(!external);
        }
        for role in [Role::Sys, Role::PllSrc, Role::PllM, Role::PllN, Role::PllP, Role::PllQ, Role::PllR, Role::Ahb] {
            if let Some(sel) = tree.selector(role) {
                let valid = self.roles.get(&role).is_some_and(|v| sel.has(v));
                if !valid {
                    // A PLL source of "DISABLE" is a useless default; prefer a real oscillator.
                    let default = match role {
                        Role::PllSrc => sel
                            .variants
                            .iter()
                            .find(|v| oscillator_base(&v.name).is_some())
                            .map(|v| v.name.as_str())
                            .unwrap_or(sel.default_variant()),
                        _ => sel.default_variant(),
                    };
                    self.roles.insert(role, default.to_string());
                }
            }
        }
        for sel in &tree.apb {
            let valid = self.apb.get(&sel.field).is_some_and(|v| sel.has(v));
            if !valid {
                self.apb.insert(sel.field.clone(), sel.default_variant().to_string());
            }
        }
        // Range-selected oscillators (MSI): variant 0 is the lowest range, not the
        // reset one, so start from the range matching the configured frequency (4 MHz).
        for (osc, sel) in &tree.osc_range {
            if self.extra.get(&sel.key()).is_some_and(|v| sel.has(v)) {
                continue;
            }
            let want = self.oscillators.get(osc).copied().unwrap_or_else(|| nominal_frequency(osc, &tree.rcc_version));
            if let Some(v) = sel.variants.iter().find(|v| range_frequency(&v.name) == Some(want)) {
                self.extra.insert(sel.key(), v.name.clone());
            }
        }
    }

    pub fn role(&self, role: Role) -> Option<&str> {
        self.roles.get(&role).map(String::as_str)
    }

    /// Current value of any selector (kernel mux or other RCC field), or its reset default.
    pub fn value<'a>(&'a self, sel: &'a Selector) -> &'a str {
        let key = sel.key();
        let stored = if is_mux_field(&sel.field) { self.muxes.get(&key) } else { self.extra.get(&key) };
        stored.map(String::as_str).filter(|v| sel.has(v)).unwrap_or(sel.default_variant())
    }

    pub fn set_value(&mut self, sel: &Selector, value: String) {
        let map = if is_mux_field(&sel.field) { &mut self.muxes } else { &mut self.extra };
        map.insert(sel.key(), value);
    }

    pub fn aux_enabled(&self, name: &str) -> bool {
        self.aux_pll_enabled.get(name).copied().unwrap_or(false)
    }

    /// Whether a PLL output divider is active: user-enabled, feeding SYSCLK, or
    /// selected by a kernel clock mux (e.g. `CLK48SEL = PLL1_Q`).
    pub fn pll_output_enabled(&self, role: Role) -> bool {
        let letter = match role {
            Role::PllP => "P",
            Role::PllQ => "Q",
            Role::PllR => "R",
            _ => return false,
        };
        let sys = self.role(Role::Sys).unwrap_or("");
        let feeds_sys = sys.starts_with("PLL") && sys.rsplit('_').next() == Some(letter);
        let feeds_mux = self
            .muxes
            .values()
            .any(|v| (v.starts_with("PLL1_") || v.starts_with("PLL_")) && v.rsplit('_').next() == Some(letter));
        feeds_sys || feeds_mux || self.pll_outputs.get(&role).copied().unwrap_or(false)
    }

    pub fn is_enabled(&self, osc: &str) -> bool {
        self.enabled.get(osc).copied().unwrap_or(false)
    }

    /// Oscillator frequency as configured (range/divider applied), ignoring enable state.
    pub fn osc_frequency(&self, tree: &ClockTree, name: &str) -> f64 {
        let mut f = tree
            .osc_range
            .get(name)
            .and_then(|sel| range_frequency(self.value(sel)))
            .unwrap_or_else(|| self.oscillators.get(name).copied().unwrap_or(0.0));
        if let Some(d) = tree.osc_div.get(name).and_then(|sel| factor(self.value(sel))) {
            f *= d;
        }
        f
    }

    pub fn compute(&self, tree: &ClockTree) -> ClockResults {
        let mut r = ClockResults::default();
        let osc = |name: &str| -> f64 {
            if !self.is_enabled(name) {
                return 0.0;
            }
            self.osc_frequency(tree, name)
        };
        // Resolve a mux variant (HSE, HSI_DIV2, ...) to an oscillator frequency.
        let source = |variant: &str| -> f64 {
            let Some(base) = oscillator_base(variant) else {
                return 0.0;
            };
            let mut f = osc(&base);
            if let Some(idx) = variant.find("DIV") {
                if let Some(d) = factor(&variant[idx..]) {
                    f *= d;
                }
            }
            f
        };

        if self.pll_enabled && tree.has_pll() {
            let src = self.role(Role::PllSrc).map(source).unwrap_or(0.0);
            let m = self.role(Role::PllM).and_then(factor).unwrap_or(1.0);
            let n = self.role(Role::PllN).and_then(factor).unwrap_or(1.0);
            r.pll_in = src * m;
            r.vco = r.pll_in * n;
            let div = |role| self.role(role).and_then(factor).map(|d| r.vco * d);
            r.pll_p = tree.pll_p.as_ref().and(div(Role::PllP));
            r.pll_q = tree.pll_q.as_ref().and(div(Role::PllQ));
            r.pll_r = tree.pll_r.as_ref().and(div(Role::PllR));
            // Families without separate P/Q/R outputs (F0/F1/L0...) feed the VCO straight through.
            if tree.pll_p.is_none() && tree.pll_r.is_none() {
                r.pll_p = Some(r.vco);
            }
        }

        let sys_variant = self.role(Role::Sys).unwrap_or("");
        r.sysclk = if sys_variant.starts_with("PLL") {
            match sys_variant.rsplit('_').next() {
                Some("R") => r.pll_r.unwrap_or(0.0),
                Some("Q") => r.pll_q.unwrap_or(0.0),
                _ => r.pll_p.or(r.pll_r).unwrap_or(0.0),
            }
        } else {
            source(sys_variant)
        };

        r.hclk = r.sysclk * self.role(Role::Ahb).and_then(factor).unwrap_or(1.0);
        r.buses.insert("SYS".into(), r.sysclk);
        r.buses.insert("HCLK".into(), r.hclk);
        for i in 1..=4 {
            r.buses.insert(format!("HCLK{i}"), r.hclk);
        }
        for sel in &tree.apb {
            let f = r.hclk * self.apb.get(&sel.field).and_then(|v| factor(v)).unwrap_or(1.0);
            let bus = apb_bus_name(&sel.field);
            r.apb.push((bus.clone(), sel.field.clone(), f));
            r.buses.insert(bus, f);
        }
        if tree.apb.len() == 1 {
            r.buses.insert("PCLK1".into(), r.apb[0].2);
        }
        for o in &tree.oscillators {
            r.buses.insert(o.clone(), osc(o));
        }
        // Timer clocks run at 2x PCLK when the APB prescaler divides.
        for (bus, field, f) in r.apb.clone() {
            let div = self.apb.get(&field).and_then(|v| factor(v)).unwrap_or(1.0);
            let tim = if div < 1.0 { f * 2.0 } else { f };
            r.apb_timers.push((bus.clone(), tim));
            r.buses.insert(format!("{bus}_TIM"), tim);
        }

        // Main PLL outputs under the names mux variants use.
        let main = tree.main_pll.trim_end_matches('1').to_string();
        for (letter, f) in [('P', r.pll_p), ('Q', r.pll_q), ('R', r.pll_r)] {
            if let Some(f) = f.filter(|_| self.pll_enabled) {
                for name in [&tree.main_pll, &main, &format!("{main}1")] {
                    r.buses.insert(format!("{name}_{letter}"), f);
                }
            }
        }
        // "PLL" alone (MCO, F1 USB) means the output that can drive SYSCLK.
        if self.pll_enabled {
            if let Some(f) = r.pll_r.or(r.pll_p) {
                r.buses.insert("PLL".into(), f);
                r.buses.insert("PLLCLK".into(), f);
            }
        }

        // Extra PLLs.
        let main_src = self.role(Role::PllSrc).map(source).unwrap_or(0.0);
        let main_m = self.role(Role::PllM).and_then(factor).unwrap_or(1.0);
        for pll in &tree.aux_plls {
            let enabled = self.aux_enabled(&pll.name);
            let src = pll.src.as_ref().map(|s| source(self.value(s))).unwrap_or(main_src);
            let m = pll.m.as_ref().and_then(|s| factor(self.value(s))).unwrap_or(main_m);
            let n = factor(self.value(&pll.n)).unwrap_or(1.0);
            let pll_in = src * m;
            let vco = pll_in * n;
            let out = |sel: &Option<Selector>| sel.as_ref().and_then(|s| factor(self.value(s))).map(|d| vco * d);
            let res = PllResult { enabled, pll_in, vco, p: out(&pll.p), q: out(&pll.q), r: out(&pll.r) };
            if enabled {
                let alias = if pll.name.ends_with(|c: char| c.is_ascii_digit()) { None } else { Some(format!("{}1", pll.name)) };
                for (letter, f) in [('P', res.p), ('Q', res.q), ('R', res.r)] {
                    let Some(f) = f else { continue };
                    r.buses.insert(format!("{}_{letter}", pll.name), f);
                    if let Some(a) = &alias {
                        r.buses.insert(format!("{a}_{letter}"), f);
                    }
                }
            }
            r.aux.push(res);
        }

        // Kernel clock muxes and clock outputs.
        for (key, sel) in &tree.muxes {
            r.kernel.insert(key.clone(), r.resolve(self.value(sel)).unwrap_or(0.0));
        }
        for (sel, pre) in &tree.mcos {
            let f = r.resolve(self.value(sel)).unwrap_or(0.0);
            let d = pre.as_ref().and_then(|p| factor(self.value(p))).unwrap_or(1.0);
            r.mco.push(f * d);
        }
        r
    }
}

#[derive(Clone, Debug, Default)]
pub struct PllResult {
    pub enabled: bool,
    pub pll_in: f64,
    pub vco: f64,
    pub p: Option<f64>,
    pub q: Option<f64>,
    pub r: Option<f64>,
}

impl PllResult {
    pub fn output(&self, letter: char) -> Option<f64> {
        match letter {
            'P' => self.p,
            'Q' => self.q,
            'R' => self.r,
            _ => None,
        }
    }
}

/// Map a prescaler field to the bus clock name used by stm32-data (`PCLK1`, ...).
fn apb_bus_name(field: &str) -> String {
    // D1PPRE (H7: APB3), D2PPRE1/2 (APB1/2), D3PPRE (APB4), PPRE1/2/3, PPRE.
    match field {
        "D1PPRE" => "PCLK3".into(),
        "D3PPRE" => "PCLK4".into(),
        _ => match field.chars().last().filter(|c| c.is_ascii_digit()) {
            Some(d) => format!("PCLK{d}"),
            None => "PCLK1".into(),
        },
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClockResults {
    pub pll_in: f64,
    pub vco: f64,
    pub pll_p: Option<f64>,
    pub pll_q: Option<f64>,
    pub pll_r: Option<f64>,
    pub sysclk: f64,
    pub hclk: f64,
    /// (bus name, prescaler field, frequency)
    pub apb: Vec<(String, String, f64)>,
    /// (bus name, timer clock)
    pub apb_timers: Vec<(String, f64)>,
    pub buses: BTreeMap<String, f64>,
    /// Extra PLLs, in `ClockTree::aux_plls` order.
    pub aux: Vec<PllResult>,
    /// Output of every mux in `ClockTree::muxes`, by `REGISTER.FIELD`.
    pub kernel: BTreeMap<String, f64>,
    /// Clock outputs, in `ClockTree::mcos` order.
    pub mco: Vec<f64>,
}

impl ClockResults {
    pub fn bus(&self, name: &str) -> Option<f64> {
        self.buses.get(name).copied()
    }

    /// Frequency of a clock named by a mux variant: `PCLK1`, `SYS`, `HSI_DIV2`,
    /// `PLLSAI1_Q`, `DISABLE`... None when the name is not a clock we model
    /// (external pins such as `SAI1_EXTCLK`).
    pub fn resolve(&self, name: &str) -> Option<f64> {
        if name.starts_with("DISABLE") || matches!(name, "NONE" | "OFF") {
            return Some(0.0);
        }
        if let Some(f) = self.bus(name) {
            return Some(f);
        }
        match name {
            "SYS" | "SYSCLK" => return Some(self.sysclk),
            "HCLK" | "AHB" => return Some(self.hclk),
            _ => {}
        }
        if let Some(base) = oscillator_base(name) {
            let mut f = self.bus(&base)?;
            if let Some(idx) = name.find("DIV") {
                if let Some(d) = factor(&name[idx..]) {
                    f *= d;
                }
            }
            return Some(f);
        }
        // An inactive PLL output still names a clock: it is just off.
        if name.starts_with("PLL") {
            return Some(0.0);
        }
        self.bus(name.trim_end_matches("_P"))
    }
}

pub fn format_hz(hz: f64) -> String {
    if hz <= 0.0 {
        "—".into()
    } else if hz >= 1e6 {
        format!("{} MHz", trim(hz / 1e6))
    } else if hz >= 1e3 {
        format!("{} kHz", trim(hz / 1e3))
    } else {
        format!("{} Hz", trim(hz))
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discovery against RCC files in the local stm32-data cache (skipped when absent).
    fn tree(version: &str) -> Option<ClockTree> {
        let dir = crate::db::app_dir().join("stm32-data").join("registers").join(format!("rcc_{version}.json"));
        if !dir.exists() {
            eprintln!("skipping rcc_{version}: not cached");
            return None;
        }
        let rcc = RegisterFile::load(&crate::db::DataSource::GitHub, "rcc", version, "RCC", false).ok()?;
        Some(ClockTree::from_rcc(&rcc, version))
    }

    fn aux_names(t: &ClockTree) -> Vec<String> {
        t.aux_plls.iter().map(|p| p.name.clone()).collect()
    }

    #[test]
    fn discovers_all_plls_and_outputs() {
        for (version, aux) in [
            ("l4plus", vec!["PLLSAI1", "PLLSAI2"]),
            ("l4", vec!["PLLSAI1", "PLLSAI2"]),
            ("f4", vec!["PLLI2S", "PLLSAI"]),
            ("g4", vec![]),
            ("u5", vec!["PLL2", "PLL3"]),
            ("h7rm0433", vec!["PLL2", "PLL3"]),
            ("f1", vec![]),
        ] {
            let Some(t) = tree(version) else { continue };
            assert_eq!(aux_names(&t), aux, "aux PLLs of {version}");
            assert!(t.sys.is_some(), "{version}: SW");
            println!("{version}: main={} osc={:?} ranges={:?} mcos={} misc={} muxes={}", t.main_pll, t.oscillators, t.osc_range.keys().collect::<Vec<_>>(), t.mcos.len(), t.misc.len(), t.muxes.len());
        }
    }

    #[test]
    fn l4plus_frequencies() {
        let Some(t) = tree("l4plus") else { return };
        let mut c = ClockConfig::default();
        c.normalize(&t);
        // Reset state: MSI 4 MHz drives SYSCLK.
        let r = c.compute(&t);
        assert_eq!(r.sysclk, 4e6);
        assert_eq!(r.resolve("PCLK1_TIM"), Some(4e6));
        // MSI 48 MHz into PLLSAI1 (own /M on L4+) feeding CLK48SEL.
        let msi = &t.osc_range["MSI"];
        c.set_value(msi, "Range48M".into());
        let sai1 = t.aux_plls.iter().find(|p| p.name == "PLLSAI1").unwrap();
        c.roles.insert(Role::PllSrc, "MSI".into());
        c.roles.insert(Role::PllM, "Div6".into());
        c.set_value(sai1.m.as_ref().unwrap(), "Div6".into());
        c.set_value(&sai1.n, "Mul24".into());
        c.set_value(sai1.q.as_ref().unwrap(), "Div2".into());
        c.aux_pll_enabled.insert("PLLSAI1".into(), true);
        let clk48 = &t.muxes["CCIPR.CLK48SEL"];
        c.set_value(clk48, "PLLSAI1_Q".into());
        let r = c.compute(&t);
        assert_eq!(r.sysclk, 48e6);
        assert_eq!(r.aux[0].vco, 192e6);
        assert_eq!(r.kernel["CCIPR.CLK48SEL"], 96e6);
        // APB1 /2 doubles the timer clock.
        c.apb.insert("PPRE1".into(), "Div2".into());
        let r = c.compute(&t);
        assert_eq!(r.resolve("PCLK1"), Some(24e6));
        assert_eq!(r.resolve("PCLK1_TIM"), Some(48e6));
    }

    /// The STM32CubeMX reference tree for the L4R5 (120 MHz). Two values differ from
    /// the written spec because the spec cannot add up otherwise:
    /// - PLL source HSI (16 MHz), not MSI (4 MHz): 4/2*30/2 = 30 MHz, 16/2*30/2 = 120 MHz,
    ///   and PLLSAI1/2 only reach 96/64 MHz from 16 MHz too (all PLLs share PLLSRC).
    /// - PLLSAI1 /Q = /4: CLK48 = PLLSAI1_Q must be 48 MHz, and 192/2 would be 96 MHz.
    pub(crate) fn cubemx_reference(t: &ClockTree) -> ClockConfig {
        let mut c = ClockConfig::default();
        c.normalize(t);
        for osc in ["LSE", "LSI", "MSI", "HSI", "HSE"] {
            c.enabled.insert(osc.into(), true);
        }
        c.oscillators.insert("HSE".into(), 8e6);
        c.oscillators.insert("LSE".into(), 32_768.0);
        c.oscillators.insert("LSI".into(), 32_000.0);
        c.set_value(&t.osc_range["MSI"], "Range4M".into());
        c.set_value(&t.muxes["BDCR.RTCSEL"], "LSE".into());
        // Main PLL: /2 *30, P Q R /2.
        c.pll_enabled = true;
        for (role, v) in [
            (Role::PllSrc, "HSI"),
            (Role::PllM, "Div2"),
            (Role::PllN, "Mul30"),
            (Role::PllP, "Div2"),
            (Role::PllQ, "Div2"),
            (Role::PllR, "Div2"),
            (Role::Sys, "PLL1_R"),
            (Role::Ahb, "Div1"),
        ] {
            c.roles.insert(role, v.into());
        }
        c.pll_outputs.insert(Role::PllP, true);
        c.pll_outputs.insert(Role::PllQ, true);
        c.apb.insert("PPRE1".into(), "Div2".into());
        c.apb.insert("PPRE2".into(), "Div1".into());
        // PLLSAI1: /1 *12, P R /2, Q /4. PLLSAI2: /1 *8, P Q R /2.
        for (name, n, q) in [("PLLSAI1", "Mul12", "Div4"), ("PLLSAI2", "Mul8", "Div2")] {
            let pll = t.aux_plls.iter().find(|p| p.name == name).unwrap();
            c.aux_pll_enabled.insert(name.into(), true);
            c.set_value(pll.m.as_ref().unwrap(), "Div1".into());
            c.set_value(&pll.n, n.into());
            c.set_value(pll.p.as_ref().unwrap(), "Div2".into());
            c.set_value(pll.q.as_ref().unwrap(), q.into());
            c.set_value(pll.r.as_ref().unwrap(), "Div2".into());
        }
        for (mux, v) in [
            ("CCIPR.CLK48SEL", "PLLSAI1_Q"),
            ("CCIPR.ADCSEL", "PLLSAI1_R"),
            ("CCIPR2.SAI1SEL", "PLLSAI1_P"),
            ("CCIPR.I2C1SEL", "PCLK1"),
            ("CCIPR.I2C2SEL", "PCLK1"),
            ("CCIPR.I2C3SEL", "PCLK1"),
            ("CCIPR.USART1SEL", "PCLK2"),
            ("CCIPR.USART2SEL", "PCLK1"),
            ("CCIPR.USART3SEL", "PCLK1"),
            ("CCIPR.UART4SEL", "PCLK1"),
            ("CCIPR.UART5SEL", "PCLK1"),
            ("CCIPR.LPUART1SEL", "PCLK1"),
            ("CCIPR.LPTIM1SEL", "PCLK1"),
            ("CCIPR.LPTIM2SEL", "PCLK1"),
        ] {
            c.set_value(&t.muxes[mux], v.into());
        }
        c
    }

    #[test]
    fn family_oscillator_defaults() {
        for (version, hsi, lsi) in [("f1", 8e6, 40e3), ("f4", 16e6, 32e3), ("l4plus", 16e6, 32e3), ("h7rm0433", 64e6, 32e3)] {
            let Some(t) = tree(version) else { continue };
            let mut c = ClockConfig::default();
            c.normalize(&t);
            assert_eq!(c.osc_frequency(&t, "HSI"), hsi, "HSI of {version}");
            assert_eq!(c.osc_frequency(&t, "LSI"), lsi, "LSI of {version}");
        }
    }

    #[test]
    fn l4plus_cubemx_reference_tree() {
        let Some(t) = tree("l4plus") else { return };
        let r = cubemx_reference(&t).compute(&t);
        let mhz = |f: f64| (f / 1e6 * 1000.0).round() / 1000.0;
        // Sources
        assert_eq!(r.resolve("LSE"), Some(32_768.0));
        assert_eq!(r.resolve("LSI"), Some(32_000.0));
        assert_eq!(r.resolve("MSI"), Some(4e6));
        assert_eq!(r.resolve("HSI"), Some(16e6));
        assert_eq!(r.resolve("HSE"), Some(8e6));
        assert_eq!(r.kernel["BDCR.RTCSEL"], 32_768.0, "RTC from LSE");
        // PLLs
        assert_eq!((mhz(r.pll_in), mhz(r.vco)), (8.0, 240.0));
        assert_eq!((r.pll_p, r.pll_q, r.pll_r), (Some(120e6), Some(120e6), Some(120e6)));
        let sai1 = &r.aux[0];
        assert_eq!((sai1.vco, sai1.p, sai1.q, sai1.r), (192e6, Some(96e6), Some(48e6), Some(96e6)));
        let sai2 = &r.aux[1];
        assert_eq!((sai2.vco, sai2.p, sai2.q, sai2.r), (128e6, Some(64e6), Some(64e6), Some(64e6)));
        // System and buses
        assert_eq!((r.sysclk, r.hclk), (120e6, 120e6));
        assert_eq!(r.resolve("PCLK1"), Some(60e6));
        assert_eq!(r.resolve("PCLK1_TIM"), Some(120e6));
        assert_eq!(r.resolve("PCLK2"), Some(120e6));
        assert_eq!(r.resolve("PCLK2_TIM"), Some(120e6));
        // Peripheral muxes
        for (mux, hz) in [
            ("CCIPR.CLK48SEL", 48e6),
            ("CCIPR.ADCSEL", 96e6),
            ("CCIPR2.SAI1SEL", 96e6),
            ("CCIPR.I2C1SEL", 60e6),
            ("CCIPR.I2C2SEL", 60e6),
            ("CCIPR.I2C3SEL", 60e6),
            ("CCIPR.USART1SEL", 120e6),
            ("CCIPR.USART2SEL", 60e6),
            ("CCIPR.USART3SEL", 60e6),
            ("CCIPR.UART4SEL", 60e6),
            ("CCIPR.UART5SEL", 60e6),
            ("CCIPR.LPUART1SEL", 60e6),
            ("CCIPR.LPTIM1SEL", 60e6),
            ("CCIPR.LPTIM2SEL", 60e6),
        ] {
            assert_eq!(r.kernel[mux], hz, "{mux}");
        }
    }
}
