//! KiCad `.kicad_sch` import (KiCad 6 and later, S-expression format).
//!
//! The sheet is redrawn as SVG — symbol bodies, pins, wires, labels and text —
//! and every symbol pin is reported with the exact position of its name, so pin
//! location does not depend on guessing from rendered text.
//!
//! Only geometry is read: nets are not traced and components are not interpreted.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};

use super::svg_out::{Anchor, SvgOut, rgb, text_width};
use super::{Drawing, TextItem};

// ---- S-expressions ----

#[derive(Debug, Clone, PartialEq)]
enum Sx {
    Atom(String),
    List(Vec<Sx>),
}

fn parse(text: &str) -> Result<Sx> {
    let b = text.as_bytes();
    let mut stack: Vec<Vec<Sx>> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' => {
                stack.push(Vec::new());
                i += 1;
            }
            b')' => {
                let done = stack.pop().ok_or_else(|| anyhow!("unbalanced ')' at byte {i}"))?;
                match stack.last_mut() {
                    Some(parent) => parent.push(Sx::List(done)),
                    None => return Ok(Sx::List(done)),
                }
                i += 1;
            }
            b'"' => {
                let mut s = Vec::new();
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => b'\n',
                            b't' => b'\t',
                            c => c,
                        });
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                i += 1;
                if let Some(top) = stack.last_mut() {
                    top.push(Sx::Atom(String::from_utf8_lossy(&s).into_owned()));
                }
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'(' && b[i] != b')' {
                    i += 1;
                }
                if let Some(top) = stack.last_mut() {
                    top.push(Sx::Atom(text[start..i].to_string()));
                }
            }
        }
    }
    bail!("unexpected end of file (unbalanced parentheses)")
}

impl Sx {
    fn items(&self) -> &[Sx] {
        match self {
            Sx::List(v) => v,
            Sx::Atom(_) => &[],
        }
    }

    fn atom(&self) -> Option<&str> {
        match self {
            Sx::Atom(s) => Some(s),
            Sx::List(_) => None,
        }
    }

    fn head(&self) -> &str {
        self.items().first().and_then(Sx::atom).unwrap_or("")
    }

    /// The n-th argument (after the head) as text.
    fn arg(&self, n: usize) -> Option<&str> {
        self.items().get(n + 1).and_then(Sx::atom)
    }

    fn num(&self, n: usize) -> f32 {
        self.arg(n).and_then(|s| s.parse().ok()).unwrap_or(0.0)
    }

    fn child(&self, name: &str) -> Option<&Sx> {
        self.items().iter().skip(1).find(|c| c.head() == name)
    }

    fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Sx> + 'a {
        self.items().iter().skip(1).filter(move |c| c.head() == name)
    }

    fn has_atom(&self, name: &str) -> bool {
        self.items().iter().skip(1).any(|c| c.atom() == Some(name))
    }

    /// `(at x y [angle])`
    fn at(&self) -> (f32, f32, f32) {
        self.child("at").map(|a| (a.num(0), a.num(1), a.num(2))).unwrap_or((0.0, 0.0, 0.0))
    }

    fn xy(&self, name: &str) -> (f32, f32) {
        self.child(name).map(|a| (a.num(0), a.num(1))).unwrap_or((0.0, 0.0))
    }

    fn pts(&self) -> Vec<(f32, f32)> {
        self.child("pts").map(|p| p.children("xy").map(|xy| (xy.num(0), xy.num(1))).collect()).unwrap_or_default()
    }

    /// `hide` as a bare atom (KiCad 6/7) or `(hide yes)` (KiCad 8+), here or in `(effects …)`.
    fn hidden(&self) -> bool {
        let here = |s: &Sx| s.has_atom("hide") || s.child("hide").is_some_and(|h| h.arg(0) != Some("no"));
        here(self) || self.child("effects").is_some_and(here)
    }

    /// Text height from `(effects (font (size h w)))`.
    fn font_size(&self) -> f32 {
        self.child("effects")
            .and_then(|e| e.child("font"))
            .and_then(|f| f.child("size"))
            .map(|s| s.num(0))
            .filter(|s| *s > 0.0)
            .unwrap_or(1.27)
    }

    fn justify(&self) -> Anchor {
        match self.child("effects").and_then(|e| e.child("justify")) {
            Some(j) if j.has_atom("left") => Anchor::Start,
            Some(j) if j.has_atom("right") => Anchor::End,
            _ => Anchor::Middle,
        }
    }
}

// ---- drawing ----

const SYMBOL: (u8, u8, u8) = (132, 0, 0);
const BODY_FILL: (u8, u8, u8) = (255, 255, 194);
const WIRE: (u8, u8, u8) = (0, 150, 0);
const BUS: (u8, u8, u8) = (0, 0, 132);
const PIN_NAME: (u8, u8, u8) = (0, 100, 100);
const PIN_NUM: (u8, u8, u8) = (169, 0, 0);
const LABEL: (u8, u8, u8) = (15, 15, 15);
const GLOBAL: (u8, u8, u8) = (132, 0, 0);
const NOTE: (u8, u8, u8) = (0, 0, 194);
const FIELD: (u8, u8, u8) = (0, 100, 100);
const DEFAULT_WIDTH: f32 = 0.1524;
/// KiCad's text size is the glyph height; Arial capitals are 0.716 em tall.
const EM: f32 = 1.0 / 0.716;

fn col(c: (u8, u8, u8)) -> String {
    rgb(c.0, c.1, c.2)
}

fn paper(sch: &Sx) -> (f32, f32) {
    let Some(p) = sch.child("paper") else { return (297.0, 210.0) };
    let (mut w, mut h) = match p.arg(0).unwrap_or("A4") {
        "A5" => (210.0, 148.0),
        "A4" => (297.0, 210.0),
        "A3" => (420.0, 297.0),
        "A2" => (594.0, 420.0),
        "A1" => (841.0, 594.0),
        "A0" => (1189.0, 841.0),
        "A" | "USLetter" => (279.4, 215.9),
        "B" | "USLedger" => (431.8, 279.4),
        "C" => (558.8, 431.8),
        "D" => (863.6, 558.8),
        "E" => (1117.6, 863.6),
        "USLegal" => (355.6, 215.9),
        "User" => (p.num(1).max(10.0), p.num(2).max(10.0)),
        _ => (297.0, 210.0),
    };
    if p.has_atom("portrait") {
        std::mem::swap(&mut w, &mut h);
    }
    (w, h)
}

/// Placement of a symbol: library coordinates (Y up) to sheet coordinates (Y down).
#[derive(Clone, Copy)]
struct Place {
    at: (f32, f32),
    /// Row-major 2×2 matrix.
    m: [f32; 4],
}

impl Place {
    fn new(sym: &Sx) -> Self {
        let (x, y, rot) = sym.at();
        let mut m = match (rot.round() as i32).rem_euclid(360) {
            90 => [0.0, -1.0, -1.0, 0.0],
            180 => [-1.0, 0.0, 0.0, 1.0],
            270 => [0.0, 1.0, 1.0, 0.0],
            _ => [1.0, 0.0, 0.0, -1.0],
        };
        // `mirror x` flips about the X axis (top-bottom), `mirror y` left-right,
        // both applied on the sheet after the rotation.
        if let Some(mirror) = sym.child("mirror") {
            if mirror.has_atom("x") {
                m[2] = -m[2];
                m[3] = -m[3];
            }
            if mirror.has_atom("y") {
                m[0] = -m[0];
                m[1] = -m[1];
            }
        }
        Self { at: (x, y), m }
    }

    fn dir(&self, v: (f32, f32)) -> (f32, f32) {
        (self.m[0] * v.0 + self.m[1] * v.1, self.m[2] * v.0 + self.m[3] * v.1)
    }

    fn pt(&self, p: (f32, f32)) -> (f32, f32) {
        let d = self.dir(p);
        (self.at.0 + d.0, self.at.1 + d.1)
    }
}

fn stroke(item: &Sx, default: (u8, u8, u8)) -> (f32, String) {
    let s = item.child("stroke");
    let width = s.and_then(|s| s.child("width")).map(|w| w.num(0)).filter(|w| *w > 0.0).unwrap_or(DEFAULT_WIDTH);
    let color = s
        .and_then(|s| s.child("color"))
        .filter(|c| c.num(0) + c.num(1) + c.num(2) + c.num(3) > 0.0)
        .map(|c| rgb(c.num(0) as u8, c.num(1) as u8, c.num(2) as u8))
        .unwrap_or_else(|| col(default));
    (width, color)
}

fn fill(item: &Sx, outline: &str) -> String {
    let Some(f) = item.child("fill") else { return "none".into() };
    match f.child("type").and_then(|t| t.arg(0)) {
        Some("outline") => outline.to_string(),
        Some("background") => col(BODY_FILL),
        Some("color") => f.child("color").map(|c| rgb(c.num(0) as u8, c.num(1) as u8, c.num(2) as u8)).unwrap_or_else(|| "none".into()),
        _ => "none".into(),
    }
}

/// An arc through three points, already in sheet coordinates.
fn arc3(out: &mut SvgOut, a: (f32, f32), mid: (f32, f32), b: (f32, f32), width: f32, color: &str) {
    // Circumcentre of the three points.
    let d = 2.0 * (a.0 * (mid.1 - b.1) + mid.0 * (b.1 - a.1) + b.0 * (a.1 - mid.1));
    if d.abs() < 1e-6 {
        out.line(a, b, width, color);
        return;
    }
    let sq = |p: (f32, f32)| p.0 * p.0 + p.1 * p.1;
    let cx = (sq(a) * (mid.1 - b.1) + sq(mid) * (b.1 - a.1) + sq(b) * (a.1 - mid.1)) / d;
    let cy = (sq(a) * (b.0 - mid.0) + sq(mid) * (a.0 - b.0) + sq(b) * (mid.0 - a.0)) / d;
    let r = (a.0 - cx).hypot(a.1 - cy);
    // Two halves through the middle point: each is under 180°, so no large-arc flag.
    let sweep = (mid.0 - a.0) * (b.1 - mid.1) - (mid.1 - a.1) * (b.0 - mid.0) > 0.0;
    out.arc(a, mid, r, false, sweep, width, color);
    out.arc(mid, b, r, false, sweep, width, color);
}

/// Text that reads left-to-right or bottom-to-top, starting (or ending) at `at`.
struct Placed {
    at: (f32, f32),
    vertical: bool,
    anchor: Anchor,
}

/// Where a string goes when it must run from `at` in direction `dir` (a unit
/// axis vector on the sheet) without ever being drawn upside down.
fn run_from(at: (f32, f32), dir: (f32, f32)) -> Placed {
    let vertical = dir.1.abs() > dir.0.abs();
    // Reading direction is +X, or -Y (upwards) for vertical text.
    let forward = if vertical { dir.1 < 0.0 } else { dir.0 > 0.0 };
    Placed { at, vertical, anchor: if forward { Anchor::Start } else { Anchor::End } }
}

struct Ctx<'a> {
    out: SvgOut,
    texts: Vec<TextItem>,
    lib: HashMap<&'a str, &'a Sx>,
}

impl Ctx<'_> {
    /// Draw `text` centred across the baseline direction; returns nothing.
    fn label(&mut self, p: &Placed, text: &str, size: f32, color: &str) {
        let em = size * EM;
        // Lift the baseline so the glyphs are centred on the anchor line.
        let at = if p.vertical { (p.at.0 + size * 0.5, p.at.1) } else { (p.at.0, p.at.1 + size * 0.5) };
        self.out.text(at, text, em, p.anchor, p.vertical, color);
    }

    fn graphics(&mut self, unit: &Sx, place: &Place) {
        for item in unit.items().iter().skip(1) {
            let (width, color) = stroke(item, SYMBOL);
            let fill = fill(item, &color);
            match item.head() {
                "rectangle" => {
                    let (a, b) = (item.xy("start"), item.xy("end"));
                    let pts = [a, (b.0, a.1), b, (a.0, b.1)].map(|p| place.pt(p));
                    self.out.poly(&pts, true, width, &color, &fill);
                }
                "polyline" | "bezier" => {
                    let pts: Vec<_> = item.pts().into_iter().map(|p| place.pt(p)).collect();
                    self.out.poly(&pts, false, width, &color, &fill);
                }
                "circle" => {
                    let r = item.child("radius").map(|r| r.num(0)).unwrap_or(0.0);
                    self.out.circle(place.pt(item.xy("center")), r, width, &color, &fill);
                }
                "arc" => {
                    if item.child("mid").is_some() {
                        arc3(&mut self.out, place.pt(item.xy("start")), place.pt(item.xy("mid")), place.pt(item.xy("end")), width, &color);
                    }
                }
                "text" => {
                    let (x, y, _) = item.at();
                    let size = item.font_size();
                    let at = place.pt((x, y));
                    self.out.text((at.0, at.1 + size * 0.5), item.arg(0).unwrap_or(""), size * EM, Anchor::Middle, false, &color);
                }
                _ => {}
            }
        }
    }

    fn pins(&mut self, unit: &Sx, place: &Place, name_offset: f32, show_names: bool, show_numbers: bool) {
        for pin in unit.children("pin") {
            if pin.hidden() {
                continue;
            }
            let (x, y, angle) = pin.at();
            let length = pin.child("length").map(|l| l.num(0)).unwrap_or(2.54);
            let (sin, cos) = angle.to_radians().sin_cos();
            let dir = place.dir((cos.round(), sin.round()));
            let outer = place.pt((x, y));
            let inner = (outer.0 + dir.0 * length, outer.1 + dir.1 * length);
            self.out.line(outer, inner, DEFAULT_WIDTH, &col(SYMBOL));

            let across = if dir.1.abs() > dir.0.abs() { (-1.0, 0.0) } else { (0.0, -1.0) };
            if show_numbers {
                if let Some(num) = pin.child("number").filter(|n| !n.hidden()) {
                    let size = num.font_size();
                    let mid = ((outer.0 + inner.0) / 2.0 + across.0 * size * 0.75, (outer.1 + inner.1) / 2.0 + across.1 * size * 0.75);
                    let p = Placed { at: mid, vertical: dir.1.abs() > dir.0.abs(), anchor: Anchor::Middle };
                    self.label(&p, num.arg(0).unwrap_or(""), size, &col(PIN_NUM));
                }
            }

            let Some(name) = pin.child("name") else { continue };
            let text = name.arg(0).unwrap_or("");
            if text.is_empty() || text == "~" || !show_names || name.hidden() {
                continue;
            }
            let size = name.font_size();
            let len = text_width(text, size * EM);
            // Inside the body past the pin, or (offset 0) outside, above the pin.
            let (start, run) = if name_offset > 0.0 {
                ((inner.0 + dir.0 * name_offset, inner.1 + dir.1 * name_offset), dir)
            } else {
                let m = ((outer.0 + inner.0) / 2.0 - across.0 * size * 0.75, (outer.1 + inner.1) / 2.0 - across.1 * size * 0.75);
                ((m.0 - dir.0 * len / 2.0, m.1 - dir.1 * len / 2.0), dir)
            };
            let p = run_from(start, run);
            self.label(&p, text, size, &col(PIN_NAME));
            let reading = if p.vertical { (0.0, -1.0) } else { (1.0, 0.0) };
            let forward = p.anchor == Anchor::Start;
            self.texts.push(TextItem {
                text: text.to_string(),
                x: start.0 + run.0 * len / 2.0,
                y: start.1 + run.1 * len / 2.0,
                len,
                height: size,
                dir: reading,
                // The body lies further along the run, away from the pin.
                inward: Some(if forward { 1.0 } else { -1.0 }),
            });
        }
    }

    fn symbol(&mut self, sym: &Sx) {
        let Some(lib) = sym.child("lib_name").or(sym.child("lib_id")).and_then(|l| l.arg(0)).and_then(|id| self.lib.get(id).copied()) else {
            return;
        };
        let place = Place::new(sym);
        let unit = sym.child("unit").map(|u| u.num(0) as i32).unwrap_or(1);
        let style = sym.child("convert").or(sym.child("body_style")).map(|u| u.num(0) as i32).unwrap_or(1);
        let names = lib.child("pin_names");
        let name_offset = names.and_then(|n| n.child("offset")).map(|o| o.num(0)).unwrap_or(0.508);
        let show_names = !names.is_some_and(Sx::hidden);
        let show_numbers = !lib.child("pin_numbers").is_some_and(Sx::hidden);

        // Sub-symbols are named `<name>_<unit>_<style>`; 0 means "all".
        let parts: Vec<&Sx> = lib
            .children("symbol")
            .filter(|s| {
                let mut it = s.arg(0).unwrap_or("").rsplit('_');
                let st: i32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                let un: i32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                (un == 0 || un == unit) && (st == 0 || st == style)
            })
            .collect();
        for part in &parts {
            self.graphics(part, &place);
        }
        for part in &parts {
            self.pins(part, &place, name_offset, show_names, show_numbers);
        }

        // Fields are stored with sheet positions.
        let turned = matches!((sym.at().2.round() as i32).rem_euclid(180), 90);
        for prop in sym.children("property") {
            let value = prop.arg(1).unwrap_or("");
            if prop.hidden() || value.is_empty() || value == "~" {
                continue;
            }
            let (x, y, angle) = prop.at();
            let vertical = ((angle.round() as i32).rem_euclid(180) == 90) != turned;
            let p = Placed { at: (x, y), vertical, anchor: prop.justify() };
            self.label(&p, value, prop.font_size(), &col(FIELD));
        }
    }

    /// Labels and notes: `(kind "text" (at x y angle) (effects …))`.
    fn sheet_text(&mut self, item: &Sx, color: (u8, u8, u8), boxed: bool) {
        let text = item.arg(0).unwrap_or("");
        let (x, y, angle) = item.at();
        let size = item.font_size();
        let dir = match (angle.round() as i32).rem_euclid(360) {
            90 => (0.0, -1.0),
            180 => (-1.0, 0.0),
            270 => (0.0, 1.0),
            _ => (1.0, 0.0),
        };
        let p = run_from((x, y), dir);
        for (i, line) in text.split('\n').enumerate() {
            let shift = i as f32 * size * 1.6;
            // Labels sit on the wire: lift the text off it.
            let lift = if boxed { 0.0 } else { size * 0.75 };
            let at = if p.vertical { (x - lift + shift, y) } else { (x, y - lift + shift) };
            let start = if boxed { (at.0 + dir.0 * size * 0.6, at.1 + dir.1 * size * 0.6) } else { at };
            self.label(&Placed { at: start, vertical: p.vertical, anchor: p.anchor }, line, size, &col(color));
        }
        if boxed {
            let len = text_width(text, size * EM) + size * 1.2;
            let h = size * 0.85;
            let across = if p.vertical { (1.0, 0.0) } else { (0.0, 1.0) };
            let far = (x + dir.0 * len, y + dir.1 * len);
            let pts = [
                (x + across.0 * h, y + across.1 * h),
                (far.0 + across.0 * h, far.1 + across.1 * h),
                (far.0 - across.0 * h, far.1 - across.1 * h),
                (x - across.0 * h, y - across.1 * h),
            ];
            self.out.poly(&pts, true, DEFAULT_WIDTH, &col(color), "none");
        }
    }
}

pub fn import(name: &str, text: &str) -> Result<Drawing> {
    let sch = parse(text)?;
    if sch.head() != "kicad_sch" {
        bail!("not a KiCad schematic (expected a kicad_sch file, found '{}')", sch.head());
    }
    let (width, height) = paper(&sch);
    let mut ctx = Ctx { out: SvgOut::new(width, height, "mm", 1.0), texts: Vec::new(), lib: HashMap::new() };
    if let Some(libs) = sch.child("lib_symbols") {
        for s in libs.children("symbol") {
            if let Some(id) = s.arg(0) {
                ctx.lib.insert(id, s);
            }
        }
    }
    // Page border.
    let m = 10.0;
    ctx.out.poly(&[(m, m), (width - m, m), (width - m, height - m), (m, height - m)], true, 0.15, &col(SYMBOL), "none");

    for item in sch.items().iter().skip(1) {
        match item.head() {
            "symbol" => ctx.symbol(item),
            "wire" | "bus" | "polyline" => {
                let default = match item.head() {
                    "wire" => WIRE,
                    "bus" => BUS,
                    _ => NOTE,
                };
                let (mut w, color) = stroke(item, default);
                if item.head() == "bus" && item.child("stroke").is_none_or(|s| s.child("width").is_none_or(|w| w.num(0) <= 0.0)) {
                    w = 0.3;
                }
                ctx.out.poly(&item.pts(), false, w, &color, "none");
            }
            "bus_entry" => {
                let (x, y, _) = item.at();
                let (dx, dy) = item.xy("size");
                ctx.out.line((x, y), (x + dx, y + dy), DEFAULT_WIDTH, &col(WIRE));
            }
            "junction" => {
                let (x, y, _) = item.at();
                ctx.out.circle((x, y), 0.45, 0.0, "none", &col(WIRE));
            }
            "no_connect" => {
                let (x, y, _) = item.at();
                let s = 0.6;
                ctx.out.line((x - s, y - s), (x + s, y + s), DEFAULT_WIDTH, &col(NOTE));
                ctx.out.line((x - s, y + s), (x + s, y - s), DEFAULT_WIDTH, &col(NOTE));
            }
            "rectangle" => {
                let (a, b) = (item.xy("start"), item.xy("end"));
                let (w, color) = stroke(item, NOTE);
                ctx.out.poly(&[a, (b.0, a.1), b, (a.0, b.1)], true, w, &color, &fill(item, &color));
            }
            "circle" => {
                let (w, color) = stroke(item, NOTE);
                let r = item.child("radius").map(|r| r.num(0)).unwrap_or(0.0);
                ctx.out.circle(item.xy("center"), r, w, &color, &fill(item, &color));
            }
            "arc" => {
                let (w, color) = stroke(item, NOTE);
                arc3(&mut ctx.out, item.xy("start"), item.xy("mid"), item.xy("end"), w, &color);
            }
            "text" => ctx.sheet_text(item, NOTE, false),
            "label" => ctx.sheet_text(item, LABEL, false),
            "hierarchical_label" => ctx.sheet_text(item, GLOBAL, false),
            "global_label" => ctx.sheet_text(item, GLOBAL, true),
            "sheet" => {
                let (x, y, _) = item.at();
                let (w, h) = item.xy("size");
                ctx.out.poly(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true, DEFAULT_WIDTH, &col(SYMBOL), &col(BODY_FILL));
                if let Some(n) = item.children("property").find(|p| matches!(p.arg(0), Some("Sheetname" | "Sheet name"))) {
                    ctx.out.text((x, y - 0.5), n.arg(1).unwrap_or(""), 1.27 * EM, Anchor::Start, false, &col(FIELD));
                }
            }
            _ => {}
        }
    }
    Ok(Drawing { name: name.to_string(), svg: ctx.out.finish(), width, height, texts: ctx.texts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schematic::{LabelDir, PinNames, locate};

    const SCH: &str = r#"(kicad_sch (version 20231120) (generator "eeschema") (paper "A4")
  (lib_symbols
    (symbol "MCU:STM32" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
      (property "Reference" "U" (at 0 12 0) (effects (font (size 1.27 1.27))))
      (symbol "STM32_0_1"
        (rectangle (start -10.16 10.16) (end 10.16 -10.16) (stroke (width 0.254) (type default)) (fill (type background))))
      (symbol "STM32_1_1"
        (pin bidirectional line (at -12.7 5.08 0) (length 2.54) (name "PA0" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
        (pin bidirectional line (at 12.7 5.08 180) (length 2.54) (name "PB3" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
        (pin bidirectional line (at 0 12.7 270) (length 2.54) (name "PC13" (effects (font (size 1.27 1.27)))) (number "3" (effects (font (size 1.27 1.27)))))
        (pin power_in line (at 0 -12.7 90) (length 2.54) (name "VSS" (effects (font (size 1.27 1.27)))) (number "4" (effects (font (size 1.27 1.27)))))
        (pin input line (at -12.7 0 0) (length 2.54) hide (name "PA1" (effects (font (size 1.27 1.27)))) (number "5" (effects (font (size 1.27 1.27)))))))
    (symbol "Device:R" (pin_numbers hide) (pin_names (offset 0))
      (symbol "R_0_1" (rectangle (start -1.016 -2.54) (end 1.016 2.54) (stroke (width 0.254) (type default)) (fill (type none))))
      (symbol "R_1_1"
        (pin passive line (at 0 3.81 270) (length 1.27) (name "~" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27))))))))
  (wire (pts (xy 80 94.92) (xy 87.3 94.92)) (stroke (width 0) (type default)))
  (junction (at 80 94.92) (diameter 0) (color 0 0 0 0))
  (label "LED \"A\"" (at 70 94.92 0) (effects (font (size 1.27 1.27)) (justify left bottom)))
  (global_label "SWDIO" (shape input) (at 130 80 180) (effects (font (size 1.27 1.27)) (justify right)))
  (symbol (lib_id "MCU:STM32") (at 100 100 0) (unit 1)
    (property "Reference" "U1" (at 100 86 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "Package_QFP:LQFP-48" (at 100 100 0) (effects (font (size 1.27 1.27)) hide)))
  (symbol (lib_id "MCU:STM32") (at 200 100 90) (mirror y) (unit 1)
    (property "Reference" "U2" (at 200 86 0) (effects (font (size 1.27 1.27)))))
  (symbol (lib_id "Device:R") (at 60 100 0) (unit 1)
    (property "Value" "10k" (at 62 100 0) (effects (font (size 1.27 1.27)) (justify left))))
)"#;

    #[test]
    fn parses_strings_and_nesting() {
        let sx = parse(r#"(a "b \"c\" (d)" (e 1.5 -2) f)"#).unwrap();
        assert_eq!(sx.arg(0), Some(r#"b "c" (d)"#));
        assert_eq!(sx.child("e").unwrap().num(1), -2.0);
        assert!(sx.has_atom("f"));
        assert!(parse("(a (b)").is_err());
    }

    #[test]
    fn pins_are_placed_from_the_symbol_geometry() {
        let d = import("mcu", SCH).unwrap();
        assert_eq!((d.width, d.height), (297.0, 210.0));
        assert!(d.svg.contains(">U1<") && d.svg.contains(">10k<") && !d.svg.contains("LQFP-48"));
        assert!(d.svg.contains("LED &quot;A&quot;") && d.svg.contains(">SWDIO<"));

        // First instance, unrotated. Library Y is up, the sheet's is down.
        let near = |t: &&TextItem| t.x > 80.0 && t.x < 120.0;
        let find = |s: &str| d.texts.iter().filter(near).find(|t| t.text == s).unwrap_or_else(|| panic!("{s} missing"));
        let pa0 = find("PA0");
        let w = text_width("PA0", 1.27 * EM);
        // Pin end at x=87.3, 2.54 long, name 1.016 further in.
        assert!((pa0.x - (100.0 - 12.7 + 2.54 + 1.016 + w / 2.0)).abs() < 1e-3 && (pa0.y - 94.92).abs() < 1e-3, "{pa0:?}");
        assert_eq!((pa0.dir, pa0.inward), ((1.0, 0.0), Some(1.0)));
        let pb3 = find("PB3");
        assert!(pb3.x > 100.0 && pb3.x < 109.144 && pb3.inward == Some(-1.0));
        // Top pin: the name hangs below the pin and reads upwards.
        let pc13 = find("PC13");
        assert!((pc13.x - 100.0).abs() < 1e-3 && pc13.y > 90.856 && pc13.y < 100.0, "{pc13:?}");
        assert_eq!((pc13.dir, pc13.inward), ((0.0, -1.0), Some(-1.0)));
        assert_eq!(find("VSS").inward, Some(1.0));
        // Hidden pins and unnamed pins give nothing.
        assert!(!d.texts.iter().any(|t| t.text == "PA1" || t.text == "~"));

        // Second instance: rotated 90° and mirrored left-right.
        let pa0b = d.texts.iter().find(|t| t.text == "PA0" && t.x > 150.0).unwrap();
        // Library (-12.7, 5.08) -> rot 90: (-5.08, 12.7) -> mirror y: (5.08, 12.7).
        assert!((pa0b.x - 205.08).abs() < 1e-3 && pa0b.y < 112.7 && pa0b.y > 100.0, "{pa0b:?}");
        assert_eq!(pa0b.dir, (0.0, -1.0));

        // And the nodes end up inside the body, beside the names (first instance only).
        let pkg = crate::schematic::tests::package(&["PA0", "PA1", "PB3", "PC13", "VSS"]);
        let first = Drawing { texts: d.texts.iter().filter(near).cloned().collect(), svg: String::new(), name: d.name.clone(), ..d };
        let out = locate(&first, &PinNames::from_package(&pkg));
        assert_eq!(out.gpios_found, 3);
        let n = out.nodes.iter().find(|n| n.pin == "PB3").unwrap();
        assert_eq!(n.label, LabelDir::Left);
        assert!(n.x * 297.0 > 100.0 && n.x * 297.0 < pb3.x);
    }
}
