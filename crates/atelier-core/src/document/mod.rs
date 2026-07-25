//! The editable document model — atelier's layered, animated core.
//!
//! A `Document` is a canvas of ordered **layers** (opacity / visibility / blend)
//! over a timeline of **frames** (each with a duration). A **cel** is one
//! layer×frame image placed at (x,y); cels are sparse. The document also holds a
//! **palette** and animation **tags** (named frame ranges).
//!
//! Persistence: a directory with `doc.json` (structure + cel file refs) and one
//! PNG per cel under `cels/`. Rendering flattens visible layers at a frame with
//! source-over compositing scaled by layer opacity; export covers spritesheets
//! (+ JSON sidecars) and animated GIF/APNG.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use image::{Rgba, RgbaImage};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::raster;

mod draw;
mod export;
mod fx;
mod operation;
mod palette;
mod region;
mod render;
mod timeline;

#[cfg(test)]
mod tests;

pub use fx::{DitherAxis, DitherPattern};
pub use operation::{color_array, draw_ops, fx_ops, validate_op};
pub use render::{ValueView, seam_axis_img};
pub use timeline::FrameAction;

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LayerMeta {
    pub name: String,
    pub opacity: u8,
    pub visible: bool,
    /// Compositing mode: normal/multiply/screen/add/overlay/soft-light/
    /// hard-light/darken/lighten/color-dodge/color-burn/difference/subtract/
    /// exclusion. Only canonical values are accepted.
    pub blend: raster::Blend,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct FrameMeta {
    pub duration_ms: u32,
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TagDirection {
    #[default]
    Forward,
    Reverse,
    Pingpong,
}

impl TagDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Reverse => "reverse",
            Self::Pingpong => "pingpong",
        }
    }
}

impl std::fmt::Display for TagDirection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct TagMeta {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: TagDirection,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CelMeta {
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub file: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DocMeta {
    pub name: String,
    pub w: u32,
    pub h: u32,
    pub palette: Vec<[u8; 4]>,
    pub layers: Vec<LayerMeta>,
    pub frames: Vec<FrameMeta>,
    pub tags: Vec<TagMeta>,
    pub cels: Vec<CelMeta>,
    /// Reference image filename inside the doc dir (`doc_ref op=set`).
    /// — the original the artwork is recreating, kept for compare loops.
    pub reference: Option<String>,
}

impl DocMeta {
    /// Validate the one current persisted metadata contract.
    pub fn validate(&self) -> Result<(), String> {
        validate_meta(self)
    }
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub(crate) meta: DocMeta,
    /// (layer, frame) -> (x, y, image)
    cels: HashMap<(usize, usize), (i32, i32, RgbaImage)>,
    /// Cels whose pixels changed since load (or that were re-keyed by a
    /// structural op). `save` writes only these — plus any cel whose file is
    /// missing — instead of re-encoding the whole document per tool call.
    dirty: HashSet<(usize, usize)>,
}

/// Result of `frame_diff_region`: `(added, removed, recolored, change_bbox,
/// image_a, image_b)` — the change tallies, the bbox of all changed pixels, and
/// both analysis images so callers can also render a grid/overlay.
pub type FrameDiff = (u32, u32, u32, Option<[i32; 4]>, RgbaImage, RgbaImage);

fn cel_file(layer: usize, frame: usize) -> String {
    format!("cels/L{}_F{}.png", layer, frame)
}

/// Validate the one current on-disk document shape. Loading does not repair,
/// default, or reinterpret persisted metadata.
fn validate_meta(meta: &DocMeta) -> Result<(), String> {
    if meta.w == 0 || meta.h == 0 {
        return Err(format!(
            "document dimensions must be non-zero, got {}x{}",
            meta.w, meta.h
        ));
    }
    if meta.layers.is_empty() {
        return Err("document must contain at least one layer".into());
    }
    if meta.frames.is_empty() {
        return Err("document must contain at least one frame".into());
    }
    for tag in &meta.tags {
        if tag.from > tag.to || tag.to >= meta.frames.len() {
            return Err(format!(
                "tag '{}' range {}..{} is outside {} frame(s)",
                tag.name,
                tag.from,
                tag.to,
                meta.frames.len()
            ));
        }
    }
    let mut cel_keys = HashSet::new();
    for cel in &meta.cels {
        if cel.layer >= meta.layers.len() || cel.frame >= meta.frames.len() {
            return Err(format!(
                "cel ({},{}) is outside {} layer(s) and {} frame(s)",
                cel.layer,
                cel.frame,
                meta.layers.len(),
                meta.frames.len()
            ));
        }
        if !cel_keys.insert((cel.layer, cel.frame)) {
            return Err(format!(
                "duplicate cel metadata for ({},{})",
                cel.layer, cel.frame
            ));
        }
        let expected = cel_file(cel.layer, cel.frame);
        if cel.file != expected {
            return Err(format!("refusing suspicious cel path '{}'", cel.file));
        }
    }
    if let Some(reference) = &meta.reference
        && reference != "reference.png"
    {
        return Err(format!(
            "stored reference must be 'reference.png', got '{reference}'"
        ));
    }
    Ok(())
}

/// True for the `L<layer>_F<frame>.png` shape `save` writes — the only files
/// the stale-cel sweep may remove.
fn is_cel_filename(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('L').and_then(|s| s.strip_suffix(".png")) else {
        return false;
    };
    let Some((l, f)) = rest.split_once("_F") else {
        return false;
    };
    !l.is_empty()
        && !f.is_empty()
        && l.chars().all(|c| c.is_ascii_digit())
        && f.chars().all(|c| c.is_ascii_digit())
}

/// How `snap_to_palette` treats the partial-alpha pixels that continuous-tone FX
/// (blur/gradient/drop_shadow) and AA fringes leave behind — the difference
/// between "snap the colour but keep 200 soft alphas off-palette" and "make it
/// crisp pixel art again".
#[derive(Clone, Copy, Debug)]
pub enum AlphaSnap {
    /// Keep each pixel's source alpha; only the RGB is snapped, preserving
    /// deliberate soft edges.
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

/// Default per-frame duration for a freshly created frame (milliseconds).
pub const DEFAULT_FRAME_MS: u32 = 100;

impl Document {
    /// Read-only view of the document metadata. The field itself is
    /// crate-private: meta and the cel map move in lock-step (layer/frame
    /// reindexing), so outside the crate every mutation goes through a method
    /// that preserves that invariant.
    pub fn meta(&self) -> &DocMeta {
        &self.meta
    }

    /// Set or clear the stored reference-image file name, returning the
    /// previous one (so a caller can delete the replaced file). The one
    /// meta field external callers may write — it has no cel coupling.
    pub fn set_reference_file(&mut self, name: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.meta.reference, name)
    }

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
                blend: raster::Blend::Normal,
            }],
            frames: vec![FrameMeta {
                duration_ms: DEFAULT_FRAME_MS,
            }],
            tags: Vec::new(),
            cels: Vec::new(),
            reference: None,
        };
        Document {
            meta,
            cels: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Document, String> {
        let s = std::fs::read_to_string(dir.join("doc.json")).map_err(|e| e.to_string())?;
        let meta: DocMeta = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        meta.validate()?;
        if let Some(reference) = &meta.reference
            && !dir.join(reference).is_file()
        {
            return Err(format!("stored reference file '{reference}' is missing"));
        }
        let mut cels = HashMap::new();
        for c in &meta.cels {
            let img = image::open(dir.join(&c.file))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            cels.insert((c.layer, c.frame), (c.x, c.y, img));
        }
        // Freshly loaded cels match their files — nothing is dirty yet.
        Ok(Document {
            meta,
            cels,
            dirty: HashSet::new(),
        })
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        if let Some(reference) = &self.meta.reference
            && !dir.join(reference).is_file()
        {
            return Err(format!("stored reference file '{reference}' is missing"));
        }
        std::fs::create_dir_all(dir.join("cels")).map_err(|e| e.to_string())?;
        let mut cel_metas = Vec::new();
        for ((layer, frame), (x, y, img)) in &self.cels {
            let file = cel_file(*layer, *frame);
            // Write only cels dirtied since load (or whose file is missing) —
            // a one-pixel edit used to re-encode and rewrite every cel in the
            // document, which made large animated docs crawl.
            if self.dirty.contains(&(*layer, *frame)) || !dir.join(&file).is_file() {
                img.save(dir.join(&file)).map_err(|e| e.to_string())?;
            }
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
        self.meta.validate()?;
        // Atomic-ish structure write: temp file + same-dir rename, so a crash
        // mid-write leaves the previous doc.json intact instead of a torn one.
        let tmp = dir.join("doc.json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&self.meta).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, dir.join("doc.json")).map_err(|e| e.to_string())?;
        // Delete cel files no longer referenced (cleared cels, deleted layers/
        // frames) so the store doesn't accumulate orphans. Only the L<n>_F<n>.png
        // shape is eligible — anything else in the dir is the user's. Runs after
        // the doc.json rename: a crash can then only leave a harmless orphan,
        // never a structure that references a deleted file.
        let keep: HashSet<String> = self.meta.cels.iter().map(|c| c.file.clone()).collect();
        if let Ok(rd) = std::fs::read_dir(dir.join("cels")) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                if is_cel_filename(&name) && !keep.contains(&format!("cels/{name}")) {
                    let _ = std::fs::remove_file(ent.path());
                }
            }
        }
        self.dirty.clear();
        Ok(())
    }

    /// Mark one cel for writing on the next `save`.
    fn mark_dirty(&mut self, layer: usize, frame: usize) {
        self.dirty.insert((layer, frame));
    }

    /// Re-keying ops (layer/frame remaps) rename every cel's file — all dirty.
    fn mark_all_dirty(&mut self) {
        self.dirty.extend(self.cels.keys().copied());
    }

    // -- structure ----------------------------------------------------------

    /// Append a new layer on top; returns its index.
    pub fn add_layer(&mut self, name: Option<String>, opacity: u8, blend: raster::Blend) -> usize {
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
        blend: Option<raster::Blend>,
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
        // Every surviving cel just changed its file name (L<old> → L<new>).
        self.mark_all_dirty();
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
        blend: raster::Blend,
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
            self.mark_dirty(new_index, f);
        }
        Ok(new_index)
    }

    /// Merge layer `index` down onto the layer below it, baking in the upper
    /// layer's opacity and blend mode (per frame), then remove the upper layer.
    /// The upper layer's pixels composite even when it is invisible — merging
    /// is a structural bake, not a render of the visible stack.
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
        let blend = upper.blend;
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

    /// Shift cel frame indices: every cel on a frame `>= from` moves by
    /// `delta` frames. The frame-axis twin of `remap_cel_layers` — the single
    /// choke point for keeping the cel map in lock-step with frame inserts,
    /// deletes and tween expansion.
    fn shift_cel_frames(&mut self, from: usize, delta: isize) {
        let keys: Vec<(usize, usize)> = self.cels.keys().filter(|k| k.1 >= from).cloned().collect();
        let mut moved = Vec::new();
        for k in keys {
            let v = self.cels.remove(&k).unwrap();
            moved.push(((k.0, (k.1 as isize + delta) as usize), v));
        }
        self.cels.extend(moved);
        // Re-keyed cels (F<old> → F<new>) all need writing under the new name.
        self.mark_all_dirty();
    }

    /// Append a new frame; with `copy_from`, duplicate that frame's cels into it.
    pub fn add_frame(&mut self, duration_ms: u32, copy_from: Option<usize>) -> usize {
        let idx = self.meta.frames.len();
        self.meta.frames.push(FrameMeta { duration_ms });
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
                self.mark_dirty(l, idx);
            }
        }
        idx
    }

    /// Add a named animation tag over an inclusive frame range.
    pub fn add_tag(
        &mut self,
        name: &str,
        from: usize,
        to: usize,
        direction: TagDirection,
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
            direction,
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
        self.mark_dirty(layer, frame);
        Ok(())
    }

    /// Remove the cel at (layer, frame), if any.
    ///
    /// Validates the target like every sibling cel op: without this,
    /// `clear_cel(99, 0)` on a one-layer document reported success, so an agent
    /// clearing the wrong index was told the cel was empty and carried on.
    pub fn clear_cel(&mut self, layer: usize, frame: usize) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.cels.remove(&(layer, frame));
        // Not dirty (nothing to write) — its old file goes stale, which `save`
        // sweeps after the doc.json rename.
        self.dirty.remove(&(layer, frame));
        Ok(())
    }

    /// JSON snapshot of the document structure (layers, frames, tags, cels,
    /// palette) for inspection — no pixel data.
    pub fn structure(&self) -> Value {
        let mut keys: Vec<(usize, usize)> = self.cels.keys().copied().collect();
        keys.sort_unstable();
        let cels: Vec<Value> = keys
            .into_iter()
            .map(|(l, f)| json!({"layer": l, "frame": f}))
            .collect();
        json!({
            "name": self.meta.name, "w": self.meta.w, "h": self.meta.h,
            "layers": self.meta.layers.iter().enumerate().map(|(i, l)| json!({
                "index": i, "name": l.name, "opacity": l.opacity, "visible": l.visible, "blend": l.blend
            })).collect::<Vec<_>>(),
            "frames": self.meta.frames.iter().enumerate().map(|(i, f)| json!({
                "index": i, "duration_ms": f.duration_ms,
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

    /// Test-only view of the live cel keys — the meta↔cel lock-step invariant
    /// the structural fuzzer asserts after every op.
    #[cfg(test)]
    pub(crate) fn cel_keys(&self) -> Vec<(usize, usize)> {
        self.cels.keys().copied().collect()
    }

    /// Read one pixel from a cel (document coords). Returns RGBA; out-of-bounds
    /// or an empty cel reads as transparent `[0,0,0,0]`. Read-only — never
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
        // Every caller of cel_canvas is a mutation op (draw/fx/region) — the
        // returned &mut image escapes our sight, so conservatively write it
        // back on the next save.
        self.mark_dirty(layer, frame);
        Ok(&mut self.cels.get_mut(&key).unwrap().2)
    }

    pub fn fill_cel(&mut self, layer: usize, frame: usize, color: [u8; 4]) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        let img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba(color));
        self.cels.insert((layer, frame), (0, 0, img));
        self.mark_dirty(layer, frame);
        Ok(())
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
}
