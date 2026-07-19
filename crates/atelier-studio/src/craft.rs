//! World-class-art tooling — the craft layer on top of the primitives.
//!
//! These methods exist to close the gaps the art-quality review found:
//! let the near-blind agent actually SEE (`look`), work without fear
//! (`checkpoint`), edit structure (`layer_ops`), and reach perceptual colour &
//! master finish (`palette`, `snap_palette`, `select_wand`, `critique`).
//! Image-returning methods hand back raw PNG bytes; the server wraps them as
//! inline MCP image content so the pixels arrive in the same turn.

use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage};

/// True only for the `cp<n>` ids `doc_checkpoint save` mints.
///
/// A checkpoint id is joined onto the store path and then handed to
/// `remove_dir_all` / `Document::load`, so an unvalidated one is a directory
/// traversal: `../../../../x` escaped the store and deleted it. Every id the
/// tool hands out matches this shape, so rejecting anything else costs nothing.
fn valid_checkpoint_id(cpid: &str) -> bool {
    cpid.strip_prefix("cp")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}
use serde_json::{json, Value};

use super::Studio;
use atelier_core::document::Document;
use atelier_core::raster;

// -- shared raster helpers --------------------------------------------------

/// Crop an image to inclusive native corners (clamped to the canvas). Returns
/// the cropped image and its native origin `(ox, oy)`.
pub(super) fn crop_region(
    img: &RgbaImage,
    region: Option<(i32, i32, i32, i32)>,
) -> Result<(RgbaImage, i32, i32), String> {
    match region {
        None => Ok((img.clone(), 0, 0)),
        Some((x0, y0, x1, y1)) => {
            let (ax, ay, bx, by) = raster::clamp_region(x0, y0, x1, y1, img.width(), img.height())
                .ok_or("region is empty after clamping to the canvas")?;
            let sub = image::imageops::crop_imm(
                img,
                ax as u32,
                ay as u32,
                (bx - ax + 1) as u32,
                (by - ay + 1) as u32,
            )
            .to_image();
            Ok((sub, ax, ay))
        }
    }
}

/// Luma value-band edges: below SHADOW_MAX reads as shadow, below MID_MAX as
/// midtone, else light — the thirds look stats and critique both bin against.
pub(super) const SHADOW_MAX: u8 = 85;
pub(super) const MID_MAX: u8 = 170;

/// Recursively snapshot a document's files (doc.json + cels/) into `dst`.
fn snapshot_files(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    fs::copy(src.join("doc.json"), dst.join("doc.json")).map_err(|e| e.to_string())?;
    let dcels = dst.join("cels");
    fs::create_dir_all(&dcels).map_err(|e| e.to_string())?;
    if let Ok(rd) = fs::read_dir(src.join("cels")) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_file() {
                fs::copy(&p, dcels.join(ent.file_name())).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

impl Studio {
    // -- doc_checkpoint: snapshot / restore / diff -------------------------

    /// History for an all-destructive editor: snapshot the document directory,
    /// list/restore/diff snapshots, or prune them. `action`: `save` | `list` |
    /// `restore` | `diff` | `prune`.
    pub fn checkpoint(
        &self,
        id: &str,
        action: &str,
        label: Option<&str>,
        checkpoint_id: Option<&str>,
    ) -> Result<Value, String> {
        if !Self::valid_id(id) {
            return Err(format!("invalid document id '{}'", id));
        }
        if !self.exists(id) {
            return Err(format!("no document '{}'", id));
        }
        // `checkpoint_id` is joined onto the store path and handed to
        // remove_dir_all / Document::load, so it is as dangerous as `doc_id` and
        // gets the same treatment. Ids are always minted as `cp{n}` (below), so
        // anything else — traversal, absolute paths, a stray name — is a
        // caller error, not a lookup miss.
        if let Some(cpid) = checkpoint_id {
            if !valid_checkpoint_id(cpid) {
                return Err(format!(
                    "invalid checkpoint id '{}' — expected the cp<n> form doc_checkpoint save returns",
                    cpid
                ));
            }
        }
        let dir = self.doc_dir(id);
        let cps = dir.join(".checkpoints");
        let list_cps = || -> Vec<String> {
            let mut v = Vec::new();
            if let Ok(rd) = fs::read_dir(&cps) {
                for e in rd.flatten() {
                    if e.path().join("doc.json").exists() {
                        v.push(e.file_name().to_string_lossy().to_string());
                    }
                }
            }
            // Numeric order: lexicographic put cp10 before cp2 past nine
            // checkpoints.
            v.sort_by_key(|id| {
                id.strip_prefix("cp")
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(u64::MAX)
            });
            v
        };
        match action {
            "save" => {
                let n = list_cps()
                    .iter()
                    .filter_map(|s| s.strip_prefix("cp").and_then(|t| t.parse::<u32>().ok()))
                    .max()
                    .unwrap_or(0)
                    + 1;
                let cpid = format!("cp{}", n);
                let dst = cps.join(&cpid);
                // A failed snapshot must not leave a partial checkpoint dir —
                // restore would treat it as valid and the prune rotation
                // (which lists by doc.json presence) might never collect it.
                if let Err(e) = snapshot_files(&dir, &dst) {
                    let _ = fs::remove_dir_all(&dst);
                    return Err(e);
                }
                if let Some(lbl) = label {
                    let _ = fs::write(dst.join("label.txt"), lbl);
                }
                Ok(json!({"saved": cpid, "label": label, "doc_id": id}))
            }
            "list" => {
                let items: Vec<Value> = list_cps()
                    .into_iter()
                    .map(|cpid| {
                        let lbl = fs::read_to_string(cps.join(&cpid).join("label.txt")).ok();
                        json!({"id": cpid, "label": lbl})
                    })
                    .collect();
                Ok(json!({"doc_id": id, "checkpoints": items, "count": items.len()}))
            }
            "restore" => {
                let cpid = checkpoint_id.ok_or("restore needs checkpoint_id")?;
                let cp = cps.join(cpid);
                if !cp.join("doc.json").exists() {
                    return Err(format!("no checkpoint '{}'", cpid));
                }
                // Stage the snapshot's files beside the live doc FIRST, then
                // swap: the old code deleted the live cels/doc.json and copied
                // into the void, so a mid-copy failure (disk full, perms)
                // destroyed the working document it was meant to rescue.
                let staging = dir.join(".restore-staging");
                let _ = fs::remove_dir_all(&staging);
                if let Err(e) = snapshot_files(&cp, &staging) {
                    let _ = fs::remove_dir_all(&staging);
                    return Err(e);
                }
                // Swap: drop the live pixels, then same-dir renames (atomic on
                // one filesystem) move the staged files into place. Not fully
                // atomic across the TWO renames: a crash in between leaves the
                // doc without a doc.json (headless) — re-run restore to finish
                // the swap; the checkpoint itself is untouched.
                let _ = fs::remove_dir_all(dir.join("cels"));
                let _ = fs::remove_file(dir.join("doc.json"));
                let swapped = fs::rename(staging.join("cels"), dir.join("cels"))
                    .and_then(|_| fs::rename(staging.join("doc.json"), dir.join("doc.json")));
                let _ = fs::remove_dir_all(&staging);
                swapped.map_err(|e| {
                    format!("restore staged but the swap failed ({e}) — re-run restore to retry")
                })?;
                Ok(json!({"restored": cpid, "doc_id": id}))
            }
            "diff" => {
                let cpid = checkpoint_id.ok_or("diff needs checkpoint_id")?;
                let cp = cps.join(cpid);
                if !cp.join("doc.json").exists() {
                    return Err(format!("no checkpoint '{}'", cpid));
                }
                let (_d, live) = self.open(id)?;
                let was = Document::load(&cp)?;
                Ok(checkpoint_diff(cpid, &was, &live))
            }
            "prune" => match checkpoint_id {
                Some(cpid) => {
                    let cp = cps.join(cpid);
                    let _ = fs::remove_dir_all(&cp);
                    Ok(json!({"pruned": cpid, "doc_id": id}))
                }
                None => {
                    let _ = fs::remove_dir_all(&cps);
                    Ok(json!({"pruned": "all", "doc_id": id}))
                }
            },
            other => Err(format!(
                "unknown checkpoint action '{}' — use save|list|restore|diff|prune",
                other
            )),
        }
    }

    // -- doc_layer_ops: the structural backbone ----------------------------

    /// Layer-stack lifecycle in one tool: `move` | `insert` | `delete` |
    /// `rename` | `duplicate` | `merge_down`. Returns the new layer list.
    pub fn layer_ops(
        &self,
        id: &str,
        action: &str,
        index: usize,
        to_index: Option<usize>,
        name: Option<String>,
        opacity: u8,
        blend: String,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let mut new_index = None;
        match action {
            "move" => doc.move_layer(index, to_index.ok_or("move needs to_index")?)?,
            "insert" => new_index = Some(doc.insert_layer(index, name, opacity, blend)),
            "delete" => doc.delete_layer(index)?,
            "rename" => doc.rename_layer(index, name.ok_or("rename needs name")?)?,
            "duplicate" => new_index = Some(doc.duplicate_layer(index)?),
            "merge_down" => doc.merge_down(index)?,
            other => {
                return Err(format!(
                "unknown layer action '{}' — use move|insert|delete|rename|duplicate|merge_down",
                other
            ))
            }
        }
        doc.save(&dir)?;
        let layers: Vec<Value> = doc
            .meta()
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| json!({"index": i, "name": l.name, "opacity": l.opacity, "visible": l.visible, "blend": l.blend}))
            .collect();
        Ok(
            json!({"ok": true, "doc_id": id, "action": action, "new_index": new_index, "layers": layers}),
        )
    }

    // -- doc_snap_palette ---------------------------------------------------

    /// Snap a cel (or the whole document) to its locked palette by perceptual
    /// nearest colour. `palette` overrides the document's stored one.
    pub fn snap_palette(
        &self,
        id: &str,
        layer: Option<usize>,
        frame: Option<usize>,
        palette: Option<Vec<[u8; 4]>>,
        alpha: atelier_core::document::AlphaSnap,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let pal = match palette {
            Some(p) => p,
            None => doc.meta().palette.clone(),
        };
        if pal.is_empty() {
            return Err(
                "no palette to snap to — pass `palette` or set one with doc_set_palette".into(),
            );
        }
        let changed = doc.snap_to_palette(&pal, layer, frame, alpha);
        doc.save(&dir)?;
        Ok(json!({"ok": true, "doc_id": id, "pixels_changed": changed, "palette_len": pal.len()}))
    }

    // -- doc_critique: the art-director scorecard --------------------------

    /// Aggregated craft scorecard — the named pixel-art failure modes the agent
    /// cannot see: orphan specks, un-AA'd jaggies, low contrast, pillow-shading,
    /// off-palette drift, and value-soup massing. Conservative verdicts so a
    /// blind agent doesn't wreck deliberate choices chasing false defects.
    pub fn critique(
        &self,
        id: &str,
        frame: usize,
        layer: Option<usize>,
        region: Option<(i32, i32, i32, i32)>,
    ) -> Result<Value, String> {
        let (_dir, doc) = self.open(id)?;
        let full = doc.analysis_image(layer, frame)?;
        let (img, _ox, _oy) = crop_region(&full, region)?;
        let palette = doc.meta().palette.clone();
        Ok(critique_image(id, frame, &img, &palette))
    }

    // -- doc_dither_ramp: graduated multi-tone dithering -------------------

    /// Graduated dithering across a whole ramp along an axis (h|v|radial) with
    /// an ordered or `ign` blue-noise pattern — master gradient shading.
    /// Honours an active selection.
    pub fn dither_ramp(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        region: Option<(i32, i32, i32, i32)>,
        ramp: Vec<[u8; 4]>,
        axis: &str,
        pattern: &str,
        only_existing: bool,
    ) -> Result<Value, String> {
        self.edit_masked(id, layer, frame, |d| {
            d.dither_ramp(layer, frame, region, &ramp, axis, pattern, only_existing)
                .map(|_| ())
        })
    }

    // -- doc_palette: one OKLCh generator for mono + harmony schemes -------

    /// Unified palette generator: one OKLCh engine for a single shading ramp
    /// (`scheme="mono"`) or a cohesive multi-hue scheme (complementary | triadic
    /// | analogous | split | tetradic), folding in the sat_curve / midtone-anchor
    /// / evenness-validation that the old `make_perceptual_ramp` had and the old
    /// `harmony_palette` lacked. Supersedes `palette_ramp` / `make_perceptual_ramp`
    /// / `harmony_palette`.
    pub fn palette(
        &self,
        base: [u8; 4],
        scheme: &str,
        count: usize,
        value_lo: Option<f32>,
        value_hi: Option<f32>,
        hue_shift: f32,
        sat_curve: &str,
        anchor_midtone: bool,
        set_doc: Option<&str>,
    ) -> Result<Value, String> {
        let offsets: Vec<f32> = match scheme {
            "mono" => vec![0.0],
            "complementary" => vec![0.0, 180.0],
            "triadic" => vec![0.0, 120.0, 240.0],
            "analogous" => vec![0.0, 30.0, -30.0],
            "split" => vec![0.0, 150.0, 210.0],
            "tetradic" => vec![0.0, 90.0, 180.0, 270.0],
            other => {
                return Err(format!(
                    "unknown scheme '{other}' — use mono|complementary|triadic|analogous|split|tetradic"
                ))
            }
        };
        let (lb, cb, hb) = raster::oklab_to_oklch(raster::srgb_to_oklab(base));
        let lo = value_lo.unwrap_or((lb - 0.32).max(0.04));
        let hi = value_hi.unwrap_or((lb + 0.32).min(0.97));
        // mono honours the exact count; a multi-hue scheme needs >=2 per ramp.
        let per = if scheme == "mono" {
            count.max(1)
        } else {
            count.max(2)
        };
        let mut ramps: Vec<Vec<[u8; 4]>> = Vec::new();
        for off in &offsets {
            let rgb = raster::oklab_to_srgb(raster::oklch_to_oklab((lb, cb, hb + off)));
            let anchor = [rgb[0], rgb[1], rgb[2], 255];
            ramps.push(raster::make_ramp_oklch(
                anchor,
                per,
                lo,
                hi,
                hue_shift,
                sat_curve,
                anchor_midtone,
            ));
        }
        let flat: Vec<[u8; 4]> = ramps.iter().flatten().copied().collect();
        let hex: Vec<String> = flat.iter().map(|c| crate::hex_rgb(c)).collect();
        // Evenness validation on the primary ramp.
        let ls: Vec<f32> = ramps[0]
            .iter()
            .map(|c| raster::srgb_to_oklab(*c).0)
            .collect();
        let steps: Vec<f32> = ls.windows(2).map(|w| w[1] - w[0]).collect();
        let monotonic = steps.iter().all(|d| *d > -0.001);
        let (mean_step, max_dev) = if steps.is_empty() {
            (0.0, 0.0)
        } else {
            let m = steps.iter().sum::<f32>() / steps.len() as f32;
            let dev = steps.iter().map(|d| (d - m).abs()).fold(0.0_f32, f32::max);
            (m, dev)
        };
        let mut out = json!({
            "scheme": scheme, "ramps": ramps, "palette": flat, "hex": hex, "count": flat.len(),
            "validation": {
                "monotonic_lightness": monotonic,
                "mean_step": (mean_step * 1000.0).round() / 1000.0,
                "max_step_deviation": (max_dev * 1000.0).round() / 1000.0,
                "even": max_dev < mean_step.abs() * 0.5 + 0.01,
            }
        });
        if let Some(did) = set_doc {
            self.edit(did, |d| {
                d.set_palette(flat.clone());
                Ok(())
            })?;
            out["set_doc"] = json!(did);
        }
        Ok(out)
    }

    // -- doc_box: 3-face shaded isometric cuboid ----------------------------

    /// Draw a shaded isometric cuboid (top + two side faces) from one base
    /// colour, auto-shaded along a perceptual ramp — the hard-surface form
    /// primitive `form` can't make. `(cx,cy)` is the centre of the top diamond,
    /// `s` its half-width, `ht` the body height; `light_right` brightens the
    /// right face (else the left).
    pub fn box_iso(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        cx: i32,
        cy: i32,
        s: i32,
        ht: i32,
        base: [u8; 4],
        light_right: bool,
    ) -> Result<Value, String> {
        let s = s.max(1);
        let ht = ht.max(1);
        let hh = (s / 2).max(1);
        // i64 + saturate: the centre/size are raw caller input, and `cx + s`
        // et al overflow i32 near the extremes (debug panic / release wrap).
        let c = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let (cx, cy, s, ht, hh) = (cx as i64, cy as i64, s as i64, ht as i64, hh as i64);
        let top = vec![
            (c(cx), c(cy - hh)),
            (c(cx + s), c(cy)),
            (c(cx), c(cy + hh)),
            (c(cx - s), c(cy)),
        ];
        let left = vec![
            (c(cx - s), c(cy)),
            (c(cx), c(cy + hh)),
            (c(cx), c(cy + hh + ht)),
            (c(cx - s), c(cy + ht)),
        ];
        let right = vec![
            (c(cx + s), c(cy)),
            (c(cx), c(cy + hh)),
            (c(cx), c(cy + hh + ht)),
            (c(cx + s), c(cy + ht)),
        ];
        let r = auto_ramp(base, 5);
        let (top_c, bright, dark) = (r[4], r[3], r[1]);
        let (right_c, left_c) = if light_right {
            (bright, dark)
        } else {
            (dark, bright)
        };
        self.edit(id, |d| {
            d.polygon(layer, frame, &left, left_c, true)?;
            d.polygon(layer, frame, &right, right_c, true)?;
            d.polygon(layer, frame, &top, top_c, true)?;
            Ok(())
        })
    }

    // -- doc_panel: a HUD/UI 9-slice-style panel ---------------------------

    /// Draw a UI panel: filled body, border, and an optional inner bevel
    /// (top/left lit, bottom/right shadowed) — a ready HUD panel/dialog box vs
    /// hand-placing every edge pixel.
    pub fn panel(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        fill: [u8; 4],
        border: [u8; 4],
        bevel: bool,
    ) -> Result<Value, String> {
        // i64 + saturate: `x + w - 1` overflows i32 for extreme caller input.
        let c = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let (x1, y1) = (c(x as i64 + w as i64 - 1), c(y as i64 + h as i64 - 1));
        self.edit(id, |d| {
            d.rect(layer, frame, x, y, x1, y1, fill, true, 1)?;
            d.rect(layer, frame, x, y, x1, y1, border, false, 1)?;
            if bevel && w > 3 && h > 3 {
                let hi = raster::shade_hsl(fill, 1, 2);
                let lo = raster::shade_hsl(fill, -1, 2);
                d.line(layer, frame, x + 1, y + 1, x1 - 1, y + 1, hi, 1)?; // top
                d.line(layer, frame, x + 1, y + 1, x + 1, y1 - 1, hi, 1)?; // left
                d.line(layer, frame, x1 - 1, y + 1, x1 - 1, y1 - 1, lo, 1)?; // right
                d.line(layer, frame, x + 1, y1 - 1, x1 - 1, y1 - 1, lo, 1)?; // bottom
            }
            Ok(())
        })
    }

    // -- doc_import_clean: reference -> clean pixel art --------------------

    /// Import an external image as clean pixel art: optional corner-seeded
    /// background removal, TRUE area-average downscale to the target size
    /// (aspect-derived height when omitted), then quantise — optionally
    /// Floyd–Steinberg — to a palette (the document's locked one, or a
    /// frequency-weighted median-cut of the SUBJECT's colours, with `pin`ned
    /// colours always kept), with optional alpha defringe. The reference-
    /// onboarding pipeline for characters and AI/photo art.
    pub fn import_clean(
        &self,
        id: &str,
        layer: usize,
        frame: usize,
        path: &str,
        target_w: u32,
        target_h: Option<u32>,
        colors: usize,
        dither: Option<bool>,
        defringe: bool,
        to_doc_palette: bool,
        remove_bg: bool,
        pin: Vec<[u8; 4]>,
    ) -> Result<Value, String> {
        let (dir, mut doc) = self.open(id)?;
        let mut src = crate::open_bounded(std::path::Path::new(path))?;
        // Background removal runs at SOURCE resolution, before any pixel of
        // backdrop can be averaged into the subject's edges or its palette.
        if remove_bg {
            raster::remove_background(&mut src, 0.08);
        }
        let tw = target_w.max(1);
        let th = target_h.unwrap_or_else(|| {
            // Derive an aspect-true height so a wrong guess can't squash the
            // subject. Round to nearest, floor 1.
            ((src.height() as f64 * tw as f64 / src.width().max(1) as f64).round() as u32).max(1)
        });
        if th as usize * tw as usize > crate::MAX_TARGET_PIXELS {
            return Err(format!(
                "target {}x{} is over the 1M-pixel cap — import at a smaller size",
                tw, th
            ));
        }
        // Default decided HERE, where the derived height is known — at sprite
        // scale (longest side ≤ 64px) error diffusion reads as speckle.
        let dither = dither.unwrap_or(tw.max(th) > 64);
        let resized = raster::downscale_area(&src, tw, th);
        let mut work: Vec<[f32; 4]> = resized
            .pixels()
            .map(|p| [p.0[0] as f32, p.0[1] as f32, p.0[2] as f32, p.0[3] as f32])
            .collect();
        let (w, h) = (resized.width() as i32, resized.height() as i32);
        if defringe {
            for px in work.iter_mut() {
                px[3] = if px[3] < 128.0 { 0.0 } else { 255.0 };
            }
        }
        // Palette: the doc's locked one, or a frequency-weighted median cut of
        // the (post-bg-removal) opaque pixels so the subject owns every slot.
        let palette: Vec<[u8; 4]> = if to_doc_palette && !doc.meta().palette.is_empty() {
            doc.meta().palette.clone()
        } else {
            let mut counts: std::collections::HashMap<[u8; 3], u64> =
                std::collections::HashMap::new();
            for p in work.iter().filter(|p| p[3] > 0.0) {
                *counts
                    .entry([p[0] as u8, p[1] as u8, p[2] as u8])
                    .or_insert(0) += 1;
            }
            if counts.is_empty() {
                return Err("imported image is fully transparent".into());
            }
            let pairs: Vec<([u8; 3], u64)> = counts.into_iter().collect();
            raster::median_cut_weighted(&pairs, colors.max(2), &pin)
        };
        // Quantise (optionally Floyd–Steinberg) to the palette.
        let mut lab = raster::PaletteLab::new(&palette);
        let mut out = RgbaImage::from_pixel(w as u32, h as u32, Rgba([0, 0, 0, 0]));
        let idx = |x: i32, y: i32| (y * w + x) as usize;
        for y in 0..h {
            for x in 0..w {
                let p = work[idx(x, y)];
                if p[3] <= 0.0 {
                    continue;
                }
                let cur = [
                    p[0].clamp(0.0, 255.0) as u8,
                    p[1].clamp(0.0, 255.0) as u8,
                    p[2].clamp(0.0, 255.0) as u8,
                    255,
                ];
                let pi = lab.nearest(cur).unwrap_or(0);
                let chosen = lab.color(pi);
                out.put_pixel(
                    x as u32,
                    y as u32,
                    Rgba([chosen[0], chosen[1], chosen[2], p[3] as u8]),
                );
                if dither {
                    let err = [
                        p[0] - chosen[0] as f32,
                        p[1] - chosen[1] as f32,
                        p[2] - chosen[2] as f32,
                    ];
                    let mut spread = |sx: i32, sy: i32, f: f32| {
                        if sx >= 0 && sy >= 0 && sx < w && sy < h {
                            let q = &mut work[idx(sx, sy)];
                            if q[3] > 0.0 {
                                for c in 0..3 {
                                    q[c] += err[c] * f;
                                }
                            }
                        }
                    };
                    spread(x + 1, y, 7.0 / 16.0);
                    spread(x - 1, y + 1, 3.0 / 16.0);
                    spread(x, y + 1, 5.0 / 16.0);
                    spread(x + 1, y + 1, 1.0 / 16.0);
                }
            }
        }
        doc.set_cel(layer, frame, 0, 0, out)?;
        if to_doc_palette && doc.meta().palette.is_empty() {
            doc.set_palette(palette.clone());
        }
        doc.save(&dir)?;
        let pal_json: Vec<Value> = palette.iter().map(|c| json!(c)).collect();
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "size": [w, h],
            "palette": pal_json,
            "dithered": dither,
            "bg_removed": remove_bg,
        }))
    }

    // -- doc_place_tiles: tilemap stamping from a tileset doc -------------

    /// Stamp tiles from a tileset document onto a canvas cel. The tileset's
    /// flattened frame 0 is sliced row-major into `tile_w`×`tile_h` tiles
    /// (index 0 = top-left); each `cells` entry `[cell_x, cell_y, tile_index]`
    /// stamps that tile source-over at pixel `(cell_x*tile_w, cell_y*tile_h)`,
    /// clipping at the canvas edges. The tileset is READ, never saved. Cells
    /// landing entirely off-canvas are counted as skipped, not errors; a bad
    /// tile index fails the whole call before anything is stamped. Every
    /// argument is plain JSON, so the call journals and replays
    /// byte-identically — no pixel buffer ever crosses the wire.
    pub fn doc_place_tiles(
        &self,
        doc_id: &str,
        layer: usize,
        frame: usize,
        tiles_doc: &str,
        tile_w: u32,
        tile_h: u32,
        cells: &[[i32; 3]],
    ) -> Result<Value, String> {
        if tile_w == 0 || tile_h == 0 {
            return Err(format!(
                "tile size must be >= 1px — got {}x{}",
                tile_w, tile_h
            ));
        }
        // Read-only: flatten frame 0 and drop the doc — it is never saved.
        let sheet = {
            let (_dir, tiles) = self.open(tiles_doc)?;
            tiles.flatten(0)
        };
        let (sw, sh) = (sheet.width(), sheet.height());
        if sw % tile_w != 0 || sh % tile_h != 0 {
            return Err(format!(
                "tileset '{}' is {}x{} — not divisible into {}x{} tiles",
                tiles_doc, sw, sh, tile_w, tile_h
            ));
        }
        let (cols, rows) = (sw / tile_w, sh / tile_h);
        let total = cols * rows;
        // Validate every index BEFORE the canvas is touched: a bad cell fails
        // the whole call, never leaves a half-stamped tilemap.
        for &[_cx, _cy, idx] in cells {
            if idx < 0 || idx as u32 >= total {
                return Err(format!(
                    "tile index {} out of range — '{}' has {} tile(s) (0..={})",
                    idx,
                    tiles_doc,
                    total,
                    total - 1
                ));
            }
        }
        let (dir, mut doc) = self.open(doc_id)?;
        if layer >= doc.meta().layers.len() {
            return Err(format!("no layer {}", layer));
        }
        if frame >= doc.meta().frames.len() {
            return Err(format!("no frame {}", frame));
        }
        let (cw, ch) = (doc.meta().w as i64, doc.meta().h as i64);
        let (mut placed, mut skipped) = (0u64, 0u64);
        for &[cx, cy, idx] in cells {
            // i64 math: cell*size is raw caller input and overflows i32 at the
            // extremes (debug panic / release wrap). Cells passing the skip
            // test below sit within one tile of the canvas, so the i32 casts
            // handed to paste_region cannot wrap.
            let (dx, dy) = (cx as i64 * tile_w as i64, cy as i64 * tile_h as i64);
            if dx >= cw || dy >= ch || dx + tile_w as i64 <= 0 || dy + tile_h as i64 <= 0 {
                skipped += 1;
                continue;
            }
            let idx = idx as u32;
            let (sx, sy) = ((idx % cols) * tile_w, (idx / cols) * tile_h);
            let tile = image::imageops::crop_imm(&sheet, sx, sy, tile_w, tile_h)
                .to_image()
                .into_raw();
            doc.paste_region(
                layer, frame, dx as i32, dy as i32, tile_w, tile_h, &tile, true,
            )?;
            placed += 1;
        }
        doc.save(&dir)?;
        Ok(json!({
            "ok": true,
            "doc_id": doc_id,
            "tiles_placed": placed,
            "cells_skipped": skipped,
        }))
    }
}

/// A perceptually-even ramp bracketing a base colour's lightness — the default
/// ramp used by `box_iso`.
fn auto_ramp(base: [u8; 4], count: usize) -> Vec<[u8; 4]> {
    let (lb, _, _) = raster::oklab_to_oklch(raster::srgb_to_oklab(base));
    let lo = (lb - 0.34).max(0.04);
    let hi = (lb + 0.34).min(0.97);
    raster::make_ramp_oklch(base, count, lo, hi, 18.0, "arc", false)
}

/// Pure scorecard over a single image — the guts of `critique`, factored out so
/// it can be unit-tested without a Studio/disk.
fn critique_image(id: &str, frame: usize, img: &RgbaImage, palette: &[[u8; 4]]) -> Value {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let op = |x: i32, y: i32| -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        }
        let p = img.get_pixel(x as u32, y as u32).0;
        if p[3] == 0 {
            None
        } else {
            Some(p)
        }
    };
    // -- value stats + masses --
    let (mut min, mut max, mut sum, mut n) = (255u8, 0u8, 0f64, 0u64);
    let (mut shadow, mut mid, mut light) = (0u64, 0u64, 0u64);
    for y in 0..h {
        for x in 0..w {
            if let Some(p) = op(x, y) {
                let v = raster::luma(p);
                min = min.min(v);
                max = max.max(v);
                sum += v as f64;
                n += 1;
                if v < SHADOW_MAX {
                    shadow += 1;
                } else if v < MID_MAX {
                    mid += 1;
                } else {
                    light += 1;
                }
            }
        }
    }
    if n == 0 {
        return json!({"doc_id": id, "frame": frame, "note": "nothing opaque to critique"});
    }
    let nf = n as f64;
    let contrast = (max - min) as f64 / 255.0;
    let masses = [shadow as f64 / nf, mid as f64 / nf, light as f64 / nf];
    let soup = masses.iter().all(|m| (0.22..=0.45).contains(m));

    // -- orphans: connected components of size <= 2 --
    let mut seen = vec![false; (w * h) as usize];
    let mut orphan_cells: Vec<Value> = Vec::new();
    let mut orphans = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if seen[i] || op(x, y).is_none() {
                continue;
            }
            // flood this component (4-conn), counting size
            let mut stack = vec![(x, y)];
            let mut cells = Vec::new();
            while let Some((px, py)) = stack.pop() {
                if px < 0 || py < 0 || px >= w || py >= h {
                    continue;
                }
                let j = (py * w + px) as usize;
                if seen[j] || op(px, py).is_none() {
                    continue;
                }
                seen[j] = true;
                cells.push((px, py));
                stack.extend_from_slice(&[(px + 1, py), (px - 1, py), (px, py + 1), (px, py - 1)]);
                if cells.len() > 2 {
                    // not an orphan; drain the rest without recording
                    while let Some((qx, qy)) = stack.pop() {
                        if qx < 0 || qy < 0 || qx >= w || qy >= h {
                            continue;
                        }
                        let k = (qy * w + qx) as usize;
                        if seen[k] || op(qx, qy).is_none() {
                            continue;
                        }
                        seen[k] = true;
                        stack.extend_from_slice(&[
                            (qx + 1, qy),
                            (qx - 1, qy),
                            (qx, qy + 1),
                            (qx, qy - 1),
                        ]);
                    }
                    break;
                }
            }
            if (1..=2).contains(&cells.len()) {
                orphans += 1;
                if orphan_cells.len() < 12 {
                    orphan_cells.push(json!([cells[0].0, cells[0].1]));
                }
            }
        }
    }

    // -- jaggies: outer-corner notches (un-AA'd staircases) --
    let mut jaggies = 0u32;
    let mut jag_cells: Vec<Value> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if op(x, y).is_some() {
                continue;
            }
            let nb = [op(x, y - 1), op(x, y + 1), op(x + 1, y), op(x - 1, y)];
            let cnt = nb.iter().filter(|p| p.is_some()).count();
            // exactly two perpendicular opaque neighbours == an outer step corner
            let perp = (nb[0].is_some() || nb[1].is_some()) && (nb[2].is_some() || nb[3].is_some());
            if cnt == 2 && perp {
                jaggies += 1;
                if jag_cells.len() < 12 {
                    jag_cells.push(json!([x, y]));
                }
            }
        }
    }

    // -- form lighting: per-form light direction + pillow-shading (the precise
    // per-component eye, superseding a whole-image radial guess) --
    let fa = crate::analysis::form_audit_image(img, 12);
    let pillow_forms = fa["pillow_forms"].as_u64().unwrap_or(0);
    let light_spread = fa["light_spread_deg"].as_f64();
    let light_inconsistent = light_spread.map(|s| s > 45.0).unwrap_or(false);

    // -- palette adherence --
    let palette_check = if palette.is_empty() {
        json!({"value": Value::Null, "verdict": "info", "note": "no locked palette"})
    } else {
        let inset: std::collections::HashSet<[u8; 4]> = palette.iter().copied().collect();
        let mut off = 0u64;
        for y in 0..h {
            for x in 0..w {
                if let Some(p) = op(x, y) {
                    if !inset.contains(&p) {
                        off += 1;
                    }
                }
            }
        }
        let off_pct = (off as f64 / nf * 1000.0).round() / 10.0;
        json!({"off_palette_pct": off_pct, "verdict": if off_pct > 5.0 { "warn" } else { "ok" },
               "note": "exact-match check; soft FX bloom counts as off-palette — snap with doc_palette op=snap if undeliberate"})
    };

    let round = |x: f64| (x * 1000.0).round() / 1000.0;
    json!({
        "doc_id": id, "frame": frame, "opaque_pixels": n,
        "checks": {
            "contrast": {"value": round(contrast), "verdict": if contrast < 0.25 { "warn" } else { "ok" },
                         "min": min, "max": max, "mean": (sum / nf).round() as u32},
            "value_masses": {"shadow": round(masses[0]), "mid": round(masses[1]), "light": round(masses[2]),
                             "verdict": if soup { "warn" } else { "ok" },
                             "note": if soup { "even thirds — value soup; group into clearer masses" } else { "" }},
            "orphans": {"count": orphans, "verdict": if orphans > 0 { "warn" } else { "ok" }, "cells": orphan_cells,
                        "note": "isolated 1-2px islands; on FX sprites deliberate sparks/embers/motes are LEGITIMATE — fix only true strays"},
            "jaggies": {"count": jaggies, "verdict": if jaggies > (n / 12).max(6) as u32 { "warn" } else { "info" },
                        "cells": jag_cells, "note": "outer step corners; even out the stair steps, or place single mid-tone (selout) pixels in the corners — small curves on a locked palette always keep some, so judge by eye rather than chasing zero"},
            "pillow_shading": {"forms": pillow_forms, "verdict": if pillow_forms > 0 { "warn" } else { "ok" },
                               "note": "forms lit brightest at the centre with no light direction; shade from a light source"},
            "form_lighting": {"dominant_azimuth_deg": fa["dominant_light_azimuth_deg"].clone(),
                              "spread_deg": fa["light_spread_deg"].clone(),
                              "verdict": if light_inconsistent { "warn" } else { "ok" },
                              "note": "lit forms should agree on one light; wide spread = mixed light directions"},
            "palette_adherence": palette_check,
        }
    })
}

/// Cross-snapshot diff: structural + per-pixel change tallies on frame 0.
fn checkpoint_diff(cpid: &str, was: &Document, now: &Document) -> Value {
    let stat = |img: &RgbaImage| -> (u64, usize, u8, u8) {
        let (mut n, mut min, mut max) = (0u64, 255u8, 0u8);
        let mut distinct = std::collections::HashSet::new();
        for p in img.pixels() {
            if p.0[3] == 0 {
                continue;
            }
            n += 1;
            let v = raster::luma(p.0);
            min = min.min(v);
            max = max.max(v);
            distinct.insert(p.0);
        }
        (n, distinct.len(), min, max)
    };
    // One flatten per document — stat and the pixel tally share it.
    let (ia, ib) = (was.flatten(0), now.flatten(0));
    let (an, ac, amin, amax) = stat(&ia);
    let (bn, bc, bmin, bmax) = stat(&ib);
    // Per-pixel change tally where the canvases line up.
    let (mut added, mut removed, mut changed) = (0u64, 0u64, 0u64);
    if was.meta().w == now.meta().w && was.meta().h == now.meta().h {
        for (pa, pb) in ia.pixels().zip(ib.pixels()) {
            match (pa.0[3] == 0, pb.0[3] == 0) {
                (true, false) => added += 1,
                (false, true) => removed += 1,
                (false, false) if pa.0 != pb.0 => changed += 1,
                _ => {}
            }
        }
    }
    json!({
        "checkpoint": cpid,
        "pixels": {"was": an, "now": bn, "delta": bn as i64 - an as i64},
        "distinct_colors": {"was": ac, "now": bc, "delta": bc as i64 - ac as i64},
        "contrast": {
            "was": ((amax - amin) as f64 / 255.0 * 1000.0).round() / 1000.0,
            "now": ((bmax - bmin) as f64 / 255.0 * 1000.0).round() / 1000.0,
        },
        "frame0_change": {"added": added, "removed": removed, "recolored": changed},
        "regressions": {
            "lost_contrast": (bmax - bmin) < (amax - amin),
            "color_creep": bc > ac + (ac / 4).max(2),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio(tag: &str) -> Studio {
        let dir = std::env::temp_dir().join(format!("atelier-craft-{}", tag));
        let _ = fs::remove_dir_all(&dir);
        Studio::with_docs_dir(dir)
    }

    /// Single draw-op shorthand: `params` is the op's JSON object (as `json!`).
    fn draw(s: &Studio, id: &str, frame: usize, op: &str, params: Value) -> Value {
        s.doc_draw(id, 0, frame, op, params.as_object().unwrap().clone())
            .unwrap()
    }

    fn opaque(stats: &Value) -> u64 {
        stats["stats"]["opaque_pixels"].as_u64().unwrap_or(0)
    }

    #[test]
    fn checkpoint_id_cannot_escape_the_store() {
        let s = studio("cp-escape");
        s.doc_create("c", 8, 8).unwrap();
        s.checkpoint("c", "save", None, None).unwrap();

        // A directory outside the store that must survive every attempt.
        let outside = std::env::temp_dir().join("atelier-test-cp-escape-victim");
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).unwrap();

        for evil in [
            "../../../../atelier-test-cp-escape-victim",
            "../..",
            "..",
            "/tmp",
            "cp1/../../..",
            "not-a-checkpoint",
            "cp",
            "cp1x",
        ] {
            for action in ["prune", "restore", "diff"] {
                let r = s.checkpoint("c", action, None, Some(evil));
                assert!(
                    r.is_err(),
                    "{action} accepted the traversal id {evil:?}: {r:?}"
                );
            }
        }
        assert!(outside.exists(), "a checkpoint id escaped the store");
        let _ = std::fs::remove_dir_all(&outside);

        // The real id still works.
        assert!(s.checkpoint("c", "restore", None, Some("cp1")).is_ok());
    }

    #[test]
    fn layer_ops_insert_and_merge() {
        let s = studio("layers");
        s.doc_create("c", 4, 4).unwrap();
        let r = s
            .layer_ops(
                "c",
                "insert",
                0,
                None,
                Some("bg".into()),
                255,
                "normal".into(),
            )
            .unwrap();
        assert_eq!(r["layers"].as_array().unwrap().len(), 2);
        let m = s
            .layer_ops("c", "merge_down", 1, None, None, 255, "normal".into())
            .unwrap();
        assert_eq!(m["layers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn critique_flags_an_orphan_speck() {
        let s = studio("crit");
        s.doc_create("c", 8, 8).unwrap();
        draw(
            &s,
            "c",
            0,
            "pencil",
            json!({"points": [[1, 1]], "color": [255, 255, 255, 255]}),
        );
        let r = s.critique("c", 0, None, None).unwrap();
        assert_eq!(r["checks"]["orphans"]["count"], 1);
    }

    #[test]
    fn critique_flags_pillow_shading_via_form_audit() {
        let s = studio("critpillow");
        s.doc_create("p", 16, 16).unwrap();
        // Concentric squares: bright centre, dark all edges — pillow-shaded, no
        // light direction. The form-audit-backed check should warn.
        draw(
            &s,
            "p",
            0,
            "rect",
            json!({"x0": 2, "y0": 2, "x1": 13, "y1": 13, "color": [50, 50, 60, 255], "fill": true}),
        );
        draw(
            &s,
            "p",
            0,
            "rect",
            json!({"x0": 4, "y0": 4, "x1": 11, "y1": 11, "color": [90, 90, 105, 255], "fill": true}),
        );
        draw(
            &s,
            "p",
            0,
            "rect",
            json!({"x0": 6, "y0": 6, "x1": 9, "y1": 9, "color": [140, 140, 160, 255], "fill": true}),
        );
        draw(
            &s,
            "p",
            0,
            "rect",
            json!({"x0": 7, "y0": 7, "x1": 8, "y1": 8, "color": [200, 200, 225, 255], "fill": true}),
        );
        let r = s.critique("p", 0, None, None).unwrap();
        assert_eq!(r["checks"]["pillow_shading"]["verdict"], "warn");
        assert!(r["checks"]["pillow_shading"]["forms"].as_u64().unwrap() >= 1);
    }

    fn distinct(stats: &Value) -> u64 {
        stats["stats"]["distinct_colors"].as_u64().unwrap_or(0)
    }

    #[test]
    fn box_iso_draws_three_shaded_faces() {
        let s = studio("box");
        s.doc_create("c", 32, 32).unwrap();
        s.box_iso("c", 0, 0, 16, 10, 8, 10, [150, 110, 80, 255], true)
            .unwrap();
        let look = s
            .look(
                "c",
                0,
                &crate::LookOptions {
                    scale: Some(1),
                    bands: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(distinct(&look.1) >= 3, "three faces => three shades");
    }

    #[test]
    fn panel_draws_fill_and_border() {
        let s = studio("panel");
        s.doc_create("c", 20, 12).unwrap();
        s.panel(
            "c",
            0,
            0,
            1,
            1,
            18,
            10,
            [60, 60, 90, 255],
            [10, 10, 20, 255],
            true,
        )
        .unwrap();
        let look = s
            .look(
                "c",
                0,
                &crate::LookOptions {
                    scale: Some(1),
                    bands: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(distinct(&look.1) >= 2);
    }

    #[test]
    fn anim_audit_arc_reports_trajectory_shape() {
        let s = studio("arc");
        s.doc_create("c", 16, 16).unwrap();
        s.doc_add_frame("c", 100, None, 1).unwrap();
        s.doc_add_frame("c", 100, None, 1).unwrap();
        draw(
            &s,
            "c",
            0,
            "pencil",
            json!({"points": [[2, 8]], "color": [255, 255, 255, 255]}),
        );
        draw(
            &s,
            "c",
            1,
            "pencil",
            json!({"points": [[8, 2]], "color": [255, 255, 255, 255]}),
        );
        draw(
            &s,
            "c",
            2,
            "pencil",
            json!({"points": [[14, 8]], "color": [255, 255, 255, 255]}),
        );
        let r = s.doc_anim_audit("c", None, None, "arc", None).unwrap();
        assert!(r["arc_residual"].as_f64().unwrap() > 0.0);
        assert_eq!(r["shape"], "arced");
    }

    #[test]
    fn import_clean_downscales_and_quantizes() {
        let s = studio("import");
        s.doc_create("c", 2, 2).unwrap();
        let p = std::env::temp_dir().join("atelier-import-src.png");
        RgbaImage::from_pixel(4, 4, Rgba([200, 30, 30, 255]))
            .save(&p)
            .unwrap();
        let r = s
            .import_clean(
                "c",
                0,
                0,
                p.to_str().unwrap(),
                2,
                Some(2),
                4,
                Some(true),
                false,
                false,
                false,
                vec![],
            )
            .unwrap();
        assert!(!r["palette"].as_array().unwrap().is_empty());
        let look = s
            .look(
                "c",
                0,
                &crate::LookOptions {
                    scale: Some(1),
                    bands: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(opaque(&look.1), 4);
    }

    #[test]
    fn import_clean_derives_aspect_and_removes_backdrop() {
        let s = studio("importbg");
        s.doc_create("c", 8, 4).unwrap();
        // 16x8 source: flat grey backdrop, red 8x8 block in the middle.
        let mut src = RgbaImage::from_pixel(16, 8, Rgba([90, 90, 90, 255]));
        for y in 0..8 {
            for x in 4..12 {
                src.put_pixel(x, y, Rgba([200, 30, 30, 255]));
            }
        }
        let p = std::env::temp_dir().join("atelier-import-bg.png");
        src.save(&p).unwrap();
        // target_h omitted → derived 4 from the 2:1 aspect; remove_bg floods
        // the grey corners away so the palette is subject-only.
        let r = s
            .import_clean(
                "c",
                0,
                0,
                p.to_str().unwrap(),
                8,
                None,
                4,
                Some(false),
                false,
                false,
                true,
                vec![],
            )
            .unwrap();
        assert_eq!(r["size"], json!([8, 4]));
        assert_eq!(r["bg_removed"], json!(true));
        // The backdrop corner became transparent; the subject survived.
        assert_eq!(
            s.doc_get_pixel("c", Some(0), 0, 0, 0).unwrap()["rgba"][3],
            json!(0)
        );
        assert!(
            s.doc_get_pixel("c", Some(0), 0, 4, 2).unwrap()["rgba"][0]
                .as_i64()
                .unwrap()
                > 150
        );
    }

    /// An 8x8 sheet holding four solid 4x4 tiles, row-major: red, green,
    /// blue, white — so each tile index has a distinct colour to assert on.
    fn tileset(s: &Studio, id: &str) {
        s.doc_create(id, 8, 8).unwrap();
        let tile = |x0, y0, c: [u8; 4]| json!({"x0": x0, "y0": y0, "x1": x0 + 3, "y1": y0 + 3, "color": c, "fill": true});
        draw(s, id, 0, "rect", tile(0, 0, [255, 0, 0, 255]));
        draw(s, id, 0, "rect", tile(4, 0, [0, 255, 0, 255]));
        draw(s, id, 0, "rect", tile(0, 4, [0, 0, 255, 255]));
        draw(s, id, 0, "rect", tile(4, 4, [255, 255, 255, 255]));
    }

    /// Every file under a doc dir as (relative path, bytes), sorted — the
    /// before/after fingerprint behind the tileset-is-read-only guarantee.
    fn dir_bytes(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for ent in fs::read_dir(&d).unwrap().flatten() {
                let p = ent.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(dir).unwrap().to_string_lossy().to_string();
                    out.push((rel, fs::read(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn place_tiles_stamps_tiles_at_exact_cell_positions() {
        let s = studio("tiles");
        tileset(&s, "ts");
        s.doc_create("c", 8, 8).unwrap();
        let r = s
            .doc_place_tiles(
                "c",
                0,
                0,
                "ts",
                4,
                4,
                &[[0, 0, 0], [1, 0, 1], [0, 1, 2], [1, 1, 3]],
            )
            .unwrap();
        assert_eq!(r["tiles_placed"], json!(4));
        assert_eq!(r["cells_skipped"], json!(0));
        let px = |x: i32, y: i32| s.doc_get_pixel("c", Some(0), 0, x, y).unwrap()["rgba"].clone();
        // Two corners per 4x4 quadrant pin each tile to its exact position.
        assert_eq!(px(0, 0), json!([255, 0, 0, 255]));
        assert_eq!(px(3, 3), json!([255, 0, 0, 255]));
        assert_eq!(px(4, 0), json!([0, 255, 0, 255]));
        assert_eq!(px(7, 3), json!([0, 255, 0, 255]));
        assert_eq!(px(0, 4), json!([0, 0, 255, 255]));
        assert_eq!(px(3, 7), json!([0, 0, 255, 255]));
        assert_eq!(px(4, 4), json!([255, 255, 255, 255]));
        assert_eq!(px(7, 7), json!([255, 255, 255, 255]));
    }

    #[test]
    fn place_tiles_blends_source_over_and_skips_off_canvas_cells() {
        let s = studio("tiles-so");
        tileset(&s, "ts");
        // Punch a transparent hole in tile 1's top-left pixel (sheet (4,0)).
        s.doc_clear_region("ts", 0, 0, 4, 0, 4, 0).unwrap();
        s.doc_create("c", 8, 8).unwrap();
        draw(
            &s,
            "c",
            0,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 7, "y1": 7, "color": [90, 90, 90, 255], "fill": true}),
        );
        // (-1,0) spans x -4..0 and (5,5) starts past the 8x8 canvas: skipped.
        let r = s
            .doc_place_tiles("c", 0, 0, "ts", 4, 4, &[[0, 0, 1], [-1, 0, 2], [5, 5, 3]])
            .unwrap();
        assert_eq!(r["tiles_placed"], json!(1));
        assert_eq!(r["cells_skipped"], json!(2));
        let px = |x: i32, y: i32| s.doc_get_pixel("c", Some(0), 0, x, y).unwrap()["rgba"].clone();
        assert_eq!(
            px(0, 0),
            json!([90, 90, 90, 255]),
            "a transparent tile pixel must not punch through the destination"
        );
        assert_eq!(px(1, 0), json!([0, 255, 0, 255]));
        assert_eq!(
            px(4, 0),
            json!([90, 90, 90, 255]),
            "the skipped cells stamped nothing"
        );
    }

    #[test]
    fn place_tiles_rejects_bad_grids_indices_and_docs() {
        let s = studio("tiles-err");
        tileset(&s, "ts");
        s.doc_create("c", 8, 8).unwrap();
        let e = s.doc_place_tiles("c", 0, 0, "ts", 0, 4, &[]).unwrap_err();
        assert!(e.contains(">= 1px"), "{e}");
        let e = s.doc_place_tiles("c", 0, 0, "ts", 4, 0, &[]).unwrap_err();
        assert!(e.contains(">= 1px"), "{e}");
        // 8 is not divisible by 3, and 16 exceeds the 8px sheet outright.
        let e = s.doc_place_tiles("c", 0, 0, "ts", 3, 3, &[]).unwrap_err();
        assert!(e.contains("not divisible"), "{e}");
        let e = s.doc_place_tiles("c", 0, 0, "ts", 16, 16, &[]).unwrap_err();
        assert!(e.contains("not divisible"), "{e}");
        // A 2x2 grid of tiles has indices 0..=3.
        let e = s
            .doc_place_tiles("c", 0, 0, "ts", 4, 4, &[[0, 0, 0], [1, 1, 4]])
            .unwrap_err();
        assert!(e.contains("out of range"), "{e}");
        assert!(s
            .doc_place_tiles("c", 0, 0, "ts", 4, 4, &[[0, 0, -1]])
            .is_err());
        let e = s
            .doc_place_tiles("nope", 0, 0, "ts", 4, 4, &[])
            .unwrap_err();
        assert!(e.contains("no document"), "{e}");
        let e = s.doc_place_tiles("c", 0, 0, "nope", 4, 4, &[]).unwrap_err();
        assert!(e.contains("no document"), "{e}");
        // Every failed call above must have left the canvas untouched.
        assert_eq!(
            s.doc_get_pixel("c", Some(0), 0, 0, 0).unwrap()["rgba"],
            json!([0, 0, 0, 0])
        );
    }

    #[test]
    fn place_tiles_never_writes_the_tileset() {
        let s = studio("tiles-ro");
        tileset(&s, "ts");
        s.doc_create("c", 8, 8).unwrap();
        let ts_dir = std::env::temp_dir().join("atelier-craft-tiles-ro/ts");
        let before = dir_bytes(&ts_dir);
        s.doc_place_tiles("c", 0, 0, "ts", 4, 4, &[[0, 0, 0], [1, 1, 3]])
            .unwrap();
        assert_eq!(
            before,
            dir_bytes(&ts_dir),
            "placing tiles mutated the tileset document"
        );
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn restore_brings_back_the_checkpointed_pixels() {
        let s = {
            let dir = std::env::temp_dir().join("atelier-craft-restore");
            let _ = fs::remove_dir_all(&dir);
            Studio::with_docs_dir(dir)
        };
        s.doc_create("c", 4, 4).unwrap();
        s.doc_draw(
            "c",
            0,
            0,
            "rect",
            json!({"x0": 0, "y0": 0, "x1": 1, "y1": 1, "color": [200, 0, 0, 255], "fill": true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        let cp = s.checkpoint("c", "save", None, None).unwrap();
        let cpid = cp["saved"].as_str().unwrap().to_string();
        // Wreck the canvas, then restore.
        s.doc_draw(
            "c",
            0,
            0,
            "fill_cel",
            json!({"color": [0, 0, 0, 255]})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        s.checkpoint("c", "restore", None, Some(&cpid)).unwrap();
        let px = s.doc_get_pixel("c", Some(0), 0, 0, 0).unwrap();
        assert_eq!(px["rgba"], json!([200, 0, 0, 255]));
        // The staging dir must not linger.
        let dir = std::env::temp_dir().join("atelier-craft-restore/documents/c/.restore-staging");
        assert!(!dir.exists());
    }
}
