//! Rasterising a sheet's SVG for the canvas.
//!
//! The drawing stays vector data (`usvg`) on a worker thread. The canvas shows a
//! whole-sheet texture straight away and, once the view has settled at a zoom
//! where that texture would look soft, a second texture rendered for exactly
//! the visible area — so the schematic is sharp at any magnification while
//! panning and zooming never wait for a render.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};

use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions, Vec2, pos2, vec2};
use resvg::{tiny_skia, usvg};

/// Longest side of the whole-sheet texture.
const BASE_SIDE: f32 = 3072.0;
/// Longest side of a detail texture.
const DETAIL_SIDE: f32 = 4096.0;
/// The view must rest this long before a detail render is requested.
const SETTLE: f64 = 0.18;

/// System fonts, loaded once (text in SVGs is laid out by `usvg`).
fn fonts() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        // Generic families used by exported schematics.
        db.set_sans_serif_family("Arial");
        db.set_serif_family("Times New Roman");
        db.set_monospace_family("Consolas");
        Arc::new(db)
    })
    .clone()
}

/// Render `area` (x, y, w, h in SVG units) of the tree into a `px` sized image on white paper.
fn raster(tree: &usvg::Tree, area: [f32; 4], px: [u32; 2]) -> Option<ColorImage> {
    let mut pixmap = tiny_skia::Pixmap::new(px[0].max(1), px[1].max(1))?;
    pixmap.fill(tiny_skia::Color::WHITE);
    let transform = tiny_skia::Transform::from_scale(px[0] as f32 / area[2], px[1] as f32 / area[3]).pre_translate(-area[0], -area[1]);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    Some(ColorImage::from_rgba_premultiplied([pixmap.width() as usize, pixmap.height() as usize], pixmap.data()))
}

struct Request {
    area: [f32; 4],
    px: [u32; 2],
}

enum Reply {
    Loaded { size: [f32; 2] },
    Base(ColorImage),
    Detail([f32; 4], ColorImage),
    Failed(String),
}

fn worker(path: PathBuf, max_side: f32, ctx: egui::Context, requests: Receiver<Request>, replies: Sender<Reply>) {
    let send = |r: Reply| {
        let ok = replies.send(r).is_ok();
        ctx.request_repaint();
        ok
    };
    let tree = (|| -> Result<usvg::Tree, String> {
        let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let opt = usvg::Options { fontdb: fonts(), resources_dir: path.parent().map(|p| p.to_path_buf()), ..Default::default() };
        usvg::Tree::from_data(&data, &opt).map_err(|e| format!("{}: {e}", path.display()))
    })();
    let tree = match tree {
        Ok(t) => t,
        Err(e) => {
            send(Reply::Failed(e));
            return;
        }
    };
    let size = [tree.size().width(), tree.size().height()];
    if !send(Reply::Loaded { size }) {
        return;
    }
    let scale = BASE_SIDE.min(max_side) / size[0].max(size[1]);
    let px = [(size[0] * scale).round() as u32, (size[1] * scale).round() as u32];
    match raster(&tree, [0.0, 0.0, size[0], size[1]], px) {
        Some(img) => {
            if !send(Reply::Base(img)) {
                return;
            }
        }
        None => {
            send(Reply::Failed("the drawing could not be rendered".into()));
            return;
        }
    }
    // Detail renders; only the newest request matters.
    while let Ok(mut req) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            req = newer;
        }
        if let Some(img) = raster(&tree, req.area, req.px) {
            if !send(Reply::Detail(req.area, img)) {
                return;
            }
        }
    }
}

/// A sheet's background: textures plus the worker that renders them.
pub struct SheetImage {
    /// Size in SVG units, once the file has been read.
    pub size: Option<Vec2>,
    pub error: Option<String>,
    base: Option<TextureHandle>,
    /// Sharp render of part of the sheet: area in SVG units, pixels per SVG unit.
    detail: Option<(Rect, f32, TextureHandle)>,
    /// Area and resolution of the render in flight.
    pending: Option<(Rect, f32)>,
    requests: Sender<Request>,
    replies: Receiver<Reply>,
    name: String,
}

impl SheetImage {
    pub fn load(ctx: &egui::Context, path: PathBuf) -> Self {
        let (requests, req_rx) = channel();
        let (rep_tx, replies) = channel();
        let max_side = ctx.input(|i| i.max_texture_side) as f32;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let ctx = ctx.clone();
        std::thread::spawn(move || worker(path, max_side, ctx, req_rx, rep_tx));
        Self { size: None, error: None, base: None, detail: None, pending: None, requests, replies, name }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        loop {
            match self.replies.try_recv() {
                Ok(Reply::Loaded { size }) => self.size = Some(vec2(size[0], size[1])),
                Ok(Reply::Base(img)) => self.base = Some(ctx.load_texture(format!("sch:{}", self.name), img, TextureOptions::LINEAR)),
                Ok(Reply::Detail(area, img)) => {
                    let rect = Rect::from_min_size(pos2(area[0], area[1]), vec2(area[2], area[3]));
                    let density = img.size[0] as f32 / area[2];
                    self.detail = Some((rect, density, ctx.load_texture(format!("sch-detail:{}", self.name), img, TextureOptions::LINEAR)));
                    self.pending = None;
                }
                Ok(Reply::Failed(e)) => self.error = Some(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if self.base.is_none() && self.error.is_none() {
                        self.error = Some("the drawing could not be rendered".into());
                    }
                    break;
                }
            }
        }
    }

    /// Paint the sheet. `origin` is the screen position of the sheet's top-left
    /// corner and `scale` the number of points per SVG unit; `moved_at` is when
    /// the view last changed.
    pub fn paint(&mut self, ui: &egui::Ui, clip: Rect, origin: Pos2, scale: f32, moved_at: f64) {
        self.poll(ui.ctx());
        let Some(size) = self.size else { return };
        let painter = ui.painter_at(clip);
        let sheet = Rect::from_min_size(origin, size * scale);
        // Paper with a soft edge, also what shows while the first render runs.
        painter.rect_filled(sheet.expand(1.0), 0.0, Color32::from_rgb(0x0c, 0x0d, 0x10));
        painter.rect_filled(sheet, 0.0, Color32::WHITE);
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let Some(base) = &self.base else { return };
        painter.image(base.id(), sheet, uv, Color32::WHITE);

        // Screen pixels per SVG unit, against what the base texture offers.
        let ppp = ui.ctx().pixels_per_point();
        let wanted = scale * ppp;
        let base_density = base.size()[0] as f32 / size.x;
        if wanted <= base_density * 1.1 {
            return;
        }
        let to_screen = |r: Rect| Rect::from_min_max(origin + r.min.to_vec2() * scale, origin + r.max.to_vec2() * scale);
        if let Some((area, _, tex)) = &self.detail {
            painter.image(tex.id(), to_screen(*area), uv, Color32::WHITE);
        }

        // Visible part of the sheet, in SVG units.
        let visible = Rect::from_min_max(((clip.min - origin) / scale).to_pos2(), ((clip.max - origin) / scale).to_pos2())
            .intersect(Rect::from_min_size(Pos2::ZERO, size));
        if !visible.is_positive() {
            return;
        }
        let good = |area: &Rect, density: f32| area.contains_rect(visible) && density >= wanted * 0.8 && density <= wanted * 2.5;
        if self.detail.as_ref().is_some_and(|(a, d, _)| good(a, *d)) || self.pending.as_ref().is_some_and(|(a, d)| good(a, *d)) {
            return;
        }
        let now = ui.input(|i| i.time);
        if now - moved_at < SETTLE {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(SETTLE));
            return;
        }
        // A margin around the view keeps small pans sharp; shrink it if the
        // texture would get too large.
        let max_side = DETAIL_SIDE.min(ui.input(|i| i.max_texture_side) as f32);
        let mut area = visible.expand2(visible.size() * 0.25).intersect(Rect::from_min_size(Pos2::ZERO, size));
        if area.width().max(area.height()) * wanted > max_side {
            area = visible;
        }
        let density = wanted.min(max_side / area.width().max(area.height()));
        let px = [(area.width() * density).round().max(1.0) as u32, (area.height() * density).round().max(1.0) as u32];
        if self.requests.send(Request { area: [area.min.x, area.min.y, area.width(), area.height()], px }).is_ok() {
            self.pending = Some((area, density));
        }
    }

    pub fn ready(&self) -> bool {
        self.base.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_an_area_of_a_drawing() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50" viewBox="0 0 100 50">
            <rect x="50" y="0" width="50" height="50" fill="#ff0000"/></svg>"##;
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap();
        // The right half only, at 2 px per unit.
        let img = raster(&tree, [50.0, 0.0, 50.0, 50.0], [100, 100]).unwrap();
        assert_eq!(img.size, [100, 100]);
        assert_eq!(img.pixels[50 * 100 + 50], Color32::from_rgb(255, 0, 0));
        // The whole sheet: left half is white paper.
        let img = raster(&tree, [0.0, 0.0, 100.0, 50.0], [200, 100]).unwrap();
        assert_eq!(img.pixels[50 * 200 + 20], Color32::WHITE);
        assert_eq!(img.pixels[50 * 200 + 180], Color32::from_rgb(255, 0, 0));
    }
}
