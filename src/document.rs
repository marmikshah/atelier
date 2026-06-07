//! The editable document model — atelier's Aseprite-class core.
//!
//! A `Document` is a canvas of ordered **layers** (opacity / visibility / blend)
//! over a timeline of **frames** (each with a duration). A **cel** is one
//! layer×frame image placed at (x,y); cels are sparse. The document also holds a
//! **palette** and animation **tags** (named frame ranges).
//!
//! Persistence: a directory with `doc.json` (structure + cel file refs) and one
//! PNG per cel under `cels/`. Rendering flattens visible layers at a frame with
//! source-over compositing scaled by layer opacity; export covers flattened PNG,
//! a spritesheet (+ JSON meta) and an animated GIF that honours frame durations.

use std::collections::HashMap;
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::raster;

#[derive(Serialize, Deserialize, Clone)]
pub struct LayerMeta {
    pub name: String,
    pub opacity: u8,
    pub visible: bool,
    /// Compositing mode: normal/multiply/screen/add/overlay/soft-light/
    /// hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/
    /// exclusion. Unknown values fall back to normal. See `Blend`.
    pub blend: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FrameMeta {
    pub duration_ms: u32,
    /// Optional anchor point in document pixels (e.g. feet / weapon mount).
    /// Engines read this to position the sprite; None = top-left origin.
    #[serde(default)]
    pub pivot: Option<[i32; 2]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TagMeta {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: String, // "forward" | "reverse" | "pingpong"
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CelMeta {
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub file: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DocMeta {
    pub name: String,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub palette: Vec<[u8; 4]>,
    pub layers: Vec<LayerMeta>,
    pub frames: Vec<FrameMeta>,
    #[serde(default)]
    pub tags: Vec<TagMeta>,
    pub cels: Vec<CelMeta>,
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub meta: DocMeta,
    /// (layer, frame) -> (x, y, image)
    cels: HashMap<(usize, usize), (i32, i32, RgbaImage)>,
}

fn cel_file(layer: usize, frame: usize) -> String {
    format!("cels/L{}_F{}.png", layer, frame)
}

impl Document {
    pub fn new(name: &str, w: u32, h: u32) -> Document {
        let meta = DocMeta {
            name: name.to_string(),
            w,
            h,
            palette: Vec::new(),
            layers: vec![LayerMeta {
                name: "Layer 1".into(),
                opacity: 255,
                visible: true,
                blend: "normal".into(),
            }],
            frames: vec![FrameMeta {
                duration_ms: 100,
                pivot: None,
            }],
            tags: Vec::new(),
            cels: Vec::new(),
        };
        Document {
            meta,
            cels: HashMap::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Document, String> {
        let s = std::fs::read_to_string(dir.join("doc.json")).map_err(|e| e.to_string())?;
        let meta: DocMeta = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        let mut cels = HashMap::new();
        for c in &meta.cels {
            let img = image::open(dir.join(&c.file))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            cels.insert((c.layer, c.frame), (c.x, c.y, img));
        }
        Ok(Document { meta, cels })
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir.join("cels")).map_err(|e| e.to_string())?;
        let mut cel_metas = Vec::new();
        for ((layer, frame), (x, y, img)) in &self.cels {
            let file = cel_file(*layer, *frame);
            img.save(dir.join(&file)).map_err(|e| e.to_string())?;
            cel_metas.push(CelMeta {
                layer: *layer,
                frame: *frame,
                x: *x,
                y: *y,
                file,
            });
        }
        cel_metas.sort_by_key(|c| (c.layer, c.frame));
        self.meta.cels = cel_metas;
        std::fs::write(
            dir.join("doc.json"),
            serde_json::to_string_pretty(&self.meta).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // -- structure ----------------------------------------------------------

    /// Append a new layer on top; returns its index.
    pub fn add_layer(&mut self, name: Option<String>, opacity: u8, blend: String) -> usize {
        let idx = self.meta.layers.len();
        self.meta.layers.push(LayerMeta {
            name: name.unwrap_or_else(|| format!("Layer {}", idx + 1)),
            opacity,
            visible: true,
            blend,
        });
        idx
    }

    /// Patch a layer's visibility / opacity / blend (each optional).
    pub fn set_layer(
        &mut self,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<(), String> {
        let l = self
            .meta
            .layers
            .get_mut(layer)
            .ok_or_else(|| format!("no layer {}", layer))?;
        if let Some(v) = visible {
            l.visible = v;
        }
        if let Some(o) = opacity {
            l.opacity = o;
        }
        if let Some(b) = blend {
            l.blend = b;
        }
        Ok(())
    }

    /// Append a new frame; with `copy_from`, duplicate that frame's cels into it.
    pub fn add_frame(&mut self, duration_ms: u32, copy_from: Option<usize>) -> usize {
        let idx = self.meta.frames.len();
        self.meta.frames.push(FrameMeta {
            duration_ms,
            pivot: None,
        });
        if let Some(src) = copy_from {
            // duplicate every cel of frame `src` into the new frame
            let to_copy: Vec<(usize, (i32, i32, RgbaImage))> = self
                .cels
                .iter()
                .filter(|((_, f), _)| *f == src)
                .map(|((l, _), v)| (*l, (v.0, v.1, v.2.clone())))
                .collect();
            for (l, v) in to_copy {
                self.cels.insert((l, idx), v);
            }
        }
        idx
    }

    /// Set a frame's display duration in milliseconds.
    pub fn set_frame_duration(&mut self, frame: usize, ms: u32) -> Result<(), String> {
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.duration_ms = ms;
        Ok(())
    }

    /// Set (or clear, with None) a frame's anchor/pivot point.
    pub fn set_pivot(&mut self, frame: usize, pivot: Option<[i32; 2]>) -> Result<(), String> {
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.pivot = pivot;
        Ok(())
    }

    /// Add a named animation tag over an inclusive frame range.
    pub fn add_tag(
        &mut self,
        name: &str,
        from: usize,
        to: usize,
        direction: &str,
    ) -> Result<(), String> {
        if from > to || to >= self.meta.frames.len() {
            return Err(format!(
                "tag range {}..{} out of bounds (frames={})",
                from,
                to,
                self.meta.frames.len()
            ));
        }
        self.meta.tags.push(TagMeta {
            name: name.into(),
            from,
            to,
            direction: direction.into(),
        });
        Ok(())
    }

    fn check_cel(&self, layer: usize, frame: usize) -> Result<(), String> {
        if layer >= self.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if frame >= self.meta.frames.len() {
            return Err(format!("no frame {}", frame));
        }
        Ok(())
    }

    /// Place (or replace) the cel image for (layer, frame) at offset (x, y).
    pub fn set_cel(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        img: RgbaImage,
    ) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.cels.insert((layer, frame), (x, y, img));
        Ok(())
    }

    /// Remove the cel at (layer, frame), if any.
    pub fn clear_cel(&mut self, layer: usize, frame: usize) {
        self.cels.remove(&(layer, frame));
    }

    // -- palette (indexed-friendly swatch list) -----------------------------

    /// Replace the document's palette swatch list.
    pub fn set_palette(&mut self, colors: Vec<[u8; 4]>) {
        self.meta.palette = colors;
    }

    /// JSON snapshot of the document structure (layers, frames, tags, cels,
    /// palette) for inspection — no pixel data.
    pub fn structure(&self) -> Value {
        let mut cels: Vec<Value> = self
            .cels
            .keys()
            .map(|(l, f)| json!({"layer": l, "frame": f}))
            .collect();
        cels.sort_by_key(|x| {
            (
                x["layer"].as_u64().unwrap_or(0),
                x["frame"].as_u64().unwrap_or(0),
            )
        });
        json!({
            "name": self.meta.name, "w": self.meta.w, "h": self.meta.h,
            "layers": self.meta.layers.iter().enumerate().map(|(i, l)| json!({
                "index": i, "name": l.name, "opacity": l.opacity, "visible": l.visible, "blend": l.blend
            })).collect::<Vec<_>>(),
            "frames": self.meta.frames.iter().enumerate().map(|(i, f)| json!({
                "index": i, "duration_ms": f.duration_ms, "pivot": f.pivot
            })).collect::<Vec<_>>(),
            "tags": self.meta.tags.iter().map(|t| json!({
                "name": t.name, "from": t.from, "to": t.to, "direction": t.direction
            })).collect::<Vec<_>>(),
            "cels": cels,
            "palette": self.meta.palette,
            "palette_len": self.meta.palette.len(),
        })
    }

    /// Read one pixel from a cel (document coords). Returns RGBA; out-of-bounds
    /// or an empty cel reads as transparent [0,0,0,0]. Read-only — never
    /// materialises a blank cel (unlike `cel_canvas`).
    pub fn get_pixel(&self, layer: usize, frame: usize, x: i32, y: i32) -> Result<[u8; 4], String> {
        self.check_cel(layer, frame)?;
        let transparent = [0, 0, 0, 0];
        let Some((cx, cy, img)) = self.cels.get(&(layer, frame)) else {
            return Ok(transparent);
        };
        let (lx, ly) = (x - cx, y - cy);
        if lx < 0 || ly < 0 || lx as u32 >= img.width() || ly as u32 >= img.height() {
            return Ok(transparent);
        }
        Ok(img.get_pixel(lx as u32, ly as u32).0)
    }

    /// Copy a rectangular region of a cel as a flat RGBA buffer (w*h*4),
    /// returned with its width/height. Out-of-cel pixels come back transparent.
    /// The rect is given as inclusive corners and normalised/clamped to canvas.
    pub fn copy_region(
        &self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        self.check_cel(layer, frame)?;
        let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
            .ok_or("region is empty after clamping to the canvas")?;
        let (rw, rh) = ((bx - ax + 1) as u32, (by - ay + 1) as u32);
        let mut buf = vec![0u8; (rw * rh * 4) as usize];
        for ry in 0..rh as i32 {
            for rx in 0..rw as i32 {
                let p = self.get_pixel(layer, frame, ax + rx, ay + ry)?;
                let i = ((ry as u32 * rw + rx as u32) * 4) as usize;
                buf[i..i + 4].copy_from_slice(&p);
            }
        }
        Ok((rw, rh, buf))
    }

    /// Paste a flat RGBA buffer onto a cel at (x,y). `blend` true = source-over
    /// (transparent source pixels keep the destination); false = overwrite
    /// (copy every pixel including transparency, so it also erases).
    pub fn paste_region(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        rw: u32,
        rh: u32,
        buf: &[u8],
        blend: bool,
    ) -> Result<(), String> {
        if buf.len() != (rw * rh * 4) as usize {
            return Err(format!("buffer length {} != {}x{}x4", buf.len(), rw, rh));
        }
        let img = self.cel_canvas(layer, frame)?;
        for ry in 0..rh as i32 {
            for rx in 0..rw as i32 {
                let i = ((ry as u32 * rw + rx as u32) * 4) as usize;
                let p = [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]];
                if blend && p[3] == 0 {
                    continue;
                }
                raster::put(img, x + rx, y + ry, p);
            }
        }
        Ok(())
    }

    /// Erase a rectangular region of a cel (set to transparent).
    pub fn clear_region(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (ax, bx) = (x0.min(x1), x0.max(x1));
        let (ay, by) = (y0.min(y1), y0.max(y1));
        for y in ay..=by {
            for x in ax..=bx {
                raster::put(img, x, y, [0, 0, 0, 0]);
            }
        }
        Ok(())
    }

    /// Move a region within one cel by (dx,dy): copy it, clear the source, paste
    /// at the offset. Overwrite-paste so the moved block is exact.
    pub fn move_region(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dx: i32,
        dy: i32,
    ) -> Result<(), String> {
        let (rw, rh, buf) = self.copy_region(layer, frame, x0, y0, x1, y1)?;
        let (ax, ay) = (x0.min(x1).max(0), y0.min(y1).max(0));
        self.clear_region(layer, frame, x0, y0, x1, y1)?;
        self.paste_region(layer, frame, ax + dx, ay + dy, rw, rh, &buf, false)
    }

    // -- per-pixel drawing --------------------------------------------------

    /// Get the cel as a full-canvas image anchored at (0,0), creating/normalising
    /// it if needed so drawing coordinates equal document pixel coordinates.
    fn cel_canvas(&mut self, layer: usize, frame: usize) -> Result<&mut RgbaImage, String> {
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let key = (layer, frame);
        let needs = match self.cels.get(&key) {
            Some((x, y, img)) => *x != 0 || *y != 0 || img.width() != w || img.height() != h,
            None => true,
        };
        if needs {
            let mut full = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
            if let Some((x, y, img)) = self.cels.get(&key) {
                for yy in 0..img.height() {
                    for xx in 0..img.width() {
                        let p = img.get_pixel(xx, yy).0;
                        if p[3] > 0 {
                            let (tx, ty) = (*x + xx as i32, *y + yy as i32);
                            if tx >= 0 && ty >= 0 && (tx as u32) < w && (ty as u32) < h {
                                full.put_pixel(tx as u32, ty as u32, Rgba(p));
                            }
                        }
                    }
                }
            }
            self.cels.insert(key, (0, 0, full));
        }
        Ok(&mut self.cels.get_mut(&key).unwrap().2)
    }

    pub fn pencil(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        for (x, y) in points {
            raster::brush(img, *x, *y, color, size.max(1));
        }
        Ok(())
    }

    pub fn line(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        raster::draw_line(img, x0, y0, x1, y1, color, size.max(1));
        Ok(())
    }

    pub fn rect(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        fill: bool,
        size: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (ax, bx) = (x0.min(x1), x0.max(x1));
        let (ay, by) = (y0.min(y1), y0.max(y1));
        if fill {
            for y in ay..=by {
                for x in ax..=bx {
                    raster::put(img, x, y, color);
                }
            }
        } else {
            raster::draw_line(img, ax, ay, bx, ay, color, size.max(1));
            raster::draw_line(img, ax, by, bx, by, color, size.max(1));
            raster::draw_line(img, ax, ay, ax, by, color, size.max(1));
            raster::draw_line(img, bx, ay, bx, by, color, size.max(1));
        }
        Ok(())
    }

    /// Draw an ellipse (rx==ry ⇒ circle). Filled or 1px outline. The radii are
    /// inflated by half a pixel in the boundary test so the four cardinal tips
    /// come out rounded instead of single-pixel nubs; the outline is the
    /// morphological inner edge of that same fill, so it is always a clean,
    /// closed, gap-free 1px ring that matches the filled shape exactly.
    pub fn ellipse(
        &mut self,
        layer: usize,
        frame: usize,
        cx: i32,
        cy: i32,
        rx: i32,
        ry: i32,
        color: [u8; 4],
        fill: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (rx, ry) = (rx.max(1), ry.max(1));
        let (a, b) = (rx as f32 + 0.5, ry as f32 + 0.5);
        let inside = |x: i32, y: i32| (x as f32 / a).powi(2) + (y as f32 / b).powi(2) <= 1.0;
        for y in -ry..=ry {
            for x in -rx..=rx {
                if !inside(x, y) {
                    continue;
                }
                let draw = fill
                    || !(inside(x - 1, y)
                        && inside(x + 1, y)
                        && inside(x, y - 1)
                        && inside(x, y + 1));
                if draw {
                    raster::put(img, cx + x, cy + y, color);
                }
            }
        }
        Ok(())
    }

    /// Connected line segments through `points` (open path). `closed` also joins
    /// the last point back to the first (polygon outline). Square brush `size`.
    pub fn polyline(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        size: i32,
        closed: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let s = size.max(1);
        if points.len() == 1 {
            raster::brush(img, points[0].0, points[0].1, color, s);
        }
        for w in points.windows(2) {
            raster::draw_line(img, w[0].0, w[0].1, w[1].0, w[1].1, color, s);
        }
        if closed && points.len() >= 3 {
            let (a, b) = (*points.last().unwrap(), points[0]);
            raster::draw_line(img, a.0, a.1, b.0, b.1, color, s);
        }
        Ok(())
    }

    /// Polygon through `points`. `fill` scanline-fills the interior (even-odd)
    /// and strokes the edge so steep sides have no 1px gaps; otherwise draws the
    /// closed outline only. Clean organic curves — canopies, ponds, bodies.
    pub fn polygon(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        fill: bool,
    ) -> Result<(), String> {
        if points.len() < 3 || !fill {
            return self.polyline(layer, frame, points, color, 1, true);
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        let ymin = points.iter().map(|p| p.1).min().unwrap().max(0);
        let ymax = points.iter().map(|p| p.1).max().unwrap().min(h - 1);
        let n = points.len();
        for y in ymin..=ymax {
            let yf = y as f32 + 0.5;
            // X where each edge crosses this scanline's centre.
            let mut xs: Vec<f32> = Vec::new();
            for i in 0..n {
                let (x1, y1) = points[i];
                let (x2, y2) = points[(i + 1) % n];
                let (y1f, y2f) = (y1 as f32, y2 as f32);
                if (y1f <= yf && y2f > yf) || (y2f <= yf && y1f > yf) {
                    let t = (yf - y1f) / (y2f - y1f);
                    xs.push(x1 as f32 + t * (x2 as f32 - x1 as f32));
                }
            }
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mut i = 0;
            while i + 1 < xs.len() {
                let xa = (xs[i].ceil() as i32).max(0);
                let xb = (xs[i + 1].floor() as i32).min(w - 1);
                for x in xa..=xb {
                    raster::put(img, x, y, color);
                }
                i += 2;
            }
        }
        self.polyline(layer, frame, points, color, 1, true)
    }

    /// Run `f` (a painting op) confined to a selection `mask` (one bool per
    /// document pixel, row-major). Snapshots the cel, runs the op over the whole
    /// cel, then restores every pixel the mask does not cover — so any op honours
    /// an arbitrary selection without each op knowing about masks.
    pub fn apply_masked<F>(
        &mut self,
        layer: usize,
        frame: usize,
        mask: &[bool],
        f: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut Self) -> Result<(), String>,
    {
        self.check_cel(layer, frame)?;
        let before = self.cel_canvas(layer, frame)?.clone();
        f(self)?;
        let img = self.cel_canvas(layer, frame)?;
        let w = img.width();
        for y in 0..img.height() {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if mask.get(i).copied() != Some(true) {
                    img.put_pixel(x, y, *before.get_pixel(x, y));
                }
            }
        }
        Ok(())
    }

    pub fn bucket_fill(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        color: [u8; 4],
        tol: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        if x < 0 || y < 0 || x >= w || y >= h {
            return Ok(());
        }
        let target = img.get_pixel(x as u32, y as u32).0;
        if raster::close(target, color, 0) {
            return Ok(());
        }
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let p = img.get_pixel(px as u32, py as u32).0;
            if !raster::close(p, target, tol) {
                continue;
            }
            img.put_pixel(px as u32, py as u32, Rgba(color));
            stack.push((px + 1, py));
            stack.push((px - 1, py));
            stack.push((px, py + 1));
            stack.push((px, py - 1));
        }
        Ok(())
    }

    pub fn replace_color(
        &mut self,
        layer: usize,
        frame: usize,
        from: [u8; 4],
        to: [u8; 4],
        tol: i32,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        for p in img.pixels_mut() {
            if raster::close(p.0, from, tol) {
                *p = Rgba(to);
            }
        }
        Ok(())
    }

    pub fn flip(&mut self, layer: usize, frame: usize, horizontal: bool) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let flipped = if horizontal {
            image::imageops::flip_horizontal(img)
        } else {
            image::imageops::flip_vertical(img)
        };
        *img = flipped;
        Ok(())
    }

    /// Shift a cel's contents by (dx,dy). `wrap` true rolls pixels around the
    /// edges (toroidal — for making/checking seamless tiles); false leaves the
    /// exposed edges transparent.
    pub fn shift(
        &mut self,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        wrap: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut out = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
        for y in 0..h {
            for x in 0..w {
                let (tx, ty) = if wrap {
                    ((x + dx).rem_euclid(w), (y + dy).rem_euclid(h))
                } else {
                    (x + dx, y + dy)
                };
                if tx >= 0 && ty >= 0 && tx < w && ty < h {
                    out.put_pixel(tx as u32, ty as u32, *img.get_pixel(x as u32, y as u32));
                }
            }
        }
        *img = out;
        Ok(())
    }

    pub fn fill_cel(&mut self, layer: usize, frame: usize, color: [u8; 4]) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        let img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba(color));
        self.cels.insert((layer, frame), (0, 0, img));
        Ok(())
    }

    /// Draw a Bézier curve through control `points`: 2 = line, 3 = quadratic,
    /// 4+ = cubic (first four). Sampled into `steps` segments with brush `size`.
    /// Smooth organic strokes — tails, vines, hair.
    pub fn bezier(
        &mut self,
        layer: usize,
        frame: usize,
        points: &[(i32, i32)],
        color: [u8; 4],
        size: i32,
        steps: i32,
    ) -> Result<(), String> {
        let s = size.max(1);
        let steps = steps.max(2);
        let img = self.cel_canvas(layer, frame)?;
        if points.len() < 2 {
            if let Some(p) = points.first() {
                raster::brush(img, p.0, p.1, color, s);
            }
            return Ok(());
        }
        let p: Vec<(f32, f32)> = points.iter().map(|&(x, y)| (x as f32, y as f32)).collect();
        let at = |t: f32| -> (f32, f32) {
            let mt = 1.0 - t;
            match p.len() {
                2 => (
                    raster::lerpf(p[0].0, p[1].0, t),
                    raster::lerpf(p[0].1, p[1].1, t),
                ),
                3 => (
                    mt * mt * p[0].0 + 2.0 * mt * t * p[1].0 + t * t * p[2].0,
                    mt * mt * p[0].1 + 2.0 * mt * t * p[1].1 + t * t * p[2].1,
                ),
                _ => (
                    mt * mt * mt * p[0].0
                        + 3.0 * mt * mt * t * p[1].0
                        + 3.0 * mt * t * t * p[2].0
                        + t * t * t * p[3].0,
                    mt * mt * mt * p[0].1
                        + 3.0 * mt * mt * t * p[1].1
                        + 3.0 * mt * t * t * p[2].1
                        + t * t * t * p[3].1,
                ),
            }
        };
        let mut prev = at(0.0);
        for i in 1..=steps {
            let cur = at(i as f32 / steps as f32);
            raster::draw_line(
                img,
                prev.0.round() as i32,
                prev.1.round() as i32,
                cur.0.round() as i32,
                cur.1.round() as i32,
                color,
                s,
            );
            prev = cur;
        }
        Ok(())
    }

    /// Full-canvas (0,0-anchored) copy of a cel, transparent where absent.
    #[allow(dead_code)] // used by tween (later step)
    fn cel_full(&self, layer: usize, frame: usize) -> RgbaImage {
        let mut img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        if let Some((cx, cy, src)) = self.cels.get(&(layer, frame)) {
            for y in 0..src.height() as i32 {
                for x in 0..src.width() as i32 {
                    let (tx, ty) = (cx + x, cy + y);
                    if tx >= 0 && ty >= 0 && (tx as u32) < self.meta.w && (ty as u32) < self.meta.h
                    {
                        img.put_pixel(tx as u32, ty as u32, *src.get_pixel(x as u32, y as u32));
                    }
                }
            }
        }
        img
    }

    // -- render / export ----------------------------------------------------

    /// Full-canvas RGBA image of one layer's cel at `frame` (anchored at 0,0,
    /// transparent where the cel is empty/outside). Read-only sibling of
    /// `flatten` for analysis tools that want a single layer instead of the
    /// composite. Out-of-range layer/frame → error.
    fn cel_image(&self, layer: usize, frame: usize) -> Result<RgbaImage, String> {
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        if let Some((cx, cy, img)) = self.cels.get(&(layer, frame)) {
            for yy in 0..img.height() {
                for xx in 0..img.width() {
                    let (tx, ty) = (*cx + xx as i32, *cy + yy as i32);
                    if tx >= 0 && ty >= 0 && (tx as u32) < w && (ty as u32) < h {
                        out.put_pixel(tx as u32, ty as u32, *img.get_pixel(xx, yy));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Analysis image for a frame: the flattened composite, or one layer's cel
    /// when `layer` is given. The single entry point read-only tools share.
    pub fn analysis_image(&self, layer: Option<usize>, frame: usize) -> Result<RgbaImage, String> {
        if frame >= self.meta.frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                self.meta.frames.len()
            ));
        }
        match layer {
            Some(l) => self.cel_image(l, frame),
            None => Ok(self.flatten(frame)),
        }
    }

    pub fn flatten(&self, frame: usize) -> RgbaImage {
        let mut out = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        for (li, layer) in self.meta.layers.iter().enumerate() {
            if !layer.visible || layer.opacity == 0 {
                continue;
            }
            if let Some((cx, cy, img)) = self.cels.get(&(li, frame)) {
                raster::composite(
                    &mut out,
                    img,
                    *cx,
                    *cy,
                    layer.opacity,
                    raster::parse_blend(&layer.blend),
                );
            }
        }
        out
    }

    /// Flatten `frame` with onion-skin ghosts of the neighbours behind it: the
    /// previous frame tinted blue and the next tinted red, both faded, so motion
    /// is visible at a glance.
    fn flatten_onion(&self, frame: usize) -> RgbaImage {
        let n = self.meta.frames.len();
        let mut out = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba([0, 0, 0, 0]));
        let ghost = |src: RgbaImage, tint: [u8; 3]| -> RgbaImage {
            let mut g = src;
            for p in g.pixels_mut() {
                if p.0[3] == 0 {
                    continue;
                }
                let mix = |i: usize| ((p.0[i] as u32 + tint[i] as u32 * 2) / 3) as u8;
                p.0 = [mix(0), mix(1), mix(2), (p.0[3] as u32 * 90 / 255) as u8];
            }
            g
        };
        if frame > 0 {
            let g = ghost(self.flatten(frame - 1), [70, 110, 255]);
            raster::composite(&mut out, &g, 0, 0, 255, raster::Blend::Normal);
        }
        if frame + 1 < n {
            let g = ghost(self.flatten(frame + 1), [255, 90, 90]);
            raster::composite(&mut out, &g, 0, 0, 255, raster::Blend::Normal);
        }
        let cur = self.flatten(frame);
        raster::composite(&mut out, &cur, 0, 0, 255, raster::Blend::Normal);
        out
    }

    /// Render a frame to an image with preview options: `onion` ghosts the
    /// neighbours; `region` crops (document pixels); `tile` repeats the result
    /// in an N×N grid (seam check); `scale` nearest-upscales; `max_size`
    /// down-scales the longest side for a cheap thumbnail.
    pub fn render_preview(
        &self,
        frame: usize,
        scale: u32,
        region: Option<(i32, i32, i32, i32)>,
        onion: bool,
        tile: u32,
        max_size: Option<u32>,
    ) -> Result<RgbaImage, String> {
        let mut base = if onion {
            self.flatten_onion(frame)
        } else {
            self.flatten(frame)
        };
        if let Some((x0, y0, x1, y1)) = region {
            let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
                .ok_or("render region is empty after clamping")?;
            base = image::imageops::crop_imm(
                &base,
                ax as u32,
                ay as u32,
                (bx - ax + 1) as u32,
                (by - ay + 1) as u32,
            )
            .to_image();
        }
        let tile = tile.max(1);
        if tile > 1 {
            let (tw, th) = (base.width(), base.height());
            let mut t = RgbaImage::from_pixel(tw * tile, th * tile, Rgba([0, 0, 0, 0]));
            for ty in 0..tile {
                for tx in 0..tile {
                    image::imageops::replace(&mut t, &base, (tx * tw) as i64, (ty * th) as i64);
                }
            }
            base = t;
        }
        let sc = scale.max(1);
        if sc > 1 {
            base = image::imageops::resize(
                &base,
                base.width() * sc,
                base.height() * sc,
                image::imageops::FilterType::Nearest,
            );
        }
        if let Some(ms) = max_size {
            let long = base.width().max(base.height());
            if long > ms && long > 0 {
                let f = ms as f32 / long as f32;
                let nw = (base.width() as f32 * f).round().max(1.0) as u32;
                let nh = (base.height() as f32 * f).round().max(1.0) as u32;
                base = image::imageops::resize(&base, nw, nh, image::imageops::FilterType::Nearest);
            }
        }
        Ok(base)
    }

    /// Draw a 1px outline around the opaque pixels of a cel. `aa` also softens
    /// the diagonal-only corner pixels (reduced alpha) so the outline reads as
    /// anti-aliased instead of stair-stepped.
    pub fn outline_cel(
        &mut self,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        aa: bool,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let opaque = |img: &RgbaImage, x: i32, y: i32| {
            x >= 0 && y >= 0 && x < w && y < h && img.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let n4 = [(-1, 0), (1, 0), (0, -1), (0, 1)];
        let nd = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
        let mut writes: Vec<(i32, i32, [u8; 4])> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if opaque(img, x, y) {
                    continue;
                }
                if n4.iter().any(|(ox, oy)| opaque(img, x + ox, y + oy)) {
                    writes.push((x, y, color));
                } else if aa && nd.iter().any(|(ox, oy)| opaque(img, x + ox, y + oy)) {
                    let a = (color[3] as u32 * 110 / 255) as u8; // soft corner
                    writes.push((x, y, [color[0], color[1], color[2], a]));
                }
            }
        }
        for (x, y, c) in writes {
            img.put_pixel(x as u32, y as u32, Rgba(c));
        }
        Ok(())
    }

    /// Project a coloured drop shadow of the cel's opaque silhouette offset by
    /// (dx,dy), at `opacity`, optionally `blur`red, and composite the original
    /// art back on top. Self-contained on one cel.
    pub fn drop_shadow(
        &mut self,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        color: [u8; 4],
        opacity: u8,
        blur: i32,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width(), orig.height());
        let mut shadow = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        let scale = (opacity as f32 / 255.0) * (color[3] as f32 / 255.0);
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let a = orig.get_pixel(x as u32, y as u32).0[3];
                let (sx, sy) = (x + dx, y + dy);
                if a > 0 && sx >= 0 && sy >= 0 && (sx as u32) < w && (sy as u32) < h {
                    let alpha = (a as f32 * scale).round().clamp(0.0, 255.0) as u8;
                    shadow.put_pixel(
                        sx as u32,
                        sy as u32,
                        Rgba([color[0], color[1], color[2], alpha]),
                    );
                }
            }
        }
        let mut out = raster::box_blur(&shadow, blur);
        raster::composite(&mut out, &orig, 0, 0, 255, raster::Blend::Normal);
        *self.cel_canvas(layer, frame)? = out;
        Ok(())
    }

    /// Bloom: blur a bright copy of the cel and composite it back through a
    /// light blend (`mode`, e.g. screen/add) at `intensity`. `color` (None =
    /// the art's own colours) tints the glow. Self-contained on one cel.
    pub fn glow(
        &mut self,
        layer: usize,
        frame: usize,
        color: Option<[u8; 4]>,
        radius: i32,
        intensity: u8,
        mode: &str,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width(), orig.height());
        let mut src = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for y in 0..h {
            for x in 0..w {
                let p = orig.get_pixel(x, y).0;
                if p[3] > 0 {
                    let c = color.unwrap_or(p);
                    src.put_pixel(x, y, Rgba([c[0], c[1], c[2], p[3]]));
                }
            }
        }
        let g = raster::box_blur(&src, radius.max(1));
        let mut out = orig.clone();
        raster::composite(&mut out, &g, 0, 0, intensity, raster::parse_blend(mode));
        *self.cel_canvas(layer, frame)? = out;
        Ok(())
    }

    /// Fake-3D bevel: lighten the top/left edge band and darken the bottom/right
    /// band of the opaque shape (each within `depth` pixels of a silhouette
    /// edge), giving raised volume. `light`/`dark` carry their own alpha as the
    /// effect strength.
    pub fn bevel(
        &mut self,
        layer: usize,
        frame: usize,
        light: [u8; 4],
        dark: [u8; 4],
        depth: i32,
    ) -> Result<(), String> {
        let orig = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (orig.width() as i32, orig.height() as i32);
        let depth = depth.max(1);
        let opaque = |x: i32, y: i32| {
            x >= 0 && y >= 0 && x < w && y < h && orig.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in 0..h {
            for x in 0..w {
                if !opaque(x, y) {
                    continue;
                }
                let mut lit = false;
                let mut shd = false;
                for d in 1..=depth {
                    if !opaque(x, y - d) || !opaque(x - d, y) {
                        lit = true;
                    }
                    if !opaque(x, y + d) || !opaque(x + d, y) {
                        shd = true;
                    }
                }
                let base = orig.get_pixel(x as u32, y as u32).0;
                let np = match (lit, shd) {
                    (true, false) => raster::composite_px(
                        base,
                        light,
                        light[3] as f32 / 255.0,
                        raster::Blend::Normal,
                    ),
                    (false, true) => raster::composite_px(
                        base,
                        dark,
                        dark[3] as f32 / 255.0,
                        raster::Blend::Normal,
                    ),
                    _ => base,
                };
                img.put_pixel(x as u32, y as u32, Rgba(np));
            }
        }
        Ok(())
    }

    /// Box-blur a cel by `radius` (premultiplied, so no dark haloes), optionally
    /// limited to `region`. Soft shadows, depth-of-field, smoke.
    pub fn blur(
        &mut self,
        layer: usize,
        frame: usize,
        radius: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        let blurred = raster::box_blur(img, radius);
        match region {
            None => *img = blurred,
            Some((x0, y0, x1, y1)) => {
                let (w, h) = (img.width() as i32, img.height() as i32);
                let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
                let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
                for y in ay..=by {
                    for x in ax..=bx {
                        img.put_pixel(x as u32, y as u32, *blurred.get_pixel(x as u32, y as u32));
                    }
                }
            }
        }
        Ok(())
    }

    /// Paint a colour gradient over a cel. `kind` "linear" runs the axis
    /// (x0,y0)->(x1,y1); "radial" centres at (x0,y0) with (x1,y1) on the rim.
    /// `stops` are (pos 0..1, RGBA); `dither` "bayer"/"noise" gives a band-free
    /// pixel-art look (else smooth lerp). `region` (inclusive corners) clips the
    /// paint; `blend` true composites over existing pixels (so stop alpha is a
    /// real falloff — vignettes, light), false overwrites.
    pub fn gradient(
        &mut self,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        mut stops: Vec<(f32, [u8; 4])>,
        dither: &str,
        seed: u64,
        region: Option<(i32, i32, i32, i32)>,
        blend: bool,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("gradient needs at least one stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (rx0, ry0, rx1, ry1) = match region {
            Some((a, b, c, d)) => raster::clamp_region(a, b, c, d, self.meta.w, self.meta.h)
                .ok_or("gradient region is empty after clamping")?,
            None => (0, 0, self.meta.w as i32 - 1, self.meta.h as i32 - 1),
        };
        let radial = kind == "radial";
        let (ax, ay) = (x0 as f32, y0 as f32);
        let (dx, dy) = (x1 as f32 - ax, y1 as f32 - ay);
        let len2 = dx * dx + dy * dy;
        let radius = len2.sqrt();
        let img = self.cel_canvas(layer, frame)?;
        for py in ry0..=ry1 {
            for px in rx0..=rx1 {
                let (fx, fy) = (px as f32, py as f32);
                let t = if radial {
                    if radius <= 0.0 {
                        0.0
                    } else {
                        (((fx - ax).powi(2) + (fy - ay).powi(2)).sqrt() / radius).clamp(0.0, 1.0)
                    }
                } else if len2 <= 0.0 {
                    0.0
                } else {
                    (((fx - ax) * dx + (fy - ay) * dy) / len2).clamp(0.0, 1.0)
                };
                let c = raster::sample_gradient(&stops, t, dither, px, py, seed);
                let (ux, uy) = (px as u32, py as u32);
                let out = if blend {
                    raster::over(img.get_pixel(ux, uy).0, c)
                } else {
                    c
                };
                img.put_pixel(ux, uy, Rgba(out));
            }
        }
        Ok(())
    }

    /// Scatter pixels of random `colors` across a region at `density` (0..1
    /// probability per pixel), deterministic for a given `seed`. `size` is a
    /// square dot. Organic grass/foliage/dust/stars without hand-listing each
    /// speckle. Source-over so alpha colours layer onto existing art.
    pub fn scatter(
        &mut self,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        colors: &[[u8; 4]],
        density: f32,
        seed: u64,
        size: i32,
    ) -> Result<(), String> {
        if colors.is_empty() {
            return Err("scatter needs at least one colour".into());
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
        let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
        let d = density.clamp(0.0, 1.0);
        let s = size.max(1);
        let o = s / 2;
        let img = self.cel_canvas(layer, frame)?;
        for py in ay..=by {
            for px in ax..=bx {
                if (raster::hash2(px, py, seed) as f32 / u32::MAX as f32) >= d {
                    continue;
                }
                let c = colors[raster::hash2(px, py, seed ^ 0xA5A5_5A5A) as usize % colors.len()];
                for ddy in 0..s {
                    for ddx in 0..s {
                        let (tx, ty) = (px - o + ddx, py - o + ddy);
                        if tx >= 0 && ty >= 0 && tx < w && ty < h {
                            let (ux, uy) = (tx as u32, ty as u32);
                            img.put_pixel(ux, uy, Rgba(raster::over(img.get_pixel(ux, uy).0, c)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Composite an image ONTO a cel at (x,y) — draws over existing content
    /// (does not replace the cel), honouring `opacity` and blend `mode`. The
    /// caller scales/rotates first (see `Studio::doc_stamp_image`). Enables
    /// sub-sprite reuse without a layer per element.
    fn stamp(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        src: &RgbaImage,
        opacity: u8,
        mode: &str,
    ) -> Result<(), String> {
        let img = self.cel_canvas(layer, frame)?;
        raster::composite(img, src, x, y, opacity, raster::parse_blend(mode));
        Ok(())
    }

    /// Place an external image into a cel with optional nearest-neighbour `scale`
    /// and `rotate` (degrees). `replace` true overwrites the cel (legacy
    /// behaviour); false composites it OVER existing content with `opacity` +
    /// blend `mode`.
    pub fn stamp_image(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        mut src: RgbaImage,
        scale: f32,
        rotate: f32,
        opacity: u8,
        mode: &str,
        replace: bool,
    ) -> Result<(), String> {
        if (scale - 1.0).abs() > 1e-6 && scale > 0.0 {
            let nw = (src.width() as f32 * scale).round().max(1.0) as u32;
            let nh = (src.height() as f32 * scale).round().max(1.0) as u32;
            src = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Nearest);
        }
        if rotate.abs() > 1e-6 {
            src = raster::rotate_nn(&src, rotate);
        }
        if replace {
            self.set_cel(layer, frame, x, y, src)
        } else {
            self.stamp(layer, frame, x, y, &src, opacity, mode)
        }
    }

    /// Mirror a cel across a vertical axis (column `vertical`) and/or a
    /// horizontal axis (row `horizontal`). `keep_left`/`keep_top` choose which
    /// side is the source that gets reflected onto the other. Draw half a sprite,
    /// mirror it for instant symmetry.
    pub fn symmetry(
        &mut self,
        layer: usize,
        frame: usize,
        vertical: Option<i32>,
        horizontal: Option<i32>,
        keep_left: bool,
        keep_top: bool,
    ) -> Result<(), String> {
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        if let Some(ax) = vertical {
            for y in 0..h {
                for x in 0..w {
                    let on_src = if keep_left { x < ax } else { x > ax };
                    if on_src {
                        let mx = 2 * ax - x;
                        if mx >= 0 && mx < w {
                            let p = *img.get_pixel(x as u32, y as u32);
                            img.put_pixel(mx as u32, y as u32, p);
                        }
                    }
                }
            }
        }
        if let Some(ay) = horizontal {
            for y in 0..h {
                for x in 0..w {
                    let on_src = if keep_top { y < ay } else { y > ay };
                    if on_src {
                        let my = 2 * ay - y;
                        if my >= 0 && my < h {
                            let p = *img.get_pixel(x as u32, y as u32);
                            img.put_pixel(x as u32, my as u32, p);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Fill a region with procedural noise mapped through colour `stops`. `kind`
    /// "cloud" (fBm value noise, `octaves`), "perlin" (gradient) or "voronoi"
    /// (cellular). `scale` is the feature size in pixels; `blend` composites over
    /// existing pixels. Textures, terrain, organic mottling.
    #[allow(clippy::too_many_arguments)]
    pub fn noise(
        &mut self,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        scale: f32,
        octaves: u32,
        seed: u64,
        mut stops: Vec<(f32, [u8; 4])>,
        blend: bool,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("noise needs at least one colour stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (ax, bx) = (x0.min(x1).max(0), x0.max(x1).min(w - 1));
        let (ay, by) = (y0.min(y1).max(0), y0.max(y1).min(h - 1));
        let freq = 1.0 / scale.max(0.0001);
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let (fx, fy) = (x as f32 * freq, y as f32 * freq);
                let t = match kind {
                    "perlin" => raster::perlin(fx, fy, seed),
                    "voronoi" => raster::voronoi(fx, fy, seed),
                    _ => raster::fbm(fx, fy, seed, octaves),
                }
                .clamp(0.0, 1.0);
                let c = raster::sample_gradient(&stops, t, "none", x, y, seed);
                let (ux, uy) = (x as u32, y as u32);
                let out = if blend {
                    raster::over(img.get_pixel(ux, uy).0, c)
                } else {
                    c
                };
                img.put_pixel(ux, uy, Rgba(out));
            }
        }
        Ok(())
    }

    /// Shift hue (`dh` degrees) and add to saturation (`ds`) / lightness (`dl`,
    /// each -1..1) of every opaque pixel, optionally limited to `region`.
    /// Recolour / tint / brighten part of a cel.
    pub fn adjust(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        dh: f32,
        ds: f32,
        dl: f32,
    ) -> Result<(), String> {
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => match raster::clamp_region(a, b, c, d, w, h) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = img.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let (hh, ss, ll) = raster::rgb_to_hsl(p[0], p[1], p[2]);
                let rgb = raster::hsl_to_rgb(
                    hh + dh,
                    (ss + ds).clamp(0.0, 1.0),
                    (ll + dl).clamp(0.0, 1.0),
                );
                img.put_pixel(x as u32, y as u32, Rgba([rgb[0], rgb[1], rgb[2], p[3]]));
            }
        }
        Ok(())
    }

    /// Edge-lit on-ramp shading. For each opaque pixel: if the neighbour 1px
    /// *toward* the light is transparent/outside it is a lit rim (push `steps`
    /// toward the light end); if the neighbour *away* from the light is
    /// transparent/outside it is core shadow (push `steps` toward the dark end).
    /// `mode` limits to just highlights or just shadows.
    ///
    /// With a `ramp` (ordered dark→light) each touched pixel snaps to its
    /// nearest ramp entry by luma, then moves ±`steps` along the ramp. Without a
    /// ramp we HSL-shift: lit pixels gain +12% lightness/`steps` and warm their
    /// hue toward 50°; shadow pixels lose 12%/`steps` and cool toward 250°.
    /// Reads the pre-op cel for neighbour tests so all writes are simultaneous
    /// (a rim pixel never re-reads an already-shaded neighbour mid-pass).
    #[allow(clippy::too_many_arguments)]
    pub fn shade(
        &mut self,
        layer: usize,
        frame: usize,
        light_dir: &str,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<(), String> {
        let (ldx, ldy) = match light_dir {
            "top-left" => (-1, -1),
            "top" => (0, -1),
            "top-right" => (1, -1),
            "left" => (-1, 0),
            "right" => (1, 0),
            "bottom-left" => (-1, 1),
            "bottom" => (0, 1),
            "bottom-right" => (1, 1),
            other => {
                return Err(format!(
                    "unknown light_dir '{}' — use top-left/top/top-right/left/right/bottom-left/bottom/bottom-right",
                    other
                ))
            }
        };
        let (do_hi, do_sh) = match mode {
            "both" => (true, true),
            "highlight" => (true, false),
            "shadow" => (false, true),
            other => {
                return Err(format!(
                    "unknown mode '{}' — use both/highlight/shadow",
                    other
                ))
            }
        };
        let steps = steps.max(1);
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => match raster::clamp_region(a, b, c, d, w, h) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let (w, h) = (w as i32, h as i32);
        // Snapshot so neighbour opacity/colour reads are all pre-op.
        let before = self.cel_canvas(layer, frame)?.clone();
        let opaque = |x: i32, y: i32| -> bool {
            x >= 0 && y >= 0 && x < w && y < h && before.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = before.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                // Lit rim wins over core shadow when a pixel is both (thin art).
                let lit = !opaque(x + ldx, y + ldy);
                let shadow = !opaque(x - ldx, y - ldy);
                let dir = if lit && do_hi {
                    1
                } else if shadow && do_sh {
                    -1
                } else {
                    continue;
                };
                let out = match &ramp {
                    Some(r) if !r.is_empty() => raster::shade_ramp(p, r, dir * steps),
                    _ => raster::shade_hsl(p, dir, steps),
                };
                img.put_pixel(x as u32, y as u32, Rgba(out));
            }
        }
        Ok(())
    }

    /// Two-colour ordered dither over a region. `pattern` "checker"/"bayer2"/
    /// "bayer4"/"bayer8"; `density` 0..1 biases the mix toward `color_b` (the
    /// fraction of pixels that take color_b via the threshold matrix). When
    /// `only_existing` only pixels already equal to color_a or color_b are
    /// repainted — recolour an existing flat region into a dither without
    /// spilling onto neighbouring art. Honours an active selection via the
    /// studio mask. Reuses the shared Bayer thresholds (bayer8).
    #[allow(clippy::too_many_arguments)]
    pub fn dither(
        &mut self,
        layer: usize,
        frame: usize,
        region: (i32, i32, i32, i32),
        color_a: [u8; 4],
        color_b: [u8; 4],
        pattern: &str,
        density: f32,
        only_existing: bool,
    ) -> Result<(), String> {
        let valid = ["checker", "bayer2", "bayer4", "bayer8"];
        if !valid.contains(&pattern) {
            return Err(format!(
                "unknown pattern '{}' — use checker/bayer2/bayer4/bayer8",
                pattern
            ));
        }
        let density = density.clamp(0.0, 1.0);
        let (x0, y0, x1, y1) = region;
        let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, self.meta.w, self.meta.h)
            .ok_or("dither region is empty after clamping to the canvas")?;
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                if only_existing {
                    let p = img.get_pixel(x as u32, y as u32).0;
                    if p != color_a && p != color_b {
                        continue;
                    }
                }
                // threshold in [0,1): paint color_b where density exceeds it.
                let c = if density > raster::dither_threshold(pattern, x, y) {
                    color_b
                } else {
                    color_a
                };
                img.put_pixel(x as u32, y as u32, Rgba(c));
            }
        }
        Ok(())
    }

    /// Remove L-corner doubles from 1px strokes (Aseprite "pixel-perfect"
    /// cleanup). A pixel P is erased when it matches the target colour(s), two
    /// orthogonally-adjacent neighbours forming an L (left+top, top+right,
    /// right+bottom or bottom+left) also match, AND the diagonal cell between
    /// that pair does NOT match — i.e. P only exists to thicken an elbow.
    /// Iterates to a fixpoint (max 8 passes). `color` (optional) restricts the
    /// target to strokes of that exact colour. Returns the count erased.
    pub fn pixel_perfect(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color: Option<[u8; 4]>,
    ) -> Result<u32, String> {
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => match raster::clamp_region(a, b, c, d, w, h) {
                Some(r) => r,
                None => return Ok(0),
            },
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let (w, h) = (w as i32, h as i32);
        let img = self.cel_canvas(layer, frame)?;
        // A cell "matches" the stroke when opaque (and == color, if restricted).
        let stroke = |img: &RgbaImage, x: i32, y: i32| -> bool {
            if x < 0 || y < 0 || x >= w || y >= h {
                return false;
            }
            let p = img.get_pixel(x as u32, y as u32).0;
            match color {
                Some(c) => p == c,
                None => p[3] > 0,
            }
        };
        // The four L-corners: (a-offset, b-offset, diagonal-offset).
        let corners = [
            ((-1, 0), (0, -1), (-1, -1)), // left + top
            ((0, -1), (1, 0), (1, -1)),   // top + right
            ((1, 0), (0, 1), (1, 1)),     // right + bottom
            ((0, 1), (-1, 0), (-1, 1)),   // bottom + left
        ];
        let mut removed = 0u32;
        for _ in 0..8 {
            let mut drop: Vec<(i32, i32)> = Vec::new();
            for y in ay..=by {
                for x in ax..=bx {
                    if !stroke(img, x, y) {
                        continue;
                    }
                    let elbow = corners.iter().any(|&((ax2, ay2), (bx2, by2), (dx, dy))| {
                        stroke(img, x + ax2, y + ay2)
                            && stroke(img, x + bx2, y + by2)
                            && !stroke(img, x + dx, y + dy)
                    });
                    if elbow {
                        drop.push((x, y));
                    }
                }
            }
            if drop.is_empty() {
                break;
            }
            for (x, y) in drop {
                img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Snap every opaque pixel to the nearest colour in `palette`. With an empty
    /// palette, derive one of `max_colors` from the cel by median cut. Returns
    /// the palette used. Posterise / down-palette imported art.
    pub fn quantize(
        &mut self,
        layer: usize,
        frame: usize,
        palette: Vec<[u8; 4]>,
        max_colors: usize,
    ) -> Result<Vec<[u8; 4]>, String> {
        let img = self.cel_canvas(layer, frame)?;
        let pal = if !palette.is_empty() {
            palette
        } else {
            let opaque: Vec<[u8; 3]> = img
                .pixels()
                .filter(|p| p.0[3] > 0)
                .map(|p| [p.0[0], p.0[1], p.0[2]])
                .collect();
            raster::median_cut(&opaque, max_colors)
        };
        if pal.is_empty() {
            return Err("quantize needs a palette or max_colors >= 1".into());
        }
        for p in img.pixels_mut() {
            if p.0[3] == 0 {
                continue;
            }
            let nearest = pal
                .iter()
                .min_by_key(|c| {
                    let d = |i: usize| (c[i] as i32 - p.0[i] as i32).pow(2);
                    d(0) + d(1) + d(2)
                })
                .unwrap();
            *p = Rgba([nearest[0], nearest[1], nearest[2], p.0[3]]);
        }
        Ok(pal)
    }

    /// Insert `steps` cross-faded (dissolve) in-between frames after frame
    /// `from`, interpolating every layer toward frame `to`. Reindexes later cels.
    pub fn tween(
        &mut self,
        from: usize,
        to: usize,
        steps: usize,
        duration_ms: u32,
    ) -> Result<usize, String> {
        let n = self.meta.frames.len();
        if from >= n || to >= n {
            return Err(format!(
                "tween frames {}->{} out of range (frames={})",
                from, to, n
            ));
        }
        if to <= from {
            return Err("tween requires to > from".into());
        }
        let steps = steps.max(1);
        let insert_at = from + 1;
        // Capture full-canvas source/target images per layer before reindexing.
        let nl = self.meta.layers.len();
        let pairs: Vec<(RgbaImage, RgbaImage)> = (0..nl)
            .map(|l| (self.cel_full(l, from), self.cel_full(l, to)))
            .collect();
        // Shift cels at/after the insertion point up by `steps`.
        let keys: Vec<(usize, usize)> = self
            .cels
            .keys()
            .filter(|k| k.1 >= insert_at)
            .cloned()
            .collect();
        let mut moved = Vec::new();
        for k in keys {
            let v = self.cels.remove(&k).unwrap();
            moved.push(((k.0, k.1 + steps), v));
        }
        self.cels.extend(moved);
        // Insert frame metadata.
        for i in 0..steps {
            self.meta.frames.insert(
                insert_at + i,
                FrameMeta {
                    duration_ms,
                    pivot: None,
                },
            );
        }
        // Build cross-fade cels.
        let (w, h) = (self.meta.w, self.meta.h);
        for s in 1..=steps {
            let t = s as f32 / (steps + 1) as f32;
            let fidx = insert_at + (s - 1);
            for (l, (a, b)) in pairs.iter().enumerate() {
                let mut img = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
                for y in 0..h {
                    for x in 0..w {
                        let pa = a.get_pixel(x, y).0;
                        let pb = b.get_pixel(x, y).0;
                        let mix = |i: usize| {
                            (pa[i] as f32 + (pb[i] as f32 - pa[i] as f32) * t)
                                .round()
                                .clamp(0.0, 255.0) as u8
                        };
                        img.put_pixel(x, y, Rgba([mix(0), mix(1), mix(2), mix(3)]));
                    }
                }
                self.cels.insert((l, fidx), (0, 0, img));
            }
        }
        Ok(steps)
    }

    /// Resolve the ordered frame indices to *play*. With `tag`, honour that
    /// tag's `[from,to]` range and direction; without one, play the whole
    /// timeline forward. `reverse` plays high→low. `pingpong` plays forward
    /// then back over the inner frames only (endpoints not duplicated) so a
    /// looping playback doesn't stutter on the turn-around frames.
    pub fn play_sequence(&self, tag: Option<&str>) -> Result<Vec<usize>, String> {
        if self.meta.frames.is_empty() {
            return Ok(vec![]);
        }
        let (from, to, dir) = match tag {
            Some(name) => {
                let t = self
                    .meta
                    .tags
                    .iter()
                    .find(|t| t.name == name)
                    .ok_or_else(|| format!("no tag '{}'", name))?;
                (t.from, t.to, t.direction.as_str())
            }
            None => (0, self.meta.frames.len() - 1, "forward"),
        };
        // Clamp defensively in case a tag references frames since removed.
        let last = self.meta.frames.len() - 1;
        let (from, to) = (from.min(last), to.min(last));
        let fwd: Vec<usize> = (from..=to).collect();
        let seq = match dir {
            "reverse" => fwd.into_iter().rev().collect(),
            "pingpong" => {
                let mut s = fwd;
                if to > from + 1 {
                    s.extend((from + 1..to).rev()); // inner frames, no dup of endpoints
                }
                s
            }
            _ => fwd, // "forward" and any unknown direction
        };
        Ok(seq)
    }

    /// Eased multi-frame region motion. `from_frame`'s region content is the
    /// source; for each frame f in (from, to] it is stamped (source-over) at the
    /// eased offset `(round(dx*t), round(dy*t))`, where t advances from 0→1 over
    /// the span shaped by `easing`. With `clear_source` the ORIGINAL region rect
    /// is cleared in each destination frame first (so a moved limb leaves no
    /// stale copy behind). `from_frame` itself is never touched. Reuses the
    /// region copy/clear/paste clipboard internals. Returns the per-frame applied
    /// offsets `[[dx,dy], ...]`.
    pub fn keyframe_move(
        &mut self,
        layer: usize,
        region: (i32, i32, i32, i32),
        from_frame: usize,
        to_frame: usize,
        dx: i32,
        dy: i32,
        easing: &str,
        clear_source: bool,
    ) -> Result<Vec<[i32; 2]>, String> {
        if to_frame <= from_frame {
            return Err("keyframe_move needs to_frame > from_frame".into());
        }
        let n = self.meta.frames.len();
        if to_frame >= n {
            return Err(format!(
                "frame {} does not exist (frames={}) — add it with doc_add_frame first",
                to_frame, n
            ));
        }
        // Snapshot the source region content once, from the start keyframe. The
        // anchored top-left is where it gets re-stamped (clamped like copy_region).
        let (x0, y0, x1, y1) = region;
        let (rw, rh, buf) = self.copy_region(layer, from_frame, x0, y0, x1, y1)?;
        let (ax, ay) = (x0.min(x1).max(0), y0.min(y1).max(0));
        let span = (to_frame - from_frame) as f32;
        let mut offsets: Vec<[i32; 2]> = Vec::new();
        for f in (from_frame + 1)..=to_frame {
            let t = raster::ease((f - from_frame) as f32 / span, easing);
            let (ox, oy) = (
                (dx as f32 * t).round() as i32,
                (dy as f32 * t).round() as i32,
            );
            if clear_source {
                self.clear_region(layer, f, x0, y0, x1, y1)?;
            }
            self.paste_region(layer, f, ax + ox, ay + oy, rw, rh, &buf, true)?;
            offsets.push([ox, oy]);
        }
        Ok(offsets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn palette_set_and_index() {
        let mut d = Document::new("t", 4, 4);
        d.set_palette(vec![[1, 1, 1, 255], [2, 2, 2, 255]]);
        assert_eq!(d.meta.palette.len(), 2);
        assert_eq!(d.meta.palette[1], [2, 2, 2, 255]);
    }

    #[test]
    fn pivot_set_and_clear() {
        let mut d = Document::new("t", 4, 4);
        d.set_pivot(0, Some([2, 3])).unwrap();
        assert_eq!(d.meta.frames[0].pivot, Some([2, 3]));
        d.set_pivot(0, None).unwrap();
        assert_eq!(d.meta.frames[0].pivot, None);
        assert!(d.set_pivot(9, Some([0, 0])).is_err());
    }

    #[test]
    fn save_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("atelier_doc_rt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let mut d = Document::new("rt", 4, 4);
        let mut img = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 0]));
        img.put_pixel(0, 0, Rgba([10, 20, 30, 255]));
        d.set_cel(0, 0, 1, 1, img).unwrap();
        d.save(&dir).unwrap();

        let loaded = Document::load(&dir).unwrap();
        assert_eq!(loaded.meta.name, "rt");
        assert_eq!((loaded.meta.w, loaded.meta.h), (4, 4));
        // the cel is recorded in meta at the offset it was placed
        assert_eq!(loaded.meta.cels.len(), 1);
        let c = &loaded.meta.cels[0];
        assert_eq!((c.layer, c.frame, c.x, c.y), (0, 0, 1, 1));
        // the pixel painted into the cel survives the round-trip
        let cel_img = image::open(dir.join(&c.file)).unwrap().to_rgba8();
        assert_eq!(cel_img.get_pixel(0, 0).0, [10, 20, 30, 255]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn structure_reports_layers_frames_and_cels() {
        let mut d = Document::new("s", 4, 4);
        d.add_layer(None, 255, "normal".into());
        let img = RgbaImage::from_pixel(1, 1, Rgba([1, 1, 1, 255]));
        d.set_cel(1, 0, 0, 0, img).unwrap();
        let v = d.structure();
        assert_eq!(v["name"], "s");
        assert_eq!(v["layers"].as_array().unwrap().len(), 2);
        assert_eq!(v["frames"].as_array().unwrap().len(), 1);
        let cels = v["cels"].as_array().unwrap();
        assert_eq!(cels.len(), 1);
        assert_eq!(cels[0]["layer"], 1);
        assert_eq!(cels[0]["frame"], 0);
    }

    #[test]
    fn filled_ellipse_top_row_is_wide_not_a_nub() {
        // Regression: the old rasteriser left a single-pixel spike at each
        // cardinal tip. The half-pixel-inflated test rounds them.
        let mut d = Document::new("t", 48, 24);
        d.ellipse(0, 0, 24, 12, 12, 8, [0, 200, 0, 255], true)
            .unwrap();
        let top = 12 - 8; // y of the extreme top row
        let width = (0..48)
            .filter(|x| d.get_pixel(0, 0, *x, top).unwrap()[3] > 0)
            .count();
        assert!(width >= 5, "top row width {} — looks like a nub", width);
    }

    #[test]
    fn ellipse_outline_is_closed_and_thin() {
        // Every outline pixel is opaque; the centre stays empty (true ring).
        let mut d = Document::new("t", 40, 40);
        d.ellipse(0, 0, 20, 20, 15, 15, [200, 0, 0, 255], false)
            .unwrap();
        assert_eq!(d.get_pixel(0, 0, 20, 20).unwrap(), [0, 0, 0, 0]); // hollow centre
        assert!(d.get_pixel(0, 0, 20, 5).unwrap()[3] > 0); // top of ring drawn
        assert!(d.get_pixel(0, 0, 5, 20).unwrap()[3] > 0); // left of ring drawn
    }

    #[test]
    fn filled_polygon_covers_interior_only() {
        let mut d = Document::new("t", 24, 24);
        let tri = [(2, 2), (20, 2), (11, 18)];
        d.polygon(0, 0, &tri, [60, 60, 200, 255], true).unwrap();
        assert!(d.get_pixel(0, 0, 11, 8).unwrap()[3] > 0); // inside
        assert_eq!(d.get_pixel(0, 0, 2, 17).unwrap(), [0, 0, 0, 0]); // outside (bottom-left)
        assert!(d.get_pixel(0, 0, 11, 18).unwrap()[3] > 0); // apex vertex stroked
    }

    #[test]
    fn polyline_draws_segments_and_can_close() {
        let mut d = Document::new("t", 16, 16);
        d.polyline(0, 0, &[(1, 1), (10, 1), (10, 10)], [9, 9, 9, 255], 1, false)
            .unwrap();
        assert!(d.get_pixel(0, 0, 5, 1).unwrap()[3] > 0); // along first segment
        assert!(d.get_pixel(0, 0, 10, 5).unwrap()[3] > 0); // along second segment
        assert_eq!(d.get_pixel(0, 0, 5, 10).unwrap(), [0, 0, 0, 0]); // open: no closing edge
    }

    #[test]
    fn apply_masked_confines_op_to_mask() {
        let mut d = Document::new("t", 4, 4);
        let mut mask = vec![false; 16];
        mask[2 * 4 + 2] = true; // select only (x=2,y=2)
        d.apply_masked(0, 0, &mask, |dd| dd.fill_cel(0, 0, [200, 0, 0, 255]))
            .unwrap();
        assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [200, 0, 0, 255]); // masked pixel painted
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]); // rest restored
    }

    /// Two opaque full-cel layers, top with `mode`, flattened at frame 0.
    fn blend_two(mode: &str, bottom: [u8; 4], top: [u8; 4]) -> [u8; 4] {
        let mut d = Document::new("t", 1, 1);
        d.fill_cel(0, 0, bottom).unwrap();
        let l = d.add_layer(None, 255, mode.into());
        d.fill_cel(l, 0, top).unwrap();
        d.flatten(0).get_pixel(0, 0).0
    }

    #[test]
    fn normal_blend_matches_plain_source_over() {
        // Opaque top fully covers the backdrop, unchanged from old compositor.
        assert_eq!(
            blend_two("normal", [255, 0, 0, 255], [0, 255, 0, 255]),
            [0, 255, 0, 255]
        );
    }

    #[test]
    fn multiply_darkens_screen_lightens() {
        // red x green channelwise -> black; red screen green -> yellow.
        assert_eq!(
            blend_two("multiply", [255, 0, 0, 255], [0, 255, 0, 255]),
            [0, 0, 0, 255]
        );
        assert_eq!(
            blend_two("screen", [255, 0, 0, 255], [0, 255, 0, 255]),
            [255, 255, 0, 255]
        );
        assert_eq!(
            blend_two("add", [200, 100, 0, 255], [100, 200, 50, 255]),
            [255, 255, 50, 255]
        );
    }

    #[test]
    fn multiply_over_empty_backdrop_keeps_source() {
        // No backdrop (αb=0): a multiply layer must not collapse to black.
        let mut d = Document::new("t", 1, 1);
        let l = d.add_layer(None, 255, "multiply".into());
        d.fill_cel(l, 0, [40, 90, 160, 255]).unwrap();
        assert_eq!(d.flatten(0).get_pixel(0, 0).0, [40, 90, 160, 255]);
    }

    #[test]
    fn layer_opacity_blends_toward_backdrop() {
        // A 50%-opacity red layer over an opaque black backdrop flattens to ~half red.
        let mut d = Document::new("t", 1, 1);
        d.fill_cel(0, 0, [0, 0, 0, 255]).unwrap();
        let l = d.add_layer(None, 128, "normal".into());
        d.fill_cel(l, 0, [255, 0, 0, 255]).unwrap();
        let p = d.flatten(0).get_pixel(0, 0).0;
        assert!(
            (p[0] as i32 - 128).abs() <= 2,
            "expected ~128, got {}",
            p[0]
        );
        assert_eq!(p[3], 255);
    }

    #[test]
    fn render_preview_tile_and_region_size() {
        let d = Document::new("t", 4, 4);
        let tiled = d.render_preview(0, 1, None, false, 3, None).unwrap();
        assert_eq!((tiled.width(), tiled.height()), (12, 12)); // 3×3 grid
        let crop = d
            .render_preview(0, 1, Some((0, 0, 1, 1)), false, 1, None)
            .unwrap();
        assert_eq!((crop.width(), crop.height()), (2, 2));
    }

    #[test]
    fn copy_then_paste_replicates_a_block() {
        let mut d = Document::new("t", 8, 8);
        d.rect(0, 0, 0, 0, 1, 1, [9, 9, 9, 255], true, 1).unwrap(); // 2x2 block
        let (w, h, buf) = d.copy_region(0, 0, 0, 0, 1, 1).unwrap();
        assert_eq!((w, h), (2, 2));
        d.paste_region(0, 0, 5, 5, w, h, &buf, false).unwrap();
        assert_eq!(d.get_pixel(0, 0, 6, 6).unwrap(), [9, 9, 9, 255]);
    }

    #[test]
    fn move_region_clears_source_and_fills_dest() {
        let mut d = Document::new("t", 8, 8);
        d.pencil(0, 0, &[(1, 1)], [5, 5, 5, 255], 1).unwrap();
        d.move_region(0, 0, 1, 1, 1, 1, 3, 0).unwrap();
        assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [0, 0, 0, 0]); // source cleared
        assert_eq!(d.get_pixel(0, 0, 4, 1).unwrap(), [5, 5, 5, 255]); // moved here
    }

    #[test]
    fn paste_blend_keeps_dest_under_transparent_source() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], [1, 2, 3, 255], 1).unwrap();
        let buf = vec![0u8; 4]; // 1x1 fully transparent
        d.paste_region(0, 0, 0, 0, 1, 1, &buf, true).unwrap(); // blend: no-op
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [1, 2, 3, 255]);
        d.paste_region(0, 0, 0, 0, 1, 1, &buf, false).unwrap(); // overwrite: erases
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn copy_region_clamps_to_canvas() {
        let d = Document::new("t", 4, 4);
        let (w, h, _) = d.copy_region(0, 0, -5, -5, 100, 100).unwrap();
        assert_eq!((w, h), (4, 4));
    }

    #[test]
    fn blur_spreads_a_dot() {
        let mut d = Document::new("t", 5, 5);
        d.pencil(0, 0, &[(2, 2)], [255, 255, 255, 255], 1).unwrap();
        d.blur(0, 0, 1, None).unwrap();
        assert!(d.get_pixel(0, 0, 2, 1).unwrap()[3] > 0); // bled into neighbour
        assert!(d.get_pixel(0, 0, 2, 2).unwrap()[3] < 255); // centre softened
    }

    #[test]
    fn drop_shadow_adds_offset_silhouette() {
        let mut d = Document::new("t", 8, 8);
        d.rect(0, 0, 1, 1, 3, 3, [255, 255, 255, 255], true, 1)
            .unwrap();
        d.drop_shadow(0, 0, 2, 2, [0, 0, 0, 255], 200, 0).unwrap();
        assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [255, 255, 255, 255]); // art still on top
        assert!(d.get_pixel(0, 0, 5, 5).unwrap()[3] > 0); // shadow offset by (2,2)
    }

    #[test]
    fn gradient_linear_lerps_between_stops() {
        let mut d = Document::new("t", 4, 1);
        d.gradient(
            0,
            0,
            "linear",
            0,
            0,
            3,
            0,
            vec![(0.0, [0, 0, 0, 255]), (1.0, [255, 255, 255, 255])],
            "none",
            0,
            None,
            false,
        )
        .unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 255]);
        assert_eq!(d.get_pixel(0, 0, 3, 0).unwrap(), [255, 255, 255, 255]);
        assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [85, 85, 85, 255]); // t=1/3
    }

    #[test]
    fn gradient_dither_uses_only_stop_colors() {
        let mut d = Document::new("t", 8, 8);
        let (a, b) = ([10, 20, 30, 255], [200, 210, 220, 255]);
        d.gradient(
            0,
            0,
            "linear",
            0,
            0,
            7,
            0,
            vec![(0.0, a), (1.0, b)],
            "bayer",
            0,
            None,
            false,
        )
        .unwrap();
        for x in 0..8 {
            for y in 0..8 {
                let p = d.get_pixel(0, 0, x, y).unwrap();
                assert!(p == a || p == b, "dither pixel {:?} not a stop colour", p);
            }
        }
    }

    #[test]
    fn gradient_region_clips() {
        let mut d = Document::new("t", 8, 8);
        d.gradient(
            0,
            0,
            "linear",
            0,
            0,
            7,
            0,
            vec![(0.0, [9, 9, 9, 255]), (1.0, [9, 9, 9, 255])],
            "none",
            0,
            Some((2, 2, 5, 5)),
            false,
        )
        .unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]); // outside region untouched
        assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [9, 9, 9, 255]); // inside painted
    }

    #[test]
    fn scatter_is_deterministic_and_density_bounded() {
        let count = |seed: u64, dens: f32| {
            let mut d = Document::new("t", 16, 16);
            d.scatter(0, 0, 0, 0, 15, 15, &[[255, 0, 0, 255]], dens, seed, 1)
                .unwrap();
            (0..16)
                .flat_map(|y| (0..16).map(move |x| (x, y)))
                .filter(|(x, y)| d.get_pixel(0, 0, *x, *y).unwrap()[3] > 0)
                .count()
        };
        assert_eq!(count(0, 0.0), 0); // density 0 paints nothing
        assert_eq!(count(7, 0.3), count(7, 0.3)); // same seed reproduces
        assert!(count(7, 0.3) < count(7, 0.8)); // higher density paints more
    }

    #[test]
    fn stamp_draws_over_without_replacing() {
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [0, 255, 0, 255]).unwrap(); // green backdrop
        let src = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
        d.stamp(0, 0, 0, 0, &src, 255, "normal").unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]); // stamped
        assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [0, 255, 0, 255]); // backdrop kept (not replaced)
    }

    #[test]
    fn symmetry_mirrors_across_vertical_axis() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 1)], [9, 9, 9, 255], 1).unwrap();
        d.symmetry(0, 0, Some(1), None, true, false).unwrap(); // reflect left over column 1
        assert_eq!(d.get_pixel(0, 0, 2, 1).unwrap(), [9, 9, 9, 255]); // 2*1-0 = 2
    }

    #[test]
    fn quantize_snaps_to_palette() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], [250, 10, 10, 255], 1).unwrap();
        d.pencil(0, 0, &[(1, 0)], [10, 10, 250, 255], 1).unwrap();
        d.quantize(0, 0, vec![[255, 0, 0, 255], [0, 0, 255, 255]], 2)
            .unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
        assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 255, 255]);
    }

    #[test]
    fn quantize_derives_palette_by_median_cut() {
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [20, 30, 40, 255]).unwrap();
        let pal = d.quantize(0, 0, vec![], 4).unwrap();
        assert!(!pal.is_empty() && pal.len() <= 4);
    }

    #[test]
    fn adjust_hue_rotates_red_toward_green() {
        let mut d = Document::new("t", 2, 2);
        d.fill_cel(0, 0, [200, 0, 0, 255]).unwrap(); // hue 0
        d.adjust(0, 0, None, 120.0, 0.0, 0.0).unwrap(); // +120° → green
        let p = d.get_pixel(0, 0, 0, 0).unwrap();
        assert!(
            p[1] > p[0] && p[1] > p[2],
            "expected green-dominant, got {:?}",
            p
        );
    }

    fn doc_with_frames(n: usize) -> Document {
        let mut d = Document::new("t", 4, 4);
        while d.meta.frames.len() < n {
            d.add_frame(100, None);
        }
        d
    }

    #[test]
    fn no_tag_plays_whole_timeline_forward() {
        let d = doc_with_frames(4);
        assert_eq!(d.play_sequence(None).unwrap(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn forward_tag_is_inclusive_range() {
        let mut d = doc_with_frames(5);
        d.add_tag("walk", 1, 3, "forward").unwrap();
        assert_eq!(d.play_sequence(Some("walk")).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn reverse_tag_plays_high_to_low() {
        let mut d = doc_with_frames(5);
        d.add_tag("rev", 1, 3, "reverse").unwrap();
        assert_eq!(d.play_sequence(Some("rev")).unwrap(), vec![3, 2, 1]);
    }

    #[test]
    fn pingpong_does_not_duplicate_endpoints() {
        let mut d = doc_with_frames(4);
        d.add_tag("blink", 0, 2, "pingpong").unwrap();
        // open -> half -> closed -> half (-> loops to open), no double closed/open
        assert_eq!(d.play_sequence(Some("blink")).unwrap(), vec![0, 1, 2, 1]);
    }

    #[test]
    fn pingpong_two_frame_range_has_no_inner_turnaround() {
        let mut d = doc_with_frames(3);
        d.add_tag("pp", 0, 1, "pingpong").unwrap();
        assert_eq!(d.play_sequence(Some("pp")).unwrap(), vec![0, 1]);
    }

    #[test]
    fn unknown_tag_errors() {
        let d = doc_with_frames(2);
        assert!(d.play_sequence(Some("nope")).is_err());
    }

    #[test]
    fn tag_range_clamps_to_existing_frames() {
        // A tag added when there were more frames must not index out of bounds.
        let mut d = doc_with_frames(5);
        d.add_tag("big", 0, 4, "forward").unwrap();
        d.meta.frames.truncate(3);
        assert_eq!(d.play_sequence(Some("big")).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn tween_inserts_and_reindexes_frames() {
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [10, 10, 10, 255]).unwrap();
        d.add_frame(100, None);
        d.fill_cel(0, 1, [200, 200, 200, 255]).unwrap();
        d.tween(0, 1, 1, 100).unwrap(); // one in-between after frame 0
        assert_eq!(d.meta.frames.len(), 3);
        assert_eq!(d.get_pixel(0, 2, 0, 0).unwrap(), [200, 200, 200, 255]); // old frame 1 → 2
        let mid = d.get_pixel(0, 1, 0, 0).unwrap();
        assert!(
            (mid[0] as i32 - 105).abs() <= 2,
            "tween mid ~105, got {}",
            mid[0]
        ); // dissolve
    }
}
