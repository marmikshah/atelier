//! The editable document model — atelier's layered, animated core.
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

/// A named collision rectangle on a frame: the body box, an attack hitbox, a
/// vulnerable hurtbox. Pure gameplay metadata — never rasterized; emitted
/// (scaled) in sheet/atlas JSON so the engine reads it straight off the sheet.
#[derive(Serialize, Deserialize, Clone)]
pub struct BoxMeta {
    pub name: String,
    /// `body` | `hit` | `hurt` (validated on set).
    pub kind: String,
    /// `[x, y, w, h]` in document pixels.
    pub rect: [i32; 4],
}

impl BoxMeta {
    /// JSON form with the rect scaled into export pixels (`scale = 1` = raw).
    pub fn to_json(&self, scale: u32) -> Value {
        let s = scale as i32;
        let [x, y, w, h] = self.rect;
        json!({"name": self.name, "kind": self.kind, "rect": [x * s, y * s, w * s, h * s]})
    }
}

/// The accepted box kinds, in declaration order — the error message lists them.
pub const BOX_KINDS: [&str; 3] = ["body", "hit", "hurt"];

#[derive(Serialize, Deserialize, Clone)]
pub struct FrameMeta {
    pub duration_ms: u32,
    /// Optional anchor point in document pixels (e.g. feet / weapon mount).
    /// Engines read this to position the sprite; None = top-left origin.
    #[serde(default)]
    pub pivot: Option<[i32; 2]>,
    /// Collision/hit/hurt boxes on this frame (gameplay metadata, not pixels).
    #[serde(default)]
    pub boxes: Vec<BoxMeta>,
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
    /// Reference image filename inside the doc dir (set by doc_set_reference)
    /// — the original the artwork is recreating, kept for compare loops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub meta: DocMeta,
    /// (layer, frame) -> (x, y, image)
    cels: HashMap<(usize, usize), (i32, i32, RgbaImage)>,
}

/// Result of `frame_diff_region`: `(added, removed, recolored, change_bbox,
/// image_a, image_b)` — the change tallies, the bbox of all changed pixels, and
/// both analysis images so callers can also render a grid/overlay.
pub type FrameDiff = (u32, u32, u32, Option<[i32; 4]>, RgbaImage, RgbaImage);

/// One light for [`Document::relight`]: a direction (need not be unit length),
/// an intensity multiplier, and an RGB colour in 0..1.
pub struct Light {
    pub dir: [f32; 3],
    pub intensity: f32,
    pub color: [f32; 3],
}

fn cel_file(layer: usize, frame: usize) -> String {
    format!("cels/L{}_F{}.png", layer, frame)
}

/// How `snap_to_palette` treats the partial-alpha pixels that continuous-tone FX
/// (glow/blur/gradient/drop_shadow) and AA fringes leave behind — the difference
/// between "snap the colour but keep 200 soft alphas off-palette" and "make it
/// crisp pixel art again".
#[derive(Clone, Copy, Debug)]
pub enum AlphaSnap {
    /// Keep each pixel's source alpha; only the RGB is snapped (legacy default,
    /// preserves deliberate soft edges).
    Preserve,
    /// Binarise alpha at `cutoff`: a pixel with alpha ≥ cutoff becomes fully
    /// opaque and snaps to the palette; below cutoff it is cleared. Collapses a
    /// bloom/AA gradient into a single crisp on-palette silhouette.
    Opaque(u8),
    /// Composite each pixel over `bg` (straight-alpha source-over) and snap the
    /// resulting opaque colour — flattens soft FX onto a known backdrop colour.
    Flatten([u8; 4]),
}

/// New index of a layer after moving the element at `from` to `to` (the same
/// remove-then-insert the `Vec` does). Used to keep cel keys in step with
/// `move_layer`.
fn remap_move(old: usize, from: usize, to: usize) -> usize {
    if old == from {
        return to;
    }
    let mut i = if old > from { old - 1 } else { old };
    if i >= to {
        i += 1;
    }
    i
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
                boxes: Vec::new(),
            }],
            tags: Vec::new(),
            cels: Vec::new(),
            reference: None,
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

    // -- layer lifecycle ----------------------------------------------------
    //
    // The layer stack is a `Vec<LayerMeta>`; cels are keyed by `(layer, frame)`.
    // Any structural change to the stack therefore has to re-key the cel map in
    // lock-step. `remap_cel_layers` is the single choke point for that so the
    // two never drift apart.

    /// Rebuild the cel map under a layer-index remapping. `map(old)` gives the
    /// new layer index, or `None` to drop that layer's cels. Frames are kept.
    fn remap_cel_layers<F: Fn(usize) -> Option<usize>>(&mut self, map: F) {
        let old = std::mem::take(&mut self.cels);
        for ((l, f), v) in old {
            if let Some(nl) = map(l) {
                self.cels.insert((nl, f), v);
            }
        }
    }

    /// Move layer `from` to index `to` (clamped), shifting the rest; cels follow.
    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if from >= n {
            return Err(format!("no layer {} (layers={})", from, n));
        }
        let to = to.min(n - 1);
        if from == to {
            return Ok(());
        }
        let lm = self.meta.layers.remove(from);
        self.meta.layers.insert(to, lm);
        self.remap_cel_layers(|old| Some(remap_move(old, from, to)));
        Ok(())
    }

    /// Insert a new empty layer at `index` (clamped to the stack length),
    /// shifting existing layers (and their cels) up. Returns the new index.
    pub fn insert_layer(
        &mut self,
        index: usize,
        name: Option<String>,
        opacity: u8,
        blend: String,
    ) -> usize {
        let n = self.meta.layers.len();
        let index = index.min(n);
        self.meta.layers.insert(
            index,
            LayerMeta {
                name: name.unwrap_or_else(|| format!("Layer {}", n + 1)),
                opacity,
                visible: true,
                blend,
            },
        );
        self.remap_cel_layers(|old| Some(if old >= index { old + 1 } else { old }));
        index
    }

    /// Delete a layer and its cels (cannot remove the last remaining layer).
    pub fn delete_layer(&mut self, index: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        if n == 1 {
            return Err("cannot delete the only layer".into());
        }
        self.meta.layers.remove(index);
        self.remap_cel_layers(|old| match old.cmp(&index) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(old - 1),
            std::cmp::Ordering::Less => Some(old),
        });
        Ok(())
    }

    /// Rename a layer.
    pub fn rename_layer(&mut self, index: usize, name: String) -> Result<(), String> {
        let l = self
            .meta
            .layers
            .get_mut(index)
            .ok_or_else(|| format!("no layer {}", index))?;
        l.name = name;
        Ok(())
    }

    /// Duplicate a layer (meta + cels) directly above it. Returns the new index.
    pub fn duplicate_layer(&mut self, index: usize) -> Result<usize, String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        let mut lm = self.meta.layers[index].clone();
        lm.name = format!("{} copy", lm.name);
        let new_index = index + 1;
        self.meta.layers.insert(new_index, lm);
        // Shift cels at/above the insertion point up; the source (at `index <
        // new_index`) is untouched, so we can then copy it into `new_index`.
        self.remap_cel_layers(|old| Some(if old >= new_index { old + 1 } else { old }));
        let src: Vec<(usize, (i32, i32, RgbaImage))> = self
            .cels
            .iter()
            .filter(|((l, _), _)| *l == index)
            .map(|((_, f), v)| (*f, (v.0, v.1, v.2.clone())))
            .collect();
        for (f, v) in src {
            self.cels.insert((new_index, f), v);
        }
        Ok(new_index)
    }

    /// Merge layer `index` down onto the layer below it, baking in the upper
    /// layer's opacity and blend mode (per frame), then remove the upper layer.
    pub fn merge_down(&mut self, index: usize) -> Result<(), String> {
        let n = self.meta.layers.len();
        if index >= n {
            return Err(format!("no layer {} (layers={})", index, n));
        }
        if index == 0 {
            return Err("layer 0 has nothing below it to merge into".into());
        }
        let lower = index - 1;
        let upper = self.meta.layers[index].clone();
        let blend = raster::parse_blend(&upper.blend);
        for f in 0..self.meta.frames.len() {
            if !self.cels.contains_key(&(index, f)) {
                continue;
            }
            let upper_img = self.cel_full(index, f);
            let lower_canvas = self.cel_canvas(lower, f)?;
            raster::composite(lower_canvas, &upper_img, 0, 0, upper.opacity, blend);
        }
        self.delete_layer(index)
    }

    /// Append a new frame; with `copy_from`, duplicate that frame's cels into it.
    pub fn add_frame(&mut self, duration_ms: u32, copy_from: Option<usize>) -> usize {
        let idx = self.meta.frames.len();
        self.meta.frames.push(FrameMeta {
            duration_ms,
            pivot: None,
            boxes: Vec::new(),
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

    /// Replace a frame's collision boxes (empty vec clears them). Each box kind
    /// must be one of `BOX_KINDS`; an unknown kind fails fast with the list.
    pub fn set_frame_boxes(&mut self, frame: usize, boxes: Vec<BoxMeta>) -> Result<(), String> {
        if let Some(b) = boxes.iter().find(|b| !BOX_KINDS.contains(&b.kind.as_str())) {
            return Err(format!(
                "box kind must be one of {:?}, got '{}'",
                BOX_KINDS, b.kind
            ));
        }
        let f = self
            .meta
            .frames
            .get_mut(frame)
            .ok_or_else(|| format!("no frame {}", frame))?;
        f.boxes = boxes;
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

    /// Swap a set of colours across the whole document in one pass — the
    /// recolour-variant workflow (one sprite, many palettes). For every cel
    /// (filtered by optional `layer`/`frame`), every pixel matching a `from`
    /// exactly (all 4 channels) becomes its `to`; stored palette entries that
    /// match a `from` are updated too. Returns the count of pixels changed.
    pub fn palette_swap(
        &mut self,
        pairs: &[([u8; 4], [u8; 4])],
        layer: Option<usize>,
        frame: Option<usize>,
    ) -> u32 {
        let mut changed = 0;
        for ((l, f), (_x, _y, img)) in self.cels.iter_mut() {
            if layer.is_some_and(|sel| sel != *l) || frame.is_some_and(|sel| sel != *f) {
                continue;
            }
            for p in img.pixels_mut() {
                if let Some((_, to)) = pairs.iter().find(|(from, _)| p.0 == *from) {
                    *p = Rgba(*to);
                    changed += 1;
                }
            }
        }
        // Keep the stored palette consistent with the recolour.
        for c in self.meta.palette.iter_mut() {
            if let Some((_, to)) = pairs.iter().find(|(from, _)| *c == *from) {
                *c = *to;
            }
        }
        changed
    }

    /// Snap every opaque pixel to its perceptually nearest palette swatch
    /// (OKLab ΔE), preserving alpha. Scope to a `layer`/`frame` or the whole
    /// document. Kills the off-palette drift that blends/dithers/effects leave
    /// behind. Returns the number of pixels moved to a different colour.
    pub fn snap_to_palette(
        &mut self,
        palette: &[[u8; 4]],
        layer: Option<usize>,
        frame: Option<usize>,
        alpha: AlphaSnap,
    ) -> u32 {
        if palette.is_empty() {
            return 0;
        }
        let mut changed = 0;
        let mut lab = raster::PaletteLab::new(palette);
        for ((l, f), (_x, _y, img)) in self.cels.iter_mut() {
            if layer.is_some_and(|s| s != *l) || frame.is_some_and(|s| s != *f) {
                continue;
            }
            for p in img.pixels_mut() {
                let src = p.0;
                if src[3] == 0 {
                    continue;
                }
                // Decide the colour to snap and the alpha to keep, per policy.
                // `Opaque` collapses a continuous-tone FX bloom into a crisp
                // on-palette silhouette; `Flatten` melts it onto a known backdrop.
                let (rgb_in, out_alpha, clear) = match alpha {
                    AlphaSnap::Preserve => (src, src[3], false),
                    AlphaSnap::Opaque(cut) => (src, 255u8, src[3] < cut),
                    AlphaSnap::Flatten(bg) => (raster::over(bg, src), 255u8, false),
                };
                let new = if clear {
                    [0, 0, 0, 0]
                } else if let Some(i) = lab.nearest(rgb_in) {
                    let c = lab.color(i);
                    [c[0], c[1], c[2], out_alpha]
                } else {
                    src
                };
                if new != src {
                    *p = Rgba(new);
                    changed += 1;
                }
            }
        }
        changed
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
                "index": i, "duration_ms": f.duration_ms, "pivot": f.pivot,
                "boxes": f.boxes.iter().map(|b| b.to_json(1)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "tags": self.meta.tags.iter().map(|t| json!({
                "name": t.name, "from": t.from, "to": t.to, "direction": t.direction
            })).collect::<Vec<_>>(),
            "cels": cels,
            "palette": self.meta.palette,
            "palette_len": self.meta.palette.len(),
            "reference": self.meta.reference,
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
    /// at the offset. Source-over paste so transparent pixels in the moved block
    /// do NOT punch a rectangular hole through the art already at the
    /// destination (the limb-nudge footgun) — only opaque source pixels write.
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
        self.paste_region(layer, frame, ax + dx, ay + dy, rw, rh, &buf, true)
    }

    /// Affine-transform a cel (or a `region` of it) in place about its centre:
    /// rotate `rot` degrees, scale (`sx`,`sy`), shear (`skew_x`,`skew_y` deg).
    /// `method` `"rotsprite"` super-samples to keep clusters from shattering;
    /// `"nearest"` is the raw grid transform. `clear_source` empties the source
    /// rect first (a true move rather than an overlay). Returns
    /// `(placed_bbox [x,y,w,h], placed_opaque_px)`.
    #[allow(clippy::too_many_arguments)]
    pub fn transform_cel(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        rot: f32,
        sx: f32,
        sy: f32,
        skew_x: f32,
        skew_y: f32,
        method: &str,
        clear_source: bool,
    ) -> Result<([i32; 4], u32), String> {
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((x0, y0, x1, y1)) => raster::clamp_region(x0, y0, x1, y1, w, h)
                .ok_or("region is empty after clamping to the canvas")?,
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let (rw, rh) = ((bx - ax + 1) as u32, (by - ay + 1) as u32);
        // Lift the source rect to a standalone image.
        let full = self.cel_image(layer, frame)?;
        let sub = image::imageops::crop_imm(&full, ax as u32, ay as u32, rw, rh).to_image();
        let ss = if method == "rotsprite" { 4 } else { 1 };
        let out = raster::affine_nn(&sub, rot, sx, sy, skew_x, skew_y, ss);
        let (tw, th) = (out.width(), out.height());
        if clear_source {
            self.clear_region(layer, frame, ax, ay, bx, by)?;
        }
        // Centre the result on the source rect's centre.
        let cx = ax as f32 + rw as f32 / 2.0;
        let cy = ay as f32 + rh as f32 / 2.0;
        let px = (cx - tw as f32 / 2.0).round() as i32;
        let py = (cy - th as f32 / 2.0).round() as i32;
        let placed_px = out.pixels().filter(|p| p.0[3] > 0).count() as u32;
        let buf = out.into_raw();
        self.paste_region(layer, frame, px, py, tw, th, &buf, true)?;
        Ok(([px, py, tw as i32, th as i32], placed_px))
    }

    /// Flood a contiguous same-colour region from `(x,y)` into a row-major
    /// boolean mask the size of the canvas — the magic-wand. `layer` None reads
    /// the flattened composite. `perceptual` uses OKLab ΔE (`tol` as a 0..255
    /// scale) instead of raw channel distance. `conn8` uses 8-connectivity.
    pub fn flood_mask(
        &self,
        layer: Option<usize>,
        frame: usize,
        x: i32,
        y: i32,
        tol: i32,
        conn8: bool,
        perceptual: bool,
    ) -> Result<Vec<bool>, String> {
        let img = self.analysis_image(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut mask = vec![false; (w * h) as usize];
        if x < 0 || y < 0 || x >= w || y >= h {
            return Ok(mask);
        }
        let target = img.get_pixel(x as u32, y as u32).0;
        let de = tol as f32 / 255.0;
        let matches = |p: [u8; 4]| -> bool {
            // Never flood across the opaque/transparent boundary: the perceptual
            // ΔE ignores alpha, so without this a wand on a black fill would
            // bleed into a transparent black background (ΔE 0).
            if (p[3] == 0) != (target[3] == 0) {
                return false;
            }
            if perceptual {
                raster::oklab_delta(p, target) <= de
            } else {
                raster::close(p, target, tol)
            }
        };
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let i = (py * w + px) as usize;
            if mask[i] {
                continue;
            }
            if !matches(img.get_pixel(px as u32, py as u32).0) {
                continue;
            }
            mask[i] = true;
            stack.push((px + 1, py));
            stack.push((px - 1, py));
            stack.push((px, py + 1));
            stack.push((px, py - 1));
            if conn8 {
                stack.push((px + 1, py + 1));
                stack.push((px - 1, py - 1));
                stack.push((px + 1, py - 1));
                stack.push((px - 1, py + 1));
            }
        }
        Ok(mask)
    }

    /// Selective anti-aliasing (selout): soften the staircase corners of the
    /// silhouette by dropping one opaque, mid-value pixel into each outer step
    /// notch. The new pixel is the mean of the two edge colours that meet there
    /// (snapped to `ramp` if given, so AA stays on-palette). With `keep_square`
    /// a notch whose two opaque legs are both axis-aligned straight runs longer
    /// than `max_run` is left crisp (so deliberate right-angle corners survive),
    /// while diagonal staircases — whose perpendicular run is 1px — are always
    /// smoothed. `only_color` restricts to corners of that fill colour; `region`
    /// clips. Returns the number of AA pixels added.
    #[allow(clippy::too_many_arguments)]
    pub fn smooth_edges(
        &mut self,
        layer: usize,
        frame: usize,
        ramp: Option<&[[u8; 4]]>,
        max_run: i32,
        keep_square: bool,
        only_color: Option<[u8; 4]>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<u32, String> {
        self.check_cel(layer, frame)?;
        let src = self.cel_image(layer, frame)?;
        let (w, h) = (src.width() as i32, src.height() as i32);
        let (ax, ay, bx, by) = match region {
            Some((x0, y0, x1, y1)) => raster::clamp_region(x0, y0, x1, y1, w as u32, h as u32)
                .ok_or("region is empty after clamping to the canvas")?,
            None => (0, 0, w - 1, h - 1),
        };
        let op = |x: i32, y: i32| -> Option<[u8; 4]> {
            if x < 0 || y < 0 || x >= w || y >= h {
                return None;
            }
            let p = src.get_pixel(x as u32, y as u32).0;
            if p[3] == 0 {
                None
            } else {
                Some(p)
            }
        };
        // Length of the FLAT edge a leg sits on: walking along the leg's
        // direction, how many steps stay on the silhouette boundary (the next
        // pixel outward toward the notch is empty). A diagonal staircase breaks
        // this after 1px (it steps); a true straight wall runs long.
        let flat = |sx: i32, sy: i32, along: (i32, i32), out: (i32, i32)| -> i32 {
            let (mut x, mut y, mut n) = (sx, sy, 0);
            while op(x, y).is_some() && op(x + out.0, y + out.1).is_none() && n <= max_run + 1 {
                n += 1;
                x += along.0;
                y += along.1;
            }
            n
        };
        let mut adds: Vec<(i32, i32, [u8; 4])> = Vec::new();
        // Each outer corner is an empty pixel with exactly two perpendicular
        // opaque orthogonal neighbours; (out1/out2) point from each leg toward
        // the notch so `flat` can measure edge straightness.
        let corners = [
            ((0, -1), (1, 0), (0, 1), (-1, 0)), // N + E
            ((1, 0), (0, 1), (-1, 0), (0, -1)), // E + S
            ((0, 1), (-1, 0), (0, -1), (1, 0)), // S + W
            ((-1, 0), (0, -1), (1, 0), (0, 1)), // W + N
        ];
        for y in ay..=by {
            for x in ax..=bx {
                if op(x, y).is_some() {
                    continue; // only fill empty notches
                }
                // total opaque orthogonal neighbours must be exactly 2
                let north = op(x, y - 1);
                let south = op(x, y + 1);
                let east = op(x + 1, y);
                let west = op(x - 1, y);
                let count = [north, south, east, west]
                    .iter()
                    .filter(|n| n.is_some())
                    .count();
                if count != 2 {
                    continue;
                }
                for ((ax1, ay1), (ax2, ay2), out1, out2) in corners {
                    let (Some(c1), Some(c2)) = (op(x + ax1, y + ay1), op(x + ax2, y + ay2)) else {
                        continue;
                    };
                    if let Some(oc) = only_color {
                        if c1 != oc || c2 != oc {
                            continue;
                        }
                    }
                    // Preserve deliberate right angles: both edges run long & flat.
                    if keep_square {
                        let l1 = flat(x + ax1, y + ay1, (ax1, ay1), out1);
                        let l2 = flat(x + ax2, y + ay2, (ax2, ay2), out2);
                        if l1 > max_run && l2 > max_run {
                            continue;
                        }
                    }
                    let mean = [
                        ((c1[0] as u16 + c2[0] as u16) / 2) as u8,
                        ((c1[1] as u16 + c2[1] as u16) / 2) as u8,
                        ((c1[2] as u16 + c2[2] as u16) / 2) as u8,
                        255,
                    ];
                    let color = match ramp {
                        Some(r) if !r.is_empty() => {
                            let i = raster::nearest_oklab(mean, r).unwrap_or(0);
                            let c = r[i];
                            [c[0], c[1], c[2], 255]
                        }
                        _ => mean,
                    };
                    adds.push((x, y, color));
                    break;
                }
            }
        }
        let n = adds.len() as u32;
        let img = self.cel_canvas(layer, frame)?;
        for (x, y, c) in adds {
            raster::put(img, x, y, c);
        }
        Ok(n)
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
        // Visited mask, not a colour check: when the fill colour is itself
        // within `tol` of the target, painted pixels still match and the
        // scan loops forever without it.
        let mut visited = vec![false; (w * h) as usize];
        let mut stack = vec![(x, y)];
        while let Some((px, py)) = stack.pop() {
            if px < 0 || py < 0 || px >= w || py >= h {
                continue;
            }
            let i = (py * w + px) as usize;
            if visited[i] {
                continue;
            }
            visited[i] = true;
            let p = img.get_pixel(px as u32, py as u32).0;
            if !raster::close_rgb(p, target, tol) {
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
            // RGB max-channel match (alpha ignored) so AA/semi-transparent edges
            // of the target colour are recoloured too, not left as a halo.
            if raster::close_rgb(p.0, from, tol) {
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
    /// 4 = cubic. More than 4 is an error (the old behaviour silently dropped
    /// the extras — a curve that quietly ignored half its control points).
    /// Sampled into `steps` segments with brush `size`. Smooth organic
    /// strokes — tails, vines, hair.
    /// Draw a variable-width, anti-aliased stroke through `pts` (each
    /// `(x, y, full_width)`) as the union of round-capped capsules — the
    /// clean-by-construction stroke core. Connected (union, no gaps between
    /// samples like stacked beziers leave), tapered (per-vertex width — `0` ends
    /// give a 1px point), and smooth (analytic coverage, not a Bresenham
    /// staircase). A two-point call is a tapered capsule limb. `aa=false` keeps a
    /// crisp edge; with a palette locked and `snap`, the stroke's RGB is pulled
    /// on-palette (alpha preserved, so soft AA stays soft but on-palette).
    pub fn stroke(
        &mut self,
        layer: usize,
        frame: usize,
        pts: &[(i32, i32, i32)],
        color: [u8; 4],
        aa: bool,
        snap: bool,
    ) -> Result<(), String> {
        let f: Vec<(f32, f32, f32)> = pts
            .iter()
            .map(|&(x, y, w)| (x as f32, y as f32, w.max(0) as f32))
            .collect();
        self.stroke_f(layer, frame, &f, color, aa, snap)
    }

    /// Sub-pixel variant of [`Self::stroke`]: each point is `(x, y, full_width)`
    /// in continuous coordinates, fed straight to the coverage core with no
    /// integer round trip. Curves and poses sampled in `f32` (arcs, IK-solved
    /// limbs, eased motion) keep their sub-pixel precision instead of collapsing
    /// to whole pixels — the fix for choppy staircased curves. `stroke` is the
    /// integer-point wrapper over this.
    pub fn stroke_f(
        &mut self,
        layer: usize,
        frame: usize,
        pts: &[(f32, f32, f32)],
        color: [u8; 4],
        aa: bool,
        snap: bool,
    ) -> Result<(), String> {
        if pts.is_empty() {
            return Err("stroke needs at least one point".into());
        }
        let pal = self.meta.palette.clone();
        let f: Vec<(f32, f32, f32)> = pts
            .iter()
            .map(|&(x, y, w)| (x, y, w.max(0.0) / 2.0))
            .collect();
        let img = self.cel_canvas(layer, frame)?;
        raster::stroke_ribbon(img, &f, color, aa);
        if snap && !pal.is_empty() {
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
        }
        Ok(())
    }

    /// Stamp `text` left-to-right with the built-in 3×5 pixel font, top-left at
    /// (x,y). `size` is the integer pixel scale of each cell (so a glyph is
    /// 3·size wide, 5·size tall), clamped to 1..=64 to bound the inner loops.
    /// Glyphs are separated by one scaled pixel of spacing. Lowercase maps to
    /// uppercase; unknown chars render as a hollow box. Returns the rendered width
    /// in document pixels so callers can lay out the next line/element. HUD text,
    /// damage numbers, lettering.
    pub fn text(
        &mut self,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        text: &str,
        color: [u8; 4],
        size: i32,
    ) -> Result<i32, String> {
        let s = size.clamp(1, 64); // bound the s×s inner loops against huge tool input
        let advance = (raster::GLYPH_W + 1) * s; // glyph cell + 1px scaled spacing
        let img = self.cel_canvas(layer, frame)?;
        let mut pen = x;
        for ch in text.chars() {
            let bits = raster::glyph(ch);
            for gy in 0..raster::GLYPH_H {
                for gx in 0..raster::GLYPH_W {
                    // Left column is the high bit of each 3-bit row.
                    let bit = gy * raster::GLYPH_W + (raster::GLYPH_W - 1 - gx);
                    if (bits >> bit) & 1 == 0 {
                        continue;
                    }
                    // Scale the lit cell into an s×s block.
                    for dy in 0..s {
                        for dx in 0..s {
                            raster::put(img, pen + gx * s + dx, y + gy * s + dy, color);
                        }
                    }
                }
            }
            pen += advance;
        }
        // Trim the trailing inter-glyph spacing from the reported width.
        Ok(if text.is_empty() { 0 } else { pen - x - s })
    }

    /// Full-canvas (0,0-anchored) copy of a cel, transparent where absent.
    /// Also the before/after snapshot for the studio's mutation-diff acks.
    pub fn cel_full(&self, layer: usize, frame: usize) -> RgbaImage {
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

    /// Render a frame into analysis space: each opaque pixel becomes a grey level
    /// derived from `mode` (transparency preserved). "grayscale" = luma; "bands" =
    /// luma posterised into `bands` even steps; "saturation"/"hue" = that HSL
    /// channel scaled to 0..255 grey. The shared core behind doc_look's value modes.
    pub fn value_image(&self, frame: usize, mode: &str, bands: u32) -> Result<RgbaImage, String> {
        let src = self.analysis_image(None, frame)?;
        let bands = bands.max(1);
        let mut out = RgbaImage::from_pixel(src.width(), src.height(), Rgba([0, 0, 0, 0]));
        for (x, y, p) in src.enumerate_pixels() {
            let c = p.0;
            if c[3] == 0 {
                continue;
            }
            let g = match mode {
                "grayscale" => raster::luma(c),
                "bands" => {
                    // Posterise luma into `bands` even buckets, spread back to 0..255.
                    let l = raster::luma(c) as u32;
                    let b = (l * bands / 256).min(bands - 1);
                    if bands == 1 {
                        128
                    } else {
                        (b * 255 / (bands - 1)) as u8
                    }
                }
                "saturation" => (raster::saturation(c) * 255.0).round() as u8,
                "hue" => (raster::hue_deg(c) / 360.0 * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8,
                other => {
                    return Err(format!(
                        "unknown value mode '{}' — use grayscale|bands|saturation|hue",
                        other
                    ))
                }
            };
            out.put_pixel(x, y, Rgba([g, g, g, c[3]]));
        }
        Ok(out)
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

    /// Multi-light form shading. The silhouette's interior-distance field is
    /// read as a height map, differentiated into per-pixel surface normals
    /// (`bulge` sets how domed), then lit by Lambert diffuse from each light
    /// plus a Fresnel-style rim term — the leap from one-direction `form`
    /// shading to key/fill/rim "painted" form. Output multiplies the base
    /// colour by the accumulated light (so hue is preserved and light colour
    /// tints it); if `ramp` is given the lit value snaps to the ramp instead,
    /// keeping the result on-palette. Region defaults to the silhouette bbox.
    #[allow(clippy::too_many_arguments)]
    pub fn relight(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        lights: &[Light],
        ambient: f32,
        amb_color: [f32; 3],
        rim: f32,
        rim_color: [f32; 3],
        bulge: f32,
        ramp: Option<Vec<[u8; 4]>>,
    ) -> Result<(), String> {
        let (w, h) = (self.meta.w, self.meta.h);
        let before = self.cel_canvas(layer, frame)?.clone();
        // Foreground mask + height field over the whole canvas.
        let fg: Vec<bool> = (0..(w * h))
            .map(|i| before.as_raw()[i as usize * 4 + 3] > 0)
            .collect();
        // Smooth the height field before differentiating to normals, so the
        // medial-axis ridge doesn't read as facet creases (spheres go round).
        let dist = raster::blur_field(
            &raster::interior_distance(&fg, w as usize, h as usize),
            w as usize,
            h as usize,
            2,
        );
        let at = |x: i32, y: i32| -> f32 {
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                0.0
            } else {
                dist[(y as usize) * w as usize + x as usize]
            }
        };
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => match raster::clamp_region(a, b, c, d, w, h) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => {
                let (mut x0, mut y0, mut x1, mut y1) = (w as i32, h as i32, -1i32, -1i32);
                for (i, on) in fg.iter().enumerate() {
                    if *on {
                        let (x, y) = ((i % w as usize) as i32, (i / w as usize) as i32);
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x);
                        y1 = y1.max(y);
                    }
                }
                if x1 < 0 {
                    return Ok(());
                }
                (x0, y0, x1, y1)
            }
        };
        let bulge = bulge.max(0.2);
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let base = before.get_pixel(x as u32, y as u32).0;
                if base[3] == 0 {
                    continue;
                }
                // Surface normal from the height gradient (Sobel-ish).
                let gx = at(x + 1, y) - at(x - 1, y);
                let gy = at(x, y + 1) - at(x, y - 1);
                let nz = bulge;
                let nl = (gx * gx + gy * gy + nz * nz).sqrt().max(1e-6);
                let n = [-gx / nl, -gy / nl, nz / nl];
                // Accumulate diffuse light (per channel, so light colour tints).
                let mut acc = [
                    ambient * amb_color[0],
                    ambient * amb_color[1],
                    ambient * amb_color[2],
                ];
                for lt in lights {
                    let ll = (lt.dir[0].powi(2) + lt.dir[1].powi(2) + lt.dir[2].powi(2))
                        .sqrt()
                        .max(1e-6);
                    let ndl = ((n[0] * lt.dir[0] + n[1] * lt.dir[1] + n[2] * lt.dir[2]) / ll)
                        .max(0.0)
                        * lt.intensity;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += ndl * lt.color[c];
                    }
                }
                // Rim/Fresnel: bright where the surface turns away from the viewer.
                if rim > 0.0 {
                    let fres = (1.0 - n[2]).clamp(0.0, 1.0).powf(2.0) * rim;
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += fres * rim_color[c];
                    }
                }
                let lit = match &ramp {
                    Some(r) if !r.is_empty() => {
                        // Lit luminance picks the ramp step.
                        let f =
                            (acc[0] * 0.2126 + acc[1] * 0.7152 + acc[2] * 0.0722).clamp(0.0, 2.0);
                        let i = ((f / 2.0) * (r.len() as f32 - 1.0)).round() as usize;
                        let cc = r[i.min(r.len() - 1)];
                        [cc[0], cc[1], cc[2], base[3]]
                    }
                    _ => [
                        (base[0] as f32 * acc[0]).round().clamp(0.0, 255.0) as u8,
                        (base[1] as f32 * acc[1]).round().clamp(0.0, 255.0) as u8,
                        (base[2] as f32 * acc[2]).round().clamp(0.0, 255.0) as u8,
                        base[3],
                    ],
                };
                img.put_pixel(x as u32, y as u32, Rgba(lit));
            }
        }
        Ok(())
    }

    /// Graduated multi-tone dithering across a whole ramp along an axis — master
    /// gradient shading, vs the two-colour Bayer of `dither`. For each pixel the
    /// position `t` along `axis` (`h`|`v`|`radial`) picks a ramp pair; an ordered
    /// or blue-noise (`ign`) threshold dithers between them. `only_existing`
    /// repaints just the opaque pixels (shade existing art) and keeps their
    /// alpha. Returns the pixel count changed.
    #[allow(clippy::too_many_arguments)]
    pub fn dither_ramp(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        ramp: &[[u8; 4]],
        axis: &str,
        pattern: &str,
        only_existing: bool,
    ) -> Result<u32, String> {
        if ramp.len() < 2 {
            return Err("dither_ramp needs a ramp of >= 2 colours".into());
        }
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => raster::clamp_region(a, b, c, d, w, h)
                .ok_or("region is empty after clamping to the canvas")?,
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let span_x = (bx - ax).max(1) as f32;
        let span_y = (by - ay).max(1) as f32;
        let (cx, cy) = ((ax + bx) as f32 / 2.0, (ay + by) as f32 / 2.0);
        let rmax = ((span_x / 2.0).powi(2) + (span_y / 2.0).powi(2))
            .sqrt()
            .max(1.0);
        let last = ramp.len() - 1;
        let img = self.cel_canvas(layer, frame)?;
        let mut changed = 0;
        for y in ay..=by {
            for x in ax..=bx {
                let cur = img.get_pixel(x as u32, y as u32).0;
                if only_existing && cur[3] == 0 {
                    continue;
                }
                let t = match axis {
                    "v" => (y - ay) as f32 / span_y,
                    "radial" => ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / rmax,
                    _ => (x - ax) as f32 / span_x,
                }
                .clamp(0.0, 1.0);
                let pos = t * last as f32;
                let k = pos.floor() as usize;
                let frac = pos - k as f32;
                let thr = raster::ramp_dither_threshold(pattern, x, y);
                let idx = if frac > thr {
                    (k + 1).min(last)
                } else {
                    k.min(last)
                };
                let c = ramp[idx];
                let out = [c[0], c[1], c[2], if only_existing { cur[3] } else { 255 }];
                if out != cur {
                    img.put_pixel(x as u32, y as u32, Rgba(out));
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }

    /// Selective / form-following outline. Instead of a flat keyline, each
    /// silhouette edge pixel takes a colour derived from the fill it borders:
    /// `mode="from_fill"` darkens that fill (a coloured contour that turns with
    /// the form), `mode="light"`/`"dark"` biases the whole outline toward the
    /// light/shadow. `ramp` keeps the outline on-palette; `steps` is how far
    /// along the ramp/HSL to push. Returns the outline pixel count.
    pub fn outline_selective(
        &mut self,
        layer: usize,
        frame: usize,
        mode: &str,
        ramp: Option<&[[u8; 4]]>,
        steps: i32,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<u32, String> {
        self.check_cel(layer, frame)?;
        let src = self.cel_image(layer, frame)?;
        let (w, h) = (src.width() as i32, src.height() as i32);
        let (ax, ay, bx, by) = match region {
            Some((x0, y0, x1, y1)) => raster::clamp_region(x0, y0, x1, y1, w as u32, h as u32)
                .ok_or("region is empty after clamping to the canvas")?,
            None => (0, 0, w - 1, h - 1),
        };
        let op = |x: i32, y: i32| -> Option<[u8; 4]> {
            if x < 0 || y < 0 || x >= w || y >= h {
                return None;
            }
            let p = src.get_pixel(x as u32, y as u32).0;
            if p[3] == 0 {
                None
            } else {
                Some(p)
            }
        };
        // Darken/lighten a fill colour for the contour.
        let dir = if mode == "light" { 1 } else { -1 };
        let shade = |fill: [u8; 4]| -> [u8; 4] {
            match ramp {
                Some(r) if !r.is_empty() => raster::shade_ramp(fill, r, dir * steps.abs()),
                _ => raster::shade_hsl(fill, dir, steps.abs()),
            }
        };
        let mut adds: Vec<(i32, i32, [u8; 4])> = Vec::new();
        for y in ay..=by {
            for x in ax..=bx {
                if op(x, y).is_some() {
                    continue; // outline goes on the empty side of the edge
                }
                // nearest opaque orthogonal neighbour donates the fill colour
                let donor = [op(x, y - 1), op(x, y + 1), op(x + 1, y), op(x - 1, y)]
                    .into_iter()
                    .flatten()
                    .next();
                if let Some(fill) = donor {
                    let c = shade(fill);
                    adds.push((x, y, [c[0], c[1], c[2], 255]));
                }
            }
        }
        let n = adds.len() as u32;
        let img = self.cel_canvas(layer, frame)?;
        for (x, y, c) in adds {
            raster::put(img, x, y, c);
        }
        Ok(n)
    }

    /// Paint a procedural material onto the OPAQUE pixels of a cel (region-
    /// clipped) by mapping a per-pixel value field through `ramp`: `metal`
    /// (vertical falloff + specular band + base reflection), `wood` (directional
    /// grain), `stone` (cloud mottle + speckle), `water` (horizontal ripples),
    /// `cloth` (fine low-contrast weave), `skin` (soft vertical gradient),
    /// `glass` (vertical sheen + diagonal streak). Deterministic in `seed`. A
    /// light `ign` dither smooths the bands. Returns the pixel count painted.
    pub fn material(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        material: &str,
        ramp: &[[u8; 4]],
        seed: u64,
    ) -> Result<u32, String> {
        if ramp.len() < 2 {
            return Err("material needs a ramp of >= 2 colours".into());
        }
        self.check_cel(layer, frame)?;
        let (w, h) = (self.meta.w, self.meta.h);
        let (ax, ay, bx, by) = match region {
            Some((x0, y0, x1, y1)) => raster::clamp_region(x0, y0, x1, y1, w, h)
                .ok_or("region is empty after clamping to the canvas")?,
            None => (0, 0, w as i32 - 1, h as i32 - 1),
        };
        let (spanx, spany) = ((bx - ax).max(1) as f32, (by - ay).max(1) as f32);
        let n = ramp.len();
        let hash = |x: i32, y: i32| -> f32 {
            let mut h = (x as u32).wrapping_mul(374_761_393)
                ^ (y as u32).wrapping_mul(668_265_263)
                ^ (seed as u32).wrapping_mul(2_246_822_519);
            h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
            ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
        };
        // smooth value noise: bilinear over a coarse hash grid
        let vnoise = |fx: f32, fy: f32| -> f32 {
            let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
            let top = lerp(hash(x0, y0), hash(x0 + 1, y0), tx);
            let bot = lerp(hash(x0, y0 + 1), hash(x0 + 1, y0 + 1), tx);
            lerp(top, bot, ty)
        };
        let known = ["metal", "wood", "stone", "water", "cloth", "skin", "glass"];
        if !known.contains(&material) {
            return Err(format!("unknown material '{}' — use {:?}", material, known));
        }
        let img = self.cel_canvas(layer, frame)?;
        let mut painted = 0u32;
        for y in ay..=by {
            for x in ax..=bx {
                let cur = img.get_pixel(x as u32, y as u32).0;
                if cur[3] == 0 {
                    continue; // material clings to the existing shape
                }
                let (u, v) = ((x - ax) as f32 / spanx, (y - ay) as f32 / spany);
                let mut t = match material {
                    // dark top, bright specular band, mid body, faint floor bounce
                    "metal" => {
                        let spec = (-((v - 0.28).powi(2)) / 0.01).exp();
                        let bounce = (-((v - 0.92).powi(2)) / 0.02).exp() * 0.5;
                        (0.25 + 0.55 * v + spec + bounce).clamp(0.0, 1.0)
                    }
                    // directional grain: stretched noise along x
                    "wood" => {
                        let g = vnoise(x as f32 * 0.12, y as f32 * 0.9);
                        (0.5 + 0.45 * (x as f32 * 0.18 + g * 4.0).sin()).clamp(0.0, 1.0)
                    }
                    // cloud mottle + occasional bright/dark speckle
                    "stone" => {
                        let base = vnoise(x as f32 * 0.18, y as f32 * 0.18);
                        let sp = hash(x, y);
                        if sp > 0.97 {
                            0.95
                        } else if sp < 0.03 {
                            0.05
                        } else {
                            base.clamp(0.0, 1.0)
                        }
                    }
                    // horizontal ripples with a noisy phase
                    "water" => {
                        let ph = vnoise(x as f32 * 0.2, y as f32 * 0.05) * 3.0;
                        (0.5 + 0.45 * (y as f32 * 0.6 + ph + x as f32 * 0.08).sin()).clamp(0.0, 1.0)
                    }
                    // fine low-contrast weave
                    "cloth" => {
                        let weave = (((x + y) & 1) as f32 - 0.5) * 0.12;
                        (0.5 + weave + (vnoise(x as f32 * 0.4, y as f32 * 0.4) - 0.5) * 0.3)
                            .clamp(0.0, 1.0)
                    }
                    // soft vertical gradient, gentle mottle
                    "skin" => {
                        (0.35 + 0.4 * v + (vnoise(x as f32 * 0.25, y as f32 * 0.25) - 0.5) * 0.15)
                            .clamp(0.0, 1.0)
                    }
                    // vertical sheen + a diagonal highlight streak
                    _ => {
                        let streak = (-(((u - v) - 0.0).powi(2)) / 0.01).exp();
                        (0.3 + 0.5 * v + streak * 0.8).clamp(0.0, 1.0)
                    }
                };
                // light blue-noise dither to break the bands
                t = (t + (raster::ign(x, y) - 0.5) / n as f32).clamp(0.0, 1.0);
                let idx = (t * (n - 1) as f32).round() as usize;
                let c = ramp[idx.min(n - 1)];
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], cur[3]]));
                painted += 1;
            }
        }
        Ok(painted)
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

    /// Cast a projected ground shadow from a caster silhouette. Unlike
    /// `drop_shadow` (a flat offset copy), this lays the caster down onto the
    /// ground from its contact row and shears it AWAY from the light, so a tall
    /// shape throws a long foreshortened shadow stretching across the floor from
    /// its feet. `az_deg` is the light azimuth (0=right, 90=down, 180=left,
    /// 270=up — pairs with the vector `doc_form_audit` infers); `length`
    /// stretches the shadow along the ground, `squash` (0..1) is how far down the
    /// floor it reaches (0 = a flat smear at the contact row). With
    /// `receiver_layer` the shadow is painted onto that layer and clipped to its
    /// opaque pixels (it only lands on the ground), else it is drawn behind the
    /// caster on its own cel. Returns the shadow pixel count.
    #[allow(clippy::too_many_arguments)]
    pub fn cast_shadow(
        &mut self,
        layer: usize,
        frame: usize,
        az_deg: f32,
        length: f32,
        squash: f32,
        color: [u8; 4],
        opacity: u8,
        receiver_layer: Option<usize>,
        snap: bool,
    ) -> Result<u32, String> {
        let pal = self.meta.palette.clone();
        let caster = self.cel_canvas(layer, frame)?.clone();
        let (w, h) = (caster.width() as i32, caster.height() as i32);
        // Contact row: the lowest opaque pixel of the caster is where it meets
        // the ground and where the shadow is anchored.
        let mut anchor_y = None;
        for y in (0..h).rev() {
            if (0..w).any(|x| caster.get_pixel(x as u32, y as u32).0[3] > 0) {
                anchor_y = Some(y);
                break;
            }
        }
        let Some(anchor_y) = anchor_y else {
            return Ok(0); // nothing to cast
        };
        // Project each caster pixel onto the ground: shear along the light's
        // opposite horizontal, foreshorten by `squash`. Accumulate max coverage.
        let az = az_deg.to_radians();
        let shear = -az.cos() * length.max(0.0);
        let squash = squash.clamp(0.0, 1.0);
        let scale = (opacity as f32 / 255.0) * (color[3] as f32 / 255.0);
        let mut mask = vec![0f32; (w * h) as usize];
        for y in 0..=anchor_y {
            for x in 0..w {
                let a = caster.get_pixel(x as u32, y as u32).0[3];
                if a == 0 {
                    continue;
                }
                let hgt = (anchor_y - y) as f32;
                // Taller pixels project further along the ground (shear) and
                // further down the floor from the contact row (foreshortened).
                let sx = (x as f32 + shear * hgt).round() as i32;
                let sy = (anchor_y as f32 + hgt * squash).round() as i32;
                if sx < 0 || sy < 0 || sx >= w || sy >= h {
                    continue;
                }
                let cov = a as f32 / 255.0 * scale;
                let idx = (sy * w + sx) as usize;
                if cov > mask[idx] {
                    mask[idx] = cov;
                }
            }
        }
        let target = receiver_layer.unwrap_or(layer);
        let orig_target = self.cel_canvas(target, frame)?.clone();
        let mut out = orig_target.clone();
        let mut painted = 0u32;
        for y in 0..h {
            for x in 0..w {
                let idx = (y * w + x) as usize;
                let cov = mask[idx];
                if cov <= 0.0 {
                    continue;
                }
                // Clip to the receiver's opaque pixels (shadow lands on ground
                // only); with no receiver, don't paint over the caster itself.
                let target_a = orig_target.get_pixel(x as u32, y as u32).0[3];
                if receiver_layer.is_some() {
                    if target_a == 0 {
                        continue;
                    }
                } else if caster.get_pixel(x as u32, y as u32).0[3] > 0 {
                    continue;
                }
                let a = (cov * 255.0).round().clamp(0.0, 255.0) as u8;
                let base = out.get_pixel(x as u32, y as u32).0;
                let px = raster::over(base, [color[0], color[1], color[2], a]);
                out.put_pixel(x as u32, y as u32, Rgba(px));
                painted += 1;
            }
        }
        // With no receiver, the caster must sit ON TOP of its own shadow.
        if receiver_layer.is_none() {
            raster::composite(&mut out, &caster, 0, 0, 255, raster::Blend::Normal);
        }
        *self.cel_canvas(target, frame)? = out;
        if snap && !pal.is_empty() {
            self.snap_to_palette(&pal, Some(target), Some(frame), AlphaSnap::Preserve);
        }
        Ok(painted)
    }

    /// Paint a RIM light along the silhouette edges that FACE the light — the
    /// edge-relative move that was 100% manual (dump-region round-trips). For each
    /// opaque pixel near the edge it estimates the outward surface normal from the
    /// directions to nearby transparent pixels (radius `width`), and where that
    /// normal faces the light (`az_deg`: 0=right, 90=down, 180=left, 270=up) it
    /// stamps `color`, weighted by `falloff`. `dark=true` lights the AWAY-facing
    /// edge instead (core/contact shadow). Topological, so it survives small
    /// canvases where a Fresnel term washes out. Returns pixels painted.
    #[allow(clippy::too_many_arguments)]
    pub fn rim_light(
        &mut self,
        layer: usize,
        frame: usize,
        color: [u8; 4],
        az_deg: f32,
        width: i32,
        falloff: f32,
        dark: bool,
        snap: bool,
    ) -> Result<u32, String> {
        let pal = self.meta.palette.clone();
        let img = self.cel_canvas(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let orig = img.clone();
        let op = |x: i32, y: i32| {
            x >= 0 && y >= 0 && x < w && y < h && orig.get_pixel(x as u32, y as u32).0[3] > 0
        };
        let r = width.clamp(1, w.max(h).max(1));
        let az = az_deg.to_radians();
        let (lx, ly) = (az.cos(), az.sin());
        let sign = if dark { -1.0 } else { 1.0 };
        let mut changed = 0u32;
        for y in 0..h {
            for x in 0..w {
                if !op(x, y) {
                    continue;
                }
                // Outward normal ≈ distance-weighted sum of vectors toward nearby
                // EMPTY (in-canvas transparent) pixels. Out-of-bounds counts as
                // solid, so the canvas border is not mistaken for a silhouette edge.
                let (mut sx, mut sy) = (0f32, 0f32);
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let (nx, ny) = (x + dx, y + dy);
                        let empty = nx >= 0
                            && ny >= 0
                            && nx < w
                            && ny < h
                            && orig.get_pixel(nx as u32, ny as u32).0[3] == 0;
                        if !empty {
                            continue;
                        }
                        let d2 = (dx * dx + dy * dy) as f32; // weight ~ 1/len (closer empties dominate)
                        sx += dx as f32 / d2;
                        sy += dy as f32 / d2;
                    }
                }
                let mag = (sx * sx + sy * sy).sqrt();
                if mag < 1e-4 {
                    continue; // interior pixel, no edge nearby
                }
                let facing = sign * (sx / mag * lx + sy / mag * ly);
                if facing <= 0.0 {
                    continue;
                }
                // Composite by facing strength (a real falloff, never punches a
                // hole) instead of a hard binary stamp.
                let strength = facing.powf(falloff.max(0.1));
                if strength < 0.06 {
                    continue;
                }
                let base = orig.get_pixel(x as u32, y as u32).0;
                let a = ((color[3] as f32 / 255.0) * strength * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
                let px = raster::over(base, [color[0], color[1], color[2], a]);
                img.put_pixel(x as u32, y as u32, Rgba(px));
                changed += 1;
            }
        }
        if snap && !pal.is_empty() {
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
        }
        Ok(changed)
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
            // A region fully off-canvas is a silent no-op, not an error.
            Some((a, b, c, d)) => {
                match raster::clamp_region(a, b, c, d, self.meta.w, self.meta.h) {
                    Some(r) => r,
                    None => return Ok(()),
                }
            }
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
            // Shrinking: area-average so a hi-res reference keeps its features
            // (nearest keeps ~1 of every k² pixels). Growing: nearest stays
            // crisp for pixel art.
            src = if scale < 1.0 {
                raster::downscale_area(&src, nw, nh)
            } else {
                image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Nearest)
            };
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

    /// Volume shading: lay a rounded-form light gradient across the *interior*
    /// of a shape and snap it to a dark→light `ramp`. Where `shade` only lights
    /// silhouette rims, `form` fills the body — a flat-filled blob gains real
    /// volume in one call (the move that turns stacked ellipses into one lit
    /// sphere).
    ///
    /// `form`:
    ///   "sphere"      ball: radial surface-normal falloff in the region ellipse
    ///   "cylinder-h"  horizontal tube: rounds top↔bottom, flat left↔right
    ///   "cylinder-v"  vertical tube: rounds left↔right, flat top↔bottom
    ///   "auto"        any shape: interior-distance of the opaque silhouette
    ///                 (bright core → dark edge), nudged toward `light_dir`
    /// `light_dir` places the highlight (the 8 compass dirs `shade` uses).
    /// `region` sets the form's bounds/centre; omitted, it's the opaque bbox.
    /// `ramp` ordered dark→light; omitted, one is derived from the mean colour.
    /// `strength` 0..1 compresses (low) or spans the full ramp (1, default).
    #[allow(clippy::too_many_arguments)]
    pub fn form(
        &mut self,
        layer: usize,
        frame: usize,
        light_dir: &str,
        form: &str,
        region: Option<(i32, i32, i32, i32)>,
        ramp: Option<Vec<[u8; 4]>>,
        strength: f32,
    ) -> Result<(), String> {
        let (ldx, ldy) = match light_dir {
            "top-left" => (-1.0f32, -1.0f32),
            "top" => (0.0, -1.0),
            "top-right" => (1.0, -1.0),
            "left" => (-1.0, 0.0),
            "right" => (1.0, 0.0),
            "bottom-left" => (-1.0, 1.0),
            "bottom" => (0.0, 1.0),
            "bottom-right" => (1.0, 1.0),
            other => {
                return Err(format!(
                    "unknown light_dir '{}' — use top-left/top/top-right/left/right/bottom-left/bottom/bottom-right",
                    other
                ))
            }
        };
        if !matches!(form, "sphere" | "cylinder-h" | "cylinder-v" | "auto") {
            return Err(format!(
                "unknown form '{}' — use sphere/cylinder-h/cylinder-v/auto",
                form
            ));
        }
        let strength = strength.clamp(0.05, 1.0);
        let (w, h) = (self.meta.w, self.meta.h);

        // Snapshot so every read is pre-op; resolve the working region.
        let before = self.cel_canvas(layer, frame)?.clone();
        let (ax, ay, bx, by) = match region {
            Some((a, b, c, d)) => match raster::clamp_region(a, b, c, d, w, h) {
                Some(r) => r,
                None => return Ok(()),
            },
            None => {
                let (mut x0, mut y0, mut x1, mut y1) = (w as i32, h as i32, -1i32, -1i32);
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        if before.get_pixel(x as u32, y as u32).0[3] > 0 {
                            x0 = x0.min(x);
                            y0 = y0.min(y);
                            x1 = x1.max(x);
                            y1 = y1.max(y);
                        }
                    }
                }
                if x1 < 0 {
                    return Ok(()); // nothing opaque to shade
                }
                (x0, y0, x1, y1)
            }
        };

        // Ramp: explicit, else derived from the mean opaque colour in-region.
        let ramp = match ramp {
            Some(r) if !r.is_empty() => r,
            _ => {
                let (mut sr, mut sg, mut sb, mut cnt) = (0u64, 0u64, 0u64, 0u64);
                for y in ay..=by {
                    for x in ax..=bx {
                        let p = before.get_pixel(x as u32, y as u32).0;
                        if p[3] > 0 {
                            sr += p[0] as u64;
                            sg += p[1] as u64;
                            sb += p[2] as u64;
                            cnt += 1;
                        }
                    }
                }
                if cnt == 0 {
                    return Ok(());
                }
                let base = [(sr / cnt) as u8, (sg / cnt) as u8, (sb / cnt) as u8, 255];
                raster::make_ramp(base, 5, 12.0, 0.22, 0.18)
            }
        };
        let n = ramp.len() as f32;

        // Region ellipse centre + radii (sphere/cylinder geometry).
        let cx = (ax + bx) as f32 * 0.5;
        let cy = (ay + by) as f32 * 0.5;
        let rx = ((bx - ax) as f32 * 0.5).max(1.0);
        let ry = ((by - ay) as f32 * 0.5).max(1.0);

        // Light vector toward the key; z biased to the viewer for a soft front-key.
        let llen = (ldx * ldx + ldy * ldy + 0.36).sqrt();
        let (lx, ly, lz) = (ldx / llen, ldy / llen, 0.6 / llen);

        // "auto" needs the silhouette's interior distance over the region rect.
        let rw = (bx - ax + 1) as usize;
        let dt = if form == "auto" {
            let rh = (by - ay + 1) as usize;
            let mut fg = vec![false; rw * rh];
            for y in ay..=by {
                for x in ax..=bx {
                    if before.get_pixel(x as u32, y as u32).0[3] > 0 {
                        fg[(y - ay) as usize * rw + (x - ax) as usize] = true;
                    }
                }
            }
            Some(raster::interior_distance(&fg, rw, rh))
        } else {
            None
        };

        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = before.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let nx = (x as f32 - cx) / rx; // -1..1 across the region
                let ny = (y as f32 - cy) / ry;
                // intensity: 0 (shadow) .. 1 (lit)
                let intensity = match form {
                    "sphere" => {
                        let z = (1.0 - nx * nx - ny * ny).max(0.0).sqrt();
                        (nx * lx + ny * ly + z * lz) * 0.5 + 0.5
                    }
                    "cylinder-h" => {
                        let z = (1.0 - ny * ny).max(0.0).sqrt();
                        (ny * ly + z * lz) * 0.5 + 0.5
                    }
                    "cylinder-v" => {
                        let z = (1.0 - nx * nx).max(0.0).sqrt();
                        (nx * lx + z * lz) * 0.5 + 0.5
                    }
                    _ => {
                        // auto: bright core, dark edge, nudged toward the light.
                        let d = dt.as_ref().unwrap()[(y - ay) as usize * rw + (x - ax) as usize];
                        let bias = (-(nx * lx) - (ny * ly)) * 0.5 + 0.5;
                        (d * (0.55 + 0.45 * bias)).clamp(0.0, 1.0)
                    }
                };
                let t = ((intensity - 0.5) * strength + 0.5).clamp(0.0, 1.0);
                let i = (t * (n - 1.0)).round().clamp(0.0, n - 1.0) as usize;
                let c = ramp[i];
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], p[3]]));
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

    /// Remove L-corner doubles from 1px strokes (the "pixel-perfect" cleanup
    /// technique). A pixel P is erased when it matches the target colour(s), two
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
        // OKLab nearest, matching snap_to_palette — the old RGB squared
        // distance disagreed with the snap metric, so iterative quantize/snap
        // cycles flipped colours back and forth (and skin tones snapped grey).
        let mut lab = raster::PaletteLab::new(&pal);
        for p in img.pixels_mut() {
            if p.0[3] == 0 {
                continue;
            }
            if let Some(i) = lab.nearest(p.0) {
                let c = lab.color(i);
                *p = Rgba([c[0], c[1], c[2], p.0[3]]);
            }
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
        // Insert frame metadata, inheriting the source frame's pivot/boxes so
        // in-betweens don't arrive with no anchor and no collision data.
        let src_meta = self.meta.frames[from].clone();
        for i in 0..steps {
            self.meta.frames.insert(
                insert_at + i,
                FrameMeta {
                    duration_ms,
                    pivot: src_meta.pivot,
                    boxes: src_meta.boxes.clone(),
                },
            );
        }
        // Keep tag ranges pointing at the frames they tagged (a tag spanning
        // the insertion stretches over the new in-betweens).
        for t in &mut self.meta.tags {
            if t.from >= insert_at {
                t.from += steps;
            }
            if t.to >= insert_at {
                t.to += steps;
            }
        }
        // Build cross-fade cels; with a locked palette, snap each blend so the
        // dissolve can't mint off-palette colours.
        let mut lab = raster::PaletteLab::new(&self.meta.palette);
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
                        // Alpha-weighted (premultiplied) RGB blend: transparent
                        // pixels are stored [0,0,0,0], and a plain lerp lets
                        // that black bleed into fringes — which the palette
                        // snap then quantizes into wrong-hue colours.
                        let alpha = (pa[3] as f32 + (pb[3] as f32 - pa[3] as f32) * t)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        let (wa, wb) = (pa[3] as f32 * (1.0 - t), pb[3] as f32 * t);
                        let mixed = if wa + wb > 0.0 {
                            let ch = |i: usize| {
                                ((pa[i] as f32 * wa + pb[i] as f32 * wb) / (wa + wb))
                                    .round()
                                    .clamp(0.0, 255.0) as u8
                            };
                            [ch(0), ch(1), ch(2), alpha]
                        } else {
                            [0, 0, 0, 0]
                        };
                        let px = match (mixed[3] > 0, lab.nearest(mixed)) {
                            (true, Some(pi)) => {
                                let c = lab.color(pi);
                                [c[0], c[1], c[2], mixed[3]]
                            }
                            _ => mixed,
                        };
                        img.put_pixel(x, y, Rgba(px));
                    }
                }
                self.cels.insert((l, fidx), (0, 0, img));
            }
        }
        Ok(steps)
    }

    /// Timeline lifecycle: `delete` | `insert` | `duplicate` | `move`. Cels
    /// reindex and tag ranges remap with the frames; a tag covering only a
    /// deleted frame is dropped. The last remaining frame can't be deleted —
    /// this is the recovery path for a bad tween or duplicated pose.
    pub fn frame_ops(
        &mut self,
        action: &str,
        frame: usize,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        let n = self.meta.frames.len();
        match action {
            "delete" => {
                if frame >= n {
                    return Err(format!("no frame {} (frames={})", frame, n));
                }
                if n == 1 {
                    return Err("cannot delete the last remaining frame".into());
                }
                self.meta.frames.remove(frame);
                self.cels.retain(|k, _| k.1 != frame);
                let keys: Vec<(usize, usize)> =
                    self.cels.keys().filter(|k| k.1 > frame).cloned().collect();
                let mut moved = Vec::new();
                for k in keys {
                    let v = self.cels.remove(&k).unwrap();
                    moved.push(((k.0, k.1 - 1), v));
                }
                self.cels.extend(moved);
                self.meta
                    .tags
                    .retain(|t| !(t.from == frame && t.to == frame));
                for t in &mut self.meta.tags {
                    if t.from > frame {
                        t.from -= 1;
                    }
                    if t.to >= frame {
                        t.to -= 1;
                    }
                }
            }
            "insert" => {
                if frame > n {
                    return Err(format!(
                        "insert index {} out of range (frames={})",
                        frame, n
                    ));
                }
                self.meta.frames.insert(
                    frame,
                    FrameMeta {
                        duration_ms: duration_ms.unwrap_or(100),
                        pivot: None,
                        boxes: Vec::new(),
                    },
                );
                let keys: Vec<(usize, usize)> =
                    self.cels.keys().filter(|k| k.1 >= frame).cloned().collect();
                let mut moved = Vec::new();
                for k in keys {
                    let v = self.cels.remove(&k).unwrap();
                    moved.push(((k.0, k.1 + 1), v));
                }
                self.cels.extend(moved);
                for t in &mut self.meta.tags {
                    if t.from >= frame {
                        t.from += 1;
                    }
                    if t.to >= frame {
                        t.to += 1;
                    }
                }
            }
            "duplicate" => {
                if frame >= n {
                    return Err(format!("no frame {} (frames={})", frame, n));
                }
                let meta = self.meta.frames[frame].clone();
                self.meta.frames.insert(frame + 1, meta);
                let keys: Vec<(usize, usize)> =
                    self.cels.keys().filter(|k| k.1 > frame).cloned().collect();
                let mut moved = Vec::new();
                for k in keys {
                    let v = self.cels.remove(&k).unwrap();
                    moved.push(((k.0, k.1 + 1), v));
                }
                self.cels.extend(moved);
                let to_copy: Vec<(usize, (i32, i32, RgbaImage))> = self
                    .cels
                    .iter()
                    .filter(|((_, f), _)| *f == frame)
                    .map(|((l, _), v)| (*l, (v.0, v.1, v.2.clone())))
                    .collect();
                for (l, v) in to_copy {
                    self.cels.insert((l, frame + 1), v);
                }
                // A tag ending on the duplicated frame grows to cover the copy.
                for t in &mut self.meta.tags {
                    if t.from > frame {
                        t.from += 1;
                    }
                    if t.to >= frame {
                        t.to += 1;
                    }
                }
            }
            "move" => {
                let to = to_index.ok_or("move needs `to_index`")?;
                if frame >= n || to >= n {
                    return Err(format!(
                        "move {}->{} out of range (frames={})",
                        frame, to, n
                    ));
                }
                if frame != to {
                    let mut order: Vec<usize> = (0..n).filter(|&i| i != frame).collect();
                    order.insert(to, frame);
                    let old_frames = std::mem::take(&mut self.meta.frames);
                    self.meta.frames = order.iter().map(|&o| old_frames[o].clone()).collect();
                    let mut map = vec![0usize; n];
                    for (newi, &old) in order.iter().enumerate() {
                        map[old] = newi;
                    }
                    let all: Vec<_> = self.cels.drain().collect();
                    self.cels = all
                        .into_iter()
                        .map(|((l, f), v)| ((l, map[f]), v))
                        .collect();
                    // Tag remap = remove-then-insert, NOT endpoint min/max
                    // through the permutation (that balloons a tag over
                    // untagged frames when a member moves far away, and drops
                    // a member on an in-tag reorder). A single-frame tag on
                    // the moved frame simply follows it.
                    for t in &mut self.meta.tags {
                        if t.from == frame && t.to == frame {
                            t.from = to;
                            t.to = to;
                            continue;
                        }
                        let was_inside = t.from <= frame && frame <= t.to;
                        // Phase 1: the moved frame leaves its old index.
                        let mut f0 = t.from - usize::from(t.from > frame);
                        let mut t0 = t.to - usize::from(t.to >= frame);
                        // Phase 2: it re-enters at final index `to`.
                        if f0 >= to {
                            f0 += 1;
                        }
                        if t0 >= to {
                            t0 += 1;
                        }
                        // A member landing back inside-or-adjacent rejoins its
                        // tag; one that moved far away leaves it (a tag must
                        // stay a contiguous range of TAGGED frames).
                        if was_inside && to <= t0 + 1 && to + 1 >= f0 {
                            f0 = f0.min(to);
                            t0 = t0.max(to);
                        }
                        t.from = f0;
                        t.to = t0;
                    }
                }
            }
            other => {
                return Err(format!(
                    "unknown frame action '{}' — use delete|insert|duplicate|move",
                    other
                ))
            }
        }
        Ok(json!({"ok": true, "action": action, "frames": self.meta.frames.len()}))
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
    /// the span shaped by `easing` (forwarded verbatim to `raster::ease` —
    /// linear/ease-in/ease-out/ease-in-out/bounce/overshoot/elastic; the
    /// non-monotone curves can push past the full offset before settling). With
    /// `clear_source` the ORIGINAL region rect
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

    /// Paint a whole region declaratively from a character grid — the inverse
    /// of dump_region. Each row string is one pixel row starting at (ox,oy);
    /// every character must be in `legend` ('.'/' ' skip, leaving the pixel
    /// untouched). LLMs emit a sprite as a grid far more reliably than as a
    /// sequence of absolute-coordinate draw calls — this removes the
    /// coordinate-math failure class. Returns `(painted, clipped)`.
    pub fn paint_grid(
        &mut self,
        layer: usize,
        frame: usize,
        ox: i32,
        oy: i32,
        legend: &std::collections::HashMap<char, [u8; 4]>,
        rows: &[String],
    ) -> Result<(u64, u64), String> {
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let img = self.cel_canvas(layer, frame)?;
        let (mut painted, mut clipped) = (0u64, 0u64);
        for (ry, row) in rows.iter().enumerate() {
            for (rx, ch) in row.chars().enumerate() {
                if ch == '.' || ch == ' ' {
                    continue;
                }
                let color = legend.get(&ch).ok_or_else(|| {
                    format!(
                        "row {} col {}: character '{}' is not in the legend",
                        ry, rx, ch
                    )
                })?;
                let (tx, ty) = (ox + rx as i32, oy + ry as i32);
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    clipped += 1;
                    continue;
                }
                img.put_pixel(tx as u32, ty as u32, Rgba(*color));
                painted += 1;
            }
        }
        Ok((painted, clipped))
    }

    /// Cut a region (and/or selection-masked pixels) of `layer` onto its OWN
    /// new layer directly above, same coordinates — converts a flat sprite
    /// into part layers (arm, head, tail) that keyframe_transform can move
    /// independently. `all_frames` cuts every frame's cel, keeping the part
    /// aligned across the timeline. Returns `(new_layer_index, pixels_moved)`.
    pub fn extract_to_layer(
        &mut self,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        mask: Option<&[bool]>,
        name: Option<String>,
        all_frames: bool,
    ) -> Result<(usize, u64), String> {
        if layer >= self.meta.layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if region.is_none() && mask.is_none() {
            return Err("extract needs a `region` or an active selection".into());
        }
        if !all_frames && frame >= self.meta.frames.len() {
            return Err(format!("no frame {}", frame));
        }
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let norm = region.map(|(x0, y0, x1, y1)| (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1)));
        let in_scope = |x: i32, y: i32| -> bool {
            if let Some((ax, ay, bx, by)) = norm {
                if x < ax || x > bx || y < ay || y > by {
                    return false;
                }
            }
            match mask {
                Some(m) => m.get((y * w + x) as usize).copied() == Some(true),
                None => true,
            }
        };
        let new_layer = layer + 1;
        self.insert_layer(
            new_layer,
            name.or_else(|| Some("part".into())),
            255,
            "normal".into(),
        );
        let frames: Vec<usize> = if all_frames {
            (0..self.meta.frames.len()).collect()
        } else {
            vec![frame]
        };
        let mut moved_total = 0u64;
        for f in frames {
            if !self.cels.contains_key(&(layer, f)) {
                continue;
            }
            let full = self.cel_full(layer, f);
            let mut part = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
            let mut rest = full.clone();
            let mut moved = 0u64;
            for y in 0..h {
                for x in 0..w {
                    let p = *full.get_pixel(x as u32, y as u32);
                    if p.0[3] > 0 && in_scope(x, y) {
                        part.put_pixel(x as u32, y as u32, p);
                        rest.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 0]));
                        moved += 1;
                    }
                }
            }
            if moved > 0 {
                self.set_cel(layer, f, 0, 0, rest)?;
                self.set_cel(new_layer, f, 0, 0, part)?;
                moved_total += moved;
            }
        }
        Ok((new_layer, moved_total))
    }

    /// Eased rotation + translation of a region about an arbitrary PIVOT
    /// across a frame range on one layer — the joint primitive: "swing the arm
    /// 30° about the shoulder over frames 1..4" in one call instead of four
    /// blind repaints. Reads the region's content from `from_frame`; each
    /// later frame gets the part rotated by the eased angle and offset by the
    /// eased (dx,dy) about `pivot` (document coords), with the original region
    /// cleared first so no stale copy remains. `snap` re-snaps the resampled
    /// pixels to the locked palette.
    #[allow(clippy::too_many_arguments)]
    pub fn keyframe_transform(
        &mut self,
        layer: usize,
        region: (i32, i32, i32, i32),
        pivot: (f32, f32),
        from_frame: usize,
        to_frame: usize,
        rot_deg: f32,
        dx: i32,
        dy: i32,
        easing: &str,
        snap: bool,
    ) -> Result<Vec<Value>, String> {
        if to_frame <= from_frame {
            return Err("keyframe_transform needs to_frame > from_frame".into());
        }
        let n = self.meta.frames.len();
        if to_frame >= n {
            return Err(format!(
                "frame {} does not exist (frames={}) — add it with doc_add_frame first",
                to_frame, n
            ));
        }
        let (x0, y0, x1, y1) = (
            region.0.min(region.2),
            region.1.min(region.3),
            region.0.max(region.2),
            region.1.max(region.3),
        );
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (rx0, ry0) = (x0.max(0), y0.max(0));
        let (rx1, ry1) = (x1.min(w - 1), y1.min(h - 1));
        if rx0 > rx1 || ry0 > ry1 {
            return Err("region is empty after clamping to the canvas".into());
        }
        let (rw, rh) = ((rx1 - rx0 + 1) as u32, (ry1 - ry0 + 1) as u32);
        let full = self.cel_full(layer, from_frame);
        let mut part = RgbaImage::from_pixel(rw, rh, Rgba([0, 0, 0, 0]));
        for y in 0..rh {
            for x in 0..rw {
                part.put_pixel(x, y, *full.get_pixel(rx0 as u32 + x, ry0 as u32 + y));
            }
        }
        let mut lab = raster::PaletteLab::new(&self.meta.palette);
        let span = (to_frame - from_frame) as f32;
        let mut placed = Vec::new();
        for f in (from_frame + 1)..=to_frame {
            let t = raster::ease((f - from_frame) as f32 / span, easing);
            let theta = rot_deg * t;
            let (ox, oy) = (
                (dx as f32 * t).round() as i32,
                (dy as f32 * t).round() as i32,
            );
            let rotated = raster::affine_nn(&part, theta, 1.0, 1.0, 0.0, 0.0, 2);
            // affine_nn rotates about the part's centre and returns a
            // bbox-sized image; place it so the JOINT pivot stays fixed:
            // c' = pivot + R(c - pivot), then the eased translation.
            let (cx, cy) = (rx0 as f32 + rw as f32 / 2.0, ry0 as f32 + rh as f32 / 2.0);
            let r = theta.to_radians();
            let (cos, sin) = (r.cos(), r.sin());
            let (vx, vy) = (cx - pivot.0, cy - pivot.1);
            let ncx = pivot.0 + cos * vx - sin * vy;
            let ncy = pivot.1 + sin * vx + cos * vy;
            let px = (ncx + ox as f32 - rotated.width() as f32 / 2.0).round() as i32;
            let py = (ncy + oy as f32 - rotated.height() as f32 / 2.0).round() as i32;
            self.clear_region(layer, f, rx0, ry0, rx1, ry1)?;
            let img = self.cel_canvas(layer, f)?;
            for (sx, sy, p) in rotated.enumerate_pixels() {
                if p.0[3] == 0 {
                    continue;
                }
                let (tx, ty) = (px + sx as i32, py + sy as i32);
                if tx < 0 || ty < 0 || tx >= w || ty >= h {
                    continue;
                }
                let c = match (snap, lab.nearest(p.0)) {
                    (true, Some(pi)) => {
                        let pc = lab.color(pi);
                        Rgba([pc[0], pc[1], pc[2], p.0[3]])
                    }
                    _ => *p,
                };
                img.put_pixel(tx as u32, ty as u32, c);
            }
            placed.push(json!({
                "frame": f,
                "rot_deg": (theta * 10.0).round() / 10.0,
                "offset": [ox, oy],
                "placed_at": [px, py],
            }));
        }
        Ok(placed)
    }

    /// Render the horizontal spritesheet image (every frame side by side,
    /// nearest-neighbour scaled). Returns `(sheet, frame_w, frame_h)`.
    fn sheet_image(&self, scale: u32) -> (RgbaImage, u32, u32) {
        let n = self.meta.frames.len() as u32;
        let fw = self.meta.w * scale;
        let fh = self.meta.h * scale;
        let mut sheet = RgbaImage::from_pixel(fw * n, fh, Rgba([0, 0, 0, 0]));
        for f in 0..self.meta.frames.len() {
            let mut img = self.flatten(f);
            if scale > 1 {
                img = image::imageops::resize(&img, fw, fh, image::imageops::FilterType::Nearest);
            }
            image::imageops::replace(&mut sheet, &img, (f as u32 * fw) as i64, 0);
        }
        (sheet, fw, fh)
    }

    pub fn export_sheet(&self, out: &Path, scale: u32) -> Result<Value, String> {
        let n = self.meta.frames.len() as u32;
        let (sheet, fw, fh) = self.sheet_image(scale);
        sheet.save(out).map_err(|e| e.to_string())?;
        let frames: Vec<Value> = self
            .meta
            .frames
            .iter()
            .enumerate()
            .map(|(i, fr)| {
                json!({
                    "rect": [i as u32 * fw, 0, fw, fh],
                    "duration_ms": fr.duration_ms,
                    // Pivot scaled into sheet pixels (engine-ready); null if unset.
                    "pivot": fr.pivot.map(|[px, py]| [px as u32 * scale, py as u32 * scale]),
                    // Collision boxes scaled into sheet pixels (gameplay metadata).
                    "boxes": fr.boxes.iter().map(|b| b.to_json(scale)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let tags: Vec<Value> = self
            .meta
            .tags
            .iter()
            .map(|t| json!({"name": t.name, "from": t.from, "to": t.to, "direction": t.direction}))
            .collect();
        let meta = json!({
            "path": out.to_string_lossy(), "frame_w": fw, "frame_h": fh,
            "count": n, "frames": frames, "tags": tags, "palette": self.meta.palette,
        });
        let mp = out.with_extension("json");
        std::fs::write(&mp, serde_json::to_string_pretty(&meta).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(meta)
    }

    /// Export the spritesheet with the industry-standard hash sprite-JSON
    /// sidecar instead of atelier's richer native shape — the layout that game
    /// engines' existing sheet importers already parse (`frames` keyed by name
    /// with `frame`/`sourceSize`/`duration`, `meta.frameTags`). Pivots and
    /// collision boxes have no slot in this shape; use the native JSON
    /// (`export_sheet`) when the engine should read those.
    pub fn export_sheet_std(&self, out: &Path, scale: u32) -> Result<Value, String> {
        let (sheet, fw, fh) = self.sheet_image(scale);
        sheet.save(out).map_err(|e| e.to_string())?;
        let stem = out
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.meta.name.clone());
        let image_name = out
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("{stem}.png"));
        let mut frames = serde_json::Map::new();
        for (i, fr) in self.meta.frames.iter().enumerate() {
            frames.insert(
                format!("{stem} {i}"),
                json!({
                    "frame": {"x": i as u32 * fw, "y": 0, "w": fw, "h": fh},
                    "rotated": false,
                    "trimmed": false,
                    "spriteSourceSize": {"x": 0, "y": 0, "w": fw, "h": fh},
                    "sourceSize": {"w": fw, "h": fh},
                    "duration": fr.duration_ms,
                }),
            );
        }
        let frame_tags: Vec<Value> = self
            .meta
            .tags
            .iter()
            .map(|t| json!({"name": t.name, "from": t.from, "to": t.to, "direction": t.direction}))
            .collect();
        let meta = json!({
            "frames": frames,
            "meta": {
                "app": "atelier",
                "version": env!("CARGO_PKG_VERSION"),
                "image": image_name,
                "format": "RGBA8888",
                "size": {"w": sheet.width(), "h": sheet.height()},
                "scale": "1",
                "frameTags": frame_tags,
            },
        });
        let mp = out.with_extension("json");
        std::fs::write(&mp, serde_json::to_string_pretty(&meta).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(json!({
            "path": out.to_string_lossy(),
            "meta_path": mp.to_string_lossy(),
            "meta_format": "standard",
            "count": self.meta.frames.len(),
            "frame_w": fw,
            "frame_h": fh,
        }))
    }

    /// Export an animated GIF. Plays `tag`'s sequence (honouring direction) or
    /// the whole timeline when `tag` is None. Returns the number of frames
    /// actually emitted (a pingpong tag emits more than its source range).
    pub fn export_gif(&self, out: &Path, scale: u32, tag: Option<&str>) -> Result<usize, String> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let seq = self.play_sequence(tag)?;
        // Build ONE palette across every frame and snap each frame to it before
        // encoding. The image crate quantizes each frame independently, so on
        // multi-colour art it picks a different 256-subset per frame — the
        // source of GIF inter-frame flicker. A shared palette (the locked one,
        // else a median-cut over all frames) makes the colours identical
        // frame-to-frame, so motion is the only thing that changes.
        let global: Vec<[u8; 4]> = if !self.meta.palette.is_empty() {
            self.meta.palette.clone()
        } else {
            let mut px: Vec<[u8; 3]> = Vec::new();
            for &f in &seq {
                for p in self.flatten(f).pixels() {
                    if p.0[3] > 0 {
                        px.push([p.0[0], p.0[1], p.0[2]]);
                    }
                }
            }
            if px.is_empty() {
                Vec::new()
            } else {
                raster::median_cut(&px, 256)
            }
        };
        let mut lab = raster::PaletteLab::new(&global);
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut enc = GifEncoder::new(std::io::BufWriter::new(file));
        enc.set_repeat(Repeat::Infinite)
            .map_err(|e| e.to_string())?;
        for &f in &seq {
            let mut img = self.flatten(f);
            if !global.is_empty() {
                for p in img.pixels_mut() {
                    if p.0[3] > 0 {
                        if let Some(i) = lab.nearest(p.0) {
                            let c = lab.color(i);
                            *p = Rgba([c[0], c[1], c[2], p.0[3]]);
                        }
                    }
                }
            }
            if scale > 1 {
                img = image::imageops::resize(
                    &img,
                    self.meta.w * scale,
                    self.meta.h * scale,
                    image::imageops::FilterType::Nearest,
                );
            }
            let delay = Delay::from_numer_denom_ms(self.meta.frames[f].duration_ms, 1);
            enc.encode_frame(Frame::from_parts(img, 0, 0, delay))
                .map_err(|e| e.to_string())?;
        }
        Ok(seq.len())
    }

    /// Export an animated PNG (APNG) — the lossless, full-alpha sibling of
    /// `export_gif` (GIF is 256 colours with 1-bit alpha). Plays `tag`'s sequence
    /// (honouring direction) or the whole timeline when `tag` is None;
    /// nearest-neighbour `scale`; per-frame delay is `duration_ms` (in 1/1000s, or
    /// 1/100s for delays beyond 65s). Returns the number of frames emitted (a
    /// pingpong tag emits more than its range).
    pub fn export_apng(&self, out: &Path, scale: u32, tag: Option<&str>) -> Result<usize, String> {
        let seq = self.play_sequence(tag)?;
        if seq.is_empty() {
            return Err("nothing to export: document has no frames".into());
        }
        let sc = scale.max(1);
        let (w, h) = (self.meta.w * sc, self.meta.h * sc);
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_animated(seq.len() as u32, 0)
            .map_err(|e| e.to_string())?;
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        for &f in &seq {
            let mut img = self.flatten(f);
            if sc > 1 {
                img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Nearest);
            }
            // PNG frame delay is a u16/u16 rational; pick a denominator that fits:
            // ms/1000 while it fits in u16, else centiseconds/100 (capped at u16).
            let ms = self.meta.frames[f].duration_ms;
            let (num, den) = if ms <= u16::MAX as u32 {
                (ms as u16, 1000)
            } else {
                ((ms / 10).min(u16::MAX as u32) as u16, 100)
            };
            writer
                .set_frame_delay(num, den)
                .map_err(|e| e.to_string())?;
            writer
                .write_image_data(img.as_raw())
                .map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
        Ok(seq.len())
    }

    /// GRADIENT MAP: remap every opaque pixel's LUMINANCE through colour
    /// `stops` (0 = darkest, 1 = lightest), preserving alpha — the one-call
    /// mood/recolour move (sunset-ify, poison-ify, night-palette a sprite)
    /// that keeps all the drawn shading structure and swaps only its colour
    /// story. Region-scopable.
    pub fn gradient_map(
        &mut self,
        layer: usize,
        frame: usize,
        mut stops: Vec<(f32, [u8; 4])>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<(), String> {
        if stops.is_empty() {
            return Err("gradient_map needs at least one colour stop".into());
        }
        stops.iter_mut().for_each(|s| s.0 = s.0.clamp(0.0, 1.0));
        stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let (w, h) = (self.meta.w as i32, self.meta.h as i32);
        let (ax, ay, bx, by) = match region {
            Some((x0, y0, x1, y1)) => (
                x0.min(x1).max(0),
                y0.min(y1).max(0),
                x0.max(x1).min(w - 1),
                y0.max(y1).min(h - 1),
            ),
            None => (0, 0, w - 1, h - 1),
        };
        let img = self.cel_canvas(layer, frame)?;
        for y in ay..=by {
            for x in ax..=bx {
                let p = img.get_pixel(x as u32, y as u32).0;
                if p[3] == 0 {
                    continue;
                }
                let t = raster::luma(p) as f32 / 255.0;
                let c = raster::sample_gradient(&stops, t, "none", x, y, 0);
                img.put_pixel(x as u32, y as u32, Rgba([c[0], c[1], c[2], p[3]]));
            }
        }
        Ok(())
    }

    /// Apply one batched drawing op described by a JSON object `{"op": "...", ...}`.
    /// Lets a headless client send many ordered edits in a single tool call.
    ///
    /// Optional per-op `"opacity"` (0..255) and `"blend_mode"` (a layer blend
    /// name) composite the op's result instead of overwriting: the op is run,
    /// then the pixels it changed are re-composited over the pre-op cel with the
    /// given opacity/mode (snapshot-diff, so any op gains blend/opacity).
    pub fn apply_op(&mut self, layer: usize, frame: usize, op: &Value) -> Result<(), String> {
        let opacity = op.get("opacity").and_then(|v| v.as_u64()).map(|v| v as u8);
        let mode = op
            .get("blend_mode")
            .and_then(|v| v.as_str())
            .map(raster::parse_blend);
        if opacity.is_none() && mode.is_none() {
            return self.apply_op_raw(layer, frame, op);
        }
        let before = self.cel_canvas(layer, frame)?.clone();
        self.apply_op_raw(layer, frame, op)?;
        let of = opacity.unwrap_or(255) as f32 / 255.0;
        let m = mode.unwrap_or(raster::Blend::Normal);
        let img = self.cel_canvas(layer, frame)?;
        for y in 0..img.height() {
            for x in 0..img.width() {
                let after = img.get_pixel(x, y).0;
                let base = before.get_pixel(x, y).0;
                if after != base {
                    img.put_pixel(x, y, Rgba(raster::composite_px(base, after, of, m)));
                }
            }
        }
        Ok(())
    }

    /// The op-name dispatch table. MUST stay in lockstep with `batch_op_keys`:
    /// every op name handled here must have an entry there (and vice versa) — the
    /// two are hand-synced, so adding/renaming an op means editing both.
    fn apply_op_raw(&mut self, layer: usize, frame: usize, op: &Value) -> Result<(), String> {
        let name = op
            .get("op")
            .and_then(|v| v.as_str())
            .ok_or("batch op missing 'op'")?;
        let gi = |k: &str, d: i64| op.get(k).and_then(|v| v.as_i64()).unwrap_or(d) as i32;
        let gb = |k: &str, d: bool| op.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
        let col = |k: &str| rgba_val(op.get(k));
        let outcome = match name {
            "pencil" => self.pencil(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gi("size", 1),
            ),
            "line" => self.line(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                col("color"),
                gi("size", 1),
            ),
            "rect" => self.rect(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                col("color"),
                gb("fill", false),
                gi("size", 1),
            ),
            "ellipse" => self.ellipse(
                layer,
                frame,
                gi("cx", 0),
                gi("cy", 0),
                gi("rx", 1),
                gi("ry", 1),
                col("color"),
                gb("fill", false),
            ),
            "polyline" => self.polyline(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gi("size", 1),
                gb("closed", false),
            ),
            "polygon" => self.polygon(
                layer,
                frame,
                &points_val(op.get("points")),
                col("color"),
                gb("fill", true),
            ),
            "stroke" => {
                let w = op.get("width").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
                self.stroke_f(
                    layer,
                    frame,
                    &points3f_val(op.get("points"), w),
                    col("color"),
                    gb("aa", true),
                    gb("snap", true),
                )
            }
            "fill" | "bucket" => self.bucket_fill(
                layer,
                frame,
                gi("x", 0),
                gi("y", 0),
                col("color"),
                gi("tolerance", 0),
            ),
            "replace_color" => {
                self.replace_color(layer, frame, col("from"), col("to"), gi("tolerance", 0))
            }
            "flip" => self.flip(layer, frame, gb("horizontal", true)),
            "shift" => self.shift(layer, frame, gi("dx", 0), gi("dy", 0), gb("wrap", false)),
            "blur" => self.blur(layer, frame, gi("radius", 1), region_val(op.get("region"))),
            "quantize" => self
                .quantize(
                    layer,
                    frame,
                    colors_val(op.get("colors")),
                    op.get("max_colors").and_then(|v| v.as_u64()).unwrap_or(16) as usize,
                )
                .map(|_| ()),
            "outline" => self.outline_cel(layer, frame, col("color"), gb("aa", false)),
            "drop_shadow" => self.drop_shadow(
                layer,
                frame,
                gi("dx", 1),
                gi("dy", 1),
                col("color"),
                op.get("opacity").and_then(|v| v.as_u64()).unwrap_or(160) as u8,
                gi("blur", 0),
            ),
            "glow" => self.glow(
                layer,
                frame,
                op.get("color").map(|_| col("color")),
                gi("radius", 2),
                op.get("intensity").and_then(|v| v.as_u64()).unwrap_or(180) as u8,
                op.get("mode").and_then(|v| v.as_str()).unwrap_or("screen"),
            ),
            "bevel" => self.bevel(layer, frame, col("light"), col("dark"), gi("depth", 1)),
            "fill_cel" => self.fill_cel(layer, frame, col("color")),
            "clear_cel" => {
                self.clear_cel(layer, frame);
                Ok(())
            }
            "gradient" => {
                self.gradient(
                    layer,
                    frame,
                    op.get("kind").and_then(|v| v.as_str()).unwrap_or("linear"),
                    gi("x0", 0),
                    gi("y0", 0),
                    gi("x1", 0),
                    gi("y1", 0),
                    stops_val(op.get("stops")),
                    op.get("dither").and_then(|v| v.as_str()).unwrap_or("none"),
                    op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                    region_val(op.get("region")),
                    gb("blend", true),
                )?;
                // Parity with the standalone doc_gradient: re-snap on-palette by
                // default when a palette is locked (the batch path used to skip it).
                if gb("snap", true) && !self.meta.palette.is_empty() {
                    let pal = self.meta.palette.clone();
                    self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
                }
                Ok(())
            }
            "scatter" => self.scatter(
                layer,
                frame,
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                &colors_val(op.get("colors")),
                op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.1) as f32,
                op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                gi("size", 1),
            ),
            "symmetry" => self.symmetry(
                layer,
                frame,
                op.get("vertical")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                op.get("horizontal")
                    .and_then(|v| v.as_i64())
                    .map(|v| v as i32),
                gb("keep_left", true),
                gb("keep_top", true),
            ),
            "adjust" => self.adjust(
                layer,
                frame,
                region_val(op.get("region")),
                op.get("hue").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                op.get("sat").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                op.get("lum").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
            ),
            "noise" => self.noise(
                layer,
                frame,
                op.get("kind").and_then(|v| v.as_str()).unwrap_or("cloud"),
                gi("x0", 0),
                gi("y0", 0),
                gi("x1", 0),
                gi("y1", 0),
                op.get("scale").and_then(|v| v.as_f64()).unwrap_or(8.0) as f32,
                op.get("octaves").and_then(|v| v.as_u64()).unwrap_or(4) as u32,
                op.get("seed").and_then(|v| v.as_u64()).unwrap_or(0),
                stops_val(op.get("stops")),
                gb("blend", false),
            ),
            "shade" => self.shade(
                layer,
                frame,
                op.get("light_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("top-left"),
                gi("steps", 1),
                region_val(op.get("region")),
                op.get("mode").and_then(|v| v.as_str()).unwrap_or("both"),
                op.get("ramp").map(|v| colors_val(Some(v))),
            ),
            "form" => self.form(
                layer,
                frame,
                op.get("light_dir")
                    .and_then(|v| v.as_str())
                    .unwrap_or("top-left"),
                op.get("form").and_then(|v| v.as_str()).unwrap_or("sphere"),
                region_val(op.get("region")),
                op.get("ramp").map(|v| colors_val(Some(v))),
                op.get("strength").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32,
            ),
            "dither" => self.dither(
                layer,
                frame,
                // In a batch the studio applies any active selection as a mask,
                // so default to the whole canvas when no explicit region.
                region_val(op.get("region")).unwrap_or((
                    0,
                    0,
                    self.meta.w as i32 - 1,
                    self.meta.h as i32 - 1,
                )),
                col("color_a"),
                col("color_b"),
                op.get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("bayer4"),
                op.get("density").and_then(|v| v.as_f64()).unwrap_or(0.5) as f32,
                gb("only_existing", false),
            ),
            "gradient_map" => self.gradient_map(
                layer,
                frame,
                stops_val(op.get("stops")),
                region_val(op.get("region")),
            ),
            "pixel_perfect" => self
                .pixel_perfect(
                    layer,
                    frame,
                    region_val(op.get("region")),
                    op.get("color").map(|_| col("color")),
                )
                .map(|_| ()),
            "text" => self
                .text(
                    layer,
                    frame,
                    gi("x", 0),
                    gi("y", 0),
                    op.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                    col("color"),
                    gi("size", 1),
                )
                .map(|_| ()),
            other => Err(format!("unknown batch op '{}'", other)),
        };
        outcome?;
        // Continuous-tone FX push a locked palette into hundreds of colours; re-snap
        // onto it by default (parity with gradient/glow) so the result stays crisp
        // pixel art. Opt out per op with `snap:false`. Hard-edged / already-on-palette
        // ops (outline, dither, pixel_perfect, quantize, adjust) are excluded.
        if matches!(name, "blur" | "drop_shadow" | "bevel" | "form" | "shade")
            && gb("snap", true)
            && !self.meta.palette.is_empty()
        {
            let pal = self.meta.palette.clone();
            self.snap_to_palette(&pal, Some(layer), Some(frame), AlphaSnap::Preserve);
        }
        Ok(())
    }

    // -- animation & tiling feedback (read-only diff/seam primitives) --------

    /// Per-pixel diff of two frames over an (already clamped) region. `layer`
    /// None diffs the flattened composite, else that cel. Classifies each pixel
    /// as unchanged / added (transparent→opaque) / removed (opaque→transparent)
    /// or recoloured (opaque→different opaque), tallying counts and the bbox of
    /// every changed pixel. Returns `(added, removed, recolored, change_bbox,
    /// img_a, img_b)` so callers can also build a text grid or overlay render.
    pub fn frame_diff_region(
        &self,
        frame_a: usize,
        frame_b: usize,
        layer: Option<usize>,
        region: (i32, i32, i32, i32),
    ) -> Result<FrameDiff, String> {
        let a = self.analysis_image(layer, frame_a)?;
        let b = self.analysis_image(layer, frame_b)?;
        let (x0, y0, x1, y1) = region;
        let (mut added, mut removed, mut recolored) = (0u32, 0u32, 0u32);
        let mut bbox: Option<[i32; 4]> = None;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let pa = a.get_pixel(x as u32, y as u32).0;
                let pb = b.get_pixel(x as u32, y as u32).0;
                if pa == pb {
                    continue;
                }
                match (pa[3] > 0, pb[3] > 0) {
                    (false, true) => added += 1,
                    (true, false) => removed += 1,
                    _ => recolored += 1, // both opaque (different) — recolour
                }
                bbox = Some(match bbox {
                    Some([ax, ay, bx, by]) => [ax.min(x), ay.min(y), bx.max(x), by.max(y)],
                    None => [x, y, x, y],
                });
            }
        }
        Ok((added, removed, recolored, bbox, a, b))
    }

    /// Wrap-test one tiling axis: compare the far edge against the near edge that
    /// would abut it when the cel repeats. `horizontal` true tests column x=w-1
    /// vs x=0 (left/right tiling), false tests row y=h-1 vs y=0 (top/bottom).
    /// `threshold` is the max per-channel delta still counted as a match. Returns
    /// `(mismatches, max_delta, worst)` where `worst` is up to 10 `[x,y,delta]`
    /// edge cells sorted by descending delta (the position is the far-edge cell).
    pub fn seam_axis(
        &self,
        layer: Option<usize>,
        frame: usize,
        horizontal: bool,
        threshold: i32,
    ) -> Result<(u32, i32, Vec<[i32; 3]>), String> {
        let img = self.analysis_image(layer, frame)?;
        let (w, h) = (img.width() as i32, img.height() as i32);
        let mut mismatches = 0u32;
        let mut max_delta = 0i32;
        let mut all: Vec<[i32; 3]> = Vec::new();
        // The pairs of (far-edge, near-edge) cells along the seam.
        let pairs: Vec<((i32, i32), (i32, i32))> = if horizontal {
            (0..h).map(|y| ((w - 1, y), (0, y))).collect()
        } else {
            (0..w).map(|x| ((x, h - 1), (x, 0))).collect()
        };
        for ((fx, fy), (nx, ny)) in pairs {
            let pf = img.get_pixel(fx as u32, fy as u32).0;
            let pn = img.get_pixel(nx as u32, ny as u32).0;
            let delta = (0..4)
                .map(|c| (pf[c] as i32 - pn[c] as i32).abs())
                .max()
                .unwrap();
            if delta > threshold {
                mismatches += 1;
                max_delta = max_delta.max(delta);
                all.push([fx, fy, delta]);
            }
        }
        all.sort_by(|a, b| b[2].cmp(&a[2]));
        all.truncate(10);
        Ok((mismatches, max_delta, all))
    }

    /// The silhouette bbox-centre (mean of the bbox corners) of a frame's opaque
    /// pixels, or None when the frame is empty. `layer` None flattens. Used by
    /// the spacing audit to track motion frame-to-frame.
    /// Opaque-mass CENTROID of a frame's silhouette (mean of opaque pixel
    /// coordinates), optionally clipped to `region`. A mass centroid, unlike
    /// the old bbox-corner midpoint, actually moves when a limb swings over a
    /// static torso — and the region clip makes one part's motion measurable
    /// on its own.
    pub fn silhouette_center(
        &self,
        layer: Option<usize>,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Option<[f64; 2]>, String> {
        let img = self.analysis_image(layer, frame)?;
        let (mut sx, mut sy, mut n) = (0f64, 0f64, 0u64);
        for (x, y, p) in img.enumerate_pixels() {
            if p.0[3] == 0 {
                continue;
            }
            if let Some((x0, y0, x1, y1)) = region {
                let (xi, yi) = (x as i32, y as i32);
                if xi < x0.min(x1) || xi > x0.max(x1) || yi < y0.min(y1) || yi > y0.max(y1) {
                    continue;
                }
            }
            sx += x as f64;
            sy += y as f64;
            n += 1;
        }
        Ok((n > 0).then(|| [sx / n as f64, sy / n as f64]))
    }

    /// Count opaque pixels in a frame (denominator for the seam loop score).
    pub fn opaque_count(&self, layer: Option<usize>, frame: usize) -> Result<u64, String> {
        let img = self.analysis_image(layer, frame)?;
        Ok(img.pixels().filter(|p| p.0[3] > 0).count() as u64)
    }
}

// -- batch-op JSON parsing helpers ------------------------------------------

fn rgba_val(v: Option<&Value>) -> [u8; 4] {
    if let Some(a) = v.and_then(|x| x.as_array()) {
        let g = |i: usize, d: u8| {
            a.get(i)
                .and_then(|n| n.as_u64())
                .map(|n| n as u8)
                .unwrap_or(d)
        };
        return [
            g(0, 0),
            g(1, 0),
            g(2, 0),
            a.get(3)
                .and_then(|n| n.as_u64())
                .map(|n| n as u8)
                .unwrap_or(255),
        ];
    }
    [0, 0, 0, 255]
}

/// Parse `[[x,y], ...]` or `[[x,y,width], ...]` into stroke vertices, filling
/// the missing width with `default_w`. Points shorter than 2 are dropped.
fn points3f_val(v: Option<&Value>, default_w: f32) -> Vec<(f32, f32, f32)> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_array())
                .filter(|pt| pt.len() >= 2)
                .map(|pt| {
                    // Parse as f64 so fractional stroke points survive to the
                    // sub-pixel coverage core instead of truncating to integers.
                    let g = |i: usize, d: f32| {
                        pt.get(i)
                            .and_then(|n| n.as_f64())
                            .map(|n| n as f32)
                            .unwrap_or(d)
                    };
                    (g(0, 0.0), g(1, 0.0), g(2, default_w))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn points_val(v: Option<&Value>) -> Vec<(i32, i32)> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| p.as_array())
                .map(|pt| {
                    let g = |i: usize| pt.get(i).and_then(|n| n.as_i64()).unwrap_or(0) as i32;
                    (g(0), g(1))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse gradient stops `[{"pos":0.0,"color":[r,g,b,a]}, ...]`.
fn stops_val(v: Option<&Value>) -> Vec<(f32, [u8; 4])> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|s| {
                    (
                        s.get("pos").and_then(|n| n.as_f64()).unwrap_or(0.0) as f32,
                        rgba_val(s.get("color")),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a colour list `[[r,g,b,a], ...]` (for scatter).
fn colors_val(v: Option<&Value>) -> Vec<[u8; 4]> {
    v.and_then(|x| x.as_array())
        .map(|a| a.iter().map(|c| rgba_val(Some(c))).collect())
        .unwrap_or_default()
}

/// Parse an optional `[x0,y0,x1,y1]` clip region.
fn region_val(v: Option<&Value>) -> Option<(i32, i32, i32, i32)> {
    let a = v.and_then(|x| x.as_array()).filter(|a| a.len() >= 4)?;
    let g = |i: usize| a[i].as_i64().unwrap_or(0) as i32;
    Some((g(0), g(1), g(2), g(3)))
}

/// The recognized keys for a batch op kind: `(required, optional)`. Every op
/// additionally accepts the compositing wrapper keys `opacity`/`blend_mode`
/// (and the discriminator `op`). Returns None for an unknown op kind.
///
/// MUST stay in lockstep with `apply_op_raw`'s match: every op name listed here
/// must be dispatched there (and vice versa) — the two tables are hand-synced.
fn batch_op_keys(kind: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match kind {
        "pencil" => (&["points", "color"], &["size"]),
        "line" => (&["x0", "y0", "x1", "y1", "color"], &["size"]),
        "rect" => (&["x0", "y0", "x1", "y1", "color"], &["fill", "size"]),
        "ellipse" => (&["cx", "cy", "rx", "ry", "color"], &["fill"]),
        "polyline" => (&["points", "color"], &["size", "closed"]),
        "polygon" => (&["points", "color"], &["fill"]),
        "stroke" => (&["points", "color"], &["width", "aa", "snap"]),
        "fill" | "bucket" => (&["x", "y", "color"], &["tolerance"]),
        "replace_color" => (&["from", "to"], &["tolerance"]),
        "flip" => (&[], &["horizontal"]),
        "shift" => (&[], &["dx", "dy", "wrap"]),
        "blur" => (&["radius"], &["region", "snap"]),
        "quantize" => (&["colors"], &["max_colors"]),
        "outline" => (&["color"], &["aa"]),
        "drop_shadow" => (&["color"], &["dx", "dy", "blur", "snap"]),
        "glow" => (&[], &["color", "radius", "intensity", "mode"]),
        "bevel" => (&["light", "dark"], &["depth", "snap"]),
        "fill_cel" => (&["color"], &[]),
        "clear_cel" => (&[], &[]),
        "gradient" => (
            &["stops"],
            &[
                "kind", "x0", "y0", "x1", "y1", "dither", "seed", "region", "blend", "snap",
            ],
        ),
        "scatter" => (
            &["colors", "x0", "y0", "x1", "y1"],
            &["density", "seed", "size"],
        ),
        "symmetry" => (&[], &["vertical", "horizontal", "keep_left", "keep_top"]),
        "adjust" => (&[], &["region", "hue", "sat", "lum"]),
        "gradient_map" => (&["stops"], &["region"]),
        "noise" => (
            &["stops", "x0", "y0", "x1", "y1"],
            &["kind", "scale", "octaves", "seed", "blend"],
        ),
        "shade" => (
            &[],
            &["light_dir", "steps", "region", "mode", "ramp", "snap"],
        ),
        "form" => (
            &[],
            &["form", "light_dir", "region", "ramp", "strength", "snap"],
        ),
        "dither" => (
            &["color_a", "color_b"],
            &["region", "pattern", "density", "only_existing"],
        ),
        "pixel_perfect" => (&[], &["region", "color"]),
        "text" => (&["x", "y", "text", "color"], &["size"]),
        _ => return None,
    })
}

/// Strictly validate one batch op object before it runs: the `op` key must name
/// a known kind, every required key must be present, and no unrecognized keys
/// may appear (typos / wrong-shape params would otherwise be silently defaulted).
/// `idx` is the op's position in the batch, used only for the error message.
pub fn validate_batch_op(idx: usize, op: &Value) -> Result<(), String> {
    let obj = op
        .as_object()
        .ok_or_else(|| format!("op[{}]: each op must be a JSON object", idx))?;
    let kind = obj
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("op[{}]: missing 'op' key naming the op kind", idx))?;
    let (required, optional) =
        batch_op_keys(kind).ok_or_else(|| format!("op[{}]: unknown op '{}'", idx, kind))?;
    // Keys every op accepts on top of its own params.
    let common = ["op", "opacity", "blend_mode"];
    let known = |k: &str| common.contains(&k) || required.contains(&k) || optional.contains(&k);
    let bad: Vec<&str> = obj
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !known(k))
        .collect();
    if !bad.is_empty() {
        let mut allowed: Vec<&str> = required.iter().chain(optional.iter()).copied().collect();
        return Err(format!(
            "op[{}] ({}): unknown keys {} — {} takes {}",
            idx,
            kind,
            bad.join(","),
            kind,
            if allowed.is_empty() {
                "(no params)".to_string()
            } else {
                allowed.sort_unstable();
                allowed.join(",")
            }
        ));
    }
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "op[{}] ({}): missing required keys {}",
            idx,
            kind,
            missing.join(",")
        ));
    }
    Ok(())
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
    fn extract_to_layer_cuts_part_onto_new_layer() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], [1, 1, 1, 255], 1).unwrap();
        d.pencil(0, 0, &[(3, 3)], [2, 2, 2, 255], 1).unwrap();
        let (new_layer, moved) = d
            .extract_to_layer(0, 0, Some((2, 2, 3, 3)), None, Some("arm".into()), false)
            .unwrap();
        assert_eq!((new_layer, moved), (1, 1));
        assert_eq!(d.meta.layers[1].name, "arm");
        // The part moved: gone from the source, present on the new layer.
        assert_eq!(d.get_pixel(0, 0, 3, 3).unwrap(), [0, 0, 0, 0]);
        assert_eq!(d.get_pixel(1, 0, 3, 3).unwrap(), [2, 2, 2, 255]);
        // Untouched pixels stayed on the source layer.
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [1, 1, 1, 255]);
    }

    #[test]
    fn keyframe_transform_translates_and_rotates_about_pivot() {
        let mut d = Document::new("t", 8, 8);
        d.pencil(0, 0, &[(1, 1)], [9, 9, 9, 255], 1).unwrap();
        d.add_frame(100, Some(0));
        d.add_frame(100, Some(0));
        // Pure translation: behaves like keyframe_move.
        d.keyframe_transform(
            0,
            (1, 1, 1, 1),
            (1.5, 1.5),
            0,
            2,
            0.0,
            4,
            0,
            "linear",
            false,
        )
        .unwrap();
        assert_eq!(d.get_pixel(0, 1, 3, 1).unwrap(), [9, 9, 9, 255]); // halfway
        assert_eq!(d.get_pixel(0, 2, 5, 1).unwrap(), [9, 9, 9, 255]); // full offset
        assert_eq!(d.get_pixel(0, 2, 1, 1).unwrap(), [0, 0, 0, 0]); // source cleared
                                                                    // Rotation: a bar swung 90° about its base ends up horizontal.
        let mut r = Document::new("r", 9, 9);
        for y in 1..=4 {
            r.pencil(0, 0, &[(4, y)], [9, 9, 9, 255], 1).unwrap();
        }
        r.add_frame(100, Some(0));
        let placed = r
            .keyframe_transform(
                0,
                (4, 1, 4, 4),
                (4.5, 4.5),
                0,
                1,
                90.0,
                0,
                0,
                "linear",
                false,
            )
            .unwrap();
        assert_eq!(placed.len(), 1);
        // The vertical bar above the pivot is gone in frame 1...
        assert_eq!(r.get_pixel(0, 1, 4, 1).unwrap(), [0, 0, 0, 0]);
        // ...and opaque pixels now sit to the LEFT of the pivot (90° clockwise
        // takes "up" to "left" in screen coords... or right for cw): just
        // assert the mass moved off the original column onto one row.
        let count_row4: usize = (0..9)
            .filter(|&x| r.get_pixel(0, 1, x, 4).unwrap()[3] > 0)
            .count();
        assert!(count_row4 >= 3, "bar should now lie along row 4");
    }

    #[test]
    fn tween_inherits_meta_remaps_tags_and_snaps_palette() {
        let mut d = Document::new("t", 2, 2);
        d.set_palette(vec![[0, 0, 0, 255], [255, 255, 255, 255]]);
        d.pencil(0, 0, &[(0, 0)], [0, 0, 0, 255], 1).unwrap();
        d.add_frame(100, None);
        d.pencil(0, 1, &[(0, 0)], [255, 255, 255, 255], 1).unwrap();
        d.set_pivot(0, Some([1, 1])).unwrap();
        d.add_tag("walk", 0, 1, "forward").unwrap();
        d.tween(0, 1, 1, 50).unwrap();
        // Tag stretched over the inserted in-between.
        assert_eq!((d.meta.tags[0].from, d.meta.tags[0].to), (0, 2));
        // The in-between inherited the source frame's pivot.
        assert_eq!(d.meta.frames[1].pivot, Some([1, 1]));
        // The 50% grey blend snapped to a palette colour instead of minting one.
        let px = d.get_pixel(0, 1, 0, 0).unwrap();
        assert!(
            d.meta.palette.iter().any(|c| c[..3] == px[..3]),
            "blend {:?} should be on-palette",
            px
        );
    }

    #[test]
    fn frame_ops_delete_reindexes_and_protects_last() {
        let mut d = Document::new("t", 2, 2);
        d.pencil(0, 0, &[(0, 0)], [1, 1, 1, 255], 1).unwrap();
        d.add_frame(100, None);
        d.pencil(0, 1, &[(0, 0)], [2, 2, 2, 255], 1).unwrap();
        d.add_frame(100, None);
        d.pencil(0, 2, &[(0, 0)], [3, 3, 3, 255], 1).unwrap();
        d.add_tag("mid", 1, 1, "forward").unwrap();
        d.add_tag("all", 0, 2, "forward").unwrap();
        d.frame_ops("delete", 1, None, None).unwrap();
        assert_eq!(d.meta.frames.len(), 2);
        // Frame 2's cel slid down to index 1.
        assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [3, 3, 3, 255]);
        // The tag covering only the deleted frame is gone; the spanning one shrank.
        assert_eq!(d.meta.tags.len(), 1);
        assert_eq!((d.meta.tags[0].from, d.meta.tags[0].to), (0, 1));
        d.frame_ops("delete", 1, None, None).unwrap();
        assert!(d.frame_ops("delete", 0, None, None).is_err()); // last frame protected
    }

    #[test]
    fn frame_ops_move_remaps_tags_without_ballooning() {
        // A tagged frame moved far away LEAVES its tag (no ballooning over
        // untagged frames).
        let mut d = Document::new("t", 2, 2);
        for _ in 0..3 {
            d.add_frame(100, None);
        }
        d.add_tag("walk", 0, 1, "forward").unwrap();
        d.frame_ops("move", 1, Some(3), None).unwrap();
        assert_eq!((d.meta.tags[0].from, d.meta.tags[0].to), (0, 0));

        // A reorder entirely INSIDE a tag keeps the tag's full coverage.
        let mut e = Document::new("t", 2, 2);
        e.add_frame(100, None);
        e.add_frame(100, None);
        e.add_tag("all", 0, 2, "forward").unwrap();
        e.frame_ops("move", 1, Some(2), None).unwrap();
        assert_eq!((e.meta.tags[0].from, e.meta.tags[0].to), (0, 2));

        // A single-frame tag follows its frame.
        let mut s = Document::new("t", 2, 2);
        for _ in 0..3 {
            s.add_frame(100, None);
        }
        s.add_tag("pose", 1, 1, "forward").unwrap();
        s.frame_ops("move", 1, Some(3), None).unwrap();
        assert_eq!((s.meta.tags[0].from, s.meta.tags[0].to), (3, 3));
    }

    #[test]
    fn frame_ops_move_and_duplicate() {
        let mut d = Document::new("t", 2, 2);
        d.pencil(0, 0, &[(0, 0)], [1, 1, 1, 255], 1).unwrap();
        d.add_frame(100, None);
        d.pencil(0, 1, &[(0, 0)], [2, 2, 2, 255], 1).unwrap();
        d.frame_ops("move", 0, Some(1), None).unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [2, 2, 2, 255]);
        assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [1, 1, 1, 255]);
        d.frame_ops("duplicate", 0, None, None).unwrap();
        assert_eq!(d.meta.frames.len(), 3);
        assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [2, 2, 2, 255]); // the copy
        assert_eq!(d.get_pixel(0, 2, 0, 0).unwrap(), [1, 1, 1, 255]); // shifted
    }

    #[test]
    fn move_layer_reorders_and_cels_follow() {
        let mut d = Document::new("t", 4, 4);
        d.add_layer(None, 255, "normal".into()); // layer 1
        d.pencil(0, 0, &[(0, 0)], [255, 0, 0, 255], 1).unwrap();
        d.pencil(1, 0, &[(1, 1)], [0, 0, 255, 255], 1).unwrap();
        d.move_layer(0, 1).unwrap();
        // old layer 1 (blue) is now index 0; old layer 0 (red) is index 1
        assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [0, 0, 255, 255]);
        assert_eq!(d.get_pixel(1, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
    }

    #[test]
    fn insert_and_delete_layer_shift_cels() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], [9, 9, 9, 255], 1).unwrap();
        d.insert_layer(0, Some("bg".into()), 255, "normal".into());
        // the drawn cel moved from layer 0 to layer 1
        assert_eq!(d.meta.layers.len(), 2);
        assert_eq!(d.get_pixel(1, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
        d.delete_layer(0).unwrap();
        assert_eq!(d.meta.layers.len(), 1);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
    }

    #[test]
    fn delete_last_layer_is_refused() {
        let mut d = Document::new("t", 4, 4);
        assert!(d.delete_layer(0).is_err());
    }

    #[test]
    fn duplicate_layer_copies_cels_above() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(2, 2)], [7, 7, 7, 255], 1).unwrap();
        let ni = d.duplicate_layer(0).unwrap();
        assert_eq!(ni, 1);
        assert_eq!(d.get_pixel(0, 0, 2, 2).unwrap(), [7, 7, 7, 255]);
        assert_eq!(d.get_pixel(1, 0, 2, 2).unwrap(), [7, 7, 7, 255]);
    }

    #[test]
    fn merge_down_bakes_upper_onto_lower() {
        let mut d = Document::new("t", 4, 4);
        d.add_layer(None, 255, "normal".into());
        d.pencil(0, 0, &[(0, 0)], [255, 0, 0, 255], 1).unwrap();
        d.pencil(1, 0, &[(0, 0)], [0, 0, 255, 255], 1).unwrap();
        d.merge_down(1).unwrap();
        assert_eq!(d.meta.layers.len(), 1);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 255, 255]);
    }

    #[test]
    fn snap_to_palette_picks_perceptual_nearest() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(0, 0)], [200, 10, 10, 255], 1).unwrap();
        let changed = d.snap_to_palette(
            &[[255, 0, 0, 255], [0, 0, 255, 255]],
            None,
            None,
            AlphaSnap::Preserve,
        );
        assert_eq!(changed, 1);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]);
    }

    #[test]
    fn replace_color_recolours_aa_edges() {
        // A solid pixel and a same-RGB anti-aliased (low-alpha) edge: both should
        // recolour at tol 0 now that the match ignores alpha (RGB max-channel).
        let mut d = Document::new("t", 3, 1);
        d.pencil(0, 0, &[(0, 0)], [200, 0, 0, 255], 1).unwrap();
        d.pencil(0, 0, &[(1, 0)], [200, 0, 0, 80], 1).unwrap();
        d.replace_color(0, 0, [200, 0, 0, 255], [0, 0, 255, 255], 0)
            .unwrap();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 255, 255]);
        assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 255, 255]); // AA edge too
    }

    #[test]
    fn snap_opaque_collapses_bloom_to_crisp_palette() {
        // A continuous-tone bloom: one bright core + one faint halo pixel, both
        // off-palette tints. Opaque snap should make the core a solid palette
        // colour and clear the faint halo — crisp, not soft.
        let mut d = Document::new("t", 4, 4);
        let pal = [[255, 0, 0, 255], [0, 0, 255, 255]];
        d.pencil(0, 0, &[(0, 0)], [200, 12, 12, 200], 1).unwrap(); // bright core
        d.pencil(0, 0, &[(1, 0)], [180, 30, 30, 30], 1).unwrap(); // faint halo
        d.snap_to_palette(&pal, None, None, AlphaSnap::Opaque(128));
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [255, 0, 0, 255]); // core → solid
        assert_eq!(d.get_pixel(0, 0, 1, 0).unwrap(), [0, 0, 0, 0]); // halo → cleared
    }

    #[test]
    fn fx_resnaps_to_locked_palette_by_default() {
        // A blur across two flat palette blocks averages the boundary into
        // off-palette mud. With a palette locked, the continuous-tone FX path
        // must re-snap by default so every pixel stays on-palette.
        let mut d = Document::new("t", 4, 4);
        let pal = vec![[220, 30, 30, 255], [30, 30, 220, 255]];
        d.set_palette(pal.clone());
        d.apply_op(
            0,
            0,
            &json!({"op": "fill_cel", "color": [220, 30, 30, 255]}),
        )
        .unwrap();
        d.apply_op(
            0,
            0,
            &json!({"op": "rect", "x0": 2, "y0": 0, "x1": 3, "y1": 3, "color": [30, 30, 220, 255], "fill": true}),
        )
        .unwrap();
        d.apply_op(0, 0, &json!({"op": "blur", "radius": 1}))
            .unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let p = d.get_pixel(0, 0, x, y).unwrap();
                assert!(pal.contains(&p), "pixel {p:?} at {x},{y} off-palette");
            }
        }
    }

    #[test]
    fn fx_snap_false_keeps_continuous_tone() {
        // The same blur with `snap:false` opts out — the blended boundary is
        // allowed to stay off-palette.
        let mut d = Document::new("t", 4, 4);
        let pal = vec![[220, 30, 30, 255], [30, 30, 220, 255]];
        d.set_palette(pal.clone());
        d.apply_op(
            0,
            0,
            &json!({"op": "fill_cel", "color": [220, 30, 30, 255]}),
        )
        .unwrap();
        d.apply_op(
            0,
            0,
            &json!({"op": "rect", "x0": 2, "y0": 0, "x1": 3, "y1": 3, "color": [30, 30, 220, 255], "fill": true}),
        )
        .unwrap();
        d.apply_op(0, 0, &json!({"op": "blur", "radius": 1, "snap": false}))
            .unwrap();
        let has_off = (0..4)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .any(|(x, y)| !pal.contains(&d.get_pixel(0, 0, x, y).unwrap()));
        assert!(
            has_off,
            "snap:false should leave off-palette blended pixels"
        );
    }

    #[test]
    fn gradient_map_remaps_luminance_keeps_alpha() {
        let mut d = Document::new("t", 4, 1);
        d.pencil(0, 0, &[(0, 0)], [20, 20, 20, 255], 1).unwrap(); // dark
        d.pencil(0, 0, &[(1, 0)], [240, 240, 240, 200], 1).unwrap(); // light, soft alpha
        d.apply_op(
            0,
            0,
            &json!({"op": "gradient_map", "stops": [
                {"pos": 0.0, "color": [10, 0, 40, 255]},
                {"pos": 1.0, "color": [255, 200, 80, 255]}
            ]}),
        )
        .unwrap();
        let dark = d.get_pixel(0, 0, 0, 0).unwrap();
        let light = d.get_pixel(0, 0, 1, 0).unwrap();
        // Dark maps near the first stop, light near the last; alpha preserved.
        assert!(
            dark[0] < 40 && dark[2] > 20,
            "dark → deep stop, got {dark:?}"
        );
        assert!(
            light[0] > 200 && light[1] > 150,
            "light → warm stop, got {light:?}"
        );
        assert_eq!(light[3], 200);
        // Transparent pixels untouched.
        assert_eq!(d.get_pixel(0, 0, 3, 0).unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn export_sheet_std_writes_engine_parsable_json() {
        let mut d = Document::new("runner", 8, 8);
        d.fill_cel(0, 0, [255, 0, 0, 255]).unwrap();
        d.add_frame(80, Some(0));
        d.add_tag("run", 0, 1, "forward").unwrap();
        let dir = std::env::temp_dir().join("atelier-std-json-test");
        let _ = std::fs::create_dir_all(&dir);
        let out = dir.join("runner.png");
        let r = d.export_sheet_std(&out, 2).unwrap();
        assert_eq!(r["meta_format"], "standard");
        let meta: Value =
            serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
                .unwrap();
        // The hash layout engines parse: frames keyed by name, rect under
        // "frame", per-frame "duration", tags under meta.frameTags.
        let f0 = &meta["frames"]["runner 0"];
        assert_eq!(f0["frame"]["w"], 16); // 8px * scale 2
        assert_eq!(f0["frame"]["x"], 0);
        assert_eq!(meta["frames"]["runner 1"]["frame"]["x"], 16);
        assert_eq!(meta["frames"]["runner 1"]["duration"], 80);
        assert_eq!(meta["meta"]["image"], "runner.png");
        assert_eq!(meta["meta"]["frameTags"][0]["name"], "run");
        assert_eq!(meta["meta"]["size"]["w"], 32);
    }

    #[test]
    fn cast_shadow_projects_behind_caster() {
        let mut d = Document::new("t", 24, 24);
        // A vertical bar caster resting on row 15.
        d.rect(0, 0, 10, 4, 12, 15, [255, 255, 255, 255], true, 1)
            .unwrap();
        let painted = d
            .cast_shadow(0, 0, 135.0, 1.5, 0.2, [20, 20, 30, 255], 180, None, false)
            .unwrap();
        assert!(painted > 0, "shadow should paint pixels");
        // The caster still sits on top (unchanged white).
        assert_eq!(d.get_pixel(0, 0, 11, 5).unwrap(), [255, 255, 255, 255]);
        // A shadow-coloured pixel exists off the caster.
        let mut found = false;
        for y in 0..24 {
            for x in 0..24 {
                let p = d.get_pixel(0, 0, x, y).unwrap();
                if p[3] > 0 && p[0] == 20 && p[1] == 20 && p[2] == 30 {
                    found = true;
                }
            }
        }
        assert!(found, "expected shadow-coloured pixels");
    }

    #[test]
    fn cast_shadow_clips_to_receiver() {
        let mut d = Document::new("t", 24, 24);
        let ground = d.add_layer(Some("ground".into()), 255, "normal".into());
        let caster = d.add_layer(Some("caster".into()), 255, "normal".into());
        d.rect(ground, 0, 0, 10, 23, 19, [40, 120, 40, 255], true, 1)
            .unwrap();
        d.rect(caster, 0, 8, 6, 10, 15, [255, 255, 255, 255], true, 1)
            .unwrap();
        let painted = d
            .cast_shadow(
                caster,
                0,
                180.0,
                1.0,
                0.3,
                [10, 10, 20, 255],
                200,
                Some(ground),
                false,
            )
            .unwrap();
        assert!(painted > 0, "shadow should land on the ground");
        // The caster layer is untouched.
        assert_eq!(d.get_pixel(caster, 0, 9, 7).unwrap(), [255, 255, 255, 255]);
        // The ground is partly darkened (shadow) and partly its own colour.
        let (mut blended, mut pure) = (0u32, 0u32);
        for y in 0..24 {
            for x in 0..24 {
                let p = d.get_pixel(ground, 0, x, y).unwrap();
                if p[3] == 0 {
                    continue;
                }
                if p == [40, 120, 40, 255] {
                    pure += 1;
                } else {
                    blended += 1;
                }
            }
        }
        assert!(
            blended > 0 && pure > 0,
            "ground should be part-shadowed (blended={blended}, pure={pure})"
        );
    }

    #[test]
    fn stroke_f_keeps_subpixel_position() {
        // A 1px AA stroke centred on row 3 lights that row nearly solid; nudging
        // it half a pixel down splits its coverage between rows 3 and 4, so the
        // row-3 alpha must drop. If the point were rounded to an integer first
        // (the old double-quantize) both cases would be identical.
        let on_row = |y: f32| {
            let mut d = Document::new("t", 12, 8);
            d.stroke_f(
                0,
                0,
                &[(2.0, y, 1.0), (8.0, y, 1.0)],
                [255, 255, 255, 255],
                true,
                false,
            )
            .unwrap();
            d.get_pixel(0, 0, 5, 3).unwrap()[3]
        };
        let centred = on_row(3.0);
        let nudged = on_row(3.5);
        assert!(
            centred > nudged + 40,
            "sub-pixel nudge should lower row-3 coverage (centred={centred}, nudged={nudged})"
        );
    }

    #[test]
    fn snap_flatten_melts_partial_alpha_onto_backdrop() {
        // A faint bluish bloom over a dark backdrop should flatten to an opaque
        // on-palette colour rather than staying semi-transparent off-palette.
        let mut d = Document::new("t", 4, 4);
        let pal = [[20, 20, 40, 255], [120, 140, 255, 255]];
        d.pencil(0, 0, &[(0, 0)], [168, 207, 255, 60], 1).unwrap();
        d.snap_to_palette(&pal, None, None, AlphaSnap::Flatten([20, 20, 40, 255]));
        let p = d.get_pixel(0, 0, 0, 0).unwrap();
        assert_eq!(p[3], 255); // fully opaque after flatten
        assert!(pal.contains(&p)); // and on-palette
    }

    #[test]
    fn stroke_is_gap_free_union() {
        // A thin diagonal capsule: the union rasterizer must leave NO empty row
        // across the span (the gap-free property stacked beziers lack).
        let mut d = Document::new("t", 48, 48);
        d.stroke(
            0,
            0,
            &[(2, 2, 2), (45, 45, 2)],
            [255, 255, 255, 255],
            false,
            false,
        )
        .unwrap();
        for y in 4..44 {
            let any = (0..48).any(|x| d.get_pixel(0, 0, x, y).unwrap()[3] > 0);
            assert!(any, "row {y} had a gap");
        }
    }

    #[test]
    fn stroke_tapers_toward_zero_width_tip() {
        let mut d = Document::new("t", 48, 16);
        d.stroke(
            0,
            0,
            &[(8, 8, 0), (40, 8, 10)],
            [255, 255, 255, 255],
            false,
            false,
        )
        .unwrap();
        let col = |x: i32| {
            (0..16)
                .filter(|&y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0)
                .count()
        };
        assert!(col(10) <= 2, "tip should be ~1px, got {}", col(10));
        assert!(col(38) >= 7, "wide end should be ~10px, got {}", col(38));
    }

    #[test]
    fn stroke_aa_emits_fractional_coverage() {
        // Bresenham yields zero partial-alpha pixels; the analytic coverage core
        // must produce a smooth AA edge.
        let mut d = Document::new("t", 32, 32);
        d.stroke(
            0,
            0,
            &[(4, 4, 3), (28, 28, 3)],
            [255, 255, 255, 255],
            true,
            false,
        )
        .unwrap();
        let frac = (0..32)
            .flat_map(|y| (0..32).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let a = d.get_pixel(0, 0, x, y).unwrap()[3];
                a > 0 && a < 255
            })
            .count();
        assert!(
            frac >= 20,
            "AA capsule should have ≥20 fractional pixels, got {frac}"
        );
    }

    #[test]
    fn rim_light_lights_only_the_facing_edge() {
        let mut d = Document::new("t", 16, 16);
        d.ellipse(0, 0, 8, 8, 6, 6, [80, 80, 80, 255], true)
            .unwrap();
        // Light from the right (az=0): the right edge lights, the left does not.
        d.rim_light(0, 0, [255, 255, 255, 255], 0.0, 1, 1.5, false, false)
            .unwrap();
        let opaque = |x: i32| d.get_pixel(0, 0, x, 8).unwrap()[3] > 0;
        let rmost = (0..16).rev().find(|&x| opaque(x)).unwrap();
        let lmost = (0..16).find(|&x| opaque(x)).unwrap();
        // Right (light-facing) edge lightens toward white; left edge untouched.
        assert!(
            d.get_pixel(0, 0, rmost, 8).unwrap()[0] > 80,
            "right (light-facing) edge should be rim-lit (lighter than base 80)"
        );
        assert_eq!(
            d.get_pixel(0, 0, lmost, 8).unwrap(),
            [80, 80, 80, 255],
            "left (away) edge should NOT be rim-lit"
        );
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
    fn frame_boxes_set_validate_and_scale() {
        let mut d = Document::new("t", 4, 4);
        let b = |kind: &str| BoxMeta {
            name: "b".into(),
            kind: kind.into(),
            rect: [1, 2, 3, 4],
        };
        d.set_frame_boxes(0, vec![b("body"), b("hit")]).unwrap();
        assert_eq!(d.meta.frames[0].boxes.len(), 2);
        // Unknown kind fails fast; a bad kind leaves the frame untouched.
        assert!(d.set_frame_boxes(0, vec![b("shield")]).is_err());
        assert_eq!(d.meta.frames[0].boxes.len(), 2);
        // Bad frame index errors.
        assert!(d.set_frame_boxes(9, vec![b("hurt")]).is_err());
        // Rect scales by the export factor; raw at scale 1.
        assert_eq!(b("hit").to_json(1)["rect"], json!([1, 2, 3, 4]));
        assert_eq!(b("hit").to_json(8)["rect"], json!([8, 16, 24, 32]));
        // Empty vec clears.
        d.set_frame_boxes(0, vec![]).unwrap();
        assert!(d.meta.frames[0].boxes.is_empty());
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
    fn form_sphere_gives_volume_toward_light() {
        let mut d = Document::new("t", 32, 32);
        // A flat-filled disc — no volume yet.
        d.ellipse(0, 0, 16, 16, 12, 12, [120, 120, 120, 255], true)
            .unwrap();
        let ramp: Vec<[u8; 4]> = vec![
            [20, 20, 20, 255],
            [70, 70, 70, 255],
            [120, 120, 120, 255],
            [180, 180, 180, 255],
            [240, 240, 240, 255],
        ];
        d.form(0, 0, "top-left", "sphere", None, Some(ramp), 1.0)
            .unwrap();
        let lum = |p: [u8; 4]| p[0] as i32 + p[1] as i32 + p[2] as i32;
        let tl = d.get_pixel(0, 0, 9, 9).unwrap(); // toward the light
        let br = d.get_pixel(0, 0, 23, 23).unwrap(); // away from it
        assert!(tl[3] > 0 && br[3] > 0); // both still opaque
        assert!(
            lum(tl) > lum(br),
            "lit corner {:?} should outshine shadow corner {:?}",
            tl,
            br
        );
    }

    #[test]
    fn form_auto_brightens_interior_over_edge() {
        let mut d = Document::new("t", 32, 32);
        d.ellipse(0, 0, 16, 16, 12, 12, [120, 120, 120, 255], true)
            .unwrap();
        d.form(0, 0, "top", "auto", None, None, 1.0).unwrap();
        let lum = |p: [u8; 4]| p[0] as i32 + p[1] as i32 + p[2] as i32;
        let core = d.get_pixel(0, 0, 16, 16).unwrap(); // deep interior
        let edge = d.get_pixel(0, 0, 16, 27).unwrap(); // bottom rim
        assert!(core[3] > 0 && edge[3] > 0);
        assert!(
            lum(core) > lum(edge),
            "interior {:?} should be brighter than the rim {:?}",
            core,
            edge
        );
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

    #[test]
    fn text_renders_known_glyph_exactly() {
        // 'I' is a top bar, a centre stem, and a bottom bar in its 3×5 cell.
        let mut d = Document::new("t", 8, 8);
        d.text(0, 0, 1, 1, "I", [255, 255, 255, 255], 1).unwrap();
        let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
        // top bar (y=1): all three columns lit
        assert!(on(1, 1) && on(2, 1) && on(3, 1));
        // centre stem (x=2) for the three middle rows
        assert!(on(2, 2) && on(2, 3) && on(2, 4));
        // the stem's flanks stay empty
        assert!(!on(1, 2) && !on(3, 2));
        // bottom bar (y=5): all three columns lit
        assert!(on(1, 5) && on(2, 5) && on(3, 5));
    }

    #[test]
    fn text_size_two_doubles_every_pixel() {
        // Each lit cell becomes a 2×2 block, so the top bar spans y=0..1.
        let mut d = Document::new("t", 16, 16);
        d.text(0, 0, 0, 0, "I", [10, 20, 30, 255], 2).unwrap();
        let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
        // top bar doubled vertically and across all six scaled columns
        for x in 0..6 {
            assert!(on(x, 0) && on(x, 1), "top bar block missing at x={x}");
        }
        // centre stem (cell col 1 -> scaled cols 2,3) doubled at the second row band
        assert!(on(2, 2) && on(3, 2) && on(2, 3) && on(3, 3));
    }

    #[test]
    fn text_returns_layout_width() {
        // size 1: glyph is 3px, +1px spacing between glyphs, no trailing space.
        let mut d = Document::new("t", 32, 8);
        assert_eq!(d.text(0, 0, 0, 0, "", [0; 4], 1).unwrap(), 0);
        assert_eq!(d.text(0, 0, 0, 0, "I", [255; 4], 1).unwrap(), 3);
        assert_eq!(d.text(0, 0, 0, 0, "II", [255; 4], 1).unwrap(), 7); // 3 + 1 + 3
                                                                       // size 2 scales the whole width.
        assert_eq!(d.text(0, 0, 0, 0, "II", [255; 4], 2).unwrap(), 14);
    }

    #[test]
    fn text_size_is_clamped_against_runaway_scaling() {
        // A huge size completes (no hang) and behaves as the 64 clamp ceiling.
        let mut d = Document::new("t", 256, 512);
        let clamped = d.text(0, 0, 0, 0, "I", [255; 4], 64).unwrap();
        let huge = d.text(0, 0, 0, 0, "I", [255; 4], 9999).unwrap();
        assert_eq!(huge, clamped);
        assert_eq!(huge, 3 * 64); // 192px, the clamped glyph width
    }

    #[test]
    fn text_unknown_char_is_hollow_box() {
        // '@' is not in the font, so it renders as the 3×5 hollow box.
        let mut d = Document::new("t", 8, 8);
        d.text(0, 0, 0, 0, "@", [255, 255, 255, 255], 1).unwrap();
        let on = |x, y| d.get_pixel(0, 0, x, y).unwrap()[3] > 0;
        assert!(on(0, 0) && on(2, 0) && on(0, 4) && on(2, 4)); // corners
        assert!(!on(1, 2)); // hollow centre
    }

    #[test]
    fn batch_text_op_validates() {
        // The "text" op must be a known batch op with its required keys checked.
        let ok = json!({"op": "text", "x": 0, "y": 0, "text": "HI", "color": [255, 255, 255]});
        assert!(validate_batch_op(0, &ok).is_ok());
        // Missing the required `text` key is rejected.
        let missing = json!({"op": "text", "x": 0, "y": 0, "color": [255, 255, 255]});
        assert!(validate_batch_op(0, &missing).is_err());
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
    fn get_pixel_reads_back_a_drawn_pixel() {
        let mut d = Document::new("t", 8, 8);
        d.pencil(0, 0, &[(3, 4)], [10, 20, 30, 255], 1).unwrap();
        assert_eq!(d.get_pixel(0, 0, 3, 4).unwrap(), [10, 20, 30, 255]);
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]); // untouched
        assert_eq!(d.get_pixel(0, 0, 99, 99).unwrap(), [0, 0, 0, 0]); // OOB
    }

    #[test]
    fn shift_wrap_rolls_pixels_around() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(3, 0)], [9, 9, 9, 255], 1).unwrap();
        d.shift(0, 0, 1, 0, true).unwrap(); // (3+1)%4 = 0
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
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
    fn move_region_does_not_punch_a_hole_in_dest_art() {
        // Move a 2x2 block that contains a transparent corner over existing art;
        // the transparent corner must NOT erase the art already there.
        let mut d = Document::new("t", 8, 8);
        d.pencil(0, 0, &[(0, 0)], [9, 9, 9, 255], 1).unwrap(); // only opaque pixel of the 2x2 source
        d.pencil(0, 0, &[(6, 2)], [7, 7, 7, 255], 1).unwrap(); // dest art, under the block's transparent corner
                                                               // Move the 2x2 rect (0,0)-(1,1) by (+5,+1): source(0,0)->(5,1), source(1,1)->(6,2).
        d.move_region(0, 0, 0, 0, 1, 1, 5, 1).unwrap();
        assert_eq!(d.get_pixel(0, 0, 5, 1).unwrap(), [9, 9, 9, 255]); // opaque pixel landed
        assert_eq!(d.get_pixel(0, 0, 6, 2).unwrap(), [7, 7, 7, 255]); // dest art survived the transparent corner
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
    fn gradient_region_fully_off_canvas_is_a_no_op() {
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
            Some((20, 20, 30, 30)),
            false,
        )
        .unwrap(); // succeeds without painting anything
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [0, 0, 0, 0]);
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

    #[test]
    fn export_sheet_writes_png_and_json_sidecar() {
        let dir = std::env::temp_dir().join(format!("atelier_sheet_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = doc_with_frames(3);
        d.fill_cel(0, 0, [255, 0, 0, 255]).unwrap();
        let out = dir.join("sheet.png");
        d.export_sheet(&out, 2).unwrap();
        // 3 frames × (4·2) wide, (4·2) tall.
        let png = image::open(&out).unwrap().to_rgba8();
        assert_eq!(png.dimensions(), (24, 8));
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(out.with_extension("json")).unwrap())
                .unwrap();
        assert_eq!(json["frames"].as_array().unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_gif_writes_nonempty_file() {
        let dir = std::env::temp_dir().join(format!("atelier_gif_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = doc_with_frames(2);
        d.fill_cel(0, 0, [0, 255, 0, 255]).unwrap();
        d.fill_cel(0, 1, [0, 0, 255, 255]).unwrap();
        let out = dir.join("anim.gif");
        let n = d.export_gif(&out, 1, None).unwrap();
        assert_eq!(n, 2);
        let bytes = std::fs::metadata(&out).unwrap().len();
        assert!(bytes > 0, "gif should be nonempty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `acTL` frame count from APNG bytes: scan for the chunk type marker, read
    /// the following 4-byte big-endian `num_frames` field (start of its data).
    #[cfg(test)]
    fn apng_frame_count(bytes: &[u8]) -> Option<u32> {
        let pos = bytes.windows(4).position(|w| w == b"acTL")?;
        let n = &bytes[pos + 4..pos + 8];
        Some(u32::from_be_bytes([n[0], n[1], n[2], n[3]]))
    }

    /// First `fcTL`'s `(delay_num, delay_den)` from APNG bytes: the two big-endian
    /// u16 fields at offsets 20 and 22 into the chunk data (after the marker).
    #[cfg(test)]
    fn apng_first_frame_delay(bytes: &[u8]) -> Option<(u16, u16)> {
        let pos = bytes.windows(4).position(|w| w == b"fcTL")? + 4;
        let num = u16::from_be_bytes([bytes[pos + 20], bytes[pos + 21]]);
        let den = u16::from_be_bytes([bytes[pos + 22], bytes[pos + 23]]);
        Some((num, den))
    }

    #[test]
    fn export_apng_is_animated_png_with_matching_frame_count() {
        let dir = std::env::temp_dir().join(format!("atelier_apng_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = doc_with_frames(2);
        d.fill_cel(0, 0, [0, 255, 0, 128]).unwrap(); // partial alpha — APNG keeps it
        d.fill_cel(0, 1, [0, 0, 255, 255]).unwrap();
        let out = dir.join("anim.png");
        let n = d.export_apng(&out, 2, None).unwrap();
        assert_eq!(n, 2);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
        assert_eq!(apng_frame_count(&bytes), Some(2), "acTL frame count");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_apng_honours_tag_direction() {
        let dir = std::env::temp_dir().join(format!("atelier_apng_tag_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = doc_with_frames(3);
        // pingpong over 0..2 plays 0,1,2,1 — 4 frames from a 3-frame range.
        d.add_tag("pp", 0, 2, "pingpong").unwrap();
        let out = dir.join("pp.png");
        let n = d.export_apng(&out, 1, Some("pp")).unwrap();
        assert_eq!(n, 4);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(
            apng_frame_count(&bytes),
            Some(4),
            "acTL matches pingpong len"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_apng_long_delay_does_not_wrap() {
        let dir = std::env::temp_dir().join(format!("atelier_apng_long_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = doc_with_frames(1);
        d.set_frame_duration(0, 70_000).unwrap(); // > u16::MAX ms — used to wrap
        let out = dir.join("long.png");
        d.export_apng(&out, 1, None).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        // 70_000ms is encoded as 7000/100s, not a truncated u16.
        assert_eq!(apng_first_frame_delay(&bytes), Some((7000, 100)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn per_op_opacity_blends_in_batch() {
        let mut d = Document::new("t", 2, 2);
        d.fill_cel(0, 0, [0, 0, 0, 255]).unwrap();
        d.apply_op(0, 0, &json!({"op":"rect","x0":0,"y0":0,"x1":1,"y1":1,"color":[200,0,0],"fill":true,"opacity":128})).unwrap();
        let p = d.get_pixel(0, 0, 0, 0).unwrap();
        assert!(
            (p[0] as i32 - 100).abs() <= 2,
            "expected ~100, got {}",
            p[0]
        ); // 50% red over black
        assert_eq!(p[3], 255);
    }

    #[test]
    fn silhouette_center_of_known_shape() {
        let mut d = Document::new("t", 6, 6);
        // Filled 3×3 block from (1,1) to (3,3): bbox-centre is (2,2).
        d.rect(0, 0, 1, 1, 3, 3, [255, 255, 255, 255], true, 1)
            .unwrap();
        let c = d.silhouette_center(None, 0, None).unwrap().unwrap();
        assert_eq!(c, [2.0, 2.0]);
    }

    #[test]
    fn seam_axis_solid_is_seamless_edge_mismatch_is_not() {
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [120, 120, 120, 255]).unwrap();
        // A solid cel tiles seamlessly: far edge == near edge on both axes.
        let (mh, _, _) = d.seam_axis(None, 0, true, 8).unwrap();
        let (mv, _, _) = d.seam_axis(None, 0, false, 8).unwrap();
        assert_eq!(mh, 0);
        assert_eq!(mv, 0);
        // Recolour the far (right) column so it no longer matches x=0.
        d.line(0, 0, 3, 0, 3, 3, [10, 10, 10, 255], 1).unwrap();
        let (mh2, max_delta, _) = d.seam_axis(None, 0, true, 8).unwrap();
        assert!(mh2 > 0, "edge mismatch should be detected");
        assert!(max_delta > 8, "delta should exceed threshold");
    }
}
