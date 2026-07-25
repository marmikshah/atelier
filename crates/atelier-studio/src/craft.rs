//! World-class-art tooling — the craft layer on top of the primitives.
//!
//! These methods exist to close the gaps the art-quality review found:
//! let the near-blind agent actually SEE (`look`), work without fear
//! (`checkpoint`), edit structure (`layer_ops`), and reach perceptual colour &
//! master finish (`palette`, `snap_palette`, `critique`).
//! Image-returning methods hand back raw PNG bytes; the server wraps them as
//! inline MCP image content so the pixels arrive in the same turn.

use std::fs;
use std::path::Path;

use image::RgbaImage;

/// True only for the `cp<n>` ids `doc_checkpoint action=save` mints.
///
/// A checkpoint id is joined onto the store path and then handed to
/// `remove_dir_all`, so an unvalidated one is a directory traversal:
/// `../../../../x` escaped the store and deleted it. Every id the
/// tool hands out matches this shape, so rejecting anything else costs nothing.
fn valid_checkpoint_id(cpid: &str) -> bool {
    cpid.strip_prefix("cp")
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}
use serde_json::{Value, json};

use super::{JOURNAL_FILE, Studio};
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

/// Snapshot every file that defines live document state. Checkpoint metadata
/// itself is intentionally excluded.
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
    // A restored document must restore both its external comparison context
    // and the recipe that describes its pixels. Otherwise the canvas rolls
    // back while replay keeps the discarded edits.
    for name in [JOURNAL_FILE, "reference.png"] {
        let path = src.join(name);
        if path.is_file() {
            fs::copy(path, dst.join(name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

impl Studio {
    // -- doc_checkpoint: snapshot / restore -------------------------------

    /// History for an all-destructive editor: snapshot the document directory,
    /// list/restore snapshots, or prune them. `action`: `save` | `list` |
    /// `restore` | `prune`.
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
        // remove_dir_all, so it is as dangerous as `doc_id` and
        // gets the same treatment. Ids are always minted as `cp{n}` (below), so
        // anything else — traversal, absolute paths, a stray name — is a
        // caller error, not a lookup miss.
        if let Some(cpid) = checkpoint_id
            && !valid_checkpoint_id(cpid)
        {
            return Err(format!(
                "invalid checkpoint id '{}' — expected the cp<n> form doc_checkpoint action=save returns",
                cpid
            ));
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
                for name in ["doc.json", JOURNAL_FILE, "reference.png"] {
                    let _ = fs::remove_file(dir.join(name));
                }
                let swapped = (|| -> std::io::Result<()> {
                    fs::rename(staging.join("cels"), dir.join("cels"))?;
                    fs::rename(staging.join("doc.json"), dir.join("doc.json"))?;
                    for name in [JOURNAL_FILE, "reference.png"] {
                        let staged = staging.join(name);
                        if staged.is_file() {
                            fs::rename(staged, dir.join(name))?;
                        }
                    }
                    Ok(())
                })();
                let _ = fs::remove_dir_all(&staging);
                swapped.map_err(|e| {
                    format!("restore staged but the swap failed ({e}) — re-run restore to retry")
                })?;
                Ok(json!({"restored": cpid, "doc_id": id}))
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
                "unknown checkpoint action '{}' — use save|list|restore|prune",
                other
            )),
        }
    }

    // -- doc_layer_ops: the structural backbone ----------------------------

    /// Layer-stack lifecycle in one tool: `move` | `insert` | `delete` |
    /// `rename` | `duplicate` | `merge_down`. Returns a compact mutation ack;
    /// callers that need the complete stack use `doc_info`.
    pub(crate) fn layer_ops(
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
                ));
            }
        }
        doc.save(&dir)?;
        Ok(json!({
            "ok": true,
            "doc_id": id,
            "action": action,
            "new_index": new_index,
            "layers": doc.meta().layers.len(),
        }))
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
                "no palette to snap to — pass `palette` or use doc_palette op=set first".into(),
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
        self.edit_with_ack(id, layer, frame, |d| {
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
                ));
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
        if p[3] == 0 { None } else { Some(p) }
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
                if let Some(p) = op(x, y)
                    && !inset.contains(&p)
                {
                    off += 1;
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
            for action in ["prune", "restore"] {
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
        assert_eq!(r["layers"], 2);
        let m = s
            .layer_ops("c", "merge_down", 1, None, None, 255, "normal".into())
            .unwrap();
        assert_eq!(m["layers"], 1);
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
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    #[test]
    fn restore_brings_back_complete_checkpointed_state() {
        let root = std::env::temp_dir().join("atelier-craft-restore");
        let _ = fs::remove_dir_all(&root);
        let s = Studio::with_docs_dir(root.clone());
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

        let red_ref = root.join("red.png");
        let blue_ref = root.join("blue.png");
        RgbaImage::from_pixel(2, 2, image::Rgba([200, 0, 0, 255]))
            .save(&red_ref)
            .unwrap();
        RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 200, 255]))
            .save(&blue_ref)
            .unwrap();
        s.set_reference("c", red_ref.to_str()).unwrap();
        s.journal_append("c", "doc_create", &json!({"name": "c"}));

        let cp = s.checkpoint("c", "save", None, None).unwrap();
        let cpid = cp["saved"].as_str().unwrap().to_string();
        // Wreck every checkpointed state surface, then restore.
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
        s.set_reference("c", blue_ref.to_str()).unwrap();
        s.journal_append("c", "doc_draw", &json!({"op": "fill_cel"}));

        s.checkpoint("c", "restore", None, Some(&cpid)).unwrap();
        let px = s.doc_get_pixel("c", Some(0), 0, 0, 0).unwrap();
        assert_eq!(px["rgba"], json!([200, 0, 0, 255]));
        assert_eq!(s.journal("c").unwrap().len(), 1);
        assert_eq!(
            image::open(root.join("c/reference.png"))
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [200, 0, 0, 255]
        );
        // The staging dir must not linger.
        assert!(!root.join("c/.restore-staging").exists());
    }
}
