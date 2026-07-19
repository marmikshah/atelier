//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by its `id` (a slug derived from its name).
//!
//! This module is the facade: the `Studio` struct, the structure/timeline and
//! per-cel ops, and the shared helpers. The store/journal lives in `store`,
//! file exports in `ops_export`, selection/clipboard in `ops_region`, and the
//! themed readers/crafters in their own modules.

// Drawing/region ops are inherently coordinate-heavy (layer, frame, x0..y1,
// colour, …); the argument-count lint fights the domain here.
#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use atelier_core::document::Document;

mod analysis;
mod craft;
mod ops_export;
mod ops_region;
mod reference;
mod set;
mod store;
mod view;
pub use view::LookOptions;

use ops_region::{Clip, Selection};

/// Hard cap on the pixel count (w×h) of an external source image. ~64 MP covers
/// any real reference/photo; the point is that a tiny-on-disk "decompression
/// bomb" (a 30000×30000 PNG is a few KB) is rejected at the header probe, before
/// its pixels are ever allocated. Shared by every `open_bounded` caller.
pub(crate) const MAX_IMPORT_PIXELS: u64 = 64 * 1024 * 1024;

/// Hard canvas ceiling: width/height a document may have (also the bound the
/// import/reference paths assume when sizing buffers).
pub(crate) const MAX_CANVAS: u32 = 4096;
/// A document's journal, beside its `doc.json` and `cels/`.
pub const JOURNAL_FILE: &str = "recipe.jsonl";
/// Text grids (silhouette/dump/diff) stay readable only so long — shared area
/// cap for every grid-emitting reader.
pub(crate) const GRID_AREA_CAP: u64 = 4096;
/// Import/reference targets above this allocate unbounded images in one call.
pub(crate) const MAX_TARGET_PIXELS: usize = 1_048_576;

/// Upper bound on an export scale factor. Canvases are already capped at 4096²
/// (`doc_create`); without a scale ceiling a `scale=64` export of that targets a
/// ~256 GB buffer. 16 matches the render/preview clamp.
pub(crate) const MAX_EXPORT_SCALE: u32 = 16;

/// Export scale when the caller leaves it unset.
pub(crate) const DEFAULT_EXPORT_SCALE: u32 = 4;

/// Clamp an export scale into `1..=MAX_EXPORT_SCALE`.
pub(crate) fn export_scale(scale: u32) -> u32 {
    scale.clamp(1, MAX_EXPORT_SCALE)
}

/// Open an external image with a size cap. The header dimensions are read first
/// and anything over [`MAX_IMPORT_PIXELS`] is rejected *before* decoding, and
/// the decoder is then bounded to those dimensions so a lying header can't
/// allocate past them either — an OOM / decompression-bomb guard for every
/// path that ingests a caller-supplied image (import, references, stamp).
/// Reject source dimensions whose pixel count exceeds [`MAX_IMPORT_PIXELS`].
/// Extracted so the cap is unit-testable without materialising a huge file.
fn check_import_dims(w: u32, h: u32) -> Result<(), String> {
    let px = w as u64 * h as u64;
    if px > MAX_IMPORT_PIXELS {
        return Err(format!(
            "source image is {w}x{h} = {px} px, over the {MAX_IMPORT_PIXELS}-px import cap"
        ));
    }
    Ok(())
}

pub(crate) fn open_bounded(path: &Path) -> Result<image::RgbaImage, String> {
    let dims = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    let (w, h) = dims;
    check_import_dims(w, h)?;
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(w.max(1));
    limits.max_image_height = Some(h.max(1));
    reader.limits(limits);
    Ok(reader.decode().map_err(|e| e.to_string())?.to_rgba8())
}

/// Public because `atelier replay` predicts the id an authored recipe's
/// `doc_create` would have minted (pre-journal recipes carry no minted id).
pub fn slugify(name: &str) -> String {
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

/// A by-colour selection request: which cel to read, an explicit target colour
/// or a sample point to read it from, and the channel-distance tolerance.
pub struct ColorSelect {
    pub layer: usize,
    pub frame: usize,
    pub color: Option<[u8; 4]>,
    pub sample: Option<(i32, i32)>,
    pub tol: i32,
}

#[derive(Clone)]
pub struct Studio {
    docs_dir: PathBuf,
    /// Cross-cel / cross-document clipboard for copy/cut → paste. Lives for the
    /// process; one shared studio means one shared clipboard across sessions.
    clipboard: Option<Clip>,
    /// Active selection mask (at most one), set by `doc_select`; painting ops
    /// confine to it. Process-lived, like the clipboard.
    selection: Option<Selection>,
}

impl Studio {
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
        if !atelier_core::raster::valid_blend(&blend) {
            return Err(format!(
                "unknown blend '{blend}' — valid: {}",
                atelier_core::raster::BLEND_NAMES
            ));
        }
        let (dir, mut doc) = self.open(id)?;
        let idx = doc.add_layer(name, opacity, blend);
        doc.save(&dir)?;
        // Slim ack — echoing the whole structure() grew O(layers×frames) per
        // call; doc_info still serves the full picture on demand.
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "added_layer": idx,
            "layers": doc.meta().layers.len(),
        }))
    }

    pub fn doc_set_layer(
        &self,
        id: &str,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<Value, String> {
        if let Some(b) = &blend {
            if !atelier_core::raster::valid_blend(b) {
                return Err(format!(
                    "unknown blend '{b}' — valid: {}",
                    atelier_core::raster::BLEND_NAMES
                ));
            }
        }
        let (dir, mut doc) = self.open(id)?;
        doc.set_layer(layer, visible, opacity, blend)?;
        self.commit(&dir, id, doc)
    }

    /// One-tool dispatch over layer structure — `op`: `add` (new layer on top) |
    /// `set` (visibility/opacity/blend of layer `index`) | `move` | `insert` |
    /// `delete` | `rename` | `duplicate` | `merge_down`. Routes to the kept
    /// `doc_add_layer` / `doc_set_layer` / `layer_ops` methods.
    /// Destructive dispatch ops must say WHICH target they hit — a defaulted
    /// index 0 silently deletes/mutates the first layer/frame.
    fn required_index(op: &str, index: Option<usize>) -> Result<usize, String> {
        index.ok_or_else(|| format!("op '{op}' needs an explicit index"))
    }

    pub fn doc_layer(
        &self,
        id: &str,
        op: &str,
        index: Option<usize>,
        to_index: Option<usize>,
        name: Option<String>,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<String>,
    ) -> Result<Value, String> {
        match op {
            "add" => self.doc_add_layer(
                id,
                name,
                opacity.unwrap_or(255),
                blend.unwrap_or_else(|| "normal".into()),
            ),
            "set" => self.doc_set_layer(
                id,
                Self::required_index(op, index)?,
                visible,
                opacity,
                blend,
            ),
            _ => self.layer_ops(
                id,
                op,
                Self::required_index(op, index)?,
                to_index,
                name,
                opacity.unwrap_or(255),
                blend.unwrap_or_else(|| "normal".into()),
            ),
        }
    }

    pub fn doc_add_frame(
        &self,
        id: &str,
        duration_ms: u32,
        copy_from: Option<usize>,
        count: usize,
    ) -> Result<Value, String> {
        let count = count.clamp(1, 256);
        let (dir, mut doc) = self.open(id)?;
        if let Some(src) = copy_from {
            if src >= doc.meta().frames.len() {
                return Err(format!(
                    "copy_from {src} out of range — document has {} frame(s) (0..={})",
                    doc.meta().frames.len(),
                    doc.meta().frames.len().saturating_sub(1)
                ));
            }
        }
        // Appending N identical frames used to cost N round-trips; `count`
        // makes "give me my 10 frames" one call.
        let mut idx = doc.add_frame(duration_ms, copy_from);
        for _ in 1..count {
            idx = doc.add_frame(duration_ms, copy_from);
        }
        doc.save(&dir)?;
        // Slim ack — echoing the whole structure() grew O(layers×frames) per
        // call during walk-cycle work; doc_info has the full picture.
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "added_frame": idx,
            "frames": doc.meta().frames.len(),
        }))
    }

    pub fn doc_set_frame_duration(&self, id: &str, frame: usize, ms: u32) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_frame_duration(frame, ms)?;
        self.commit(&dir, id, doc)
    }

    /// One-tool dispatch over frame lifecycle + timing — `op`: `add` (append,
    /// optional `copy_from`) | `duration` (set frame `frame`'s ms) | `delete` |
    /// `insert` | `duplicate` | `move`. Routes to the kept `doc_add_frame` /
    /// `doc_set_frame_duration` / `doc_frame_ops`. (Pivot, boxes, tags and
    /// keyframe motion keep their own tools.)
    pub fn doc_frame(
        &self,
        id: &str,
        op: &str,
        frame: Option<usize>,
        copy_from: Option<usize>,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
        count: Option<usize>,
    ) -> Result<Value, String> {
        match op {
            "add" => self.doc_add_frame(
                id,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
                copy_from,
                count.unwrap_or(1),
            ),
            "duration" => self.doc_set_frame_duration(
                id,
                Self::required_index(op, frame)?,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
            ),
            _ => self.doc_frame_ops(
                id,
                op,
                Self::required_index(op, frame)?,
                to_index,
                duration_ms,
            ),
        }
    }

    pub fn doc_add_tag(
        &self,
        id: &str,
        name: &str,
        from: usize,
        to: usize,
        direction: &str,
    ) -> Result<Value, String> {
        if !matches!(direction, "forward" | "reverse" | "pingpong") {
            return Err(format!(
                "unknown tag direction '{direction}' — valid: forward | reverse | pingpong"
            ));
        }
        let (dir, mut doc) = self.open(id)?;
        doc.add_tag(name, from, to, direction)?;
        self.commit(&dir, id, doc)
    }

    pub fn doc_clear_cel(&self, id: &str, layer: usize, frame: usize) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.clear_cel(layer, frame)?;
        self.commit(&dir, id, doc)
    }

    // -- render ---------------------------------------------------------------

    /// Flatten one frame and encode it straight to PNG bytes in memory (no file).
    /// Backs the MCP `render` resource, which serves the bytes as a blob.
    pub fn render_png_bytes(&self, id: &str, frame: usize, scale: u32) -> Result<Vec<u8>, String> {
        let (_dir, doc) = self.open(id)?;
        if frame >= doc.meta().frames.len() {
            return Err(format!(
                "no frame {} (frames={})",
                frame,
                doc.meta().frames.len()
            ));
        }
        let img = doc.render_preview(frame, scale.max(1), None, false, 1, None)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(buf.into_inner())
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

    /// Diff two canvas-sized snapshots into a mutation ack: pixel count + bbox
    /// of every change, and an explicit warning when NOTHING changed — the
    /// usual symptom of coordinates that ran off-canvas or hit the wrong cel.
    /// Blind `{ok:true}` acks let those mistakes compound between renders.
    fn change_ack(id: &str, before: &image::RgbaImage, after: &image::RgbaImage) -> Value {
        let (mut changed, mut bbox): (u64, Option<[u32; 4]>) = (0, None);
        for (b, (x, y, a)) in before.pixels().zip(after.enumerate_pixels()) {
            if b != a {
                changed += 1;
                bbox = Some(match bbox {
                    None => [x, y, x, y],
                    Some([a0, b0, c0, d0]) => [a0.min(x), b0.min(y), c0.max(x), d0.max(y)],
                });
            }
        }
        let mut out = json!({
            "ok": true,
            "doc_id": id,
            "pixels_changed": changed,
            "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
        });
        if changed == 0 {
            out["warning"] =
                json!("no pixels changed — coordinates may be off-canvas, the edit may match what's already there (a no-op, not a failure), or the selection may exclude the area");
        }
        out
    }

    /// Like `edit`, but if an active selection covers this document the op `f`
    /// is confined to the selected pixels. Used by the painting ops so
    /// `doc_select` masks any of them. A stale selection (dims mismatch) is an
    /// error, never a silent unmasked apply. Returns a change ack (pixel
    /// count + bbox) instead of a blind ok.
    fn edit_masked<F>(&self, id: &str, layer: usize, frame: usize, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        let before = doc.cel_full(layer, frame);
        match self.selection_mask_for(id, doc.meta().w, doc.meta().h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, f)?,
            None => f(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        Ok(Self::change_ack(id, &before, &after))
    }

    /// Timeline lifecycle (delete | insert | duplicate | move) with cel
    /// reindexing and tag remapping — the recovery path for a bad tween.
    pub fn doc_frame_ops(
        &self,
        id: &str,
        action: &str,
        frame: usize,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let out = doc.frame_ops(action, frame, to_index, duration_ms)?;
        doc.save(&dir)?;
        Ok(out)
    }

    /// Best-effort auto-checkpoint before a destructive op, labelled
    /// `auto:<tool>`, keeping only the newest few auto snapshots so repeated
    /// ops don't grow the doc dir without bound. Never fails the caller.
    pub fn auto_checkpoint(&self, id: &str, tool: &str) {
        const KEEP: usize = 5;
        let label = format!("auto:{}", tool);
        let _ = self.checkpoint(id, "save", Some(&label), None);
        if let Ok(list) = self.checkpoint(id, "list", None, None) {
            let mut autos: Vec<String> = list["checkpoints"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| c["label"].as_str().is_some_and(|l| l.starts_with("auto:")))
                        .filter_map(|c| c["id"].as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // Numeric order — lexical sort would call cp10 older than cp2.
            autos.sort_by_key(|s| {
                s.strip_prefix("cp")
                    .and_then(|t| t.parse::<u32>().ok())
                    .unwrap_or(0)
            });
            if autos.len() > KEEP {
                for cpid in &autos[..autos.len() - KEEP] {
                    let _ = self.checkpoint(id, "prune", None, Some(cpid));
                }
            }
        }
    }

    // -- palette -------------------------------------------------------------

    pub fn doc_set_palette(&self, id: &str, colors: Vec<[u8; 4]>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_palette(colors);
        let mut out = self.commit(&dir, id, doc)?;
        out["palette_set"] = json!(true);
        Ok(out)
    }

    /// Recolour paired `from`→`to` colours across the whole document (one sprite,
    /// many palettes) in a single open→save cycle, also updating the stored
    /// palette. `from`/`to` must be the same non-empty length.
    pub fn doc_palette_swap(
        &self,
        id: &str,
        from: Vec<[u8; 4]>,
        to: Vec<[u8; 4]>,
        layer: Option<usize>,
        frame: Option<usize>,
    ) -> Result<Value, String> {
        if from.is_empty() || from.len() != to.len() {
            return Err("from/to must be non-empty and the same length".into());
        }
        let pairs: Vec<([u8; 4], [u8; 4])> = from.into_iter().zip(to).collect();
        let (dir, mut doc) = self.open(id)?;
        let changed = doc.palette_swap(&pairs, layer, frame);
        doc.save(&dir)?;
        Ok(json!({"doc_id": id, "changed": changed}))
    }

    /// Apply many ordered drawing ops to one cel in a single open→save cycle.
    /// Declarative grid painting: legend (char -> colour or palette index) +
    /// row strings paint a whole region in one call. Palette-index legends are
    /// palette-true by construction. Honours an active selection.
    pub fn doc_paint_grid(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        legend: serde_json::Map<String, Value>,
        rows: Vec<String>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let palette = doc.meta().palette.clone();
        let (dw, dh) = (doc.meta().w, doc.meta().h);
        drop(doc);
        let mut map = std::collections::HashMap::new();
        for (k, v) in &legend {
            let mut chars = k.chars();
            let ch = match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => return Err(format!("legend key '{}' must be a single character", k)),
            };
            if ch == '.' || ch == ' ' {
                return Err("'.' and ' ' are reserved for 'leave untouched'".into());
            }
            let color = match v {
                Value::Number(n) => {
                    let i = n.as_u64().ok_or_else(|| {
                        format!(
                            "legend '{}': palette index must be a non-negative integer",
                            k
                        )
                    })? as usize;
                    *palette.get(i).ok_or_else(|| {
                        format!(
                            "legend '{}': palette index {} out of range (palette has {})",
                            k,
                            i,
                            palette.len()
                        )
                    })?
                }
                Value::Array(_) => {
                    // Strict like validate_batch_op's colour check: exactly
                    // [r,g,b] or [r,g,b,a], every component 0..=255 — one shared
                    // predicate in core. The old parse dropped non-numbers
                    // silently and truncated via `as u8` (300 → 44) into a
                    // wrong-but-plausible colour.
                    match atelier_core::document::color_array(v) {
                        Some(c) => c,
                        None => {
                            return Err(format!(
                                "legend '{}': colour must be [r,g,b] or [r,g,b,a] with 0..=255 values, got {}",
                                k, v
                            ))
                        }
                    }
                }
                _ => {
                    return Err(format!(
                        "legend '{}': value must be [r,g,b(,a)] or a palette index",
                        k
                    ))
                }
            };
            map.insert(ch, color);
        }
        let counts = std::cell::Cell::new((0u64, 0u64));
        let mut ack = self.edit_masked(id, layer, frame, |d| {
            counts.set(d.paint_grid(layer, frame, x, y, &map, &rows)?);
            Ok(())
        })?;
        let (mut painted, clipped) = counts.get();
        // Under an active selection, edit_masked reverts cells the mask
        // excludes AFTER paint_grid counted them — recount so `painted`
        // reports what actually landed (dims captured above; no third load).
        {
            if let Some(mask) = self.selection_mask_for(id, dw, dh)? {
                let (dwi, dhi) = (dw as i32, dh as i32);
                let (mut kept, mut masked) = (0u64, 0u64);
                for (ry, row) in rows.iter().enumerate() {
                    for (rx, ch) in row.chars().enumerate() {
                        if ch == '.' || ch == ' ' {
                            continue;
                        }
                        let (tx, ty) = (x + rx as i32, y + ry as i32);
                        if tx < 0 || ty < 0 || tx >= dwi || ty >= dhi {
                            continue;
                        }
                        match mask.get((ty * dwi + tx) as usize).copied() {
                            Some(true) => kept += 1,
                            _ => masked += 1,
                        }
                    }
                }
                if masked > 0 {
                    painted = kept;
                    ack["masked"] = json!(masked);
                    ack["warning"] = json!(format!(
                        "{} grid cells fell inside the canvas but outside the active \
                         selection and were not painted",
                        masked
                    ));
                }
            }
        }
        ack["painted"] = json!(painted);
        if clipped > 0 {
            ack["clipped"] = json!(clipped);
            ack["warning"] = json!(format!(
                "{} grid cells fell outside the canvas — check x/y and row widths",
                clipped
            ));
        }
        Ok(ack)
    }

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
            atelier_core::document::validate_batch_op(i, op)?;
        }
        let run = |doc: &mut Document| -> Result<(), String> {
            for (i, op) in ops.iter().enumerate() {
                doc.apply_op(layer, frame, op)
                    .map_err(|e| format!("op {}: {}", i, e))?;
            }
            Ok(())
        };
        let before = doc.cel_full(layer, frame);
        match self.selection_mask_for(id, doc.meta().w, doc.meta().h)? {
            Some(mask) => doc.apply_masked(layer, frame, mask, run)?,
            None => run(&mut doc)?,
        }
        let after = doc.cel_full(layer, frame);
        doc.save(&dir)?;
        let mut ack = Self::change_ack(id, &before, &after);
        ack["ops"] = json!(ops.len());
        Ok(ack)
    }

    /// Apply ONE drawing op to a cel — the single-op form of [`Self::doc_batch`],
    /// scoped to the "add marks" vocabulary (geometry, fills, text, procedural).
    /// `params` is the op's flattened args; the op name is injected and the call
    /// routes through the same validate-and-apply path a one-element batch uses,
    /// so there is one source of truth for the op schema and behaviour.
    pub fn doc_draw(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        op: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let draw_ops = atelier_core::document::draw_ops();
        if !draw_ops.contains(&op) {
            return Err(format!(
                "doc_draw: '{op}' is not a draw op — use one of [{}] (filters and lighting live on their own tools)",
                draw_ops.join(", ")
            ));
        }
        params.insert("op".into(), json!(op));
        self.doc_batch(id, layer, frame, vec![Value::Object(params)])
    }

    /// Apply ONE transform/effect op to a cel — the single-op form of
    /// [`doc_batch`](Self::doc_batch) for the ops that REWORK existing pixels
    /// (filters, lighting, colour, geometry); the complement of
    /// [`doc_draw`](Self::doc_draw), which adds new marks. Same validated dispatch.
    /// (`glow` is batch-only — its on-palette `snap` is not a single-op form.)
    pub fn doc_fx(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        op: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let fx_ops = atelier_core::document::fx_ops();
        if !fx_ops.contains(&op) {
            return Err(format!(
                "doc_fx: '{op}' is not an fx op — use one of [{}] (drawing marks → doc_draw; glow is a batch-only op — call it inside doc_batch)",
                fx_ops.join(", ")
            ));
        }
        params.insert("op".into(), json!(op));
        self.doc_batch(id, layer, frame, vec![Value::Object(params)])
    }
}

/// Adaptive preview scale: aim for ~384px on the longest side (big enough for a
/// vision model to judge sprite-scale detail), clamped to 1..=16.
pub(crate) fn preview_scale(w: u32, h: u32) -> u32 {
    (384 / w.max(h).max(1)).clamp(1, 16)
}

/// `#rrggbb` — the one place the report hex format lives.
pub(crate) fn hex_rgb(c: &[u8]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// `#rrggbbaa` — hex with alpha, for translucency-aware reports.
pub(crate) fn hex_rgba(c: &[u8]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c[0], c[1], c[2], c[3])
}

/// Nearest-neighbour upscale (keeps the pixel grid crisp).
///
/// The scale is clamped to `MAX_EXPORT_SCALE` here rather than at each of the
/// nine call sites: `doc_look`, `select_render`, `contact_sheet` and the diff
/// overlays all took it straight from the caller, and `scale: 1000000000`
/// overflowed the dimension multiply — a panic in debug, a multi-terabyte
/// allocation in release, and either way a poisoned studio lock that broke every
/// later call. One choke point cannot be forgotten.
pub(crate) fn scale_nn(img: &image::RgbaImage, scale: u32) -> image::RgbaImage {
    let scale = export_scale(scale);
    if scale <= 1 {
        return img.clone();
    }
    image::imageops::resize(
        img,
        img.width() * scale,
        img.height() * scale,
        image::imageops::FilterType::Nearest,
    )
}

/// Encode an RGBA image to in-memory PNG bytes.
pub(crate) fn encode_png(img: &image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-test-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn frame_add_count_appends_in_one_call() {
        // "Give me my 10 frames" used to cost 9 identical round-trips.
        let s = studio("addcount");
        s.doc_create("d", 4, 4).unwrap();
        let out = s.doc_frame("d", "add", None, Some(0), None, Some(80), Some(9));
        assert!(out.is_ok(), "{out:?}");
        let info = s.doc_info("d").unwrap();
        assert_eq!(info["frames"].as_array().map(|f| f.len()), Some(10));
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use std::fs;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-hard-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    #[test]
    fn paint_grid_legend_rejects_out_of_range_colours() {
        let s = studio("legend");
        s.doc_create("d", 4, 4).unwrap();
        let mk = |color: Value| {
            let mut legend = serde_json::Map::new();
            legend.insert("k".into(), color);
            s.doc_paint_grid("d", 0, 0, 0, 0, legend, vec!["k".into()])
        };
        // 300 used to truncate to 44 via `as u8`; now it's a loud error.
        let e = mk(json!([255, 300, 0])).unwrap_err();
        assert!(e.contains("0..=255"), "got: {e}");
        assert!(mk(json!([255, 128, 0])).is_ok());
        assert!(mk(json!([255, 128, 0, 200])).is_ok());
        assert!(mk(json!([1, 2])).is_err(), "too short must error");
        assert!(mk(json!([1, 2, 3, 4, 5])).is_err(), "too long must error");
    }
}
