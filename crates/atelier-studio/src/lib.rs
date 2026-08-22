//! The document store: a flat library of editable pixel-art documents.
//!
//! State lives under ~/.atelier (override with ATELIER_HOME). Each document
//! is a directory `documents/<id>/` with a `doc.json` (structure + cel refs) and
//! one PNG per cel under `cels/`. There is no project/grouping layer — a document
//! is the unit, addressed by the opaque `doc_id` minted when it is created.
//!
//! This module is the facade: the `Studio` struct, the structure/timeline and
//! per-cel ops, and the shared helpers. The store/journal lives in `store`,
//! file exports in `ops_export`, rectangular edits in `ops_region`, and the
//! themed readers/crafters in their own modules.

// Drawing/region ops are inherently coordinate-heavy (layer, frame, x0..y1,
// colour, …); the argument-count lint fights the domain here.
#![allow(clippy::too_many_arguments)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use atelier_core::document::{Document, FrameAction, TagDirection};
use atelier_core::raster::Blend;

mod analysis;
mod archive;
mod checkpoint;
mod control;
mod craft;
mod integrity;
mod ops_export;
mod ops_region;
mod reference;
mod renameat2;
mod store;
mod transaction;
mod view;
pub use control::{
    AlphaMode, AnimAuditMode, AnimationFormat, CheckpointAction, CompareMode, DiffRender,
    DocumentId, DumpMode, ExportOp, FrameOp, LayerOp, LookBackground, LookMode, PaletteOp,
    PaletteScheme, ReferenceOp, RegionOp, SeamAxis, SheetMeta, ToolName,
};
pub use integrity::{IntegrityIssue, IntegritySeverity, StoreIntegrityReport};
pub use store::{JOURNAL_FORMAT_VERSION, JournalEntry, validate_journal};
pub use transaction::{CommitOutcome, StoreTransaction};
pub use view::LookOptions;

/// Hard cap on the pixel count (w×h) of an external source image. ~64 MP covers
/// any real reference/photo; the point is that a tiny-on-disk "decompression
/// bomb" (a 30000×30000 PNG is a few KB) is rejected at the header probe, before
/// its pixels are ever allocated. Shared by every `open_bounded` caller.
pub(crate) const MAX_SOURCE_PIXELS: u64 = 64 * 1024 * 1024;

/// Hard canvas ceiling: owned by the persisted core contract so creation and
/// loading cannot disagree.
pub(crate) const MAX_CANVAS: u32 = atelier_core::document::MAX_DOCUMENT_DIMENSION;
/// A document's journal, beside its `doc.json` and `cels/`.
pub const JOURNAL_FILE: &str = "recipe.jsonl";
/// Store-owned optimistic-concurrency generation, beside `doc.json`.
///
/// This stays outside the core document format: older 1.x documents have no
/// sidecar and therefore begin at revision zero when first opened by a build
/// that supports guarded writes.
pub const REVISION_FILE: &str = "revision";
/// Text grids (silhouette/dump/diff) stay readable only so long — shared area
/// cap for every grid-emitting reader.
pub(crate) const GRID_AREA_CAP: u64 = 4096;
/// Reference-analysis targets above this allocate unbounded images in one call.
pub(crate) const MAX_TARGET_PIXELS: usize = 1_048_576;

/// Upper bound on an export scale factor. Canvases are already capped at 4096²
/// (`doc_new`); without a scale ceiling a `scale=64` export of that targets a
/// ~256 GB buffer. 16 matches the render/preview clamp.
pub(crate) const MAX_EXPORT_SCALE: u32 = 16;

/// Export scale when the caller leaves it unset.
pub(crate) const DEFAULT_EXPORT_SCALE: u32 = 4;

/// Largest consecutive frame count accepted by one timeline call.
const MAX_FRAME_COUNT: usize = 256;

/// Aggregate full-canvas work allowed for one ranged draw/effect call.
///
/// This equals one maximum-size document canvas, so a range never scans more
/// pixels than the largest already-supported single-frame edit.
const MAX_FRAME_EDIT_PIXELS: u64 = MAX_CANVAS as u64 * MAX_CANVAS as u64;

/// Clamp an export scale into `1..=MAX_EXPORT_SCALE`.
pub(crate) fn export_scale(scale: u32) -> u32 {
    scale.clamp(1, MAX_EXPORT_SCALE)
}

/// Open an external image with a size cap. The header dimensions are read first
/// and anything over [`MAX_SOURCE_PIXELS`] is rejected *before* decoding, and
/// the decoder is then bounded to those dimensions so a lying header can't
/// allocate past them either — an OOM / decompression-bomb guard for every
/// path that ingests a caller-supplied reference image.
/// Reject source dimensions whose pixel count exceeds [`MAX_SOURCE_PIXELS`].
/// Extracted so the cap is unit-testable without materialising a huge file.
fn check_source_dims(w: u32, h: u32) -> Result<(), String> {
    let px = w as u64 * h as u64;
    if px > MAX_SOURCE_PIXELS {
        return Err(format!(
            "source image is {w}x{h} = {px} px, over the {MAX_SOURCE_PIXELS}-px safety cap"
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
    check_source_dims(w, h)?;
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

#[derive(Clone)]
pub struct Studio {
    docs_dir: PathBuf,
}

impl Studio {
    // -- structure / timeline (open -> mutate -> save) ----------------------

    fn doc_add_layer(
        &self,
        id: &str,
        name: Option<String>,
        opacity: u8,
        blend: Blend,
    ) -> Result<Value, String> {
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

    fn doc_set_layer(
        &self,
        id: &str,
        layer: usize,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<Blend>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.set_layer(layer, visible, opacity, blend)?;
        doc.save(&dir)?;
        let current = &doc.meta().layers[layer];
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "layer": layer,
            "visible": current.visible,
            "opacity": current.opacity,
            "blend": current.blend.as_str(),
        }))
    }

    /// One-tool dispatch over layer structure — `op`: `add` (new layer on top) |
    /// `set` (visibility/opacity/blend of layer `index`) | `move` | `insert` |
    /// `delete` | `rename` | `duplicate` | `merge_down`. Routes to the kept
    /// `doc_add_layer` / `doc_set_layer` / `layer_ops` methods.
    /// Destructive dispatch ops must say WHICH target they hit — a defaulted
    /// index 0 silently deletes/mutates the first layer/frame.
    fn required_index(op: impl std::fmt::Display, index: Option<usize>) -> Result<usize, String> {
        index.ok_or_else(|| format!("op '{op}' needs an explicit index"))
    }

    pub fn doc_layer(
        &self,
        id: &str,
        op: LayerOp,
        index: Option<usize>,
        to_index: Option<usize>,
        name: Option<String>,
        visible: Option<bool>,
        opacity: Option<u8>,
        blend: Option<Blend>,
    ) -> Result<Value, String> {
        match op {
            LayerOp::Add => {
                self.doc_add_layer(id, name, opacity.unwrap_or(255), blend.unwrap_or_default())
            }
            LayerOp::Set => self.doc_set_layer(
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
                blend.unwrap_or_default(),
            ),
        }
    }

    pub(crate) fn doc_add_frame(
        &self,
        id: &str,
        duration_ms: u32,
        copy_from: Option<usize>,
        count: usize,
    ) -> Result<Value, String> {
        let count = Self::checked_frame_count(count)?;
        let (dir, mut doc) = self.open(id)?;
        if let Some(src) = copy_from
            && src >= doc.meta().frames.len()
        {
            return Err(format!(
                "copy_from {src} out of range — document has {} frame(s) (0..={})",
                doc.meta().frames.len(),
                doc.meta().frames.len().saturating_sub(1)
            ));
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

    fn checked_frame_count(count: usize) -> Result<usize, String> {
        if !(1..=MAX_FRAME_COUNT).contains(&count) {
            return Err(format!(
                "frame count must be 1..={MAX_FRAME_COUNT}, got {count}"
            ));
        }
        Ok(count)
    }

    fn doc_set_frame_duration(
        &self,
        id: &str,
        frame: usize,
        ms: u32,
        count: usize,
    ) -> Result<Value, String> {
        let count = Self::checked_frame_count(count)?;
        let (dir, mut doc) = self.open(id)?;
        let frame_to = frame
            .checked_add(count - 1)
            .ok_or_else(|| format!("duration frame range from {frame} overflowed"))?;
        if frame_to >= doc.meta().frames.len() {
            return Err(format!(
                "duration frame range {frame}..={frame_to} out of range — document has {} frame(s) (0..={})",
                doc.meta().frames.len(),
                doc.meta().frames.len().saturating_sub(1)
            ));
        }
        for index in frame..=frame_to {
            doc.set_frame_duration(index, ms)?;
        }
        doc.save(&dir)?;
        let mut out = json!({
            "ok": true,
            "doc_id": id,
            "frame": frame,
            "duration_ms": ms,
        });
        if count > 1 {
            out["frame_to"] = json!(frame_to);
            out["frames_updated"] = json!(count);
        }
        Ok(out)
    }

    /// One-tool dispatch over frame lifecycle + timing — `op`: `add` (append,
    /// optional `copy_from`) | `duration` (set frame `frame`'s ms) | `delete` |
    /// `insert` | `duplicate` | `move`. Routes to the kept `doc_add_frame` /
    /// `doc_set_frame_duration` / `doc_frame_ops`. (Pivot, boxes, tags and
    /// keyframe motion keep their own tools.)
    pub fn doc_frame(
        &self,
        id: &str,
        op: FrameOp,
        frame: Option<usize>,
        copy_from: Option<usize>,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
        count: Option<usize>,
    ) -> Result<Value, String> {
        match op {
            FrameOp::Add => self.doc_add_frame(
                id,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
                copy_from,
                count.unwrap_or(1),
            ),
            FrameOp::Duration => self.doc_set_frame_duration(
                id,
                Self::required_index(op, frame)?,
                duration_ms.unwrap_or(atelier_core::document::DEFAULT_FRAME_MS),
                count.unwrap_or(1),
            ),
            FrameOp::Delete => self.doc_frame_ops(
                id,
                FrameAction::Delete,
                Self::required_index(op, frame)?,
                to_index,
                duration_ms,
            ),
            FrameOp::Insert => self.doc_frame_ops(
                id,
                FrameAction::Insert,
                Self::required_index(op, frame)?,
                to_index,
                duration_ms,
            ),
            FrameOp::Duplicate => self.doc_frame_ops(
                id,
                FrameAction::Duplicate,
                Self::required_index(op, frame)?,
                to_index,
                duration_ms,
            ),
            FrameOp::Move => self.doc_frame_ops(
                id,
                FrameAction::Move,
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
        direction: TagDirection,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        doc.add_tag(name, from, to, direction)?;
        doc.save(&dir)?;
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "tag": name,
            "from": from,
            "to": to,
            "direction": direction.as_str(),
        }))
    }

    // -- render ---------------------------------------------------------------

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

    fn change_ack_summary(id: &str, changed: u64, bbox: Option<[u32; 4]>) -> Value {
        let mut out = json!({
            "ok": true,
            "doc_id": id,
            "pixels_changed": changed,
            "change_bbox": bbox.map(|b| json!(b)).unwrap_or(Value::Null),
        });
        if changed == 0 {
            out["warning"] = json!(
                "no pixels changed — coordinates may be off-canvas or the edit may match what's already there (a no-op, not a failure)"
            );
        }
        out
    }

    fn checked_edit_frame_range(
        doc: &Document,
        layer: usize,
        frame: usize,
        frame_to: usize,
    ) -> Result<usize, String> {
        if layer >= doc.meta().layers.len() {
            return Err(format!("no layer {layer}"));
        }
        if frame > frame_to {
            return Err(format!(
                "frame range start {frame} is after inclusive end {frame_to}"
            ));
        }
        if frame_to >= doc.meta().frames.len() {
            return Err(format!(
                "frame range {frame}..={frame_to} out of range — document has {} frame(s) (0..={})",
                doc.meta().frames.len(),
                doc.meta().frames.len().saturating_sub(1)
            ));
        }
        let count = frame_to
            .checked_sub(frame)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(|| format!("frame range {frame}..={frame_to} overflowed"))?;
        if count > MAX_FRAME_COUNT {
            return Err(format!(
                "frame range {frame}..={frame_to} targets {count} frames; limit is {MAX_FRAME_COUNT}"
            ));
        }
        let canvas_pixels = u64::from(doc.meta().w)
            .checked_mul(u64::from(doc.meta().h))
            .ok_or("frame range canvas size overflowed")?;
        let work =
            canvas_pixels
                .checked_mul(u64::try_from(count).map_err(|_| {
                    format!("frame range count {count} does not fit the work budget")
                })?)
                .ok_or_else(|| format!("frame range {frame}..={frame_to} work overflowed"))?;
        if work > MAX_FRAME_EDIT_PIXELS {
            return Err(format!(
                "frame range {frame}..={frame_to} would scan {work} full-canvas pixels; limit is {MAX_FRAME_EDIT_PIXELS}; split the range"
            ));
        }
        Ok(count)
    }

    /// Apply a cel edit and return its changed-pixel count and bounding box.
    fn edit_with_ack<F>(&self, id: &str, layer: usize, frame: usize, f: F) -> Result<Value, String>
    where
        F: FnOnce(&mut Document) -> Result<(), String>,
    {
        let (dir, mut doc) = self.open(id)?;
        let before = doc.cel_full(layer, frame);
        f(&mut doc)?;
        let (changed, bbox) = doc.cel_change_summary(layer, frame, &before)?;
        drop(before);
        doc.save(&dir)?;
        Ok(Self::change_ack_summary(id, changed, bbox))
    }

    /// Timeline lifecycle (delete | insert | duplicate | move) with cel
    /// reindexing and tag remapping — the recovery path for a bad tween.
    fn doc_frame_ops(
        &self,
        id: &str,
        action: FrameAction,
        frame: usize,
        to_index: Option<usize>,
        duration_ms: Option<u32>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let mut out = doc.frame_ops(action, frame, to_index, duration_ms)?;
        doc.save(&dir)?;
        out["doc_id"] = json!(id);
        Ok(out)
    }

    // -- palette -------------------------------------------------------------

    pub fn doc_set_palette(&self, id: &str, colors: Vec<[u8; 4]>) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let count = colors.len();
        doc.set_palette(colors)?;
        doc.save(&dir)?;
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "palette_set": true,
            "colors": count,
        }))
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
        let changed = doc.palette_swap(&pairs, layer, frame)?;
        doc.save(&dir)?;
        Ok(json!({"doc_id": id, "changed": changed}))
    }

    /// Declarative grid painting: legend (char -> colour or palette index) +
    /// row strings paint a whole region in one call. Palette-index legends are
    /// palette-true by construction.
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
                    // Strict like validate_op's colour check: exactly
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
                            ));
                        }
                    }
                }
                _ => {
                    return Err(format!(
                        "legend '{}': value must be [r,g,b(,a)] or a palette index",
                        k
                    ));
                }
            };
            map.insert(ch, color);
        }
        let counts = std::cell::Cell::new((0u64, 0u64));
        let mut ack = self.edit_with_ack(id, layer, frame, |d| {
            counts.set(d.paint_grid(layer, frame, x, y, &map, &rows)?);
            Ok(())
        })?;
        let (painted, clipped) = counts.get();
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

    /// Validate and apply exactly one draw or effect operation.
    fn edit_operation(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        frame_to: Option<usize>,
        op: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        params.insert("op".into(), json!(op));
        let operation = Value::Object(params);
        let mut ack = match frame_to {
            None => self.edit_with_ack(id, layer, frame, |doc| {
                doc.apply_op(layer, frame, &operation)
            })?,
            Some(frame_to) => {
                let (dir, mut doc) = self.open(id)?;
                let count = Self::checked_edit_frame_range(&doc, layer, frame, frame_to)?;
                atelier_core::document::validate_op(&operation)?;

                let (mut changed, mut bbox, mut frames_changed): (u64, Option<[u32; 4]>, usize) =
                    (0, None, 0);
                for current in frame..=frame_to {
                    let before = doc.cel_full(layer, current);
                    doc.apply_op(layer, current, &operation)?;
                    let (frame_changed, frame_bbox) =
                        doc.cel_change_summary(layer, current, &before)?;
                    changed += frame_changed;
                    if frame_changed > 0 {
                        frames_changed += 1;
                    }
                    if let Some([x0, y0, x1, y1]) = frame_bbox {
                        bbox = Some(match bbox {
                            None => [x0, y0, x1, y1],
                            Some([a0, b0, a1, b1]) => {
                                [a0.min(x0), b0.min(y0), a1.max(x1), b1.max(y1)]
                            }
                        });
                    }
                }
                doc.save(&dir)?;
                let mut ack = Self::change_ack_summary(id, changed, bbox);
                ack["frame"] = json!(frame);
                ack["frame_to"] = json!(frame_to);
                ack["frames_targeted"] = json!(count);
                ack["frames_changed"] = json!(frames_changed);
                ack
            }
        };
        ack["op"] = json!(op);
        Ok(ack)
    }

    /// Apply one drawing operation to a cel, scoped to the "add marks"
    /// vocabulary (geometry, fills, text, and procedural drawing).
    pub fn doc_draw(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        frame_to: Option<usize>,
        op: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let draw_ops = atelier_core::document::draw_ops();
        if !draw_ops.contains(&op) {
            return Err(format!(
                "doc_draw: '{op}' is not a draw op — use one of [{}] (filters and lighting live on their own tools)",
                draw_ops.join(", ")
            ));
        }
        self.edit_operation(id, layer, frame, frame_to, op, params)
    }

    /// Apply one transform/effect operation to a cel. These operations rework
    /// existing pixels; [`doc_draw`](Self::doc_draw) adds new marks.
    pub fn doc_fx(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        frame_to: Option<usize>,
        op: &str,
        params: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        let fx_ops = atelier_core::document::fx_ops();
        if !fx_ops.contains(&op) {
            return Err(format!(
                "doc_fx: '{op}' is not an fx op — use one of [{}] (drawing marks → doc_draw)",
                fx_ops.join(", ")
            ));
        }
        self.edit_operation(id, layer, frame, frame_to, op, params)
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
/// The scale is clamped to `MAX_EXPORT_SCALE` here rather than at every
/// preview, contact-sheet, and diff caller. A `scale: 1000000000`
/// overflowed the dimension multiply — a panic in debug or a multi-terabyte
/// allocation in release. One choke point cannot be forgotten.
pub(crate) fn scale_nn(img: &image::RgbaImage, scale: u32) -> Result<image::RgbaImage, String> {
    let scale = export_scale(scale);
    let (width, height) = atelier_core::raster::checked_rgba_dimensions(
        "scaled preview",
        img.width() as u64 * scale as u64,
        img.height() as u64 * scale as u64,
    )?;
    if scale <= 1 {
        return Ok(img.clone());
    }
    Ok(image::imageops::resize(
        img,
        width,
        height,
        image::imageops::FilterType::Nearest,
    ))
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

    fn set_duration(
        studio: &Studio,
        id: &str,
        frame: usize,
        count: Option<usize>,
        duration_ms: u32,
    ) -> Result<Value, String> {
        studio.doc_frame(
            id,
            FrameOp::Duration,
            Some(frame),
            None,
            None,
            Some(duration_ms),
            count,
        )
    }

    fn frame_durations(studio: &Studio, id: &str) -> Vec<u64> {
        studio.doc_info(id).unwrap()["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| frame["duration_ms"].as_u64().unwrap())
            .collect()
    }

    #[test]
    fn frame_add_count_appends_in_one_call() {
        // "Give me my 10 frames" used to cost 9 identical round-trips.
        let s = studio("addcount");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let out = s.doc_frame(id, FrameOp::Add, None, Some(0), None, Some(80), Some(9));
        assert!(out.is_ok(), "{out:?}");
        let info = s.doc_info(id).unwrap();
        assert_eq!(info["frames"].as_array().map(|f| f.len()), Some(10));
    }

    #[test]
    fn frame_duration_count_updates_one_validated_range() {
        let s = studio("duration-count");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_frame(id, FrameOp::Add, None, None, None, Some(80), Some(3))
            .unwrap();

        let single = set_duration(&s, id, 0, None, 120).unwrap();
        assert_eq!(
            single,
            json!({
                "ok": true,
                "doc_id": id,
                "frame": 0,
                "duration_ms": 120,
            })
        );

        let out = set_duration(&s, id, 1, Some(2), 240).unwrap();
        assert_eq!(out["frame"], 1);
        assert_eq!(out["frame_to"], 2);
        assert_eq!(out["frames_updated"], 2);
        assert_eq!(out["duration_ms"], 240);
        assert_eq!(frame_durations(&s, id), vec![120, 240, 240, 80]);
    }

    #[test]
    fn frame_counts_and_duration_ranges_fail_without_partial_changes() {
        let s = studio("duration-count-errors");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_frame(id, FrameOp::Add, None, None, None, Some(80), Some(3))
            .unwrap();

        for count in [0, MAX_FRAME_COUNT + 1] {
            let add = s
                .doc_frame(id, FrameOp::Add, None, None, None, Some(90), Some(count))
                .unwrap_err();
            assert!(add.contains("frame count must be"), "{add}");

            let duration = set_duration(&s, id, 1, Some(count), 240).unwrap_err();
            assert!(duration.contains("frame count must be"), "{duration}");
        }

        let out_of_range = set_duration(&s, id, 1, Some(4), 240).unwrap_err();
        assert!(
            out_of_range.contains("1..=4 out of range"),
            "{out_of_range}"
        );

        let overflow = set_duration(&s, id, usize::MAX, Some(2), 240).unwrap_err();
        assert!(overflow.contains("overflowed"), "{overflow}");
        assert_eq!(frame_durations(&s, id), vec![100, 80, 80, 80]);
    }

    #[test]
    fn draw_and_fx_ranges_return_one_bounded_aggregate() {
        let s = studio("draw-fx-range");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_frame(id, FrameOp::Add, None, None, None, None, Some(2))
            .unwrap();

        let legacy = s
            .doc_draw(
                id,
                0,
                0,
                None,
                "fill_cel",
                json!({"color": [200, 0, 0, 255]})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            legacy,
            json!({
                "ok": true,
                "doc_id": id,
                "op": "fill_cel",
                "pixels_changed": 16,
                "change_bbox": [0, 0, 3, 3],
            })
        );

        let draw = s
            .doc_draw(
                id,
                0,
                0,
                Some(2),
                "fill_cel",
                json!({"color": [20, 30, 40, 255]})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(draw["frame"], 0);
        assert_eq!(draw["frame_to"], 2);
        assert_eq!(draw["frames_targeted"], 3);
        assert_eq!(draw["frames_changed"], 3);
        assert_eq!(draw["pixels_changed"], 48);
        assert_eq!(draw["change_bbox"], json!([0, 0, 3, 3]));

        let fx = s
            .doc_fx(
                id,
                0,
                0,
                Some(2),
                "replace_color",
                json!({"from": [20, 30, 40, 255], "to": [1, 2, 3, 255]})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(fx["frames_targeted"], 3);
        assert_eq!(fx["frames_changed"], 3);
        assert_eq!(fx["pixels_changed"], 48);
        for frame in 0..=2 {
            assert_eq!(
                s.doc_get_pixel(id, Some(0), frame, 0, 0).unwrap()["rgba"],
                json!([1, 2, 3, 255])
            );
        }

        let seeded = s
            .doc_draw(
                id,
                0,
                0,
                Some(2),
                "scatter",
                json!({
                    "colors": [[40, 50, 60, 255], [70, 80, 90, 255]],
                    "x0": 0, "y0": 0, "x1": 3, "y1": 3,
                    "density": 0.5, "seed": 42
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap();
        assert_eq!(seeded["frames_targeted"], 3);
        for frame in 1..=2 {
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(
                        s.doc_get_pixel(id, Some(0), frame, x, y).unwrap()["rgba"],
                        s.doc_get_pixel(id, Some(0), 0, x, y).unwrap()["rgba"],
                        "the same seed and params must produce the same marks on frame {frame}"
                    );
                }
            }
        }
    }

    #[test]
    fn edit_acks_stay_exact_for_smaller_and_absent_result_cels() {
        let s = studio("borrowed-edit-ack");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_draw(
            id,
            0,
            0,
            None,
            "fill_cel",
            json!({"color": [7, 8, 9, 255]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();

        let scaled = s
            .doc_fx(
                id,
                0,
                0,
                None,
                "scale",
                json!({"w": 2, "h": 2, "method": "nearest"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            scaled,
            json!({
                "ok": true,
                "doc_id": id,
                "op": "scale",
                "pixels_changed": 12,
                "change_bbox": [0, 0, 3, 3],
            })
        );

        let cleared = s
            .doc_draw(id, 0, 0, None, "clear_cel", serde_json::Map::new())
            .unwrap();
        assert_eq!(
            cleared,
            json!({
                "ok": true,
                "doc_id": id,
                "op": "clear_cel",
                "pixels_changed": 4,
                "change_bbox": [0, 0, 1, 1],
            })
        );

        let no_op = s
            .doc_draw(id, 0, 0, None, "clear_cel", serde_json::Map::new())
            .unwrap();
        assert_eq!(
            no_op,
            json!({
                "ok": true,
                "doc_id": id,
                "op": "clear_cel",
                "pixels_changed": 0,
                "change_bbox": null,
                "warning": "no pixels changed — coordinates may be off-canvas or the edit may match what's already there (a no-op, not a failure)",
            })
        );
    }

    #[test]
    fn draw_ranges_reject_invalid_targets_and_work_before_editing() {
        let s = studio("draw-range-limits");
        let created = s.doc_new("d", 1, 1).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_frame(
            id,
            FrameOp::Add,
            None,
            None,
            None,
            None,
            Some(MAX_FRAME_COUNT),
        )
        .unwrap();
        let initial = json!({"color": [9, 8, 7, 255]})
            .as_object()
            .unwrap()
            .clone();
        let params = || {
            json!({"color": [1, 2, 3, 255]})
                .as_object()
                .unwrap()
                .clone()
        };
        s.doc_draw(id, 0, 0, None, "fill_cel", initial).unwrap();

        for (from, to, message) in [
            (2, 1, "after inclusive end"),
            (0, MAX_FRAME_COUNT + 1, "out of range"),
            (0, MAX_FRAME_COUNT, "targets 257 frames"),
        ] {
            let error = s
                .doc_draw(id, 0, from, Some(to), "fill_cel", params())
                .unwrap_err();
            assert!(error.contains(message), "{error}");
        }
        assert_eq!(
            s.doc_get_pixel(id, Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([9, 8, 7, 255])
        );
        assert_eq!(
            s.doc_get_pixel(id, Some(0), 1, 0, 0).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );

        let large = s.doc_new("large", 1024, 1024).unwrap();
        let large_id = large["doc_id"].as_str().unwrap();
        s.doc_frame(large_id, FrameOp::Add, None, None, None, None, Some(16))
            .unwrap();
        let error = s
            .doc_draw(large_id, 0, 0, Some(16), "fill_cel", params())
            .unwrap_err();
        assert!(error.contains("full-canvas pixels"), "{error}");
        assert_eq!(s.doc_info(large_id).unwrap()["cels"], json!([]));
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
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        let mk = |color: Value| {
            let mut legend = serde_json::Map::new();
            legend.insert("k".into(), color);
            s.doc_paint_grid(id, 0, 0, 0, 0, legend, vec!["k".into()])
        };
        // 300 used to truncate to 44 via `as u8`; now it's a loud error.
        let e = mk(json!([255, 300, 0])).unwrap_err();
        assert!(e.contains("0..=255"), "got: {e}");
        assert!(mk(json!([255, 128, 0])).is_ok());
        assert!(mk(json!([255, 128, 0, 200])).is_ok());
        assert!(mk(json!([1, 2])).is_err(), "too short must error");
        assert!(mk(json!([1, 2, 3, 4, 5])).is_err(), "too long must error");
    }

    #[test]
    fn optional_palette_scopes_reject_missing_layers_and_frames() {
        let s = studio("palette-scope");
        let created = s.doc_new("d", 4, 4).unwrap();
        let id = created["doc_id"].as_str().unwrap();
        s.doc_set_palette(id, vec![[1, 2, 3, 255]]).unwrap();

        let swap = |layer, frame| {
            s.doc_palette_swap(id, vec![[1, 2, 3, 255]], vec![[4, 5, 6, 255]], layer, frame)
        };
        assert!(swap(Some(1), None).unwrap_err().contains("no layer 1"));
        assert!(swap(None, Some(1)).unwrap_err().contains("no frame 1"));
        assert!(
            s.snap_palette(
                id,
                Some(1),
                None,
                None,
                atelier_core::document::AlphaSnap::Preserve,
            )
            .unwrap_err()
            .contains("no layer 1")
        );
        assert!(
            s.snap_palette(
                id,
                None,
                Some(1),
                None,
                atelier_core::document::AlphaSnap::Preserve,
            )
            .unwrap_err()
            .contains("no frame 1")
        );
    }
}
