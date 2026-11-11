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
        let mut add = |s: &Option<Selector>| {
            for v in s.iter().flat_map(|s| s.variants.iter()) {
                if let Some(base) = oscillator_base(&v.name) {
                    osc.insert(base);
                }
            }
        };
        add(&tree.sys);
        add(&tree.pll_src);
        add(&find(&["RTCSEL"]));
        // Put HSE first, then the rest in a stable order.
        let mut list: Vec<String> = osc.into_iter().collect();
        list.sort_by_key(|o| (!o.starts_with("HSE"), !o.starts_with("HS"), o.clone()));
        tree.oscillators = list;
        tree
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
}

/// Nominal oscillator frequency used to seed a new configuration.
/// Users edit these to match their board / the datasheet.
pub fn nominal_frequency(osc: &str) -> f64 {
    match osc {
        "HSE" => 8_000_000.0,
        "HSI48" => 48_000_000.0,
        "HSI" => 16_000_000.0,
        "MSI" | "MSIS" | "MSIK" | "CSI" => 4_000_000.0,
        "LSE" => 32_768.0,
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
                .or_insert_with(|| nominal_frequency(osc));
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
    }

    pub fn role(&self, role: Role) -> Option<&str> {
        self.roles.get(&role).map(String::as_str)
    }

    /// Whether a PLL output divider is active: user-enabled or feeding SYSCLK.
    pub fn pll_output_enabled(&self, role: Role) -> bool {
        let letter = match role {
            Role::PllP => "P",
            Role::PllQ => "Q",
            Role::PllR => "R",
            _ => return false,
        };
        let sys = self.role(Role::Sys).unwrap_or("");
        let feeds_sys = sys.starts_with("PLL") && sys.rsplit('_').next() == Some(letter);
        feeds_sys || self.pll_outputs.get(&role).copied().unwrap_or(false)
    }

    pub fn is_enabled(&self, osc: &str) -> bool {
        self.enabled.get(osc).copied().unwrap_or(false)
    }

    pub fn compute(&self, tree: &ClockTree) -> ClockResults {
        let mut r = ClockResults::default();
        let osc = |name: &str| -> f64 {
            if !self.is_enabled(name) {
                return 0.0;
            }
            self.oscillators.get(name).copied().unwrap_or(0.0)
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
        r
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
    pub buses: BTreeMap<String, f64>,
}

impl ClockResults {
    pub fn bus(&self, name: &str) -> Option<f64> {
        self.buses.get(name).copied()
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
