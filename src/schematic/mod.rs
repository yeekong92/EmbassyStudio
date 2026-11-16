//! Schematic overlay engine.
//!
//! A hardware schematic (SVG, PDF, KiCad `.kicad_sch` or Altium `.SchDoc`) is
//! turned into one SVG drawing per sheet plus a list of positioned text items.
//! The text is cross-referenced against the pin names of the selected MCU
//! package to place one interactive node beside every pin it finds.
//!
//! This module is strictly about *where things are drawn*: there is no netlist,
//! no knowledge of external components and no trace checking.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::db::{Package, gpio_name};

mod altium;
mod kicad;
mod pdf;
pub mod render;
mod svg_out;
mod svg_text;

/// Folder inside the project that holds the imported drawings.
pub const DIR: &str = "schematic";
/// File extensions offered by the import dialog.
pub const EXTENSIONS: &[&str] = &["svg", "pdf", "kicad_sch", "SchDoc", "schdoc"];

// ---------------------------------------------------------------------------
// Project model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SchematicConfig {
    #[serde(default)]
    pub sheets: Vec<Sheet>,
}

impl SchematicConfig {
    pub fn is_empty(&self) -> bool {
        self.sheets.is_empty()
    }
}

/// One imported schematic page with its pin nodes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sheet {
    pub name: String,
    /// Background drawing (SVG), relative to the project folder.
    pub file: String,
    /// What it was imported from (display only).
    #[serde(default)]
    pub source: String,
    /// Full path of the imported file, so the sheet can be refreshed from it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,
    /// Snap grid used while calibrating, in drawing units (0 = off).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub grid: f32,
    /// Node diameter as a fraction of the sheet width.
    #[serde(default = "default_node_size")]
    pub node_size: f32,
    #[serde(default)]
    pub nodes: Vec<Node>,
}

fn default_node_size() -> f32 {
    0.006
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Which way a node's label runs, i.e. away from the pin name it sits beside.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LabelDir {
    #[default]
    Right,
    Left,
    Up,
    Down,
}

impl LabelDir {
    pub fn is_default(&self) -> bool {
        *self == Self::Right
    }

    /// Next direction, clockwise.
    pub fn rotated(self) -> Self {
        match self {
            Self::Right => Self::Down,
            Self::Down => Self::Left,
            Self::Left => Self::Up,
            Self::Up => Self::Right,
        }
    }

    fn from_vector(x: f32, y: f32) -> Self {
        if x.abs() >= y.abs() {
            if x >= 0.0 { Self::Right } else { Self::Left }
        } else if y > 0.0 {
            Self::Down
        } else {
            Self::Up
        }
    }
}

/// An interactive pin node glued to the schematic.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Node {
    /// MCU pin name: a GPIO (`PA5`) or a power / system pin (`VDD`, `NRST`).
    pub pin: String,
    /// Position relative to the sheet: 0,0 is the top-left corner, 1,1 the bottom-right.
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "LabelDir::is_default")]
    pub label: LabelDir,
}

impl Node {
    /// Round to 1/100000 of the sheet so the project file stays readable.
    pub fn rounded(&self) -> Self {
        let r = |v: f32| (v.clamp(0.0, 1.0) * 1e5).round() / 1e5;
        Self { pin: self.pin.clone(), x: r(self.x), y: r(self.y), label: self.label }
    }
}

// ---------------------------------------------------------------------------
// Imported drawings
// ---------------------------------------------------------------------------

/// A piece of text on a drawing, in drawing units (Y down).
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    pub text: String,
    /// Centre of the text.
    pub x: f32,
    pub y: f32,
    /// Extent along and across the reading direction.
    pub len: f32,
    pub height: f32,
    /// Reading direction (unit vector).
    pub dir: (f32, f32),
    /// Known side of the symbol body: `+1` when the body continues past the end
    /// of the text, `-1` when it lies before its start. `None` for free text.
    pub inward: Option<f32>,
}

/// One sheet of an imported schematic.
pub struct Drawing {
    pub name: String,
    pub svg: String,
    pub width: f32,
    pub height: f32,
    pub texts: Vec<TextItem>,
}

/// Read a schematic and convert every sheet that matters to SVG.
pub fn import(path: &Path, pins: &PinNames) -> Result<Vec<Drawing>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("schematic").to_string();
    let drawings = match ext.as_str() {
        "svg" => {
            let svg = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            vec![svg_text::import(&stem, svg)?]
        }
        "pdf" => {
            let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            pdf::import(&stem, data, pins)?
        }
        "kicad_sch" => {
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            vec![kicad::import(&stem, &text)?]
        }
        "schdoc" => vec![altium::import(&stem, path)?],
        "sch" => bail!("Legacy .sch files are not supported; export the sheet as PDF or SVG, or use a .kicad_sch / .SchDoc file"),
        other => bail!("Unsupported schematic type '.{other}' (use SVG, PDF, KiCad .kicad_sch or Altium .SchDoc)"),
    };
    if drawings.is_empty() {
        bail!("{} contains no drawable sheet", path.display());
    }
    Ok(drawings)
}

/// A file name for a sheet's drawing inside [`DIR`].
pub fn file_name(sheet_name: &str) -> String {
    let mut out = String::new();
    for c in sheet_name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_matches('_');
    format!("{}.svg", if out.is_empty() { "schematic" } else { out })
}

/// What [`add_to_project`] did.
#[derive(Debug, Default)]
pub struct Imported {
    /// Index of the sheet showing the most pins.
    pub sheet: Option<usize>,
    /// Drawing files written (new or replaced).
    pub files: Vec<String>,
    /// GPIO pins located on any of the imported sheets.
    pub gpios: BTreeSet<String>,
    pub errors: Vec<String>,
}

/// Store imported drawings in the project folder and add them, with their
/// auto-located nodes, to the project.
///
/// Importing a sheet again refreshes its drawing but keeps the nodes already on
/// it — they may have been calibrated by hand — and only adds nodes for pins
/// that had none.
pub fn add_to_project(config: &mut SchematicConfig, project_dir: &Path, source: &Path, drawings: Vec<Drawing>, pins: &PinNames) -> Result<Imported> {
    let origin = source.to_string_lossy().to_string();
    let source = source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    std::fs::create_dir_all(project_dir.join(DIR)).with_context(|| format!("creating the {DIR} folder"))?;
    let mut out = Imported::default();
    let mut most = 0;
    for d in drawings {
        let located = locate(&d, pins);
        let rel = format!("{DIR}/{}", file_name(&d.name));
        if let Err(e) = std::fs::write(project_dir.join(&rel), &d.svg) {
            out.errors.push(format!("{rel}: {e}"));
            continue;
        }
        out.gpios.extend(located.nodes.iter().filter(|n| pins.is_gpio(&n.pin)).map(|n| n.pin.clone()));
        let idx = match config.sheets.iter().position(|s| s.file == rel) {
            Some(i) => {
                let sheet = &mut config.sheets[i];
                let kept: BTreeSet<String> = sheet.nodes.iter().map(|n| n.pin.clone()).collect();
                sheet.nodes.extend(located.nodes.into_iter().filter(|n| !kept.contains(&n.pin)));
                sheet.source = source.clone();
                sheet.origin = origin.clone();
                i
            }
            None => {
                config.sheets.push(Sheet {
                    name: d.name,
                    file: rel.clone(),
                    source: source.clone(),
                    origin: origin.clone(),
                    grid: 0.0,
                    node_size: located.node_size,
                    nodes: located.nodes,
                });
                config.sheets.len() - 1
            }
        };
        if out.sheet.is_none() || located.gpios_found > most {
            out.sheet = Some(idx);
            most = located.gpios_found;
        }
        out.files.push(rel);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pin location
// ---------------------------------------------------------------------------

/// The pin names of one MCU package, as the schematic is expected to spell them.
#[derive(Clone, Debug, Default)]
pub struct PinNames {
    pub gpios: BTreeSet<String>,
    /// Other spellings of GPIO names: `IO4` -> `GPIO4` (ESP32 module schematics).
    pub aliases: BTreeMap<String, String>,
    /// Power and system pins: normalised name -> (name as in stm32-data, pin count).
    pub passive: BTreeMap<String, (String, usize)>,
}

/// `VCAP_1` and `VCAP1`, `VREF+` and `Vref+` are the same pin.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

impl PinNames {
    pub fn from_package(pkg: &Package) -> Self {
        let mut out = Self::default();
        for p in &pkg.pins {
            if let Some(g) = p.signals.iter().find_map(|s| gpio_name(s)) {
                if let Some(n) = g.strip_prefix("GPIO") {
                    out.aliases.insert(format!("IO{n}"), g.clone());
                }
                out.gpios.insert(g);
            } else if let Some(s) = p.signals.first() {
                out.passive.entry(normalize(s)).or_insert_with(|| (s.clone(), 0)).1 += 1;
            }
        }
        out
    }

    pub fn is_gpio(&self, pin: &str) -> bool {
        self.gpios.contains(pin)
    }

    /// Number of GPIO names of this package that appear in `texts`.
    pub fn count_matches(&self, texts: &[TextItem]) -> usize {
        let mut found = BTreeSet::new();
        for t in texts {
            for (pin, _) in self.candidates(t) {
                found.insert(pin);
            }
        }
        found.len()
    }

    /// GPIO names found in one text item, each with the part of the item that spells it.
    fn candidates(&self, item: &TextItem) -> Vec<(String, Candidate)> {
        let chars: Vec<char> = item.text.chars().collect();
        let total = chars.len().max(1) as f32;
        let whole = item.text.trim();
        let has_space = whole.contains(char::is_whitespace);
        let mut out = Vec::new();
        let mut i = 0;
        let mut first = true;
        while i < chars.len() {
            if !chars[i].is_ascii_alphanumeric() {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let token: String = chars[start..i].iter().map(|c| c.to_ascii_uppercase()).collect();
            let is_first = std::mem::take(&mut first);
            let Some(pin) = (if self.gpios.contains(&token) { Some(token.clone()) } else { self.aliases.get(&token).cloned() }) else {
                continue;
            };
            // 0: the text is exactly the pin name. 1: it starts with it (`PC14-OSC32_IN`).
            // 2: the name is buried in a longer string.
            let rank = if whole.eq_ignore_ascii_case(&token) {
                0
            } else if is_first && !has_space {
                1
            } else {
                2
            };
            let mut c = Candidate { x: item.x, y: item.y, len: item.len, height: item.height, dir: item.dir, inward: item.inward, rank };
            if rank == 2 {
                // Only the token's share of the text.
                let k = (i - start) as f32;
                let mid = (start as f32 + k / 2.0) / total - 0.5;
                c.x += item.dir.0 * mid * item.len;
                c.y += item.dir.1 * mid * item.len;
                c.len = item.len * k / total;
            }
            out.push((pin, c));
        }
        out
    }
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    x: f32,
    y: f32,
    len: f32,
    height: f32,
    dir: (f32, f32),
    inward: Option<f32>,
    rank: u8,
}

impl Candidate {
    /// Coordinate along the reading direction, and across it.
    fn along(&self, x: f32, y: f32) -> f32 {
        x * self.dir.0 + y * self.dir.1
    }
    fn across(&self, x: f32, y: f32) -> f32 {
        -x * self.dir.1 + y * self.dir.0
    }
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Result of matching a drawing's text against the package pins.
pub struct Located {
    pub nodes: Vec<Node>,
    /// Suggested node diameter, relative to the sheet width.
    pub node_size: f32,
    /// GPIO pins of the package that were found.
    pub gpios_found: usize,
}

/// Find the MCU pins on a drawing and place one node beside each pin name.
///
/// A node goes just past the end of the pin name that faces the inside of the
/// symbol, so it never covers the name, the pin number or the wire.
pub fn locate(d: &Drawing, pins: &PinNames) -> Located {
    // Best-ranked candidates per GPIO.
    let mut by_pin: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for t in &d.texts {
        for (pin, c) in pins.candidates(t) {
            by_pin.entry(pin).or_default().push(c);
        }
    }
    for v in by_pin.values_mut() {
        let best = v.iter().map(|c| c.rank).min().unwrap_or(0);
        v.retain(|c| c.rank == best);
    }

    // A name that appears more than once (net label, second sheet reference…) is
    // resolved towards where the unambiguous pins are.
    let seeds: Vec<&Candidate> = by_pin.values().filter(|v| v.len() == 1).map(|v| &v[0]).collect();
    let pool: Vec<&Candidate> = if seeds.is_empty() { by_pin.values().flatten().collect() } else { seeds };
    let centre = (median(pool.iter().map(|c| c.x).collect()), median(pool.iter().map(|c| c.y).collect()));
    let chosen: Vec<(String, Candidate)> = by_pin
        .into_iter()
        .filter_map(|(pin, v)| {
            let dist = |c: &Candidate| (c.x - centre.0).powi(2) + (c.y - centre.1).powi(2);
            v.into_iter().min_by(|a, b| dist(a).total_cmp(&dist(b))).map(|c| (pin, c))
        })
        .collect();

    let height = median(chosen.iter().map(|(_, c)| c.height).collect()).max(d.width * 0.002);
    let numbers: Vec<&TextItem> = d
        .texts
        .iter()
        .filter(|t| !t.text.trim().is_empty() && t.text.trim().chars().all(|c| c.is_ascii_digit()))
        .collect();

    let mut nodes = Vec::new();
    for (pin, c) in &chosen {
        let side = c.inward.unwrap_or_else(|| inward_side(c, &chosen, &numbers, centre));
        let reach = c.len / 2.0 + height;
        let x = c.x + c.dir.0 * side * reach;
        let y = c.y + c.dir.1 * side * reach;
        nodes.push(Node {
            pin: pin.clone(),
            x: x / d.width,
            y: y / d.height,
            label: LabelDir::from_vector(c.dir.0 * side, c.dir.1 * side),
        });
    }
    let gpios_found = nodes.len();

    // Power and system pins: only exact names, and only inside the MCU symbol
    // (the same names label nets all over a schematic).
    if !chosen.is_empty() {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (_, c) in &chosen {
            x0 = x0.min(c.x);
            y0 = y0.min(c.y);
            x1 = x1.max(c.x);
            y1 = y1.max(c.y);
        }
        // Power pins often sit in a row beyond the last I/O pin.
        let margin = (height * 8.0).max((x1 - x0).max(y1 - y0) * 0.1);
        let inside = |t: &TextItem| t.x >= x0 - margin && t.x <= x1 + margin && t.y >= y0 - margin && t.y <= y1 + margin;
        let mut found: BTreeMap<String, Vec<&TextItem>> = BTreeMap::new();
        for t in d.texts.iter().filter(|t| inside(t)) {
            let key = normalize(t.text.trim());
            if pins.passive.contains_key(&key) {
                found.entry(key).or_default().push(t);
            }
        }
        for (key, mut items) in found {
            let (name, count) = &pins.passive[&key];
            let dist = |t: &TextItem| (t.x - centre.0).powi(2) + (t.y - centre.1).powi(2);
            items.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
            for t in items.into_iter().take(*count) {
                let c = Candidate { x: t.x, y: t.y, len: t.len, height: t.height, dir: t.dir, inward: t.inward, rank: 0 };
                let side = c.inward.unwrap_or_else(|| inward_side(&c, &chosen, &numbers, centre));
                let reach = c.len / 2.0 + height;
                nodes.push(Node {
                    pin: name.clone(),
                    x: (c.x + c.dir.0 * side * reach) / d.width,
                    y: (c.y + c.dir.1 * side * reach) / d.height,
                    label: LabelDir::from_vector(c.dir.0 * side, c.dir.1 * side),
                });
            }
        }
    }

    for n in &mut nodes {
        *n = n.rounded();
    }
    Located { nodes, node_size: (height * 1.15 / d.width).clamp(0.001, 0.05), gpios_found }
}

/// Which end of a pin name faces the inside of the symbol body: `+1` for the
/// end of the text, `-1` for its start. Purely geometric:
///
/// * pin names of one side of a symbol are aligned at the body edge, so the
///   end that lines up with the neighbours is the *outer* one;
/// * the pin number is printed beyond the outer end;
/// * failing both, the body is assumed to be towards the middle of all pins.
fn inward_side(c: &Candidate, all: &[(String, Candidate)], numbers: &[&TextItem], centre: (f32, f32)) -> f32 {
    let tol = c.height * 0.35;
    let start = c.along(c.x, c.y) - c.len / 2.0;
    let end = start + c.len;
    let mut score = 0.0;

    let (mut at_start, mut at_end) = (0, 0);
    for (_, o) in all {
        // Same reading direction, different row.
        if (o.dir.0 * c.dir.0 + o.dir.1 * c.dir.1) < 0.95 || (c.across(o.x, o.y) - c.across(c.x, c.y)).abs() < tol {
            continue;
        }
        let o_mid = c.along(o.x, o.y);
        if (o_mid - o.len / 2.0 - start).abs() < tol {
            at_start += 1;
        }
        if (o_mid + o.len / 2.0 - end).abs() < tol {
            at_end += 1;
        }
    }
    if at_start != at_end {
        score += if at_start > at_end { 2.0 } else { -2.0 };
    }

    let mut nearest: Option<(f32, f32)> = None;
    for n in numbers {
        if (c.across(n.x, n.y) - c.across(c.x, c.y)).abs() > c.height * 1.6 {
            continue;
        }
        let pos = c.along(n.x, n.y);
        let (gap, side) = if pos < start { (start - pos, 1.0) } else if pos > end { (pos - end, -1.0) } else { continue };
        if gap < c.height * 8.0 && nearest.is_none_or(|(g, _)| gap < g) {
            nearest = Some((gap, side));
        }
    }
    if let Some((_, side)) = nearest {
        score += 2.0 * side;
    }

    let to_centre = c.along(centre.0, centre.1) - c.along(c.x, c.y);
    if to_centre.abs() > c.height {
        score += to_centre.signum();
    }
    if score < 0.0 { -1.0 } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PackagePin;

    pub(crate) fn package(names: &[&str]) -> Package {
        Package {
            name: "TEST".into(),
            package: "LQFP".into(),
            pins: names
                .iter()
                .enumerate()
                .map(|(i, n)| PackagePin { position: (i + 1).to_string(), signals: vec![n.to_string()] })
                .collect(),
        }
    }

    fn text(s: &str, x: f32, y: f32) -> TextItem {
        TextItem { text: s.into(), x, y, len: s.len() as f32 * 6.0, height: 8.0, dir: (1.0, 0.0), inward: None }
    }

    /// A symbol 200 wide: left pins are left-aligned at x=110, right pins right-aligned at x=290.
    fn symbol() -> Drawing {
        let mut texts = Vec::new();
        for (i, n) in ["PA0", "PA1", "PA10", "PC14-OSC32_IN"].iter().enumerate() {
            let len = n.len() as f32 * 6.0;
            texts.push(text(n, 110.0 + len / 2.0, 100.0 + 20.0 * i as f32));
            texts.push(text(&format!("{}", i + 1), 90.0, 100.0 + 20.0 * i as f32));
        }
        for (i, n) in ["PB3", "PB10", "PB6"].iter().enumerate() {
            let len = n.len() as f32 * 6.0;
            texts.push(text(n, 290.0 - len / 2.0, 100.0 + 20.0 * i as f32));
        }
        // A net label far away that reuses a pin name.
        texts.push(text("PA0", 900.0, 700.0));
        texts.push(text("VDD", 200.0, 90.0));
        texts.push(text("VDD", 850.0, 80.0));
        Drawing { name: "t".into(), svg: String::new(), width: 1000.0, height: 800.0, texts }
    }

    #[test]
    fn locates_pins_inside_the_body() {
        let pkg = package(&["PA0", "PA1", "PA10", "PC14-OSC32_IN", "PB3", "PB10", "PB6", "PB7", "VDD", "VSS"]);
        let pins = PinNames::from_package(&pkg);
        let d = symbol();
        let out = locate(&d, &pins);
        assert_eq!(out.gpios_found, 7);
        let node = |p: &str| out.nodes.iter().find(|n| n.pin == p).unwrap_or_else(|| panic!("{p} not placed"));

        // Left pins: node right of the name, label continuing right.
        let pa0 = node("PA0");
        assert!(pa0.x * 1000.0 > 130.0 && pa0.x * 1000.0 < 145.0, "PA0 at {}", pa0.x * 1000.0);
        assert!((pa0.y * 800.0 - 100.0).abs() < 0.1);
        assert_eq!(pa0.label, LabelDir::Right);
        // `PC14-OSC32_IN` is PC14, placed after the whole name.
        assert!(node("PC14").x * 1000.0 > 110.0 + 13.0 * 6.0);

        // Right pins: node left of the name.
        let pb3 = node("PB3");
        assert!(pb3.x * 1000.0 < 272.0 && pb3.x * 1000.0 > 255.0, "PB3 at {}", pb3.x * 1000.0);
        assert_eq!(pb3.label, LabelDir::Left);

        // The VDD pin inside the symbol is found, the net label elsewhere is not.
        let vdd: Vec<_> = out.nodes.iter().filter(|n| n.pin == "VDD").collect();
        assert_eq!(vdd.len(), 1);
        assert!(vdd[0].x < 0.4);
        assert!(!out.nodes.iter().any(|n| n.pin == "PB7"));
    }

    #[test]
    fn importing_again_keeps_calibrated_nodes() {
        let pkg = package(&["PA0", "PA1", "PA10", "PC14-OSC32_IN", "PB3", "PB10", "PB6"]);
        let pins = PinNames::from_package(&pkg);
        let dir = std::env::temp_dir().join(format!("embassy-studio-reimport-{}", std::process::id()));
        let mut config = SchematicConfig::default();
        let first = add_to_project(&mut config, &dir, Path::new("a.svg"), vec![symbol()], &pins).unwrap();
        assert_eq!((first.sheet, first.gpios.len(), config.sheets.len()), (Some(0), 7, 1));
        assert!(dir.join("schematic/t.svg").is_file());

        // The user moves PA0 and removes PB6, then imports the sheet again.
        let sheet = &mut config.sheets[0];
        sheet.nodes.retain(|n| n.pin != "PB6");
        sheet.nodes.iter_mut().find(|n| n.pin == "PA0").unwrap().x = 0.5;
        let count = sheet.nodes.len();
        add_to_project(&mut config, &dir, Path::new("a.svg"), vec![symbol()], &pins).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let sheet = &config.sheets[0];
        assert_eq!((config.sheets.len(), sheet.nodes.len()), (1, count + 1));
        assert_eq!(sheet.nodes.iter().find(|n| n.pin == "PA0").unwrap().x, 0.5);
        assert!(sheet.nodes.iter().any(|n| n.pin == "PB6"));
    }

    #[test]
    fn name_inside_longer_text_uses_its_own_span() {
        let pkg = package(&["PA5"]);
        let pins = PinNames::from_package(&pkg);
        let item = text("21 PA5", 100.0, 50.0);
        let c = pins.candidates(&item);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1.rank, 2);
        // "PA5" is the right half of the 36-wide string.
        assert!((c[0].1.x - 109.0).abs() < 0.01 && (c[0].1.len - 18.0).abs() < 0.01);
        // PA50 or XPA5 are not PA5.
        assert!(pins.candidates(&text("PA50", 0.0, 0.0)).is_empty());
    }

    /// Manual check of a real schematic: imports it for a chip, prints what was
    /// located and writes one PNG per sheet with the nodes drawn on top.
    ///
    /// ```text
    /// SCH_FILE=board.pdf SCH_CHIP=…/chips/STM32L4R5ZI.json SCH_PACKAGE=STM32L4R5ZITx SCH_OUT=out \
    ///     cargo test --release schematic::tests::overlay_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs SCH_FILE, SCH_CHIP and SCH_OUT"]
    fn overlay_preview() {
        use resvg::{tiny_skia, usvg};
        let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let chip: crate::db::Chip = serde_json::from_str(&std::fs::read_to_string(var("SCH_CHIP")).unwrap()).unwrap();
        let wanted = std::env::var("SCH_PACKAGE").ok();
        let pkg = chip.packages.iter().find(|p| Some(&p.name) == wanted.as_ref()).unwrap_or(&chip.packages[0]);
        let pins = PinNames::from_package(pkg);
        let out = std::path::PathBuf::from(var("SCH_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let started = std::time::Instant::now();
        let drawings = import(Path::new(&var("SCH_FILE")), &pins).unwrap();
        println!("{} ({}): {} sheet(s) in {:.2?}", chip.name, pkg.name, drawings.len(), started.elapsed());

        let mut opt = usvg::Options::default();
        opt.fontdb_mut().load_system_fonts();
        for d in &drawings {
            let located = locate(d, &pins);
            let missing: Vec<&String> = pins.gpios.iter().filter(|g| !located.nodes.iter().any(|n| &n.pin == *g)).collect();
            println!(
                "  {}: {}x{}, {} text items, {} of {} I/O pins, {} nodes, node size {:.4}, missing {:?}",
                d.name,
                d.width,
                d.height,
                d.texts.len(),
                located.gpios_found,
                pins.gpios.len(),
                located.nodes.len(),
                located.node_size,
                missing
            );
            // SCH_DEBUG=PA0 lists the text items spelling a name, relative to the sheet.
            if let Ok(name) = std::env::var("SCH_DEBUG") {
                for t in d.texts.iter().filter(|t| t.text.contains(&name)) {
                    println!(
                        "    {:?} at {:.5},{:.5} len {:.5} height {:.5} dir {:?} inward {:?}",
                        t.text,
                        t.x / d.width,
                        t.y / d.height,
                        t.len / d.width,
                        t.height / d.width,
                        t.dir,
                        t.inward
                    );
                }
            }
            let tree = usvg::Tree::from_str(&d.svg, &opt).unwrap();
            let size = tree.size();
            let scale = 5000.0 / size.width().max(size.height());
            let (w, h) = (size.width() * scale, size.height() * scale);
            let mut pixmap = tiny_skia::Pixmap::new(w as u32, h as u32).unwrap();
            pixmap.fill(tiny_skia::Color::WHITE);
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            let mut paint = tiny_skia::Paint::default();
            for n in &located.nodes {
                let r = located.node_size * w / 2.0;
                let (r, color) = if pins.is_gpio(&n.pin) { (r, (230, 40, 160)) } else { (r * 0.7, (40, 90, 230)) };
                paint.set_color_rgba8(color.0, color.1, color.2, 200);
                let (x, y) = (n.x * w, n.y * h);
                let dot = tiny_skia::PathBuilder::from_circle(x, y, r).unwrap();
                pixmap.fill_path(&dot, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
                // A tick showing which way the label runs.
                let (dx, dy) = match n.label {
                    LabelDir::Right => (1.0, 0.0),
                    LabelDir::Left => (-1.0, 0.0),
                    LabelDir::Up => (0.0, -1.0),
                    LabelDir::Down => (0.0, 1.0),
                };
                let tick = tiny_skia::Rect::from_ltrb(
                    (x + dx * r).min(x + dx * r * 3.0) - r * 0.2,
                    (y + dy * r).min(y + dy * r * 3.0) - r * 0.2,
                    (x + dx * r).max(x + dx * r * 3.0) + r * 0.2,
                    (y + dy * r).max(y + dy * r * 3.0) + r * 0.2,
                )
                .unwrap();
                pixmap.fill_rect(tick, &paint, tiny_skia::Transform::identity(), None);
            }
            let name = file_name(&d.name);
            std::fs::write(out.join(&name), &d.svg).unwrap();
            pixmap.save_png(out.join(name.replace(".svg", ".png"))).unwrap();
        }
    }

    #[test]
    fn nodes_round_trip_through_json() {
        let sheet = Sheet {
            name: "MCU".into(),
            file: "schematic/MCU.svg".into(),
            source: "board.pdf".into(),
            origin: String::new(),
            grid: 0.0,
            node_size: 0.006,
            nodes: vec![
                Node { pin: "PA0".into(), x: 0.123456789, y: 0.5, label: LabelDir::Right }.rounded(),
                Node { pin: "PB3".into(), x: 0.9, y: 0.25, label: LabelDir::Up },
            ],
        };
        let json = serde_json::to_string(&sheet).unwrap();
        assert!(json.contains(r#"{"pin":"PA0","x":0.12346,"y":0.5}"#), "{json}");
        assert!(json.contains(r#""label":"up""#));
        assert_eq!(serde_json::from_str::<Sheet>(&json).unwrap(), sheet);
    }
}
