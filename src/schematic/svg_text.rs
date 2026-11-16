//! SVG import: the drawing is used as it is, and the positions of its text are
//! read from the `<text>` elements (and from KiCad's stroked-text groups, whose
//! glyphs are plotted as lines with the string in a `<desc>`).

use anyhow::{Context as _, Result};
use resvg::usvg;
use resvg::usvg::roxmltree::{Document, Node, ParsingOptions};

use super::{Drawing, TextItem};

/// 2D affine transform `[a b c d e f]`: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tf(pub [f32; 6]);

impl Tf {
    pub const IDENTITY: Tf = Tf([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }

    /// Transform a direction (no translation).
    pub fn vector(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        (m[0] * x + m[2] * y, m[1] * x + m[3] * y)
    }

    /// `self` applied after `inner`.
    pub fn then(&self, inner: &Tf) -> Tf {
        let (a, b) = (&self.0, &inner.0);
        Tf([
            a[0] * b[0] + a[2] * b[1],
            a[1] * b[0] + a[3] * b[1],
            a[0] * b[2] + a[2] * b[3],
            a[1] * b[2] + a[3] * b[3],
            a[0] * b[4] + a[2] * b[5] + a[4],
            a[1] * b[4] + a[3] * b[5] + a[5],
        ])
    }
}

/// All numbers in a string such as `12,3.5-4e1` or `1.5.5`.
fn numbers(s: &str) -> Vec<f32> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i] == b'-' || b[i] == b'+' {
            i += 1;
        }
        let mut digits = false;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits = true;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                digits = true;
            }
        }
        if digits && i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        if digits {
            if let Ok(v) = s[start..i].parse() {
                out.push(v);
            }
        } else {
            i = start + 1;
        }
    }
    out
}

/// Parse an SVG `transform` attribute.
pub fn parse_transform(s: &str) -> Tf {
    let mut tf = Tf::IDENTITY;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let v = numbers(&rest[open + 1..open + close]);
        let g = |i: usize, default: f32| v.get(i).copied().unwrap_or(default);
        let step = match name {
            "matrix" if v.len() >= 6 => Tf([v[0], v[1], v[2], v[3], v[4], v[5]]),
            "translate" => Tf([1.0, 0.0, 0.0, 1.0, g(0, 0.0), g(1, 0.0)]),
            "scale" => Tf([g(0, 1.0), 0.0, 0.0, g(1, g(0, 1.0)), 0.0, 0.0]),
            "rotate" => {
                let (sin, cos) = g(0, 0.0).to_radians().sin_cos();
                let (cx, cy) = (g(1, 0.0), g(2, 0.0));
                Tf([cos, sin, -sin, cos, cx - cos * cx + sin * cy, cy - sin * cx - cos * cy])
            }
            "skewX" => Tf([1.0, 0.0, g(0, 0.0).to_radians().tan(), 1.0, 0.0, 0.0]),
            "skewY" => Tf([1.0, g(0, 0.0).to_radians().tan(), 0.0, 1.0, 0.0, 0.0]),
            _ => Tf::IDENTITY,
        };
        tf = tf.then(&step);
        rest = &rest[open + close + 1..];
    }
    tf
}

/// A presentation property, from the attribute or the `style` attribute.
fn property<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    if let Some(style) = node.attribute("style") {
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':') {
                if k.trim() == name {
                    return Some(v.trim());
                }
            }
        }
    }
    node.attribute(name)
}

/// A length in user units (`px`), given the inherited font size for `em`.
fn length(s: &str, font_size: f32) -> Option<f32> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic() || c == '%').unwrap_or(s.len());
    let v: f32 = s[..split].trim().parse().ok()?;
    Some(match &s[split..] {
        "pt" => v * 96.0 / 72.0,
        "mm" => v * 96.0 / 25.4,
        "cm" => v * 96.0 / 2.54,
        "in" => v * 96.0,
        "pc" => v * 16.0,
        "em" => v * font_size,
        "%" => v / 100.0 * font_size,
        _ => v,
    })
}

#[derive(Clone, Copy)]
struct Inherited {
    tf: Tf,
    font_size: f32,
    /// 0 start, 0.5 middle, 1 end.
    anchor: f32,
}

struct Extractor {
    texts: Vec<TextItem>,
    /// Indices of items that came from stroked-text groups (exact geometry).
    stroked: Vec<usize>,
}

impl Extractor {
    fn walk(&mut self, node: Node, mut inh: Inherited) {
        if !node.is_element() {
            return;
        }
        let name = node.tag_name().name();
        if matches!(name, "defs" | "symbol" | "clipPath" | "mask" | "pattern" | "marker" | "metadata" | "title" | "style" | "script") {
            return;
        }
        if property(node, "display") == Some("none") {
            return;
        }
        if let Some(t) = node.attribute("transform") {
            inh.tf = inh.tf.then(&parse_transform(t));
        }
        if let Some(fs) = property(node, "font-size").and_then(|s| length(s, inh.font_size)) {
            inh.font_size = fs;
        }
        match property(node, "text-anchor") {
            Some("middle") => inh.anchor = 0.5,
            Some("end") => inh.anchor = 1.0,
            Some("start") => inh.anchor = 0.0,
            _ => {}
        }
        match name {
            "text" => self.text(node, inh),
            "g" if node.attribute("class").is_some_and(|c| c.split_whitespace().any(|c| c == "stroked-text")) => {
                self.stroked_text(node, inh);
            }
            _ => {
                for child in node.children() {
                    self.walk(child, inh);
                }
            }
        }
    }

    /// `<text>` with optional `<tspan>` children. Glyph widths are estimated,
    /// which is plenty to tell which pin a node belongs to.
    fn text(&mut self, node: Node, inh: Inherited) {
        let first = |n: Node, a: &str, fs: f32| n.attribute(a).and_then(|s| s.split([' ', ',']).find(|p| !p.is_empty())).and_then(|s| length(s, fs));
        let mut pen = (first(node, "x", inh.font_size).unwrap_or(0.0), first(node, "y", inh.font_size).unwrap_or(0.0));
        pen.0 += first(node, "dx", inh.font_size).unwrap_or(0.0);
        pen.1 += first(node, "dy", inh.font_size).unwrap_or(0.0);
        let single = node.children().filter(|c| c.is_text() || c.is_element()).count() == 1;
        let text_length = if single { first(node, "textLength", inh.font_size) } else { None };
        self.runs(node, inh, &mut pen, text_length);
    }

    fn runs(&mut self, node: Node, inh: Inherited, pen: &mut (f32, f32), text_length: Option<f32>) {
        for child in node.children() {
            if child.is_text() {
                let raw = child.text().unwrap_or("");
                let s = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                if s.is_empty() {
                    continue;
                }
                let width = text_length.unwrap_or_else(|| super::svg_out::text_width(&s, inh.font_size));
                let start = pen.0 - width * inh.anchor;
                self.push(&s, start, pen.1, width, inh);
                pen.0 = start + width;
            } else if child.is_element() && matches!(child.tag_name().name(), "tspan" | "a" | "textPath") {
                let mut sub = inh;
                if let Some(t) = child.attribute("transform") {
                    sub.tf = sub.tf.then(&parse_transform(t));
                }
                if let Some(fs) = property(child, "font-size").and_then(|s| length(s, inh.font_size)) {
                    sub.font_size = fs;
                }
                match property(child, "text-anchor") {
                    Some("middle") => sub.anchor = 0.5,
                    Some("end") => sub.anchor = 1.0,
                    Some("start") => sub.anchor = 0.0,
                    _ => {}
                }
                let first = |a: &str| child.attribute(a).and_then(|s| s.split([' ', ',']).find(|p| !p.is_empty())).and_then(|s| length(s, sub.font_size));
                if let Some(x) = first("x") {
                    pen.0 = x;
                }
                if let Some(y) = first("y") {
                    pen.1 = y;
                }
                pen.0 += first("dx").unwrap_or(0.0);
                pen.1 += first("dy").unwrap_or(0.0);
                self.runs(child, sub, pen, first("textLength").or(text_length));
            }
        }
    }

    fn push(&mut self, s: &str, start: f32, baseline: f32, width: f32, inh: Inherited) {
        let fs = inh.font_size;
        let (x, y) = inh.tf.apply(start + width / 2.0, baseline - fs * 0.36);
        let along = inh.tf.vector(1.0, 0.0);
        let across = inh.tf.vector(0.0, 1.0);
        let sx = along.0.hypot(along.1);
        let sy = across.0.hypot(across.1);
        if sx <= 0.0 || sy <= 0.0 || !x.is_finite() || !y.is_finite() {
            return;
        }
        self.texts.push(TextItem {
            text: s.to_string(),
            x,
            y,
            len: width * sx,
            height: fs * 0.72 * sy,
            dir: (along.0 / sx, along.1 / sx),
            inward: None,
        });
    }

    /// KiCad: `<g class="stroked-text"><desc>PA0</desc><path d="M… L…"/>…</g>`.
    fn stroked_text(&mut self, node: Node, inh: Inherited) {
        let Some(desc) = node.children().find(|c| c.has_tag_name("desc")).and_then(|d| d.text()) else { return };
        let s = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.is_empty() {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for el in node.descendants().filter(|n| n.is_element()) {
            let coords = match el.tag_name().name() {
                // Only absolute move/line commands, which is all a stroke font needs.
                "path" => match el.attribute("d") {
                    Some(d) if !d.chars().any(|c| c.is_ascii_alphabetic() && !matches!(c, 'M' | 'L' | 'Z' | 'z' | 'e' | 'E')) => numbers(d),
                    _ => continue,
                },
                "polyline" | "polygon" => numbers(el.attribute("points").unwrap_or("")),
                _ => continue,
            };
            for p in coords.chunks_exact(2) {
                let (x, y) = inh.tf.apply(p[0], p[1]);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        if x0 > x1 {
            return;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        // A string that is taller than wide was plotted vertically (bottom to top).
        let vertical = h > w * 1.2 && s.chars().count() > 1;
        self.stroked.push(self.texts.len());
        self.texts.push(TextItem {
            text: s,
            x: (x0 + x1) / 2.0,
            y: (y0 + y1) / 2.0,
            len: if vertical { h } else { w },
            height: if vertical { w } else { h }.max(0.001),
            dir: if vertical { (0.0, -1.0) } else { (1.0, 0.0) },
            inward: None,
        });
    }
}

/// Text items of an SVG, in the coordinate system of its rendered size (`width` × `height`).
pub fn extract(svg: &str, width: f32, height: f32) -> Result<Vec<TextItem>> {
    let opt = ParsingOptions { allow_dtd: true, ..Default::default() };
    let doc = Document::parse_with_options(svg, opt).context("parsing the SVG")?;
    let root = doc.root_element();

    // viewBox -> rendered size (preserveAspectRatio: xMidYMid meet unless "none").
    let mut tf = Tf::IDENTITY;
    if let Some(vb) = root.attribute("viewBox").map(numbers).filter(|v| v.len() == 4 && v[2] > 0.0 && v[3] > 0.0) {
        let (mut sx, mut sy) = (width / vb[2], height / vb[3]);
        if root.attribute("preserveAspectRatio").map(str::trim) != Some("none") {
            sx = sx.min(sy);
            sy = sx;
        }
        let ox = (width - vb[2] * sx) / 2.0 - vb[0] * sx;
        let oy = (height - vb[3] * sy) / 2.0 - vb[1] * sy;
        tf = Tf([sx, 0.0, 0.0, sy, ox, oy]);
    }

    let mut ex = Extractor { texts: Vec::new(), stroked: Vec::new() };
    let inh = Inherited { tf, font_size: 16.0, anchor: 0.0 };
    for child in root.children() {
        ex.walk(child, inh);
    }

    // KiCad writes each string twice: as an invisible <text> (for searching) and
    // as strokes. Keep the strokes, whose extent is exact.
    let stroked: Vec<TextItem> = ex.stroked.iter().map(|&i| ex.texts[i].clone()).collect();
    let mut i = 0;
    let stroked_idx: std::collections::BTreeSet<usize> = ex.stroked.iter().copied().collect();
    ex.texts.retain(|t| {
        let keep = stroked_idx.contains(&i)
            || !stroked.iter().any(|s| s.text == t.text && (s.x - t.x).hypot(s.y - t.y) < (s.len + s.height * 2.0).max(t.height * 2.0));
        i += 1;
        keep
    });
    Ok(ex.texts)
}

pub fn import(name: &str, svg: String) -> Result<Drawing> {
    // usvg decides the rendered size (units, missing viewBox, percentages…), so
    // node positions agree with what is drawn.
    let tree = usvg::Tree::from_str(&svg, &usvg::Options::default()).context("reading the SVG")?;
    let (width, height) = (tree.size().width(), tree.size().height());
    let texts = extract(&svg, width, height)?;
    Ok(Drawing { name: name.to_string(), svg, width, height, texts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(texts: &'a [TextItem], s: &str) -> &'a TextItem {
        texts.iter().find(|t| t.text == s).unwrap_or_else(|| panic!("{s} not found in {texts:?}"))
    }

    #[test]
    fn transforms_compose() {
        let t = parse_transform("translate(10,20) scale(2)");
        assert_eq!(t.apply(1.0, 1.0), (12.0, 22.0));
        let r = parse_transform("rotate(90 10 10)");
        let (x, y) = r.apply(20.0, 10.0);
        assert!((x - 10.0).abs() < 1e-4 && (y - 20.0).abs() < 1e-4);
        assert_eq!(numbers("1.5-2,3e1.5"), vec![1.5, -2.0, 30.0, 0.5]);
    }

    #[test]
    fn text_positions_follow_viewbox_groups_and_anchors() {
        let svg = r#"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200mm" height="100mm" viewBox="0 0 2000 1000">
  <defs><text x="1" y="1">PZ9</text></defs>
  <g transform="translate(100 200)" font-size="20">
    <text x="50" y="40">PA0</text>
    <text x="400" y="40" text-anchor="end">PB12</text>
    <text x="0" y="300" style="font-size:10px"><tspan x="10" y="310">PC1</tspan><tspan x="10" y="330">PC2</tspan></text>
  </g>
  <text transform="rotate(-90 1000 500)" x="1000" y="500" font-size="20">PD2</text>
</svg>"#;
        // 200mm at 96 dpi; the scale from viewBox units is the same on both axes.
        let d = import("t", svg.to_string()).unwrap();
        let k = d.width / 2000.0;
        assert!((d.width - 200.0 * 96.0 / 25.4).abs() < 0.01 && (d.height - d.width / 2.0).abs() < 0.01);
        assert!(!d.texts.iter().any(|t| t.text == "PZ9"));

        let w = crate::schematic::svg_out::text_width("PA0", 20.0);
        let pa0 = find(&d.texts, "PA0");
        assert!((pa0.x - (150.0 + w / 2.0) * k).abs() < 0.01, "{pa0:?}");
        assert!((pa0.y - (240.0 - 7.2) * k).abs() < 0.01);
        assert!((pa0.len - w * k).abs() < 0.01 && pa0.dir == (1.0, 0.0));

        let pb12 = find(&d.texts, "PB12");
        let w = crate::schematic::svg_out::text_width("PB12", 20.0);
        assert!((pb12.x - (500.0 - w / 2.0) * k).abs() < 0.01);

        assert!((find(&d.texts, "PC2").y - (530.0 - 3.6) * k).abs() < 0.01);

        // Rotated text reads bottom-to-top, upwards from its anchor.
        let pd2 = find(&d.texts, "PD2");
        assert!(pd2.dir.0.abs() < 1e-4 && (pd2.dir.1 + 1.0).abs() < 1e-4, "{pd2:?}");
        assert!(pd2.y < 500.0 * k && (pd2.x - (1000.0 - 7.2) * k).abs() < 0.01);
    }

    #[test]
    fn kicad_stroked_text_wins_over_hidden_text() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100">
<text x="10" y="20" textLength="9" font-size="4" opacity="0">PA5</text>
<g class="stroked-text"><desc>PA5</desc>
<path d="M10 20 L10 16 L12 16" /><path d="M13 20 L19 16" /></g>
<g class="stroked-text"><desc>PB7</desc><path d="M50 60 L50 40 M53 60 L53 40"/></g>
</svg>"#;
        let texts = extract(svg, 100.0, 100.0).unwrap();
        assert_eq!(texts.len(), 2, "{texts:?}");
        let pa5 = find(&texts, "PA5");
        assert_eq!((pa5.x, pa5.y, pa5.len, pa5.height), (14.5, 18.0, 9.0, 4.0));
        let pb7 = find(&texts, "PB7");
        assert_eq!((pb7.dir, pb7.len), ((0.0, -1.0), 20.0));
    }
}
