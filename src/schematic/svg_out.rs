//! A tiny SVG writer for the drawings generated from KiCad and Altium files.

use std::fmt::Write as _;

pub const FONT: &str = "Arial, Helvetica, sans-serif";

#[derive(Clone, Copy, PartialEq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

pub struct SvgOut {
    buf: String,
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

pub fn rgb(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Approximate advance width of `text` set in Arial at `size`.
pub fn text_width(text: &str, size: f32) -> f32 {
    let em: f32 = text
        .chars()
        .map(|c| match c {
            'I' | 'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' | ' ' | '/' | '\\' => 0.28,
            'M' | 'W' | 'm' | 'w' | '@' => 0.86,
            'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '-' => 0.33,
            c if c.is_ascii_uppercase() => 0.68,
            _ => 0.556,
        })
        .sum();
    em * size
}

impl SvgOut {
    /// `unit` is the CSS unit of one drawing unit (`mm`, `px`…).
    pub fn new(width: f32, height: f32, unit: &str, scale: f32) -> Self {
        let mut buf = String::new();
        let _ = write!(
            buf,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}{unit}" height="{}{unit}" viewBox="0 0 {width} {height}" stroke-linecap="round" stroke-linejoin="round" fill="none">"#,
            width * scale,
            height * scale
        );
        let _ = writeln!(buf, r##"<rect width="{width}" height="{height}" fill="#ffffff"/>"##);
        Self { buf }
    }

    pub fn finish(mut self) -> String {
        self.buf.push_str("</svg>\n");
        self.buf
    }

    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), width: f32, color: &str) {
        let _ = writeln!(
            self.buf,
            r#"<path d="M{:.3} {:.3}L{:.3} {:.3}" stroke="{color}" stroke-width="{width:.3}"/>"#,
            a.0, a.1, b.0, b.1
        );
    }

    /// An open or closed polyline; `fill` is a colour or `none`.
    pub fn poly(&mut self, pts: &[(f32, f32)], closed: bool, width: f32, color: &str, fill: &str) {
        if pts.len() < 2 {
            return;
        }
        let mut d = String::new();
        for (i, p) in pts.iter().enumerate() {
            let _ = write!(d, "{}{:.3} {:.3}", if i == 0 { 'M' } else { 'L' }, p.0, p.1);
        }
        if closed {
            d.push('Z');
        }
        self.path(&d, width, color, fill);
    }

    pub fn path(&mut self, d: &str, width: f32, color: &str, fill: &str) {
        let _ = writeln!(self.buf, r#"<path d="{d}" stroke="{color}" stroke-width="{width:.3}" fill="{fill}"/>"#);
    }

    pub fn circle(&mut self, c: (f32, f32), r: f32, width: f32, color: &str, fill: &str) {
        let _ = writeln!(
            self.buf,
            r#"<circle cx="{:.3}" cy="{:.3}" r="{r:.3}" stroke="{color}" stroke-width="{width:.3}" fill="{fill}"/>"#,
            c.0, c.1
        );
    }

    pub fn ellipse(&mut self, c: (f32, f32), rx: f32, ry: f32, width: f32, color: &str, fill: &str) {
        let _ = writeln!(
            self.buf,
            r#"<ellipse cx="{:.3}" cy="{:.3}" rx="{rx:.3}" ry="{ry:.3}" stroke="{color}" stroke-width="{width:.3}" fill="{fill}"/>"#,
            c.0, c.1
        );
    }

    /// A circular arc from `a` to `b`. `sweep` is the SVG sweep flag (clockwise on screen).
    pub fn arc(&mut self, a: (f32, f32), b: (f32, f32), r: f32, large: bool, sweep: bool, width: f32, color: &str) {
        let d = format!("M{:.3} {:.3}A{r:.3} {r:.3} 0 {} {} {:.3} {:.3}", a.0, a.1, large as u8, sweep as u8, b.0, b.1);
        self.path(&d, width, color, "none");
    }

    /// Text whose baseline starts, is centred or ends at `at`. With `vertical`
    /// it reads bottom-to-top (rotated 90° counter-clockwise about `at`).
    pub fn text(&mut self, at: (f32, f32), text: &str, size: f32, anchor: Anchor, vertical: bool, color: &str) {
        if text.trim().is_empty() {
            return;
        }
        let anchor = match anchor {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        };
        let rot = if vertical { format!(r#" transform="rotate(-90 {:.3} {:.3})""#, at.0, at.1) } else { String::new() };
        let _ = writeln!(
            self.buf,
            r#"<text x="{:.3}" y="{:.3}" font-family="{FONT}" font-size="{size:.3}" text-anchor="{anchor}" fill="{color}"{rot}>{}</text>"#,
            at.0,
            at.1,
            esc(text)
        );
    }
}
