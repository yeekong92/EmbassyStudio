//! Altium Designer `.SchDoc` import.
//!
//! A SchDoc is an OLE compound file whose `FileHeader` stream is a list of
//! records, each a `|KEY=VALUE|…` property list. Primitives are stored in sheet
//! coordinates (units of 10 mil, Y up), so drawing them needs no symbol
//! transforms. The sheet is redrawn as SVG and every pin is reported with the
//! position of its name.
//!
//! Only geometry is read: nets are not traced and components are not interpreted.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

use super::svg_out::{Anchor, SvgOut, rgb, text_width};
use super::{Drawing, TextItem};

type Record = HashMap<String, String>;

/// Split the `FileHeader` stream into records. Binary records are kept as empty
/// maps so that `OWNERINDEX` keeps pointing at the right record.
fn records(stream: &[u8]) -> Vec<Record> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= stream.len() {
        let len = u32::from_le_bytes([stream[i], stream[i + 1], stream[i + 2], 0]) as usize;
        let binary = stream[i + 3] != 0;
        i += 4;
        let end = (i + len).min(stream.len());
        let mut rec = Record::new();
        if !binary {
            for part in stream[i..end].split(|b| *b == b'|') {
                let Some(eq) = part.iter().position(|b| *b == b'=') else { continue };
                let key = String::from_utf8_lossy(&part[..eq]).to_ascii_uppercase();
                let raw = &part[eq + 1..];
                let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
                match key.strip_prefix("%UTF8%") {
                    // The UTF-8 spelling of a value replaces the ANSI one.
                    Some(k) => {
                        rec.insert(k.to_string(), String::from_utf8_lossy(raw).into_owned());
                    }
                    None => {
                        rec.entry(key).or_insert_with(|| raw.iter().map(|b| *b as char).collect());
                    }
                }
            }
        }
        out.push(rec);
        i = end;
    }
    out
}

trait Rec {
    fn int(&self, key: &str) -> Option<i64>;
    fn text(&self, key: &str) -> &str;
    fn flag(&self, key: &str) -> bool;
    /// A coordinate: integer part plus an optional `_FRAC` part in 1/100000.
    fn coord(&self, key: &str) -> f32;
    fn point(&self, prefix: &str) -> (f32, f32);
    fn color(&self, key: &str, default: (u8, u8, u8)) -> String;
}

impl Rec for Record {
    fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(|v| v.trim().parse().ok())
    }
    fn text(&self, key: &str) -> &str {
        self.get(key).map(String::as_str).unwrap_or("")
    }
    fn flag(&self, key: &str) -> bool {
        self.text(key).eq_ignore_ascii_case("T")
    }
    fn coord(&self, key: &str) -> f32 {
        let whole = self.int(key).unwrap_or(0) as f32;
        let frac = self.int(&format!("{key}_FRAC")).unwrap_or(0) as f32;
        whole + frac / 100_000.0
    }
    fn point(&self, prefix: &str) -> (f32, f32) {
        (self.coord(&format!("{prefix}.X")), self.coord(&format!("{prefix}.Y")))
    }
    /// Colours are stored as `0xBBGGRR`.
    fn color(&self, key: &str, default: (u8, u8, u8)) -> String {
        match self.int(key) {
            Some(c) => rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8),
            None => rgb(default.0, default.1, default.2),
        }
    }
}

/// Sheet sizes in units of 10 mil, by `SHEETSTYLE`.
const SHEETS: [(f32, f32); 18] = [
    (1150.0, 760.0),  // A4
    (1550.0, 1110.0), // A3
    (2230.0, 1570.0), // A2
    (3150.0, 2230.0), // A1
    (4460.0, 3150.0), // A0
    (950.0, 750.0),   // A
    (1500.0, 950.0),  // B
    (2000.0, 1500.0), // C
    (3200.0, 2000.0), // D
    (4200.0, 3200.0), // E
    (1100.0, 850.0),  // Letter
    (1400.0, 850.0),  // Legal
    (1700.0, 1100.0), // Tabloid
    (990.0, 790.0),   // OrCAD A
    (1540.0, 990.0),  // OrCAD B
    (2060.0, 1560.0), // OrCAD C
    (3260.0, 2060.0), // OrCAD D
    (4280.0, 3280.0), // OrCAD E
];

const BLUE: (u8, u8, u8) = (0, 0, 128);
const BLACK: (u8, u8, u8) = (0, 0, 0);
const RED: (u8, u8, u8) = (128, 0, 0);
const LINE: f32 = 1.0;

/// Line width from `LINEWIDTH` (0 smallest … 3 large).
fn line_width(r: &Record) -> f32 {
    match r.int("LINEWIDTH").unwrap_or(1) {
        0 => 0.5,
        1 => 1.0,
        2 => 2.0,
        _ => 4.0,
    }
}

struct Sheet {
    out: SvgOut,
    height: f32,
    /// Font sizes by `FONTID` (1-based).
    fonts: Vec<f32>,
    texts: Vec<TextItem>,
}

impl Sheet {
    /// Sheet coordinates (Y up) to SVG (Y down).
    fn p(&self, p: (f32, f32)) -> (f32, f32) {
        (p.0, self.height - p.1)
    }

    /// Em size of a record's font, in sheet units. Altium's 10 "pt" default is
    /// drawn about one grid step (100 mil) tall.
    fn em(&self, r: &Record) -> f32 {
        let id = r.int("FONTID").unwrap_or(1).max(1) as usize;
        self.fonts.get(id - 1).copied().unwrap_or(10.0) * 1.05
    }

    fn points(&self, r: &Record) -> Vec<(f32, f32)> {
        let n = r.int("LOCATIONCOUNT").unwrap_or(0).max(0) as usize;
        (1..=n).map(|i| self.p((r.coord(&format!("X{i}")), r.coord(&format!("Y{i}"))))).collect()
    }

    /// Text anchored bottom-left at `LOCATION`, turned by `ORIENTATION` quarter turns.
    fn text(&mut self, r: &Record, text: &str, color: &str) {
        let at = self.p(r.point("LOCATION"));
        let em = self.em(r);
        let (vertical, anchor) = match r.int("ORIENTATION").unwrap_or(0) & 3 {
            0 => (false, Anchor::Start),
            1 => (true, Anchor::Start),
            2 => (false, Anchor::End),
            _ => (true, Anchor::End),
        };
        // Strip the bar-over-name markers (`N\R\S\T\`).
        let text: String = text.chars().filter(|c| *c != '\\').collect();
        let lift = em * 0.12;
        let at = if vertical { (at.0 - lift, at.1) } else { (at.0, at.1 - lift) };
        self.out.text(at, &text, em, anchor, vertical, color);
    }

    fn pin(&mut self, r: &Record) {
        let conglomerate = r.int("PINCONGLOMERATE").unwrap_or(0);
        // Direction from the body towards the electrical end.
        let dir = match conglomerate & 3 {
            0 => (1.0, 0.0),
            1 => (0.0, -1.0),
            2 => (-1.0, 0.0),
            _ => (0.0, 1.0),
        };
        let body = self.p(r.point("LOCATION"));
        let length = r.coord("PINLENGTH");
        let hot = (body.0 + dir.0 * length, body.1 + dir.1 * length);
        self.out.line(body, hot, LINE, &r.color("COLOR", BLACK));

        let em = 10.5;
        let vertical = dir.1 != 0.0;
        let across = if vertical { (-1.0, 0.0) } else { (0.0, -1.0) };
        if conglomerate & 0x10 != 0 {
            let at = (body.0 + dir.0 * 8.0 + across.0 * 1.5, body.1 + dir.1 * 8.0 + across.1 * 1.5);
            // Reads left-to-right / bottom-to-top, starting near the body.
            let forward = if vertical { dir.1 < 0.0 } else { dir.0 > 0.0 };
            self.out.text(at, r.text("DESIGNATOR"), em, if forward { Anchor::Start } else { Anchor::End }, vertical, &rgb(0, 0, 0));
        }

        let name: String = r.text("NAME").chars().filter(|c| *c != '\\').collect();
        if name.trim().is_empty() {
            return;
        }
        // The name is printed inside the body, 50 mil in from the pin.
        let inward = (-dir.0, -dir.1);
        let start = (body.0 + inward.0 * 5.0, body.1 + inward.1 * 5.0);
        let len = text_width(&name, em);
        let forward = if vertical { inward.1 < 0.0 } else { inward.0 > 0.0 };
        if conglomerate & 0x08 != 0 {
            let shift = em * 0.36;
            let at = if vertical { (start.0 + shift, start.1) } else { (start.0, start.1 + shift) };
            self.out.text(at, &name, em, if forward { Anchor::Start } else { Anchor::End }, vertical, &rgb(0, 0, 0));
        }
        self.texts.push(TextItem {
            text: name,
            x: start.0 + inward.0 * len / 2.0,
            y: start.1 + inward.1 * len / 2.0,
            len,
            height: em * 0.72,
            dir: if vertical { (0.0, -1.0) } else { (1.0, 0.0) },
            inward: Some(if forward { 1.0 } else { -1.0 }),
        });
    }

    fn draw(&mut self, r: &Record) {
        let kind = r.int("RECORD").unwrap_or(-1);
        let fill = |r: &Record| if r.flag("ISSOLID") { r.color("AREACOLOR", (255, 255, 176)) } else { "none".to_string() };
        match kind {
            2 => self.pin(r),
            // Label, net label, designator, sheet name / file name.
            4 | 25 | 34 | 32 | 33 => {
                let text = r.text("TEXT").to_string();
                if !r.flag("ISHIDDEN") && !text.starts_with('=') {
                    self.text(r, &text, &r.color("COLOR", if kind == 25 { RED } else { BLUE }));
                }
            }
            // Parameters are hidden unless they are the visible comment / value.
            41 => {
                let text = r.text("TEXT").to_string();
                if !r.flag("ISHIDDEN") && !text.is_empty() && !text.starts_with('=') && text != "*" {
                    self.text(r, &text, &r.color("COLOR", BLUE));
                }
            }
            // Polyline, wire, bus.
            6 | 27 | 26 => {
                let width = if kind == 26 { 3.0 } else { line_width(r) };
                self.out.poly(&self.points(r), false, width, &r.color("COLOR", BLUE), "none");
            }
            7 => self.out.poly(&self.points(r), true, line_width(r), &r.color("COLOR", BLUE), &fill(r)),
            13 => {
                let (a, b) = (self.p(r.point("LOCATION")), self.p(r.point("CORNER")));
                self.out.line(a, b, line_width(r), &r.color("COLOR", BLUE));
            }
            // Rectangle, rounded rectangle, sheet symbol.
            14 | 10 | 15 => {
                let a = r.point("LOCATION");
                let b = if kind == 15 { (a.0 + r.coord("XSIZE"), a.1 - r.coord("YSIZE")) } else { r.point("CORNER") };
                let (a, b) = (self.p(a), self.p(b));
                let fill = if kind == 15 { r.color("AREACOLOR", (255, 255, 176)) } else { fill(r) };
                self.out.poly(&[a, (b.0, a.1), b, (a.0, b.1)], true, line_width(r), &r.color("COLOR", RED), &fill);
            }
            8 => {
                let rx = r.coord("RADIUS");
                let ry = if r.contains_key("SECONDARYRADIUS") { r.coord("SECONDARYRADIUS") } else { rx };
                self.out.ellipse(self.p(r.point("LOCATION")), rx, ry, line_width(r), &r.color("COLOR", BLUE), &fill(r));
            }
            12 => {
                let c = self.p(r.point("LOCATION"));
                let radius = r.coord("RADIUS");
                let a0: f32 = r.text("STARTANGLE").trim().parse().unwrap_or(0.0);
                let a1: f32 = r.text("ENDANGLE").trim().parse().unwrap_or(360.0);
                let span = (a1 - a0).rem_euclid(360.0);
                let color = r.color("COLOR", BLUE);
                if span < 0.01 {
                    self.out.circle(c, radius, line_width(r), &color, "none");
                } else {
                    // Counter-clockwise with Y up, which is sweep flag 0 once Y is flipped.
                    let at = |deg: f32| (c.0 + radius * deg.to_radians().cos(), c.1 - radius * deg.to_radians().sin());
                    self.out.arc(at(a0), at(a1), radius, span > 180.0, false, line_width(r), &color);
                }
            }
            // Power port: a stub and the net name.
            17 => {
                let at = self.p(r.point("LOCATION"));
                let dir = match r.int("ORIENTATION").unwrap_or(0) & 3 {
                    0 => (1.0, 0.0),
                    1 => (0.0, -1.0),
                    2 => (-1.0, 0.0),
                    _ => (0.0, 1.0),
                };
                let color = r.color("COLOR", RED);
                let tip = (at.0 + dir.0 * 10.0, at.1 + dir.1 * 10.0);
                let bar = (dir.1 * 5.0, dir.0 * 5.0);
                self.out.line(at, tip, LINE, &color);
                self.out.line((tip.0 - bar.0, tip.1 - bar.1), (tip.0 + bar.0, tip.1 + bar.1), LINE, &color);
                if !r.flag("ISHIDDEN") && r.text("SHOWNETNAME") != "F" {
                    let label = (tip.0 + dir.0 * 3.0, tip.1 + dir.1 * 3.0 + if dir.1 == 0.0 { 3.5 } else if dir.1 > 0.0 { 9.0 } else { 0.0 });
                    let anchor = if dir.0 > 0.0 { Anchor::Start } else if dir.0 < 0.0 { Anchor::End } else { Anchor::Middle };
                    self.out.text(label, r.text("TEXT"), 10.5, anchor, false, &color);
                }
            }
            // Port: a box with its name.
            18 => {
                let at = self.p(r.point("LOCATION"));
                let (w, h) = (r.coord("WIDTH").max(10.0), r.coord("HEIGHT").max(10.0));
                let color = r.color("COLOR", RED);
                self.out.poly(
                    &[(at.0, at.1 - h / 2.0), (at.0 + w, at.1 - h / 2.0), (at.0 + w, at.1 + h / 2.0), (at.0, at.1 + h / 2.0)],
                    true,
                    LINE,
                    &color,
                    &r.color("AREACOLOR", (255, 255, 176)),
                );
                self.out.text((at.0 + w / 2.0, at.1 + 3.6), r.text("NAME"), 10.5, Anchor::Middle, false, &r.color("TEXTCOLOR", RED));
            }
            29 => self.out.circle(self.p(r.point("LOCATION")), 2.0, 0.0, "none", &r.color("COLOR", RED)),
            22 => {
                let c = self.p(r.point("LOCATION"));
                let color = r.color("COLOR", RED);
                self.out.line((c.0 - 3.0, c.1 - 3.0), (c.0 + 3.0, c.1 + 3.0), LINE, &color);
                self.out.line((c.0 - 3.0, c.1 + 3.0), (c.0 + 3.0, c.1 - 3.0), LINE, &color);
            }
            _ => {}
        }
    }
}

/// Redraw the records of a `FileHeader` stream.
fn convert(name: &str, stream: &[u8]) -> Result<Drawing> {
    let all = records(stream);
    if !all.first().is_some_and(|h| h.text("HEADER").contains("Schematic")) {
        bail!("not an Altium schematic document (no schematic header record)");
    }
    // `OWNERINDEX` counts from the record after the header.
    let objects = &all[1..];
    let sheet = objects.iter().find(|r| r.int("RECORD") == Some(31));

    let (mut width, mut height) = match sheet {
        Some(s) if s.flag("USECUSTOMSHEET") => (s.coord("CUSTOMX"), s.coord("CUSTOMY")),
        Some(s) => SHEETS[(s.int("SHEETSTYLE").unwrap_or(0).max(0) as usize).min(SHEETS.len() - 1)],
        None => SHEETS[0],
    };
    if sheet.is_some_and(|s| s.int("WORKSPACEORIENTATION") == Some(1)) {
        std::mem::swap(&mut width, &mut height);
    }
    if width < 10.0 || height < 10.0 {
        (width, height) = SHEETS[0];
    }
    let fonts: Vec<f32> = match sheet {
        Some(s) => (1..=s.int("FONTIDCOUNT").unwrap_or(0).clamp(0, 512)).map(|i| s.int(&format!("SIZE{i}")).unwrap_or(10) as f32).collect(),
        None => Vec::new(),
    };

    // 10 mil per unit: one unit is 0.254 mm on paper.
    let mut out = Sheet { out: SvgOut::new(width, height, "mm", 0.254), height, fonts, texts: Vec::new() };
    out.out.poly(&[(2.0, 2.0), (width - 2.0, 2.0), (width - 2.0, height - 2.0), (2.0, height - 2.0)], true, 1.0, &rgb(0, 0, 0), "none");

    // A component shows one of its parts in one display mode; primitives of the
    // other parts are stored alongside and must be skipped.
    let visible = |r: &Record| -> bool {
        let Some(owner) = r.int("OWNERINDEX").and_then(|i| objects.get(i as usize)) else { return true };
        if owner.int("RECORD") != Some(1) {
            return true;
        }
        let part = r.int("OWNERPARTID").unwrap_or(-1);
        let mode = r.int("OWNERPARTDISPLAYMODE").unwrap_or(0);
        (part <= 0 || part == owner.int("CURRENTPARTID").unwrap_or(1)) && mode == owner.int("DISPLAYMODE").unwrap_or(0)
    };
    // Filled shapes first so they never cover lines and text.
    for pass in 0..2 {
        for r in objects.iter().filter(|r| visible(r)) {
            let shape = matches!(r.int("RECORD"), Some(7 | 8 | 10 | 14 | 15 | 18));
            if shape == (pass == 0) {
                out.draw(r);
            }
        }
    }
    Ok(Drawing { name: name.to_string(), svg: out.out.finish(), width, height, texts: out.texts })
}

pub fn import(name: &str, path: &Path) -> Result<Drawing> {
    let mut file = cfb::open(path).with_context(|| format!("{} is not an Altium compound document", path.display()))?;
    let mut stream = Vec::new();
    file.open_stream("/FileHeader")
        .context("no FileHeader stream (is this a schematic document?)")?
        .read_to_end(&mut stream)?;
    convert(name, &stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(props: &str) -> Vec<u8> {
        let mut body = props.as_bytes().to_vec();
        body.push(0);
        let mut out = (body.len() as u32).to_le_bytes().to_vec();
        out.extend(body);
        out
    }

    fn sample() -> Vec<u8> {
        let mut s = Vec::new();
        for r in [
            "|HEADER=Protel for Windows - Schematic Capture Binary File Version 5.0|WEIGHT=9",
            // 0: sheet, A4
            "|RECORD=31|FONTIDCOUNT=1|SIZE1=10|FONTNAME1=Times New Roman|SHEETSTYLE=0",
            // 1: component, showing part 1
            "|RECORD=1|LIBREFERENCE=STM32|PARTCOUNT=3|DISPLAYMODECOUNT=1|OWNERINDEX=0|LOCATION.X=400|LOCATION.Y=400|CURRENTPARTID=1",
            "|RECORD=14|OWNERINDEX=1|OWNERPARTID=1|LOCATION.X=400|LOCATION.Y=300|CORNER.X=600|CORNER.Y=500|ISSOLID=T|AREACOLOR=11599871|COLOR=128",
            // Left pin: points left (2), name and designator shown.
            "|RECORD=2|OWNERINDEX=1|OWNERPARTID=1|NAME=PA0|DESIGNATOR=10|PINLENGTH=30|PINCONGLOMERATE=58|LOCATION.X=400|LOCATION.Y=450",
            // Right pin with a fractional coordinate and an overlined name.
            "|RECORD=2|OWNERINDEX=1|OWNERPARTID=1|NAME=N\\R\\S\\T\\|DESIGNATOR=7|PINLENGTH=30|PINCONGLOMERATE=56|LOCATION.X=600|LOCATION.Y=450|LOCATION.Y_FRAC=50000",
            // Bottom pin: points down (3).
            "|RECORD=2|OWNERINDEX=1|OWNERPARTID=1|NAME=PB3|DESIGNATOR=55|PINLENGTH=30|PINCONGLOMERATE=59|LOCATION.X=500|LOCATION.Y=300",
            // A pin of part 2, which is not on this sheet.
            "|RECORD=2|OWNERINDEX=1|OWNERPARTID=2|NAME=PC0|DESIGNATOR=8|PINLENGTH=30|PINCONGLOMERATE=58|LOCATION.X=400|LOCATION.Y=350",
            "|RECORD=27|OWNERINDEX=0|LINEWIDTH=1|COLOR=8388608|LOCATIONCOUNT=2|X1=300|Y1=450|X2=370|Y2=450",
            "|RECORD=25|OWNERINDEX=0|LOCATION.X=300|LOCATION.Y=450|COLOR=128|FONTID=1|TEXT=LED|%UTF8%TEXT=LED_µ",
        ] {
            s.extend(record(r));
        }
        s
    }

    #[test]
    fn redraws_records_and_reports_pin_names() {
        let d = convert("board", &sample()).unwrap();
        assert_eq!((d.width, d.height), (1150.0, 760.0));
        assert!(d.svg.contains(">LED_µ<") && d.svg.contains(">PA0<") && d.svg.contains(">NRST<"));
        // BGR colour 11599871 = 0xB0FFFF.
        assert!(d.svg.contains("#ffffb0"));

        let find = |s: &str| d.texts.iter().find(|t| t.text == s).unwrap_or_else(|| panic!("{s} missing"));
        // Y is flipped (760 - 450) and the name starts 5 units inside the body.
        let pa0 = find("PA0");
        let w = text_width("PA0", 10.5);
        assert!((pa0.x - (405.0 + w / 2.0)).abs() < 1e-3 && (pa0.y - 310.0).abs() < 1e-3, "{pa0:?}");
        assert_eq!((pa0.dir, pa0.inward), ((1.0, 0.0), Some(1.0)));
        let nrst = find("NRST");
        assert!(nrst.x < 595.0 && nrst.x > 500.0 && (nrst.y - 309.5).abs() < 1e-3 && nrst.inward == Some(-1.0), "{nrst:?}");
        // Bottom pin: the name runs up into the body.
        let pb3 = find("PB3");
        assert!((pb3.x - 500.0).abs() < 1e-3 && pb3.y < 455.0 && pb3.y > 400.0, "{pb3:?}");
        assert_eq!((pb3.dir, pb3.inward), ((0.0, -1.0), Some(1.0)));
        // Other parts of a multi-part component are not drawn.
        assert!(!d.texts.iter().any(|t| t.text == "PC0"));
    }

    #[test]
    fn reads_a_compound_file() {
        let dir = std::env::temp_dir().join(format!("embassy-studio-altium-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Board.SchDoc");
        {
            use std::io::Write;
            let mut f = cfb::create(&path).unwrap();
            f.create_stream("/FileHeader").unwrap().write_all(&sample()).unwrap();
            f.flush().unwrap();
        }
        let d = import("Board", &path).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(d.texts.len(), 3);

        assert!(convert("x", &record("|HEADER=Protel for Windows - PCB Binary")).is_err());
    }
}
