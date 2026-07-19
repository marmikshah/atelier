//! The editable document model — atelier's layered, animated core.
//!
//! A `Document` is a canvas of ordered **layers** (opacity / visibility / blend)
//! over a timeline of **frames** (each with a duration). A **cel** is one
//! layer×frame image placed at (x,y); cels are sparse — and a cel may be
//! **linked**, sharing another cel's pixels (copy-on-write: editing it breaks
//! the link instead of writing through). The document also holds a **palette**,
//! animation **tags** (named frame ranges) and **slices** (named canvas rects
//! with optional 9-slice guides and pivot).
//!
//! Persistence: a directory with `doc.json` (structure + cel file refs) and one
//! PNG per cel under `cels/` (a linked cel writes none — its `file` points at
//! the target's PNG). Rendering flattens visible layers at a frame with
//! source-over compositing scaled by layer opacity; export covers spritesheets
//! (+ JSON sidecars), animated GIF/APNG, and tileset slicing.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::raster;

mod batch;
mod draw;
mod export;
mod fx;
mod palette;
mod region;
mod render;
mod timeline;

#[cfg(test)]
mod tests;

pub use batch::{color_array, draw_ops, fx_ops, validate_batch_op};
pub use render::seam_axis_img;

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
}

#[derive(Serialize, Deserialize, Clone)]
pub struct TagMeta {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub direction: String, // "forward" | "reverse" | "pingpong"
}

/// A named canvas rect — a slice. `rect` is INCLUSIVE corners
/// `[x0, y0, x1, y1]`: the convention `copy_region`, `render_preview` and the
/// frame-diff bbox already use (not the x/y/w/h the engine sidecars want —
/// `export_sheet_std` converts). `center` is the optional 9-slice guide rect
/// (same convention, clamped inside `rect`); `pivot` an optional point.
#[derive(Serialize, Deserialize, Clone)]
pub struct SliceMeta {
    pub name: String,
    pub rect: [i32; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<[i32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pivot: Option<[i32; 2]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CelMeta {
    pub layer: usize,
    pub frame: usize,
    pub x: i32,
    pub y: i32,
    pub file: String,
    /// Linked cel: the (layer, frame) of the cel whose pixels this one
    /// shares. A linked cel has no PNG of its own — `file` names the
    /// target's — and `save` skips writing it. Old doc.json files predate
    /// the field (hence the serde defaults).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<(usize, usize)>,
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
    /// Named canvas rects (see `SliceMeta`) — defaulted so doc.json files
    /// written before slices existed still load.
    #[serde(default)]
    pub slices: Vec<SliceMeta>,
    pub cels: Vec<CelMeta>,
    /// Reference image filename inside the doc dir (set by doc_set_reference)
    /// — the original the artwork is recreating, kept for compare loops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// A loaded document: structure + the cel images in memory.
pub struct Document {
    pub(crate) meta: DocMeta,
    /// (layer, frame) -> (x, y, image). ALWAYS holds a linked cel's resolved
    /// pixels too (a snapshot refreshed from its target), so every reader —
    /// flatten, analysis, region copies — works unchanged.
    cels: HashMap<(usize, usize), (i32, i32, RgbaImage)>,
    /// Linked cels: (layer, frame) -> the (layer, frame) whose pixels it
    /// shares. Only records the sharing; the sync rules live on top:
    /// whole-cel writes to a target propagate eagerly, canvas-level strokes
    /// refresh dependents at the next edit entry point (`sync_links`), and a
    /// link whose target disappears materialises — the snapshot becomes the
    /// cel's own pixels — rather than dangling.
    links: HashMap<(usize, usize), (usize, usize)>,
    /// Cels whose pixels changed since load (or that were re-keyed by a
    /// structural op). `save` writes only these — plus any cel whose file is
    /// missing — instead of re-encoding the whole document per tool call.
    dirty: HashSet<(usize, usize)>,
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
                blend: "normal".into(),
            }],
            frames: vec![FrameMeta {
                duration_ms: DEFAULT_FRAME_MS,
            }],
            tags: Vec::new(),
            slices: Vec::new(),
            cels: Vec::new(),
            reference: None,
        };
        Document {
            meta,
            cels: HashMap::new(),
            links: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    pub fn load(dir: &Path) -> Result<Document, String> {
        let s = std::fs::read_to_string(dir.join("doc.json")).map_err(|e| e.to_string())?;
        let meta: DocMeta = serde_json::from_str(&s).map_err(|e| e.to_string())?;
        let mut cels = HashMap::new();
        for c in &meta.cels {
            // A linked cel reads no file of its own — its pixels come from
            // the target cel, resolved in the second pass below.
            if c.link.is_some() {
                continue;
            }
            // `c.file` comes from doc.json; a synced/hand-crafted doc could point
            // it at `../…` or an absolute path to read arbitrary files. `save`
            // only ever writes the `cels/L{}_F{}.png` shape, so require exactly
            // that: a two-component, `cels/`-rooted relative path.
            let rel = Path::new(&c.file);
            let ok = c.file == format!("cels/L{}_F{}.png", c.layer, c.frame)
                || (rel.components().count() == 2
                    && rel.starts_with("cels")
                    && rel
                        .components()
                        .all(|comp| matches!(comp, std::path::Component::Normal(_))));
            if !ok {
                return Err(format!("refusing suspicious cel path '{}'", c.file));
            }
            let img = image::open(dir.join(rel))
                .map_err(|e| e.to_string())?
                .to_rgba8();
            cels.insert((c.layer, c.frame), (c.x, c.y, img));
        }
        // Second pass: linked cels take their target's already-loaded pixels.
        // Targets are looked up ONLY among the first pass's real cels — a
        // chain (link at a linked cel) is refused: our saves never make one,
        // and a hand-built one would resolve in meta order.
        let real: HashSet<(usize, usize)> = cels.keys().copied().collect();
        let mut links = HashMap::new();
        for c in &meta.cels {
            let Some(target) = c.link else {
                continue;
            };
            if !real.contains(&target) {
                return Err(format!(
                    "cel L{}_F{} links to L{}_F{}, which is missing or itself linked",
                    c.layer, c.frame, target.0, target.1
                ));
            }
            let img = cels[&target].2.clone();
            cels.insert((c.layer, c.frame), (c.x, c.y, img));
            links.insert((c.layer, c.frame), target);
        }
        // Freshly loaded cels match their files — nothing is dirty yet.
        Ok(Document {
            meta,
            cels,
            links,
            dirty: HashSet::new(),
        })
    }

    pub fn save(&mut self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir.join("cels")).map_err(|e| e.to_string())?;
        // Materialise anything dangling before metas are written — a link
        // pointing at a gone cel would fail the NEXT load.
        self.sync_links();
        let mut cel_metas = Vec::new();
        for ((layer, frame), (x, y, img)) in &self.cels {
            // A linked cel's pixels live in its target's PNG: write nothing,
            // and point `file` at that PNG so the orphan sweep below keeps it.
            if let Some(&target) = self.links.get(&(*layer, *frame)) {
                cel_metas.push(CelMeta {
                    layer: *layer,
                    frame: *frame,
                    x: *x,
                    y: *y,
                    file: cel_file(target.0, target.1),
                    link: Some(target),
                });
                continue;
            }
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
                link: None,
            });
        }
        cel_metas.sort_by_key(|c| (c.layer, c.frame));
        self.meta.cels = cel_metas;
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

    /// Refresh every linked cel's snapshot from its target, and materialise
    /// any link whose cel or target is gone (the snapshot stays in the cel
    /// map, the link entry drops — a link never dangles). Runs at every
    /// canvas-edit entry point and before save: canvas-level strokes on a
    /// target return through callers we no longer see, so dependents catch
    /// up here instead.
    fn sync_links(&mut self) {
        let cels = &self.cels;
        self.links
            .retain(|k, t| cels.contains_key(k) && cels.contains_key(t));
        let live: Vec<((usize, usize), (usize, usize))> =
            self.links.iter().map(|(&k, &t)| (k, t)).collect();
        for (k, t) in live {
            if let Some(v) = self.cels.get(&t).cloned() {
                self.cels.insert(k, v);
            }
        }
    }

    /// Push one cel's current pixels into every cel linked to it — a linked
    /// cel tracks its target until it is itself edited. The whole-cel writes
    /// (set/fill/clear) call this so dependents track eagerly.
    fn propagate_link(&mut self, key: (usize, usize)) {
        let deps: Vec<(usize, usize)> = self
            .links
            .iter()
            .filter(|(_, t)| **t == key)
            .map(|(k, _)| *k)
            .collect();
        if deps.is_empty() {
            return;
        }
        match self.cels.get(&key).cloned() {
            Some(v) => {
                for d in deps {
                    self.cels.insert(d, v.clone());
                }
            }
            // The target is gone (cleared): dependents materialise on the
            // snapshot they already hold.
            None => {
                for d in deps {
                    self.links.remove(&d);
                }
            }
        }
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
        // Links follow the same remap, keys and targets alike; one whose cel
        // or target dropped out materialises (its snapshot stays in the cel
        // map under the remapped key, only the link entry is gone).
        let old_links = std::mem::take(&mut self.links);
        for ((l, f), (tl, tf)) in old_links {
            if let (Some(nl), Some(ntl)) = (map(l), map(tl)) {
                self.links.insert((nl, f), (ntl, tf));
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

    /// Shift cel frame indices: every cel on a frame `>= from` moves by
    /// `delta` frames. The frame-axis twin of `remap_cel_layers` — the single
    /// choke point for keeping the cel map in lock-step with frame inserts,
    /// deletes and tween expansion.
    fn shift_cel_frames(&mut self, from: usize, delta: isize) {
        // Materialise links already dangling (the caller's frame delete just
        // removed their cel or target) BEFORE any re-keying — afterwards a
        // moved key could land on the dead target's old index and look alive.
        let cels = &self.cels;
        self.links
            .retain(|k, t| cels.contains_key(k) && cels.contains_key(t));
        let keys: Vec<(usize, usize)> = self.cels.keys().filter(|k| k.1 >= from).cloned().collect();
        let mut moved = Vec::new();
        for k in keys {
            let v = self.cels.remove(&k).unwrap();
            moved.push(((k.0, (k.1 as isize + delta) as usize), v));
        }
        self.cels.extend(moved);
        // Links shift by the same frame rule, keys and targets alike.
        let old_links = std::mem::take(&mut self.links);
        for ((l, f), (tl, tf)) in old_links {
            let shift = |f: usize| {
                if f >= from {
                    (f as isize + delta) as usize
                } else {
                    f
                }
            };
            self.links.insert((l, shift(f)), (tl, shift(tf)));
        }
        let cels = &self.cels;
        self.links
            .retain(|k, t| cels.contains_key(k) && cels.contains_key(t));
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

    /// Add a named slice (a labelled canvas rect with optional 9-slice
    /// `center` guides and a `pivot` point), returning its
    /// index. Rects are inclusive corners `[x0, y0, x1, y1]` — the
    /// `copy_region` convention — and follow its policy too: corner order is
    /// normalised, the rect is clamped to the canvas, and one that intersects
    /// nothing is an error. `center` is clamped INTO the slice rect (9-slice
    /// guides outside their slice are meaningless); `pivot` is taken as-is
    /// (a rotation origin may legitimately sit off-canvas).
    pub fn add_slice(
        &mut self,
        name: &str,
        rect: [i32; 4],
        center: Option<[i32; 4]>,
        pivot: Option<[i32; 2]>,
    ) -> Result<usize, String> {
        if name.is_empty() {
            return Err("slice name must not be empty".into());
        }
        let (x0, y0, x1, y1) =
            raster::clamp_region(rect[0], rect[1], rect[2], rect[3], self.meta.w, self.meta.h)
                .ok_or("slice rect is empty after clamping to the canvas")?;
        let center = match center {
            Some(c) => {
                // The canvas clamp reused in rect-local coordinates:
                // normalise + clamp into the slice, then translate back.
                let (rw, rh) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
                let (cx0, cy0, cx1, cy1) =
                    raster::clamp_region(c[0] - x0, c[1] - y0, c[2] - x0, c[3] - y0, rw, rh)
                        .ok_or("slice center is empty after clamping into the slice rect")?;
                Some([cx0 + x0, cy0 + y0, cx1 + x0, cy1 + y0])
            }
            None => None,
        };
        let idx = self.meta.slices.len();
        self.meta.slices.push(SliceMeta {
            name: name.into(),
            rect: [x0, y0, x1, y1],
            center,
            pivot,
        });
        Ok(idx)
    }

    /// Delete every slice named `name`; errors when no slice has it. (Names
    /// are not policed for duplicates — same as tags — so one call clears
    /// them all.)
    pub fn delete_slice(&mut self, name: &str) -> Result<(), String> {
        let before = self.meta.slices.len();
        self.meta.slices.retain(|s| s.name != name);
        if self.meta.slices.len() == before {
            return Err(format!("no slice '{}'", name));
        }
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
        // Wholesale replace is an edit: copy-on-write breaks the link …
        self.links.remove(&(layer, frame));
        self.cels.insert((layer, frame), (x, y, img));
        self.mark_dirty(layer, frame);
        // … and the new pixels flow on to anything linked to this cel.
        self.propagate_link((layer, frame));
        Ok(())
    }

    /// Remove the cel at (layer, frame), if any.
    ///
    /// Validates the target like every sibling cel op: without this,
    /// `clear_cel(99, 0)` on a one-layer document reported success, so an agent
    /// clearing the wrong index was told the cel was empty and carried on.
    pub fn clear_cel(&mut self, layer: usize, frame: usize) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.links.remove(&(layer, frame));
        self.cels.remove(&(layer, frame));
        // Clearing a target is target loss: dependents materialise on their
        // snapshots (propagate finds the key gone and just unlinks them).
        self.propagate_link((layer, frame));
        // Not dirty (nothing to write) — its old file goes stale, which `save`
        // sweeps after the doc.json rename.
        self.dirty.remove(&(layer, frame));
        Ok(())
    }

    /// JSON snapshot of the document structure (layers, frames, tags, slices,
    /// cels, palette) for inspection — no pixel data.
    pub fn structure(&self) -> Value {
        let mut keys: Vec<(usize, usize)> = self.cels.keys().copied().collect();
        keys.sort_unstable();
        let cels: Vec<Value> = keys
            .into_iter()
            .map(|(l, f)| json!({"layer": l, "frame": f, "link": self.links.get(&(l, f))}))
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
            "slices": self.meta.slices.iter().map(|s| json!({
                "name": s.name, "rect": s.rect, "center": s.center, "pivot": s.pivot
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
        self.sync_links();
        // Copy-on-write: drawing into a linked cel breaks the link first —
        // the just-refreshed snapshot becomes its own pixels and the target
        // is never written through.
        self.links.remove(&(layer, frame));
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

    pub fn fill_cel(&mut self, layer: usize, frame: usize, color: [u8; 4]) -> Result<(), String> {
        self.check_cel(layer, frame)?;
        self.links.remove(&(layer, frame));
        let img = RgbaImage::from_pixel(self.meta.w, self.meta.h, Rgba(color));
        self.cels.insert((layer, frame), (0, 0, img));
        self.mark_dirty(layer, frame);
        self.propagate_link((layer, frame));
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

#[cfg(test)]
mod slice_link_tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atelier_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slice_meta_serde_round_trip() {
        let s = SliceMeta {
            name: "hud".into(),
            rect: [1, 2, 9, 8],
            center: Some([3, 4, 7, 6]),
            pivot: Some([5, 5]),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["rect"], json!([1, 2, 9, 8]));
        let back: SliceMeta = serde_json::from_value(v).unwrap();
        assert_eq!(back.name, "hud");
        assert_eq!(back.rect, [1, 2, 9, 8]);
        assert_eq!(back.center, Some([3, 4, 7, 6]));
        assert_eq!(back.pivot, Some([5, 5]));
        // center/pivot predate nothing either — absent means None.
        let minimal: SliceMeta =
            serde_json::from_value(json!({"name": "a", "rect": [0, 0, 1, 1]})).unwrap();
        assert_eq!(minimal.center, None);
        assert_eq!(minimal.pivot, None);
    }

    #[test]
    fn doc_meta_slices_serde_round_trip() {
        let mut d = Document::new("t", 8, 8);
        d.add_slice("hud", [0, 0, 7, 3], Some([2, 1, 5, 2]), None)
            .unwrap();
        let v = serde_json::to_value(&d.meta).unwrap();
        assert_eq!(v["slices"].as_array().unwrap().len(), 1);
        let back: DocMeta = serde_json::from_value(v).unwrap();
        assert_eq!(back.slices.len(), 1);
        assert_eq!(back.slices[0].rect, [0, 0, 7, 3]);
        // A DocMeta JSON written before slices existed still parses.
        let old: DocMeta = serde_json::from_value(
            json!({"name": "x", "w": 1, "h": 1, "layers": [], "frames": [], "cels": []}),
        )
        .unwrap();
        assert!(old.slices.is_empty());
    }

    #[test]
    fn cel_meta_link_serde_round_trip() {
        let c = CelMeta {
            layer: 0,
            frame: 1,
            x: 0,
            y: 0,
            file: "cels/L0_F0.png".into(),
            link: Some((0, 0)),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["link"], json!([0, 0]));
        let back: CelMeta = serde_json::from_value(v).unwrap();
        assert_eq!(back.link, Some((0, 0)));
        // The old format has no `link` key at all — and stays that way.
        let old: CelMeta = serde_json::from_value(
            json!({"layer": 0, "frame": 0, "x": 0, "y": 0, "file": "cels/L0_F0.png"}),
        )
        .unwrap();
        assert_eq!(old.link, None);
        let v = serde_json::to_value(&old).unwrap();
        assert!(v.get("link").is_none());
    }

    #[test]
    fn old_doc_json_without_slices_or_link_loads() {
        let dir = tmp_dir("oldfmt");
        std::fs::create_dir_all(dir.join("cels")).unwrap();
        RgbaImage::from_pixel(2, 2, Rgba([9, 8, 7, 255]))
            .save(dir.join("cels/L0_F0.png"))
            .unwrap();
        // Hand-written pre-slices/pre-link doc.json: no `slices`, no `link`.
        std::fs::write(
            dir.join("doc.json"),
            r#"{
                "name": "old", "w": 2, "h": 2,
                "layers": [{"name": "Layer 1", "opacity": 255, "visible": true, "blend": "normal"}],
                "frames": [{"duration_ms": 100}],
                "cels": [{"layer": 0, "frame": 0, "x": 0, "y": 0, "file": "cels/L0_F0.png"}]
            }"#,
        )
        .unwrap();
        let d = Document::load(&dir).unwrap();
        assert!(d.meta.slices.is_empty());
        assert!(d.links.is_empty());
        assert_eq!(d.meta.cels[0].link, None);
        assert_eq!(d.get_pixel(0, 0, 1, 1).unwrap(), [9, 8, 7, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_slice_validates_clamps_and_structures() {
        let mut d = Document::new("t", 16, 16);
        assert!(d.add_slice("", [0, 0, 3, 3], None, None).is_err());
        // Fully off-canvas intersects nothing → error, not a clamped sliver.
        assert!(d.add_slice("off", [20, 20, 30, 30], None, None).is_err());
        let i = d
            .add_slice("hud", [-4, 2, 20, 10], None, Some([1, 1]))
            .unwrap();
        assert_eq!(i, 0);
        assert_eq!(d.meta.slices[0].rect, [0, 2, 15, 10]);
        assert_eq!(d.meta.slices[0].pivot, Some([1, 1]));
        // Corner order is normalised; the center clamps INTO the slice rect.
        let j = d
            .add_slice("nine", [8, 8, 2, 2], Some([10, 10, 4, 4]), None)
            .unwrap();
        assert_eq!(d.meta.slices[j].rect, [2, 2, 8, 8]);
        assert_eq!(d.meta.slices[j].center, Some([4, 4, 8, 8]));
        assert!(d
            .add_slice("bad", [0, 0, 3, 3], Some([9, 9, 12, 12]), None)
            .is_err());
        let s = d.structure();
        assert_eq!(s["slices"].as_array().unwrap().len(), 2);
        assert_eq!(s["slices"][0]["name"], json!("hud"));
        assert_eq!(s["slices"][0]["rect"], json!([0, 2, 15, 10]));
    }

    #[test]
    fn delete_slice_by_name() {
        let mut d = Document::new("t", 8, 8);
        d.add_slice("a", [0, 0, 3, 3], None, None).unwrap();
        d.add_slice("b", [0, 0, 3, 3], None, None).unwrap();
        d.add_slice("a", [1, 1, 4, 4], None, None).unwrap();
        assert!(d.delete_slice("nope").is_err());
        d.delete_slice("a").unwrap(); // names aren't unique — both "a"s go
        assert_eq!(d.meta.slices.len(), 1);
        assert_eq!(d.meta.slices[0].name, "b");
    }

    #[test]
    fn linked_cels_save_and_load_round_trip() {
        let dir = tmp_dir("linkrt");
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [10, 20, 30, 255]).unwrap();
        d.duplicate_frame(0, true).unwrap();
        assert_eq!(d.links.len(), 1);
        d.save(&dir).unwrap();
        // The linked cel wrote no file; the target's PNG survives the sweep.
        assert!(dir.join("cels/L0_F0.png").is_file());
        assert!(!dir.join("cels/L0_F1.png").is_file());
        // doc.json records the link, pointing file at the target's PNG.
        let meta: DocMeta =
            serde_json::from_str(&std::fs::read_to_string(dir.join("doc.json")).unwrap()).unwrap();
        let linked = meta.cels.iter().find(|c| c.frame == 1).unwrap();
        assert_eq!(linked.link, Some((0, 0)));
        assert_eq!(linked.file, "cels/L0_F0.png");
        // Loading re-resolves from the target's pixels…
        let mut back = Document::load(&dir).unwrap();
        assert_eq!(back.links.len(), 1);
        assert_eq!(back.get_pixel(0, 1, 0, 0).unwrap(), [10, 20, 30, 255]);
        // …and the link is live again: target edits show through.
        back.fill_cel(0, 0, [1, 2, 3, 255]).unwrap();
        assert_eq!(back.get_pixel(0, 1, 0, 0).unwrap(), [1, 2, 3, 255]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_rejects_link_chains_and_missing_targets() {
        let dir = tmp_dir("linkbad");
        std::fs::create_dir_all(dir.join("cels")).unwrap();
        RgbaImage::from_pixel(1, 1, Rgba([1, 1, 1, 255]))
            .save(dir.join("cels/L0_F0.png"))
            .unwrap();
        let doc = |cels: Value| {
            json!({
                "name": "t", "w": 1, "h": 1,
                "layers": [{"name": "L", "opacity": 255, "visible": true, "blend": "normal"}],
                "frames": [{"duration_ms": 100}, {"duration_ms": 100}, {"duration_ms": 100}],
                "cels": cels,
            })
        };
        // A chain: F1 links to F2, which is itself linked — refused.
        std::fs::write(
            dir.join("doc.json"),
            serde_json::to_string_pretty(&doc(json!([
                {"layer": 0, "frame": 0, "x": 0, "y": 0, "file": "cels/L0_F0.png"},
                {"layer": 0, "frame": 2, "x": 0, "y": 0, "file": "cels/L0_F0.png", "link": [0, 0]},
                {"layer": 0, "frame": 1, "x": 0, "y": 0, "file": "cels/L0_F0.png", "link": [0, 2]}
            ])))
            .unwrap(),
        )
        .unwrap();
        match Document::load(&dir) {
            Err(e) => assert!(
                e.contains("missing or itself linked"),
                "unexpected error: {e}"
            ),
            Ok(_) => panic!("a link chain must be refused"),
        }
        // A link at nothing — refused.
        std::fs::write(
            dir.join("doc.json"),
            serde_json::to_string_pretty(&doc(json!([
                {"layer": 0, "frame": 0, "x": 0, "y": 0, "file": "cels/L0_F0.png"},
                {"layer": 0, "frame": 1, "x": 0, "y": 0, "file": "cels/L0_F0.png", "link": [0, 2]}
            ])))
            .unwrap(),
        )
        .unwrap();
        assert!(Document::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_on_write_breaks_link_and_spares_target() {
        let dir = tmp_dir("cow");
        let mut d = Document::new("t", 4, 4);
        d.fill_cel(0, 0, [10, 20, 30, 255]).unwrap();
        d.duplicate_frame(0, true).unwrap();
        // A canvas-level edit on the LINKED cel (the cel_canvas path).
        d.pencil(0, 1, &[(0, 0)], [200, 0, 0, 255], 1).unwrap();
        // Link broken; the target is untouched; the snapshot became its own.
        assert!(d.links.is_empty());
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [10, 20, 30, 255]);
        assert_eq!(d.get_pixel(0, 1, 0, 0).unwrap(), [200, 0, 0, 255]);
        assert_eq!(d.get_pixel(0, 1, 1, 1).unwrap(), [10, 20, 30, 255]);
        // Later target edits no longer show through…
        d.fill_cel(0, 0, [1, 1, 1, 255]).unwrap();
        assert_eq!(d.get_pixel(0, 1, 1, 1).unwrap(), [10, 20, 30, 255]);
        // …and the materialised cel is a real cel on disk after save.
        d.save(&dir).unwrap();
        assert!(dir.join("cels/L0_F1.png").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flatten_resolves_linked_pixels() {
        let mut d = Document::new("t", 4, 4);
        d.pencil(0, 0, &[(1, 1)], [5, 6, 7, 255], 1).unwrap();
        d.duplicate_frame(0, true).unwrap();
        assert_eq!(d.flatten(0), d.flatten(1));
        assert_eq!(d.flatten(1).get_pixel(1, 1).0, [5, 6, 7, 255]);
        // structure() shows the sharing: source has no link, the copy does.
        let s = d.structure();
        let cels = s["cels"].as_array().unwrap();
        assert_eq!(cels[0]["link"], json!(null));
        assert_eq!(cels[1]["link"], json!([0, 0]));
    }

    #[test]
    fn delete_layer_materializes_link_to_lost_target() {
        let mut d = Document::new("t", 4, 4);
        d.add_layer(Some("upper".into()), 255, "normal".into());
        d.fill_cel(1, 0, [3, 3, 3, 255]).unwrap();
        d.fill_cel(0, 0, [7, 7, 7, 255]).unwrap();
        // Hand-link layer 0's cel to layer 1's (duplicate_frame only links
        // within a layer; a cross-layer link exercises the layer remap).
        d.links.insert((0, 0), (1, 0));
        d.sync_links();
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [3, 3, 3, 255]);
        d.delete_layer(1).unwrap();
        assert!(d.links.is_empty()); // materialised, not dangling
        assert_eq!(d.get_pixel(0, 0, 0, 0).unwrap(), [3, 3, 3, 255]);
    }

    #[test]
    fn move_layer_retargets_links() {
        let mut d = Document::new("t", 4, 4);
        d.add_layer(None, 255, "normal".into());
        d.add_layer(None, 255, "normal".into());
        d.fill_cel(0, 0, [1, 1, 1, 255]).unwrap();
        d.fill_cel(1, 0, [2, 2, 2, 255]).unwrap();
        d.links.insert((0, 0), (1, 0));
        // Bottom layer moves to the top: 2→0, 0→1, 1→2 — the link follows.
        d.move_layer(2, 0).unwrap();
        assert_eq!(d.links.get(&(1, 0)), Some(&(2, 0)));
        d.fill_cel(2, 0, [9, 9, 9, 255]).unwrap();
        assert_eq!(d.get_pixel(1, 0, 0, 0).unwrap(), [9, 9, 9, 255]);
    }
}
