//! PDF import. Each page is interpreted twice with `hayro` (pure Rust): once
//! with a device that only records where the glyphs land, to find the text, and
//! once to convert the page to SVG for the background.

use anyhow::{Result, anyhow};
use hayro_svg::hayro_interpret::font::{self, GlyphRun};
use hayro_svg::hayro_interpret::hayro_cmap::BfString;
use hayro_svg::hayro_interpret::kurbo::{Affine, BezPath, Point, Rect};
use hayro_svg::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps, InterpreterCache, InterpreterSettings, SoftMask,
    TransformExt, interpret_page,
};
use hayro_svg::hayro_syntax::Pdf;
use hayro_svg::hayro_syntax::page::Page;
use hayro_svg::{RenderCache, SvgRenderSettings};

use super::{Drawing, PinNames, TextItem};

/// Pages converted when no page mentions a pin of the MCU.
const FALLBACK_PAGES: usize = 4;
/// Outline glyphs are described in 1/1000 em.
const UNITS_PER_EM: f64 = 1000.0;

struct Glyph {
    text: String,
    /// Origin on the page (Y down) and the vector of one em along the baseline.
    origin: Point,
    em: (f64, f64),
    /// A glyph of KiCad's embedded stroke font.
    stroke_font: bool,
}

/// Records glyph positions; draws nothing.
#[derive(Default)]
struct TextDevice {
    glyphs: Vec<Glyph>,
    /// The PDF was plotted by KiCad.
    kicad: bool,
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    // Every text render mode counts, including invisible text (mode 3), which
    // scanned or stroke-plotted drawings carry precisely so text can be found.
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for g in run.glyphs() {
            let text = match g.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                None => match char::from_u32(g.char_code()) {
                    Some(c) if c.is_ascii_graphic() || c == ' ' => c.to_string(),
                    _ => continue,
                },
            };
            let t: Affine = props.transform * g.transform();
            let origin = t * Point::new(0.0, 0.0);
            let x = t * Point::new(UNITS_PER_EM, 0.0);
            // KiCad plots strings with its stroke font embedded as Type 3 fonts.
            let stroke_font = self.kicad && matches!(**g, font::Glyph::Type3(_));
            self.glyphs.push(Glyph { text, origin, em: (x.x - origin.x, x.y - origin.y), stroke_font });
        }
    }
}

/// Join glyphs into strings: consecutive glyphs on one baseline, split at spaces and gaps.
///
/// Ordinary fonts put capitals between the baseline and about 0.72 em above it.
/// KiCad's stroke font is different: its font size is the height of a capital,
/// and the glyph origin lies below the baseline, so the text is centred 0.8 em
/// above the origin; its glyphs also start a quarter em after their origin.
fn group(glyphs: &[Glyph]) -> Vec<TextItem> {
    struct Run {
        text: String,
        start: Point,
        last: Point,
        size: f64,
        dir: (f64, f64),
        /// Centre above the baseline and glyph height, in em.
        rise: f64,
        cap: f64,
        /// Ink of the first glyph after its origin, and of the last glyph, in em.
        lead: f64,
        tail: f64,
    }
    fn flush(run: Option<Run>, out: &mut Vec<TextItem>) {
        let Some(r) = run else { return };
        if r.text.trim().is_empty() {
            return;
        }
        // The last glyph's advance is not known; allow it the width of a typical capital.
        let len = ((r.last.x - r.start.x) * r.dir.0 + (r.last.y - r.start.y) * r.dir.1).max(0.0) + r.size * (r.tail - r.lead);
        let mid = r.size * r.lead + len / 2.0;
        // "Up" for text reading along `dir` on a Y-down page.
        let up = (r.dir.1, -r.dir.0);
        let cx = r.start.x + r.dir.0 * mid + up.0 * r.size * r.rise;
        let cy = r.start.y + r.dir.1 * mid + up.1 * r.size * r.rise;
        out.push(TextItem {
            text: r.text,
            x: cx as f32,
            y: cy as f32,
            len: len as f32,
            height: (r.size * r.cap) as f32,
            dir: (r.dir.0 as f32, r.dir.1 as f32),
            inward: None,
        });
    }

    let mut out = Vec::new();
    let mut run: Option<Run> = None;
    for g in glyphs {
        let size = g.em.0.hypot(g.em.1);
        if size <= 0.0 || !size.is_finite() {
            continue;
        }
        let dir = (g.em.0 / size, g.em.1 / size);
        if g.text.chars().all(char::is_whitespace) {
            flush(run.take(), &mut out);
            continue;
        }
        let joins = run.as_ref().is_some_and(|r| {
            let d = (g.origin.x - r.last.x, g.origin.y - r.last.y);
            let along = d.0 * r.dir.0 + d.1 * r.dir.1;
            let across = (-d.0 * r.dir.1 + d.1 * r.dir.0).abs();
            (dir.0 * r.dir.0 + dir.1 * r.dir.1) > 0.99
                && (size / r.size - 1.0).abs() < 0.2
                && across < r.size * 0.3
                && along > -r.size * 0.1
                && along < r.size * 1.3
        });
        if !joins {
            flush(run.take(), &mut out);
            let (rise, cap, lead, tail) = if g.stroke_font { (0.8, 1.0, 0.25, 0.85) } else { (0.36, 0.72, 0.0, 0.62) };
            run = Some(Run { text: String::new(), start: g.origin, last: g.origin, size, dir, rise, cap, lead, tail });
        }
        if let Some(r) = &mut run {
            r.text.push_str(&g.text);
            r.last = g.origin;
        }
    }
    flush(run, &mut out);
    out
}

fn page_texts<'a>(page: &'a Page<'a>, cache: &InterpreterCache<'a>, kicad: bool) -> Vec<TextItem> {
    let (w, h) = page.render_dimensions();
    let mut ctx = Context::new(
        page.initial_transform(true).to_kurbo(),
        Rect::new(0.0, 0.0, w as f64, h as f64),
        cache,
        page.xref(),
        InterpreterSettings::default(),
    );
    let mut device = TextDevice { glyphs: Vec::new(), kicad };
    interpret_page(page, &mut ctx, &mut device);
    group(&device.glyphs)
}

/// Convert the pages that show pins of the MCU (or the first few, if none does).
pub fn import(stem: &str, data: Vec<u8>, pins: &PinNames) -> Result<Vec<Drawing>> {
    // Written by KiCad's plotter (`/Producer (KiCad PDF)`, `/Creator (Eeschema-PDF)`)?
    let kicad = [&b"(KiCad PDF)"[..], b"(Eeschema"].iter().any(|tag| data.windows(tag.len()).any(|w| w == *tag));
    let pdf = Pdf::new(data).map_err(|e| anyhow!("not a readable PDF ({e:?})"))?;
    let pages = pdf.pages();
    let text_cache = InterpreterCache::new();
    let texts: Vec<Vec<TextItem>> = pages.iter().map(|p| page_texts(p, &text_cache, kicad)).collect();
    let matches: Vec<usize> = texts.iter().map(|t| pins.count_matches(t)).collect();
    let any = matches.iter().any(|&m| m > 0);

    let cache = RenderCache::new();
    let settings = SvgRenderSettings { bg_color: [255, 255, 255, 255] };
    let mut out = Vec::new();
    for (i, (page, texts)) in pages.iter().zip(texts).enumerate() {
        if if any { matches[i] == 0 } else { i >= FALLBACK_PAGES } {
            continue;
        }
        let (width, height) = page.render_dimensions();
        let svg = hayro_svg::convert(page, &cache, &InterpreterSettings::default(), &settings);
        let name = if pages.len() > 1 { format!("{stem} p{}", i + 1) } else { stem.to_string() };
        out.push(Drawing { name, svg, width, height, texts });
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A one-page PDF (300 × 200 pt) with Helvetica text; `strings` are (x, y from the bottom, size, text).
    pub(crate) fn sample_pdf(strings: &[(f32, f32, f32, &str)], vertical: &[(f32, f32, f32, &str)]) -> Vec<u8> {
        let mut content = String::new();
        for (x, y, size, s) in strings {
            content.push_str(&format!("BT /F1 {size} Tf {x} {y} Td ({s}) Tj ET\n"));
        }
        for (x, y, size, s) in vertical {
            // Text matrix rotated 90° counter-clockwise: reads bottom to top.
            content.push_str(&format!("BT /F1 {size} Tf 0 1 -1 0 {x} {y} Tm ({s}) Tj ET\n"));
        }
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for o in offsets {
            pdf.push_str(&format!("{o:010} 00000 n \n"));
        }
        pdf.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
        pdf.into_bytes()
    }

    #[test]
    fn finds_text_positions_on_a_page() {
        let data = sample_pdf(&[(50.0, 150.0, 10.0, "PA0"), (120.0, 150.0, 10.0, "PA1 LED"), (50.0, 100.0, 10.0, "14")], &[(200.0, 40.0, 10.0, "PB7")]);
        let pkg = crate::schematic::tests::package(&["PA0", "PA1", "PB7", "PC0"]);
        let pins = PinNames::from_package(&pkg);
        let pages = import("board", data, &pins).unwrap();
        assert_eq!(pages.len(), 1);
        let d = &pages[0];
        assert_eq!((d.width, d.height), (300.0, 200.0));
        assert!(d.svg.contains("<svg"));

        let find = |s: &str| d.texts.iter().find(|t| t.text == s).unwrap_or_else(|| panic!("{s} not in {:?}", d.texts));
        // Page Y is flipped: 150 pt from the bottom is 50 from the top; the centre is above the baseline.
        let pa0 = find("PA0");
        assert!(pa0.x > 55.0 && pa0.x < 64.0 && (pa0.y - 46.4).abs() < 0.2, "{pa0:?}");
        assert!((pa0.height - 7.2).abs() < 0.01 && pa0.dir == (1.0, 0.0));
        // Words are separate items.
        assert!(find("PA1").x < find("LED").x);
        // Rotated text reads upwards.
        let pb7 = find("PB7");
        assert!(pb7.dir.0.abs() < 1e-3 && pb7.dir.1 < -0.99, "{pb7:?}");
        assert!(pb7.y < 160.0 && pb7.y > 140.0 && (pb7.x - 196.4).abs() < 0.2, "{pb7:?}");

        assert_eq!(pins.count_matches(&d.texts), 3);
    }
}
