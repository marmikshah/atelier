//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by its `id` (a slug derived from its name).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::document::Document;

fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in name.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

/// An active pixel selection: which document it belongs to, its dimensions, and
/// one `bool` per pixel (row-major). Painting ops confine to the `true` pixels.
// Constructed by `doc_select` (a later step); for now the painting ops read it.
#[allow(dead_code)]
#[derive(Clone)]
struct Selection {
    doc_id: String,
    w: u32,
    h: u32,
    mask: Vec<bool>,
}

#[derive(Clone)]
pub struct Studio {
    docs_dir: PathBuf,
    /// Active selection mask (at most one), set by `doc_select`; painting ops
    /// confine to it. Process-lived, like the clipboard.
    selection: Option<Selection>,
}

impl Studio {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Studio {
        let home = std::env::var("ATELIER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".atelier"));
        let docs_dir = home.join("documents");
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            selection: None,
        }
    }

    /// Test-only: build a studio rooted at an explicit directory (avoids the
    /// process-global ATELIER_HOME env var, so tests stay parallel-safe).
    #[cfg(test)]
    fn with_docs_dir(docs_dir: PathBuf) -> Studio {
        let _ = fs::create_dir_all(&docs_dir);
        Studio {
            docs_dir,
            selection: None,
        }
    }

    fn doc_dir(&self, id: &str) -> PathBuf {
        self.docs_dir.join(id)
    }

    fn exists(&self, id: &str) -> bool {
        self.doc_dir(id).join("doc.json").exists()
    }

    /// All document ids on disk (directories with a doc.json), sorted.
    fn doc_ids(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.docs_dir) {
            for e in rd.flatten() {
                if e.path().join("doc.json").exists() {
                    out.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
        out.sort();
        out
    }

    fn unique_id(&self, base: &str) -> String {
        let base = slugify(base);
        if !self.exists(&base) {
            return base;
        }
        let mut i = 2;
        loop {
            let cand = format!("{}-{}", base, i);
            if !self.exists(&cand) {
                return cand;
            }
            i += 1;
        }
    }

    fn open(&self, id: &str) -> Result<(PathBuf, Document), String> {
        let dir = self.doc_dir(id);
        if !dir.join("doc.json").exists() {
            let existing = self.doc_ids().join(", ");
            return Err(format!(
                "no document '{}'. existing: {}",
                id,
                if existing.is_empty() {
                    "(none)".into()
                } else {
                    existing
                }
            ));
        }
        let doc = Document::load(&dir)?;
        Ok((dir, doc))
    }

    // -- library ------------------------------------------------------------

    pub fn doc_create(&self, name: &str, w: u32, h: u32) -> Result<Value, String> {
        let id = self.unique_id(name);
        let dir = self.doc_dir(&id);
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut doc = Document::new(name, w, h);
        doc.save(&dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn doc_info(&self, id: &str) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn list_docs(&self) -> Value {
        let mut items = Vec::new();
        for id in self.doc_ids() {
            // Read doc.json directly (don't load cel images just to list).
            let meta = fs::read_to_string(self.doc_dir(&id).join("doc.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let (name, w, h, frames, layers) = match &meta {
                Some(m) => (
                    m["name"].clone(),
                    m["w"].clone(),
                    m["h"].clone(),
                    m["frames"].as_array().map(|a| a.len()).unwrap_or(0),
                    m["layers"].as_array().map(|a| a.len()).unwrap_or(0),
                ),
                None => (json!(id), json!(null), json!(null), 0, 0),
            };
            items.push(
                json!({"id": id, "name": name, "w": w, "h": h, "frames": frames, "layers": layers}),
            );
        }
        json!({"count": items.len(), "documents": items})
    }

    pub fn delete_doc(&self, id: &str) -> Result<Value, String> {
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        fs::remove_dir_all(self.doc_dir(id)).map_err(|e| e.to_string())?;
        Ok(json!({"deleted": id}))
    }

    // -- structure / timeline (open -> mutate -> save) ----------------------

    fn commit(&self, dir: &Path, id: &str, mut doc: Document) -> Result<Value, String> {
        doc.save(dir)?;
        let mut out = doc.structure();
        out["id"] = json!(id);
        Ok(out)
    }

    pub fn doc_add_layer(
        &self,
        id: &str,
        name: Option<String>,
        opacity: u8,
        blend: String,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let idx = doc.add_layer(name, opacity, blend);
        let mut out = self.commit(&dir, id, doc)?;
        out["added_layer"] = json!(idx);
        Ok(out)
    }

    pub fn doc_set_layer(
        &self,
        id: &str,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_layer(layer, visible, opacity, blend)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_add_frame(
        &self,
        id: &str,
        duration_ms: u32,
        copy_from: Option<usize>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let idx = doc.add_frame(duration_ms, copy_from);
        let mut out = self.commit(&dir, id, doc)?;
        out["added_frame"] = json!(idx);
        Ok(out)
    }

    pub fn doc_set_frame_duration(&self, id: &str, frame: usize, ms: u32) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_frame_duration(frame, ms)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_add_tag(
        &self,
        id: &str,
        name: &str,
        from: usize,
        to: usize,
        direction: &str,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.add_tag(name, from, to, direction)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_fill_cel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: [u8; 4],
    ) -> Result<Value, String> {
        // Masked: with an active selection this fills only the selected pixels.
        self.edit_masked(id, layer, frame, |d| d.fill_cel(layer, frame, color))
    }

    pub fn doc_clear_cel(&self, id: &str, layer: usize, frame: usize) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.clear_cel(layer, frame);
        self.commit(&dir, id, doc)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_stamp_image(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        png_path: &str,
        scale: f32,
        rotate: f32,
        opacity: u8,
        blend: &str,
        replace: bool,
    ) -> Result<Value, String> {
        let img = image::open(png_path).map_err(|e| e.to_string())?.to_rgba8();
        self.edit_masked(id, layer, frame, |d| {
            d.stamp_image(
                layer, frame, x, y, img, scale, rotate, opacity, blend, replace,
            )
        })
    }

    pub fn doc_symmetry(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        vertical: Option<i32>,
        horizontal: Option<i32>,
        keep_left: bool,
        keep_top: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.symmetry(layer, frame, vertical, horizontal, keep_left, keep_top)
        })
    }

    // -- render / export ----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn doc_render(
        &self,
        id: &str,
        frame: usize,
        out_path: Option<&str>,
        scale: u32,
        region: Option<(i32, i32, i32, i32)>,
        onion: bool,
        tile: u32,
        max_size: Option<u32>,
    ) -> Result<Value, String> {
        let (dir, doc) = self.open(id)?;
        let out = match out_path {
            Some(p) => PathBuf::from(p),
            None => dir.join(format!("preview_f{}.png", frame)),
        };
        if frame >= doc.meta.frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta.frames.len()
            ));
        }
        let img = doc.render_preview(frame, scale.max(1), region, onion, tile, max_size)?;
        let (w, h) = (img.width(), img.height());
        img.save(&out).map_err(|e| e.to_string())?;
        Ok(json!({"path": out.to_string_lossy(), "size": [w, h], "frame": frame}))
    }

    pub fn doc_export_sheet(&self, id: &str, out_path: &str, scale: u32) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        doc.export_sheet(Path::new(out_path), scale.max(1))
    }

    pub fn doc_export_gif(
        &self,
        id: &str,
        out_path: &str,
        scale: u32,
        tag: Option<&str>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        if let Some(p) = Path::new(out_path).parent() {
            let _ = fs::create_dir_all(p);
        }
        let frames = doc.export_gif(Path::new(out_path), scale.max(1), tag)?;
        Ok(json!({"path": out_path, "frames": frames, "tag": tag}))
    }

    // -- per-cel drawing ----------------------------------------------------

    fn edit<F>(&self, id: &str, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        f(&mut doc)?;
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id}))
    }

    /// Like `edit`, but if an active selection covers this document (matching
    /// id + dimensions) the op `f` is confined to the selected pixels. Used by
    /// the painting ops so `doc_select` masks any of them.
    fn edit_masked<F>(&self, id: &str, layer: usize, frame: usize, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        match &self.selection {
            Some(s) if s.doc_id == id && s.w == doc.meta.w && s.h == doc.meta.h => {
                doc.apply_masked(layer, frame, &s.mask, f)?
            }
            _ => f(&mut doc)?,
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id}))
    }

    pub fn doc_pencil(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.pencil(layer, frame, &points, color, size)
        })
    }

    pub fn doc_line(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.line(layer, frame, x0, y0, x1, y1, color, size)
        })
    }

    pub fn doc_rect(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: [u8; 4],
        fill: bool,
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.rect(layer, frame, x0, y0, x1, y1, color, fill, size)
        })
    }

    pub fn doc_ellipse(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        cx: i32,
        cy: i32,
        rx: i32,
        ry: i32,
        color: [u8; 4],
        fill: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.ellipse(layer, frame, cx, cy, rx, ry, color, fill)
        })
    }

    pub fn doc_polygon(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        fill: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.polygon(layer, frame, &points, color, fill)
        })
    }

    pub fn doc_polyline(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
        closed: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.polyline(layer, frame, &points, color, size, closed)
        })
    }

    pub fn doc_fill(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        color: [u8; 4],
        tol: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bucket_fill(layer, frame, x, y, color, tol)
        })
    }

    pub fn doc_replace_color(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        from: [u8; 4],
        to: [u8; 4],
        tol: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.replace_color(layer, frame, from, to, tol)
        })
    }

    pub fn doc_flip(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        horizontal: bool,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.flip(layer, frame, horizontal))
    }

    pub fn doc_shift(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        wrap: bool,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.shift(layer, frame, dx, dy, wrap))
    }

    pub fn doc_blur(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        radius: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| d.blur(layer, frame, radius, region))
    }

    pub fn doc_quantize(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        palette: Vec<[u8; 4]>,
        max_colors: usize,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let pal = doc.quantize(layer, frame, palette, max_colors)?;
        doc.save(&dir)?;
        let hex: Vec<String> = pal
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]))
            .collect();
        Ok(json!({"doc_id": id, "count": pal.len(), "palette": pal, "hex": hex}))
    }

    pub fn doc_tween(
        &self,
        id: &str,
        from: usize,
        to: usize,
        steps: usize,
        duration_ms: u32,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let added = doc.tween(from, to, steps, duration_ms)?;
        let mut out = self.commit(&dir, id, doc)?;
        out["inserted_frames"] = json!(added);
        Ok(out)
    }

    pub fn doc_outline(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        aa: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| d.outline_cel(layer, frame, color, aa))
    }

    pub fn doc_drop_shadow(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        dx: i32,
        dy: i32,
        color: [u8; 4],
        opacity: u8,
        blur: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.drop_shadow(layer, frame, dx, dy, color, opacity, blur)
        })
    }

    pub fn doc_glow(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        color: Option<[u8; 4]>,
        radius: i32,
        intensity: u8,
        mode: &str,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.glow(layer, frame, color, radius, intensity, mode)
        })
    }

    pub fn doc_bevel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        light: [u8; 4],
        dark: [u8; 4],
        depth: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bevel(layer, frame, light, dark, depth)
        })
    }

    pub fn doc_adjust(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        hue: f32,
        sat: f32,
        lum: f32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.adjust(layer, frame, region, hue, sat, lum)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_noise(
        &self,
        id: &str,
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
        stops: Vec<(f32, [u8; 4])>,
        blend: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.noise(
                layer, frame, kind, x0, y0, x1, y1, scale, octaves, seed, stops, blend,
            )
        })
    }

    pub fn doc_bezier(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        points: Vec<(i32, i32)>,
        color: [u8; 4],
        size: i32,
        steps: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.bezier(layer, frame, &points, color, size, steps)
        })
    }

    /// Generate a hue-shifted shading ramp from a base colour. If `set_doc` is
    /// given, also store it as that document's palette. Returns the colours.
    pub fn palette_ramp(
        &self,
        base: [u8; 4],
        count: usize,
        hue_shift: f32,
        light_range: f32,
        sat_shift: f32,
        set_doc: Option<&str>,
    ) -> Result<Value, String> {
        let ramp = crate::raster::make_ramp(base, count, hue_shift, light_range, sat_shift);
        if let Some(id) = set_doc {
            let (dir, mut doc) = self.open(id)?;
            doc.set_palette(ramp.clone());
            doc.save(&dir)?;
        }
        let hex: Vec<String> = ramp
            .iter()
            .map(|c| format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3]))
            .collect();
        Ok(json!({"count": ramp.len(), "colors": ramp, "hex": hex, "set_doc": set_doc}))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_gradient(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        kind: &str,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        stops: Vec<(f32, [u8; 4])>,
        dither: &str,
        seed: u64,
        region: Option<(i32, i32, i32, i32)>,
        blend: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.gradient(
                layer, frame, kind, x0, y0, x1, y1, stops, dither, seed, region, blend,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn doc_scatter(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        colors: Vec<[u8; 4]>,
        density: f32,
        seed: u64,
        size: i32,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.scatter(layer, frame, x0, y0, x1, y1, &colors, density, seed, size)
        })
    }

    /// Edge-lit on-ramp shading: lit rims toward the light, core shadow away.
    /// `ramp` (dark→light) snaps each touched pixel and steps along it; without
    /// one we HSL-shift (warm highlights, cool shadows). Masked by the active
    /// selection, like the other painting ops.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_shade(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        light_dir: &str,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
        mode: &str,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.shade(layer, frame, light_dir, steps, region, mode, ramp)
        })
    }

    /// Two-colour ordered dither over a region. `region` is required unless an
    /// active selection covers this document (the selection then bounds it).
    /// Masked by the active selection, like the other painting ops.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_dither(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color_a: [u8; 4],
        color_b: [u8; 4],
        pattern: &str,
        density: f32,
        only_existing: bool,
    ) -> Result<Value, String> {
        // The region defaults to the selection's bounding box when omitted; if
        // there's neither a region nor a selection it's an error (no target).
        let region = match region {
            Some(r) => r,
            None => self
                .selection_bbox(id)
                .ok_or("dither needs a `region` [x0,y0,x1,y1] unless a selection is active")?,
        };
        self.edit_masked(id, layer, frame, |d| {
            d.dither(
                layer,
                frame,
                region,
                color_a,
                color_b,
                pattern,
                density,
                only_existing,
            )
        })
    }

    /// Remove L-corner doubles from 1px strokes (Aseprite pixel-perfect cleanup).
    /// `color` (optional) restricts to strokes of that exact colour. Masked by
    /// the active selection. Returns the erased-pixel `removed` count.
    pub fn doc_pixel_perfect(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        color: Option<[u8; 4]>,
    ) -> Result<Value, String> {
        // pixel_perfect returns a count, so we thread it out via a cell rather
        // than the unit-returning edit_masked closure.
        let removed = std::cell::Cell::new(0u32);
        self.edit_masked(id, layer, frame, |d| {
            removed.set(d.pixel_perfect(layer, frame, region, color)?);
            Ok(())
        })?;
        Ok(json!({"ok": true, "doc_id": id, "removed": removed.get()}))
    }

    /// The bounding box [x0,y0,x1,y1] of the active selection on document `id`,
    /// or None when there is no matching selection (or it's empty). Lets the
    /// dither op fall back to the selected area when no explicit region.
    fn selection_bbox(&self, id: &str) -> Option<(i32, i32, i32, i32)> {
        let s = self.selection.as_ref().filter(|s| s.doc_id == id)?;
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (i, on) in s.mask.iter().enumerate() {
            if *on {
                let (x, y) = ((i as u32 % s.w) as i32, (i as u32 / s.w) as i32);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
        (x0 <= x1).then_some((x0, y0, x1, y1))
    }

    // -- animation & tiling feedback (read-only) + keyframe write -----------

    /// Eased multi-frame region motion across an existing frame span. The region
    /// content at `from_frame` is stamped (source-over) into every frame in
    /// (from, to] at the eased offset; `clear_source` first clears the original
    /// rect in each destination frame. Reuses the region copy/clear/paste paths.
    #[allow(clippy::too_many_arguments)]
    pub fn doc_keyframe_move(
        &self,
        id: &str,
        layer: usize,
        region: (i32, i32, i32, i32),
        from_frame: usize,
        to_frame: usize,
        dx: i32,
        dy: i32,
        easing: &str,
        clear_source: bool,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let offsets = doc.keyframe_move(
            layer,
            region,
            from_frame,
            to_frame,
            dx,
            dy,
            easing,
            clear_source,
        )?;
        doc.save(&dir)?;
        let offs: Vec<Value> = offsets.iter().map(|o| json!(o)).collect();
        Ok(json!({
            "doc_id": id,
            "frames_touched": offsets.len(),
            "offsets": offs,
        }))
    }

    pub fn doc_move_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        dx: i32,
        dy: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.move_region(layer, frame, x0, y0, x1, y1, dx, dy))
    }

    pub fn doc_clear_region(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    ) -> Result<Value, String> {
        self.edit(id, |d| d.clear_region(layer, frame, x0, y0, x1, y1))
    }

    // -- pivots / palette ---------------------------------------------------

    pub fn doc_set_pivot(
        &self,
        id: &str,
        frame: usize,
        pivot: Option<[i32; 2]>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_pivot(frame, pivot)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_set_palette(&self, id: &str, colors: Vec<[u8; 4]>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_palette(colors);
        let mut out = self.commit(&dir, id, doc)?;
        out["palette_set"] = json!(true);
        Ok(out)
    }

    /// Apply many ordered drawing ops to one cel in a single open→save cycle.
    pub fn doc_batch(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        ops: Vec<Value>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        // Strict pre-flight: reject typo'd / wrong-shape ops up front so the whole
        // batch fails cleanly instead of silently defaulting bad params.
        for (i, op) in ops.iter().enumerate() {
            crate::document::validate_batch_op(i, op)?;
        }
        let run = |doc: &mut Document| -> Result<(), String> {
            for (i, op) in ops.iter().enumerate() {
                doc.apply_op(layer, frame, op)
                    .map_err(|e| format!("op {}: {}", i, e))?;
            }
            Ok(())
        };
        match &self.selection {
            Some(s) if s.doc_id == id && s.w == doc.meta.w && s.h == doc.meta.h => {
                doc.apply_masked(layer, frame, &s.mask, run)?
            }
            _ => run(&mut doc)?,
        }
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "ops": ops.len()}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-test-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn create_persists_and_lists() {
        let s = studio("create");
        s.doc_create("Hero Sprite", 16, 16).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 1);
        // slug derived from the name
        assert_eq!(listed["documents"][0]["id"], "hero-sprite");
        // reloads from disk (open path), not just in-memory
        assert_eq!(s.doc_info("hero-sprite").unwrap()["w"], 16);
    }

    #[test]
    fn slugify_normalizes_names() {
        assert_eq!(slugify("Hero Sprite"), "hero-sprite");
        assert_eq!(slugify("  Multi   Space  "), "multi-space");
        assert_eq!(slugify("Weird!!Chars??"), "weird-chars");
        // empty / punctuation-only falls back
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("---"), "untitled");
    }

    #[test]
    fn unique_id_disambiguates_collisions() {
        let s = studio("unique");
        // three docs with the same name → suffixed slugs
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        s.doc_create("dup", 4, 4).unwrap();
        let listed = s.list_docs();
        assert_eq!(listed["count"], 3);
        let ids: Vec<String> = listed["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["dup", "dup-2", "dup-3"]);
    }

    // Read one pixel via the Document model. The studio-level `doc_get_pixel`
    // reader lands with the analysis readers (a later step); these editing tests
    // assert on raw RGBA, so reading straight off disk keeps them identical.
    fn px(s: &Studio, id: &str, layer: usize, frame: usize, x: i32, y: i32) -> [u8; 4] {
        let (_dir, doc) = s.open(id).unwrap();
        doc.get_pixel(layer, frame, x, y).unwrap()
    }

    #[test]
    fn pivot_and_palette_persist_to_disk() {
        let s = studio("meta");
        s.doc_create("p", 8, 8).unwrap();
        s.doc_set_pivot("p", 0, Some([4, 7])).unwrap();
        s.doc_set_palette("p", vec![[1, 2, 3, 255], [4, 5, 6, 255]])
            .unwrap();
        let info = s.doc_info("p").unwrap(); // reloads from disk
        assert_eq!(info["frames"][0]["pivot"], json!([4, 7]));
        assert_eq!(info["palette_len"], 2);
    }

    #[test]
    fn batch_strict_rejects_unknown_and_missing_keys() {
        let s = studio("batchstrict");
        s.doc_create("d", 8, 8).unwrap();
        // ellipse with rect-style keys → unknown-keys error naming the op
        let bad = s.doc_batch(
            "d",
            0,
            0,
            vec![json!({"op": "ellipse", "x0": 1, "y0": 1, "x1": 8, "y1": 8, "color": [1, 2, 3]})],
        );
        let msg = bad.unwrap_err();
        assert!(msg.contains("op[0]") && msg.contains("ellipse") && msg.contains("x0"));
        // missing a required key
        assert!(s
            .doc_batch("d", 0, 0, vec![json!({"op": "ellipse"})])
            .unwrap_err()
            .contains("missing required keys"));
        // a valid batch still passes
        assert!(s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "ellipse", "cx": 4, "cy": 4, "rx": 3, "ry": 3, "color": [1, 2, 3], "fill": true})],
            )
            .is_ok());
    }

    #[test]
    fn dither_mixes_two_colors_and_respects_only_existing() {
        let s = studio("dither");
        s.doc_create("d", 8, 8).unwrap();
        let a = [10, 10, 10, 255];
        let b = [200, 200, 200, 255];
        // checker over the whole cel: both colours appear, alternating.
        s.doc_dither("d", 0, 0, Some((0, 0, 7, 7)), a, b, "checker", 0.5, false)
            .unwrap();
        let p00 = px(&s, "d", 0, 0, 0, 0);
        let p10 = px(&s, "d", 0, 0, 1, 0);
        assert_ne!(p00, p10); // chequerboard flips each step
        assert!(p00 == a || p00 == b);
        // density 1.0 floods color_b; only_existing keeps untouched art intact.
        s.doc_pencil("d", 0, 0, vec![(0, 0)], [7, 7, 7, 255], 1)
            .unwrap(); // a stray colour, neither a nor b
        s.doc_dither("d", 0, 0, Some((0, 0, 7, 7)), a, b, "bayer4", 1.0, true)
            .unwrap();
        // the stray pixel is left alone (not a or b), the rest become b
        assert_eq!(px(&s, "d", 0, 0, 0, 0), [7, 7, 7, 255]);
        assert_eq!(px(&s, "d", 0, 0, 3, 3), b);
        // no region and no selection → actionable error
        assert!(s
            .doc_dither("d", 0, 0, None, a, b, "checker", 0.5, false)
            .is_err());
    }

    #[test]
    fn shade_lights_rim_toward_light_with_ramp() {
        let s = studio("shade");
        s.doc_create("d", 6, 6).unwrap();
        // a solid mid block; ramp dark→light snaps the mid colour to index 1.
        let dark = [40, 40, 40, 255];
        let mid = [120, 120, 120, 255];
        let light = [220, 220, 220, 255];
        s.doc_rect("d", 0, 0, 1, 1, 4, 4, mid, true, 1).unwrap();
        s.doc_shade(
            "d",
            0,
            0,
            "top-left",
            1,
            None,
            "both",
            Some(vec![dark, mid, light]),
        )
        .unwrap();
        // top-left rim pixel: neighbour toward the light (-1,-1) is empty → lit.
        assert_eq!(px(&s, "d", 0, 0, 1, 1), light);
        // bottom-right rim: neighbour away from light (-1,-1) is solid but the
        // one toward the light is solid too, and away-from-light (+1,+1) is empty
        // → core shadow steps to dark.
        assert_eq!(px(&s, "d", 0, 0, 4, 4), dark);
        // an interior pixel (all neighbours opaque) is untouched.
        assert_eq!(px(&s, "d", 0, 0, 2, 2), mid);
        // bad light_dir is an actionable error
        assert!(s
            .doc_shade("d", 0, 0, "nowhere", 1, None, "both", None)
            .is_err());
    }

    #[test]
    fn pixel_perfect_removes_l_corner() {
        let s = studio("pp");
        s.doc_create("d", 6, 6).unwrap();
        let c = [255, 0, 0, 255];
        // an L: (1,1),(1,2),(2,2). The elbow (1,2) has left+? — build the classic
        // staircase elbow: horizontal then down, with the corner doubled.
        // pixels: (1,1) top, (1,2) corner, (2,2) right of corner.
        s.doc_pencil("d", 0, 0, vec![(1, 1), (1, 2), (2, 2)], c, 1)
            .unwrap();
        let r = s.doc_pixel_perfect("d", 0, 0, None, None).unwrap();
        // the corner pixel (1,2) is an L-double (top (1,1) + right (2,2) set,
        // diagonal (2,1) clear) → removed.
        assert_eq!(r["removed"], json!(1));
        assert_eq!(px(&s, "d", 0, 0, 1, 2), [0, 0, 0, 0]);
        // the two endpoints survive
        assert_eq!(px(&s, "d", 0, 0, 1, 1), c);
        assert_eq!(px(&s, "d", 0, 0, 2, 2), c);
        // colour filter ignores strokes of other colours
        let r2 = s
            .doc_pixel_perfect("d", 0, 0, None, Some([0, 255, 0, 255]))
            .unwrap();
        assert_eq!(r2["removed"], json!(0));
    }

    #[test]
    fn craft_ops_run_in_batch_and_validate_keys() {
        let s = studio("craftbatch");
        s.doc_create("d", 8, 8).unwrap();
        // valid batch: fill then dither then shade then pixel_perfect
        s.doc_batch(
            "d",
            0,
            0,
            vec![
                json!({"op": "fill_cel", "color": [120, 120, 120]}),
                json!({"op": "dither", "region": [0, 0, 7, 7], "color_a": [40, 40, 40], "color_b": [200, 200, 200], "pattern": "bayer4"}),
                json!({"op": "shade", "light_dir": "left", "steps": 1}),
                json!({"op": "pixel_perfect"}),
            ],
        )
        .unwrap();
        // dither op with a rect-style typo key is rejected, naming the op + key
        let bad = s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "dither", "color_a": [0, 0, 0], "color_b": [1, 1, 1], "x0": 0})],
            )
            .unwrap_err();
        assert!(bad.contains("op[0]") && bad.contains("dither") && bad.contains("x0"));
        // dither missing a required colour is rejected
        assert!(s
            .doc_batch(
                "d",
                0,
                0,
                vec![json!({"op": "dither", "color_a": [0, 0, 0]})]
            )
            .unwrap_err()
            .contains("missing required keys"));
    }
}
